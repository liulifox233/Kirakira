use std::{
    collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher},
    env, fmt,
    hash::{Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};
use image::{ImageBuffer, ImageFormat, Rgba};
use krkr_assets::{NativeAssetStore, ProjectStorage};
use krkr_audio::{AudioEvent, AudioSystem};
use krkr_core::{
    ButtonState, Color, DrawCommand, EngineEvent, EngineKey, FrameInput, ImageCommand, ImageUpload,
    LayerId, MessageLayerModel, Point, PointerButton, Rect, Size, TextureId,
};
use krkr_engine::{
    EngineConfig, EngineFrame, EngineInput, KrkrEngine, KrkrHost, NativeTextDrawEvent,
    RuntimeSession, RuntimeSessionError, SystemPaths, TransitionPolicy,
};
use krkr_plugins::register_reference_plugins;
use terminal_size::{Height, Width, terminal_size};

fn main() -> ExitCode {
    let result = TerminalApp::from_args(env::args().skip(1))
        .map_err(AppError::Message)
        .and_then(|mut app| app.run());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("krkr-terminal failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageProtocolArg {
    Auto,
    Kitty,
    Iterm2,
    Sixel,
    Ansi,
}

impl ImageProtocolArg {
    fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "kitty" => Ok(Self::Kitty),
            "iterm2" | "iterm" => Ok(Self::Iterm2),
            "sixel" => Ok(Self::Sixel),
            "ansi" => Ok(Self::Ansi),
            _ => Err(format!("unknown image protocol `{value}`")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageProtocol {
    Kitty,
    Iterm2,
    Sixel,
    Ansi,
}

fn select_image_protocol(arg: ImageProtocolArg) -> ImageProtocol {
    match arg {
        ImageProtocolArg::Kitty => ImageProtocol::Kitty,
        ImageProtocolArg::Iterm2 => ImageProtocol::Iterm2,
        ImageProtocolArg::Sixel => ImageProtocol::Sixel,
        ImageProtocolArg::Ansi => ImageProtocol::Ansi,
        ImageProtocolArg::Auto => detect_image_protocol(),
    }
}

fn detect_image_protocol() -> ImageProtocol {
    let term = env::var("TERM").unwrap_or_default().to_ascii_lowercase();
    if env::var_os("KITTY_WINDOW_ID").is_some() || term.contains("xterm-kitty") {
        return ImageProtocol::Kitty;
    }
    if env::var("TERM_PROGRAM").is_ok_and(|value| value.eq_ignore_ascii_case("iTerm.app")) {
        return ImageProtocol::Iterm2;
    }
    if term.contains("sixel") || env::var_os("SIXEL_SUPPORT").is_some() {
        return ImageProtocol::Sixel;
    }
    ImageProtocol::Ansi
}

struct TerminalApp {
    project_root: PathBuf,
    protocol: ImageProtocol,
    debug_interaction: bool,
    runtime: RuntimeSession,
    presentation: TerminalRenderer,
    interaction: TerminalInteraction,
    debug_log: DebugInteractionLog,
    transcript: TranscriptTracker,
    native_transcript: NativeTranscriptTracker,
    pending_events: Vec<EngineEvent>,
    viewport_size: Size,
    quit_requested: bool,
}

impl TerminalApp {
    fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut project_root = None;
        let mut protocol_arg = ImageProtocolArg::Auto;
        let mut debug_interaction = false;
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            if arg == "--image-protocol" {
                let value = iter
                    .next()
                    .ok_or_else(|| "--image-protocol requires a value".to_string())?;
                protocol_arg = ImageProtocolArg::parse(&value)?;
            } else if let Some(value) = arg.strip_prefix("--image-protocol=") {
                protocol_arg = ImageProtocolArg::parse(value)?;
            } else if arg == "--debug-interaction" {
                debug_interaction = true;
            } else if arg.starts_with('-') {
                return Err(format!("unknown option `{arg}`"));
            } else if project_root.replace(PathBuf::from(&arg)).is_some() {
                return Err("only one PROJECT_ROOT may be provided".to_string());
            }
        }

        let project_root = project_root
            .or_else(initial_project_root)
            .ok_or_else(|| "failed to resolve project root".to_string())?;
        if !project_root.is_dir() {
            return Err(format!(
                "project path is not a directory: {}",
                project_root.display()
            ));
        }

        let storage = ProjectStorage::for_root(project_root.clone())
            .map_err(|error| format!("storage initialization failed: {error}"))?;
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            system_paths: system_paths_for_project(&project_root),
            video_factory: std::sync::Arc::new(krkr_video::PlatformVideoFactory),
            ..EngineConfig::default()
        })
        .map_err(|error| format!("engine initialization failed: {error}"))?;
        engine
            .host_mut()
            .set_transition_policy(TransitionPolicy::Immediate);
        register_reference_plugins(&mut engine)
            .map_err(|error| format!("reference plugin registration failed: {error}"))?;
        let viewport_size = engine
            .preferred_viewport_size()
            .filter(|size| !size.is_empty())
            .unwrap_or_else(|| Size::new(960.0, 540.0));
        let protocol = select_image_protocol(protocol_arg);
        let mut audio = AudioSystem::new();
        audio
            .prepare()
            .map_err(|error| format!("audio backend unavailable: {error}"))?;
        audio
            .set_resource_provider(engine.host().resource_provider())
            .map_err(|error| format!("audio resource provider failed: {error}"))?;
        let mut runtime = RuntimeSession::new(
            engine,
            Box::new(NativeAssetStore::new(project_root.clone())),
            Box::new(audio),
            Box::new(krkr_core::VirtualClock::default()),
        );
        run_project_startup(&mut runtime).map_err(|error| error.to_string())?;

        Ok(Self {
            project_root,
            protocol,
            debug_interaction,
            runtime,
            presentation: TerminalRenderer::new(protocol),
            interaction: TerminalInteraction::new(viewport_size),
            debug_log: DebugInteractionLog::new(),
            transcript: TranscriptTracker::new(),
            native_transcript: NativeTranscriptTracker::new(),
            pending_events: Vec::new(),
            viewport_size,
            quit_requested: false,
        })
    }

    fn run(&mut self) -> Result<(), AppError> {
        let mut ui = TerminalUi::enter()?;
        ui.status(&self.status_text("starting"))?;
        let mut last_frame = Instant::now();

        loop {
            self.drain_audio_events();
            self.read_input()?;

            let now = Instant::now();
            let delta = now.duration_since(last_frame);
            last_frame = now;
            let frame_input = FrameInput::new(self.viewport_size, delta.as_secs_f32());
            let runtime_frame = self.runtime.update(
                EngineInput::new(frame_input, std::mem::take(&mut self.pending_events)),
                delta,
            )?;
            let mut frame = runtime_frame.engine;
            prepare_terminal_frame(self.runtime.engine().host(), &mut frame);

            self.interaction.refresh_candidates(
                self.runtime.engine().host().layer_tree(),
                self.viewport_size,
            );

            if self.debug_interaction {
                self.debug_log.write_if_changed(
                    &mut ui.stdout,
                    &self.interaction,
                    self.runtime
                        .engine()
                        .host()
                        .layer_tree()
                        .hit_test(self.interaction.cursor),
                    &frame,
                )?;
            } else if self.presentation.present(&mut ui.stdout, &frame)? {
                ui.stdout.flush()?;
            }
            let transcript = self.transcript.diff(&frame.message_layer);
            if !transcript.is_empty() {
                write_terminal_text(&mut ui.stdout, &transcript)?;
                ui.stdout.flush()?;
            }
            let native_text_events = self
                .runtime
                .engine_mut()
                .host_mut()
                .take_native_text_draw_events();
            let native_transcript = self
                .native_transcript
                .diff(&native_text_events, self.runtime.engine().host());
            if !native_transcript.is_empty() {
                write_terminal_text(&mut ui.stdout, &native_transcript)?;
                ui.stdout.flush()?;
            }

            if frame.input.unhandled_escape_pressed
                && !self.runtime.engine().host().termination_requested()
            {
                let _ = self.runtime.engine_mut().request_runtime_close();
            }
            ui.status(&self.status_text(&format!("{:?}", frame.tick.state)))?;
            if self.quit_requested || self.runtime.engine().host().termination_requested() {
                break;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        Ok(())
    }

    fn read_input(&mut self) -> io::Result<()> {
        while event::poll(Duration::from_millis(0))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                self.quit_requested = true;
                continue;
            }
            if self.handle_interaction_key(key) {
                continue;
            }
            if let Some(engine_key) = map_key(key) {
                self.pending_events.push(EngineEvent::KeyboardInput {
                    key: engine_key,
                    state: ButtonState::Pressed,
                    repeat: false,
                });
                self.pending_events.push(EngineEvent::KeyboardInput {
                    key: engine_key,
                    state: ButtonState::Released,
                    repeat: false,
                });
            }
        }
        Ok(())
    }

    fn handle_interaction_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.presentation.adjust_scale(0.1);
                true
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.presentation.adjust_scale(-0.1);
                true
            }
            KeyCode::Char('0') => {
                self.presentation.reset_scale();
                true
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                let step = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    32.0
                } else {
                    8.0
                };
                self.interaction.move_cursor(key.code, step);
                self.pending_events.push(EngineEvent::CursorMoved {
                    position: self.interaction.cursor,
                });
                true
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let reverse =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                if self.interaction.select_next(reverse) {
                    self.pending_events.push(EngineEvent::CursorMoved {
                        position: self.interaction.cursor,
                    });
                }
                true
            }
            KeyCode::Enter => {
                if self.interaction.selected.is_none() && self.interaction.candidates.len() == 1 {
                    self.interaction.selected = Some(0);
                    self.interaction.cursor = self.interaction.candidates[0].center;
                }
                self.pending_events.push(EngineEvent::CursorMoved {
                    position: self.interaction.cursor,
                });
                self.pending_events.push(EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Pressed,
                });
                self.pending_events.push(EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Released,
                });
                true
            }
            _ => false,
        }
    }

    fn drain_audio_events(&mut self) {
        let events = self.runtime.audio_mut().poll_events();
        for event in events {
            match event {
                AudioEvent::PlaybackStopped { id } => {
                    let _ = self.runtime.engine_mut().notify_audio_stopped(id);
                }
                AudioEvent::Status(status) => {
                    eprintln!("audio: {}", status.message);
                }
            }
        }
    }

    fn status_text(&self, state: &str) -> String {
        format!(
            " Kirakira terminal | {} | {:?} | img={:.0}% | cursor=({:.0},{:.0}) | +/- size | Tab target | Enter click | Esc close | Ctrl+C quit ",
            self.project_root.display(),
            self.protocol,
            self.presentation.display_scale * 100.0,
            self.interaction.cursor.x,
            self.interaction.cursor.y,
        ) + state
    }
}

