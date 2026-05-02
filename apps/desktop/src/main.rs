use std::{fmt, path::PathBuf, process::ExitCode, sync::Arc, time::Instant};

use krkr_audio::AudioSystem;
use krkr_core::{
    ButtonState, Engine, EngineConfig, EngineEvent, EngineKey, FrameInput, Panel, Point,
    PointerButton, Size, StatusLevel, UiAction,
};
use krkr_engine::{EngineInput as KrkrEngineInput, KrkrEngine};
use krkr_platform::{pick_folder, show_error};
use krkr_render::{RenderError, Renderer};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Fullscreen, Window, WindowAttributes, WindowId},
};

fn main() -> ExitCode {
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            let message = format!("failed to create event loop: {error}");
            log_error(&message);
            show_error("Kirakira startup failed", &message);
            return ExitCode::FAILURE;
        }
    };
    let initial_project_root = std::env::args_os().nth(1).map(PathBuf::from);
    let mut app = DesktopApp::new(initial_project_root);

    match event_loop.run_app(&mut app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = format!("krkr-desktop failed: {error}");
            log_error(&message);
            show_error("Kirakira failed", &message);
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopState {
    Launcher,
    Settings,
    Running,
    FatalError,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DesktopStatus {
    level: StatusLevel,
    message: String,
}

impl DesktopStatus {
    fn new(level: StatusLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
        }
    }
}

struct DesktopApp {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    engine: Engine,
    audio: AudioSystem,
    krkr_engine: Option<KrkrEngine>,
    runtime_viewport_size: Option<Size>,
    pending_runtime_events: Vec<EngineEvent>,
    state: DesktopState,
    project_root: Option<PathBuf>,
    initial_project_root: Option<PathBuf>,
    status: Option<DesktopStatus>,
    last_frame: Instant,
}

impl DesktopApp {
    fn new(initial_project_root: Option<PathBuf>) -> Self {
        Self {
            window: None,
            renderer: None,
            engine: Engine::new(EngineConfig::default()),
            audio: AudioSystem::new(),
            krkr_engine: None,
            runtime_viewport_size: None,
            pending_runtime_events: Vec::new(),
            state: DesktopState::Launcher,
            project_root: None,
            initial_project_root,
            status: None,
            last_frame: Instant::now(),
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        log_info("creating desktop window");
        let attributes = WindowAttributes::default()
            .with_title("Kirakira")
            .with_inner_size(LogicalSize::new(960.0, 600.0))
            .with_min_inner_size(LogicalSize::new(420.0, 320.0));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.report_startup_failure(event_loop, "window creation failed", error);
                return;
            }
        };

        log_info("initializing wgpu renderer");
        let renderer = match pollster::block_on(Renderer::new(window.clone())) {
            Ok(renderer) => renderer,
            Err(error) => {
                let message = format!("renderer initialization failed: {error}");
                window.set_title(&format!("Kirakira - {message}"));
                log_error(&message);
                show_error("Kirakira renderer failed", &message);
                self.state = DesktopState::FatalError;
                self.status = Some(DesktopStatus::new(StatusLevel::Error, message));
                self.window = Some(window);
                event_loop.exit();
                return;
            }
        };
        log_info(&format!(
            "renderer capabilities: {:?}",
            renderer.capabilities()
        ));

        if let Err(error) = self.audio.prepare() {
            let message = format!("audio backend unavailable: {error}");
            log_warn(&message);
            self.set_status(StatusLevel::Warning, message, Some(&window));
        }

        self.window = Some(window.clone());
        self.renderer = Some(renderer);
        self.update_window_title(&window);

        if let Some(root) = self.initial_project_root.take() {
            self.launch_project(root, &window);
        }
    }

    fn handle_redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = &mut self.renderer else {
            return;
        };
        let window = self.window.clone();

        let now = Instant::now();
        let delta_seconds = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        let window_logical_size = renderer.logical_size();
        let content_size = if self.state == DesktopState::Running {
            self.runtime_viewport_size.unwrap_or(window_logical_size)
        } else {
            window_logical_size
        };
        let frame_input = FrameInput::new(content_size, delta_seconds);
        let mut return_to_launcher_after_render = false;
        let frame = match self.state {
            DesktopState::Launcher => {
                self.engine.set_panel(Panel::Launcher);
                self.engine.tick(frame_input)
            }
            DesktopState::Settings => {
                self.engine.set_panel(Panel::Settings);
                self.engine.tick(frame_input)
            }
            DesktopState::Running => {
                if let Some(krkr_engine) = &mut self.krkr_engine {
                    let events = std::mem::take(&mut self.pending_runtime_events);
                    match krkr_engine.update(
                        KrkrEngineInput::new(frame_input, events),
                        std::time::Duration::from_secs_f32(delta_seconds.max(0.0)),
                    ) {
                        Ok(frame) => {
                            return_to_launcher_after_render =
                                krkr_engine.host().termination_requested();
                            let audio_commands = krkr_engine.host_mut().take_audio_commands();
                            if let Err(error) = self.audio.apply_commands(audio_commands) {
                                let message = format!("audio command failed: {error}");
                                log_warn(&message);
                                self.status =
                                    Some(DesktopStatus::new(StatusLevel::Warning, message));
                                self.engine.set_status_level(Some(StatusLevel::Warning));
                            }
                            if let Some(window) = window.as_deref() {
                                apply_window_fullscreen(window, krkr_engine.window_fullscreen());
                            }
                            frame.output
                        }
                        Err(error) => {
                            let message = format!("engine update failed: {error}");
                            log_error(&message);
                            self.status = Some(DesktopStatus::new(StatusLevel::Error, message));
                            self.engine.set_status_level(Some(StatusLevel::Error));
                            self.engine.tick_running(frame_input)
                        }
                    }
                } else {
                    self.engine.tick_running(frame_input)
                }
            }
            DesktopState::FatalError => {
                self.engine.set_status_level(Some(StatusLevel::Error));
                self.engine.tick(frame_input)
            }
        };

        let render_content_size = (self.state == DesktopState::Running).then_some(content_size);
        renderer.set_content_size(render_content_size);
        if let Err(error) = renderer.render(&frame) {
            match error {
                RenderError::OutOfMemory => {
                    let message = "GPU surface is out of memory; exiting".to_string();
                    log_error(&message);
                    show_error("Kirakira renderer failed", &message);
                    event_loop.exit();
                }
            }
        }

        if return_to_launcher_after_render && let Some(window) = self.window.as_ref().cloned() {
            self.return_to_launcher(&window);
        }
    }

    fn resize_renderer(&mut self, size: PhysicalSize<u32>) {
        if let (Some(window), Some(renderer)) = (&self.window, &mut self.renderer) {
            renderer.resize(size, window.scale_factor());
            log_info(&format!("resized viewport: {:?}", renderer.viewport()));
        }
    }

    fn handle_core_event(&mut self, event: EngineEvent, window: &Window) {
        self.engine.handle_event(event);
        if let Some(action) = self.engine.take_last_action() {
            self.apply_action(action, window);
        }
    }

    fn apply_action(&mut self, action: UiAction, window: &Window) {
        match action {
            UiAction::LaunchRequested => self.launch_selected_project(window),
            UiAction::OpenProjectRequested => self.pick_and_launch_project(window),
            UiAction::SettingsOpened => {
                self.state = DesktopState::Settings;
                self.engine.set_panel(Panel::Settings);
                self.update_window_title(window);
                log_info("entered settings");
            }
            UiAction::SettingsClosed => {
                self.state = DesktopState::Launcher;
                self.engine.set_panel(Panel::Launcher);
                self.update_window_title(window);
                log_info("returned to launcher");
            }
        }
    }

    fn launch_selected_project(&mut self, window: &Window) {
        let Some(root) = self.project_root.clone() else {
            let message = "project resource root missing; use Open Project first";
            self.set_status(StatusLevel::Error, message, Some(window));
            show_error("Project resource root missing", message);
            return;
        };

        self.launch_project(root, window);
    }

    fn pick_and_launch_project(&mut self, window: &Window) {
        match pick_folder() {
            Some(root) => self.launch_project(root, window),
            None => {
                let message = "project selection canceled";
                self.set_status(StatusLevel::Warning, message, Some(window));
                log_warn(message);
            }
        }
    }

    fn launch_project(&mut self, root: PathBuf, window: &Window) {
        if !root.is_dir() {
            let message = format!(
                "selected project path is not a directory: {}",
                root.display()
            );
            self.set_status(StatusLevel::Error, message.clone(), Some(window));
            show_error("Invalid project directory", &message);
            return;
        }

        let mut krkr_engine = match KrkrEngine::for_project(&root) {
            Ok(engine) => engine,
            Err(error) => {
                let message = format!("engine initialization failed: {error}");
                self.set_status(StatusLevel::Error, message.clone(), Some(window));
                show_error("Engine initialization failed", &message);
                return;
            }
        };

        let has_startup_tjs = krkr_engine.host().storage_exists("startup.tjs");
        let has_startup_ks = krkr_engine.host().storage_exists("startup.ks");
        if has_startup_tjs || !has_startup_ks {
            match krkr_engine.execute_startup() {
                Ok(value) => log_info(&format!(
                    "startup.tjs completed for {} with result {}",
                    root.display(),
                    value
                )),
                Err(error) => {
                    let message = format!("startup.tjs failed: {error}");
                    self.set_status(StatusLevel::Error, message.clone(), Some(window));
                    show_error("Project startup failed", &message);
                    return;
                }
            }
        }

        if !krkr_engine.has_kag_scenario() && has_startup_ks {
            if let Err(error) = krkr_engine.load_kag_scenario("startup.ks") {
                let message = format!("startup.ks failed: {error}");
                self.set_status(StatusLevel::Error, message.clone(), Some(window));
                show_error("Project KAG startup failed", &message);
                return;
            }
            log_info(&format!("loaded startup.ks for {}", root.display()));
        }

        let preferred_size = krkr_engine.preferred_viewport_size();
        if let Some(size) = preferred_size {
            self.resize_window_for_runtime(window, size);
        }
        apply_window_fullscreen(window, krkr_engine.window_fullscreen());

        self.krkr_engine = Some(krkr_engine);
        self.runtime_viewport_size = preferred_size
            .filter(|size| !size.is_empty())
            .or_else(|| self.renderer.as_ref().map(Renderer::logical_size));
        self.project_root = Some(root.clone());
        self.state = DesktopState::Running;
        self.clear_status(Some(window));
        log_info(&format!("entered engine: {}", root.display()));
    }

    fn return_to_launcher(&mut self, window: &Window) {
        self.state = DesktopState::Launcher;
        self.krkr_engine = None;
        self.runtime_viewport_size = None;
        self.pending_runtime_events.clear();
        if let Some(renderer) = &mut self.renderer {
            renderer.set_content_size(None);
        }
        apply_window_fullscreen(window, false);
        self.engine.set_panel(Panel::Launcher);
        self.set_status(
            StatusLevel::Info,
            "returned to launcher from engine shell",
            Some(window),
        );
        log_info("returned to launcher from engine shell");
    }

    fn resize_window_for_runtime(&mut self, window: &Window, size: Size) {
        if size.is_empty() {
            return;
        }

        let width = size.width.clamp(320.0, 4096.0) as f64;
        let height = size.height.clamp(240.0, 4096.0) as f64;
        let _ = window.request_inner_size(LogicalSize::new(width, height));
        self.resize_renderer(window.inner_size());
        log_info(&format!(
            "requested runtime viewport: {}x{}",
            width as u32, height as u32
        ));
    }

    fn runtime_pointer_position(&self, position: Point) -> Point {
        let Some(content_size) = self.runtime_viewport_size else {
            return position;
        };
        let Some(renderer) = &self.renderer else {
            return position;
        };
        map_window_point_to_content(position, renderer.logical_size(), content_size)
    }

    fn set_status(
        &mut self,
        level: StatusLevel,
        message: impl Into<String>,
        window: Option<&Window>,
    ) {
        let status = DesktopStatus::new(level, message);
        match level {
            StatusLevel::Info => log_info(&status.message),
            StatusLevel::Warning => log_warn(&status.message),
            StatusLevel::Error => log_error(&status.message),
        }
        self.engine.set_status_level(Some(level));
        self.status = Some(status);
        if let Some(window) = window {
            self.update_window_title(window);
        }
    }

    fn clear_status(&mut self, window: Option<&Window>) {
        self.status = None;
        self.engine.set_status_level(None);
        if let Some(window) = window {
            self.update_window_title(window);
        }
    }

    fn update_window_title(&self, window: &Window) {
        window.set_title(&self.window_title());
    }

    fn window_title(&self) -> String {
        let state = match self.state {
            DesktopState::Launcher => "Launcher".to_string(),
            DesktopState::Settings => "Settings".to_string(),
            DesktopState::Running => match &self.project_root {
                Some(root) => format!("Running - {}", root.display()),
                None => "Running".to_string(),
            },
            DesktopState::FatalError => "Fatal Error".to_string(),
        };

        match &self.status {
            Some(status) => format!(
                "Kirakira - {state} - {}: {}",
                status_level_label(status.level),
                truncate_for_title(&status.message)
            ),
            None => format!("Kirakira - {state}"),
        }
    }

    fn report_startup_failure(
        &mut self,
        event_loop: &ActiveEventLoop,
        summary: &str,
        error: impl fmt::Display,
    ) {
        let message = format!("{summary}: {error}");
        log_error(&message);
        show_error("Kirakira startup failed", &message);
        self.state = DesktopState::FatalError;
        self.status = Some(DesktopStatus::new(StatusLevel::Error, message));
        event_loop.exit();
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
        let Some(window) = self.window.clone() else {
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
                if matches!(self.state, DesktopState::Launcher | DesktopState::Settings) {
                    let position = position.to_logical::<f64>(window.scale_factor());
                    self.handle_core_event(
                        EngineEvent::CursorMoved {
                            position: Point::new(position.x as f32, position.y as f32),
                        },
                        &window,
                    );
                    window.request_redraw();
                } else if self.state == DesktopState::Running {
                    let position = position.to_logical::<f64>(window.scale_factor());
                    let position = self
                        .runtime_pointer_position(Point::new(position.x as f32, position.y as f32));
                    self.pending_runtime_events
                        .push(EngineEvent::CursorMoved { position });
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if matches!(self.state, DesktopState::Launcher | DesktopState::Settings) {
                    self.handle_core_event(
                        EngineEvent::PointerInput {
                            button: map_mouse_button(button),
                            state: map_button_state(state),
                        },
                        &window,
                    );
                    window.request_redraw();
                } else if self.state == DesktopState::Running {
                    self.pending_runtime_events.push(EngineEvent::PointerInput {
                        button: map_mouse_button(button),
                        state: map_button_state(state),
                    });
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if self.state == DesktopState::Running
                    && event.state == ElementState::Pressed
                    && matches!(map_key(&event.logical_key), Some(EngineKey::Escape))
                {
                    self.return_to_launcher(&window);
                    window.request_redraw();
                    return;
                }

                if matches!(self.state, DesktopState::Launcher | DesktopState::Settings)
                    && let Some(key) = map_key(&event.logical_key)
                {
                    self.handle_core_event(
                        EngineEvent::KeyboardInput {
                            key,
                            state: map_button_state(event.state),
                        },
                        &window,
                    );
                    window.request_redraw();
                } else if self.state == DesktopState::Running
                    && let Some(key) = map_key(&event.logical_key)
                {
                    self.pending_runtime_events
                        .push(EngineEvent::KeyboardInput {
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

fn map_window_point_to_content(position: Point, window_size: Size, content_size: Size) -> Point {
    if window_size.is_empty() || content_size.is_empty() {
        return position;
    }

    let scale =
        (window_size.width / content_size.width).min(window_size.height / content_size.height);
    let rendered_width = content_size.width * scale;
    let rendered_height = content_size.height * scale;
    let x_offset = (window_size.width - rendered_width) * 0.5;
    let y_offset = (window_size.height - rendered_height) * 0.5;

    Point::new(
        (position.x - x_offset) / scale,
        (position.y - y_offset) / scale,
    )
}

fn apply_window_fullscreen(window: &Window, enabled: bool) {
    let active = window.fullscreen().is_some();
    if active == enabled {
        return;
    }

    let fullscreen = if enabled {
        Some(Fullscreen::Borderless(window.current_monitor()))
    } else {
        None
    };
    window.set_fullscreen(fullscreen);
    window.request_redraw();
    if enabled {
        log_info("entered fullscreen");
    } else {
        log_info("left fullscreen");
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
        Key::Named(NamedKey::Tab) => Some(EngineKey::Tab),
        Key::Named(NamedKey::ArrowLeft) => Some(EngineKey::Left),
        Key::Named(NamedKey::ArrowUp) => Some(EngineKey::Up),
        Key::Named(NamedKey::ArrowRight) => Some(EngineKey::Right),
        Key::Named(NamedKey::ArrowDown) => Some(EngineKey::Down),
        Key::Named(NamedKey::PageUp) => Some(EngineKey::PageUp),
        Key::Named(NamedKey::PageDown) => Some(EngineKey::PageDown),
        Key::Named(NamedKey::Backspace) => Some(EngineKey::Backspace),
        Key::Named(NamedKey::Delete) => Some(EngineKey::Delete),
        Key::Named(NamedKey::Shift) => Some(EngineKey::Shift),
        Key::Named(NamedKey::Control) => Some(EngineKey::Control),
        Key::Named(NamedKey::Alt) => Some(EngineKey::Alt),
        Key::Character(text) => text
            .chars()
            .next()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .map(|ch| EngineKey::Character(ch.to_ascii_uppercase())),
        _ => None,
    }
}

fn truncate_for_title(message: &str) -> String {
    const MAX_CHARS: usize = 96;
    if message.chars().count() <= MAX_CHARS {
        return message.to_string();
    }

    let mut truncated: String = message.chars().take(MAX_CHARS - 3).collect();
    truncated.push_str("...");
    truncated
}

fn status_level_label(level: StatusLevel) -> &'static str {
    match level {
        StatusLevel::Info => "info",
        StatusLevel::Warning => "warning",
        StatusLevel::Error => "error",
    }
}

fn log_info(message: &str) {
    eprintln!("[krkr-desktop][info] {message}");
}

fn log_warn(message: &str) {
    eprintln!("[krkr-desktop][warn] {message}");
}

fn log_error(message: &str) {
    eprintln!("[krkr-desktop][error] {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_fullscreen_window_points_to_runtime_content_space() {
        let point = map_window_point_to_content(
            Point::new(960.0, 540.0),
            Size::new(1920.0, 1080.0),
            Size::new(800.0, 600.0),
        );

        assert_eq!(point, Point::new(400.0, 300.0));
    }

    #[test]
    fn maps_letterboxed_window_points_to_runtime_content_space() {
        let point = map_window_point_to_content(
            Point::new(120.0, 180.0),
            Size::new(1000.0, 600.0),
            Size::new(800.0, 600.0),
        );

        assert_eq!(point, Point::new(20.0, 180.0));
    }

    #[test]
    fn keeps_pointer_position_when_sizes_are_empty() {
        let point = Point::new(12.0, 34.0);

        assert_eq!(
            map_window_point_to_content(point, Size::new(0.0, 1080.0), Size::new(800.0, 600.0)),
            point
        );
        assert_eq!(
            map_window_point_to_content(point, Size::new(1920.0, 1080.0), Size::new(800.0, 0.0)),
            point
        );
    }
}
