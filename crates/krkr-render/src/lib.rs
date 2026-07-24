use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    path::PathBuf,
    sync::Arc,
};

use bytemuck::{Pod, Zeroable};
use krkr_core::{
    Color, DrawCommand, FrameOutput, FrameTransition, ImageCommand, ImageUpload, Rect, Size,
    TextureId, TransitionMethod,
};
use krkr_font::FontSystem;
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

#[derive(Debug)]
pub enum RendererInitError {
    CreateSurface(wgpu::CreateSurfaceError),
    AdapterUnavailable,
    RequestDevice(wgpu::RequestDeviceError),
    NoSurfaceFormats,
}

impl fmt::Display for RendererInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateSurface(error) => write!(formatter, "failed to create surface: {error}"),
            Self::AdapterUnavailable => write!(formatter, "no compatible GPU adapter found"),
            Self::RequestDevice(error) => write!(formatter, "failed to request device: {error}"),
            Self::NoSurfaceFormats => write!(formatter, "surface reported no supported formats"),
        }
    }
}

impl Error for RendererInitError {}

#[derive(Debug)]
pub enum RenderError {
    OutOfMemory,
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => write!(formatter, "surface is out of memory"),
        }
    }
}

impl Error for RenderError {}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    texture_pipeline: TexturePipelineResources,
    transition_pipeline: TransitionPipelineResources,
    textures: BTreeMap<TextureId, CachedTexture>,
    text_font_system: FontSystem,
    next_text_texture_id: TextureId,
    frame_text_textures: BTreeSet<TextureId>,
    physical_size: PhysicalSize<u32>,
    scale_factor: f64,
    content_size: Option<Size>,
    /// One-shot surface capture target set through `capture_next_frame`.
    capture_path: Option<PathBuf>,
    /// One-shot per-texture capture target set through `capture_texture_next_frame`.
    capture_texture: Option<(TextureId, PathBuf)>,
    /// Whether capture support is enabled (adds COPY_SRC usage to surfaces and
    /// uploaded textures). Enabled via the KRKR_CAPTURE_* environment variables.
    capture_enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TexturePipelineState {
    Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderCapabilities {
    pub rectangles: bool,
    pub clipping: bool,
    pub texture_pipeline: TexturePipelineState,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderViewport {
    pub physical_size: PhysicalSize<u32>,
    pub logical_size: Size,
    pub scale_factor: f64,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Result<Self, RendererInitError> {
        let physical_size = window.inner_size();
        let scale_factor = window.scale_factor();

        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = preferred_backends();
        let instance = wgpu::Instance::new(instance_descriptor);
        let surface = instance
            .create_surface(window)
            .map_err(RendererInitError::CreateSurface)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| RendererInitError::AdapterUnavailable)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Kirakira device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(RendererInitError::RequestDevice)?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| capabilities.formats.first().copied())
            .ok_or(RendererInitError::NoSurfaceFormats)?;
        let present_mode = if capabilities
            .present_modes
            .contains(&wgpu::PresentMode::Fifo)
        {
            wgpu::PresentMode::Fifo
        } else {
            capabilities.present_modes[0]
        };
        let alpha_mode = capabilities.alpha_modes[0];
        // Surface capture (KRKR_CAPTURE_FRAME at the desktop app) copies the
        // presented texture for headless render diagnostics.
        let capture_enabled = std::env::var_os("KRKR_CAPTURE_FRAME").is_some()
            || std::env::var_os("KRKR_CAPTURE_VIDEO").is_some();
        let config = wgpu::SurfaceConfiguration {
            usage: if capture_enabled {
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC
            } else {
                wgpu::TextureUsages::RENDER_ATTACHMENT
            },
            format,
            width: physical_size.width.max(1),
            height: physical_size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let pipeline = create_rect_pipeline(&device, format);
        let texture_pipeline = TexturePipelineResources::new(&device, format);
        let transition_pipeline = TransitionPipelineResources::new(&device, format);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            texture_pipeline,
            transition_pipeline,
            textures: BTreeMap::new(),
            text_font_system: FontSystem::new(),
            next_text_texture_id: 1 << 60,
            frame_text_textures: BTreeSet::new(),
            physical_size,
            scale_factor,
            content_size: None,
            capture_path: None,
            capture_texture: None,
            capture_enabled,
        })
    }

    /// Arms a one-shot capture of the next presented frame into a PNG file.
    pub fn capture_next_frame(&mut self, path: impl Into<PathBuf>) {
        self.capture_path = Some(path.into());
    }

    /// Arms a one-shot capture of a specific uploaded texture (as currently
    /// cached) into a PNG file on the next rendered frame. Used together with
    /// `capture_next_frame` to tell upload-path faults from draw-path faults.
    pub fn capture_texture_next_frame(&mut self, texture_id: TextureId, path: impl Into<PathBuf>) {
        self.capture_texture = Some((texture_id, path.into()));
    }

    pub fn resize(&mut self, physical_size: PhysicalSize<u32>, scale_factor: f64) {
        self.physical_size = physical_size;
        self.scale_factor = scale_factor.max(1.0);

        if physical_size.width == 0 || physical_size.height == 0 {
            return;
        }

        self.config.width = physical_size.width;
        self.config.height = physical_size.height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn logical_size(&self) -> Size {
        Size::new(
            self.physical_size.width as f32 / self.scale_factor as f32,
            self.physical_size.height as f32 / self.scale_factor as f32,
        )
    }

    pub fn set_content_size(&mut self, content_size: Option<Size>) {
        self.content_size = content_size.filter(|size| !size.is_empty());
    }

    pub fn viewport(&self) -> RenderViewport {
        RenderViewport {
            physical_size: self.physical_size,
            logical_size: self.logical_size(),
            scale_factor: self.scale_factor,
        }
    }

    pub fn capabilities(&self) -> RenderCapabilities {
        RenderCapabilities {
            rectangles: true,
            clipping: true,
            texture_pipeline: self.texture_pipeline.state(),
        }
    }

    pub fn render(&mut self, frame: &FrameOutput) -> Result<(), RenderError> {
        let prepared = self.prepare_frame(frame);
        self.upload_frame_images(&prepared);
        self.retain_frame_textures(&prepared);
        if self.physical_size.width == 0 || self.physical_size.height == 0 {
            return Ok(());
        }

        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(surface_texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(surface_texture) => surface_texture,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Kirakira render encoder"),
            });

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        if let Some(transition) = &prepared.transition {
            let old_target = self.create_offscreen_target("Kirakira transition frozen target");
            let new_target = self.create_offscreen_target("Kirakira transition live target");
            self.render_commands_to_view(
                &mut encoder,
                &old_target.view,
                "Kirakira transition frozen pass",
                frame.clear_color,
                None,
                &transition.frozen_draw_commands,
            );
            self.render_commands_to_view(
                &mut encoder,
                &new_target.view,
                "Kirakira transition live pass",
                frame.clear_color,
                frame.clip,
                &prepared.draw_commands,
            );
            self.render_transition_to_view(
                &mut encoder,
                &view,
                frame.clear_color,
                &old_target.view,
                &new_target.view,
                transition,
            );
        } else {
            self.render_commands_to_view(
                &mut encoder,
                &view,
                "Kirakira render pass",
                frame.clear_color,
                frame.clip,
                &prepared.draw_commands,
            );
        }

        let capture_path = self.capture_path.take();
        let capture_buffer = capture_path.as_ref().map(|_| {
            let padded_bytes_per_row = padded_capture_row_bytes(self.config.width);
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Kirakira surface capture buffer"),
                size: u64::from(padded_bytes_per_row) * u64::from(self.config.height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });
        if let Some(buffer) = &capture_buffer {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &surface_texture.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_capture_row_bytes(self.config.width)),
                        rows_per_image: Some(self.config.height),
                    },
                },
                wgpu::Extent3d {
                    width: self.config.width,
                    height: self.config.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let capture_texture = self.capture_texture.take();
        let capture_texture_buffer = capture_texture
            .as_ref()
            .and_then(|(texture_id, _)| self.textures.get(texture_id))
            .map(|cached| {
                let padded_bytes_per_row = padded_capture_row_bytes(cached.width);
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Kirakira texture capture buffer"),
                    size: u64::from(padded_bytes_per_row) * u64::from(cached.height),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: &cached._texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(padded_bytes_per_row),
                            rows_per_image: Some(cached.height),
                        },
                    },
                    wgpu::Extent3d {
                        width: cached.width,
                        height: cached.height,
                        depth_or_array_layers: 1,
                    },
                );
                (buffer, cached.width, cached.height)
            });
        self.queue.submit(Some(encoder.finish()));
        if let (Some(path), Some(buffer)) = (capture_path, capture_buffer)
            && let Err(error) = self.save_capture_buffer(
                &buffer,
                &path,
                self.config.width,
                self.config.height,
                matches!(
                    self.config.format,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
                ),
            )
        {
            eprintln!("[krkr-render][warn] surface capture failed: {error}");
        }
        if let (Some((_, path)), Some((buffer, width, height))) =
            (capture_texture, capture_texture_buffer)
        {
            // Uploaded textures are Rgba8UnormSrgb, so no BGRA swap.
            if let Err(error) = self.save_capture_buffer(&buffer, &path, width, height, false) {
                eprintln!("[krkr-render][warn] texture capture failed: {error}");
            }
        }
        surface_texture.present();
        Ok(())
    }

    fn save_capture_buffer(
        &self,
        buffer: &wgpu::Buffer,
        path: &std::path::Path,
        width: u32,
        height: u32,
        bgra: bool,
    ) -> Result<(), String> {
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| error.to_string())?;
        rx.recv()
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let padded_bytes_per_row = padded_capture_row_bytes(width);
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        {
            let mapped = slice.get_mapped_range();
            for row in 0..height {
                let start = (row * padded_bytes_per_row) as usize;
                let row_data = &mapped[start..start + (width * 4) as usize];
                if bgra {
                    for pixel in row_data.chunks_exact(4) {
                        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                    }
                } else {
                    rgba.extend_from_slice(row_data);
                }
            }
        }
        buffer.unmap();
        write_capture_png(path, width, height, &rgba).map_err(|error| error.to_string())
    }

    fn prepare_frame(&mut self, frame: &FrameOutput) -> FrameOutput {
        for texture_id in std::mem::take(&mut self.frame_text_textures) {
            self.textures.remove(&texture_id);
        }

        let (draw_commands, mut image_uploads) = self.prepare_commands(&frame.draw_commands);
        image_uploads.extend(frame.image_uploads.iter().cloned());
        let transition = frame.transition.as_ref().map(|transition| {
            let (frozen_draw_commands, mut frozen_image_uploads) =
                self.prepare_commands(&transition.frozen_draw_commands);
            frozen_image_uploads.extend(transition.frozen_image_uploads.iter().cloned());
            FrameTransition {
                method: transition.method.clone(),
                progress: transition.progress,
                params: transition.params.clone(),
                rule_texture_id: transition.rule_texture_id,
                rule_image_upload: transition.rule_image_upload.clone(),
                frozen_draw_commands,
                frozen_image_uploads,
            }
        });

        FrameOutput {
            clear_color: frame.clear_color,
            clip: frame.clip,
            draw_commands,
            image_uploads,
            transition,
        }
    }

    fn prepare_commands(
        &mut self,
        commands: &[DrawCommand],
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        let mut prepared = Vec::with_capacity(commands.len());
        let mut uploads = Vec::new();
        for command in commands {
            match command {
                DrawCommand::Text(text) => {
                    let image = self
                        .text_font_system
                        .rasterize_text(&text.font, text.style, &text.text);
                    if image.width == 0 || image.height == 0 {
                        continue;
                    }
                    let texture_id = self.next_text_texture_id;
                    self.next_text_texture_id = self.next_text_texture_id.saturating_add(1);
                    self.frame_text_textures.insert(texture_id);
                    uploads.push(ImageUpload::new(
                        texture_id,
                        image.width,
                        image.height,
                        Arc::from(image.rgba),
                    ));
                    prepared.push(DrawCommand::Image(ImageCommand {
                        texture_id,
                        rect: Rect::new(
                            text.position.x,
                            text.position.y,
                            image.width as f32,
                            image.height as f32,
                        ),
                        source_rect: Rect::new(0.0, 0.0, image.width as f32, image.height as f32),
                        texture_size: Size::new(image.width as f32, image.height as f32),
                        opacity: text.color.a,
                    }));
                }
                _ => prepared.push(command.clone()),
            }
        }
        (prepared, uploads)
    }

    fn upload_frame_images(&mut self, frame: &FrameOutput) {
        self.upload_images(&frame.image_uploads);
        if let Some(transition) = &frame.transition {
            self.upload_images(&transition.frozen_image_uploads);
            if let Some(upload) = &transition.rule_image_upload {
                self.upload_images(std::slice::from_ref(upload));
            }
        }
    }

    fn upload_images(&mut self, uploads: &[ImageUpload]) {
        for upload in uploads {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Kirakira uploaded texture"),
                size: wgpu::Extent3d {
                    width: upload.width.max(1),
                    height: upload.height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: if self.capture_enabled {
                    wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST
                },
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &upload.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(upload.width.saturating_mul(4)),
                    rows_per_image: Some(upload.height),
                },
                wgpu::Extent3d {
                    width: upload.width.max(1),
                    height: upload.height.max(1),
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Kirakira texture bind group"),
                layout: &self.texture_pipeline.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.texture_pipeline.sampler),
                    },
                ],
            });
            self.textures.insert(
                upload.texture_id,
                CachedTexture {
                    _texture: texture,
                    _view: view,
                    bind_group,
                    width: upload.width,
                    height: upload.height,
                },
            );
        }
    }

    fn retain_frame_textures(&mut self, frame: &FrameOutput) {
        let mut referenced = BTreeSet::new();
        collect_image_texture_ids(&frame.draw_commands, &mut referenced);
        if let Some(transition) = &frame.transition {
            collect_image_texture_ids(&transition.frozen_draw_commands, &mut referenced);
            if let Some(texture_id) = transition.rule_texture_id {
                referenced.insert(texture_id);
            }
        }
        self.textures
            .retain(|texture_id, _| referenced.contains(texture_id));
    }

    fn render_commands_to_view(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        label: &'static str,
        clear_color: Color,
        clip: Option<Rect>,
        commands: &[DrawCommand],
    ) {
        let physical_clip = clip.and_then(|clip| self.physical_rect(clip));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu_color(clear_color)),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        if let Some(clip) = physical_clip {
            pass.set_scissor_rect(clip.x, clip.y, clip.width, clip.height);
        }
        if clip.is_none() || physical_clip.is_some() {
            self.draw_commands(&mut pass, commands);
        }
    }

    fn render_transition_to_view(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        clear_color: Color,
        old_view: &wgpu::TextureView,
        new_view: &wgpu::TextureView,
        transition: &FrameTransition,
    ) {
        let uniforms = transition_uniforms(
            transition,
            self.config.width.max(1) as f32,
            self.config.height.max(1) as f32,
        );
        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Kirakira transition uniforms"),
                contents: bytemuck::cast_slice(&[uniforms]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let rule_view = transition
            .rule_texture_id
            .and_then(|texture_id| self.textures.get(&texture_id))
            .map(|texture| &texture._view)
            .unwrap_or(old_view);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Kirakira transition bind group"),
            layout: &self.transition_pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(old_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.texture_pipeline.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(new_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.texture_pipeline.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(rule_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&self.texture_pipeline.sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Kirakira transition composite pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu_color(clear_color)),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.draw_transition_fullscreen(&mut pass, &bind_group);
    }

    fn create_offscreen_target(&self, label: &'static str) -> OffscreenTarget {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: self.config.width.max(1),
                height: self.config.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        OffscreenTarget {
            _texture: texture,
            view,
        }
    }

    fn draw_commands<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        commands: &[DrawCommand],
    ) {
        let mut vertices = Vec::new();
        for command in commands {
            match command {
                DrawCommand::Rect(rect) => {
                    vertices.extend_from_slice(&self.rect_vertices(rect.rect, rect.color));
                }
                DrawCommand::Text(_) => {}
                DrawCommand::Image(image) => {
                    self.flush_rect_vertices(pass, &mut vertices);
                    self.draw_image(pass, image);
                }
            }
        }
        self.flush_rect_vertices(pass, &mut vertices);
    }

    fn flush_rect_vertices<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        vertices: &mut Vec<Vertex>,
    ) {
        if vertices.is_empty() {
            return;
        }
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Kirakira rect vertices"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.draw(0..vertices.len() as u32, 0..1);
        vertices.clear();
    }

    fn draw_image<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>, command: &ImageCommand) {
        let Some(texture) = self.textures.get(&command.texture_id) else {
            return;
        };
        if command.rect.width <= 0.0 || command.rect.height <= 0.0 {
            return;
        }
        let vertices = self.image_vertices(command);
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Kirakira image vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        pass.set_pipeline(&self.texture_pipeline.pipeline);
        pass.set_bind_group(0, &texture.bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.draw(0..vertices.len() as u32, 0..1);
    }

    fn draw_transition_fullscreen<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        bind_group: &'pass wgpu::BindGroup,
    ) {
        let tint = [1.0, 1.0, 1.0, 1.0];
        let vertices = [
            TexturedVertex::new([-1.0, 1.0], [0.0, 0.0], tint),
            TexturedVertex::new([1.0, 1.0], [1.0, 0.0], tint),
            TexturedVertex::new([1.0, -1.0], [1.0, 1.0], tint),
            TexturedVertex::new([-1.0, 1.0], [0.0, 0.0], tint),
            TexturedVertex::new([1.0, -1.0], [1.0, 1.0], tint),
            TexturedVertex::new([-1.0, -1.0], [0.0, 1.0], tint),
        ];
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Kirakira fullscreen texture vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        pass.set_pipeline(&self.transition_pipeline.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.draw(0..vertices.len() as u32, 0..1);
    }

    fn image_vertices(&self, command: &ImageCommand) -> [TexturedVertex; 6] {
        let transform = self.render_transform();
        let x0 = transform.x_offset + command.rect.x * transform.x_scale;
        let y0 = transform.y_offset + command.rect.y * transform.y_scale;
        let x1 = transform.x_offset + (command.rect.x + command.rect.width) * transform.x_scale;
        let y1 = transform.y_offset + (command.rect.y + command.rect.height) * transform.y_scale;
        let tx0 = command.source_rect.x / command.texture_size.width.max(1.0);
        let ty0 = command.source_rect.y / command.texture_size.height.max(1.0);
        let tx1 = (command.source_rect.x + command.source_rect.width)
            / command.texture_size.width.max(1.0);
        let ty1 = (command.source_rect.y + command.source_rect.height)
            / command.texture_size.height.max(1.0);
        let tint = [1.0, 1.0, 1.0, command.opacity.clamp(0.0, 1.0)];

        [
            TexturedVertex::new(self.ndc(x0, y0), [tx0, ty0], tint),
            TexturedVertex::new(self.ndc(x1, y0), [tx1, ty0], tint),
            TexturedVertex::new(self.ndc(x1, y1), [tx1, ty1], tint),
            TexturedVertex::new(self.ndc(x0, y0), [tx0, ty0], tint),
            TexturedVertex::new(self.ndc(x1, y1), [tx1, ty1], tint),
            TexturedVertex::new(self.ndc(x0, y1), [tx0, ty1], tint),
        ]
    }

    fn rect_vertices(&self, rect: Rect, color: Color) -> [Vertex; 6] {
        let transform = self.render_transform();
        let x0 = transform.x_offset + rect.x * transform.x_scale;
        let y0 = transform.y_offset + rect.y * transform.y_scale;
        let x1 = transform.x_offset + (rect.x + rect.width) * transform.x_scale;
        let y1 = transform.y_offset + (rect.y + rect.height) * transform.y_scale;
        let color = [color.r, color.g, color.b, color.a];

        [
            Vertex::new(self.ndc(x0, y0), color),
            Vertex::new(self.ndc(x1, y0), color),
            Vertex::new(self.ndc(x1, y1), color),
            Vertex::new(self.ndc(x0, y0), color),
            Vertex::new(self.ndc(x1, y1), color),
            Vertex::new(self.ndc(x0, y1), color),
        ]
    }

    fn ndc(&self, x: f32, y: f32) -> [f32; 2] {
        let width = self.config.width as f32;
        let height = self.config.height as f32;
        [(x / width) * 2.0 - 1.0, 1.0 - (y / height) * 2.0]
    }

    fn physical_rect(&self, rect: Rect) -> Option<PhysicalRect> {
        let transform = self.render_transform();
        let target_width = self.config.width as f32;
        let target_height = self.config.height as f32;
        let x0 = (transform.x_offset + rect.x * transform.x_scale)
            .floor()
            .clamp(0.0, target_width);
        let y0 = (transform.y_offset + rect.y * transform.y_scale)
            .floor()
            .clamp(0.0, target_height);
        let x1 = (transform.x_offset + (rect.x + rect.width) * transform.x_scale)
            .ceil()
            .clamp(0.0, target_width);
        let y1 = (transform.y_offset + (rect.y + rect.height) * transform.y_scale)
            .ceil()
            .clamp(0.0, target_height);

        if x1 <= x0 || y1 <= y0 {
            return None;
        }

        Some(PhysicalRect {
            x: x0 as u32,
            y: y0 as u32,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }

    fn render_transform(&self) -> RenderTransform {
        let content_size = self.content_size.unwrap_or_else(|| self.logical_size());
        let target_width = self.config.width as f32;
        let target_height = self.config.height as f32;
        let scale = (target_width / content_size.width.max(1.0))
            .min(target_height / content_size.height.max(1.0));
        let rendered_width = content_size.width * scale;
        let rendered_height = content_size.height * scale;
        RenderTransform {
            x_scale: scale,
            y_scale: scale,
            x_offset: (target_width - rendered_width) * 0.5,
            y_offset: (target_height - rendered_height) * 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RenderTransform {
    x_scale: f32,
    y_scale: f32,
    x_offset: f32,
    y_offset: f32,
}

#[cfg(target_os = "macos")]
fn preferred_backends() -> wgpu::Backends {
    wgpu::Backends::METAL
}

#[cfg(not(target_os = "macos"))]
fn preferred_backends() -> wgpu::Backends {
    wgpu::Backends::PRIMARY
}

fn collect_image_texture_ids(commands: &[DrawCommand], texture_ids: &mut BTreeSet<TextureId>) {
    for command in commands {
        if let DrawCommand::Image(image) = command {
            texture_ids.insert(image.texture_id);
        }
    }
}

fn create_rect_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::include_wgsl!("rect.wgsl"));
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Kirakira rect pipeline layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Kirakira rect pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Vertex::layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

struct TexturePipelineResources {
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
}

impl TexturePipelineResources {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Kirakira texture bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Kirakira texture sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("texture.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Kirakira texture pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Kirakira texture pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[TexturedVertex::layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            bind_group_layout,
            sampler,
            pipeline,
        }
    }

    fn state(&self) -> TexturePipelineState {
        TexturePipelineState::Ready
    }
}

struct TransitionPipelineResources {
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl TransitionPipelineResources {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Kirakira transition bind group layout"),
            entries: &[
                texture_bind_group_layout_entry(0),
                sampler_bind_group_layout_entry(1),
                texture_bind_group_layout_entry(2),
                sampler_bind_group_layout_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_bind_group_layout_entry(5),
                sampler_bind_group_layout_entry(6),
            ],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("transition.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Kirakira transition pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Kirakira transition pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[TexturedVertex::layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            bind_group_layout,
            pipeline,
        }
    }
}

fn texture_bind_group_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_bind_group_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

struct CachedTexture {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

struct OffscreenTarget {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PhysicalRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn wgpu_color(color: Color) -> wgpu::Color {
    wgpu::Color {
        r: color.r as f64,
        g: color.g as f64,
        b: color.b as f64,
        a: color.a as f64,
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 4],
}

impl Vertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];

    const fn new(position: [f32; 2], color: [f32; 4]) -> Self {
        Self { position, color }
    }

    fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TexturedVertex {
    position: [f32; 2],
    tex_coord: [f32; 2],
    tint: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TransitionUniforms {
    data: [[f32; 4]; 8],
}

fn transition_uniforms(
    transition: &FrameTransition,
    viewport_width: f32,
    viewport_height: f32,
) -> TransitionUniforms {
    let params = &transition.params;
    let primary_bg_color = if matches!(
        params.method,
        TransitionMethod::Turn | TransitionMethod::RotateSwap
    ) {
        params.bg_color
    } else {
        params.bg_color1
    };
    TransitionUniforms {
        data: [
            [
                transition.progress.clamp(0.0, 1.0),
                params.method.as_code(),
                if transition.rule_texture_id.is_some() {
                    1.0
                } else {
                    0.0
                },
                0.0,
            ],
            [
                viewport_width,
                viewport_height,
                params.vague.max(0.0),
                params.scroll_from as u8 as f32,
            ],
            [
                params.scroll_stay as u8 as f32,
                params.wave_type,
                params.max_h.max(0.0),
                params.max_omega.max(0.0),
            ],
            color_uniform(primary_bg_color),
            color_uniform(params.bg_color2),
            [
                params.max_size.max(1.0),
                params.factor.max(0.0),
                params.accel,
                params.twist,
            ],
            [
                params.twist_accel,
                params.center_x,
                params.center_y,
                params.ripple_width.max(1.0),
            ],
            [
                params.roundness.max(0.01),
                params.speed.max(0.01),
                params.max_drift.max(0.0),
                0.0,
            ],
        ],
    }
}

fn color_uniform(color: Color) -> [f32; 4] {
    [color.r, color.g, color.b, color.a]
}

impl TexturedVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];

    const fn new(position: [f32; 2], tex_coord: [f32; 2], tint: [f32; 4]) -> Self {
        Self {
            position,
            tex_coord,
            tint,
        }
    }

    fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

fn padded_capture_row_bytes(width: u32) -> u32 {
    (width * 4).div_ceil(256) * 256
}

fn write_capture_png(
    path: &std::path::Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((width * height * 4 + height) as usize);
    for row in rgba.chunks_exact((width * 4) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // zlib stream with stored (uncompressed) deflate blocks.
    let mut zdata = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        zdata.push(u8::from(last));
        zdata.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        zdata.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        zdata.extend_from_slice(chunk);
    }
    zdata.extend_from_slice(&capture_adler32(&raw).to_be_bytes());

    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    write_capture_png_chunk(&mut png, b"IHDR", &ihdr);
    write_capture_png_chunk(&mut png, b"IDAT", &zdata);
    write_capture_png_chunk(&mut png, b"IEND", &[]);
    std::fs::write(path, png)
}

fn write_capture_png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    png.extend_from_slice(&capture_crc32(kind, data).to_be_bytes());
}

fn capture_crc32(kind: &[u8], data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn capture_adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}