#[derive(Clone, Debug, PartialEq)]
struct InteractionCandidate {
    id: LayerId,
    name: String,
    rect: Rect,
    center: Point,
    z_order: i32,
    opacity: u8,
}

impl InteractionCandidate {
    fn area(&self) -> f32 {
        self.rect.width * self.rect.height
    }
}

struct TerminalInteraction {
    cursor: Point,
    viewport_size: Size,
    candidates: Vec<InteractionCandidate>,
    selected: Option<usize>,
}

impl TerminalInteraction {
    fn new(viewport_size: Size) -> Self {
        Self {
            cursor: Point::new(viewport_size.width / 2.0, viewport_size.height / 2.0),
            viewport_size,
            candidates: Vec::new(),
            selected: None,
        }
    }

    fn refresh_candidates(&mut self, layers: &krkr_core::LayerTree, viewport_size: Size) {
        self.viewport_size = viewport_size;
        let previous_id = self
            .selected
            .and_then(|index| self.candidates.get(index))
            .map(|candidate| candidate.id);
        let viewport_area = (viewport_size.width * viewport_size.height).max(1.0);
        let mut candidates = layers
            .layers()
            .filter_map(|layer| {
                if !layer.renderable
                    || !layer.visible
                    || !layer.enabled
                    || layer.opacity == 0
                    || layer.image.is_none()
                    || layer.name.starts_with("kag:message")
                    || layer.width < 4.0
                    || layer.height < 4.0
                {
                    return None;
                }
                let origin = layers.absolute_position(layer.id)?;
                let rect = Rect::new(origin.x, origin.y, layer.width, layer.height);
                if rect.width * rect.height > viewport_area * 0.8
                    && rect.width > viewport_size.width * 0.65
                    && rect.height > viewport_size.height * 0.65
                {
                    return None;
                }
                let center = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                if !layers.hit_test_all(center).contains(&layer.id) {
                    return None;
                }
                Some(InteractionCandidate {
                    id: layer.id,
                    name: layer.name.clone(),
                    rect,
                    center,
                    z_order: layer.z_order,
                    opacity: layer.opacity,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| {
            a.rect
                .y
                .total_cmp(&b.rect.y)
                .then_with(|| a.rect.x.total_cmp(&b.rect.x))
                .then_with(|| b.z_order.cmp(&a.z_order))
                .then_with(|| a.id.cmp(&b.id))
        });
        self.candidates = candidates;
        self.selected = previous_id.and_then(|id| {
            self.candidates
                .iter()
                .position(|candidate| candidate.id == id)
        });
    }

    fn move_cursor(&mut self, key: KeyCode, step: f32) {
        match key {
            KeyCode::Left => self.cursor.x -= step,
            KeyCode::Right => self.cursor.x += step,
            KeyCode::Up => self.cursor.y -= step,
            KeyCode::Down => self.cursor.y += step,
            _ => {}
        }
        self.cursor.x = self.cursor.x.clamp(0.0, self.viewport_size.width.max(1.0));
        self.cursor.y = self.cursor.y.clamp(0.0, self.viewport_size.height.max(1.0));
        self.selected = self
            .candidates
            .iter()
            .position(|candidate| candidate.rect.contains(self.cursor));
    }

    fn select_next(&mut self, reverse: bool) -> bool {
        if self.candidates.is_empty() {
            self.selected = None;
            return false;
        }
        let len = self.candidates.len();
        let next = match (self.selected, reverse) {
            (Some(current), true) => (current + len - 1) % len,
            (Some(current), false) => (current + 1) % len,
            (None, true) => len - 1,
            (None, false) => 0,
        };
        self.selected = Some(next);
        self.cursor = self.candidates[next].center;
        true
    }

    fn selected_candidate(&self) -> Option<&InteractionCandidate> {
        self.selected.and_then(|index| self.candidates.get(index))
    }
}

#[derive(Default)]
struct DebugInteractionLog {
    last_key: Option<String>,
    last_snapshot: Option<String>,
}

impl DebugInteractionLog {
    fn new() -> Self {
        Self::default()
    }

    fn write_if_changed(
        &mut self,
        out: &mut impl Write,
        interaction: &TerminalInteraction,
        hit: Option<LayerId>,
        frame: &EngineFrame,
    ) -> io::Result<()> {
        let key = format_debug_key(interaction, hit, frame);
        if self.last_key.as_deref() == Some(key.as_str()) {
            return Ok(());
        }
        self.last_key = Some(key);

        let mut snapshot = format!(
            "state={:?} cursor=({:.0},{:.0}) hit={:?} selected={}\n",
            frame.tick.state,
            interaction.cursor.x,
            interaction.cursor.y,
            hit,
            interaction
                .selected_candidate()
                .map(format_candidate_short)
                .unwrap_or_else(|| "none".to_string())
        );
        for (index, candidate) in interaction.candidates.iter().take(24).enumerate() {
            let marker = if Some(index) == interaction.selected {
                '>'
            } else {
                ' '
            };
            snapshot.push_str(&format!(
                "{marker}{index:02} id={} name={} rect=({:.0},{:.0},{:.0},{:.0}) center=({:.0},{:.0}) z={} opacity={} area={:.0}\n",
                candidate.id,
                candidate.name,
                candidate.rect.x,
                candidate.rect.y,
                candidate.rect.width,
                candidate.rect.height,
                candidate.center.x,
                candidate.center.y,
                candidate.z_order,
                candidate.opacity,
                candidate.area(),
            ));
        }
        if interaction.candidates.len() > 24 {
            snapshot.push_str(&format!(
                "... {} more candidates\n",
                interaction.candidates.len() - 24
            ));
        }
        self.last_snapshot = Some(snapshot.clone());
        writeln!(out, "\n[debug-interaction]\n{snapshot}")?;
        out.flush()
    }
}

fn format_debug_key(
    interaction: &TerminalInteraction,
    hit: Option<LayerId>,
    frame: &EngineFrame,
) -> String {
    let mut key = format!(
        "{:?}|{:.0}|{:.0}|{}|{}",
        frame.tick.state,
        interaction.cursor.x,
        interaction.cursor.y,
        hit.is_some(),
        interaction
            .selected
            .map(|index| index.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    for candidate in &interaction.candidates {
        key.push_str(&format!(
            "|{:.0},{:.0},{:.0},{:.0},{}",
            candidate.rect.x,
            candidate.rect.y,
            candidate.rect.width,
            candidate.rect.height,
            candidate.z_order
        ));
    }
    key
}

fn format_candidate_short(candidate: &InteractionCandidate) -> String {
    format!(
        "{}:{}@({:.0},{:.0})",
        candidate.id, candidate.name, candidate.center.x, candidate.center.y
    )
}

fn run_project_startup(runtime: &mut RuntimeSession) -> Result<(), Box<dyn std::error::Error>> {
    runtime.start_project()?;
    Ok(())
}

fn initial_project_root() -> Option<PathBuf> {
    if env::current_dir()
        .ok()
        .as_ref()
        .is_some_and(|path| looks_like_project_root(path))
    {
        return env::current_dir().ok();
    }
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
        .or_else(|| env::current_dir().ok())
}

fn system_paths_for_project(root: &Path) -> SystemPaths {
    let root_display = root.display().to_string();
    let temp_display = env::temp_dir().display().to_string();
    SystemPaths {
        exe_path: format!("{}/", root_display.trim_end_matches(['/', '\\'])),
        data_path: if cfg!(windows) {
            let data = root.join("savedata").display().to_string();
            format!("{}\\", data.trim_end_matches(['/', '\\']))
        } else {
            "savedata/".to_string()
        },
        personal_path: format!("{}/", temp_display.trim_end_matches(['/', '\\'])),
        app_data_path: format!("{}/", temp_display.trim_end_matches(['/', '\\'])),
    }
}

fn looks_like_project_root(path: &Path) -> bool {
    path.join("startup.tjs").is_file()
        || path.join("startup.ks").is_file()
        || directory_has_xp3(path)
        || directory_has_xp3(&path.join("sys"))
}

fn directory_has_xp3(path: &Path) -> bool {
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

fn prepare_terminal_frame(host: &KrkrHost, frame: &mut EngineFrame) {
    let hidden_layers = terminal_hidden_layer_ids(host);
    let (draw_commands, image_uploads) = host
        .layer_tree()
        .draw_model_filtered(|layer| !hidden_layers.contains(&layer.id));
    frame.output.draw_commands = draw_commands
        .into_iter()
        .filter(|command| matches!(command, DrawCommand::Image(_)))
        .collect();
    frame.output.image_uploads = image_uploads;
    frame.output.transition = None;
}

fn terminal_hidden_layer_ids(host: &KrkrHost) -> BTreeSet<LayerId> {
    let layers = host.layer_tree();
    let mut hidden = BTreeSet::new();
    for layer in layers.layers() {
        if terminal_dialogue_layer(host, layer.id)
            || terminal_message_frame_layer(host, layer.id)
            || terminal_system_button_layer(host, layer.id)
        {
            hidden.insert(layer.id);
        }
    }
    hidden
}

fn terminal_dialogue_layer(host: &KrkrHost, layer_id: LayerId) -> bool {
    host.kag_layer_slot_for_render_layer(layer_id)
        .is_some_and(|(_, layer)| matches!(layer.as_str(), "message0" | "message3"))
}

fn terminal_message_frame_layer(host: &KrkrHost, layer_id: LayerId) -> bool {
    let Some((_, layer)) = host.kag_layer_slot_for_render_layer(layer_id) else {
        return false;
    };
    if !matches!(layer.as_str(), "2" | "3") {
        return false;
    }
    host.layer_image_storage(layer_id)
        .map(storage_stem)
        .is_some_and(|storage| matches!(storage.as_str(), "win" | "win-2"))
}

fn terminal_system_button_layer(host: &KrkrHost, layer_id: LayerId) -> bool {
    host.layer_image_storage(layer_id)
        .map(storage_stem)
        .is_some_and(|storage| storage.starts_with("sysbt_bt_"))
}

fn storage_stem(storage: &str) -> String {
    let normalized = storage.rsplit(['/', '\\']).next().unwrap_or(storage);
    normalized
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(normalized)
        .to_ascii_lowercase()
}

#[derive(Default)]
struct TranscriptTracker {
    last_page: usize,
    last_lines: Vec<String>,
    initialized: bool,
}

impl TranscriptTracker {
    fn new() -> Self {
        Self::default()
    }

    fn diff(&mut self, model: &MessageLayerModel) -> String {
        let mut out = String::new();
        if !self.initialized {
            self.last_page = model.page;
            self.initialized = true;
        } else if model.page != self.last_page {
            self.last_page = model.page;
            self.last_lines.clear();
            out.push('\n');
        }

        if model.lines.len() < self.last_lines.len() {
            self.last_lines = model.lines.clone();
            return out;
        }

        for (index, line) in model.lines.iter().enumerate() {
            let old = self.last_lines.get(index).map(String::as_str).unwrap_or("");
            let suffix = if line.starts_with(old) {
                &line[old.len()..]
            } else {
                line.as_str()
            };
            if index >= self.last_lines.len() && index > 0 {
                out.push('\n');
            }
            out.push_str(suffix);
        }
        self.last_lines = model.lines.clone();
        out
    }
}

#[derive(Default)]
struct NativeTranscriptTracker {
    seen: BTreeSet<u64>,
}

impl NativeTranscriptTracker {
    fn new() -> Self {
        Self::default()
    }

    fn diff(&mut self, events: &[NativeTextDrawEvent], host: &KrkrHost) -> String {
        let lines = coalesce_native_text_events(events, host);
        let mut out = String::new();
        for line in lines {
            let mut hasher = DefaultHasher::new();
            line.hash(&mut hasher);
            if !self.seen.insert(hasher.finish()) {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&line);
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }
}

fn coalesce_native_text_events(events: &[NativeTextDrawEvent], host: &KrkrHost) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_key: Option<(Option<LayerId>, i64)> = None;
    let mut current = String::new();
    for event in events {
        if !native_text_event_is_dialogue(event, host) {
            continue;
        }
        let text = normalize_native_text(&event.text);
        if text.is_empty() {
            continue;
        }
        let key = (event.layer_id, event.y / 8);
        let char_like = text.chars().count() <= 2 && !text.contains('\n');
        if char_like && current_key == Some(key) {
            current.push_str(&text);
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if char_like {
            current_key = Some(key);
            current.push_str(&text);
        } else {
            current_key = None;
            lines.push(text);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn normalize_native_text(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_terminal_text(out: &mut impl Write, text: &str) -> io::Result<()> {
    for ch in text.chars() {
        if ch == '\n' {
            out.write_all(b"\r\n")?;
        } else {
            write!(out, "{ch}")?;
        }
    }
    Ok(())
}

fn native_text_event_is_dialogue(event: &NativeTextDrawEvent, host: &KrkrHost) -> bool {
    let Some(layer_id) = event.layer_id else {
        return false;
    };
    terminal_dialogue_layer(host, layer_id)
}

struct TerminalRenderer {
    protocol: ImageProtocol,
    textures: BTreeMap<TextureId, UploadedTexture>,
    last_hash: Option<u64>,
    display_scale: f32,
}

impl TerminalRenderer {
    fn new(protocol: ImageProtocol) -> Self {
        Self {
            protocol,
            textures: BTreeMap::new(),
            last_hash: None,
            display_scale: 0.5,
        }
    }

    fn adjust_scale(&mut self, delta: f32) {
        self.display_scale = (self.display_scale + delta).clamp(0.25, 2.0);
        self.last_hash = None;
    }

    fn reset_scale(&mut self) {
        self.display_scale = 0.5;
        self.last_hash = None;
    }

    fn present(&mut self, out: &mut impl Write, frame: &EngineFrame) -> Result<bool, AppError> {
        for upload in &frame.output.image_uploads {
            self.textures.insert(upload.texture_id, upload.into());
        }
        let scene = compose_scene(&frame.output, &self.textures);
        let hash = scene.visual_hash();
        if self.last_hash == Some(hash) {
            return Ok(false);
        }
        self.last_hash = Some(hash);
        write_scene_image(out, self.protocol, &scene, self.display_scale)?;
        Ok(true)
    }
}

#[derive(Clone)]
struct UploadedTexture {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl From<&ImageUpload> for UploadedTexture {
    fn from(upload: &ImageUpload) -> Self {
        Self {
            width: upload.width,
            height: upload.height,
            rgba: upload.rgba.to_vec(),
        }
    }
}

struct SceneBuffer {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl SceneBuffer {
    fn visual_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.width.hash(&mut hasher);
        self.height.hash(&mut hasher);
        self.rgba.hash(&mut hasher);
        hasher.finish()
    }
}

fn compose_scene(
    output: &krkr_core::FrameOutput,
    textures: &BTreeMap<TextureId, UploadedTexture>,
) -> SceneBuffer {
    let bounds = output
        .draw_commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Image(image) => Some(image.rect),
            DrawCommand::Rect(_) | DrawCommand::Text(_) => None,
        })
        .fold(Rect::new(0.0, 0.0, 1.0, 1.0), union_rect);
    let width = bounds.width.ceil().max(1.0).min(4096.0) as u32;
    let height = bounds.height.ceil().max(1.0).min(4096.0) as u32;
    let mut scene = SceneBuffer {
        width,
        height,
        rgba: vec![0; width as usize * height as usize * 4],
    };
    fill_color(&mut scene, output.clear_color);
    for command in &output.draw_commands {
        if let DrawCommand::Image(image) = command
            && let Some(texture) = textures.get(&image.texture_id)
        {
            composite_image(&mut scene, texture, image);
        }
    }
    scene
}

fn union_rect(acc: Rect, rect: Rect) -> Rect {
    let x0 = acc.x.min(rect.x).min(0.0);
    let y0 = acc.y.min(rect.y).min(0.0);
    let x1 = (acc.x + acc.width).max(rect.x + rect.width);
    let y1 = (acc.y + acc.height).max(rect.y + rect.height);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

fn fill_color(scene: &mut SceneBuffer, color: Color) {
    let r = float_byte(color.r);
    let g = float_byte(color.g);
    let b = float_byte(color.b);
    let a = float_byte(color.a);
    for pixel in scene.rgba.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[r, g, b, a]);
    }
}

fn composite_image(scene: &mut SceneBuffer, texture: &UploadedTexture, command: &ImageCommand) {
    let x0 = command.rect.x.floor().max(0.0) as u32;
    let y0 = command.rect.y.floor().max(0.0) as u32;
    let x1 = (command.rect.x + command.rect.width)
        .ceil()
        .min(scene.width as f32)
        .max(0.0) as u32;
    let y1 = (command.rect.y + command.rect.height)
        .ceil()
        .min(scene.height as f32)
        .max(0.0) as u32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    for y in y0..y1 {
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - command.rect.x) / command.rect.width.max(1.0);
            let v = (y as f32 + 0.5 - command.rect.y) / command.rect.height.max(1.0);
            let sx = (command.source_rect.x + u * command.source_rect.width)
                .floor()
                .clamp(0.0, texture.width.saturating_sub(1) as f32) as u32;
            let sy = (command.source_rect.y + v * command.source_rect.height)
                .floor()
                .clamp(0.0, texture.height.saturating_sub(1) as f32) as u32;
            let src = ((sy * texture.width + sx) * 4) as usize;
            let dst = ((y * scene.width + x) * 4) as usize;
            let alpha = texture.rgba[src + 3] as f32 / 255.0 * command.opacity.clamp(0.0, 1.0);
            for channel in 0..3 {
                scene.rgba[dst + channel] = (texture.rgba[src + channel] as f32 * alpha
                    + scene.rgba[dst + channel] as f32 * (1.0 - alpha))
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            scene.rgba[dst + 3] = ((alpha + scene.rgba[dst + 3] as f32 / 255.0 * (1.0 - alpha))
                * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}

fn float_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn write_scene_image(
    out: &mut impl Write,
    protocol: ImageProtocol,
    scene: &SceneBuffer,
    display_scale: f32,
) -> Result<(), AppError> {
    match protocol {
        ImageProtocol::Kitty => write_kitty(out, scene, display_scale),
        ImageProtocol::Iterm2 => write_iterm2(out, scene, display_scale),
        ImageProtocol::Sixel => write_sixel(out, scene),
        ImageProtocol::Ansi => write_ansi_halfblocks(out, scene, display_scale),
    }
}

fn write_kitty(
    out: &mut impl Write,
    scene: &SceneBuffer,
    display_scale: f32,
) -> Result<(), AppError> {
    let png = encode_png(scene)?;
    let data = BASE64.encode(png);
    let (cols, _) = terminal_dimensions();
    let display_cols = scaled_terminal_columns(cols, display_scale);
    write!(
        out,
        "\r\n\x1b_Gf=100,a=T,c={display_cols},m=0;{data}\x1b\\\r\n"
    )?;
    Ok(())
}

fn write_iterm2(
    out: &mut impl Write,
    scene: &SceneBuffer,
    display_scale: f32,
) -> Result<(), AppError> {
    let png = encode_png(scene)?;
    let data = BASE64.encode(png);
    let (cols, _) = terminal_dimensions();
    let display_cols = scaled_terminal_columns(cols, display_scale);
    write!(
        out,
        "\r\n\x1b]1337;File=inline=1;width={display_cols};height=auto;preserveAspectRatio=1:{data}\x07\r\n"
    )?;
    Ok(())
}

fn write_sixel(out: &mut impl Write, scene: &SceneBuffer) -> Result<(), AppError> {
    let sixel = icy_sixel::SixelImage::try_from_rgba(
        scene.rgba.clone(),
        scene.width as usize,
        scene.height as usize,
    )?
    .encode()?;
    write!(out, "\r\n{sixel}\r\n")?;
    Ok(())
}

fn write_ansi_halfblocks(
    out: &mut impl Write,
    scene: &SceneBuffer,
    display_scale: f32,
) -> Result<(), AppError> {
    let (cols, rows) = terminal_dimensions();
    let display_cols = scaled_terminal_columns(cols, display_scale);
    let target_w = scene.width.min(display_cols as u32).max(1);
    let target_h = scene
        .height
        .min(rows.saturating_sub(2).max(1) as u32 * 2)
        .max(1);
    for row in (0..target_h).step_by(2) {
        for col in 0..target_w {
            let top = sample(scene, col, row, target_w, target_h);
            let bottom = sample(scene, col, (row + 1).min(target_h - 1), target_w, target_h);
            write!(
                out,
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▀",
                top[0], top[1], top[2], bottom[0], bottom[1], bottom[2]
            )?;
        }
        writeln!(out, "\x1b[0m")?;
    }
    Ok(())
}

fn scaled_terminal_columns(cols: u16, display_scale: f32) -> u16 {
    ((cols.max(1) as f32 * display_scale.clamp(0.25, 2.0)).round() as u16).max(1)
}

fn encode_png(scene: &SceneBuffer) -> Result<Vec<u8>, AppError> {
    let image =
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(scene.width, scene.height, scene.rgba.clone())
            .ok_or_else(|| AppError::Message("invalid scene buffer".to_string()))?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    image.write_to(&mut cursor, ImageFormat::Png)?;
    Ok(cursor.into_inner())
}

fn sample(scene: &SceneBuffer, x: u32, y: u32, target_w: u32, target_h: u32) -> [u8; 4] {
    let sx = (x as u64 * scene.width as u64 / target_w as u64).min(scene.width as u64 - 1) as u32;
    let sy = (y as u64 * scene.height as u64 / target_h as u64).min(scene.height as u64 - 1) as u32;
    let index = ((sy * scene.width + sx) * 4) as usize;
    [
        scene.rgba[index],
        scene.rgba[index + 1],
        scene.rgba[index + 2],
        scene.rgba[index + 3],
    ]
}

fn terminal_dimensions() -> (u16, u16) {
    terminal_size()
        .map(|(Width(width), Height(height))| (width, height))
        .unwrap_or((80, 24))
}

struct TerminalUi {
    stdout: io::Stdout,
    last_status: Option<String>,
}

impl TerminalUi {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, cursor::Hide, terminal::Clear(ClearType::All))?;
        let (_, rows) = terminal_dimensions();
        write!(stdout, "\x1b[1;{}r", rows.saturating_sub(1).max(1))?;
        stdout.flush()?;
        Ok(Self {
            stdout,
            last_status: None,
        })
    }

    fn status(&mut self, text: &str) -> io::Result<()> {
        let (cols, rows) = terminal_dimensions();
        let mut label = text.to_string();
        label.truncate(cols as usize);
        if self.last_status.as_deref() == Some(label.as_str()) {
            return Ok(());
        }
        self.last_status = Some(label.clone());
        execute!(
            self.stdout,
            cursor::SavePosition,
            cursor::MoveTo(0, rows.saturating_sub(1)),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        write!(
            self.stdout,
            "\x1b[7m{label:<width$}\x1b[0m",
            width = cols as usize
        )?;
        execute!(self.stdout, cursor::RestorePosition)?;
        self.stdout.flush()
    }
}

impl Drop for TerminalUi {
    fn drop(&mut self) {
        let _ = write!(self.stdout, "\x1b[r\x1b[0m");
        let _ = execute!(self.stdout, cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

fn map_key(key: KeyEvent) -> Option<EngineKey> {
    match key.code {
        KeyCode::Enter => Some(EngineKey::Enter),
        KeyCode::Char(' ') => Some(EngineKey::Space),
        KeyCode::Esc => Some(EngineKey::Escape),
        KeyCode::Tab => Some(EngineKey::Tab),
        KeyCode::Left => Some(EngineKey::Left),
        KeyCode::Right => Some(EngineKey::Right),
        KeyCode::Up => Some(EngineKey::Up),
        KeyCode::Down => Some(EngineKey::Down),
        KeyCode::PageUp => Some(EngineKey::PageUp),
        KeyCode::PageDown => Some(EngineKey::PageDown),
        KeyCode::Backspace => Some(EngineKey::Backspace),
        KeyCode::Delete => Some(EngineKey::Delete),
        KeyCode::Char(c) => Some(EngineKey::Character(c)),
        _ => None,
    }
}

#[derive(Debug)]
enum AppError {
    Io(io::Error),
    Engine(krkr_tjs2::TjsError),
    Kag(krkr_kag::KagError),
    Audio(krkr_audio::AudioError),
    Image(image::ImageError),
    Sixel(icy_sixel::SixelError),
    Message(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Engine(error) => write!(formatter, "{error}"),
            Self::Kag(error) => write!(formatter, "{error}"),
            Self::Audio(error) => write!(formatter, "{error}"),
            Self::Image(error) => write!(formatter, "{error}"),
            Self::Sixel(error) => write!(formatter, "{error}"),
            Self::Message(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<krkr_tjs2::TjsError> for AppError {
    fn from(error: krkr_tjs2::TjsError) -> Self {
        Self::Engine(error)
    }
}

impl From<krkr_kag::KagError> for AppError {
    fn from(error: krkr_kag::KagError) -> Self {
        Self::Kag(error)
    }
}

impl From<krkr_audio::AudioError> for AppError {
    fn from(error: krkr_audio::AudioError) -> Self {
        Self::Audio(error)
    }
}

impl From<RuntimeSessionError> for AppError {
    fn from(error: RuntimeSessionError) -> Self {
        match error {
            RuntimeSessionError::Engine(error) => Self::Engine(error),
            RuntimeSessionError::Audio(error) => Self::Audio(error),
        }
    }
}

impl From<image::ImageError> for AppError {
    fn from(error: image::ImageError) -> Self {
        Self::Image(error)
    }
}

impl From<icy_sixel::SixelError> for AppError {
    fn from(error: icy_sixel::SixelError) -> Self {
        Self::Sixel(error)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use krkr_core::{FrameOutput, ImageUpload};

    #[test]
    fn transcript_tracker_emits_only_appended_text() {
        let mut tracker = TranscriptTracker::new();
        let mut model = MessageLayerModel::default();
        model.append_text("Hel");
        assert_eq!(tracker.diff(&model), "Hel");
        model.append_text("lo");
        assert_eq!(tracker.diff(&model), "lo");
        model.newline();
        model.append_text("World");
        assert_eq!(tracker.diff(&model), "\nWorld");
        assert_eq!(tracker.diff(&model), "");
        model.page_break();
        model.clear_text();
        model.append_text("Next");
        assert_eq!(tracker.diff(&model), "\nNext");
    }

    #[test]
    fn visual_hash_ignores_transcript_only_changes() {
        let texture = UploadedTexture {
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 255],
        };
        let mut textures = BTreeMap::new();
        textures.insert(1, texture);
        let output = FrameOutput::new(
            Color::new(0.0, 0.0, 0.0, 1.0),
            vec![DrawCommand::Image(ImageCommand {
                texture_id: 1,
                rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                source_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                texture_size: Size::new(1.0, 1.0),
                opacity: 1.0,
            })],
        );
        let first = compose_scene(&output, &textures).visual_hash();
        let second = compose_scene(&output, &textures).visual_hash();
        assert_eq!(first, second);
    }

    #[test]
    fn compositor_handles_opacity_source_rect_and_scaling() {
        let upload = ImageUpload::new(1, 2, 1, Arc::from(vec![255, 0, 0, 255, 0, 0, 255, 255]));
        let mut textures = BTreeMap::new();
        textures.insert(1, UploadedTexture::from(&upload));
        let output = FrameOutput::new(
            Color::new(0.0, 0.0, 0.0, 1.0),
            vec![DrawCommand::Image(ImageCommand {
                texture_id: 1,
                rect: Rect::new(0.0, 0.0, 2.0, 1.0),
                source_rect: Rect::new(1.0, 0.0, 1.0, 1.0),
                texture_size: Size::new(2.0, 1.0),
                opacity: 0.5,
            })],
        );
        let scene = compose_scene(&output, &textures);
        assert_eq!(&scene.rgba[0..4], &[0, 0, 128, 255]);
        assert_eq!(&scene.rgba[4..8], &[0, 0, 128, 255]);
    }

    #[test]
    fn protocol_selection_respects_cli_override() {
        assert_eq!(
            select_image_protocol(ImageProtocolArg::Ansi),
            ImageProtocol::Ansi
        );
        assert_eq!(
            select_image_protocol(ImageProtocolArg::Kitty),
            ImageProtocol::Kitty
        );
    }

    #[test]
    fn terminal_text_writer_uses_crlf_for_newlines() {
        let mut out = Vec::new();
        write_terminal_text(&mut out, "A\nB\n").expect("write transcript");
        assert_eq!(String::from_utf8(out).expect("utf8"), "A\r\nB\r\n");
    }
}
