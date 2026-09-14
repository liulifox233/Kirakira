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
    TextCommand, TextureId, TransitionMethod,
};
use krkr_font::FontSystem;
use wgpu::util::DeviceExt;

/// The `wave` kernel's CPU mirror: the executable specification the shader is
/// held to by the tests in this crate.  It ships only in test builds -- the
/// renderer's implementation of a transition kernel is the WGSL.
#[cfg(test)]
mod wave;
#[cfg(feature = "winit-surface")]
pub use winit::dpi::PhysicalSize;
#[cfg(feature = "winit-surface")]
use winit::window::Window;
#[cfg(not(feature = "winit-surface"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PhysicalSize<T> {
    pub width: T,
    pub height: T,
}

#[cfg(not(feature = "winit-surface"))]
impl<T> PhysicalSize<T> {
    pub const fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

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
    /// Format for uploaded bitmaps, matched to the surface's encoding.
    ///
    /// WebGPU canvases only accept non-sRGB formats, so an sRGB texture would
    /// be linearised on sample and written to the canvas without the matching
    /// linear->sRGB encode, darkening every image.
    image_texture_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    texture_pipeline: TexturePipelineResources,
    transition_pipeline: TransitionPipelineResources,
    textures: BTreeMap<TextureId, CachedTexture>,
    text_font_system: FontSystem,
    text_cache: TextImageCache,
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
    suspended: bool,
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
    #[cfg(feature = "winit-surface")]
    pub async fn new(window: Arc<Window>) -> Result<Self, RendererInitError> {
        let physical_size = window.inner_size();
        let scale_factor = window.scale_factor();

        // The instance carries the window's display handle: the GL backend
        // needs it to present on Wayland, and without it a session whose
        // Vulkan driver never loads has no backend able to create the surface
        // at all (the failure then reports an empty per-backend error map).
        let mut instance_descriptor =
            wgpu::InstanceDescriptor::new_with_display_handle(Box::new(window.clone()));
        instance_descriptor.backends = preferred_backends();
        let instance = wgpu::Instance::new(instance_descriptor);
        let surface = instance
            .create_surface(window)
            .map_err(RendererInitError::CreateSurface)?;
        Self::new_with_surface(instance, surface, physical_size, scale_factor).await
    }

    /// Initializes a renderer from an application-owned surface.  Desktop and
    /// Web shells can now create their own winit/canvas target and keep window
    /// lifecycle concerns out of the renderer itself.
    pub async fn new_with_surface(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        physical_size: PhysicalSize<u32>,
        scale_factor: f64,
    ) -> Result<Self, RendererInitError> {
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
        // TVP presents 8-bit display-referred values unchanged, and the engine
        // feeds sRGB-normalized colors and bitmaps. A non-sRGB target keeps
        // that pass-through; an sRGB target would treat fragment output as
        // linear and re-encode it, so a mid-gray fill would brighten. WebGPU
        // canvases only expose non-sRGB formats anyway, so this also makes the
        // desktop and browser paths agree.
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
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
        #[cfg(not(target_arch = "wasm32"))]
        let capture_enabled = std::env::var_os("KRKR_CAPTURE_FRAME").is_some()
            || std::env::var_os("KRKR_CAPTURE_VIDEO").is_some();
        #[cfg(target_arch = "wasm32")]
        let capture_enabled = false;
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
        let image_texture_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };

