use std::{error::Error, fmt, sync::Arc};

use bytemuck::{Pod, Zeroable};
use krkr_core::{Color, DrawCommand, FrameOutput, Rect, Size};
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
    physical_size: PhysicalSize<u32>,
    scale_factor: f64,
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
                label: Some("krkr-ruri device"),
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
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: physical_size.width.max(1),
            height: physical_size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let pipeline = create_pipeline(&device, format);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            physical_size,
            scale_factor,
        })
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

    pub fn render(&mut self, frame: &FrameOutput) -> Result<(), RenderError> {
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

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let vertices = self.vertices_for_frame(frame);
        let vertex_buffer = (!vertices.is_empty()).then(|| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("krkr-ruri rect vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                })
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("krkr-ruri render encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("krkr-ruri render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu_color(frame.clear_color)),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if let Some(vertex_buffer) = &vertex_buffer {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.draw(0..vertices.len() as u32, 0..1);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        surface_texture.present();
        Ok(())
    }

    fn vertices_for_frame(&self, frame: &FrameOutput) -> Vec<Vertex> {
        let rect_count = frame
            .draw_commands
            .iter()
            .filter(|command| matches!(command, DrawCommand::Rect(_)))
            .count();
        let mut vertices = Vec::with_capacity(rect_count * 6);

        for command in &frame.draw_commands {
            match command {
                DrawCommand::Rect(rect) => {
                    vertices.extend_from_slice(&self.rect_vertices(rect.rect, rect.color));
                }
            }
        }

        vertices
    }

    fn rect_vertices(&self, rect: Rect, color: Color) -> [Vertex; 6] {
        let scale = self.scale_factor as f32;
        let x0 = rect.x * scale;
        let y0 = rect.y * scale;
        let x1 = (rect.x + rect.width) * scale;
        let y1 = (rect.y + rect.height) * scale;
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
}

#[cfg(target_os = "macos")]
fn preferred_backends() -> wgpu::Backends {
    wgpu::Backends::METAL
}

#[cfg(not(target_os = "macos"))]
fn preferred_backends() -> wgpu::Backends {
    wgpu::Backends::PRIMARY
}

fn create_pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::include_wgsl!("rect.wgsl"));
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("krkr-ruri rect pipeline layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("krkr-ruri rect pipeline"),
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
