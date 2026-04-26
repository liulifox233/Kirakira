use std::{process::ExitCode, sync::Arc, time::Instant};

use krkr_audio::AudioSystem;
use krkr_core::{
    ButtonState, Engine, EngineConfig, EngineEvent, EngineKey, FrameInput, Point, PointerButton,
};
use krkr_render::{RenderError, Renderer};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowAttributes, WindowId},
};

fn main() -> ExitCode {
    let event_loop = EventLoop::new().expect("failed to create event loop");
    let mut app = DesktopApp::new();

    match event_loop.run_app(&mut app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("krkr-desktop failed: {error}");
            ExitCode::FAILURE
        }
    }
}

struct DesktopApp {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    engine: Engine,
    audio: AudioSystem,
    last_frame: Instant,
}

impl DesktopApp {
    fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            engine: Engine::new(EngineConfig::default()),
            audio: AudioSystem::new(),
            last_frame: Instant::now(),
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = WindowAttributes::default()
            .with_title("krkr-ruri")
            .with_inner_size(LogicalSize::new(960.0, 600.0))
            .with_min_inner_size(LogicalSize::new(420.0, 320.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("failed to create window"),
        );
        let renderer = pollster::block_on(Renderer::new(window.clone()))
            .expect("failed to initialize renderer");

        let _ = self.audio.prepare();
        self.window = Some(window);
        self.renderer = Some(renderer);
    }

    fn handle_redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = &mut self.renderer else {
            return;
        };

        let now = Instant::now();
        let delta_seconds = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        let frame = self
            .engine
            .tick(FrameInput::new(renderer.logical_size(), delta_seconds));
        if let Err(error) = renderer.render(&frame) {
            match error {
                RenderError::OutOfMemory => event_loop.exit(),
            }
        }
    }

    fn resize_renderer(&mut self, size: PhysicalSize<u32>) {
        if let (Some(window), Some(renderer)) = (&self.window, &mut self.renderer) {
            renderer.resize(size, window.scale_factor());
        }
    }
}

impl ApplicationHandler for DesktopApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.create_window(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = &self.window else {
            return;
        };
        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.resize_renderer(size),
            WindowEvent::ScaleFactorChanged { .. } => {
                self.resize_renderer(window.inner_size());
            }
            WindowEvent::CursorMoved { position, .. } => {
                let position = position.to_logical::<f64>(window.scale_factor());
                self.engine.handle_event(EngineEvent::CursorMoved {
                    position: Point::new(position.x as f32, position.y as f32),
                });
                window.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.engine.handle_event(EngineEvent::PointerInput {
                    button: map_mouse_button(button),
                    state: map_button_state(state),
                });
                window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(key) = map_key(&event.logical_key) {
                    self.engine.handle_event(EngineEvent::KeyboardInput {
                        key,
                        state: map_button_state(event.state),
                    });
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.handle_redraw(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

fn map_button_state(state: ElementState) -> ButtonState {
    match state {
        ElementState::Pressed => ButtonState::Pressed,
        ElementState::Released => ButtonState::Released,
    }
}

fn map_mouse_button(button: MouseButton) -> PointerButton {
    match button {
        MouseButton::Left => PointerButton::Primary,
        MouseButton::Right => PointerButton::Secondary,
        MouseButton::Middle => PointerButton::Middle,
        MouseButton::Back => PointerButton::Other(3),
        MouseButton::Forward => PointerButton::Other(4),
        MouseButton::Other(value) => PointerButton::Other(value),
    }
}

fn map_key(key: &Key) -> Option<EngineKey> {
    match key {
        Key::Named(NamedKey::Escape) => Some(EngineKey::Escape),
        Key::Named(NamedKey::Enter) => Some(EngineKey::Enter),
        Key::Named(NamedKey::Space) => Some(EngineKey::Space),
        _ => None,
    }
}