        Ok(Self {
            surface,
            device,
            queue,
            config,
            image_texture_format,
            pipeline,
            texture_pipeline,
            transition_pipeline,
            textures: BTreeMap::new(),
            text_font_system: FontSystem::new(),
            text_cache: TextImageCache::new(),
            physical_size,
            scale_factor,
            content_size: None,
            capture_path: None,
            capture_texture: None,
            capture_enabled,
            suspended: physical_size.width == 0 || physical_size.height == 0,
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
        self.suspended = physical_size.width == 0 || physical_size.height == 0;

        if physical_size.width == 0 || physical_size.height == 0 {
            return;
        }

        self.config.width = physical_size.width;
        self.config.height = physical_size.height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Suspends presentation while a mobile/window surface is detached. The
    /// engine keeps running and its draw list/textures remain valid; a later
    /// `resume`/`resize` simply reconfigures the existing device surface.
    pub fn suspend(&mut self) {
        self.suspended = true;
    }

    pub fn resume(&mut self, physical_size: PhysicalSize<u32>, scale_factor: f64) {
        self.resize(physical_size, scale_factor);
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
        // An entry outlives its text's draw command only until the renderer
        // drops the texture under it; purge it then, so the cache is bounded by
        // the textures the prepared frame still references and never names one
        // the renderer no longer holds.
        self.text_cache
            .retain_live_textures(|texture_id| self.textures.contains_key(&texture_id));
        if self.suspended || self.physical_size.width == 0 || self.physical_size.height == 0 {
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
        if prepared.transitions.is_empty() {
            self.render_commands_to_view(
                &mut encoder,
                &view,
                "Kirakira render pass",
                frame.clear_color,
                frame.clip,
                &prepared.draw_commands,
            );
        } else {
            // The live tree is drawn first; each transition then replaces only
            // its own destination rectangle (`tTVPDivisibleData::Dest`,
            // `LayerIntf.cpp:6513`), so unrelated layers keep drawing normally
            // while their neighbours transition.
            self.render_commands_to_view(
                &mut encoder,
                &view,
                "Kirakira render pass",
                frame.clear_color,
                frame.clip,
                &prepared.draw_commands,
            );
            for transition in &prepared.transitions {
                let plan = transition_composite_plan();
                let old_target = self.create_offscreen_target("Kirakira transition frozen target");
                self.render_commands_to_view(
                    &mut encoder,
                    &old_target.view,
                    transition_face_label(plan.old),
                    frame.clear_color,
                    None,
                    transition_face_commands(transition, plan.old),
                );
                let new_target = self.create_offscreen_target("Kirakira transition source target");
                for (face, load) in plan.incoming {
                    match load {
                        FaceLoad::Clear => self.render_commands_to_view(
                            &mut encoder,
                            &new_target.view,
                            transition_face_label(face),
                            frame.clear_color,
                            None,
                            transition_face_commands(transition, face),
                        ),
                        FaceLoad::Load => self.render_commands_over_view(
                            &mut encoder,
                            &new_target.view,
                            transition_face_label(face),
                            None,
                            transition_face_commands(transition, face),
                        ),
                    }
                }
                // Kernels that write a vacated region as a colour have to
                // composite that colour over the scene *beneath* the
                // destination layer, which the incoming face cannot supply (it
                // already has the source drawn over it).  Render the under face
                // on its own only for those kernels.
                let under_target = wave_under_face_needed(transition.params.method).then(|| {
                    let target = self.create_offscreen_target("Kirakira transition under target");
                    self.render_commands_to_view(
                        &mut encoder,
                        &target.view,
                        "Kirakira transition under face",
                        frame.clear_color,
                        None,
                        &transition.under_draw_commands,
                    );
                    target
                });
                self.render_transition_to_view(
                    &mut encoder,
                    &view,
                    transition.dest_rect,
                    TransitionFaceViews {
                        old: &old_target.view,
                        new: &new_target.view,
                        under: under_target.as_ref().map(|target| &target.view),
                    },
                    transition,
                );
            }
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
            // Uploaded textures always use an RGBA format, so no BGRA swap.
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
        for texture_id in &frame.image_releases {
            self.textures.remove(texture_id);
        }

        // Text textures prepared earlier in this frame are not in
        // `self.textures` yet (uploading runs after preparation), so every
        // preparation sees the ids the frame has already minted as live, and a
        // text that appears twice in one frame — in one draw list, or in one
        // and then the next — rasterizes and uploads once.  The set is filled
        // by the preparation itself, as it mints each id.
        let mut queued = BTreeSet::new();
        let (draw_commands, mut image_uploads) =
            self.prepare_commands(&frame.draw_commands, &mut queued);
        image_uploads.extend(frame.image_uploads.iter().cloned());
        let transitions = frame
            .transitions
            .iter()
            .map(|transition| {
                let (frozen_draw_commands, mut frozen_image_uploads) =
                    self.prepare_commands(&transition.frozen_draw_commands, &mut queued);
                frozen_image_uploads.extend(transition.frozen_image_uploads.iter().cloned());
                let (under_draw_commands, mut under_image_uploads) =
                    self.prepare_commands(&transition.under_draw_commands, &mut queued);
                under_image_uploads.extend(transition.under_image_uploads.iter().cloned());
                let (source_draw_commands, mut source_image_uploads) =
                    self.prepare_commands(&transition.source_draw_commands, &mut queued);
                source_image_uploads.extend(transition.source_image_uploads.iter().cloned());
                FrameTransition {
                    method: transition.method.clone(),
                    progress: transition.progress,
                    params: transition.params.clone(),
                    dest_rect: transition.dest_rect,
                    rule_texture_id: transition.rule_texture_id,
                    rule_image_upload: transition.rule_image_upload.clone(),
                    frozen_draw_commands,
                    frozen_image_uploads,
                    under_draw_commands,
                    under_image_uploads,
                    source_draw_commands,
                    source_image_uploads,
                }
            })
            .collect();

        FrameOutput {
            clear_color: frame.clear_color,
            clip: frame.clip,
            draw_commands,
            image_uploads,
            image_releases: frame.image_releases.clone(),
            transitions,
        }
    }

    fn prepare_commands(
        &mut self,
        commands: &[DrawCommand],
        queued: &mut BTreeSet<TextureId>,
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        prepare_commands(
            commands,
            &self.text_font_system,
            &mut self.text_cache,
            queued,
            |texture_id| self.textures.contains_key(&texture_id),
        )
    }

    fn upload_frame_images(&mut self, frame: &FrameOutput) {
        self.upload_images(&frame.image_uploads);
        for transition in &frame.transitions {
            self.upload_images(&transition.frozen_image_uploads);
            self.upload_images(&transition.under_image_uploads);
            self.upload_images(&transition.source_image_uploads);
            if let Some(upload) = &transition.rule_image_upload {
                self.upload_images(std::slice::from_ref(upload));
            }
        }
    }

    fn upload_images(&mut self, uploads: &[ImageUpload]) {
        upload_images(
            &self.device,
            &self.queue,
            &self.texture_pipeline,
            self.image_texture_format,
            self.capture_enabled,
            &mut self.textures,
            uploads,
        );
    }

    fn retain_frame_textures(&mut self, frame: &FrameOutput) {
        let mut referenced = BTreeSet::new();
        collect_image_texture_ids(&frame.draw_commands, &mut referenced);
        for transition in &frame.transitions {
            collect_image_texture_ids(&transition.frozen_draw_commands, &mut referenced);
            collect_image_texture_ids(&transition.under_draw_commands, &mut referenced);
            collect_image_texture_ids(&transition.source_draw_commands, &mut referenced);
            if let Some(texture_id) = transition.rule_texture_id {
                referenced.insert(texture_id);
            }
        }
        self.textures
            .retain(|texture_id, _| referenced.contains(texture_id));
    }

    /// Draws `commands` on top of whatever `view` already holds.
    fn render_commands_over_view(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        label: &'static str,
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
                    load: wgpu::LoadOp::Load,
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
        dest_rect: Option<Rect>,
        faces: TransitionFaceViews<'_>,
        transition: &FrameTransition,
    ) {
        let TransitionFaceViews {
            old: old_view,
            new: new_view,
            under: under_view,
        } = faces;
        // A transition whose destination has no measurable geometry covers the
        // whole frame; otherwise only the destination layer's own area is
        // rewritten and the live frame stays visible everywhere else.
        let clip = match dest_rect {
            Some(rect) => match self.physical_rect(rect) {
                Some(clip) => Some(clip),
                None => return,
            },
            None => None,
        };
        let content_size = self.content_size.unwrap_or_else(|| self.logical_size());
        let uniforms = transition_uniforms(
            transition,
            self.config.width.max(1) as f32,
            self.config.height.max(1) as f32,
            self.render_transform(),
            content_size,
            under_view.is_some(),
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
        // Kernels without an under face still have to bind a texture there;
        // `data[9].w` tells them it is not the scene beneath the destination.
        let under_view = under_view.unwrap_or(new_view);
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
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(under_view),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
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
                    // The live frame is already on the surface; the transition
                    // only rewrites its destination rectangle, so the rest of
                    // the frame survives untouched.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(clip) = clip {
            pass.set_scissor_rect(clip.x, clip.y, clip.width, clip.height);
        }
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
            TexturedVertex::new([-1.0, 1.0], [0.0, 0.0], tint, 0.0),
            TexturedVertex::new([1.0, 1.0], [1.0, 0.0], tint, 0.0),
            TexturedVertex::new([1.0, -1.0], [1.0, 1.0], tint, 0.0),
            TexturedVertex::new([-1.0, 1.0], [0.0, 0.0], tint, 0.0),
            TexturedVertex::new([1.0, -1.0], [1.0, 1.0], tint, 0.0),
            TexturedVertex::new([-1.0, -1.0], [0.0, 1.0], tint, 0.0),
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
        image_vertices(
            self.render_transform(),
            self.config.width,
            self.config.height,
            command,
        )
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
        ndc(x, y, self.config.width, self.config.height)
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

/// The quad an image command draws, in the target's normalized device
/// coordinates.  `transform` maps the command's logical rect into target
/// pixels, which `target_width`/`target_height` then turn into NDC — the
/// geometry the render pass samples the command's texture with, so a caller
/// outside the renderer (the offscreen text tests) draws the same pixels.
fn image_vertices(
    transform: RenderTransform,
    target_width: u32,
    target_height: u32,
    command: &ImageCommand,
) -> [TexturedVertex; 6] {
    let x0 = transform.x_offset + command.rect.x * transform.x_scale;
    let y0 = transform.y_offset + command.rect.y * transform.y_scale;
    let x1 = transform.x_offset + (command.rect.x + command.rect.width) * transform.x_scale;
    let y1 = transform.y_offset + (command.rect.y + command.rect.height) * transform.y_scale;
    let tx0 = command.source_rect.x / command.texture_size.width.max(1.0);
    let ty0 = command.source_rect.y / command.texture_size.height.max(1.0);
    let tx1 =
        (command.source_rect.x + command.source_rect.width) / command.texture_size.width.max(1.0);
    let ty1 =
        (command.source_rect.y + command.source_rect.height) / command.texture_size.height.max(1.0);
    let tint = [1.0, 1.0, 1.0, command.opacity.clamp(0.0, 1.0)];
    let force_opaque = if command.opaque { 1.0 } else { 0.0 };
    let ndc = |x: f32, y: f32| ndc(x, y, target_width, target_height);

    [
        TexturedVertex::new(ndc(x0, y0), [tx0, ty0], tint, force_opaque),
        TexturedVertex::new(ndc(x1, y0), [tx1, ty0], tint, force_opaque),
        TexturedVertex::new(ndc(x1, y1), [tx1, ty1], tint, force_opaque),
        TexturedVertex::new(ndc(x0, y0), [tx0, ty0], tint, force_opaque),
        TexturedVertex::new(ndc(x1, y1), [tx1, ty1], tint, force_opaque),
        TexturedVertex::new(ndc(x0, y1), [tx0, ty1], tint, force_opaque),
    ]
}

fn ndc(x: f32, y: f32, target_width: u32, target_height: u32) -> [f32; 2] {
    let width = target_width as f32;
    let height = target_height as f32;
    [(x / width) * 2.0 - 1.0, 1.0 - (y / height) * 2.0]
}

#[cfg(all(feature = "winit-surface", target_os = "macos"))]
fn preferred_backends() -> wgpu::Backends {
    wgpu::Backends::METAL
}

#[cfg(all(feature = "winit-surface", not(target_os = "macos")))]
fn preferred_backends() -> wgpu::Backends {
    // `Backends::PRIMARY` has no GL.  A Linux session whose Vulkan driver is
    // not loadable (a bare nix shell, a machine without the loader path) would
    // otherwise have no backend able to create the window surface; the GL
    // backend covers it, and the display handle above gives it the Wayland
    // connection it needs.
    wgpu::Backends::PRIMARY | wgpu::Backends::GL
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
                // The under face (`under_draw_commands`): the scene beneath the
                // destination layer, which the kernels that fill a vacated
                // region with a colour need in order to composite it (`wave`).
                texture_bind_group_layout_entry(7),
                sampler_bind_group_layout_entry(8),
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

/// Creates (or replaces) one cached texture per upload: the texture, its view
/// and the bind group the draw path samples it through.  A free function so a
/// caller that draws outside the renderer — the offscreen text tests and their
/// cost probe — uploads through the exact path the renderer does.
#[allow(clippy::too_many_arguments)]
fn upload_images(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &TexturePipelineResources,
    format: wgpu::TextureFormat,
    capture_enabled: bool,
    textures: &mut BTreeMap<TextureId, CachedTexture>,
    uploads: &[ImageUpload],
) {
    for upload in uploads {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Kirakira uploaded texture"),
            size: wgpu::Extent3d {
                width: upload.width.max(1),
                height: upload.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: if capture_enabled {
                wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
            } else {
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST
            },
            view_formats: &[],
        });
        queue.write_texture(
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Kirakira texture bind group"),
            layout: &pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                },
            ],
        });
        textures.insert(
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

/// Everything a rasterized `DrawCommand::Text` depends on, and the only inputs
/// `FontSystem::rasterize_text` reads. Two text commands share a cached texture
/// only when this key matches, so a change to the font spec, the style, the
/// string or the font system's own state (`FontSystem::generation`) is a miss
/// that rasterizes afresh.
///
/// `height` is keyed by its bit pattern: `f32` has no total equality, and two
/// heights that differ in bits are two rasterizations even if they compare
/// equal.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TextImageKey {
    font_generation: u64,
    face: String,
    height_bits: u32,
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
    angle: i32,
    face_is_file_name: bool,
    rasterizer: String,
    color: [u8; 4],
    anti_alias: bool,
    shadow: Option<(i32, i32, [u8; 4])>,
    text: String,
}

impl TextImageKey {
    fn new(font_generation: u64, text: &TextCommand) -> Self {
        let font = &text.font;
        Self {
            font_generation,
            face: font.face.clone(),
            height_bits: font.height.to_bits(),
            bold: font.bold,
            italic: font.italic,
            underline: font.underline,
            strikeout: font.strikeout,
            angle: font.angle,
            face_is_file_name: font.face_is_file_name,
            rasterizer: font.rasterizer.clone(),
            color: text.style.color,
            anti_alias: text.style.anti_alias,
            shadow: text
                .style
                .shadow
                .map(|shadow| (shadow.offset_x, shadow.offset_y, shadow.color)),
            text: text.text.clone(),
        }
    }
}

/// The texture one text command's rasterization was uploaded to, and the image
/// geometry its draw command uses.
struct TextImageEntry {
    texture_id: TextureId,
    width: u32,
    height: u32,
}

/// Rasterized text images kept across frames, so an unchanged
/// `DrawCommand::Text` is neither re-rasterized nor re-uploaded every frame.
///
/// An entry names a texture the renderer holds — never one it dropped: the
/// texture is what the next frame's unchanged command draws.  The renderer
/// drops entries together with the textures `retain_frame_textures` dropped
/// ([`TextImageCache::retain_live_textures`]), which bounds the cache by the
/// last prepared frame's text — deliberately so: a text that leaves the draw
/// list for a frame is rasterized again when it comes back, instead of keeping
/// pixel buffers alive for an unbounded time.
#[derive(Default)]
struct TextImageCache {
    entries: BTreeMap<TextImageKey, TextImageEntry>,
    next_texture_id: TextureId,
}

impl TextImageCache {
    /// Text textures are named far above every id the engine mints for layers,
    /// so a `FrameOutput::image_releases` list can never name one.
    const FIRST_TEXTURE_ID: TextureId = 1 << 60;

    fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_texture_id: Self::FIRST_TEXTURE_ID,
        }
    }

    fn get(&self, key: &TextImageKey) -> Option<&TextImageEntry> {
        self.entries.get(key)
    }

    /// Rasterizes `text` and records the image under `key`; `None` for a text
    /// that rasterizes to no pixels (the command is dropped, as before).
    fn rasterize(
        &mut self,
        fonts: &FontSystem,
        key: TextImageKey,
        text: &TextCommand,
    ) -> Option<ImageUpload> {
        let image = fonts.rasterize_text(&text.font, text.style, &text.text);
        if image.width == 0 || image.height == 0 {
            return None;
        }
        let texture_id = self.next_texture_id;
        self.next_texture_id = self.next_texture_id.saturating_add(1);
        let upload = ImageUpload::new(texture_id, image.width, image.height, Arc::from(image.rgba));
        self.entries.insert(
            key,
            TextImageEntry {
                texture_id,
                width: image.width,
                height: image.height,
            },
        );
        Some(upload)
    }

    fn retain_live_textures(&mut self, texture_is_live: impl Fn(TextureId) -> bool) {
        self.entries
            .retain(|_, entry| texture_is_live(entry.texture_id));
    }
}

/// The draw commands one frame draws, with every [`DrawCommand::Text`]
/// replaced by the image command for its cached rasterization.  A command
/// whose text has no cached image rasterizes now; one whose image is already
/// cached contributes no upload at all — either way the draw list is the same
/// pixels, at the same position, with the same alpha, as rasterizing it inline
/// would produce.
///
/// `queued` carries the texture ids this frame has minted but not yet
/// uploaded (`upload_frame_images` runs after preparation): the call adds each
/// id it mints to the set, and treats an id in it as live, so a text that
/// repeats within one draw list — or in a later face of the same frame — shares
/// the one rasterization and upload instead of minting a second.
fn prepare_commands(
    commands: &[DrawCommand],
    fonts: &FontSystem,
    text_cache: &mut TextImageCache,
    queued: &mut BTreeSet<TextureId>,
    texture_is_live: impl Fn(TextureId) -> bool,
) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
    let mut prepared = Vec::with_capacity(commands.len());
    let mut uploads = Vec::new();
    for command in commands {
        match command {
            DrawCommand::Text(text) => {
                if let Some(command) = prepare_text_command(
                    fonts,
                    text_cache,
                    queued,
                    &texture_is_live,
                    text,
                    &mut uploads,
                ) {
                    prepared.push(command);
                }
            }
            _ => prepared.push(command.clone()),
        }
    }
    (prepared, uploads)
}

fn prepare_text_command(
    fonts: &FontSystem,
    text_cache: &mut TextImageCache,
    queued: &mut BTreeSet<TextureId>,
    texture_is_live: &dyn Fn(TextureId) -> bool,
    text: &TextCommand,
    uploads: &mut Vec<ImageUpload>,
) -> Option<DrawCommand> {
    let key = TextImageKey::new(fonts.generation(), text);
    let (texture_id, width, height) = match text_cache.get(&key) {
        Some(entry) if texture_is_live(entry.texture_id) || queued.contains(&entry.texture_id) => {
            (entry.texture_id, entry.width, entry.height)
        }
        // A hit whose texture the renderer no longer holds (the frame that
        // dropped the texture did not draw this text) is treated as a miss:
        // rasterizing again keeps the drawn pixels right, where an image
        // command naming a dropped texture would draw nothing at all.
        _ => {
            let upload = text_cache.rasterize(fonts, key, text)?;
            let geometry = (upload.texture_id, upload.width, upload.height);
            queued.insert(upload.texture_id);
            uploads.push(upload);
            geometry
        }
    };
    Some(DrawCommand::Image(ImageCommand {
        texture_id,
        rect: Rect::new(
            text.position.x,
            text.position.y,
            width as f32,
            height as f32,
        ),
        source_rect: Rect::new(0.0, 0.0, width as f32, height as f32),
        texture_size: Size::new(width as f32, height as f32),
        opacity: text.color.a,
        opaque: false,
    }))
}

/// The face textures one composite reads: the frozen scene (binding 0), the
/// incoming face (binding 2) and, for the kernels that need the scene beneath
/// the destination layer, the under face (binding 7; see
/// `wave_under_face_needed`).  `under` is `None` for every other kernel, and
/// `data[9].w` tells the shader so.
struct TransitionFaceViews<'a> {
    old: &'a wgpu::TextureView,
    new: &'a wgpu::TextureView,
    under: Option<&'a wgpu::TextureView>,
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
    force_opaque: f32,
    _pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TransitionUniforms {
    data: [[f32; 4]; 12],
}

/// One of the three faces a transition is composed from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransitionFace {
    /// The scene the transition started from (`tTVPDivisibleData::Src1`,
    /// `LayerIntf.cpp:6592`).  It is the composite's other input, never part of
    /// the incoming target.
    Old,
    /// The scene *without* the destination layer's subtree.
    ///
    /// Official's crossfade is `dest = lerp(src1, src2, p)` per channel
    /// including alpha (`const_alpha_blend_functor`, `blend_functor_c.h:584-594`;
    /// `sd_blend_func_c`, `blend_function.cpp:109-114`; called with `Phase` from
    /// `TransIntf.cpp:680`), and the layer manager composites the result.  The
    /// destination's bitmap is therefore *not* pre-composited over the scene
    /// when the blend runs, so the incoming face has to be drawn over the bare
    /// under-content: at a pixel the source does not cover, the destination's
    /// pixels fade out by `1 - p` and the scene beneath shows through.
    Under,
    /// The source layer's own bitmap (`tTVPDivisibleData::Src2`, `:6611`).
    Source,
}

/// The draw list behind one face.
fn transition_face_commands(transition: &FrameTransition, face: TransitionFace) -> &[DrawCommand] {
    match face {
        TransitionFace::Old => &transition.frozen_draw_commands,
        TransitionFace::Under => &transition.under_draw_commands,
        TransitionFace::Source => &transition.source_draw_commands,
    }
}

/// How a face starts its target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaceLoad {
    /// Clear to the frame colour, then draw the face.
    Clear,
    /// Draw the face over what the target already holds.
    Load,
}

