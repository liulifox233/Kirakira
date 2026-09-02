use std::{fmt, path::PathBuf, process::ExitCode, sync::Arc, time::Instant};

use krkr_assets::NativeAssetStore;
use krkr_audio::AudioSystem;
use krkr_core::{
    AudioEvent, AudioStatusLevel, ButtonState, Clock, Engine, EngineConfig, EngineEvent, EngineKey,
    FrameInput, Point, PointerButton, Size, StatusLevel,
};
use krkr_engine::{
    EngineConfig as KrkrEngineConfig, EngineInput as KrkrEngineInput, KrkrEngine, RuntimeSession,
    SystemMetrics,
};
use krkr_plugins::register_reference_plugins;
use krkr_render::{RenderError, Renderer};
use rfd::{MessageButtons, MessageDialog, MessageLevel};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent},
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
    let initial_project_root = initial_project_root();
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

fn show_error(title: &str, message: &str) {
    let _ = MessageDialog::new()
        .set_level(MessageLevel::Error)
        .set_title(title)
        .set_description(message)
        .set_buttons(MessageButtons::Ok)
        .show();
}

fn initial_project_root() -> Option<PathBuf> {
    if let Some(arg) = std::env::args_os().nth(1) {
        return Some(PathBuf::from(arg));
    }

    let current_dir = std::env::current_dir().ok();
    if current_dir
        .as_ref()
        .is_some_and(|path| looks_like_project_root(path))
    {
        return current_dir;
    }

    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
        .or(current_dir)
}

fn looks_like_project_root(path: &std::path::Path) -> bool {
    path.join("startup.tjs").is_file()
        || path.join("startup.ks").is_file()
        || directory_has_xp3(path)
        || directory_has_xp3(&path.join("sys"))
}

fn directory_has_xp3(path: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        entry.path().is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("xp3"))
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopState {
    Running,
    FatalError,
}

#[derive(Clone, Copy, Debug)]
struct DesktopClock {
    started: Instant,
}