/// Everything one transition's composite is built from, in render order.
///
/// Official's crossfade is `dest = lerp(src1, src2, p)` per channel including
/// alpha (`const_alpha_blend_functor`, `blend_functor_c.h:584-594`;
/// `sd_blend_func_c`, `blend_function.cpp:109-114`; caller `TransIntf.cpp:680`)
/// and the layer manager composites the *result* afterwards.  Rendering `old`
/// and `source-over-under` and mixing them at the progress expands to
/// `(1-p)·a_d·D + p·a_s·S + [(1-p)(1-a_d) + p(1-a_s)]·U`, which is the official
/// blend over the scene beneath the destination layer for every destination
/// alpha, source alpha and progress.
///
/// The value is the renderer's whole plan: it says which face feeds the
/// composite's `old` input, which this plan builds the incoming target from and
/// in what order, and whether each starts by clearing.  `Old` as the base would
/// hold the destination's content where official fades it out; dropping the
/// base would leave every transparent pixel of the source on the frame colour.
fn transition_composite_plan() -> TransitionCompositePlan {
    TransitionCompositePlan {
        old: TransitionFace::Old,
        incoming: [
            (TransitionFace::Under, FaceLoad::Clear),
            (TransitionFace::Source, FaceLoad::Load),
        ],
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TransitionCompositePlan {
    old: TransitionFace,
    incoming: [(TransitionFace, FaceLoad); 2],
}

fn transition_face_label(face: TransitionFace) -> &'static str {
    match face {
        TransitionFace::Old => "Kirakira transition frozen pass",
        TransitionFace::Under => "Kirakira transition under pass",
        TransitionFace::Source => "Kirakira transition source pass",
    }
}

/// Whether the kernel needs the under face as its own texture.
///
/// The reference's handlers composite into the destination layer's *own*
/// bitmap, so a region they overwrite with a colour (`TVPFillARGB`) shows that
/// colour over the scene beneath the layer once the layer manager composites it
/// (`LayerIntf.cpp:6513-6540`).  The incoming face cannot stand in for that
/// scene: it already has the source layer drawn over it.  `wave` is the kernel
/// with such a region (`extrans/wave.cpp:203-221`); its strip must composite
/// `bgcolor` over the under face, which is what the renderer binds at
/// `transition.wgsl`'s binding 7.
fn wave_under_face_needed(method: TransitionMethod) -> bool {
    matches!(method, TransitionMethod::Wave)
}

/// Everything `transition.wgsl` reads for one composite.
///
/// The first eight slots are the option/parameter block; `data[8..11]` carry the
/// geometry and clock the extrans kernels need on top of it (the destination
/// layer's rectangle, the logical-to-physical transform, and the transition's
/// duration in milliseconds) -- see the accessors at the top of
/// `transition.wgsl`.
fn transition_uniforms(
    transition: &FrameTransition,
    viewport_width: f32,
    viewport_height: f32,
    transform: RenderTransform,
    content_size: Size,
    under_available: bool,
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
    // `tTVPDivisibleData::Dest` (`LayerIntf.cpp:6513-6540`) in logical frame
    // pixels.  A destination without measurable geometry makes the composite
    // cover the whole frame, and the reference's handlers then work on the
    // whole layer bitmap, which is the frame's content size here.
    let image_rect = transition
        .dest_rect
        .filter(|rect| rect.width > 0.0 && rect.height > 0.0)
        .map(|rect| [rect.x, rect.y, rect.width, rect.height])
        .unwrap_or([
            0.0,
            0.0,
            content_size.width.max(1.0),
            content_size.height.max(1.0),
        ]);
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
            image_rect,
            [
                transform.x_scale,
                transform.x_offset,
                transform.y_offset,
                if under_available { 1.0 } else { 0.0 },
            ],
            [params.duration_millis.max(0.0), 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.0],
        ],
    }
}

fn color_uniform(color: Color) -> [f32; 4] {
    [color.r, color.g, color.b, color.a]
}

impl TransitionUniforms {
    /// The `wave` kernel's view of this uniform block, for the CPU mirror
    /// (`wave.rs`) and its tests: the same slots the shader's accessors read.
    #[cfg(test)]
    fn wave_frame(&self) -> wave::WaveFrame {
        wave::WaveFrame {
            progress: self.data[0][0],
            duration_millis: self.data[10][0],
            viewport: [self.data[1][0], self.data[1][1]],
            scale: self.data[9][0],
            origin: [self.data[9][1], self.data[9][2]],
            image_rect: self.data[8],
            wave_type: self.data[2][1],
            max_h: self.data[2][2],
            max_omega: self.data[2][3],
            bg_color1: self.data[3],
            bg_color2: self.data[4],
            under_available: self.data[9][3] >= 0.5,
        }
    }
}

impl TexturedVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 4] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32];

    const fn new(
        position: [f32; 2],
        tex_coord: [f32; 2],
        tint: [f32; 4],
        force_opaque: f32,
    ) -> Self {
        Self {
            position,
            tex_coord,
            tint,
            force_opaque,
            _pad: [0.0; 3],
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

#[cfg(test)]
mod tests {
    use super::*;
    use krkr_core::{FontSpec, Point, Rect, RectCommand, ShadowStyle, TextStyle, TransitionParams};

    fn transition(
        frozen: Vec<DrawCommand>,
        under: Vec<DrawCommand>,
        source: Vec<DrawCommand>,
    ) -> FrameTransition {
        FrameTransition {
            method: "crossfade".to_string(),
            progress: 0.5,
            params: TransitionParams::default(),
            dest_rect: Some(Rect::new(0.0, 0.0, 4.0, 4.0)),
            rule_texture_id: None,
            rule_image_upload: None,
            frozen_draw_commands: frozen,
            frozen_image_uploads: Vec::new(),
            under_draw_commands: under,
            under_image_uploads: Vec::new(),
            source_draw_commands: source,
            source_image_uploads: Vec::new(),
        }
    }

    // ---------------------------------------------------------------------
    // The `wave` kernel: the GPU path against the CPU mirror, and the CPU
    // mirror against the reference C++ (`wave.rs` holds the latter tests).
    // ---------------------------------------------------------------------

    const WAVE_WIDTH: u32 = 64;
    const WAVE_HEIGHT: u32 = 16;

    /// The same bitmaps the CPU fidelity test uses (`wave.rs`), so both sides
    /// read identical inputs.
    fn wave_dest_pixel(x: i32, y: i32) -> [u8; 4] {
        [(x * 27) as u8, (y * 41) as u8, (128 + x * 7) as u8, 255]
    }

    fn wave_source_pixel(x: i32, y: i32) -> [u8; 4] {
        [
            (255 - x * 23) as u8,
            (255 - y * 31) as u8,
            (x * 11 + y * 3) as u8,
            255,
        ]
    }

    fn wave_uploads(bitmap: fn(i32, i32) -> [u8; 4]) -> Vec<u8> {
        let mut data = Vec::with_capacity((WAVE_WIDTH * WAVE_HEIGHT * 4) as usize);
        for y in 0..WAVE_HEIGHT as i32 {
            for x in 0..WAVE_WIDTH as i32 {
                data.extend_from_slice(&bitmap(x, y));
            }
        }
        data
    }

    /// Drives the device-request futures in this module's test to completion.
    ///
    /// The renderer is async, a test cannot be, and the two requests resolve on
    /// wgpu's own threads: poll them until they are ready instead of pulling in
    /// an executor dependency for it.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut future = Box::pin(future);
        let mut context = Context::from_waker(Waker::noop());
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
    }

    /// A headless device for the shader-level test.  `None` where the host has
    /// no adapter, so the test reports that instead of failing.
    fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Kirakira transition kernel test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .ok()
    }

    fn wave_transition(progress: f32) -> FrameTransition {
        FrameTransition {
            method: "wave".to_string(),
            progress,
            params: TransitionParams {
                method: TransitionMethod::Wave,
                wave_type: 0.0,
                max_h: 50.0,
                max_omega: 0.2,
                // `wave`'s own default: transparent black (`wave.cpp:327-331`).
                bg_color1: Color::new(0.0, 0.0, 0.0, 0.0),
                bg_color2: Color::new(0.0, 0.0, 0.0, 0.0),
                duration_millis: 1000.0,
                ..TransitionParams::default()
            },
            dest_rect: Some(Rect::new(0.0, 0.0, WAVE_WIDTH as f32, WAVE_HEIGHT as f32)),
            rule_texture_id: None,
            rule_image_upload: None,
            frozen_draw_commands: Vec::new(),
            frozen_image_uploads: Vec::new(),
            under_draw_commands: Vec::new(),
            under_image_uploads: Vec::new(),
            source_draw_commands: Vec::new(),
            source_image_uploads: Vec::new(),
        }
    }

    /// The GPU path is the one that ships: this renders the composite pass with
    /// `transition.wgsl`'s `transition_wave` on a real device and compares every
    /// output pixel against the CPU mirror, which the tests in `wave.rs` pin
    /// against the reference C++ in turn.  Hosts without an adapter report the
    /// skip instead of failing.
    #[test]
    fn wave_shader_matches_the_cpu_kernel_on_the_gpu() {
        let Some((device, queue)) = headless_device() else {
            eprintln!("no wgpu adapter: the wave kernel's GPU path was not verified on this host");
            return;
        };
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pipeline = TransitionPipelineResources::new(&device, format);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Kirakira transition kernel test sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let upload = |bitmap: fn(i32, i32) -> [u8; 4], label: &str| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: WAVE_WIDTH,
                    height: WAVE_HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &wave_uploads(bitmap),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(WAVE_WIDTH * 4),
                    rows_per_image: Some(WAVE_HEIGHT),
                },
                wgpu::Extent3d {
                    width: WAVE_WIDTH,
                    height: WAVE_HEIGHT,
                    depth_or_array_layers: 1,
                },
            );
            texture.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let old_view = upload(wave_dest_pixel, "Kirakira test wave old face");
        let new_view = upload(wave_source_pixel, "Kirakira test wave new face");
        let under_data: [u8; 4] = [51, 102, 153, 255];
        let under_view = {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Kirakira test wave under face"),
                size: wgpu::Extent3d {
                    width: WAVE_WIDTH,
                    height: WAVE_HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &under_data.repeat((WAVE_WIDTH * WAVE_HEIGHT) as usize),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(WAVE_WIDTH * 4),
                    rows_per_image: Some(WAVE_HEIGHT),
                },
                wgpu::Extent3d {
                    width: WAVE_WIDTH,
                    height: WAVE_HEIGHT,
                    depth_or_array_layers: 1,
                },
            );
            texture.create_view(&wgpu::TextureViewDescriptor::default())
        };

        let expected_under = [
            under_data[0] as f32 / 255.0,
            under_data[1] as f32 / 255.0,
            under_data[2] as f32 / 255.0,
            under_data[3] as f32 / 255.0,
        ];
        let transform = RenderTransform {
            x_scale: 1.0,
            y_scale: 1.0,
            x_offset: 0.0,
            y_offset: 0.0,
        };
        let bitmaps = [wave_dest_pixel, wave_source_pixel];
        let sample = |bitmap: fn(i32, i32) -> [u8; 4], uv: [f32; 2]| -> [f32; 4] {
            let x = (uv[0] * WAVE_WIDTH as f32 - 0.5).round() as i32;
            let y = (uv[1] * WAVE_HEIGHT as f32 - 0.5).round() as i32;
            if x < 0 || y < 0 || x >= WAVE_WIDTH as i32 || y >= WAVE_HEIGHT as i32 {
                return [0.0, 0.0, 0.0, 0.0];
            }
            let pixel = bitmap(x, y);
            [
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
                pixel[3] as f32 / 255.0,
            ]
        };

        let mut max_deviation = 0.0f32;
        // `(progress, duration_millis)`: the progress sweep on a 1 s clock, plus
        // the clamped-duration cases -- `1 ms` must run as the reference's 2 ms
        // (`wave.cpp:336`) in the shader too, and `0` is the untimed fallback.
        let cases = [(0.5f32, 1.0f32), (0.5, 2.0), (0.5, 0.0)]
            .into_iter()
            .chain((0..=10).map(|step| (step as f32 / 10.0, 1000.0)));
        for (progress, duration_millis) in cases {
            let mut transition = wave_transition(progress);
            transition.params.duration_millis = duration_millis;
            let uniforms = transition_uniforms(
                &transition,
                WAVE_WIDTH as f32,
                WAVE_HEIGHT as f32,
                transform,
                Size::new(WAVE_WIDTH as f32, WAVE_HEIGHT as f32),
                true,
            );
            let frame = uniforms.wave_frame();
            let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Kirakira test wave uniforms"),
                contents: bytemuck::cast_slice(&[uniforms]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Kirakira test wave bind group"),
                layout: &pipeline.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&old_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&new_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&old_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(&under_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Kirakira test wave target"),
                size: wgpu::Extent3d {
                    width: WAVE_WIDTH,
                    height: WAVE_HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Kirakira test wave readback"),
                size: u64::from(WAVE_WIDTH * 4 * WAVE_HEIGHT),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            {
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Kirakira test wave encoder"),
                });
                let tint = [1.0, 1.0, 1.0, 1.0];
                let vertices = [
                    TexturedVertex::new([-1.0, 1.0], [0.0, 0.0], tint, 0.0),
                    TexturedVertex::new([1.0, 1.0], [1.0, 0.0], tint, 0.0),
                    TexturedVertex::new([1.0, -1.0], [1.0, 1.0], tint, 0.0),
                    TexturedVertex::new([-1.0, 1.0], [0.0, 0.0], tint, 0.0),
                    TexturedVertex::new([1.0, -1.0], [1.0, 1.0], tint, 0.0),
                    TexturedVertex::new([-1.0, -1.0], [0.0, 1.0], tint, 0.0),
                ];
                let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Kirakira test wave vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Kirakira test wave pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&pipeline.pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.draw(0..6, 0..1);
                drop(pass);
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: &target,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &readback,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(WAVE_WIDTH * 4),
                            rows_per_image: Some(WAVE_HEIGHT),
                        },
                    },
                    wgpu::Extent3d {
                        width: WAVE_WIDTH,
                        height: WAVE_HEIGHT,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit([encoder.finish()]);
            }
            let slice = readback.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("poll");
            rx.recv().expect("map").expect("mapped");
            let mapped = slice.get_mapped_range().to_vec();
            readback.unmap();

            for y in 0..WAVE_HEIGHT {
                for x in 0..WAVE_WIDTH {
                    let index = ((y * WAVE_WIDTH + x) * 4) as usize;
                    let gpu = [
                        mapped[index] as f32 / 255.0,
                        mapped[index + 1] as f32 / 255.0,
                        mapped[index + 2] as f32 / 255.0,
                        mapped[index + 3] as f32 / 255.0,
                    ];
                    let uv = [
                        (x as f32 + 0.5) / WAVE_WIDTH as f32,
                        (y as f32 + 0.5) / WAVE_HEIGHT as f32,
                    ];
                    let cpu = wave::wave_pixel(
                        &frame,
                        uv,
                        &|uv| sample(bitmaps[0], uv),
                        &|uv| sample(bitmaps[1], uv),
                        &|_| expected_under,
                    );
                    for channel in 0..4 {
                        let deviation = (gpu[channel] - cpu[channel]).abs();
                        max_deviation = max_deviation.max(deviation);
                        // The device quantizes to Rgba8Unorm, so one 8-bit step is
                        // the ceiling; the measured maximum is half of that.
                        assert!(
                            deviation <= 1.0 / 255.0,
                            "progress {}: pixel ({x}, {y}) channel {channel}: gpu {} vs cpu {}",
                            transition.progress,
                            gpu[channel],
                            cpu[channel]
                        );
                    }
                }
            }
        }
        println!("wave shader vs CPU mirror: max per-channel deviation {max_deviation:.5}");
    }

    /// The rule-driven `universal` transition's real shader at two window
    /// scales.  The rule is sampled in the destination bitmap's logical pixels
    /// (`image_local`), so its black/white split stays at the middle of the
    /// *game's* screen however much larger the physical window is -- the fix
    /// for the reported "four small copies in a 2x2 grid".  A sample taken in
    /// window space (the kernel before M206) tiles the rule 2x2 here, which
    /// flips both assertions.
    #[test]
    fn universal_rule_ignores_the_window_scale_on_the_gpu() {
        let Some((device, queue)) = headless_device() else {
            eprintln!(
                "no wgpu adapter: the universal kernel's GPU path was not verified on this host"
            );
            return;
        };
        const W: u32 = 64;
        const H: u32 = 16;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pipeline = TransitionPipelineResources::new(&device, format);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Kirakira test universal sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let upload = |pixel: fn(u32, u32) -> [u8; 4], label: &str| {
            let mut data = Vec::with_capacity((W * H * 4) as usize);
            for y in 0..H {
                for x in 0..W {
                    data.extend_from_slice(&pixel(x, y));
                }
            }
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: W,
                    height: H,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(W * 4),
                    rows_per_image: Some(H),
                },
                wgpu::Extent3d {
                    width: W,
                    height: H,
                    depth_or_array_layers: 1,
                },
            );
            texture.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let old_view = upload(
            |_, _| [220, 30, 20, 255],
            "Kirakira test universal old face",
        );
        let new_view = upload(
            |_, _| [20, 40, 230, 255],
            "Kirakira test universal new face",
        );
        let rule_view = upload(
            |x, _| {
                if x < W / 2 {
                    [0, 0, 0, 255]
                } else {
                    [255, 255, 255, 255]
                }
            },
            "Kirakira test universal rule",
        );

        let transition = FrameTransition {
            method: "universal".to_string(),
            progress: 0.5,
            params: TransitionParams {
                method: TransitionMethod::Universal,
                vague: 64.0,
                duration_millis: 1000.0,
                ..TransitionParams::default()
            },
            dest_rect: Some(Rect::new(0.0, 0.0, W as f32, H as f32)),
            rule_texture_id: Some(1),
            rule_image_upload: None,
            frozen_draw_commands: Vec::new(),
            frozen_image_uploads: Vec::new(),
            under_draw_commands: Vec::new(),
            under_image_uploads: Vec::new(),
            source_draw_commands: Vec::new(),
            source_image_uploads: Vec::new(),
        };

        let render = |uniforms: &TransitionUniforms, width: u32, height: u32| -> Vec<u8> {
            let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Kirakira test universal uniforms"),
                contents: bytemuck::cast_slice(&[*uniforms]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Kirakira test universal bind group"),
                layout: &pipeline.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&old_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&new_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&rule_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(&new_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Kirakira test universal target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Kirakira test universal readback"),
                size: u64::from(width * 4 * height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            {
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Kirakira test universal encoder"),
                });
                let tint = [1.0, 1.0, 1.0, 1.0];
                let vertices = [
                    TexturedVertex::new([-1.0, 1.0], [0.0, 0.0], tint, 0.0),
                    TexturedVertex::new([1.0, 1.0], [1.0, 0.0], tint, 0.0),
                    TexturedVertex::new([1.0, -1.0], [1.0, 1.0], tint, 0.0),
                    TexturedVertex::new([-1.0, 1.0], [0.0, 0.0], tint, 0.0),
                    TexturedVertex::new([1.0, -1.0], [1.0, 1.0], tint, 0.0),
                    TexturedVertex::new([-1.0, -1.0], [0.0, 1.0], tint, 0.0),
                ];
                let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Kirakira test universal vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Kirakira test universal pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&pipeline.pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.draw(0..6, 0..1);
                drop(pass);
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: &target,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &readback,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(width * 4),
                            rows_per_image: Some(height),
                        },
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit([encoder.finish()]);
            }
            let slice = readback.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("poll");
            rx.recv().expect("map").expect("mapped");
            let mapped = slice.get_mapped_range().to_vec();
            readback.unmap();
            mapped
        };

        // `(physical viewport, render scale)`: the game's screen is 64x16
        // logical pixels; a 2x window renders it into 128x32.
        for (viewport, scale) in [((W, H), 1.0f32), ((W * 2, H * 2), 2.0)] {
            let uniforms = transition_uniforms(
                &transition,
                viewport.0 as f32,
                viewport.1 as f32,
                RenderTransform {
                    x_scale: scale,
                    y_scale: scale,
                    x_offset: 0.0,
                    y_offset: 0.0,
                },
                Size::new(W as f32, H as f32),
                false,
            );
            let mapped = render(&uniforms, viewport.0, viewport.1);
            let pixel = |x: u32, y: u32| -> [f32; 4] {
                let index = ((y * viewport.0 + x) * 4) as usize;
                [
                    mapped[index] as f32 / 255.0,
                    mapped[index + 1] as f32 / 255.0,
                    mapped[index + 2] as f32 / 255.0,
                    mapped[index + 3] as f32 / 255.0,
                ]
            };
            // Both samples sit eight logical pixels either side of the rule's
            // split, at the vertical centre of the frame.
            let center_y = viewport.1 / 2;
            let left = pixel(viewport.0 / 2 - 8 * scale as u32, center_y);
            assert!(
                left[2] > 0.75 && left[0] < 0.25,
                "at {scale}x the rule's black half must take the incoming (blue) \
                 face, got {left:?}"
            );
            let right = pixel(viewport.0 / 2 + 8 * scale as u32, center_y);
            assert!(
                right[0] > 0.75 && right[2] < 0.25,
                "at {scale}x the rule's white half must keep the outgoing (red) \
                 face, got {right:?}"
            );
        }
    }

    /// The shader text and the CPU mirror must carry the same constants and the
    /// same branch conditions; the GPU test proves the formula, this catches the
    /// two texts drifting apart in the parts a text check can see.
    #[test]
    fn wave_shader_carries_the_cpu_kernel_constants() {
        let source = include_str!("transition.wgsl");
        let body = |name: &str| -> &str {
            let (_, tail) = source
                .split_once(name)
                .unwrap_or_else(|| panic!("transition.wgsl has {name}"));
            &tail[..tail.find("\n}").expect("the function has an end")]
        };
        let kernel = body("fn transition_wave(");
        let ratio = body("fn wave_blend_ratio(");
        for marker in [
            "3.14159265359",
            "floor(local.y)",
            "trunc(sin(rad) * cur_h)",
            "let src_x = local.x - d;",
            "floor(image.w * 0.5)",
            "bg.rgb * bg.a + under.rgb * (1.0 - bg.a)",
            "under_available()",
            "duration_millis()",
            "image_rect()",
        ] {
            assert!(
                kernel.contains(marker),
                "the shader's wave kernel lost `{marker}`; wave.rs carries it"
            );
        }
        for marker in ["255.0", "256.0"] {
            assert!(
                ratio.contains(marker),
                "the shader's wave blend ratio lost `{marker}`"
            );
        }
        // The shader's π literal, parsed back from its own text, is the mirror's
        // constant bit for bit.
        let pi = "3.14159265359";
        assert!(
            kernel.contains(pi),
            "the shader's wave kernel lost its π literal"
        );
        assert_eq!(
            wave::WAVE_PI.to_bits(),
            pi.parse::<f32>().expect("a float literal").to_bits()
        );
        assert_eq!(wave::WAVE_RATIO_STEPS, 255.0);
        assert_eq!(wave::WAVE_RATIO_DIVISOR, 256.0);
    }

    /// Only the kernels that fill a vacated region with the background colour
    /// need the under face; today that is `wave` (`extrans/wave.cpp:203-221`).
    #[test]
    fn only_wave_asks_for_the_under_face() {
        for method in [
            TransitionMethod::Crossfade,
            TransitionMethod::Universal,
            TransitionMethod::Scroll,
            TransitionMethod::Mosaic,
            TransitionMethod::Turn,
            TransitionMethod::RotateZoom,
            TransitionMethod::RotateVanish,
            TransitionMethod::RotateSwap,
            TransitionMethod::Ripple,
        ] {
            assert!(!wave_under_face_needed(method), "{method:?}");
        }
        assert!(wave_under_face_needed(TransitionMethod::Wave));
    }

    fn image(texture_id: TextureId) -> DrawCommand {
        DrawCommand::Image(ImageCommand {
            texture_id,
            rect: Rect::new(0.0, 0.0, 4.0, 4.0),
            source_rect: Rect::new(0.0, 0.0, 4.0, 4.0),
            texture_size: Size::new(4.0, 4.0),
            opacity: 1.0,
            opaque: false,
        })
    }

    /// The whole composition, as the renderer must perform it: `Old` feeds the
    /// composite, and the incoming target is the under-content cleared and then
    /// the source drawn over it.  Reversing any of that -- `Old` as the base,
    /// the source cleared instead of loaded, the two swapped -- fails here.
    #[test]
    fn composite_plan_is_the_official_composition() {
        let plan = transition_composite_plan();
        assert_eq!(plan.old, TransitionFace::Old);
        assert_eq!(
            plan.incoming,
            [
                (TransitionFace::Under, FaceLoad::Clear),
                (TransitionFace::Source, FaceLoad::Load),
            ]
        );

        let transition = transition(vec![image(1)], vec![image(2)], vec![image(3)]);
        // Each face reads its own list.
        assert!(matches!(
            transition_face_commands(&transition, plan.old),
            [DrawCommand::Image(image)] if image.texture_id == 1
        ));
        assert!(matches!(
            transition_face_commands(&transition, plan.incoming[0].0),
            [DrawCommand::Image(image)] if image.texture_id == 2
        ));
        assert!(matches!(
            transition_face_commands(&transition, plan.incoming[1].0),
            [DrawCommand::Image(image)] if image.texture_id == 3
        ));
    }

    /// Straight-alpha source-over, the blend the renderer's pipeline uses to
    /// draw one face into a target.
    fn over(src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for channel in 0..3 {
            out[channel] = src[channel] * src[3] + dst[channel] * (1.0 - src[3]);
        }
        out[3] = src[3] + dst[3] * (1.0 - src[3]);
        out
    }

    /// The composite the plan produces for one pixel, together with the
    /// official blend it has to reproduce.
    ///
    /// `destination` is the destination layer's own bitmap (`Src1`) with its
    /// alpha, `under` the scene beneath it, `source` the source layer's bitmap
    /// (`Src2`) with its alpha.
    fn plan_result(
        plan: TransitionCompositePlan,
        destination: [f32; 4],
        under: [f32; 4],
        source: [f32; 4],
        progress: f32,
    ) -> ([f32; 4], [f32; 4]) {
        let clear = [0.0, 0.0, 0.0, 1.0];
        let scene = over(destination, under);
        let face_value = |face: TransitionFace| match face {
            TransitionFace::Old => scene,
            TransitionFace::Under => under,
            TransitionFace::Source => source,
        };
        let mut target = clear;
        for (face, load) in plan.incoming {
            target = match load {
                FaceLoad::Clear => over(face_value(face), clear),
                FaceLoad::Load => over(face_value(face), target),
            };
        }
        let old = face_value(plan.old);
        let mut result = [0.0f32; 4];
        for channel in 0..4 {
            result[channel] = old[channel] * (1.0 - progress) + target[channel] * progress;
        }
        // The official crossfade, expanded: `TVPConstAlphaBlend_SD` lerps the
        // two layer bitmaps per channel including alpha
        // (`blend_functor_c.h:584-594`), and the layer manager composites the
        // result over the scene beneath the destination layer -- the shared
        // under-content keeps weight `(1-p)(1-a_d) + p(1-a_s)`.
        let destination_alpha = destination[3];
        let source_alpha = source[3];
        let mut official = [0.0f32; 4];
        for channel in 0..3 {
            official[channel] = destination_alpha * destination[channel] * (1.0 - progress)
                + source_alpha * source[channel] * progress
                + ((1.0 - destination_alpha) * (1.0 - progress) + (1.0 - source_alpha) * progress)
                    * under[channel];
        }
        official[3] = 1.0;
        (result, official)
    }

    #[test]
    fn composite_plan_reproduces_the_official_blend_for_every_alpha_and_progress() {
        let plan = transition_composite_plan();
        let under = [0.1f32, 0.2, 0.3, 1.0];
        let source_colour = [0.9f32, 0.1, 0.4, 1.0];
        let destination_colour = [0.2f32, 0.7, 0.5, 1.0];
        for destination_alpha in [0.0f32, 0.25, 0.5, 1.0] {
            for source_alpha in [0.0f32, 0.25, 0.5, 1.0] {
                for progress in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
                    let mut destination = destination_colour;
                    destination[3] = destination_alpha;
                    let mut source = source_colour;
                    source[3] = source_alpha;
                    let (result, official) =
                        plan_result(plan, destination, under, source, progress);
                    for channel in 0..4 {
                        assert!(
                            (result[channel] - official[channel]).abs() < 1e-6,
                            "channel {channel} at destination alpha {destination_alpha}, \
                             source alpha {source_alpha}, progress {progress}: \
                             {} vs {}",
                            result[channel],
                            official[channel]
                        );
                    }
                }
            }
        }

        // The discriminating cases: with the destination opaque and the source
        // transparent, `Old` as the base holds the destination's content where
        // official fades it out, and a cleared source pass replaces it with the
        // frame colour.
        let opaque_destination = [0.2f32, 0.7, 0.5, 1.0];
        let transparent_source = [0.0f32, 0.0, 0.0, 0.0];
        let (held, faded) = plan_result(
            TransitionCompositePlan {
                old: TransitionFace::Old,
                incoming: [
                    (TransitionFace::Old, FaceLoad::Clear),
                    (TransitionFace::Source, FaceLoad::Load),
                ],
            },
            opaque_destination,
            under,
            transparent_source,
            0.75,
        );
        // Holding the destination is exactly what the wrong plan does.
        assert_eq!(held, opaque_destination);
        assert!((held[0] - faded[0]).abs() > 0.02);
        // A plan whose first pass clears without the under-content leaves the
        // source sitting on the frame colour.
        let (cleared, faded) = plan_result(
            TransitionCompositePlan {
                old: TransitionFace::Old,
                incoming: [
                    (TransitionFace::Source, FaceLoad::Clear),
                    (TransitionFace::Source, FaceLoad::Load),
                ],
            },
            opaque_destination,
            under,
            transparent_source,
            0.75,
        );
        assert!((cleared[0] - faded[0]).abs() > 0.02);
    }

    // ---------------------------------------------------------------------
    // The text image cache: an unchanged `DrawCommand::Text` must not be
    // rasterized or uploaded again, and what it draws must be exactly what
    // rasterizing it inline draws.
    // ---------------------------------------------------------------------

    fn probe_text(text: &str, style: TextStyle, font: FontSpec) -> TextCommand {
        let size = font.height;
        TextCommand {
            position: Point::new(12.0, 34.0),
            text: text.to_string(),
            color: Color::new(1.0, 1.0, 1.0, 0.75),
            size,
            font,
            style,
        }
    }

    /// The renderer's own `prepare_commands` wiring, minus the device:
    /// `textures` are the ids the renderer holds from earlier frames, `queued`
    /// the ids this frame has minted but not yet uploaded — exactly the pair
    /// `Renderer::prepare_commands` keys liveness on, so these tests drive the
    /// live-set the production path uses (a constant predicate would hide a
    /// within-frame miss).
    fn prepare_text_commands(
        commands: &[DrawCommand],
        fonts: &FontSystem,
        cache: &mut TextImageCache,
        textures: &BTreeSet<TextureId>,
        queued: &mut BTreeSet<TextureId>,
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        prepare_commands(commands, fonts, cache, queued, |id| textures.contains(&id))
    }

    /// The ids a frame's uploads put into the renderer's texture map: what the
    /// next frame's preparation sees as live.
    fn uploaded_textures(uploads: &[ImageUpload]) -> BTreeSet<TextureId> {
        uploads.iter().map(|upload| upload.texture_id).collect()
    }

    /// One frame's preparation that starts with nothing held from an earlier
    /// frame.
    fn prepare_fresh_frame(
        commands: &[DrawCommand],
        fonts: &FontSystem,
        cache: &mut TextImageCache,
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        prepare_text_commands(
            commands,
            fonts,
            cache,
            &BTreeSet::new(),
            &mut BTreeSet::new(),
        )
    }

    /// One frame's preparation that still holds `textures` from earlier frames.
    fn prepare_frame_holding(
        commands: &[DrawCommand],
        fonts: &FontSystem,
        cache: &mut TextImageCache,
        textures: &BTreeSet<TextureId>,
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        prepare_text_commands(commands, fonts, cache, textures, &mut BTreeSet::new())
    }

    #[test]
    fn unchanged_text_rasterizes_and_uploads_once() {
        let fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let command = DrawCommand::Text(probe_text(
            "a cached line of text",
            TextStyle::default(),
            FontSpec::default(),
        ));

        let (first_commands, first_uploads) =
            prepare_fresh_frame(std::slice::from_ref(&command), &fonts, &mut cache);
        assert_eq!(
            first_uploads.len(),
            1,
            "the first frame rasterizes and uploads"
        );
        let [DrawCommand::Image(first_image)] = &first_commands[..] else {
            panic!("expected one image command, got {first_commands:?}");
        };
        assert_eq!(first_image.texture_id, first_uploads[0].texture_id);

        let (second_commands, second_uploads) = prepare_frame_holding(
            std::slice::from_ref(&command),
            &fonts,
            &mut cache,
            &uploaded_textures(&first_uploads),
        );
        assert!(
            second_uploads.is_empty(),
            "an unchanged text must not upload again"
        );
        assert_eq!(
            second_commands, first_commands,
            "same pixels, same texture, same geometry"
        );
    }

    #[test]
    fn cached_text_is_drawn_as_the_rasterizations_own_bytes() {
        let fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let text = probe_text("pixel probe", TextStyle::default(), FontSpec::default());
        let expected = fonts.rasterize_text(&text.font, text.style, &text.text);

        let (commands, uploads) =
            prepare_fresh_frame(&[DrawCommand::Text(text.clone())], &fonts, &mut cache);
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].width, expected.width);
        assert_eq!(uploads[0].height, expected.height);
        assert_eq!(uploads[0].rgba.as_ref(), expected.rgba.as_slice());

        let [DrawCommand::Image(image)] = &commands[..] else {
            panic!("expected one image command, got {commands:?}");
        };
        assert_eq!(
            image.rect,
            Rect::new(12.0, 34.0, expected.width as f32, expected.height as f32)
        );
        assert_eq!(image.opacity, text.color.a);
    }

    /// The text-heavy frame the cache is held to: a message box background, a
    /// layer image the engine uploaded itself, three dialogue lines (one of
    /// them repeated, the way a line can appear on more than one page), a
    /// shadowed line at another height, and an empty text.  The empty string
    /// rasterizes to a transparent image and draws nothing either way, so it
    /// pins the "no pixels" case as much as the visible lines.
    fn message_box_frame() -> Vec<DrawCommand> {
        let style = TextStyle {
            color: [255, 255, 255, 255],
            anti_alias: true,
            shadow: None,
        };
        let font = FontSpec {
            height: 24.0,
            ..FontSpec::default()
        };
        let mut commands = vec![
            DrawCommand::Rect(RectCommand {
                rect: Rect::new(0.0, 240.0, 640.0, 160.0),
                color: Color::new(0.05, 0.05, 0.1, 0.85),
            }),
            // A texture the engine uploaded (a layer image), not one of the
            // text cache's: the text preparation must leave it untouched.
            DrawCommand::Image(ImageCommand {
                texture_id: 7,
                rect: Rect::new(8.0, 8.0, 96.0, 96.0),
                source_rect: Rect::new(0.0, 0.0, 96.0, 96.0),
                texture_size: Size::new(96.0, 96.0),
                opacity: 1.0,
                opaque: false,
            }),
        ];
        for (index, line) in [
            "こんにちは、世界。",
            "きょうも いい てんき ですね。",
            "こんにちは、世界。",
        ]
        .into_iter()
        .enumerate()
        {
            let mut text = probe_text(line, style, font.clone());
            text.position = Point::new(24.0, 256.0 + index as f32 * 23.0);
            text.color = Color::new(1.0, 1.0, 1.0, 0.75);
            commands.push(DrawCommand::Text(text));
        }
        let mut shadowed = probe_text(
            "影つきの一行",
            TextStyle {
                shadow: Some(ShadowStyle {
                    offset_x: 2,
                    offset_y: 2,
                    color: [0, 0, 0, 160],
                }),
                ..style
            },
            FontSpec {
                height: 32.0,
                ..font.clone()
            },
        );
        shadowed.position = Point::new(24.0, 344.0);
        shadowed.color = Color::new(1.0, 0.9, 0.8, 1.0);
        commands.push(DrawCommand::Text(shadowed));
        let mut empty = probe_text("", style, font);
        empty.position = Point::new(24.0, 384.0);
        commands.push(DrawCommand::Text(empty));
        commands
    }

    /// The pre-change text preparation, verbatim: every text rasterizes inline,
    /// mints a fresh texture id and uploads under it, with no reuse across
    /// commands or frames.  The cache has to draw exactly what this produces.
    fn prepare_commands_inline_reference(
        commands: &[DrawCommand],
        fonts: &FontSystem,
        next_texture_id: &mut TextureId,
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        let mut prepared = Vec::with_capacity(commands.len());
        let mut uploads = Vec::new();
        for command in commands {
            match command {
                DrawCommand::Text(text) => {
                    let image = fonts.rasterize_text(&text.font, text.style, &text.text);
                    if image.width == 0 || image.height == 0 {
                        continue;
                    }
                    let texture_id = *next_texture_id;
                    *next_texture_id = next_texture_id.saturating_add(1);
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
                        opaque: false,
                    }));
                }
                _ => prepared.push(command.clone()),
            }
        }
        (prepared, uploads)
    }

    /// One prepared command reduced to what decides the pixels it draws: its
    /// geometry, its blend inputs, and — for an image this frame uploaded —
    /// the bytes behind its texture.  Texture ids are deliberately absent:
    /// reusing an id across frames is the point of the cache, and the bytes a
    /// live id holds are compared instead.
    #[derive(Debug, PartialEq)]
    enum PixelCommand {
        Rect {
            rect: Rect,
            color: Color,
        },
        Image {
            rect: Rect,
            source_rect: Rect,
            texture_size: Size,
            opacity: f32,
            opaque: bool,
            /// `None` for a texture this preparation did not upload (an engine
            /// layer image), whose pixels are the engine's business.
            rgba: Option<Arc<[u8]>>,
        },
    }

    fn pixel_commands(commands: &[DrawCommand], uploads: &[ImageUpload]) -> Vec<PixelCommand> {
        let bytes: BTreeMap<TextureId, Arc<[u8]>> = uploads
            .iter()
            .map(|upload| (upload.texture_id, Arc::clone(&upload.rgba)))
            .collect();
        commands
            .iter()
            .map(|command| match command {
                DrawCommand::Rect(rect) => PixelCommand::Rect {
                    rect: rect.rect,
                    color: rect.color,
                },
                DrawCommand::Image(image) => PixelCommand::Image {
                    rect: image.rect,
                    source_rect: image.source_rect,
                    texture_size: image.texture_size,
                    opacity: image.opacity,
                    opaque: image.opaque,
                    rgba: bytes.get(&image.texture_id).cloned(),
                },
                DrawCommand::Text(text) => {
                    panic!("a prepared frame must not carry text, got {text:?}")
                }
            })
            .collect()
    }

    #[test]
    fn a_text_whose_texture_the_renderer_dropped_is_rasterized_again() {
        let fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let command = DrawCommand::Text(probe_text(
            "reappearing line",
            TextStyle::default(),
            FontSpec::default(),
        ));

        let (first_commands, first_uploads) =
            prepare_fresh_frame(std::slice::from_ref(&command), &fonts, &mut cache);
        // The frame that dropped the texture did not draw this text, so the
        // renderer dropped the entry with it: the text is rasterized again
        // under a fresh id rather than drawn as a texture that is gone (which
        // would draw nothing at all).  Nothing is held from that frame.
        let (again_commands, again_uploads) = prepare_frame_holding(
            std::slice::from_ref(&command),
            &fonts,
            &mut cache,
            &BTreeSet::new(),
        );
        assert_eq!(again_uploads.len(), 1, "a dropped texture rasterizes again");
        assert_ne!(
            again_uploads[0].texture_id, first_uploads[0].texture_id,
            "under a texture id of its own"
        );
        assert_eq!(
            again_uploads[0].rgba.as_ref(),
            first_uploads[0].rgba.as_ref(),
            "with the bytes a fresh rasterization of the same text produces"
        );
        assert_eq!(
            pixel_commands(&again_commands, &again_uploads),
            pixel_commands(&first_commands, &first_uploads),
            "and the same drawn pixels"
        );
    }

    #[test]
    fn a_message_box_frame_is_prepared_exactly_as_inline_rasterization_would() {
        let fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let commands = message_box_frame();

        let mut inline_next_id = TextImageCache::FIRST_TEXTURE_ID;
        let (inline_commands, inline_uploads) =
            prepare_commands_inline_reference(&commands, &fonts, &mut inline_next_id);

        let (first_commands, first_uploads) = prepare_fresh_frame(&commands, &fonts, &mut cache);
        assert_eq!(
            pixel_commands(&inline_commands, &inline_uploads),
            pixel_commands(&first_commands, &first_uploads),
            "the first frame draws what inline rasterization draws"
        );

        // The second frame is the cache's own: nothing to upload, and exactly
        // the same commands (same textures, same geometry, same alpha).
        let (second_commands, second_uploads) = prepare_frame_holding(
            &commands,
            &fonts,
            &mut cache,
            &uploaded_textures(&first_uploads),
        );
        assert!(
            second_uploads.is_empty(),
            "an unchanged frame must not upload again"
        );
        assert_eq!(
            first_commands, second_commands,
            "the cached frame prepares the same commands as the frame that uploaded"
        );
    }

    /// The within-frame live-set is the part a constant predicate cannot
    /// exercise: two commands with the same text in one draw list, prepared
    /// with the real `(*textures, *queued)` pair (`textures` empty — nothing is
    /// uploaded until the frame is drawn), must rasterize once and upload once.
    #[test]
    fn a_text_that_appears_twice_in_one_draw_list_uploads_once() {
        let fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let text = probe_text("twin", TextStyle::default(), FontSpec::default());
        let commands = [
            DrawCommand::Text(text.clone()),
            DrawCommand::Text(text.clone()),
        ];

        let mut queued = BTreeSet::new();
        let (prepared, uploads) =
            prepare_text_commands(&commands, &fonts, &mut cache, &BTreeSet::new(), &mut queued);
        assert_eq!(
            uploads.len(),
            1,
            "the second command must reuse the id the first minted this frame"
        );
        assert!(queued.contains(&uploads[0].texture_id));
        let [DrawCommand::Image(first), DrawCommand::Image(second)] = &prepared[..] else {
            panic!("expected two image commands, got {prepared:?}");
        };
        assert_eq!(first, second);
    }

    /// The live-set spans the frame's draw lists: a text a transition face's
    /// list rasterized first is a hit when the live list draws it again, with
    /// no upload for the second — the ids minted earlier in the frame count as
    /// live even though `upload_frame_images` has not run yet.
    #[test]
    fn a_text_shared_by_two_draw_lists_of_one_frame_uploads_once() {
        let fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let text = probe_text("shared line", TextStyle::default(), FontSpec::default());
        let live_list = [DrawCommand::Text(text.clone())];
        let face_list = [DrawCommand::Text(text)];

        let mut queued = BTreeSet::new();
        let (_, live_uploads) = prepare_text_commands(
            &live_list,
            &fonts,
            &mut cache,
            &BTreeSet::new(),
            &mut queued,
        );
        let (_, face_uploads) = prepare_text_commands(
            &face_list,
            &fonts,
            &mut cache,
            &BTreeSet::new(),
            &mut queued,
        );
        assert_eq!(live_uploads.len(), 1);
        assert!(
            face_uploads.is_empty(),
            "the second list must reuse the texture the first minted"
        );
    }

    #[test]
    fn a_changed_font_style_or_string_is_a_new_texture() {
        let fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let base = probe_text("base", TextStyle::default(), FontSpec::default());
        let (_, first_uploads) =
            prepare_fresh_frame(&[DrawCommand::Text(base.clone())], &fonts, &mut cache);
        let held = uploaded_textures(&first_uploads);

        let variants = [
            probe_text("base!", TextStyle::default(), FontSpec::default()),
            probe_text(
                "base",
                TextStyle {
                    color: [255, 0, 0, 255],
                    ..TextStyle::default()
                },
                FontSpec::default(),
            ),
            probe_text(
                "base",
                TextStyle {
                    color: [255, 255, 255, 128],
                    ..TextStyle::default()
                },
                FontSpec::default(),
            ),
            probe_text(
                "base",
                TextStyle {
                    anti_alias: false,
                    ..TextStyle::default()
                },
                FontSpec::default(),
            ),
            probe_text(
                "base",
                TextStyle {
                    shadow: Some(ShadowStyle {
                        offset_x: 1,
                        offset_y: 2,
                        color: [0, 0, 0, 255],
                    }),
                    ..TextStyle::default()
                },
                FontSpec::default(),
            ),
            probe_text(
                "base",
                TextStyle::default(),
                FontSpec {
                    height: 32.0,
                    ..FontSpec::default()
                },
            ),
            probe_text(
                "base",
                TextStyle::default(),
                FontSpec {
                    bold: true,
                    ..FontSpec::default()
                },
            ),
            probe_text(
                "base",
                TextStyle::default(),
                FontSpec {
                    italic: true,
                    ..FontSpec::default()
                },
            ),
            probe_text(
                "base",
                TextStyle::default(),
                FontSpec {
                    underline: true,
                    ..FontSpec::default()
                },
            ),
            probe_text(
                "base",
                TextStyle::default(),
                FontSpec {
                    strikeout: true,
                    ..FontSpec::default()
                },
            ),
            probe_text(
                "base",
                TextStyle::default(),
                FontSpec {
                    angle: 90,
                    ..FontSpec::default()
                },
            ),
            probe_text(
                "base",
                TextStyle::default(),
                FontSpec {
                    face: "another face".to_string(),
                    ..FontSpec::default()
                },
            ),
            probe_text(
                "base",
                TextStyle::default(),
                FontSpec {
                    face: "font/probe.otf".to_string(),
                    face_is_file_name: true,
                    ..FontSpec::default()
                },
            ),
            probe_text(
                "base",
                TextStyle::default(),
                FontSpec {
                    rasterizer: "prerendered".to_string(),
                    ..FontSpec::default()
                },
            ),
        ];
        for variant in variants {
            let (_, uploads) =
                prepare_frame_holding(&[DrawCommand::Text(variant)], &fonts, &mut cache, &held);
            assert_eq!(uploads.len(), 1, "a changed key must rasterize");
            assert_ne!(uploads[0].texture_id, first_uploads[0].texture_id);
        }

        // A command's own position, opacity and size are not rasterization
        // inputs: they stay per command, on a shared image.
        let mut moved = base;
        moved.position = Point::new(200.0, 90.0);
        moved.color = Color::new(1.0, 1.0, 1.0, 0.25);
        moved.size = 99.0;
        let (prepared, uploads) =
            prepare_frame_holding(&[DrawCommand::Text(moved)], &fonts, &mut cache, &held);
        assert!(
            uploads.is_empty(),
            "position, opacity and size come from the command, not the cache"
        );
        let [DrawCommand::Image(image)] = &prepared[..] else {
            panic!("expected one image command, got {prepared:?}");
        };
        assert_eq!(image.texture_id, first_uploads[0].texture_id);
        assert_eq!(image.rect.x, 200.0);
        assert_eq!(image.opacity, 0.25);
    }

    #[test]
    fn a_font_system_change_is_a_new_texture() {
        let mut fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let text = probe_text("generation", TextStyle::default(), FontSpec::default());
        let (_, first_uploads) =
            prepare_fresh_frame(&[DrawCommand::Text(text.clone())], &fonts, &mut cache);
        let held = uploaded_textures(&first_uploads);

        // An alias, an embedded-font table, a loaded face or the project
        // language can change what the same spec resolves to; an entry filled
        // before the change must not answer after it.
        fonts.register_font_aliases(vec![("alias".to_string(), "target".to_string())]);
        let (_, uploads) =
            prepare_frame_holding(&[DrawCommand::Text(text)], &fonts, &mut cache, &held);
        assert_eq!(uploads.len(), 1, "a font system change is a miss");
        assert_ne!(uploads[0].texture_id, first_uploads[0].texture_id);
    }

    #[test]
    fn a_font_generation_bump_invalidates_every_entry() {
        let fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let text = probe_text("generation", TextStyle::default(), FontSpec::default());
        prepare_fresh_frame(&[DrawCommand::Text(text.clone())], &fonts, &mut cache);

        let stale = TextImageKey::new(fonts.generation().wrapping_add(1), &text);
        assert!(
            cache.get(&stale).is_none(),
            "a cache entry from before the font system changed must not answer"
        );
        let current = TextImageKey::new(fonts.generation(), &text);
        assert!(cache.get(&current).is_some());
    }

    #[test]
    fn cache_entries_die_with_their_textures() {
        let fonts = FontSystem::new();
        let mut cache = TextImageCache::new();
        let text = probe_text("bounded", TextStyle::default(), FontSpec::default());
        prepare_fresh_frame(&[DrawCommand::Text(text.clone())], &fonts, &mut cache);

        let key = TextImageKey::new(fonts.generation(), &text);
        assert!(cache.get(&key).is_some());
        cache.retain_live_textures(|_| true);
        assert!(cache.get(&key).is_some(), "a live texture keeps its entry");
        cache.retain_live_textures(|_| false);
        assert!(
            cache.get(&key).is_none(),
            "an entry whose texture the renderer dropped is dropped too"
        );
    }

    // ---------------------------------------------------------------------
    // The same frames, rendered: the offscreen device draws the cached
    // preparation and the inline one, and the two have to come out byte for
    // byte the same picture.
    // ---------------------------------------------------------------------

    const TEXT_FRAME_WIDTH: u32 = 640;
    const TEXT_FRAME_HEIGHT: u32 = 400;

    /// The engine-uploaded layer image [`message_box_frame`] carries (texture
    /// 7): the text preparation must leave it alone, so every stream is given
    /// the same bytes under the same id.
    fn engine_layer_upload() -> ImageUpload {
        let (width, height) = (96u32, 96u32);
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[
                    (x * 255 / width) as u8,
                    (y * 255 / height) as u8,
                    40,
                    if (x / 8 + y / 8) % 2 == 0 { 255 } else { 128 },
                ]);
            }
        }
        ImageUpload::new(7, width, height, Arc::from(rgba))
    }

    /// Draws the image commands of a prepared frame into an offscreen target
    /// with the renderer's own texture pipeline and vertex geometry (the
    /// identity transform: a command's rect is already in target pixels) and
    /// submits the pass, returning the target and its readback buffer so the
    /// caller can map the pixels ([`read_back_pixels`]).
    fn draw_image_frame_offscreen(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &TexturePipelineResources,
        textures: &BTreeMap<TextureId, CachedTexture>,
        clear: Color,
        commands: &[DrawCommand],
    ) -> (wgpu::Texture, wgpu::Buffer) {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Kirakira text frame target"),
            size: wgpu::Extent3d {
                width: TEXT_FRAME_WIDTH,
                height: TEXT_FRAME_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let row_bytes = TEXT_FRAME_WIDTH * 4;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Kirakira text frame readback"),
            size: u64::from(row_bytes) * u64::from(TEXT_FRAME_HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Kirakira text frame encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Kirakira text frame pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(clear.r),
                            g: f64::from(clear.g),
                            b: f64::from(clear.b),
                            a: f64::from(clear.a),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let transform = RenderTransform {
                x_scale: 1.0,
                y_scale: 1.0,
                x_offset: 0.0,
                y_offset: 0.0,
            };
            for command in commands {
                let DrawCommand::Image(image) = command else {
                    continue;
                };
                let Some(texture) = textures.get(&image.texture_id) else {
                    panic!(
                        "prepared frame names texture {} with no upload",
                        image.texture_id
                    );
                };
                let vertices =
                    image_vertices(transform, TEXT_FRAME_WIDTH, TEXT_FRAME_HEIGHT, image);
                let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Kirakira text frame vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                pass.set_pipeline(&pipeline.pipeline);
                pass.set_bind_group(0, &texture.bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.draw(0..6, 0..1);
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(TEXT_FRAME_HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: TEXT_FRAME_WIDTH,
                height: TEXT_FRAME_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        (target, readback)
    }

    /// Maps the readback buffer [`draw_image_frame_offscreen`] filled and
    /// returns its RGBA bytes.
    fn read_back_pixels(device: &wgpu::Device, readback: &wgpu::Buffer) -> Vec<u8> {
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        rx.recv().expect("map").expect("mapped");
        let pixels = slice.get_mapped_range().to_vec();
        readback.unmap();
        pixels
    }

    /// The pixels one prepared frame draws offscreen, read back for comparison.
    fn render_image_frame_offscreen(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &TexturePipelineResources,
        textures: &BTreeMap<TextureId, CachedTexture>,
        clear: Color,
        commands: &[DrawCommand],
    ) -> Vec<u8> {
        let (_target, readback) =
            draw_image_frame_offscreen(device, queue, pipeline, textures, clear, commands);
        read_back_pixels(device, &readback)
    }

    /// The frames the cache produces have to be the frames inline rasterization
    /// produces, on the device that draws them: this renders the message-box
    /// frame three times — the pre-change preparation, the cache's first frame
    /// and the cache's unchanged second frame — and compares the RGBA readback
    /// byte for byte.  Then it changes the font system and checks the next
    /// frame is the freshly rasterized picture again, not the stale one.  A
    /// host without a wgpu adapter reports the skip instead of failing.
    #[test]
    fn cached_text_frames_render_byte_identically_to_inline_rasterization() {
        let Some((device, queue)) = headless_device() else {
            eprintln!(
                "no wgpu adapter: the cached text frames' pixels were not compared on this host"
            );
            return;
        };
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pipeline = TexturePipelineResources::new(&device, format);
        let mut fonts = FontSystem::new();
        let commands = message_box_frame();
        let clear = Color::new(0.02, 0.02, 0.05, 1.0);

        // "before the cache": every text rasterized inline, every frame.
        let engine_layer = engine_layer_upload();
        let upload_frame_textures = |textures: &mut BTreeMap<TextureId, CachedTexture>,
                                     uploads: &[ImageUpload]| {
            upload_images(&device, &queue, &pipeline, format, false, textures, uploads);
            upload_images(
                &device,
                &queue,
                &pipeline,
                format,
                false,
                textures,
                std::slice::from_ref(&engine_layer),
            );
        };
        let mut inline_next_id = TextImageCache::FIRST_TEXTURE_ID;
        let (inline_commands, inline_uploads) =
            prepare_commands_inline_reference(&commands, &fonts, &mut inline_next_id);
        let mut inline_textures = BTreeMap::new();
        upload_frame_textures(&mut inline_textures, &inline_uploads);
        let inline_pixels = render_image_frame_offscreen(
            &device,
            &queue,
            &pipeline,
            &inline_textures,
            clear,
            &inline_commands,
        );
        assert!(
            inline_pixels.chunks_exact(4).any(|pixel| pixel[3] != 0),
            "the probe frame must actually draw something"
        );

        // "after": the cache's first (cold) frame.
        let mut cache = TextImageCache::new();
        let (first_commands, first_uploads) = prepare_fresh_frame(&commands, &fonts, &mut cache);
        // The frame repeats a line, and the preparation mints each id into the
        // live-set as it goes: the repeated line draws from the texture its
        // first occurrence minted, so this frame uploads one image fewer than
        // the inline reference does for the same commands — and draws the same
        // pixels (asserted below).
        assert!(
            first_uploads.len() < inline_uploads.len(),
            "the repeated line must share its first occurrence's texture"
        );
        let held = uploaded_textures(&first_uploads);
        let mut textures = BTreeMap::new();
        upload_frame_textures(&mut textures, &first_uploads);
        let first_pixels = render_image_frame_offscreen(
            &device,
            &queue,
            &pipeline,
            &textures,
            clear,
            &first_commands,
        );
        assert_eq!(
            inline_pixels, first_pixels,
            "the cache's first frame must render the inline frame's pixels"
        );

        // The unchanged second frame: the cache's hits, drawn again.
        let (second_commands, second_uploads) =
            prepare_frame_holding(&commands, &fonts, &mut cache, &held);
        assert!(
            second_uploads.is_empty(),
            "the second frame has nothing to upload"
        );
        let second_pixels = render_image_frame_offscreen(
            &device,
            &queue,
            &pipeline,
            &textures,
            clear,
            &second_commands,
        );
        assert_eq!(
            inline_pixels, second_pixels,
            "an unchanged frame from the cache must render the inline frame's pixels"
        );

        // A font system change: the next frame is a fresh rasterization, and it
        // is still the picture inline rasterization draws.
        fonts.register_font_aliases(vec![("alias".to_string(), "target".to_string())]);
        let (changed_commands, changed_uploads) =
            prepare_frame_holding(&commands, &fonts, &mut cache, &held);
        assert!(
            !changed_uploads.is_empty(),
            "a font system change must rasterize the frame again"
        );
        let mut changed_textures = BTreeMap::new();
        upload_frame_textures(&mut changed_textures, &changed_uploads);
        let changed_pixels = render_image_frame_offscreen(
            &device,
            &queue,
            &pipeline,
            &changed_textures,
            clear,
            &changed_commands,
        );
        let fresh_commands = {
            let mut fresh_next_id = TextImageCache::FIRST_TEXTURE_ID;
            let (fresh_commands, fresh_uploads) =
                prepare_commands_inline_reference(&commands, &fonts, &mut fresh_next_id);
            let mut fresh_textures = BTreeMap::new();
            upload_frame_textures(&mut fresh_textures, &fresh_uploads);
            let fresh_pixels = render_image_frame_offscreen(
                &device,
                &queue,
                &pipeline,
                &fresh_textures,
                clear,
                &fresh_commands,
            );
            assert_eq!(
                fresh_pixels, changed_pixels,
                "after a font change the cached frame is the freshly rasterized picture"
            );
            fresh_commands
        };

        // Evidence for the record: the exact pixels each preparation drew.
        if let Ok(directory) = std::env::var("KRKR_TEXT_FRAME_DUMP") {
            let directory = std::path::Path::new(&directory);
            for (name, pixels) in [
                ("inline", &inline_pixels),
                ("cached-first", &first_pixels),
                ("cached-second", &second_pixels),
                ("cached-after-font-change", &changed_pixels),
            ] {
                let path = directory.join(format!("text-frame-{name}.png"));
                write_capture_png(&path, TEXT_FRAME_WIDTH, TEXT_FRAME_HEIGHT, pixels)
                    .expect("write frame dump");
                eprintln!("wrote {}", path.display());
            }
            eprintln!(
                "inline frame commands: {}, cached frame commands: {}",
                inline_commands.len(),
                fresh_commands.len()
            );
        }
    }

    /// The cost probe behind M203's numbers: what preparing a text-heavy frame
    /// costs on this host with the cache and without it, how much of a frame
    /// that is, and how much of it is the upload.  Prints to stdout; run it
    /// with
    ///
    /// ```text
    /// cargo test -p krkr-render --release -- --ignored text_frame_cost --nocapture
    /// ```
    #[test]
    #[ignore = "measurement probe: needs a wgpu adapter and prints timings"]
    fn text_frame_cost_probe() {
        use std::time::Instant;

        let Some((device, queue)) = headless_device() else {
            eprintln!("no wgpu adapter: the text frame cost was not measured on this host");
            return;
        };
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pipeline = TexturePipelineResources::new(&device, format);
        let fonts = FontSystem::new();
        let commands = message_box_frame();
        let clear = Color::new(0.02, 0.02, 0.05, 1.0);
        let frame_count = 120u32;

        let texts: Vec<&TextCommand> = commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text(text) => Some(text),
                _ => None,
            })
            .collect();

        // One rasterization per frame, per line.
        let start = Instant::now();
        for _ in 0..frame_count {
            for text in &texts {
                std::hint::black_box(fonts.rasterize_text(&text.font, text.style, &text.text));
            }
        }
        let rasterize_frame = start.elapsed() / frame_count;

        // One upload per frame, per line: create, write_texture, bind group.
        let uploads: Vec<ImageUpload> = texts
            .iter()
            .map(|text| {
                let image = fonts.rasterize_text(&text.font, text.style, &text.text);
                ImageUpload::new(0, image.width, image.height, Arc::from(image.rgba))
            })
            .collect();
        let start = Instant::now();
        for _ in 0..frame_count {
            let mut textures = BTreeMap::new();
            upload_images(
                &device,
                &queue,
                &pipeline,
                format,
                false,
                &mut textures,
                &uploads,
            );
        }
        let upload_frame = start.elapsed() / frame_count;

        // The pre-change preparation, per frame: rasterize + upload, nothing
        // reused.
        let start = Instant::now();
        for _ in 0..frame_count {
            let mut next_texture_id = TextImageCache::FIRST_TEXTURE_ID;
            let (_, uploads) =
                prepare_commands_inline_reference(&commands, &fonts, &mut next_texture_id);
            let mut textures = BTreeMap::new();
            upload_images(
                &device,
                &queue,
                &pipeline,
                format,
                false,
                &mut textures,
                &uploads,
            );
        }
        let inline_prepare_frame = start.elapsed() / frame_count;

        // The cache, per frame: the first frame prepares and uploads, every
        // later frame is the renderer's steady state — every text a hit drawn
        // from the textures the first frame left behind, one `queued` set per
        // frame, no device resource touched.
        let mut cache = TextImageCache::new();
        let (first_commands, first_uploads) = prepare_fresh_frame(&commands, &fonts, &mut cache);
        let held = uploaded_textures(&first_uploads);
        let mut textures = BTreeMap::new();
        upload_images(
            &device,
            &queue,
            &pipeline,
            format,
            false,
            &mut textures,
            &first_uploads,
        );
        // The engine-uploaded layer image the frame carries, so the draw below
        // has every texture the prepared commands name.
        upload_images(
            &device,
            &queue,
            &pipeline,
            format,
            false,
            &mut textures,
            std::slice::from_ref(&engine_layer_upload()),
        );
        let start = Instant::now();
        for _ in 0..frame_count {
            let mut queued = BTreeSet::new();
            let (hits, uploads) =
                prepare_text_commands(&commands, &fonts, &mut cache, &held, &mut queued);
            assert!(uploads.is_empty(), "the cached frame must not upload");
            std::hint::black_box(hits);
        }
        let cached_prepare_frame = start.elapsed() / frame_count;

        // Drawing the prepared frame: the render pass plus its submit, the
        // part of a real frame's cost that is not the text preparation.
        let start = Instant::now();
        for _ in 0..frame_count {
            let (target, readback) = draw_image_frame_offscreen(
                &device,
                &queue,
                &pipeline,
                &textures,
                clear,
                &first_commands,
            );
            std::hint::black_box((target, readback));
        }
        let draw_frame = start.elapsed() / frame_count;

        let inline_total = inline_prepare_frame + draw_frame;
        let cached_total = cached_prepare_frame + draw_frame;
        println!(
            "M203 text frame cost probe ({} frames per case)",
            frame_count
        );
        println!("  texts in frame: {}", texts.len());
        println!("  rasterize_text, per frame:      {rasterize_frame:?}");
        println!("  upload_images, per frame:       {upload_frame:?}");
        println!(
            "  prepare+upload, inline (before): {inline_prepare_frame:?} ({:.1}% of {inline_total:?})",
            inline_prepare_frame.as_secs_f64() / inline_total.as_secs_f64() * 100.0
        );
        println!(
            "  prepare, cached (after):         {cached_prepare_frame:?} ({:.1}% of {cached_total:?})",
            cached_prepare_frame.as_secs_f64() / cached_total.as_secs_f64() * 100.0
        );
        println!("  draw+submit one frame:           {draw_frame:?}");
        println!("  frame total inline -> cached:    {inline_total:?} -> {cached_total:?}");
    }
}