impl DesktopClock {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Clock for DesktopClock {
    fn now_millis(&mut self) -> i64 {
        self.started.elapsed().as_millis().min(i64::MAX as u128) as i64
    }
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
    runtime: Option<RuntimeSession>,
    runtime_viewport_size: Option<Size>,
    pending_runtime_events: Vec<EngineEvent>,
    pending_runtime_text: Vec<krkr_core::TextInputEvent>,
    state: DesktopState,
    project_root: Option<PathBuf>,
    initial_project_root: Option<PathBuf>,
    status: Option<DesktopStatus>,
    last_frame: Instant,
    rendered_frames: u64,
    video_present_frame: Option<u64>,
    video_texture_id: Option<u64>,
}

impl DesktopApp {
    fn new(initial_project_root: Option<PathBuf>) -> Self {
        Self {
            window: None,
            renderer: None,
            engine: Engine::new(EngineConfig::default()),
            runtime: None,
            runtime_viewport_size: None,
            pending_runtime_events: Vec::new(),
            pending_runtime_text: Vec::new(),
            state: DesktopState::Running,
            project_root: None,
            initial_project_root,
            status: None,
            last_frame: Instant::now(),
            rendered_frames: 0,
            video_present_frame: None,
            video_texture_id: None,
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

        self.window = Some(window.clone());
        self.renderer = Some(renderer);
        self.update_window_title(&window);

        let root = self.initial_project_root.take().unwrap_or_else(|| {
            let fallback = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            log_warn(&format!(
                "failed to resolve startup project root; falling back to {}",
                fallback.display()
            ));
            fallback
        });
        if !self.launch_project(root, &window) {
            event_loop.exit();
        }
    }

    fn handle_redraw(&mut self, event_loop: &ActiveEventLoop) {
        self.report_audio_events();

        let Some(renderer) = &mut self.renderer else {
            return;
        };
        let window = self.window.clone();

        let now = Instant::now();
        let delta_seconds = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        let window_logical_size = renderer.logical_size();
        let content_size = self.runtime_viewport_size.unwrap_or(window_logical_size);
        let frame_input = FrameInput::new(content_size, delta_seconds);
        let mut exit_after_render = false;
        let frame = match self.state {
            DesktopState::Running => {
                if let Some(runtime) = &mut self.runtime {
                    let events = std::mem::take(&mut self.pending_runtime_events);
                    let text = std::mem::take(&mut self.pending_runtime_text);
                    match runtime.update(
                        KrkrEngineInput::new(frame_input, events).with_text(text),
                        std::time::Duration::from_secs_f32(delta_seconds.max(0.0)),
                    ) {
                        Ok(runtime_frame) => {
                            let frame = runtime_frame.engine;
                            if frame.input.unhandled_escape_pressed
                                && !runtime.engine().host().termination_requested()
                            {
                                if let Err(error) = runtime.engine_mut().request_runtime_close() {
                                    log_warn(&format!(
                                        "failed to request runtime close from Escape fallback: {error}"
                                    ));
                                    if let Err(error) = runtime.engine_mut().persist_runtime_state()
                                    {
                                        log_warn(&format!(
                                            "failed to persist runtime state before Escape fallback close: {error}"
                                        ));
                                    }
                                    exit_after_render = true;
                                }
                            }
                            exit_after_render |= runtime.engine().host().termination_requested();
                            for line in runtime.engine_mut().host_mut().drain_logs() {
                                log_info(&line);
                                if line.starts_with("VideoOverlay: presenting")
                                    && self.video_present_frame.is_none()
                                {
                                    self.video_present_frame = Some(self.rendered_frames);
                                    self.video_texture_id = parse_presenting_texture_id(&line);
                                }
                            }
                            if let Some(window) = window.as_deref() {
                                apply_window_fullscreen(
                                    window,
                                    runtime.engine().window_fullscreen(),
                                );
                            }
                            frame.output
                        }
                        Err(error) => {
                            let message = format!("engine update failed: {error}");
                            log_error(&message);
                            for line in runtime.engine_mut().host_mut().drain_logs() {
                                log_info(&line);
                            }
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
                self.engine.tick_running(frame_input)
            }
        };

        renderer.set_content_size(Some(content_size));
        if let Some(target) = capture_frame_target(self.rendered_frames) {
            let path = std::env::var("KRKR_CAPTURE_PATH")
                .unwrap_or_else(|_| "/tmp/krkr_capture.png".to_string());
            renderer.capture_next_frame(&path);
            log_info(&format!(
                "surface capture armed for frame {target} at {path}"
            ));
        }
        if let (Some(delay), Some(start)) = (capture_video_delay(), self.video_present_frame)
            && self.rendered_frames == start + delay
        {
            let path = std::env::var("KRKR_CAPTURE_PATH")
                .unwrap_or_else(|_| "/tmp/krkr_capture.png".to_string());
            renderer.capture_next_frame(&path);
            log_info(&format!("video surface capture armed at {path}"));
            if let Some(texture_id) = self.video_texture_id {
                let texture_path = std::env::var("KRKR_CAPTURE_TEXTURE_PATH")
                    .unwrap_or_else(|_| "/tmp/krkr_capture_tex.png".to_string());
                renderer.capture_texture_next_frame(texture_id, &texture_path);
                log_info(&format!(
                    "video texture capture armed for texture={texture_id} at {texture_path}"
                ));
            }
        }
        self.rendered_frames = self.rendered_frames.saturating_add(1);
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

        if exit_after_render {
            self.persist_running_project();
            event_loop.exit();
        }
    }

    fn resize_renderer(&mut self, size: PhysicalSize<u32>) {
        if self.state == DesktopState::Running {
            self.pending_runtime_events.push(EngineEvent::Lifecycle {
                state: if size.width == 0 || size.height == 0 {
                    krkr_core::LifecycleState::SurfaceSuspended
                } else {
                    krkr_core::LifecycleState::SurfaceResumed
                },
            });
        }
        if let (Some(window), Some(renderer)) = (&self.window, &mut self.renderer) {
            renderer.resize(size, window.scale_factor());
            log_info(&format!("resized viewport: {:?}", renderer.viewport()));
        }
    }

    fn launch_project(&mut self, root: PathBuf, window: &Window) -> bool {
        if !root.is_dir() {
            let message = format!("project path is not a directory: {}", root.display());
            self.set_status(StatusLevel::Error, message.clone(), Some(window));
            show_error("Invalid project directory", &message);
            self.state = DesktopState::FatalError;
            return false;
        }

        let mut krkr_engine = match KrkrEngine::new(KrkrEngineConfig {
            project_root: Some(root.clone()),
            system_metrics: system_metrics_for_window(window),
            ..KrkrEngineConfig::default()
        }) {
            Ok(engine) => engine,
            Err(error) => {
                let message = format!("engine initialization failed: {error}");
                self.set_status(StatusLevel::Error, message.clone(), Some(window));
                show_error("Engine initialization failed", &message);
                self.state = DesktopState::FatalError;
                return false;
            }
        };
        if let Err(error) = register_reference_plugins(&mut krkr_engine) {
            let message = format!("reference plugin registration failed: {error}");
            self.set_status(StatusLevel::Error, message.clone(), Some(window));
            show_error("Plugin initialization failed", &message);
            self.state = DesktopState::FatalError;
            return false;
        }
        let mut audio = AudioSystem::new();
        if let Err(error) = audio.prepare() {
            let message = format!("audio backend unavailable: {error}");
            log_warn(&message);
            self.set_status(StatusLevel::Warning, message, Some(window));
        }
        if let Err(error) = audio.set_resource_provider(krkr_engine.host().resource_provider()) {
            let message = format!("audio worker unavailable: {error}");
            self.set_status(StatusLevel::Warning, message, Some(window));
        }

        let mut runtime = RuntimeSession::new(
            krkr_engine,
            Box::new(NativeAssetStore::new(root.clone())),
            Box::new(audio),
            Box::new(DesktopClock::new()),
        );
        if let Err(error) = runtime.start_project() {
            let message = format!("project startup failed: {error}");
            self.set_status(StatusLevel::Error, message.clone(), Some(window));
            show_error("Project startup failed", &message);
            self.state = DesktopState::FatalError;
            return false;
        }
        log_info(&format!(
            "project startup dispatched for {}",
            root.display()
        ));

        let preferred_size = runtime.engine().preferred_viewport_size();
        if let Some(size) = preferred_size {
            self.resize_window_for_runtime(window, size);
        }
        apply_window_fullscreen(window, runtime.engine().window_fullscreen());
        self.runtime = Some(runtime);
        self.runtime_viewport_size = preferred_size
            .filter(|size| !size.is_empty())
            .or_else(|| self.renderer.as_ref().map(Renderer::logical_size));
        self.project_root = Some(root.clone());
        self.state = DesktopState::Running;
        self.clear_status(Some(window));
        log_info(&format!("entered engine: {}", root.display()));
        true
    }

    fn persist_running_project(&mut self) {
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        if let Err(error) = runtime.engine_mut().persist_runtime_state() {
            log_warn(&format!(
                "failed to persist runtime state before shutdown: {error}"
            ));
        }
    }

    fn request_running_project_close(&mut self) -> bool {
        let Some(runtime) = &mut self.runtime else {
            return false;
        };
        if let Err(error) = runtime.engine_mut().request_runtime_close() {
            log_warn(&format!("failed to request runtime close: {error}"));
            self.persist_running_project();
            return false;
        }
        true
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

    fn report_audio_events(&mut self) {
        let events = self
            .runtime
            .as_mut()
            .map(|runtime| runtime.audio_mut().poll_events())
            .unwrap_or_default();
        for event in events {
            match event {
                AudioEvent::Status(event) => {
                    let level = match event.level {
                        AudioStatusLevel::Warning => StatusLevel::Warning,
                        AudioStatusLevel::Error => StatusLevel::Error,
                    };
                    let message = format!("audio: {}", event.message);
                    match level {
                        StatusLevel::Info => log_info(&message),
                        StatusLevel::Warning => log_warn(&message),
                        StatusLevel::Error => log_error(&message),
                    }
                    self.engine.set_status_level(Some(level));
                    self.status = Some(DesktopStatus::new(level, message));
                }
                AudioEvent::PlaybackStopped { id } => {
                    if let Some(runtime) = &mut self.runtime
                        && let Err(error) = runtime.engine_mut().notify_audio_stopped(id)
                    {
                        let message = format!("audio completion callback failed: {error}");
                        log_warn(&message);
                        self.engine.set_status_level(Some(StatusLevel::Warning));
                        self.status = Some(DesktopStatus::new(StatusLevel::Warning, message));
                    }
                }
            }
        }
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
            WindowEvent::CloseRequested => {
                if self.state == DesktopState::Running {
                    if self.request_running_project_close() {
                        window.request_redraw();
                    } else {
                        event_loop.exit();
                    }
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => self.resize_renderer(size),
            WindowEvent::ScaleFactorChanged { .. } => {
                self.resize_renderer(window.inner_size());
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.state == DesktopState::Running {
                    let position = position.to_logical::<f64>(window.scale_factor());
                    let position = self
                        .runtime_pointer_position(Point::new(position.x as f32, position.y as f32));
                    self.pending_runtime_events
                        .push(EngineEvent::CursorMoved { position });
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if self.state == DesktopState::Running {
                    self.pending_runtime_events.push(EngineEvent::PointerInput {
                        button: map_mouse_button(button),
                        state: map_button_state(state),
                    });
                    window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta = map_wheel_delta(delta);
                if delta == 0 {
                    return;
                }
                if self.state == DesktopState::Running {
                    self.pending_runtime_events
                        .push(EngineEvent::MouseWheel { delta });
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if self.state == DesktopState::Running
                    && let Some(key) = map_key(&event.logical_key)
                {
                    self.pending_runtime_events
                        .push(EngineEvent::KeyboardInput {
                            key,
                            state: map_button_state(event.state),
                            repeat: event.repeat,
                        });
                    window.request_redraw();
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                if self.state == DesktopState::Running {
                    self.pending_runtime_text.push(krkr_core::TextInputEvent {
                        text,
                        composing: false,
                    });
                    window.request_redraw();
                }
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                if self.state == DesktopState::Running {
                    self.pending_runtime_text.push(krkr_core::TextInputEvent {
                        text,
                        composing: true,
                    });
                    window.request_redraw();
                }
            }
            WindowEvent::Occluded(occluded) => {
                if self.state == DesktopState::Running {
                    self.pending_runtime_events.push(EngineEvent::Lifecycle {
                        state: if occluded {
                            krkr_core::LifecycleState::Background
                        } else {
                            krkr_core::LifecycleState::Foreground
                        },
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

fn system_metrics_for_window(window: &Window) -> SystemMetrics {
    if let Some(monitor) = window.current_monitor() {
        let size = monitor.size();
        let position = monitor.position();
        return SystemMetrics {
            screen_width: size.width as i64,
            screen_height: size.height as i64,
            desktop_left: position.x as i64,
            desktop_top: position.y as i64,
            desktop_width: size.width as i64,
            desktop_height: size.height as i64,
        };
    }

    let size = window.inner_size();
    SystemMetrics {
        screen_width: size.width as i64,
        screen_height: size.height as i64,
        desktop_left: 0,
        desktop_top: 0,
        desktop_width: size.width as i64,
        desktop_height: size.height as i64,
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

fn map_wheel_delta(delta: MouseScrollDelta) -> i32 {
    let y = match delta {
        MouseScrollDelta::LineDelta(_, y) => y * 120.0,
        MouseScrollDelta::PixelDelta(position) => position.y as f32,
    };
    if y.abs() < f32::EPSILON {
        0
    } else if y.abs() < 1.0 {
        y.signum() as i32
    } else {
        y.round() as i32
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

/// `KRKR_CAPTURE_FRAME=<n>` arms a one-shot surface capture just before frame
/// `n` is rendered (path from `KRKR_CAPTURE_PATH`, default
/// `/tmp/krkr_capture.png`).
fn capture_frame_target(rendered_frames: u64) -> Option<u64> {
    let target: u64 = std::env::var("KRKR_CAPTURE_FRAME").ok()?.parse().ok()?;
    (rendered_frames == target).then_some(target)
}

/// `KRKR_CAPTURE_VIDEO=<frames>` arms a one-shot surface capture this many
/// frames after the first video quad is presented (path from
/// `KRKR_CAPTURE_PATH`, default `/tmp/krkr_capture.png`).
fn capture_video_delay() -> Option<u64> {
    std::env::var("KRKR_CAPTURE_VIDEO").ok()?.parse().ok()
}

/// Extracts the `texture=N` id out of a `VideoOverlay: presenting (...)` log
/// line so the desktop app can capture that exact texture from the renderer.
fn parse_presenting_texture_id(line: &str) -> Option<u64> {
    let start = line.find("texture=")? + "texture=".len();
    let end = line[start..]
        .find(|c: char| !c.is_ascii_digit())
        .map(|offset| start + offset)?;
    line[start..end].parse().ok()
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
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

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

    #[test]
    fn detects_project_root_with_startup_script() {
        let root = make_temp_dir("startup-script");
        fs::write(root.join("startup.tjs"), b"").expect("write startup");

        assert!(looks_like_project_root(&root));

        fs::remove_dir_all(root).expect("remove temp project");
    }

    #[test]
    fn detects_project_root_with_xp3_archive() {
        let root = make_temp_dir("xp3-root");
        fs::create_dir(root.join("sys")).expect("create sys");
        fs::write(root.join("sys/data.xp3"), b"").expect("write xp3");

        assert!(looks_like_project_root(&root));

        fs::remove_dir_all(root).expect("remove temp project");
    }

    fn make_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "kirakira-desktop-{name}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temp dir");
        path
    }
}
