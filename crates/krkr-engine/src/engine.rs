use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use krkr_core::ProjectStoragePort;
use krkr_core::{
    AudioBus, AudioCommand, AudioInstanceId, AudioLoadPolicy, ButtonState, Color,
    Engine as CoreEngine, EngineConfig as CoreEngineConfig, EngineEvent, EngineKey, FrameInput,
    FrameOutput, ImageUpload, LayerId, MessageLayerModel, Point, PointerButton, Size,
    TextInputEvent, TransitionMethod, TransitionParams, TransitionScrollFrom, TransitionScrollStay,
};
use krkr_kag::{KagError, KagParser, ParserSnapshot, Tag};
use krkr_tjs2::{
    Result, TjsError,
    debug::Pause,
    runtime::{ObjectHandle, Runtime, Variant},
};
use krkr_video::{UnavailableVideoFactory, VideoDecoderFactory};

#[cfg(test)]
use krkr_assets::ProjectStorage;

use crate::{
    globals::install_tvp_globals,
    host::{
        ImageLoadRequest, ImageLoadState, ImageLoadTarget, KrkrHost, SystemPaths, TraceCategory,
    },
    kag::{EngineKagHost, tag_to_dictionary},
    native::classes::{
        apply_completed_image_load, apply_completed_resource_loads, call_wave_status_changed,
        complete_layer_before_draw, complete_pending_layer_paints,
        finish_completed_native_transitions, plain_member_object, register_kag_layer_slots_from_tjs,
        set_wave_paused, set_wave_status,
    },
    native::{
        create_kag_parser_object, kag_to_tjs, refresh_kag_parser_object, tick_video_overlays,
        video_overlay_frame_quads,
    },
    plugin::KrkrPlugin,
    scheduler::{
        ASYNC_TRIGGER_EVENT_NAME, AUDIO_FADE_COMPLETED_EVENT_NAME, IdleEvent, ScriptEvent,
        ScriptEventKind, ScriptEventSelection, TIMER_EVENT_NAME,
    },
    script::{
        execute_bytecode_if_present_on_runtime, execute_expression_on_runtime,
        execute_script_on_runtime,
    },
};

/// Resource waits can be wrapped in a TJS runtime stack trace while crossing
/// a suspended native call. Keep one predicate for all host/update boundaries
/// so those waits never escape as fatal frame errors.
fn is_resource_pending_error(error: &TjsError) -> bool {
    error.kind == krkr_tjs2::TjsErrorKind::ResourcePending
        || error.to_string().contains("KAG resource is pending:")
}

/// Wall-clock budget timer that remains safe on browser WASM targets. The
/// standard library's `Instant` is unavailable there; the browser host still
/// supplies frame pacing through `EngineInput.delta`, so a zero elapsed value
/// simply disables the optional per-tick wall-time guard.
#[derive(Clone, Copy, Debug)]
struct BudgetTimer(Option<Instant>);

impl BudgetTimer {
    fn start() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            Self(None)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self(Some(Instant::now()))
        }
    }

    fn elapsed(self) -> Duration {
        self.0
            .map(|started| started.elapsed())
            .unwrap_or(Duration::ZERO)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KagRunBudget {
    pub max_tags_per_tick: usize,
    pub max_wall_time: Duration,
}

impl Default for KagRunBudget {
    fn default() -> Self {
        Self {
            max_tags_per_tick: 1000,
            // The wall-clock guard exists to keep a frame responsive, not to
            // shape script semantics: a tag run that decodes an image easily
            // outlives 16 ms, so how much of a scenario one `update` covers
            // depends on the machine.  Unit tests assert on the state a single
            // update produced -- the doc comment on this struct's timer calls
            // it an "optional per-tick guard" -- so under `cfg(test)` the
            // guard is pinned wide enough to never trip and the tests stay
            // about what the tags do.  The budget's own behaviour is covered
            // by the tests that pass it explicitly.
            #[cfg(test)]
            max_wall_time: Duration::from_secs(1),
            #[cfg(not(test))]
            max_wall_time: Duration::from_millis(16),
        }
    }
}

#[derive(Clone)]
pub struct EngineConfig {
    /// Storage is created by the platform shell and injected into the engine.
    /// The engine never discovers a project path or opens the process current
    /// directory itself.
    pub project_storage: Option<Arc<dyn ProjectStoragePort>>,
    /// Host-provided logical paths exposed through the KRKR `System` object.
    pub system_paths: SystemPaths,
    pub kag_budget: KagRunBudget,
    pub system_metrics: SystemMetrics,
    /// Host-owned video decoder capability. Web supplies a DOM overlay and
    /// native shells select their OS framework without engine-side cfg paths.
    pub video_factory: Arc<dyn VideoDecoderFactory>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            project_storage: None,
            system_paths: SystemPaths::default(),
            kag_budget: KagRunBudget::default(),
            system_metrics: SystemMetrics::default(),
            video_factory: Arc::new(UnavailableVideoFactory),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemMetrics {
    pub screen_width: i64,
    pub screen_height: i64,
    pub desktop_left: i64,
    pub desktop_top: i64,
    pub desktop_width: i64,
    pub desktop_height: i64,
}

impl Default for SystemMetrics {
    fn default() -> Self {
        Self {
            screen_width: 1920,
            screen_height: 1080,
            desktop_left: 0,
            desktop_top: 0,
            desktop_width: 1920,
            desktop_height: 1080,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KagTaskState {
    Running,
    WaitingClick,
    WaitingTimer { remaining: Duration },
    WaitingTransition,
    WaitingAudio,
    WaitingResource,
    WaitingModal,
    Finished,
    Error { message: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KagYieldReason {
    AlreadyBlocked,
    BudgetExhausted,
    HandlerYield,
    Waiting(KagTaskState),
    Finished,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineTickResult {
    pub state: KagTaskState,
    pub reason: KagYieldReason,
    pub tags_processed: usize,
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EngineInputResult {
    pub unhandled_escape_pressed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineInput {
    pub frame: FrameInput,
    pub events: Vec<EngineEvent>,
    /// Text committed/composed by an IME. Kept separate from pointer/key
    /// events because composition payloads are owned UTF-8 strings rather than
    /// small copyable key codes.
    pub text: Vec<TextInputEvent>,
    /// Host clock sample for deterministic cross-platform scheduling. `None`
    /// keeps the legacy host-owned clock behavior for direct engine callers.
    pub now_millis: Option<i64>,
}

impl EngineInput {
    pub fn new(frame: FrameInput, events: Vec<EngineEvent>) -> Self {
        Self {
            frame,
            events,
            text: Vec::new(),
            now_millis: None,
        }
    }

    pub fn with_text(mut self, text: Vec<TextInputEvent>) -> Self {
        self.text = text;
        self
    }

    pub fn with_now_millis(mut self, now_millis: i64) -> Self {
        self.now_millis = Some(now_millis);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KagLocation {
    pub storage: Option<String>,
    pub label: Option<String>,
    pub line: Option<usize>,
    pub page: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineFrame {
    pub output: FrameOutput,
    pub tick: EngineTickResult,
    pub input: EngineInputResult,
    pub message_layer: MessageLayerModel,
    pub location: KagLocation,
}

/// Typed host-facing resource request emitted at a frame boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalResourceRequest {
    pub path: String,
    pub kind: krkr_core::AssetKind,
}

/// A frame is either ready for presentation or waiting on one or more
/// external resources. Waiting is normal scheduling state, not an exception
/// that a host must detect by parsing an error string.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineStep {
    Ready {
        frame: EngineFrame,
        requests: Vec<ExternalResourceRequest>,
    },
    Waiting {
        frame: EngineFrame,
        requests: Vec<ExternalResourceRequest>,
    },
}

pub struct KrkrEngine {
    tjs_runtime: Runtime<KrkrHost>,
    kag_session: KagSession,
    core_engine: CoreEngine,
    kag_budget: KagRunBudget,
    plugins: Vec<Arc<dyn KrkrPlugin>>,
    cursor_position: Option<Point>,
    /// Rounded cursor position of the last dispatched move, so
    /// `onMouseMove` only fires when the position actually changed
    /// (`tTVPLayerManager::LastMouseMoveX/Y`, `LayerManager.cpp:400`).
    last_mouse_move: Option<(i64, i64)>,
    pressed_layer: Option<LayerId>,
    input_result: EngineInputResult,
    scheduler_turn_started: bool,
    /// Session state the parked-VM marker replaced, restored once the parked
    /// work lands. `None` while no mark is active.
    parked_resource_state: Option<KagTaskState>,
}

impl KrkrEngine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let host = match config.project_storage {
            Some(storage) => KrkrHost::from_storage_port(
                storage,
                config.system_paths,
                Arc::clone(&config.video_factory),
            )?,
            None => KrkrHost::with_system_paths_and_video_factory(
                config.system_paths,
                Arc::clone(&config.video_factory),
            ),
        };
        let mut tjs_runtime = Runtime::with_host(host);
        install_tvp_globals(&mut tjs_runtime);
        install_system_metrics(&mut tjs_runtime, config.system_metrics);
        Ok(Self {
            tjs_runtime,
            kag_session: KagSession::new(),
            core_engine: CoreEngine::new(CoreEngineConfig::default()),
            kag_budget: config.kag_budget,
            plugins: Vec::new(),
            cursor_position: None,
            last_mouse_move: None,
            pressed_layer: None,
            input_result: EngineInputResult::default(),
            scheduler_turn_started: false,
            parked_resource_state: None,
        })
    }

    #[cfg(test)]
    fn for_project(root: &std::path::Path) -> Result<Self> {
        let storage = ProjectStorage::for_root(root)?;
        Self::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            // Test projects exercise the same host-selected native backend as
            // the desktop shell; production engine defaults stay inert.
            video_factory: Arc::new(krkr_video::PlatformVideoFactory),
            ..EngineConfig::default()
        })
    }

    pub fn tjs_runtime(&self) -> &Runtime<KrkrHost> {
        &self.tjs_runtime
    }

    pub fn tjs_runtime_mut(&mut self) -> &mut Runtime<KrkrHost> {
        &mut self.tjs_runtime
    }

    pub fn host(&self) -> &KrkrHost {
        self.tjs_runtime.host()
    }

    pub fn host_mut(&mut self) -> &mut KrkrHost {
        self.tjs_runtime.host_mut()
    }

    pub fn video_overlay_snapshots(&mut self) -> Vec<crate::VideoOverlaySnapshot> {
        self.tjs_runtime.host_mut().video_overlay_snapshots()
    }

    pub fn preferred_viewport_size(&self) -> Option<Size> {
        let window = self.runtime_window_object()?;
        // `width`/`height` report the paint box, which is the logical inner
        // size scaled by `zoomNumer/zoomDenom` (`WindowFormUnit.cpp:681`).
        let width = object_positive_i64(&self.tjs_runtime, window, "width")
            .or_else(|| object_positive_i64(&self.tjs_runtime, window, "innerWidth"))?;
        let height = object_positive_i64(&self.tjs_runtime, window, "height")
            .or_else(|| object_positive_i64(&self.tjs_runtime, window, "innerHeight"))?;
        Some(Size::new(width as f32, height as f32))
    }

    /// Returns the logical scene size used by layers and hit testing.
    ///
    /// KRKR keeps the script window size (`innerWidth`/`innerHeight`) separate
    /// from the scene/design size (`scWidth`/`scHeight`).  A desktop window may
    /// be 1440x810 while a game still lays out its 1920x1080 scene; shells must
    /// render and translate pointer coordinates in the latter coordinate
    /// system so the same scale transform is applied in both directions.
    pub fn content_viewport_size(&self) -> Option<Size> {
        let window = self.runtime_window_object()?;
        let width = object_positive_i64(&self.tjs_runtime, window, "scWidth")
            .or_else(|| object_positive_i64(&self.tjs_runtime, window, "width"))?;
        let height = object_positive_i64(&self.tjs_runtime, window, "scHeight")
            .or_else(|| object_positive_i64(&self.tjs_runtime, window, "height"))?;
        Some(Size::new(width as f32, height as f32))
    }

    /// Updates the host-provided System screen metrics after a window/canvas
    /// resize. Platform shells own the actual viewport; scripts only observe
    /// this synchronized snapshot.
    pub fn set_system_metrics(&mut self, metrics: SystemMetrics) {
        install_system_metrics(&mut self.tjs_runtime, metrics);
    }

    pub fn persist_runtime_state(&mut self) -> Result<()> {
        let Some(window) = self.runtime_window_object() else {
            return Ok(());
        };
        self.sync_kag_system_state_for_persist(window);
        if matches!(
            self.tjs_runtime
                .object_member(window, "saveSystemVariables"),
            Variant::Void
        ) {
            return Ok(());
        }
        self.tjs_runtime
            .call_object_method(window, "saveSystemVariables", Vec::new())
            .map(|_| ())
    }

    fn sync_kag_system_state_for_persist(&mut self, window: ObjectHandle) {
        // A KAG object stored by script arrives bound to itself
        // (`Variant::self_bound`); the engine's own readers want the object.
        let Some(scflags) = self
            .tjs_runtime
            .object_member(window, "scflags")
            .object_handle()
        else {
            return;
        };

        let full_screen = i64::from(
            self.tjs_runtime
                .object_member(window, "fullScreen")
                .is_truthy(),
        );
        self.tjs_runtime
            .set_object_member(window, "fullScreened", Variant::Integer(full_screen));
        self.tjs_runtime
            .set_object_member(scflags, "fullScreen", Variant::Integer(full_screen));

        self.sync_kag_bgm_system_state(window, scflags);
        self.sync_kag_se_system_state(window, scflags);
        self.sync_kag_sflags_audio_state(window);
    }

    fn sync_kag_bgm_system_state(&mut self, window: ObjectHandle, scflags: ObjectHandle) {
        let Some(bgm) = self.tjs_runtime.object_member(window, "bgm").object_handle() else {
            return;
        };
        let Some(buffer) = self
            .tjs_runtime
            .object_member(bgm, "currentBuffer")
            .object_handle()
        else {
            return;
        };
        let Some(volume2) = self
            .tjs_runtime
            .host()
            .native_audio_buffer(buffer)
            .map(|buffer| buffer.volume2)
        else {
            return;
        };

        let bgm_flags = ensure_object_member(&mut self.tjs_runtime, scflags, "bgm");
        if object_member_is_false(&self.tjs_runtime, bgm_flags, "enable") {
            return;
        }
        self.tjs_runtime
            .set_object_member(bgm_flags, "globalVolume", Variant::Integer(volume2));
    }

    fn sync_kag_se_system_state(&mut self, window: ObjectHandle, scflags: ObjectHandle) {
        let Some(se) = self.tjs_runtime.object_member(window, "se").object_handle() else {
            return;
        };
        let count = self
            .tjs_runtime
            .object_member(se, "count")
            .to_integer()
            .unwrap_or(0)
            .max(0) as usize;
        if count == 0 {
            return;
        }

        let mut volumes = vec![None; count];
        let mut last_live_index = None;
        for (index, volume_slot) in volumes.iter_mut().enumerate() {
            let Some(buffer) = self
                .tjs_runtime
                .object_member(se, &index.to_string())
                .object_handle()
            else {
                continue;
            };
            let Some(volume2) = self
                .tjs_runtime
                .host()
                .native_audio_buffer(buffer)
                .map(|buffer| buffer.volume2)
            else {
                continue;
            };
            *volume_slot = Some(volume2);
            last_live_index = Some(index);
        }

        let Some(last_live_index) = last_live_index else {
            return;
        };
        let se_flags = self.tjs_runtime.alloc_array_object(Vec::new());
        for volume in volumes.into_iter().take(last_live_index + 1) {
            let Some(volume) = volume else {
                self.tjs_runtime.array_push(se_flags, Variant::Void);
                continue;
            };
            let item_flags = self.alloc_dictionary_object();
            self.tjs_runtime.set_object_member(
                item_flags,
                "globalVolume",
                Variant::Integer(volume),
            );
            self.tjs_runtime
                .array_push(se_flags, Variant::Object(item_flags));
        }
        self.tjs_runtime
            .set_object_member(scflags, "se", Variant::Object(se_flags));
    }

    fn alloc_dictionary_object(&mut self) -> ObjectHandle {
        let handle = self.tjs_runtime.alloc_ordinary_object();
        self.tjs_runtime.add_object_class_info(handle, "Dictionary");
        handle
    }

    fn sync_kag_sflags_audio_state(&mut self, window: ObjectHandle) {
        let Some(sflags) = self
            .tjs_runtime
            .object_member(window, "sflags")
            .object_handle()
        else {
            return;
        };

        if let Some(volume) = self.kag_bgm_volume2(window) {
            self.set_existing_sflag_volume_percent(sflags, "bgm_vol", volume);
        }
        if let Some(volume) = self.kag_se_volume2(window, 0) {
            self.set_existing_sflag_volume_percent(sflags, "se_vol", volume);
        }
        if let Some(volume) = self.kag_se_volume2(window, 2) {
            self.set_existing_sflag_volume_percent(sflags, "sysse_vol", volume);
        }
        if let Some(volume) = self.kag_se_volume2(window, 3) {
            self.set_existing_sflag_volume_percent(sflags, "cv_vol", volume);
        }
    }

    fn kag_bgm_volume2(&self, window: ObjectHandle) -> Option<i64> {
        let bgm = self
            .tjs_runtime
            .object_member(window, "bgm")
            .object_handle()?;
        let buffer = self
            .tjs_runtime
            .object_member(bgm, "currentBuffer")
            .object_handle()?;
        self.tjs_runtime
            .host()
            .native_audio_buffer(buffer)
            .map(|buffer| buffer.volume2)
    }

    fn kag_se_volume2(&self, window: ObjectHandle, index: usize) -> Option<i64> {
        let se = self
            .tjs_runtime
            .object_member(window, "se")
            .object_handle()?;
        let buffer = self
            .tjs_runtime
            .object_member(se, &index.to_string())
            .object_handle()?;
        self.tjs_runtime
            .host()
            .native_audio_buffer(buffer)
            .map(|buffer| buffer.volume2)
    }

    fn set_existing_sflag_volume_percent(
        &mut self,
        sflags: ObjectHandle,
        name: &str,
        volume2: i64,
    ) {
        if !self.tjs_runtime.has_object_member(sflags, name) {
            return;
        }
        self.tjs_runtime.set_object_member(
            sflags,
            name,
            Variant::Integer(volume2_to_kag_percent(volume2)),
        );
    }

    pub fn request_runtime_close(&mut self) -> Result<()> {
        let Some(window) = self.runtime_window_object() else {
            self.tjs_runtime.host_mut().request_termination();
            return Ok(());
        };
        if matches!(
            self.tjs_runtime.object_member(window, "close"),
            Variant::Void
        ) {
            self.tjs_runtime.host_mut().request_termination();
            return Ok(());
        }
        self.tjs_runtime
            .call_object_method(window, "close", Vec::new())
            .map(|_| ())
    }

    pub fn window_fullscreen(&self) -> bool {
        let Some(window) = self.runtime_window_object() else {
            return false;
        };
        self.tjs_runtime
            .object_member(window, "fullScreen")
            .is_truthy()
    }

    pub fn active_kag_parser_handle(&self) -> Option<ObjectHandle> {
        self.kag_session.active_parser()
    }

    pub fn kag_parser(&self) -> &KagParser {
        let handle = self
            .kag_session
            .active_parser()
            .expect("active KAG parser is not initialized");
        self.tjs_runtime
            .host()
            .kag_parser(handle)
            .expect("active KAG parser state is not registered")
    }

    pub fn execute_script(&mut self, source_name: &str, source: &str) -> Result<Variant> {
        let result = execute_script_on_runtime(&mut self.tjs_runtime, source_name, source);
        self.sync_kag_slots_after_ok(result)
    }

    pub fn execute_expression(&mut self, source_name: &str, source: &str) -> Result<Variant> {
        let result = execute_expression_on_runtime(&mut self.tjs_runtime, source_name, source);
        self.sync_kag_slots_after_ok(result)
    }

    pub fn execute_storage(&mut self, name: &str) -> Result<Variant> {
        let bytes = self.tjs_runtime.host_mut().read_binary_storage(name)?;
        if let Some(result) =
            execute_bytecode_if_present_on_runtime(&mut self.tjs_runtime, name, &bytes)?
        {
            return self.sync_kag_slots_after_ok(Ok(result));
        }
        let source = self.tjs_runtime.host_mut().read_text_storage(name)?;
        let result = execute_script_on_runtime(&mut self.tjs_runtime, name, &source);
        self.sync_kag_slots_after_ok(result)
    }

    pub fn eval_storage(&mut self, name: &str) -> Result<Variant> {
        let bytes = self.tjs_runtime.host_mut().read_binary_storage(name)?;
        if let Some(result) =
            execute_bytecode_if_present_on_runtime(&mut self.tjs_runtime, name, &bytes)?
        {
            return self.sync_kag_slots_after_ok(Ok(result));
        }
        let source = self.tjs_runtime.host_mut().read_text_storage(name)?;
        let result = execute_expression_on_runtime(&mut self.tjs_runtime, name, &source);
        self.sync_kag_slots_after_ok(result)
    }

    fn sync_kag_slots_after_ok<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_ok() {
            register_kag_layer_slots_from_tjs(&mut self.tjs_runtime);
        }
        result
    }

    pub fn execute_startup(&mut self) -> Result<Variant> {
        self.execute_storage("startup.tjs")
    }

    /// Starts a project using the same contract on every host.
    ///
    /// A KRKR project owns its opening flow: `startup.tjs` may construct a
    /// KAG window/parser and dispatch first-run/logo/title scripts.  The
    /// engine only provides the conventional `startup.ks` fallback when no
    /// parser was created.  Hosts must call this method instead of guessing
    /// scenario names such as `first.ks` or `title.ks`.
    /// Runs a loose `patch.tjs` sitting beside the game data before the
    /// startup script.  kirikiri2/z have no such hook, but kirikiroid2 boots
    /// one and repacks rely on it to hook `Scripts.execStorage` and register
    /// their own archives.  Only a filesystem file qualifies: a `patch.tjs`
    /// shipped inside an archive is ordinary game data.
    fn execute_boot_patch(&mut self) {
        if self.tjs_runtime.host().placed_path("patch.tjs").is_none() {
            return;
        }
        self.tjs_runtime
            .host_mut()
            .log("project startup: executing patch.tjs boot patch");
        if let Err(error) = self.execute_storage("patch.tjs") {
            self.tjs_runtime
                .host_mut()
                .log(&format!("patch.tjs failed: {}", error.message));
        }
    }

    pub fn start_project(&mut self) -> Result<()> {
        self.execute_boot_patch();
        let has_startup_tjs = self.tjs_runtime.host().storage_exists("startup.tjs");
        let has_startup_ks = self.tjs_runtime.host().storage_exists("startup.ks");
        let mut startup_exception_handled = false;
        if has_startup_tjs || !has_startup_ks {
            self.tjs_runtime
                .host_mut()
                .log("project startup: executing startup.tjs dispatcher");
            if let Err(error) = self.execute_startup() {
                if is_resource_pending_error(&error) || error.is_debug_quit() {
                    return Err(error);
                }
                if self.tjs_runtime.process_unhandled_exception(&error)? {
                    startup_exception_handled = true;
                    self.tjs_runtime
                        .host_mut()
                        .log(&format!("handled startup exception: {}", error.message));
                } else {
                    return Err(error);
                }
            }
        }
        if !startup_exception_handled && !self.has_kag_scenario() && has_startup_ks {
            self.tjs_runtime
                .host_mut()
                .log("project startup: falling back to startup.ks scenario");
            self.load_kag_scenario("startup.ks").map_err(kag_to_tjs)?;
        }
        Ok(())
    }

    pub fn load_kag_scenario(&mut self, storage: &str) -> krkr_kag::Result<()> {
        self.kag_session
            .load_scenario(storage, &mut self.tjs_runtime)
    }

    pub fn next_kag_tag(&mut self) -> krkr_kag::Result<Option<Tag>> {
        self.kag_session.next_tag(&mut self.tjs_runtime)
    }

    pub fn kag_state(&self) -> &KagTaskState {
        self.kag_session.state()
    }

    /// Marks the scenario as blocked on an asynchronous resource.  Platform
    /// sessions use this as a final safety net when a nested TJS callback
    /// still returns a wrapped pending error at the frame boundary; the next
    /// frame will collect the outstanding external request and resume once it
    /// is supplied.
    pub fn mark_resource_waiting(&mut self) {
        self.kag_session.state = KagTaskState::WaitingResource;
    }

    pub fn has_kag_scenario(&self) -> bool {
        self.kag_session.loaded()
    }

    pub fn is_script_suspended(&self) -> bool {
        self.tjs_runtime.is_suspended()
    }

    /// True while the TJS VM is parked on a resource request that has not
    /// landed yet.
    ///
    /// This is the wait a frame boundary has to respect even when the engine's
    /// own KAG session never started one: a game that drives KAG from its own
    /// TJS conductor keeps the session `Finished` while a `Layer.loadImages`
    /// call inside a logo/opening callback is blocked on the decode worker.
    fn parked_on_pending_resource(&self) -> bool {
        self.tjs_runtime.is_suspended()
            && (self.tjs_runtime.host().has_pending_resource_loads()
                || self.tjs_runtime.host().has_pending_external_resources())
    }

    /// True when the session state is one the parked-VM marker must not
    /// overwrite.
    ///
    /// Scenario waits (`WaitingClick`/`WaitingTimer`/`WaitingAudio`/
    /// `WaitingTransition`/`WaitingModal`) and errors live only in that field,
    /// and the path that clears each of them is the only place that knows when
    /// it ends: `signal_click` clears a click wait, the `WaitingTimer` branch
    /// of `update_wait` decrements its clock, a transition ends its own wait.
    /// Replacing one with `WaitingResource` would let `update_wait` switch the
    /// session to `Running` when the parked work lands, silently dropping the
    /// wait. A recorded `WaitingResource` is held too: it already is the mark
    /// the KAG tag loop set, so there is nothing to save and restore.
    fn session_state_is_held(&self) -> bool {
        matches!(
            self.kag_session.state,
            KagTaskState::WaitingClick
                | KagTaskState::WaitingTimer { .. }
                | KagTaskState::WaitingAudio
                | KagTaskState::WaitingTransition
                | KagTaskState::WaitingModal
                | KagTaskState::WaitingResource
                | KagTaskState::Error { .. }
        )
    }

    /// Returns scheduler queues useful to platform diagnostics: continuous
    /// handlers, queued script events, and idle async triggers.
    pub fn scheduler_diagnostics(&self) -> (usize, usize, usize, usize, usize) {
        self.tjs_runtime.host().scheduler_diagnostics()
    }

    pub fn scheduler_timer_diagnostics(&mut self) -> (usize, usize, usize, usize, i64) {
        let now = self.tjs_runtime.host_mut().now_millis();
        let handles = self.tjs_runtime.host().scheduler().timer_handles();
        let mut enabled = 0;
        let mut scheduled = 0;
        let mut due = 0;
        for handle in handles.iter().copied() {
            if !self
                .tjs_runtime
                .object_member(handle, "enabled")
                .is_truthy()
            {
                continue;
            }
            enabled += 1;
            let interval = self
                .tjs_runtime
                .object_member(handle, "interval")
                .to_integer()
                .unwrap_or_default();
            if interval == 0 {
                continue;
            }
            scheduled += 1;
            if self
                .tjs_runtime
                .host()
                .scheduler()
                .timer_next_fire_millis(handle)
                .is_some_and(|next| next <= now)
            {
                due += 1;
            }
        }
        (handles.len(), enabled, scheduled, due, now)
    }

    pub fn message_layer(&self) -> &MessageLayerModel {
        self.kag_session.message_layer()
    }

    pub fn kag_location(&self) -> KagLocation {
        self.kag_session.location(&self.tjs_runtime)
    }

    pub fn set_kag_handler(&mut self, handler: ObjectHandle) {
        self.kag_session.set_handler(handler);
    }

    pub fn clear_kag_handler(&mut self) {
        self.kag_session.clear_handler();
    }

    pub fn signal_kag_click(&mut self) {
        self.kag_session.signal_click();
    }

    pub fn set_external_resource_catalog<I, S>(&mut self, paths: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tjs_runtime
            .host_mut()
            .set_external_resource_catalog(paths);
    }

    pub fn take_external_resource_requests(&mut self) -> Vec<(String, krkr_core::AssetKind)> {
        self.tjs_runtime
            .host_mut()
            .take_external_resource_requests()
    }

    pub fn has_external_resource_request(&self, path: &str) -> bool {
        self.tjs_runtime.host().has_external_resource_request(path)
    }

    pub fn provide_external_resource(&mut self, path: &str, bytes: Vec<u8>) -> Result<()> {
        self.tjs_runtime.host_mut().log(&format!(
            "external resource provided `{path}` ({} bytes)",
            bytes.len()
        ));
        self.tjs_runtime
            .host_mut()
            .provide_external_resource(path, bytes)?;
        if self.tjs_runtime.is_suspended() {
            // Resuming a suspended KAG/TJS call can immediately request the
            // next resource (for example custom.ks -> title.ks).  That is a
            // normal asynchronous boundary, not a fatal engine error: leave
            // the VM suspended and let the next asset event resume it.
            match self.tjs_runtime.resume_suspended() {
                Ok(_) => {}
                Err(error) if is_resource_pending_error(&error) => {
                    self.tjs_runtime.host_mut().log(&format!(
                        "external resource resume deferred for `{path}`: {error}"
                    ));
                    self.kag_session.state = KagTaskState::WaitingResource;
                    return Ok(());
                }
                Err(error) => {
                    self.tjs_runtime.host_mut().log(&format!(
                        "external resource resume failed for `{path}`: {error}"
                    ));
                    return Err(error);
                }
            }
        }
        if self.kag_session.state == KagTaskState::WaitingResource {
            self.kag_session.state = KagTaskState::Running;
        }
        Ok(())
    }

    pub fn notify_audio_stopped(&mut self, id: AudioInstanceId) -> Result<()> {
        let handle = self
            .tjs_runtime
            .host_mut()
            .mark_native_audio_instance_stopped(id);
        self.kag_session.signal_audio_finished();

        let Some(handle) = handle else {
            return Ok(());
        };
        if !self.tjs_runtime.object_valid(handle) {
            return Ok(());
        }

        let status_changed = set_wave_status(&mut self.tjs_runtime, handle, "stop");
        let paused_changed = set_wave_paused(&mut self.tjs_runtime, handle, false);
        if !status_changed && !paused_changed {
            return self.sync_kag_slots_after_ok(Ok(()));
        }
        match call_wave_status_changed(&mut self.tjs_runtime, handle) {
            Ok(()) => self.sync_kag_slots_after_ok(Ok(())),
            Err(error) => {
                self.handle_callback_error("audio status callback", error)?;
                self.sync_kag_slots_after_ok(Ok(()))
            }
        }
    }

    /// Completes a browser-owned media element and mirrors its `ended` event
    /// into the native overlay conductor. Desktop decoders reach the same
    /// path from `tick_video_overlays`; Web media elements call this hook.
    pub fn notify_video_ended(&mut self, id: u64) -> Result<()> {
        let handles = self.tjs_runtime.host().video_overlay_handles();
        for handle in handles {
            if handle.0 as u64 == id && self.tjs_runtime.object_valid(handle) {
                if let Err(error) = self
                    .tjs_runtime
                    .call_object_method(handle, "stop", Vec::new())
                {
                    self.handle_callback_error("video stop callback", error)?;
                }
            }
        }
        Ok(())
    }

    pub fn tick(&mut self) -> Result<EngineTickResult> {
        self.advance(Duration::ZERO)
    }

    /// Returns a human-readable hit-test trace for browser/host diagnostics.
    ///
    /// KRKR controls often layer a script-backed button below transparent
    /// image children. Keeping this inspection on the engine makes it
    /// possible for a browser driver to explain why a click was routed to a
    /// decorative layer instead of guessing from pixels alone.
    pub fn inspect_pointer(&mut self, position: Point) -> Result<Vec<String>> {
        let candidates = self.hit_tested_layers_at_position(position)?;
        Ok(candidates
            .into_iter()
            .filter_map(|layer_id| {
                let node = self.tjs_runtime.host().layer_tree().layer(layer_id)?.clone();
                let object = self.tjs_runtime.host().native_object_for_layer(layer_id)?;
                let classes = self.tjs_runtime.object_class_infos(object).join("/");
                let class_name = match self.tjs_runtime.object_member(object, "__className") {
                    Variant::String(value) => value,
                    _ => String::new(),
                };
                let handler_state = |method: &str| match self.tjs_runtime.object_member(object, method)
                {
                    Variant::Void => "none",
                    handler if self.tjs_runtime.variant_is_native_function(&handler) => "native",
                    _ => "script",
                };
                let link_num = self.tjs_runtime.object_member(object, "linkNum");
                let link = !matches!(link_num, Variant::Void);
                let event_transparent = self
                    .tjs_runtime
                    .object_member(object, "eventTransparent")
                    .is_truthy();
                Some(format!(
                    "id={} name={:?} class={}/{} rect=({},{} {}x{}) z={} hitType={} threshold={} transparent={} handlers=down:{} up:{} hit:{} click:{} link={} linkNum={:?}",
                    layer_id,
                    node.name,
                    classes,
                    class_name,
                    node.left,
                    node.top,
                    node.width,
                    node.height,
                    node.z_order,
                    node.hit_type,
                    node.hit_threshold,
                    event_transparent,
                    handler_state("onMouseDown"),
                    handler_state("onMouseUp"),
                    handler_state("onHitTest"),
                    handler_state("onClick"),
                    link,
                    link_num,
                ))
            })
            .collect())
    }

    pub fn update(&mut self, input: EngineInput, delta: Duration) -> Result<EngineFrame> {
        self.input_result = EngineInputResult::default();
        if let Some(now_millis) = input.now_millis {
            self.tjs_runtime.host_mut().set_clock_millis(now_millis);
        } else {
            #[cfg(target_arch = "wasm32")]
            self.tjs_runtime.host_mut().advance_clock(delta);
        }
        self.sync_scheduler_event_disabled();
        {
            let scheduler = self.tjs_runtime.host_mut().scheduler_mut();
            scheduler.begin_frame();
            for event in input.events.iter().copied() {
                scheduler.post_input_event(event);
            }
        }
        for text in input.text {
            self.tjs_runtime.host_mut().push_text_input(text);
        }
        // Do not dispatch script/idle callbacks while a previous callback is
        // blocked on an external resource. Such a callback can immediately
        // retry the same nested KAG jump and recreate the pending error before
        // the platform session has had a chance to start the fetch. Input is
        // retained in the scheduler and delivered after the resource arrives.
        //
        // The suspended VM is the wait: games that drive KAG from their own
        // TJS conductor (`parser.getNextTag()` in script) never move the
        // engine session out of `Finished`, so a callback parked on a script
        // image decode (`Layer.loadImages`) would otherwise keep the engine
        // looking runnable — callbacks would be dispatched into the parked VM
        // and the parked call would only resume on the frame that happened to
        // poll the decode completion. Marking the session here keeps the
        // pending load visible to every frame boundary until it lands.
        //
        // The mark is a mask, not a new wait: it may not replace a state the
        // session itself is holding (see `session_state_is_held`), and the
        // state it did replace comes back once the parked work lands.
        if self.parked_on_pending_resource() {
            if !self.session_state_is_held() {
                if self.parked_resource_state.is_none() {
                    self.parked_resource_state = Some(self.kag_session.state.clone());
                }
                self.kag_session.state = KagTaskState::WaitingResource;
            }
        } else if let Some(previous) = self.parked_resource_state.take() {
            // The parked work landed and `update_wait` has cleared the marked
            // wait to `Running`; give the session back what it held before the
            // park, so a parser-less game reports `Finished` again. Any other
            // state means the session moved on (or another load is still
            // pending) and owns the field again.
            if self.kag_session.state == KagTaskState::Running {
                self.kag_session.state = previous;
            }
        }
        let waiting_for_resource = self.kag_session.state == KagTaskState::WaitingResource
            && (self.tjs_runtime.host().has_pending_external_resources()
                || self.tjs_runtime.host().has_pending_resource_loads());
        if !waiting_for_resource
            && let Err(error) = self.pump_runtime_scheduler(RuntimeSchedulerPump::Full)
            && !(error.to_string().contains("KAG resource is pending:") && {
                self.kag_session.state = KagTaskState::WaitingResource;
                true
            })
        {
            return Err(error);
        }
        if !waiting_for_resource
            && let Err(error) = self.resume_modal_call_if_ready()
            && !(error.to_string().contains("KAG resource is pending:") && {
                self.kag_session.state = KagTaskState::WaitingResource;
                true
            })
        {
            return Err(error);
        }
        let tick = self.advance(delta)?;
        // Video overlays advance on the same clock; status events fired here
        // (notably the end-of-stream `stop` KAG movie conductors wait for)
        // wake the scenario on the next advance.
        let video_tick = tick_video_overlays(&mut self.tjs_runtime);
        match video_tick {
            Ok(()) => self.sync_kag_slots_after_ok(Ok(()))?,
            Err(error) => self.handle_callback_error("video status callback", error)?,
        }
        let resource_waiting_after_tick = self.kag_session.state == KagTaskState::WaitingResource
            && (self.tjs_runtime.host().has_pending_external_resources()
                || self.tjs_runtime.host().has_pending_resource_loads());
        if !waiting_for_resource && !resource_waiting_after_tick {
            self.pump_runtime_scheduler(RuntimeSchedulerPump::WindowUpdatesOnly)?;
            if let Err(error) = complete_pending_layer_paints(&mut self.tjs_runtime) {
                if is_resource_pending_error(&error) || error.is_debug_quit() {
                    return Err(error);
                }
                if self.tjs_runtime.process_unhandled_exception(&error)? {
                    self.tjs_runtime
                        .host_mut()
                        .log(&format!("handled layer onPaint error: {}", error.message));
                } else {
                    return Err(error);
                }
            }
        }
        let suppressed_images = self.tjs_runtime.host().suppressed_transition_live_images();
        let transition = self.tjs_runtime.host().frame_transitions();
        let mut output = self
            .core_engine
            .tick_running_with_layers_suppressing_images_and_transitions(
                input.frame,
                self.tjs_runtime.host().layer_tree(),
                self.kag_session.message_layer(),
                &suppressed_images,
                transition,
            );
        // Movie quads present on top of the layer tree (krkrz draws the
        // overlay/mixer video above the normal window contents).
        let (mut video_uploads, video_commands) =
            video_overlay_frame_quads(self.tjs_runtime.host_mut());
        output.image_uploads.append(&mut video_uploads);
        output.draw_commands.extend(video_commands);
        let frame = EngineFrame {
            output,
            tick,
            input: self.input_result,
            message_layer: self.kag_session.message_layer().clone(),
            location: self.kag_location(),
        };
        Ok(frame)
    }

    /// Advances one host frame and transfers external resource requests along
    /// with the frame result. Runtime hosts should prefer this method over
    /// calling `update` and then inspecting host state separately.
    pub fn step(&mut self, input: EngineInput, delta: Duration) -> Result<EngineStep> {
        let frame = self.update(input, delta)?;
        let requests = self
            .take_external_resource_requests()
            .into_iter()
            .map(|(path, kind)| ExternalResourceRequest { path, kind })
            .collect::<Vec<_>>();
        let waiting =
            !requests.is_empty() || matches!(frame.tick.state, KagTaskState::WaitingResource);
        Ok(if waiting {
            EngineStep::Waiting { frame, requests }
        } else {
            EngineStep::Ready { frame, requests }
        })
    }

    fn resume_modal_call_if_ready(&mut self) -> Result<()> {
        while let Some(window) = self.tjs_runtime.host().current_modal_window() {
            if !self.modal_window_is_closed(window) {
                break;
            }

            self.tjs_runtime.host_mut().pop_modal_window(window);
            self.tjs_runtime.resume_suspended()?;
            if self.kag_session.state == KagTaskState::WaitingModal
                && !self.tjs_runtime.is_suspended()
            {
                self.kag_session.state = KagTaskState::Running;
            }
        }
        // A host may invalidate a modal window while resuming a suspended
        // callback (for example when a browser-side window has no native DOM
        // peer). In that case the modal stack is already empty, but the KAG
        // state still needs to leave `WaitingModal` once the VM is running.
        if self.kag_session.state == KagTaskState::WaitingModal
            && !self.tjs_runtime.is_suspended()
            && self.tjs_runtime.host().current_modal_window().is_none()
        {
            self.tjs_runtime
                .host_mut()
                .log("KAG modal wait cleared without active modal");
            self.kag_session.state = KagTaskState::Running;
        }
        Ok(())
    }

    fn modal_window_is_closed(&self, window: ObjectHandle) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            // The Web shell has no native modal-window peer yet. Keeping the
            // VM suspended here makes title/startup scripts wait forever on
            // an invisible dialog, so treat browser-side modal helpers as
            // immediately dismissed until a DOM dialog bridge is provided.
            let _ = window;
            return true;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let visible = self
                .tjs_runtime
                .host()
                .native_window_property(window, "visible");
            !self.tjs_runtime.object_valid(window)
            || self.tjs_runtime.host().native_window_closed(window)
            // Browser hosts do not create a native DOM peer for every KRKR
            // modal helper. If no host-side window state exists, there is no
            // modal surface to keep the VM blocked on, so resume it.
            || (visible.is_none()
                && !self.tjs_runtime.host().modal_window_snapshot().is_some())
            || !visible
                .unwrap_or_else(|| self.tjs_runtime.object_member(window, "visible"))
                .is_truthy()
        }
    }

    fn advance(&mut self, delta: Duration) -> Result<EngineTickResult> {
        if let Err(error) = apply_completed_resource_loads(&mut self.tjs_runtime) {
            if is_resource_pending_error(&error) {
                self.kag_session.state = KagTaskState::WaitingResource;
                return Ok(EngineTickResult {
                    state: self.kag_session.state.clone(),
                    reason: KagYieldReason::Waiting(self.kag_session.state.clone()),
                    tags_processed: 0,
                    elapsed: Duration::ZERO,
                });
            }
            return Err(error);
        }
        if let Err(error) = self.refresh_transition_ticks() {
            self.handle_callback_error("transition tick callback", error)?;
        }
        self.tjs_runtime.host_mut().advance_transition(delta);
        if let Err(error) = finish_completed_native_transitions(&mut self.tjs_runtime) {
            self.handle_callback_error("transition completion callback", error)?;
        }
        let transition_active = self.tjs_runtime.host().has_active_transition();
        // A parked VM keeps the resource wait alive even for requests the
        // session's own load bookkeeping does not track (an external resource
        // supplied by the host asset scheduler).
        let resource_pending = self.tjs_runtime.host().has_pending_resource_loads()
            || self.parked_on_pending_resource();
        self.kag_session
            .update_wait(delta, transition_active, resource_pending);
        match self
            .kag_session
            .run_until_yield(&mut self.tjs_runtime, self.kag_budget)
        {
            Ok(tick) => Ok(tick),
            Err(error)
                if error.kind == krkr_tjs2::TjsErrorKind::ResourcePending
                    || error.to_string().contains("KAG resource is pending:") =>
            {
                // Nested parser jumps can wrap ResourcePending in a runtime
                // stack trace. Keep the session resumable at the frame
                // boundary so the asset store can fetch and wake it.
                self.kag_session.state = KagTaskState::WaitingResource;
                Ok(EngineTickResult {
                    state: self.kag_session.state.clone(),
                    reason: KagYieldReason::Waiting(self.kag_session.state.clone()),
                    tags_processed: 0,
                    elapsed: Duration::ZERO,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Asks every running `callback`-driven transition for its tick
    /// (`tTJSNI_BaseLayer::GetTransTick`, `LayerIntf.cpp:6683`).
    fn refresh_transition_ticks(&mut self) -> Result<()> {
        let callbacks = self.tjs_runtime.host().transition_tick_callbacks();
        for (dest, callback) in callbacks {
            let tick = self
                .tjs_runtime
                .call_function(callback, Vec::new())?
                .to_integer()?;
            self.tjs_runtime
                .host_mut()
                .set_transition_tick(dest, tick);
        }
        Ok(())
    }

    fn runtime_window_object(&self) -> Option<ObjectHandle> {
        if let Some(window) = self.active_modal_window() {
            return Some(window);
        }

        if let Some(kag) = self.tjs_runtime.global_member("kag").object_handle()
            && has_window_state_member(&self.tjs_runtime, kag)
        {
            return Some(kag);
        }

        // `Window.mainWindow` is the first constructed window
        // (`TVPMainWindow`, `WindowIntf.cpp:1796`); the host records it when
        // the window registers, which is also what the property getters read.
        self.tjs_runtime.host().main_window()
    }

    fn active_modal_window(&self) -> Option<ObjectHandle> {
        let window = self.tjs_runtime.host().current_modal_window()?;
        (!self.modal_window_is_closed(window)).then_some(window)
    }

    fn pump_runtime_scheduler(&mut self, mode: RuntimeSchedulerPump) -> Result<()> {
        const MAX_NATIVE_EVENT_PASSES: usize = 1024;

        if matches!(mode, RuntimeSchedulerPump::Full) {
            self.sync_scheduler_event_disabled();
            // KRKR snapshots the event sequence once per external delivery
            // turn. The first delivery pass starts it after due timers/fades
            // have been posted; later native passes retain the same snapshot.
            self.scheduler_turn_started = false;
        }

        if self.tjs_runtime.is_suspended() {
            if matches!(mode, RuntimeSchedulerPump::Full) {
                if self.tjs_runtime.host().scheduler().event_disabled() {
                    return Ok(());
                }
                while let Some(event) = self
                    .tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .pop_input_event()
                {
                    self.handle_input_event(event)?;
                    if self
                        .tjs_runtime
                        .host()
                        .current_modal_window()
                        .is_some_and(|window| self.modal_window_is_closed(window))
                    {
                        break;
                    }
                }
            }
            return Ok(());
        }

        for _ in 0..MAX_NATIVE_EVENT_PASSES {
            let mut delivered_script_or_input = false;
            if matches!(mode, RuntimeSchedulerPump::Full) {
                delivered_script_or_input = self.deliver_script_and_input_turn()?;
                if delivered_script_or_input && self.tjs_runtime.is_suspended() {
                    return Ok(());
                }
            }

            if matches!(mode, RuntimeSchedulerPump::Full)
                && !self.tjs_runtime.host().scheduler().event_disabled()
                && self.deliver_idle_scheduler_event()?
            {
                if self.tjs_runtime.is_suspended() {
                    return Ok(());
                }
                continue;
            }

            let delivered_window_update = self.deliver_window_update_events()?;
            if delivered_window_update && self.tjs_runtime.is_suspended() {
                return Ok(());
            }

            if delivered_script_or_input || delivered_window_update {
                continue;
            }

            return Ok(());
        }

        self.tjs_runtime
            .host_mut()
            .log("runtime scheduler reached its per-frame pass budget; remaining events deferred");
        Ok(())
    }

    fn deliver_script_and_input_turn(&mut self) -> Result<bool> {
        self.post_due_scheduler_events()?;
        if self.tjs_runtime.host().scheduler().event_disabled() {
            return Ok(false);
        }
        if !self.scheduler_turn_started {
            self.tjs_runtime
                .host_mut()
                .scheduler_mut()
                .begin_script_delivery_turn();
            self.scheduler_turn_started = true;
        }

        let mut delivered = false;
        let mut delivered_exclusive = false;
        while let Some(event) = self
            .tjs_runtime
            .host_mut()
            .scheduler_mut()
            .pop_script_event(ScriptEventSelection::Exclusive)
        {
            delivered = true;
            delivered_exclusive = true;
            self.fire_script_event(event)?;
            if self.tjs_runtime.is_suspended() {
                return Ok(true);
            }
        }

        if delivered_exclusive
            || self
                .tjs_runtime
                .host()
                .scheduler()
                .has_exclusive_script_event()
        {
            return Ok(delivered);
        }

        while !self.tjs_runtime.host().scheduler().event_disabled() {
            let Some(event) = self
                .tjs_runtime
                .host_mut()
                .scheduler_mut()
                .pop_input_event()
            else {
                break;
            };
            delivered = true;
            self.handle_input_event(event)?;
            if self.tjs_runtime.is_suspended()
                || self
                    .tjs_runtime
                    .host()
                    .scheduler()
                    .has_exclusive_script_event()
            {
                return Ok(true);
            }
        }

        while let Some(event) = self
            .tjs_runtime
            .host_mut()
            .scheduler_mut()
            .pop_script_event(ScriptEventSelection::Any)
        {
            delivered = true;
            self.fire_script_event(event)?;
            if self.tjs_runtime.is_suspended() {
                return Ok(true);
            }
        }

        Ok(delivered)
    }

    fn sync_scheduler_event_disabled(&mut self) {
        let disabled = match self.tjs_runtime.global_member("System").object_handle() {
            Some(system) => self
                .tjs_runtime
                .resolve_object_member(system, "eventDisabled")
                .map(|value| value.is_truthy())
                .unwrap_or(false),
            None => false,
        };
        self.tjs_runtime
            .host_mut()
            .scheduler_mut()
            .set_event_disabled(disabled);
    }

    fn post_due_scheduler_events(&mut self) -> Result<()> {
        let now = self.tjs_runtime.host_mut().now_millis();
        self.tjs_runtime
            .host_mut()
            .scheduler_mut()
            .post_due_audio_fade_completions(now);

        let timer_handles = self.tjs_runtime.host().scheduler().timer_handles();
        for handle in timer_handles {
            let enabled = self
                .tjs_runtime
                .object_member(handle, "enabled")
                .is_truthy();
            if !enabled {
                self.tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .cancel_timer_events(handle);
                self.tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .set_timer_next_fire_millis(handle, None);
                continue;
            }

            let interval = self
                .tjs_runtime
                .object_member(handle, "interval")
                .to_real()?
                .round()
                .max(0.0) as i64;
            // KRKR2/KRKRZ skip enabled Timers whose interval is zero.  A
            // zero interval is therefore a disabled schedule, rather than an
            // idle continuation; KAG continuations use the event queue.
            if interval == 0 {
                self.tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .cancel_timer_events(handle);
                self.tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .set_timer_next_fire_millis(handle, None);
                continue;
            }

            let interval_changed = self
                .tjs_runtime
                .host_mut()
                .scheduler_mut()
                .set_timer_interval(handle, interval);
            if interval_changed {
                self.tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .cancel_timer_events(handle);
                self.tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .set_timer_next_fire_millis(handle, Some(now.saturating_add(interval)));
                continue;
            }

            let next_fire = match self
                .tjs_runtime
                .host()
                .scheduler()
                .timer_next_fire_millis(handle)
            {
                Some(next_fire) => next_fire,
                None => {
                    let next_fire = now.saturating_add(interval);
                    self.tjs_runtime
                        .host_mut()
                        .scheduler_mut()
                        .set_timer_next_fire_millis(handle, Some(next_fire));
                    next_fire
                }
            };

            if now < next_fire {
                continue;
            }

            let capacity = self
                .tjs_runtime
                .object_member(handle, "capacity")
                .to_integer()
                .unwrap_or(6);
            let capacity = if capacity == 0 {
                usize::MAX
            } else if capacity < 0 {
                // A negative Capacity cannot queue timer events in KRKR;
                // treating it as one would spuriously fire callbacks.
                0
            } else {
                capacity as usize
            };
            let queued_script = self.tjs_runtime.host().scheduler().count_script_events(
                handle,
                handle,
                TIMER_EVENT_NAME,
                0,
            );
            let queued_idle = self
                .tjs_runtime
                .host()
                .scheduler()
                .count_idle_timer_events(handle);
            let mut queued = queued_script.saturating_add(queued_idle);
            // Preserve KRKR's cadence when a frame/update arrives late.  The
            // native timer drops excessive catch-up work after forty periods
            // and restarts from the current clock; shorter stalls retain the
            // original cadence.
            let elapsed_periods = ((now.saturating_sub(next_fire)) / interval).saturating_add(1);
            let due_periods = if elapsed_periods > 40 {
                1
            } else {
                elapsed_periods as usize
            };
            let timer_mode = self
                .tjs_runtime
                .object_member(handle, "mode")
                .to_integer()
                .unwrap_or(0);
            let scheduler = self.tjs_runtime.host_mut().scheduler_mut();
            let tag = scheduler.next_timer_tag(handle);
            for _ in 0..due_periods {
                if queued >= capacity {
                    break;
                }
                scheduler.post_timer_event(handle, tag, timer_mode);
                queued = queued.saturating_add(1);
            }
            let next_fire = if elapsed_periods > 40 {
                now.saturating_add(interval)
            } else {
                next_fire.saturating_add(interval.saturating_mul(elapsed_periods))
            };
            scheduler.set_timer_next_fire_millis(handle, Some(next_fire));
        }

        Ok(())
    }

    fn deliver_window_update_events(&mut self) -> Result<bool> {
        let started = self
            .tjs_runtime
            .host_mut()
            .scheduler_mut()
            .begin_window_update_delivery();
        if !started {
            return Ok(false);
        }

        while let Some(layer) = self
            .tjs_runtime
            .host_mut()
            .scheduler_mut()
            .pop_window_update_event()
        {
            self.fire_layer_paint_event(layer)?;
            self.tjs_runtime
                .host_mut()
                .scheduler_mut()
                .finish_window_update_event(layer);
            if self.tjs_runtime.is_suspended() {
                self.tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .finish_window_update_delivery();
                return Ok(true);
            }
        }

        self.tjs_runtime
            .host_mut()
            .scheduler_mut()
            .finish_window_update_delivery();
        Ok(true)
    }

    fn fire_layer_paint_event(&mut self, layer: ObjectHandle) -> Result<()> {
        if self.tjs_runtime.object_valid(layer) {
            if let Err(error) = complete_layer_before_draw(&mut self.tjs_runtime, layer) {
                if is_resource_pending_error(&error) || error.is_debug_quit() {
                    return Err(error);
                }
                if self.tjs_runtime.process_unhandled_exception(&error)? {
                    self.tjs_runtime
                        .host_mut()
                        .log(&format!("handled layer onPaint error: {}", error.message));
                } else {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn deliver_idle_scheduler_event(&mut self) -> Result<bool> {
        let Some(event) = self.tjs_runtime.host_mut().scheduler_mut().pop_idle_event() else {
            return Ok(false);
        };

        match event {
            IdleEvent::Timer(handle, tag) => {
                self.tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .post_current_timer_event(handle, tag);
            }
            IdleEvent::AsyncTrigger(handle) => {
                self.tjs_runtime
                    .host_mut()
                    .scheduler_mut()
                    .post_current_async_event(handle);
            }
            IdleEvent::ContinuousHandlers(handlers) => {
                let tick = self.tjs_runtime.host_mut().now_millis();
                for handler in handlers {
                    if matches!(handler, Variant::Void) {
                        continue;
                    }
                    // The native continuous vector is live during a pass:
                    // removing a later handler from an earlier callback
                    // suppresses that callback immediately.
                    if !self
                        .tjs_runtime
                        .host()
                        .scheduler()
                        .has_continuous_handler(&handler)
                    {
                        continue;
                    }
                    if let Err(mut error) = self
                        .tjs_runtime
                        .call_function(handler.clone(), vec![Variant::Integer(tick)])
                    {
                        if is_resource_pending_error(&error) || error.is_debug_quit() {
                            return Err(error);
                        }
                        if self.tjs_runtime.process_unhandled_exception(&error)? {
                            self.tjs_runtime.host_mut().log(&format!(
                                "handled continuous handler error: {}",
                                error.message
                            ));
                            self.tjs_runtime
                                .host_mut()
                                .remove_continuous_handler(&handler);
                        } else {
                            error.message =
                                format!("continuous handler {handler:?} failed: {}", error.message);
                            self.tjs_runtime
                                .host_mut()
                                .remove_continuous_handler(&handler);
                            return Err(error);
                        }
                    }
                    if self.tjs_runtime.is_suspended()
                        || self
                            .tjs_runtime
                            .host()
                            .scheduler()
                            .has_exclusive_script_event()
                    {
                        break;
                    }
                }
            }
        }
        Ok(true)
    }

    fn fire_script_event(&mut self, event: ScriptEvent) -> Result<()> {
        if !self.tjs_runtime.object_valid(event.target) {
            return Ok(());
        }

        if event.kind == ScriptEventKind::Timer
            && !self
                .tjs_runtime
                .object_member(event.target, "enabled")
                .is_truthy()
        {
            return Ok(());
        }

        let method = match event.kind {
            ScriptEventKind::Timer => TIMER_EVENT_NAME,
            ScriptEventKind::AsyncTrigger => ASYNC_TRIGGER_EVENT_NAME,
            ScriptEventKind::AudioFadeCompleted => AUDIO_FADE_COMPLETED_EVENT_NAME,
            ScriptEventKind::Custom => event.name.as_str(),
        };
        if matches!(
            self.tjs_runtime.object_member(event.target, method),
            Variant::Void
        ) {
            return Ok(());
        }
        let result = self
            .tjs_runtime
            .call_object_method(event.target, method, event.args)
            .map(|_| ());
        match result {
            Ok(()) => self.sync_kag_slots_after_ok(Ok(())),
            Err(mut error) => {
                // KRKR2/KRKRZ process errors only after the event callback
                // has unwound.  Preserve the original exception identity
                // while adding the event site to its message; this lets a
                // project's System.exceptionHandler recognize framework
                // exceptions such as ConductorException.
                if is_resource_pending_error(&error) || error.is_debug_quit() {
                    return Err(error);
                }
                match self.tjs_runtime.process_unhandled_exception(&error) {
                    Ok(true) => {
                        self.tjs_runtime.host_mut().log(&format!(
                            "handled {:?} event `{method}` error: {}",
                            event.kind, error.message
                        ));
                        Ok(())
                    }
                    Ok(false) => {
                        error.message = format!(
                            "{:?} event `{method}` on object#{} failed: {}",
                            event.kind, event.target.0, error.message
                        );
                        Err(error)
                    }
                    Err(handler_error) => Err(handler_error),
                }
            }
        }
    }

    fn handle_input_event(&mut self, event: EngineEvent) -> Result<()> {
        self.handle_input_events(&[event])
    }

    fn handle_input_events(&mut self, events: &[EngineEvent]) -> Result<()> {
        for event in events {
            self.update_input_key_state(event);
            match event {
                EngineEvent::CursorMoved { position } => {
                    self.cursor_position = Some(*position);
                    self.tjs_runtime.host_mut().set_cursor_position(*position);
                    self.dispatch_window_cursor_move(*position)?;
                    self.dispatch_layer_cursor_move(*position)?;
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Pressed,
                } => {
                    let modal_active = self.active_modal_window().is_some();
                    self.dispatch_window_pointer_event("onMouseDown", 0)?;
                    // A KRKR control is commonly composed of a script-backed
                    // parent and one or more decorative/image children. The
                    // reference engine hands pointer events to the front-most
                    // hittable layer (`GetMostFrontChildAt`), so a decorative
                    // child is expected to mark itself event-transparent with
                    // `hitThreshold = 256`; whatever the hit test returns here
                    // is the event target and the capture owner.
                    let raw_target = self.layer_at_cursor()?;
                    let handled_by_script = raw_target.is_some_and(|layer_id| {
                        self.layer_has_script_handler(layer_id, "onMouseDown")
                    });
                    self.dispatch_layer_pointer_event("onMouseDown", 0, raw_target)?;
                    self.pressed_layer = raw_target;
                    self.tjs_runtime.host_mut().set_captured_layer(raw_target);
                    if !modal_active
                        && !handled_by_script
                        && self.should_fire_primary_click(raw_target)
                    {
                        self.fire_kag_primary_click(false)?;
                    }
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Released,
                } => {
                    let modal_active = self.active_modal_window().is_some();
                    self.dispatch_window_pointer_event("onMouseUp", 0)?;
                    let release_hit = self.layer_at_cursor()?;
                    // The click handler can invalidate/rebuild its layer
                    // (many title Start handlers do exactly that). Determine
                    // ownership while the original target is still alive;
                    // querying afterwards turns a handled click into an
                    // erroneous KAG click-through.
                    let handled_by_script = [self.pressed_layer, release_hit]
                        .into_iter()
                        .flatten()
                        .any(|layer_id| {
                            ["onMouseDown", "onMouseUp", "onClick"]
                                .iter()
                                .any(|method| self.layer_has_script_handler(layer_id, method))
                        });
                    let captured = self.tjs_runtime.host().captured_layer();
                    self.tjs_runtime.host_mut().log(&format!(
                        "input primary release pressed={:?} captured={:?} release={:?} handled_by_script={handled_by_script}",
                        self.pressed_layer, captured, release_hit
                    ));
                    self.dispatch_layer_pointer_event(
                        "onMouseUp",
                        0,
                        captured.or(release_hit),
                    )?;
                    let click_target = self.pressed_layer.filter(|pressed| {
                        release_hit == Some(*pressed) || self.layer_contains_cursor(*pressed)
                    });
                    if let Some(pressed) = click_target {
                        self.dispatch_window_click()?;
                        self.dispatch_layer_click(pressed)?;
                    }
                    self.pressed_layer = None;
                    self.tjs_runtime.host_mut().set_captured_layer(None);
                    if !modal_active && !handled_by_script {
                        self.signal_kag_click();
                    }
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Secondary,
                    state: ButtonState::Pressed,
                } => {
                    let modal_active = self.active_modal_window().is_some();
                    let handled_by_window = self.dispatch_window_pointer_event("onMouseDown", 1)?;
                    let raw_target = self.layer_at_cursor()?;
                    let handled_by_layer = raw_target.is_some_and(|layer_id| {
                        self.layer_has_script_handler(layer_id, "onMouseDown")
                    });
                    self.dispatch_layer_pointer_event("onMouseDown", 1, raw_target)?;
                    self.tjs_runtime.host_mut().set_captured_layer(raw_target);
                    if !modal_active && !handled_by_window && !handled_by_layer {
                        self.fire_kag_secondary_click()?;
                    }
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Secondary,
                    state: ButtonState::Released,
                } => {
                    self.dispatch_window_pointer_event("onMouseUp", 1)?;
                    let release_hit = self.layer_at_cursor()?;
                    let captured = self.tjs_runtime.host().captured_layer();
                    self.dispatch_layer_pointer_event("onMouseUp", 1, captured.or(release_hit))?;
                    self.tjs_runtime.host_mut().set_captured_layer(None);
                }
                EngineEvent::MouseWheel { delta } => {
                    self.dispatch_window_mouse_wheel(*delta)?;
                    self.dispatch_layer_mouse_wheel(*delta)?;
                }
                EngineEvent::KeyboardInput { key, state, repeat } => {
                    let shift = self.current_shift_state(*repeat);
                    let handled = match state {
                        ButtonState::Pressed => {
                            let handled_by_window =
                                self.dispatch_window_key_event("onKeyDown", *key, shift)?;
                            let handled_by_layer =
                                self.dispatch_focused_layer_key_event("onKeyDown", *key, shift)?;
                            handled_by_window || handled_by_layer
                        }
                        ButtonState::Released => {
                            let handled_by_window =
                                self.dispatch_window_key_event("onKeyUp", *key, shift)?;
                            let handled_by_layer =
                                self.dispatch_focused_layer_key_event("onKeyUp", *key, shift)?;
                            handled_by_window || handled_by_layer
                        }
                    };
                    if matches!(state, ButtonState::Pressed)
                        && matches!(key, EngineKey::Enter | EngineKey::Space)
                        && !handled
                        && self.active_modal_window().is_none()
                    {
                        self.fire_kag_primary_click(true)?;
                        self.signal_kag_click();
                    }
                    if matches!(state, ButtonState::Pressed)
                        && matches!(key, EngineKey::Escape)
                        && !handled
                    {
                        self.input_result.unhandled_escape_pressed = true;
                    }
                }
                EngineEvent::TouchInput {
                    id: _,
                    position,
                    phase,
                } => {
                    // Touch uses the same KRKR pointer callbacks as a primary
                    // mouse button. The contact id is retained by the host
                    // protocol so mobile gesture adapters can track multiple
                    // contacts; the legacy KRKR API itself exposes one active
                    // pointer, therefore the first contact drives it here.
                    self.cursor_position = Some(*position);
                    self.tjs_runtime.host_mut().set_cursor_position(*position);
                    match phase {
                        krkr_core::TouchPhase::Started => {
                            self.handle_input_event(EngineEvent::PointerInput {
                                button: PointerButton::Primary,
                                state: ButtonState::Pressed,
                            })?;
                        }
                        krkr_core::TouchPhase::Moved => {
                            self.dispatch_window_cursor_move(*position)?;
                            self.dispatch_layer_cursor_move(*position)?;
                        }
                        krkr_core::TouchPhase::Ended => {
                            self.handle_input_event(EngineEvent::PointerInput {
                                button: PointerButton::Primary,
                                state: ButtonState::Released,
                            })?;
                        }
                        krkr_core::TouchPhase::Cancelled => {
                            self.pressed_layer = None;
                            self.tjs_runtime.host_mut().set_captured_layer(None);
                        }
                    }
                }
                EngineEvent::Lifecycle { state } => {
                    self.tjs_runtime.host_mut().set_lifecycle_state(*state);
                }
                EngineEvent::PointerInput { .. } => {}
            }
        }
        Ok(())
    }

    fn update_input_key_state(&mut self, event: &EngineEvent) {
        let key = match event {
            EngineEvent::KeyboardInput { key, .. } => engine_key_vk_code(*key),
            EngineEvent::PointerInput { button, .. } => pointer_button_vk_code(*button),
            EngineEvent::CursorMoved { .. }
            | EngineEvent::MouseWheel { .. }
            | EngineEvent::TouchInput { .. }
            | EngineEvent::Lifecycle { .. } => None,
        };
        let state = match event {
            EngineEvent::KeyboardInput { state, .. } | EngineEvent::PointerInput { state, .. } => {
                *state
            }
            EngineEvent::CursorMoved { .. }
            | EngineEvent::MouseWheel { .. }
            | EngineEvent::TouchInput { .. }
            | EngineEvent::Lifecycle { .. } => return,
        };
        if let Some(key) = key {
            self.tjs_runtime
                .host_mut()
                .set_key_state(key, matches!(state, ButtonState::Pressed));
        }
    }

    fn current_shift_state(&self, repeat: bool) -> i64 {
        let host = self.tjs_runtime.host();
        let mut shift = 0;
        if host.key_state(0x10) {
            shift |= 1 << 0;
        }
        if host.key_state(0x12) {
            shift |= 1 << 1;
        }
        if host.key_state(0x11) {
            shift |= 1 << 2;
        }
        if host.key_state(0x01) {
            shift |= 1 << 3;
        }
        if host.key_state(0x02) {
            shift |= 1 << 4;
        }
        if host.key_state(0x04) {
            shift |= 1 << 5;
        }
        if repeat {
            shift |= 1 << 7;
        }
        shift
    }

    fn dispatch_focused_layer_key_event(
        &mut self,
        method: &str,
        key: EngineKey,
        shift: i64,
    ) -> Result<bool> {
        let Some(key_code) = engine_key_vk_code(key) else {
            return Ok(false);
        };
        let Some(window) = self.runtime_window_object() else {
            return Ok(false);
        };
        let Some(focused_layer) = self
            .tjs_runtime
            .host()
            .native_window_focused_layer(window)
            .or_else(|| plain_member_object(&self.tjs_runtime, window, "focusedLayer"))
        else {
            return Ok(false);
        };
        if !self.tjs_runtime.object_valid(focused_layer) {
            return Ok(false);
        }
        let handler = self.tjs_runtime.object_member(focused_layer, method);
        if matches!(handler, Variant::Void) {
            return Ok(false);
        }
        let handled_by_script = !self.tjs_runtime.variant_is_native_function(&handler);
        self.call_event_method(
            focused_layer,
            method,
            vec![
                Variant::Integer(key_code),
                Variant::Integer(shift),
                Variant::Integer(1),
            ],
        )
        .map(|_| handled_by_script)
    }

    fn dispatch_window_key_event(
        &mut self,
        method: &str,
        key: EngineKey,
        shift: i64,
    ) -> Result<bool> {
        let Some(key_code) = engine_key_vk_code(key) else {
            return Ok(false);
        };
        let Some(window) = self.runtime_window_object() else {
            return Ok(false);
        };
        let handler = self.tjs_runtime.object_member(window, method);
        if matches!(handler, Variant::Void) {
            return Ok(false);
        }
        let handled_by_script = !self.tjs_runtime.variant_is_native_function(&handler);
        self.call_event_method(
            window,
            method,
            vec![Variant::Integer(key_code), Variant::Integer(shift)],
        )
        .map(|_| handled_by_script)
    }

    /// `tTJSNI_BaseWindow::OnMouseMove` posts `onMouseMove(x, y, shift)` to the
    /// window itself before handing the same position to the draw device (which
    /// is what eventually produces the per-layer `onMouseMove`).  KAG builds its
    /// whole hover machinery on that window event -- `KAGWindow.onMouseMove`
    /// runs the `mouseMove` hook list, which drives the quick menu slide-in, the
    /// soft cursor, icon fading and cursor auto-hide -- so skipping it leaves
    /// those surfaces permanently in their initial state.
    ///
    /// The official post uses `TVP_EPT_DISCARDABLE`, so a move that arrives
    /// while the script is mid-execution is simply superseded by the next one
    /// rather than re-entering the interpreter.  A parked resource load is that
    /// same "script is still running" state here, and the hooks are registered
    /// before the objects they touch exist -- `QuickMenuLayerBase` installs its
    /// `mouseMove` hook and only then runs `setup()`, whose `uiload` suspends
    /// long before `createOffTimer()` -- so delivering during a suspend throws
    /// on `offtimer.enabled`.  Drop the move instead, exactly as a discardable
    /// event would be.
    fn dispatch_window_cursor_move(&mut self, position: Point) -> Result<()> {
        if self.tjs_runtime.is_suspended() {
            return Ok(());
        }
        let Some(window) = self.runtime_window_object() else {
            return Ok(());
        };
        if matches!(
            self.tjs_runtime.object_member(window, "onMouseMove"),
            Variant::Void
        ) {
            return Ok(());
        }
        let shift = self.current_shift_state(false);
        self.call_event_method(
            window,
            "onMouseMove",
            vec![
                Variant::Integer(position.x.round() as i64),
                Variant::Integer(position.y.round() as i64),
                Variant::Integer(shift),
            ],
        )
        .map(|_| ())
    }

    fn dispatch_window_pointer_event(&mut self, method: &str, button: i64) -> Result<bool> {
        let Some(position) = self.cursor_position else {
            return Ok(false);
        };
        let Some(window) = self.runtime_window_object() else {
            return Ok(false);
        };
        let shift = self.current_shift_state(false);
        let handler = self.tjs_runtime.object_member(window, method);
        if matches!(handler, Variant::Void) {
            return Ok(false);
        }
        let handled_by_script = !self.tjs_runtime.variant_is_native_function(&handler);
        self.call_event_method(
            window,
            method,
            vec![
                Variant::Integer(position.x.round() as i64),
                Variant::Integer(position.y.round() as i64),
                Variant::Integer(button),
                Variant::Integer(shift),
            ],
        )
        .map(|_| handled_by_script)
    }

    fn call_event_method(
        &mut self,
        object: ObjectHandle,
        method: &str,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        self.call_event_method_inner(object, method, args, false)
    }

    /// Event delivery on a layer: `tTVPEvent::Deliver` (`EventIntf.cpp:95`)
    /// ignores `TJS_E_MEMBERNOTFOUND`, so a target without that handler just
    /// carries on (no `System.exceptionHandler` report).
    fn call_optional_event_method(
        &mut self,
        object: ObjectHandle,
        method: &str,
        args: Vec<Variant>,
    ) -> Result<()> {
        self.call_event_method_inner(object, method, args, true)
            .map(|_| ())
    }

    fn call_event_method_inner(
        &mut self,
        object: ObjectHandle,
        method: &str,
        args: Vec<Variant>,
        optional: bool,
    ) -> Result<Variant> {
        let result = if self.tjs_runtime.is_suspended() {
            self.tjs_runtime
                .call_object_method_during_suspend(object, method, args)
        } else {
            self.tjs_runtime.call_object_method(object, method, args)
        };
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if is_resource_pending_error(&error) || error.is_debug_quit() {
                    return Err(error);
                }
                if optional && error.is_member_not_found() {
                    return Ok(Variant::Void);
                }
                // KRKR delivers every posted event inside
                // `TVP_CATCH_AND_SHOW_SCRIPT_EXCEPTION` (`EventIntf.cpp:622`);
                // the macro reports the exception through
                // `System.exceptionHandler` when the game installed one and
                // otherwise shows it (`TVPShowScriptException`), and event
                // delivery then carries on. An exception escaping a handler
                // must not abort the frame.
                let handled = self.tjs_runtime.process_unhandled_exception(&error)?;
                self.tjs_runtime.host_mut().log(&format!(
                    "{} event `{method}` on object#{} error: {}",
                    if handled { "handled" } else { "unhandled" },
                    object.0,
                    error.message
                ));
                Ok(Variant::Void)
            }
        }
    }

    fn handle_callback_error(&mut self, context: &str, error: TjsError) -> Result<()> {
        if is_resource_pending_error(&error) || error.is_debug_quit() {
            return Err(error);
        }
        // Same rule as `call_event_method`: KRKR runs every posted callback
        // inside `TVP_CATCH_AND_SHOW_SCRIPT_EXCEPTION`
        // (`base/ScriptMgnIntf.h:77`), which reports the exception and lets
        // the engine carry on. A game callback that throws must not take the
        // frame down with it.
        let handled = self.tjs_runtime.process_unhandled_exception(&error)?;
        self.tjs_runtime.host_mut().log(&format!(
            "{} {context}: {}",
            if handled { "handled" } else { "unhandled" },
            error.message
        ));
        Ok(())
    }

    fn dispatch_window_mouse_wheel(&mut self, delta: i32) -> Result<()> {
        let Some(position) = self.cursor_position else {
            return Ok(());
        };
        let Some(window) = self.runtime_window_object() else {
            return Ok(());
        };
        if matches!(
            self.tjs_runtime.object_member(window, "onMouseWheel"),
            Variant::Void
        ) {
            return Ok(());
        }
        self.call_event_method(
            window,
            "onMouseWheel",
            vec![
                Variant::Integer(self.current_shift_state(false)),
                Variant::Integer(delta as i64),
                Variant::Integer(position.x.round() as i64),
                Variant::Integer(position.y.round() as i64),
            ],
        )
        .map(|_| ())
    }

    fn dispatch_window_click(&mut self) -> Result<()> {
        let Some(position) = self.cursor_position else {
            return Ok(());
        };
        let Some(window) = self.runtime_window_object() else {
            return Ok(());
        };
        if matches!(
            self.tjs_runtime.object_member(window, "onClick"),
            Variant::Void
        ) {
            return Ok(());
        }
        self.call_event_method(
            window,
            "onClick",
            vec![
                Variant::Integer(position.x.round() as i64),
                Variant::Integer(position.y.round() as i64),
            ],
        )
        .map(|_| ())
    }

    fn dispatch_layer_pointer_event(
        &mut self,
        method: &str,
        button: i64,
        layer_override: Option<LayerId>,
    ) -> Result<Option<LayerEventTarget>> {
        let Some(position) = self.cursor_position else {
            return Ok(None);
        };
        let layer_id = match layer_override {
            Some(layer_id) => Some(layer_id),
            None => self.layer_at_position(position)?,
        };
        let Some(layer_id) = layer_id else {
            return Ok(None);
        };
        let Some(object) = self.tjs_runtime.host().native_object_for_layer(layer_id) else {
            return Ok(None);
        };
        let Some(origin) = self
            .tjs_runtime
            .host()
            .layer_tree()
            .absolute_position(layer_id)
        else {
            return Ok(None);
        };
        let x = (position.x - origin.x).round() as i64;
        let y = (position.y - origin.y).round() as i64;
        let shift = self.current_shift_state(false);
        self.call_event_method(
            object,
            method,
            vec![
                Variant::Integer(x),
                Variant::Integer(y),
                Variant::Integer(button),
                Variant::Integer(shift),
            ],
        )
        .map(|_| Some(LayerEventTarget { layer_id, object }))
    }

    fn should_fire_primary_click(&self, target: Option<LayerId>) -> bool {
        let Some(layer_id) = target else {
            return true;
        };
        let Some(target) = self.tjs_runtime.host().native_object_for_layer(layer_id) else {
            return true;
        };
        matches!(
            self.tjs_runtime.object_member(target, "linkNum"),
            Variant::Void
        ) && !self.layer_has_script_handler(layer_id, "onClick")
            && !self.layer_has_script_handler(layer_id, "onMouseUp")
    }

    fn fire_kag_primary_click(&mut self, keyboard: bool) -> Result<()> {
        let Some(kag) = self.tjs_runtime.global_member("kag").object_handle() else {
            return Ok(());
        };
        let method = if keyboard
            && !matches!(
                self.tjs_runtime.object_member(kag, "onPrimaryClickByKey"),
                Variant::Void
            ) {
            "onPrimaryClickByKey"
        } else {
            "onPrimaryClick"
        };
        if matches!(self.tjs_runtime.object_member(kag, method), Variant::Void) {
            return Ok(());
        }
        self.call_event_method(kag, method, Vec::new()).map(|_| ())
    }

    fn fire_kag_secondary_click(&mut self) -> Result<()> {
        if let Some(kag) = self.tjs_runtime.global_member("kag").object_handle()
            && !matches!(
                self.tjs_runtime.object_member(kag, "onPrimaryRightClick"),
                Variant::Void
            )
        {
            return self
                .call_event_method(kag, "onPrimaryRightClick", Vec::new())
                .map(|_| ());
        }

        self.kag_session.fire_right_click(&mut self.tjs_runtime)
    }

    fn dispatch_layer_cursor_move(&mut self, position: Point) -> Result<()> {
        // `tTVPLayerManager::PrimaryMouseMove` (`LayerManager.cpp:398`):
        // the target is the capture owner, else the front-most hittable layer
        // -- never a layer picked for "having a handler"; enter/leave follow
        // a change of that target, and `onMouseMove` is only posted while the
        // integer cursor position actually changed (`poschanged`).
        let moved = self.record_cursor_move_position(position);
        let mut target = self.cursor_event_target(position)?;
        let hovered = self.tjs_runtime.host().hovered_layer();
        if target != hovered {
            if let Some(layer_id) = hovered {
                self.call_layer_event(layer_id, "onMouseLeave", Vec::new())?;
                // The handler may invalidate the layer, so the target is
                // re-queried once after each enter/leave, exactly like the
                // reference implementation.
                target = self.cursor_event_target(position)?;
            }
            if let Some(layer_id) = target {
                self.call_layer_event(layer_id, "onMouseEnter", Vec::new())?;
                let rechecked = self.cursor_event_target(position)?;
                if rechecked != target {
                    self.call_layer_event(layer_id, "onMouseLeave", Vec::new())?;
                    target = rechecked;
                    if let Some(next) = target {
                        self.call_layer_event(next, "onMouseEnter", Vec::new())?;
                    }
                }
            }
            self.tjs_runtime.host_mut().set_hovered_layer(target);
        }

        if !moved {
            return Ok(());
        }
        let Some(layer_id) = target else {
            return Ok(());
        };
        let shift = self.current_shift_state(false);
        if let Some((x, y)) = self.layer_local_point(layer_id, position) {
            self.call_layer_event(
                layer_id,
                "onMouseMove",
                vec![
                    Variant::Integer(x),
                    Variant::Integer(y),
                    Variant::Integer(shift),
                ],
            )?;
        }
        Ok(())
    }

    /// The layer a pointer event is delivered to: the capture owner while a
    /// button is held, the front-most hittable layer otherwise
    /// (`tTVPLayerManager::PrimaryMouseMove`/`PrimaryMouseDown`,
    /// `LayerManager.cpp:398` and `:360`).  `tTVPEvent::Deliver` drops the
    /// event when that layer has no matching handler
    /// (`EventIntf.cpp:104`), so a handler-less layer swallows the event
    /// instead of passing it to a layer behind it.
    fn cursor_event_target(&mut self, position: Point) -> Result<Option<LayerId>> {
        if let Some(captured) = self.tjs_runtime.host().captured_layer() {
            return Ok(Some(captured));
        }
        self.layer_at_position(position)
    }

    /// `poschanged = LastMouseMoveX != x || LastMouseMoveY != y`
    /// (`LayerManager.cpp:400`): the reference compares rounded positions, so
    /// a sub-pixel move posts no `onMouseMove`.
    fn record_cursor_move_position(&mut self, position: Point) -> bool {
        let current = (position.x.round() as i64, position.y.round() as i64);
        let moved = self.last_mouse_move != Some(current);
        self.last_mouse_move = Some(current);
        moved
    }

    fn dispatch_layer_mouse_wheel(&mut self, delta: i32) -> Result<()> {
        let Some(position) = self.cursor_position else {
            return Ok(());
        };
        let Some(window) = self.runtime_window_object() else {
            return Ok(());
        };
        let Some(focused_layer) = self
            .tjs_runtime
            .host()
            .native_window_focused_layer(window)
            .or_else(|| plain_member_object(&self.tjs_runtime, window, "focusedLayer"))
        else {
            return Ok(());
        };
        if !self.tjs_runtime.object_valid(focused_layer)
            || matches!(
                self.tjs_runtime
                    .object_member(focused_layer, "onMouseWheel"),
                Variant::Void
            )
        {
            return Ok(());
        }
        self.call_event_method(
            focused_layer,
            "onMouseWheel",
            vec![
                Variant::Integer(self.current_shift_state(false)),
                Variant::Integer(delta as i64),
                Variant::Integer(position.x.round() as i64),
                Variant::Integer(position.y.round() as i64),
            ],
        )
        .map(|_| ())
    }

    fn layer_at_cursor(&mut self) -> Result<Option<LayerId>> {
        let Some(position) = self.cursor_position else {
            return Ok(None);
        };
        self.layer_at_position(position)
    }

    fn layer_at_position(&mut self, position: Point) -> Result<Option<LayerId>> {
        Ok(self
            .hit_tested_layers_at_position(position)?
            .into_iter()
            .next())
    }

    /// Pointer capture keeps a press target alive while a control redraws or
    /// replaces its child layers between down and up.  Use the target's live
    /// bounds as a release fallback when the normal topmost hit changed; this
    /// prevents an asynchronous asset/UI refresh from swallowing onClick.
    fn layer_contains_cursor(&self, layer_id: LayerId) -> bool {
        let Some(position) = self.cursor_position else {
            return false;
        };
        let Some(origin) = self
            .tjs_runtime
            .host()
            .layer_tree()
            .absolute_position(layer_id)
        else {
            return false;
        };
        let Some(node) = self.tjs_runtime.host().layer_tree().layer(layer_id) else {
            return false;
        };
        position.x >= origin.x
            && position.y >= origin.y
            && position.x < origin.x + node.width
            && position.y < origin.y + node.height
    }

    fn hit_tested_layers_at_position(&mut self, position: Point) -> Result<Vec<LayerId>> {
        let candidates = self.tjs_runtime.host().layer_tree().hit_test_all(position);
        let modal_window = self.active_modal_window();
        let mut hits = Vec::new();
        for layer_id in candidates {
            if let Some(window) = modal_window
                && !self.layer_belongs_to_window(layer_id, window)
            {
                continue;
            }
            if self.layer_accepts_script_hit_test(layer_id, position)? {
                hits.push(layer_id);
            }
        }
        Ok(hits)
    }

    fn layer_belongs_to_window(&self, layer_id: LayerId, window: ObjectHandle) -> bool {
        let Some(object) = self.tjs_runtime.host().native_object_for_layer(layer_id) else {
            return false;
        };
        self.tjs_runtime
            .host()
            .native_layer_window(object)
            .or_else(|| plain_member_object(&self.tjs_runtime, object, "window"))
            == Some(window)
    }

    fn layer_has_script_handler(&self, layer_id: LayerId, method: &str) -> bool {
        let Some(object) = self.tjs_runtime.host().native_object_for_layer(layer_id) else {
            return false;
        };
        let handler = self.tjs_runtime.object_member(object, method);
        !matches!(handler, Variant::Void) && !self.tjs_runtime.variant_is_native_function(&handler)
    }

    fn layer_accepts_script_hit_test(
        &mut self,
        layer_id: LayerId,
        position: Point,
    ) -> Result<bool> {
        let Some(object) = self.tjs_runtime.host().native_object_for_layer(layer_id) else {
            return Ok(false);
        };
        let Some(origin) = self
            .tjs_runtime
            .host()
            .layer_tree()
            .absolute_position(layer_id)
        else {
            return Ok(false);
        };
        let x = (position.x - origin.x).round() as i64;
        let y = (position.y - origin.y).round() as i64;
        self.tjs_runtime
            .set_object_member(object, "__nativeHitTestWork", Variant::Integer(1));
        let has_handler = !matches!(
            self.tjs_runtime.object_member(object, "onHitTest"),
            Variant::Void
        );
        if has_handler {
            self.call_event_method(
                object,
                "onHitTest",
                vec![
                    Variant::Integer(x),
                    Variant::Integer(y),
                    Variant::Integer(1),
                ],
            )?;
        }
        let result = self
            .tjs_runtime
            .object_member(object, "__nativeHitTestWork")
            .is_truthy();
        Ok(result)
    }

    fn dispatch_layer_click(&mut self, layer_id: LayerId) -> Result<()> {
        let Some((x, y)) = self.cursor_position.and_then(|position| {
            self.tjs_runtime
                .host()
                .layer_tree()
                .absolute_position(layer_id)
                .map(|origin| {
                    (
                        (position.x - origin.x).round() as i64,
                        (position.y - origin.y).round() as i64,
                    )
                })
        }) else {
            return Ok(());
        };
        self.tjs_runtime.host_mut().log(&format!(
            "input dispatch layer onClick id={:?} local=({x},{y})",
            layer_id
        ));
        self.call_layer_event(
            layer_id,
            "onClick",
            vec![Variant::Integer(x), Variant::Integer(y)],
        )
    }

    fn call_layer_event(
        &mut self,
        layer_id: LayerId,
        method: &str,
        args: Vec<Variant>,
    ) -> Result<()> {
        let Some(object) = self.tjs_runtime.host().native_object_for_layer(layer_id) else {
            return Ok(());
        };
        // `tTVPEvent::Deliver` (`EventIntf.cpp:95`) FuncCalls the event name on
        // the target with the target itself as objthis, and a member miss is
        // simply "no handler here".  The lookup must go through `missing`
        // (`tTJSCustomObject::FuncCall`, `tjsObject.cpp:1316`): a layer that
        // inherits the handler from a script parent -- KAGEX's
        // `ParentHackLayer` forwards every member write to the message layer --
        // is still called with itself as `this`, which is what the forwarded
        // wrapper reads `dragHookOwner` from.
        let result = self.call_optional_event_method(object, method, args);
        self.sync_kag_slots_after_ok(result)
    }

    fn layer_local_point(&self, layer_id: LayerId, position: Point) -> Option<(i64, i64)> {
        let origin = self
            .tjs_runtime
            .host()
            .layer_tree()
            .absolute_position(layer_id)?;
        Some((
            (position.x - origin.x).round() as i64,
            (position.y - origin.y).round() as i64,
        ))
    }

    pub fn register_plugin<P>(&mut self, plugin: P) -> Result<()>
    where
        P: KrkrPlugin + 'static,
    {
        plugin.register(&mut self.tjs_runtime)?;
        let plugin: Arc<dyn KrkrPlugin> = Arc::new(plugin);
        self.tjs_runtime
            .host_mut()
            .register_plugin(Arc::clone(&plugin));
        self.plugins.push(plugin);
        Ok(())
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LayerEventTarget {
    layer_id: LayerId,
    object: ObjectHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeSchedulerPump {
    Full,
    WindowUpdatesOnly,
}

/// Debugger hook run before each KAG tag is processed. Stops on KAG
/// line/label breakpoints and KAG tag stepping, invoking the same debug UI
/// the TJS VM uses.
fn debug_check_kag_tag(
    parser: &KagParser,
    runtime: &mut Runtime<KrkrHost>,
    tag: &Tag,
) -> Result<()> {
    if runtime.debugger().is_none() {
        return Ok(());
    }
    let storage = parser.cur_storage().unwrap_or_default().to_string();
    let label = parser.cur_label().map(str::to_string);
    let reason = runtime.debugger_mut().and_then(|debugger| {
        debugger.check_kag(&storage, Some(tag.location.line), label.as_deref())
    });
    let Some(reason) = reason else {
        return Ok(());
    };
    let Some(mut ui) = runtime.take_debug_ui() else {
        return Ok(());
    };
    let action = {
        let mut pause = Pause::new_kag(reason, runtime);
        ui.on_pause(&mut pause)
    };
    runtime.put_debug_ui(ui);
    if let Some(debugger) = runtime.debugger_mut() {
        debugger.apply_action(action, None, 0)?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct KagSession {
    parser: Option<ObjectHandle>,
    parser_revision: u64,
    state: KagTaskState,
    handler: Option<ObjectHandle>,
    pending_tags: VecDeque<Tag>,
    temp_snapshots: BTreeMap<i64, KagTempSnapshot>,
    right_click: RightClickAction,
    system_hooks: BTreeMap<String, SystemHook>,
    loaded: bool,
    clear_page_on_click: bool,
    clear_page_on_timer: bool,
    message_layer: MessageLayerModel,
}

#[derive(Clone, Debug)]
struct KagTempSnapshot {
    parser: ParserSnapshot,
    message_layer: MessageLayerModel,
    state: KagTaskState,
    pending_tags: VecDeque<Tag>,
    right_click: RightClickAction,
    clear_page_on_click: bool,
    clear_page_on_timer: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RightClickAction {
    enabled: bool,
    call: bool,
    jump: bool,
    storage: Option<String>,
    target: Option<String>,
}

#[derive(Clone, Debug)]
struct SystemHook {
    storage: Option<String>,
    target: Option<String>,
    call: bool,
}

impl KagSession {
    fn new() -> Self {
        Self {
            parser: None,
            parser_revision: 0,
            state: KagTaskState::Finished,
            handler: None,
            pending_tags: VecDeque::new(),
            temp_snapshots: BTreeMap::new(),
            right_click: RightClickAction::default(),
            system_hooks: BTreeMap::new(),
            loaded: false,
            clear_page_on_click: false,
            clear_page_on_timer: false,
            message_layer: MessageLayerModel::default(),
        }
    }

    fn active_parser(&self) -> Option<ObjectHandle> {
        self.parser
    }

    fn state(&self) -> &KagTaskState {
        &self.state
    }

    fn loaded(&self) -> bool {
        self.loaded
    }

    fn message_layer(&self) -> &MessageLayerModel {
        &self.message_layer
    }

    fn location(&self, runtime: &Runtime<KrkrHost>) -> KagLocation {
        let parser = self
            .parser
            .and_then(|handle| runtime.host().kag_parser(handle));
        KagLocation {
            storage: parser.and_then(KagParser::cur_storage).map(str::to_string),
            label: parser.and_then(KagParser::cur_label).map(str::to_string),
            line: parser.and_then(KagParser::cur_line),
            page: self.message_layer.page,
        }
    }

    fn ensure_active_parser(
        &mut self,
        runtime: &mut Runtime<KrkrHost>,
    ) -> krkr_kag::Result<ObjectHandle> {
        if let Some(handle) = self.parser
            && runtime.object_valid(handle)
            && runtime.host().kag_parser(handle).is_some()
        {
            return Ok(handle);
        }

        let handle = create_kag_parser_object(runtime).map_err(tjs_to_kag)?;
        self.parser = Some(handle);
        self.parser_revision = runtime.host().kag_parser_revision(handle);
        Ok(handle)
    }

    fn active_parser_or_error(&self) -> krkr_kag::Result<ObjectHandle> {
        self.parser.ok_or(KagError::NoScenario)
    }

    fn observe_external_parser_changes(&mut self, runtime: &Runtime<KrkrHost>) {
        let Some(handle) = self.parser else {
            return;
        };
        let revision = runtime.host().kag_parser_revision(handle);
        if revision == self.parser_revision {
            return;
        }
        self.parser_revision = revision;
        self.loaded = runtime
            .host()
            .kag_parser(handle)
            .is_some_and(|parser| parser.cur_storage().is_some());
        if self.loaded {
            self.pending_tags.clear();
            self.clear_page_on_click = false;
            self.clear_page_on_timer = false;
            self.message_layer.waiting_for_click = false;
            self.state = KagTaskState::Running;
        }
    }

    fn load_scenario(
        &mut self,
        storage: &str,
        runtime: &mut Runtime<KrkrHost>,
    ) -> krkr_kag::Result<()> {
        let handle = self.ensure_active_parser(runtime)?;
        self.with_parser_for_kag(runtime, |parser, session, runtime, owner| {
            let mut host = EngineKagHost::for_owner(runtime, owner);
            parser.load_scenario_with(storage, &mut host)?;
            session.message_layer.clear();
            session.start();
            Ok(())
        })?;
        self.parser_revision = runtime.host().kag_parser_revision(handle);
        Ok(())
    }

    fn next_tag(&mut self, runtime: &mut Runtime<KrkrHost>) -> krkr_kag::Result<Option<Tag>> {
        self.observe_external_parser_changes(runtime);
        self.active_parser_or_error()?;
        self.with_parser_for_kag(runtime, |parser, _, runtime, owner| {
            let mut host = EngineKagHost::for_owner(runtime, owner);
            parser.next_tag_with(&mut host)
        })
    }

    fn with_parser_for_kag<R, F>(
        &mut self,
        runtime: &mut Runtime<KrkrHost>,
        f: F,
    ) -> krkr_kag::Result<R>
    where
        F: FnOnce(
            &mut KagParser,
            &mut KagSession,
            &mut Runtime<KrkrHost>,
            ObjectHandle,
        ) -> krkr_kag::Result<R>,
    {
        let handle = self.active_parser_or_error()?;
        let mut parser = runtime
            .host_mut()
            .take_kag_parser(handle)
            .ok_or(KagError::NoScenario)?;

        let result = (|| {
            runtime.host_mut().insert_kag_parser(handle, parser.clone());
            let revision_before_callback = runtime.host().kag_parser_revision(handle);
            let result = f(&mut parser, self, runtime, handle);
            // Engine-side callbacks may re-enter the native KAGParser (for
            // example while onScenarioLoaded is running).  Reconcile the
            // parser that the callback mutated before putting the outer
            // snapshot back in the host map.
            let revision_after_callback = runtime.host().kag_parser_revision(handle);
            if let Some(updated) = runtime.host_mut().take_kag_parser(handle)
                && revision_after_callback != revision_before_callback
            {
                parser = updated;
            }
            refresh_kag_parser_object(runtime, handle, &parser).map_err(tjs_to_kag)?;
            result
        })();

        runtime.host_mut().insert_kag_parser(handle, parser);
        self.parser_revision = runtime.host().kag_parser_revision(handle);
        result
    }

    fn with_parser_for_tjs<R, F>(&mut self, runtime: &mut Runtime<KrkrHost>, f: F) -> Result<R>
    where
        F: FnOnce(
            &mut KagParser,
            &mut KagSession,
            &mut Runtime<KrkrHost>,
            ObjectHandle,
        ) -> Result<R>,
    {
        self.observe_external_parser_changes(runtime);
        let handle = self
            .active_parser_or_error()
            .map_err(|error| TjsError::runtime(error.to_string()))?;
        let mut parser = runtime
            .host_mut()
            .take_kag_parser(handle)
            .ok_or_else(|| TjsError::runtime("active KAG parser is not registered"))?;

        let result = (|| {
            runtime.host_mut().insert_kag_parser(handle, parser.clone());
            let revision_before_callback = runtime.host().kag_parser_revision(handle);
            let result = f(&mut parser, self, runtime, handle);
            // TJS callbacks may call KAGParser.loadScenario/goToLabel through
            // the host map while this outer parser is borrowed. Pull the
            // callback's mutated parser back before refreshing/reinserting;
            // otherwise a nested jump is silently overwritten by the stale
            // outer clone when the callback returns.
            let revision_after_callback = runtime.host().kag_parser_revision(handle);
            if let Some(updated) = runtime.host_mut().take_kag_parser(handle)
                && revision_after_callback != revision_before_callback
            {
                parser = updated;
            }
            refresh_kag_parser_object(runtime, handle, &parser)?;
            result
        })();

        runtime.host_mut().insert_kag_parser(handle, parser);
        self.parser_revision = runtime.host().kag_parser_revision(handle);
        result
    }

    fn start(&mut self) {
        self.state = KagTaskState::Running;
        self.pending_tags.clear();
        self.temp_snapshots.clear();
        self.right_click = RightClickAction::default();
        self.loaded = true;
        self.clear_page_on_click = false;
        self.clear_page_on_timer = false;
    }

    fn set_handler(&mut self, handler: ObjectHandle) {
        self.handler = Some(handler);
    }

    fn clear_handler(&mut self) {
        self.handler = None;
    }

    fn signal_click(&mut self) {
        if self.state == KagTaskState::WaitingClick {
            if self.clear_page_on_click {
                self.message_layer.clear_text();
                self.clear_page_on_click = false;
            }
            self.clear_page_on_timer = false;
            self.message_layer.waiting_for_click = false;
            self.state = KagTaskState::Running;
        }
    }

    fn signal_audio_finished(&mut self) {
        if self.state == KagTaskState::WaitingAudio {
            self.state = KagTaskState::Running;
        }
    }

    fn update_wait(&mut self, delta: Duration, transition_active: bool, resource_pending: bool) {
        if let KagTaskState::WaitingTimer { remaining } = self.state.clone() {
            self.state = if delta >= remaining {
                if self.clear_page_on_timer {
                    self.message_layer.clear_text();
                    self.clear_page_on_timer = false;
                }
                KagTaskState::Running
            } else {
                KagTaskState::WaitingTimer {
                    remaining: remaining - delta,
                }
            };
        } else if (self.state == KagTaskState::WaitingTransition && !transition_active)
            || (self.state == KagTaskState::WaitingResource && !resource_pending)
        {
            self.state = KagTaskState::Running;
        }
    }

    fn run_until_yield(
        &mut self,
        runtime: &mut Runtime<KrkrHost>,
        budget: KagRunBudget,
    ) -> Result<EngineTickResult> {
        self.observe_external_parser_changes(runtime);
        if self.parser.is_none() {
            let started = BudgetTimer::start();
            return Ok(EngineTickResult {
                state: self.state.clone(),
                reason: KagYieldReason::Finished,
                tags_processed: 0,
                elapsed: started.elapsed(),
            });
        }
        self.with_parser_for_tjs(runtime, |parser, session, runtime, owner| {
            session.run_until_yield_with_parser(parser, runtime, owner, budget)
        })
    }

    fn run_until_yield_with_parser(
        &mut self,
        parser: &mut KagParser,
        runtime: &mut Runtime<KrkrHost>,
        owner: ObjectHandle,
        budget: KagRunBudget,
    ) -> Result<EngineTickResult> {
        let started = BudgetTimer::start();
        let mut tags_processed = 0;

        if self.state != KagTaskState::Running {
            return Ok(EngineTickResult {
                state: self.state.clone(),
                reason: if self.state == KagTaskState::Finished {
                    KagYieldReason::Finished
                } else {
                    KagYieldReason::AlreadyBlocked
                },
                tags_processed,
                elapsed: started.elapsed(),
            });
        }

        loop {
            if runtime.is_suspended() {
                let modal = runtime.host().current_modal_window();
                let pending_resources = runtime.host().has_pending_resource_loads();
                let pending_external = runtime.host().has_pending_external_resources();
                runtime.host_mut().log(&format!(
                    "KAG VM suspended: modal={:?} pendingResources={} pendingExternal={}",
                    modal, pending_resources, pending_external
                ));
                if pending_resources || pending_external {
                    self.state = KagTaskState::WaitingResource;
                    return Ok(EngineTickResult {
                        state: self.state.clone(),
                        reason: KagYieldReason::Waiting(self.state.clone()),
                        tags_processed,
                        elapsed: started.elapsed(),
                    });
                }
                // Some hosts can tear down the modal object while its TJS
                // callback is suspended. There is then no user-visible
                // modal left to wait for; resume the VM so KAG does not stay
                // permanently in WaitingModal with an empty modal stack.
                if runtime.host().current_modal_window().is_none() {
                    let resumed = runtime.resume_suspended();
                    let still_suspended = runtime.is_suspended();
                    runtime.host_mut().log(&format!(
                        "KAG resume without modal: result={:?} stillSuspended={}",
                        resumed
                            .as_ref()
                            .map(|value| value.as_ref().map(|_| "value")),
                        still_suspended
                    ));
                    resumed?;
                    if !runtime.is_suspended() {
                        runtime
                            .host_mut()
                            .log("KAG resumed suspended VM without active modal");
                        continue;
                    }
                }
                self.state = KagTaskState::WaitingModal;
                return Ok(EngineTickResult {
                    state: self.state.clone(),
                    reason: KagYieldReason::Waiting(self.state.clone()),
                    tags_processed,
                    elapsed: started.elapsed(),
                });
            }

            if tags_processed >= budget.max_tags_per_tick
                || (tags_processed > 0 && started.elapsed() >= budget.max_wall_time)
            {
                return Ok(EngineTickResult {
                    state: self.state.clone(),
                    reason: KagYieldReason::BudgetExhausted,
                    tags_processed,
                    elapsed: started.elapsed(),
                });
            }

            let tag = match self.pending_tags.pop_front() {
                Some(tag) => Some(tag),
                None => {
                    let mut host = EngineKagHost::for_owner(runtime, owner);
                    match parser.next_tag_with(&mut host) {
                        Ok(tag) => tag,
                        Err(
                            KagError::ResourcePending { storage: _ }
                            | KagError::HostSuspended { storage: _ },
                        ) => {
                            // The parser rewound to the item that could not be
                            // processed yet (remote scenario text or a script
                            // call parked on an async resource); retry it once
                            // the resource provider resumes the VM.
                            self.state = KagTaskState::WaitingResource;
                            return Ok(EngineTickResult {
                                state: self.state.clone(),
                                reason: KagYieldReason::Waiting(self.state.clone()),
                                tags_processed,
                                elapsed: started.elapsed(),
                            });
                        }
                        Err(error) => {
                            let message = error.to_string();
                            self.state = KagTaskState::Error {
                                message: message.clone(),
                            };
                            return Err(TjsError::runtime(message));
                        }
                    }
                }
            };

            let Some(tag) = tag else {
                self.state = KagTaskState::Finished;
                return Ok(EngineTickResult {
                    state: self.state.clone(),
                    reason: KagYieldReason::Finished,
                    tags_processed,
                    elapsed: started.elapsed(),
                });
            };

            tags_processed += 1;
            debug_check_kag_tag(parser, runtime, &tag)?;
            let action = match self.process_tag(parser, runtime, owner, tag.clone()) {
                Ok(action) => action,
                Err(error) if error.kind == krkr_tjs2::TjsErrorKind::ResourcePending => {
                    // Native tag handlers (notably image/audio/video loads)
                    // can suspend while resolving a remote storage. Requeue
                    // the tag so the exact operation is retried after the
                    // asset provider resumes the VM.
                    self.pending_tags.push_front(tag);
                    self.state = KagTaskState::WaitingResource;
                    return Ok(EngineTickResult {
                        state: self.state.clone(),
                        reason: KagYieldReason::Waiting(self.state.clone()),
                        tags_processed: tags_processed.saturating_sub(1),
                        elapsed: started.elapsed(),
                    });
                }
                Err(error) => return Err(error),
            };
            match action {
                TagAction::Continue => {}
                TagAction::Yield(reason) => {
                    return Ok(EngineTickResult {
                        state: self.state.clone(),
                        reason,
                        tags_processed,
                        elapsed: started.elapsed(),
                    });
                }
            }
        }
    }

    fn process_tag(
        &mut self,
        parser: &mut KagParser,
        runtime: &mut Runtime<KrkrHost>,
        owner: ObjectHandle,
        tag: Tag,
    ) -> Result<TagAction> {
        if matches!(
            tag.tagname.as_str(),
            "syshook" | "sysjump" | "addSysHook" | "addSysScript"
        ) {
            runtime.host_mut().log(&format!(
                "KAG control tag `{}` attrs={:?}",
                tag.tagname, tag.attributes
            ));
        }
        // The title screen uses a small set of KRKR-specific tags which are
        // normally implemented by the system KAG conductor.  A game's
        // generic `onTag` hook is intentionally permissive and may report an
        // unknown tag as handled without doing any rendering work.  Dispatch
        // these visual tags natively first so the background/logo/effect are
        // always materialized as real KAG layers.
        if matches!(
            tag.tagname.as_str(),
            "title_image" | "title_logo" | "title_effect"
        ) {
            // A title jump can leave the previous scenario's message wait
            // state alive while the system UI is being rebuilt.  The stock
            // title conductors hide that layer; clear the portable fallback
            // overlay at the same boundary so it cannot cover the menu.
            if tag.tagname == "title_image" {
                self.message_layer.clear();
            }
            return self.process_native_fallback_tag(parser, runtime, owner, &tag);
        }
        if matches!(tag.tagname.as_str(), "syspage" | "uiload") {
            runtime.host_mut().log(&format!(
                "KAG UI tag `{}` attrs={:?}",
                tag.tagname, tag.attributes
            ));
        }
        // System hook declarations/dispatch are engine control-flow tags.
        // They must not be consumed by a generic game `onTag` callback that
        // returns success for every unknown tag.
        if matches!(
            tag.tagname.as_str(),
            "syshook" | "addSysHook" | "addSysScript"
        ) {
            return self.process_native_fallback_tag(parser, runtime, owner, &tag);
        }
        if tag.tagname == "sysjump" {
            // Let the game's TJS hook run first (it commonly installs title
            // hooks or performs bookkeeping), then apply the parser jump on
            // the session-owned parser when the callback did not mutate this
            // parser instance. TJS callbacks can operate on a cloned parser
            // object, so relying on that mutation alone loses the jump when
            // control returns to the KAG frame loop.
            let before = parser.cur_storage().map(str::to_string);
            if let Some(action) = self.try_tjs_tag_handler(runtime, owner, &tag)? {
                if matches!(action, TjsTagAction::Handled(_))
                    && parser.cur_storage().map(str::to_string) == before
                {
                    self.process_native_fallback_tag(parser, runtime, owner, &tag)?;
                }
                return Ok(match action {
                    TjsTagAction::Handled(action) => action,
                    TjsTagAction::NativeFallback => {
                        self.process_native_fallback_tag(parser, runtime, owner, &tag)?
                    }
                });
            }
            return self.process_native_fallback_tag(parser, runtime, owner, &tag);
        }
        if let Some(action) = self.try_tjs_tag_handler(runtime, owner, &tag)? {
            match action {
                TjsTagAction::Handled(action) => Ok(action),
                TjsTagAction::NativeFallback => {
                    self.process_native_fallback_tag(parser, runtime, owner, &tag)
                }
            }
        } else {
            self.process_native_fallback_tag(parser, runtime, owner, &tag)
        }
    }

    fn try_tjs_tag_handler(
        &mut self,
        runtime: &mut Runtime<KrkrHost>,
        owner: ObjectHandle,
        tag: &Tag,
    ) -> Result<Option<TjsTagAction>> {
        for handler in self.tjs_tag_handler_candidates(runtime, owner) {
            if !matches!(runtime.object_member(handler, "onTag"), Variant::Void) {
                let tag_object = tag_to_dictionary(runtime, tag)?;
                let value = self.call_tag_handler(
                    runtime,
                    handler,
                    "onTag",
                    vec![Variant::Object(tag_object)],
                )?;
                return self.apply_tjs_handler_step(tag.clone(), value).map(Some);
            }

            if NativeFallbackTag::from_name(&tag.tagname).is_none()
                && !matches!(
                    runtime.object_member(handler, "onUnknownTag"),
                    Variant::Void
                )
            {
                if matches!(
                    tag.tagname.as_str(),
                    "syshook" | "addSysHook" | "addSysScript"
                ) {
                    runtime.host_mut().log(&format!(
                        "KAG unknown handler dispatch tag={} handler={:?}",
                        tag.tagname, handler
                    ));
                }
                runtime.host_mut().trace(
                    TraceCategory::Kag,
                    &format!("kag: tag `{}` -> onUnknownTag", tag.tagname),
                );
                let tag_object = tag_to_dictionary(runtime, tag)?;
                let value = self.call_tag_handler(
                    runtime,
                    handler,
                    "onUnknownTag",
                    vec![
                        Variant::String(tag.tagname.clone()),
                        Variant::Object(tag_object),
                    ],
                )?;
                if matches!(
                    tag.tagname.as_str(),
                    "syshook" | "addSysHook" | "addSysScript"
                ) {
                    runtime.host_mut().log(&format!(
                        "KAG unknown handler result tag={} value={:?}",
                        tag.tagname, value
                    ));
                }
                return self.apply_tjs_handler_step(tag.clone(), value).map(Some);
            }
        }

        Ok(None)
    }

    fn tjs_tag_handler_candidates(
        &self,
        runtime: &Runtime<KrkrHost>,
        owner: ObjectHandle,
    ) -> Vec<ObjectHandle> {
        let mut candidates = Vec::new();
        push_unique_handler(&mut candidates, self.handler);
        push_unique_handler(&mut candidates, Some(owner));

        if let Some(kag) = runtime.global_member("kag").object_handle() {
            if let Some(conductor) = runtime.object_member(kag, "conductor").object_handle() {
                push_unique_handler(&mut candidates, Some(conductor));
            }
            if let Some(conductor) = runtime.object_member(kag, "mainConductor").object_handle() {
                push_unique_handler(&mut candidates, Some(conductor));
            }
            push_unique_handler(&mut candidates, Some(kag));
        }

        candidates
    }

    fn call_tag_handler(
        &mut self,
        runtime: &mut Runtime<KrkrHost>,
        handler: ObjectHandle,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        call_tag_handler(runtime, handler, name, args).inspect_err(|error| {
            if error.kind != krkr_tjs2::TjsErrorKind::ResourcePending {
                runtime
                    .host_mut()
                    .log(&format!("KAG handler `{name}` failed:\n{error}"));
                self.state = KagTaskState::Error {
                    message: error.to_string(),
                };
            }
        })
    }

    fn process_native_fallback_tag(
        &mut self,
        parser: &mut KagParser,
        runtime: &mut Runtime<KrkrHost>,
        owner: ObjectHandle,
        tag: &Tag,
    ) -> Result<TagAction> {
        let Some(native_tag) = NativeFallbackTag::from_name(&tag.tagname) else {
            runtime.host_mut().trace(
                TraceCategory::Kag,
                &format!("kag: no handler for tag `{}`, ignored", tag.tagname),
            );
            return Ok(TagAction::Continue);
        };
        match native_tag {
            NativeFallbackTag::Ch => {
                if let Some(text) = tag.literal_attr("text") {
                    self.message_layer.append_text(text);
                }
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::R => {
                self.message_layer.newline();
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::L => {
                self.message_layer.newline();
                if kag_auto_mode(runtime) {
                    return Ok(self.wait_auto_timer(kag_auto_line_wait(runtime), false));
                }
                Ok(self.wait_click(false))
            }
            NativeFallbackTag::P => {
                self.message_layer.page_break();
                if kag_auto_mode(runtime) {
                    self.message_layer.waiting_for_click = false;
                    self.clear_page_on_click = false;
                    return Ok(self.wait_auto_timer(kag_auto_page_wait(runtime), true));
                }
                Ok(self.wait_click(true))
            }
            NativeFallbackTag::Font => {
                apply_message_font_tag(&mut self.message_layer, tag)?;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::ResetFont => {
                self.message_layer.font = krkr_core::FontSpec::default();
                self.message_layer.style = krkr_core::TextStyle::default();
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Style => {
                apply_message_style_tag(&mut self.message_layer, tag);
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Locate => {
                if let Some(x) = tag_i64(tag, "x")? {
                    self.message_layer.cursor_x = x as i32;
                }
                if let Some(y) = tag_i64(tag, "y")? {
                    self.message_layer.cursor_y = y as i32;
                }
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::PText => {
                if let Some(text) = tag.literal_attr("text") {
                    self.message_layer.append_text(text);
                }
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::WaitClick => Ok(self.wait_click(false)),
            NativeFallbackTag::Wait => Ok(match tag_millis(tag, "time") {
                Some(duration) => self.wait(KagTaskState::WaitingTimer {
                    remaining: duration,
                }),
                None => self.wait_click(false),
            }),
            NativeFallbackTag::Eval => {
                execute_eval_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Trace => {
                execute_trace_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::ClearText => {
                self.message_layer.clear_text();
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Image => {
                if apply_image_tag(runtime, tag)? {
                    Ok(self.wait(KagTaskState::WaitingResource))
                } else {
                    Ok(TagAction::Continue)
                }
            }
            NativeFallbackTag::TitleImage
            | NativeFallbackTag::TitleLogo
            | NativeFallbackTag::TitleEffect => {
                if apply_title_image_tag(runtime, tag)? {
                    Ok(self.wait(KagTaskState::WaitingResource))
                } else {
                    Ok(TagAction::Continue)
                }
            }
            NativeFallbackTag::LayerOptions => {
                apply_layer_options_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::FreeImage => {
                apply_freeimage_tag(runtime, tag);
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Backlay => {
                let layer = tag.literal_attr("layer");
                runtime.host_mut().backlay_kag_layers(layer);
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Rclick => {
                self.apply_right_click_tag(tag);
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::TempSave => {
                let place = tag_i64(tag, "place")?.unwrap_or(0);
                self.temp_snapshots.insert(
                    place,
                    KagTempSnapshot {
                        parser: parser.store(),
                        message_layer: self.message_layer.clone(),
                        state: self.state.clone(),
                        pending_tags: self.pending_tags.clone(),
                        right_click: self.right_click.clone(),
                        clear_page_on_click: self.clear_page_on_click,
                        clear_page_on_timer: self.clear_page_on_timer,
                    },
                );
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::TempLoad => {
                let place = tag_i64(tag, "place")?.unwrap_or(0);
                if let Some(snapshot) = self.temp_snapshots.get(&place).cloned() {
                    parser
                        .restore(snapshot.parser)
                        .map_err(|error| TjsError::runtime(error.to_string()))?;
                    self.message_layer = snapshot.message_layer;
                    self.pending_tags = snapshot.pending_tags;
                    self.right_click = snapshot.right_click;
                    self.clear_page_on_click = snapshot.clear_page_on_click;
                    self.clear_page_on_timer = snapshot.clear_page_on_timer;
                    self.state = snapshot.state;
                }
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Noop => Ok(TagAction::Continue),
            NativeFallbackTag::AddSysHook | NativeFallbackTag::AddSysScript => {
                let Some(name) = tag.literal_attr("name").map(str::to_string) else {
                    return Ok(TagAction::Continue);
                };
                let hook = SystemHook {
                    storage: tag.literal_attr("storage").and_then(non_empty_string),
                    target: tag.literal_attr("target").and_then(non_empty_string),
                    call: match native_tag {
                        NativeFallbackTag::AddSysHook => {
                            kag_bool_attr(tag, "call").unwrap_or(false)
                        }
                        NativeFallbackTag::AddSysScript => false,
                        _ => false,
                    },
                };
                self.system_hooks.insert(name.clone(), hook);
                runtime
                    .host_mut()
                    .log(&format!("KAG registered system hook `{name}`"));
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::SysHook => {
                let Some(name) = tag.literal_attr("name") else {
                    return Ok(TagAction::Continue);
                };
                if self.system_hooks.is_empty() {
                    if let Ok(text) = runtime.host_mut().read_text_storage_for_tjs("custom.ks") {
                        for line in text.lines() {
                            let Some(start) = line
                                .find("[addSysHook")
                                .or_else(|| line.find("[addSysScript"))
                            else {
                                continue;
                            };
                            let command = &line[start..];
                            let Some(hook_name) = hook_attr(command, "name") else {
                                continue;
                            };
                            self.system_hooks.insert(
                                hook_name,
                                SystemHook {
                                    storage: hook_attr(command, "storage"),
                                    target: hook_attr(command, "target"),
                                    call: command.starts_with("[addSysHook")
                                        && command.contains(" call"),
                                },
                            );
                        }
                        runtime.host_mut().log(&format!(
                            "KAG loaded {} system hook declarations",
                            self.system_hooks.len()
                        ));
                    }
                }
                let hook = self.system_hooks.get(name).cloned().or_else(|| {
                    runtime.host().system_hook(name).map(|hook| SystemHook {
                        storage: hook.storage,
                        target: hook.target,
                        call: hook.call,
                    })
                });
                let Some(hook) = hook else {
                    return Ok(TagAction::Continue);
                };
                let mut host = EngineKagHost::for_owner(runtime, owner);
                if hook.call {
                    parser
                        .call_with(hook.storage.as_deref(), hook.target.as_deref(), &mut host)
                        .map_err(|error| TjsError::runtime(error.to_string()))?;
                } else {
                    parser
                        .go_to_with(hook.storage.as_deref(), hook.target.as_deref(), &mut host)
                        .map_err(|error| TjsError::runtime(error.to_string()))?;
                }
                self.pending_tags.clear();
                self.state = KagTaskState::Running;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::SysJump => {
                let target = tag
                    .literal_attr("to")
                    .ok_or_else(|| TjsError::runtime("sysjump requires `to`"))?;
                let storage = if target.ends_with(".ks") {
                    target.to_string()
                } else {
                    format!("{target}.ks")
                };
                let mut host = EngineKagHost::for_owner(runtime, owner);
                parser
                    .load_scenario_with(storage, &mut host)
                    .map_err(|error| match error {
                        krkr_kag::KagError::ResourcePending { storage } => {
                            TjsError::resource_pending(storage)
                        }
                        error => TjsError::runtime(error.to_string()),
                    })?;
                self.pending_tags.clear();
                self.message_layer.clear();
                self.state = KagTaskState::Running;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::GotoStart => {
                if let Some(storage) = parser.cur_storage().map(str::to_string) {
                    parser
                        .set_cur_storage(storage)
                        .map_err(|error| TjsError::runtime(error.to_string()))?;
                    self.message_layer.clear();
                    self.pending_tags.clear();
                    self.state = KagTaskState::Running;
                }
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::LayCount => {
                apply_laycount_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Current => {
                apply_current_tag(runtime, tag);
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Trans => {
                // The `time` attribute is the handler's own clock and every
                // provider clamps it to the reference's 2 ms floor
                // (`TRANSITION_MIN_MILLIS`).  KAG's layer maps a specified `0`
                // to `1` first (`KAGLayer.tjs:198-201`) and the provider then
                // raises it to 2, so an explicit zero is a 2 ms transition and
                // not an exchange.
                let duration = tag_millis(tag, "time")
                    .map(|duration| {
                        duration.max(Duration::from_millis(
                            crate::native::classes::TRANSITION_MIN_MILLIS,
                        ))
                    })
                    .unwrap_or(Duration::ZERO);
                let (mut params, rule_image_upload) = kag_transition_spec(runtime, tag)?;
                // The clock the extrans kernels run on; `0` is "not supplied",
                // which the kernels read as the progress-derived fallback.
                params.duration_millis = duration.as_millis() as f32;
                // KAG's own `[trans]` is `kag.fore.base.beginTransition`
                // (`KAGLayer.tjs`), so the projection owns that layer object
                // when the script has one.
                let page_base =
                    crate::native::classes::kag_layer_object(runtime, "fore", "base");
                runtime
                    .host_mut()
                    .begin_kag_transition(duration, params, rule_image_upload, page_base);
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Wt => {
                if runtime.host().has_active_transition() {
                    Ok(self.wait(KagTaskState::WaitingTransition))
                } else {
                    Ok(TagAction::Continue)
                }
            }
            NativeFallbackTag::PlayBgm => {
                play_kag_audio_tag(
                    runtime,
                    tag,
                    AudioBus::Bgm,
                    AudioLoadPolicy::Streaming,
                    true,
                )?;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::PlaySe => {
                play_kag_audio_tag(
                    runtime,
                    tag,
                    AudioBus::SoundEffect,
                    AudioLoadPolicy::StaticCached,
                    false,
                )?;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::PlayVoice => {
                play_kag_audio_tag(
                    runtime,
                    tag,
                    AudioBus::SoundEffect,
                    AudioLoadPolicy::Streaming,
                    false,
                )?;
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::StopBgm => {
                runtime
                    .host_mut()
                    .trace(TraceCategory::Audio, "kag audio stop: bus=Bgm");
                runtime
                    .host_mut()
                    .queue_audio_command(AudioCommand::StopBus {
                        bus: AudioBus::Bgm,
                        fade_seconds: tag_millis(tag, "time")
                            .unwrap_or(Duration::ZERO)
                            .as_secs_f32(),
                    });
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::StopSe | NativeFallbackTag::StopVoice => {
                runtime
                    .host_mut()
                    .trace(TraceCategory::Audio, "kag audio stop: bus=SoundEffect");
                runtime
                    .host_mut()
                    .queue_audio_command(AudioCommand::StopBus {
                        bus: AudioBus::SoundEffect,
                        fade_seconds: tag_millis(tag, "time")
                            .unwrap_or(Duration::ZERO)
                            .as_secs_f32(),
                    });
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::WaitAudio => Ok(self.wait(KagTaskState::WaitingAudio)),
            NativeFallbackTag::WaitResource => Ok(self.wait(KagTaskState::WaitingResource)),
            NativeFallbackTag::CancelAutoMode => {
                cancel_kag_auto_mode(runtime);
                Ok(TagAction::Continue)
            }
            NativeFallbackTag::Stop => {
                self.state = KagTaskState::Finished;
                Ok(TagAction::Yield(KagYieldReason::Finished))
            }
        }
    }

    fn apply_right_click_tag(&mut self, tag: &Tag) {
        if let Some(enabled) = kag_bool_attr(tag, "enabled") {
            self.right_click.enabled = enabled;
        }
        if let Some(call) = kag_bool_attr(tag, "call") {
            self.right_click.call = call;
            if call {
                self.right_click.jump = false;
            }
        }
        if let Some(jump) = kag_bool_attr(tag, "jump") {
            self.right_click.jump = jump;
            if jump {
                self.right_click.call = false;
            }
        }
        if let Some(storage) = tag.literal_attr("storage") {
            self.right_click.storage = non_empty_string(storage);
        }
        if let Some(target) = tag.literal_attr("target") {
            self.right_click.target = non_empty_string(target);
        }
    }

    fn fire_right_click(&mut self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        if self.parser.is_none() || !self.right_click.enabled {
            return Ok(());
        }
        self.with_parser_for_tjs(runtime, |parser, session, runtime, owner| {
            session.fire_right_click_with_parser(parser, runtime, owner)
        })
    }

    fn fire_right_click_with_parser(
        &mut self,
        parser: &mut KagParser,
        runtime: &mut Runtime<KrkrHost>,
        owner: ObjectHandle,
    ) -> Result<()> {
        if !self.right_click.enabled {
            return Ok(());
        }
        let storage = self.right_click.storage.clone();
        let target = self.right_click.target.clone();
        if storage.is_none() && target.is_none() {
            return Ok(());
        }

        let mut host = EngineKagHost::for_owner(runtime, owner);
        if self.right_click.call {
            parser
                .call_with(storage.as_deref(), target.as_deref(), &mut host)
                .map_err(|error| TjsError::runtime(error.to_string()))?;
        } else if self.right_click.jump {
            parser
                .go_to_with(storage.as_deref(), target.as_deref(), &mut host)
                .map_err(|error| TjsError::runtime(error.to_string()))?;
        } else {
            return Ok(());
        }

        self.pending_tags.clear();
        self.state = KagTaskState::Running;
        self.clear_page_on_click = false;
        self.message_layer.waiting_for_click = false;
        Ok(())
    }

    fn apply_tjs_handler_step(&mut self, tag: Tag, value: Variant) -> Result<TjsTagAction> {
        if matches!(value, Variant::Void) {
            // KRKR's built-in dispatcher uses void in both directions: for a
            // built-in tag it asks the native KAG implementation to continue,
            // while a script-defined unknown tag commonly performs all work
            // in `onUnknownTag` and falls off the end without returning a
            // numeric conductor step.  The latter is a successful, immediate
            // handler completion (not an engine error).
            if NativeFallbackTag::from_name(&tag.tagname).is_some() {
                return Ok(TjsTagAction::NativeFallback);
            }
            return Ok(TjsTagAction::Handled(TagAction::Continue));
        }

        let step = value.to_integer()?;
        if step == TJS_NATIVE_FALLBACK_STEP {
            return Ok(TjsTagAction::NativeFallback);
        }
        Ok(match step {
            0 => TjsTagAction::Handled(TagAction::Continue),
            -5 => {
                self.pending_tags.push_front(tag);
                TjsTagAction::Handled(TagAction::Yield(KagYieldReason::HandlerYield))
            }
            -4 => TjsTagAction::Handled(TagAction::Yield(KagYieldReason::HandlerYield)),
            -3 => {
                self.pending_tags.push_front(tag);
                TjsTagAction::Handled(TagAction::Yield(KagYieldReason::HandlerYield))
            }
            -2 => TjsTagAction::Handled(TagAction::Yield(KagYieldReason::HandlerYield)),
            -1 => {
                self.state = KagTaskState::Finished;
                TjsTagAction::Handled(TagAction::Yield(KagYieldReason::Finished))
            }
            n if n > 0 => {
                self.state = KagTaskState::WaitingTimer {
                    remaining: Duration::from_millis(n as u64),
                };
                TjsTagAction::Handled(TagAction::Yield(KagYieldReason::Waiting(
                    self.state.clone(),
                )))
            }
            _ => TjsTagAction::Handled(TagAction::Yield(KagYieldReason::HandlerYield)),
        })
    }

    fn wait(&mut self, state: KagTaskState) -> TagAction {
        if !matches!(state, KagTaskState::WaitingTimer { .. }) {
            self.clear_page_on_timer = false;
        }
        self.state = state;
        TagAction::Yield(KagYieldReason::Waiting(self.state.clone()))
    }

    fn wait_auto_timer(&mut self, duration: Duration, clear_page_on_timer: bool) -> TagAction {
        if duration.is_zero() {
            self.clear_page_on_timer = false;
            if clear_page_on_timer {
                self.message_layer.clear_text();
            }
            return TagAction::Continue;
        }
        self.clear_page_on_timer = clear_page_on_timer;
        self.wait(KagTaskState::WaitingTimer {
            remaining: duration,
        })
    }

    fn wait_click(&mut self, clear_page_on_click: bool) -> TagAction {
        self.message_layer.waiting_for_click = true;
        self.clear_page_on_click = clear_page_on_click;
        self.wait(KagTaskState::WaitingClick)
    }
}

fn hook_attr(command: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=");
    let start = command.find(&marker)? + marker.len();
    let rest = command[start..].trim_start();
    if let Some(quote) = rest.chars().next().filter(|ch| *ch == '\"' || *ch == '\'') {
        let body = &rest[quote.len_utf8()..];
        let end = body.find(quote).unwrap_or(body.len());
        return Some(body[..end].to_string());
    }
    non_empty_string(rest.trim_end_matches(|ch: char| ch.is_whitespace() || ch == ']' || ch == ';'))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TagAction {
    Continue,
    Yield(KagYieldReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TjsTagAction {
    Handled(TagAction),
    NativeFallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeFallbackTag {
    Ch,
    R,
    L,
    P,
    Font,
    ResetFont,
    Style,
    Locate,
    PText,
    WaitClick,
    Wait,
    Eval,
    Trace,
    AddSysHook,
    AddSysScript,
    SysHook,
    ClearText,
    Image,
    TitleImage,
    TitleLogo,
    TitleEffect,
    LayerOptions,
    FreeImage,
    Backlay,
    Rclick,
    TempSave,
    TempLoad,
    Noop,
    SysJump,
    GotoStart,
    LayCount,
    Current,
    Trans,
    Wt,
    PlayBgm,
    PlaySe,
    PlayVoice,
    StopBgm,
    StopSe,
    StopVoice,
    WaitAudio,
    WaitResource,
    CancelAutoMode,
    Stop,
}

impl NativeFallbackTag {
    fn from_name(tagname: &str) -> Option<Self> {
        Some(match tagname {
            "ch" => Self::Ch,
            "r" => Self::R,
            "l" => Self::L,
            "p" => Self::P,
            "font" | "deffont" => Self::Font,
            "resetfont" => Self::ResetFont,
            "style" => Self::Style,
            "locate" => Self::Locate,
            "ptext" => Self::PText,
            "waitclick" => Self::WaitClick,
            "wait" => Self::Wait,
            "eval" => Self::Eval,
            "trace" => Self::Trace,
            "addSysHook" => Self::AddSysHook,
            "addSysScript" => Self::AddSysScript,
            "syshook" => Self::SysHook,
            "cm" | "ct" | "er" => Self::ClearText,
            "image" => Self::Image,
            "title_image" => Self::TitleImage,
            "title_logo" => Self::TitleLogo,
            "title_effect" => Self::TitleEffect,
            "layopt" | "position" => Self::LayerOptions,
            "freeimage" => Self::FreeImage,
            "backlay" => Self::Backlay,
            "rclick" => Self::Rclick,
            "tempsave" => Self::TempSave,
            "tempload" => Self::TempLoad,
            "commit" | "history" | "defstyle" | "resetstyle" | "ruby" => Self::Noop,
            "sysjump" => Self::SysJump,
            "gotostart" => Self::GotoStart,
            "laycount" => Self::LayCount,
            "current" => Self::Current,
            "trans" => Self::Trans,
            "wt" => Self::Wt,
            "playbgm" => Self::PlayBgm,
            "playse" => Self::PlaySe,
            "playvoice" => Self::PlayVoice,
            "stopbgm" => Self::StopBgm,
            "stopse" => Self::StopSe,
            "stopvoice" => Self::StopVoice,
            "wq" | "wf" | "wb" | "wm" | "ws" => Self::WaitAudio,
            "waitload" | "waittrig" => Self::WaitResource,
            "cancelautomode" => Self::CancelAutoMode,
            "s" => Self::Stop,
            _ => return None,
        })
    }
}

const TJS_NATIVE_FALLBACK_STEP: i64 = -1_000_000;

fn push_unique_handler(candidates: &mut Vec<ObjectHandle>, handler: Option<ObjectHandle>) {
    let Some(handler) = handler else {
        return;
    };
    if !candidates.contains(&handler) {
        candidates.push(handler);
    }
}

fn call_tag_handler(
    runtime: &mut Runtime<KrkrHost>,
    handler: ObjectHandle,
    name: &str,
    args: Vec<Variant>,
) -> Result<Variant> {
    runtime
        .call_object_method(handler, name, args)
        .map_err(|error| {
            if error.to_string().contains("KAG resource is pending:") {
                let storage = error
                    .to_string()
                    .split_once("KAG resource is pending:")
                    .and_then(|(_, value)| value.lines().next())
                    .unwrap_or("resource")
                    .trim()
                    .to_string();
                TjsError::resource_pending(storage)
            } else {
                error
            }
        })
}

fn tjs_to_kag(error: TjsError) -> KagError {
    KagError::host(error.to_string())
}

fn tag_millis(tag: &Tag, name: &str) -> Option<Duration> {
    tag.attr(name)
        .and_then(|value| value.raw().parse::<u64>().ok())
        .map(Duration::from_millis)
}

fn kag_auto_mode(runtime: &Runtime<KrkrHost>) -> bool {
    let Some(kag) = runtime.global_member("kag").object_handle() else {
        return false;
    };
    runtime.object_member(kag, "autoMode").is_truthy()
}

fn kag_auto_line_wait(runtime: &Runtime<KrkrHost>) -> Duration {
    kag_auto_wait(runtime, "autoModeLineWait", 300)
}

fn kag_auto_page_wait(runtime: &Runtime<KrkrHost>) -> Duration {
    kag_auto_wait(runtime, "autoModePageWait", 1000)
}

fn kag_auto_wait(runtime: &Runtime<KrkrHost>, name: &str, default_millis: u64) -> Duration {
    let Some(kag) = runtime.global_member("kag").object_handle() else {
        return Duration::from_millis(default_millis);
    };
    let millis = runtime
        .object_member(kag, name)
        .to_integer()
        .unwrap_or(default_millis as i64)
        .max(0) as u64;
    Duration::from_millis(millis)
}

fn cancel_kag_auto_mode(runtime: &mut Runtime<KrkrHost>) {
    let Some(kag) = runtime.global_member("kag").object_handle() else {
        return;
    };
    runtime.set_object_member(kag, "autoMode", Variant::Integer(0));
}

fn kag_transition_spec(
    runtime: &mut Runtime<KrkrHost>,
    tag: &Tag,
) -> Result<(TransitionParams, Option<ImageUpload>)> {
    let name = tag
        .literal_attr("method")
        .or_else(|| tag.literal_attr("rule").map(|_| "universal"))
        .unwrap_or("crossfade");
    // `Layer.beginTransition` resolves the provider name before it reads any
    // option (`LayerIntf.cpp:6206`) and the lookup is exact: a tag whose method
    // is not registered stops the scenario with the official message instead
    // of degrading to a crossfade.
    let method = crate::native::classes::resolve_transition_method(runtime, name)
        .map_err(|unknown| TjsError::runtime(unknown.message()))?;
    let mut params = TransitionParams {
        method,
        ..TransitionParams::default()
    };
    match params.method {
        TransitionMethod::RotateVanish => {
            params.accel = 2.0;
            params.twist_accel = 2.0;
        }
        TransitionMethod::RotateSwap => {
            params.twist = 1.0;
        }
        _ => {}
    }

    if let Some(value) = kag_f32_attr(tag, "vague")? {
        params.vague = value.max(0.0);
    }
    if let Some(value) = kag_scroll_from_attr(tag, "from")? {
        params.scroll_from = value;
    }
    if let Some(value) = kag_scroll_stay_attr(tag, "stay")? {
        params.scroll_stay = value;
    }
    if let Some(value) = kag_f32_attr(tag, "wavetype")? {
        params.wave_type = value;
    }
    if let Some(value) = kag_f32_attr(tag, "maxh")? {
        params.max_h = value.max(0.0);
    }
    if let Some(value) = kag_f32_attr(tag, "maxomega")? {
        params.max_omega = value.max(0.0);
    }
    if let Some(value) = kag_transition_color_attr(tag, "bgcolor1")? {
        params.bg_color1 = value;
    }
    if let Some(value) = kag_transition_color_attr(tag, "bgcolor2")? {
        params.bg_color2 = value;
    }
    if let Some(value) = kag_f32_attr(tag, "maxsize")? {
        params.max_size = value.max(1.0);
    }
    if let Some(value) = kag_transition_color_attr(tag, "bgcolor")? {
        params.bg_color = value;
    }
    if let Some(value) = kag_f32_attr(tag, "factor")? {
        params.factor = value.max(0.0);
    }
    if let Some(value) = kag_f32_attr(tag, "accel")? {
        params.accel = value;
    }
    if let Some(value) = kag_f32_attr(tag, "twist")? {
        params.twist = value;
    }
    if let Some(value) = kag_f32_attr(tag, "twistaccel")? {
        params.twist_accel = value;
    }
    if let Some(value) = kag_f32_attr(tag, "centerx")? {
        params.center_x = value;
    }
    if let Some(value) = kag_f32_attr(tag, "centery")? {
        params.center_y = value;
    }
    if let Some(value) = kag_f32_attr(tag, "rwidth")? {
        params.ripple_width = value.max(1.0);
    }
    if let Some(value) = kag_f32_attr(tag, "roundness")? {
        params.roundness = value.max(0.01);
    }
    if let Some(value) = kag_f32_attr(tag, "speed")? {
        params.speed = value.max(0.01);
    }
    if let Some(value) = kag_f32_attr(tag, "maxdrift")? {
        params.max_drift = value.max(0.0);
    }

    let rule_image_upload = if params.method == TransitionMethod::Universal {
        match tag.literal_attr("rule") {
            Some(rule) => Some(runtime.host_mut().load_image_storage(rule)?.upload),
            None => None,
        }
    } else {
        None
    };

    Ok((params, rule_image_upload))
}

fn kag_f32_attr(tag: &Tag, name: &str) -> Result<Option<f32>> {
    tag.literal_attr(name)
        .map(|value| {
            parse_kag_number(value)
                .map(|value| value as f32)
                .map_err(|error| {
                    TjsError::runtime(format!("invalid KAG {name} value `{value}`: {error}"))
                })
        })
        .transpose()
}

fn parse_kag_number(value: &str) -> std::result::Result<f64, String> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return i64::from_str_radix(hex, 16)
            .map(|value| value as f64)
            .map_err(|error| error.to_string());
    }
    trimmed.parse::<f64>().map_err(|error| error.to_string())
}

/// `bgcolor`, `bgcolor1` and `bgcolor2` of a `[trans]` tag: a `tjs_uint32`
/// **ARGB** value, the same option the script path reads
/// (`native::classes::object_optional_color`).
///
/// A KAG tag attribute reaches the provider as a string and official converts
/// it with `(tjs_int)`, i.e. a TJS numeric parse (`TJSParseNumber`,
/// `tjsLex.cpp:731-758`, so `0xff000000` is a number) that yields `0` for
/// anything else -- a colour name like `red` is not a number and becomes
/// transparent black.  The top byte is alpha, so `0xff000000` is the spelling
/// for opaque black (`wave.cpp:345`/`:348`, `TVPFillARGB` at `:219`).
fn kag_transition_color_attr(tag: &Tag, name: &str) -> Result<Option<Color>> {
    let Some(value) = tag.literal_attr(name) else {
        return Ok(None);
    };
    let color = parse_kag_number(value).unwrap_or(0.0) as i64;
    Ok(Some(crate::native::classes::argb_color(color)))
}

fn kag_scroll_from_attr(tag: &Tag, name: &str) -> Result<Option<TransitionScrollFrom>> {
    let Some(value) = tag.literal_attr(name) else {
        return Ok(None);
    };
    Ok(Some(match value {
        "left" | "0" => TransitionScrollFrom::Left,
        "top" | "1" => TransitionScrollFrom::Top,
        "right" | "2" => TransitionScrollFrom::Right,
        "bottom" | "3" => TransitionScrollFrom::Bottom,
        _ => {
            return Err(TjsError::runtime(format!(
                "invalid KAG {name} value `{value}`"
            )));
        }
    }))
}

fn kag_scroll_stay_attr(tag: &Tag, name: &str) -> Result<Option<TransitionScrollStay>> {
    let Some(value) = tag.literal_attr(name) else {
        return Ok(None);
    };
    Ok(Some(match value {
        "nostay" | "0" => TransitionScrollStay::NoStay,
        "stayfore" | "1" => TransitionScrollStay::StayDest,
        "stayback" | "2" => TransitionScrollStay::StaySrc,
        _ => {
            return Err(TjsError::runtime(format!(
                "invalid KAG {name} value `{value}`"
            )));
        }
    }))
}

fn play_kag_audio_tag(
    runtime: &mut Runtime<KrkrHost>,
    tag: &Tag,
    bus: AudioBus,
    load_policy: AudioLoadPolicy,
    default_looping: bool,
) -> Result<()> {
    let storage = tag
        .literal_attr("storage")
        .or_else(|| tag.literal_attr("file"))
        .or_else(|| tag.literal_attr("src"))
        .ok_or_else(|| TjsError::runtime(format!("{} requires storage", tag.tagname)))?;
    let looping = kag_bool_attr(tag, "loop").unwrap_or(default_looping)
        || kag_bool_attr(tag, "looping").unwrap_or(false);
    let volume = tag_i64(tag, "volume")?
        .map(|value| (value as f32 / 100.0).clamp(0.0, 1.0))
        .unwrap_or(1.0);
    runtime.host_mut().trace(
        TraceCategory::Audio,
        &format!("kag audio play: storage={storage} bus={bus:?} looping={looping}"),
    );
    runtime
        .host_mut()
        .queue_kag_audio_play(storage, bus, load_policy, looping, volume)?;
    Ok(())
}

fn apply_message_font_tag(message_layer: &mut MessageLayerModel, tag: &Tag) -> Result<()> {
    if let Some(face) = tag.literal_attr("face") {
        message_layer.font.face = face.to_string();
    }
    if let Some(size) = tag_i64(tag, "size")? {
        message_layer.font.height = size.max(1) as f32;
    }
    if let Some(height) = tag_i64(tag, "height")? {
        message_layer.font.height = height.max(1) as f32;
    }
    if let Some(color) = parse_color_attr(tag, "color")? {
        message_layer.style.color = color;
    }
    if let Some(value) = kag_bool_attr(tag, "bold") {
        message_layer.font.bold = value;
    }
    if let Some(value) = kag_bool_attr(tag, "italic") {
        message_layer.font.italic = value;
    }
    if let Some(value) = kag_bool_attr(tag, "underline") {
        message_layer.font.underline = value;
    }
    if let Some(value) = kag_bool_attr(tag, "strikeout") {
        message_layer.font.strikeout = value;
    }
    Ok(())
}

fn apply_message_style_tag(message_layer: &mut MessageLayerModel, tag: &Tag) {
    if let Some(value) = kag_bool_attr(tag, "bold") {
        message_layer.font.bold = value;
    }
    if let Some(value) = kag_bool_attr(tag, "italic") {
        message_layer.font.italic = value;
    }
    if let Some(value) = kag_bool_attr(tag, "underline") {
        message_layer.font.underline = value;
    }
    if let Some(value) = kag_bool_attr(tag, "strikeout") {
        message_layer.font.strikeout = value;
    }
}

fn parse_color_attr(tag: &Tag, name: &str) -> Result<Option<[u8; 4]>> {
    let Some(value) = tag.literal_attr(name) else {
        return Ok(None);
    };
    let color = match value {
        "black" => 0x000000,
        "white" => 0xffffff,
        "red" => 0xff0000,
        "green" => 0x00ff00,
        "blue" => 0x0000ff,
        value => {
            let value = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .or_else(|| value.strip_prefix('#'))
                .unwrap_or(value);
            i64::from_str_radix(value, 16).map_err(|error| {
                TjsError::runtime(format!("invalid KAG {name} color `{value}`: {error}"))
            })?
        }
    };
    Ok(Some([
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
        255,
    ]))
}

fn execute_eval_tag(runtime: &mut Runtime<KrkrHost>, tag: &Tag) -> Result<()> {
    let expression = tag
        .literal_attr("exp")
        .ok_or_else(|| TjsError::runtime("KAG eval tag requires exp"))?;
    let result = execute_expression_on_runtime(runtime, "kag eval", expression);
    if result.is_ok() {
        register_kag_layer_slots_from_tjs(runtime);
    }
    result.map(|_| ())
}

fn execute_trace_tag(runtime: &mut Runtime<KrkrHost>, tag: &Tag) -> Result<()> {
    let Some(expression) = tag.literal_attr("exp") else {
        return Ok(());
    };
    let value = execute_expression_on_runtime(runtime, "kag trace", expression)?;
    runtime
        .host_mut()
        .log(&format!("KAG trace `{expression}` => {value}"));
    Ok(())
}

fn engine_key_vk_code(key: EngineKey) -> Option<i64> {
    match key {
        EngineKey::Escape => Some(0x1b),
        EngineKey::Enter => Some(0x0d),
        EngineKey::Space => Some(0x20),
        EngineKey::Tab => Some(0x09),
        EngineKey::Left => Some(0x25),
        EngineKey::Up => Some(0x26),
        EngineKey::Right => Some(0x27),
        EngineKey::Down => Some(0x28),
        EngineKey::PageUp => Some(0x21),
        EngineKey::PageDown => Some(0x22),
        EngineKey::Backspace => Some(0x08),
        EngineKey::Delete => Some(0x2e),
        EngineKey::Shift => Some(0x10),
        EngineKey::Control => Some(0x11),
        EngineKey::Alt => Some(0x12),
        EngineKey::Character(ch) if ch.is_ascii() => Some(ch.to_ascii_uppercase() as i64),
        EngineKey::Character(_) | EngineKey::Other => None,
    }
}

fn pointer_button_vk_code(button: PointerButton) -> Option<i64> {
    match button {
        PointerButton::Primary => Some(0x01),
        PointerButton::Secondary => Some(0x02),
        PointerButton::Middle => Some(0x04),
        PointerButton::Other(_) => None,
    }
}

fn install_system_metrics(runtime: &mut Runtime<KrkrHost>, metrics: SystemMetrics) {
    let Some(system) = runtime.global_member("System").object_handle() else {
        return;
    };
    for (name, value) in [
        ("screenWidth", metrics.screen_width),
        ("screenHeight", metrics.screen_height),
        ("desktopLeft", metrics.desktop_left),
        ("desktopTop", metrics.desktop_top),
        ("desktopWidth", metrics.desktop_width),
        ("desktopHeight", metrics.desktop_height),
    ] {
        runtime.set_object_member(system, name, Variant::Integer(value.max(0)));
    }
}

fn ensure_object_member(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> ObjectHandle {
    if let Some(handle) = runtime.object_member(object, name).object_handle() {
        return handle;
    }
    let handle = runtime.alloc_ordinary_object();
    runtime.set_object_member(object, name, Variant::Object(handle));
    handle
}

fn object_member_is_false(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> bool {
    matches!(runtime.object_member(object, name), Variant::Integer(0))
}

fn volume2_to_kag_percent(volume2: i64) -> i64 {
    (volume2.clamp(0, 100000) + 500) / 1000
}

fn object_positive_i64(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Option<i64> {
    match runtime.object_member(object, name) {
        Variant::Void => None,
        value => value.to_integer().ok().filter(|value| *value > 0),
    }
}

fn has_window_state_member(runtime: &Runtime<KrkrHost>, object: ObjectHandle) -> bool {
    ["innerWidth", "width", "fullScreen"]
        .iter()
        .any(|name| !matches!(runtime.object_member(object, name), Variant::Void))
}

fn apply_image_tag(runtime: &mut Runtime<KrkrHost>, tag: &Tag) -> Result<bool> {
    let storage = tag
        .literal_attr("storage")
        .ok_or_else(|| TjsError::runtime("KAG image tag requires storage"))?;
    let (page, layer_name) = kag_target(runtime, tag);
    let has_explicit_width = tag.literal_attr("width").is_some();
    let has_explicit_height = tag.literal_attr("height").is_some();
    let request = ImageLoadRequest {
        owner: None,
        target: ImageLoadTarget::Kag {
            page,
            layer: layer_name,
        },
        storage: storage.to_string(),
        visible: kag_bool_attr(tag, "visible").unwrap_or(true),
        left: tag_i64(tag, "left")?,
        top: tag_i64(tag, "top")?,
        width: tag_i64(tag, "width")?,
        height: tag_i64(tag, "height")?,
        opacity: tag_i64(tag, "opacity")?,
        z_order: tag_i64(tag, "zorder")?
            .map(|value| value.clamp(i32::MIN as i64, i32::MAX as i64) as i32),
    };
    match runtime.host_mut().request_image_load(request.clone())? {
        ImageLoadState::Ready(mut completion) => {
            if has_explicit_width {
                completion.request.width = request.width;
            }
            if has_explicit_height {
                completion.request.height = request.height;
            }
            apply_completed_image_load(runtime, *completion)?;
            Ok(false)
        }
        ImageLoadState::Pending => Ok(true),
    }
}

/// Materialize KRKR's title-screen image tags as ordinary KAG layers.
///
/// `title_image`, `title_logo` and `title_effect` are supplied by the stock
/// system conductor rather than the generic KAG tag set.  They use `file`
/// instead of `storage` and express the layer order through `zorder`; the
/// web/native host can render them with the same image upload path as a normal
/// `[image]` tag.  Keeping the request asynchronous also preserves the
/// resource-pending/yield semantics used by remote web packages.
fn apply_title_image_tag(runtime: &mut Runtime<KrkrHost>, tag: &Tag) -> Result<bool> {
    let storage = tag
        .literal_attr("file")
        .or_else(|| tag.literal_attr("storage"))
        .ok_or_else(|| TjsError::runtime("KAG title image tag requires file"))?;
    let (page, _) = kag_target(runtime, tag);
    let default_zorder = match tag.tagname.as_str() {
        "title_logo" => 120,
        "title_effect" => 110,
        _ => 100,
    };
    // Keep stable layer identities so a title redraw replaces the previous
    // image instead of accumulating roots.  The host gives these special
    // names explicit z-orders: the opaque background must be below the
    // native `syspage` UI, while the logo/effect can sit above it.
    let zorder = tag_i64(tag, "zorder")?.unwrap_or(default_zorder);
    let layer_name = match tag.tagname.as_str() {
        "title_logo" => "__title_logo",
        "title_effect" => "__title_effect",
        _ => "__title_image",
    }
    .to_string();
    let left = match tag_i64(tag, "xpos")? {
        Some(value) => Some(value),
        None => tag_i64(tag, "left")?,
    };
    let top = match tag_i64(tag, "ypos")? {
        Some(value) => Some(value),
        None => tag_i64(tag, "top")?,
    };
    let request = ImageLoadRequest {
        owner: None,
        target: ImageLoadTarget::Kag {
            page: page.clone(),
            layer: layer_name.clone(),
        },
        storage: storage.to_string(),
        visible: kag_bool_attr(tag, "visible").unwrap_or(true),
        left,
        top,
        width: tag_i64(tag, "width")?,
        height: tag_i64(tag, "height")?,
        opacity: tag_i64(tag, "opacity")?,
        z_order: Some(zorder.clamp(i32::MIN as i64, i32::MAX as i64) as i32),
    };
    runtime.host_mut().log(&format!(
        "KAG title image request tag={} storage={} page={} layer={} zorder={}",
        tag.tagname, request.storage, page, layer_name, zorder
    ));
    match runtime.host_mut().request_image_load(request.clone())? {
        ImageLoadState::Ready(completion) => {
            apply_completed_image_load(runtime, *completion)?;
            Ok(false)
        }
        ImageLoadState::Pending => Ok(true),
    }
}

fn apply_layer_options_tag(runtime: &mut Runtime<KrkrHost>, tag: &Tag) -> Result<()> {
    let (page, layer_name) = kag_target(runtime, tag);
    runtime
        .host_mut()
        .mutate_kag_layer(&page, &layer_name, |layer| {
            apply_tag_geometry(layer, tag)?;
            if let Some(visible) = kag_bool_attr(tag, "visible") {
                layer.visible = visible;
            }
            Ok(())
        })
}

fn apply_freeimage_tag(runtime: &mut Runtime<KrkrHost>, tag: &Tag) {
    let (page, layer_name) = kag_target(runtime, tag);
    runtime
        .host_mut()
        .mutate_kag_layer(&page, &layer_name, |layer| {
            layer.clear_image();
            layer.visible = false;
        });
    runtime
        .host_mut()
        .clear_kag_layer_image_storage(&page, &layer_name);
}

fn apply_current_tag(runtime: &mut Runtime<KrkrHost>, tag: &Tag) {
    let page = kag_page_attr(tag)
        .map(str::to_string)
        .unwrap_or_else(|| runtime.host().current_kag_page().to_string());
    let layer_name = tag
        .literal_attr("layer")
        .map(str::to_string)
        .unwrap_or_else(|| runtime.host().current_kag_layer().to_string());
    runtime
        .host_mut()
        .set_current_kag_layer(page.clone(), layer_name.clone());
    runtime.host_mut().ensure_kag_layer(&page, &layer_name);
    runtime
        .host_mut()
        .log(&format!("KAG current layer set to {page}:{layer_name}"));
    runtime
        .host_mut()
        .mutate_kag_layer(&page, &layer_name, |layer| {
            layer.visible = kag_bool_attr(tag, "visible").unwrap_or(layer.visible);
        });
}

fn apply_tag_geometry(layer: &mut krkr_core::LayerNode, tag: &Tag) -> Result<()> {
    if let Some(value) = tag_i64(tag, "left")? {
        layer.left = value as f32;
    }
    if let Some(value) = tag_i64(tag, "top")? {
        layer.top = value as f32;
    }
    if let Some(value) = tag_i64(tag, "width")? {
        layer.width = value.max(0) as f32;
    }
    if let Some(value) = tag_i64(tag, "height")? {
        layer.height = value.max(0) as f32;
    }
    if let Some(value) = tag_i64(tag, "opacity")? {
        layer.opacity = value.clamp(0, 255) as u8;
    }
    Ok(())
}

fn kag_target(runtime: &Runtime<KrkrHost>, tag: &Tag) -> (String, String) {
    let page = kag_page_attr(tag)
        .map(str::to_string)
        .unwrap_or_else(|| runtime.host().current_kag_page().to_string());
    let layer = tag
        .literal_attr("layer")
        .map(str::to_string)
        .unwrap_or_else(|| runtime.host().current_kag_layer().to_string());
    (page, layer)
}

fn kag_page_attr(tag: &Tag) -> Option<&str> {
    match tag.literal_attr("page") {
        Some("back") | Some("background") => Some("back"),
        Some("fore") | Some("foreground") => Some("fore"),
        Some(_) => Some("fore"),
        None => None,
    }
}

fn kag_bool_attr(tag: &Tag, name: &str) -> Option<bool> {
    match tag.literal_attr(name)? {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn tag_i64(tag: &Tag, name: &str) -> Result<Option<i64>> {
    tag.literal_attr(name)
        .map(|value| {
            value.parse::<i64>().map_err(|error| {
                TjsError::runtime(format!("invalid KAG {name} value `{value}`: {error}"))
            })
        })
        .transpose()
}

fn non_empty_string(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn apply_laycount_tag(runtime: &mut Runtime<KrkrHost>, tag: &Tag) -> Result<()> {
    if let Some(layers) = tag_i64(tag, "layers")? {
        for index in 0..layers.max(0) {
            runtime
                .host_mut()
                .ensure_kag_layer("fore", &index.to_string());
        }
    }
    if let Some(messages) = tag_i64(tag, "messages")? {
        for index in 0..messages.max(0) {
            runtime
                .host_mut()
                .ensure_kag_layer("fore", &format!("message{index}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::{Path, PathBuf},
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use krkr_assets::ProjectStorage;
    use krkr_core::Size;
    use krkr_kag::{Attribute, AttributeValue};
    use krkr_tjs2::{
        Result,
        runtime::{Runtime, Variant},
    };

    use super::*;
    use crate::{
        KrkrHost, KrkrPlugin,
        plugin_api::{ResourceStream, StorageMediaProvider, storage},
    };

    #[test]
    fn engine_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<KrkrEngine>();
    }

    #[test]
    fn installs_core_tjs_and_tvp_globals() {
        let engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        for name in [
            "Array",
            "Dictionary",
            "Date",
            "Math",
            "Exception",
            "RegExp",
            "Debug",
            "System",
            "Storages",
            "Plugins",
            "KAGParser",
            "Scripts",
            "Window",
            "Layer",
            "Bitmap",
            "WaveSoundBuffer",
        ] {
            assert!(
                !matches!(engine.tjs_runtime().global_member(name), Variant::Void),
                "{name} should be registered"
            );
        }
    }

    #[test]
    fn layer_paint_dispatches_script_extender_before_native_placeholder() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class PaintExtender extends Layer {
                    function onPaint() {
                        global.paintCalls++;
                        super.onPaint(...);
                    }
                }
                global.paintCalls = 0;
                global.window = new Window();
                global.layer = new Layer(window, null);
                (PaintExtender incontextof layer)();
                layer.callOnPaint = 1;
                "#,
            )
            .expect("install paint extender");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("dispatch layer paint");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "paintCalls")
                .expect("paint call count"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn transition_policy_immediate_applies_kag_transition_synchronously() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .host_mut()
            .set_transition_policy(crate::TransitionPolicy::Immediate);
        engine.host_mut().mutate_kag_layer("back", "base", |layer| {
            layer.visible = true;
            layer.width = 10.0;
            layer.height = 10.0;
        });

        engine.host_mut().begin_kag_transition(
            Duration::from_millis(1000),
            TransitionParams::default(),
            None,
            None,
        );

        assert!(!engine.host().has_active_transition());
        assert!(
            engine
                .host()
                .kag_layer("fore", "base")
                .is_some_and(|layer| layer.visible && layer.width == 10.0)
        );
    }

    #[test]
    fn transition_policy_immediate_completes_native_transition_callback() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .host_mut()
            .set_transition_policy(crate::TransitionPolicy::Immediate);
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.win = new Window();
                global.source = new Layer(win);
                global.dest = new Layer(win);
                win.completed = 0;
                dest.visible = true;
                source.visible = true;
                dest.onTransitionCompleted = function(dest, src) {
                    win.completed++;
                };
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                "#,
            )
            .expect("begin transition");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("finish immediate transition");

        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "win.completed")
                .expect("completed"),
            Variant::Integer(1)
        );
    }

    /// The transition-completed event hands the destination and the source out
    /// as `tTJSVariant(TransDestObj, TransDestObj)` /
    /// `(TransSrcObj, TransSrcObj)` (`LayerIntf.cpp:6411-6418`), so a handler's
    /// `===` against the layers the script holds is true.
    #[test]
    fn transition_completed_hands_out_self_bound_layers() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .host_mut()
            .set_transition_policy(crate::TransitionPolicy::Immediate);
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.win = new Window();
                global.source = new Layer(win);
                global.dest = new Layer(win);
                global.record = "";
                dest.visible = true;
                source.visible = true;
                dest.onTransitionCompleted = function(destArg, srcArg) {
                    global.record = "" + (destArg === global.dest) + ":" +
                        (srcArg === global.source) + ":" + (destArg === srcArg);
                };
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                "#,
            )
            .expect("begin transition");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("finish immediate transition");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "global.record")
                .expect("record"),
            Variant::String("1:1:0".to_string())
        );
    }

    #[test]
    fn scripts_eval_runs_in_engine() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "Math.abs(-4)")
                .expect("eval"),
            Variant::Real(4.0)
        );
        assert_eq!(
            engine
                .execute_script("inline.tjs", r#"return Scripts.eval("1 + 2");"#)
                .expect("script"),
            Variant::Integer(3)
        );
    }

    #[test]
    fn system_paths_are_injected_by_the_platform_host() {
        let mut engine = KrkrEngine::new(EngineConfig {
            system_paths: SystemPaths {
                exe_path: "game://root/".to_string(),
                data_path: "save://profile/".to_string(),
                personal_path: "user://".to_string(),
                app_data_path: "app://".to_string(),
            },
            ..EngineConfig::default()
        })
        .expect("engine");

        let paths = engine
            .execute_script(
                "inline.tjs",
                r#"
                return System.exePath + "|" + System.dataPath + "|" +
                    System.personalPath + "|" + System.appDataPath;
                "#,
            )
            .expect("system paths");

        assert_eq!(
            paths,
            Variant::String("game://root/|save://profile/|user://|app://".to_string())
        );
    }

    #[test]
    fn scripts_eval_and_exec_honor_the_optional_context_argument() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(
            engine
                .execute_script(
                    "inline.tjs",
                    r#"
                    var target = new Dictionary();
                    target.result = 0;
                    Scripts.eval("this.result = 42", "context.tjs", 0, target);
                    Scripts.exec("this.result += 3;", "context.tjs", 0, target);
                    return target.result;
                    "#,
                )
                .expect("contextual script execution"),
            Variant::Integer(45)
        );
    }

    /// GINKA's `title.ks` inline-string path, reduced: `drawTitle` builds a
    /// `%[file: ...]` Dictionary and hands it to KAGEX's
    /// `applyInlineStringVariableExtract`, which wraps the format into an
    /// `@'...'` source and runs `Scripts.eval(source, void, void, context)`.
    /// The `${GetBgmTitleImageFile(file)}` placeholder is a bare call of a
    /// *global* function with the Dictionary as the eval context, so the call
    /// has to be the reference's direct `VM_CALLD` on the `%-2` proxy: a
    /// `FuncCall` miss walks past the Dictionary to the global object
    /// (`tTJSDictionaryObject::FuncCall` keeps `TJS_E_MEMBERNOTFOUND`,
    /// `tjsDictionary.cpp:713-722`), while a property read would stop there
    /// with void and the call would die as
    /// `Cannot convert the variable type ((void) to Object)`.  The trailing
    /// empty `${}` is the sentinel the game appends and must evaluate to
    /// nothing.
    #[test]
    fn scripts_eval_resolves_a_bare_global_call_with_a_dictionary_context() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(
            engine
                .execute_script(
                    "inline.tjs",
                    r#"
                    function GetBgmTitleImageFile(file) { return file; }
                    var context = new Dictionary();
                    context.file = "bgm001";
                    return Scripts.eval("@'bgmtitle_${GetBgmTitleImageFile(file)}${}'", void, void, context);
                    "#,
                )
                .expect("inline string eval"),
            Variant::String("bgmtitle_bgm001".to_string())
        );
    }

    #[test]
    fn scripts_object_keys_match_scriptsex_enumeration_shape() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var d = new Dictionary();
                d.b = 2;
                d.a = 1;
                var keys = Scripts.getObjectKeys(d);
                return keys.join(",") + ":" + Scripts.getObjectCount(d);
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("a,b:2".to_string()));
    }

    #[test]
    fn scripts_foreach_visits_array_indices_and_dictionary_members() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.foreachOutput = "";
                Scripts.foreach(["a", "b"], function(index, value, suffix) {
                    global.foreachOutput += index + "=" + value + suffix;
                }, ";");
                var values = new Dictionary();
                values.name = "kirakira";
                Scripts.foreach(values, function(key, value) {
                    global.foreachOutput += key + "=" + value;
                });
                return global.foreachOutput;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("0=a;1=b;name=kirakira".to_string()));
    }

    #[test]
    fn scripts_foreach_stops_on_and_returns_a_non_void_callback_result() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.foreachVisited = "";
                var result = Scripts.foreach([10, 20, 30], function(index, value) {
                    global.foreachVisited += value + ",";
                    if(index == 1) return "stop";
                });
                return global.foreachVisited + result;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("10,20,stop".to_string()));
    }

    #[test]
    fn scripts_foreach_preserves_anonymous_function_this_context() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class Collector {
                    var output = "";
                    function collect() {
                        Scripts.foreach(["a", "b"], function(index, value) {
                            this.output += index + value;
                        });
                        return output;
                    }
                }
                return (new Collector()).collect();
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("0a1b".to_string()));
    }

    #[test]
    fn scripts_equal_struct_compares_nested_arrays_and_dictionaries() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var shared = %[name: "kirakira"];
                return "" +
                    Scripts.equalStruct(1, 1) + ":" +
                    Scripts.equalStruct(1, "1") + ":" +
                    Scripts.equalStruct([1, %[value: [2, 3]]], [1, %[value: [2, 3]]]) + ":" +
                    Scripts.equalStruct([1, 2], [1, 3]) + ":" +
                    Scripts.equalStruct(%[a: 1], %[a: 1, b: 2]) + ":" +
                    Scripts.equalStruct(shared, shared);
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1:0:1:0:0:1".to_string()));
    }

    #[test]
    fn dictionary_save_struct_persists_to_system_data_path() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project root");
        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var path = System.dataPath + "vars.ksd";
                var saved = %[];
                saved.answer = 42;
                saved.name = "kirakira";
                (Dictionary.saveStruct incontextof saved)(path, "");
                var loaded = Scripts.evalStorage(path);
                return loaded.answer + ":" + loaded.name;
                "#,
            )
            .expect("save and load");

        assert_eq!(result, Variant::String("42:kirakira".to_string()));
        assert!(root.join("savedata/vars.ksd").is_file());
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `SaveStruct` serializes what `tTJSVariantClosure::SelectObjectNoAddRef`
    /// selects -- ObjThis when there is one (`tjsVariant.h:194`,
    /// `tjsDictionary.cpp:457`) -- so a bound array method serializes the array
    /// behind it rather than the function object.
    #[test]
    fn save_struct_serializes_the_object_behind_a_bound_method() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project root");
        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var list = [1, 2, 3];
                var holder = %[];
                holder.entry = list.push;
                var path = System.dataPath + "bound.ksd";
                (Dictionary.saveStruct incontextof holder)(path, "c");
                var loaded = Scripts.evalStorage(path, "c");
                return loaded.entry.count + ":" + loaded.entry[2];
                "#,
            )
            .expect("save and load");

        assert_eq!(result, Variant::String("3:3".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn structured_persistence_supports_krkr_modes_and_binary_format() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project root");
        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var textPath = System.dataPath + "vars_c.ksd";
                var zipPath = System.dataPath + "vars_z.ksd";
                var binPath = System.dataPath + "vars_b.ksd";
                var saved = %[];
                saved.answer = 42;
                saved.child = %[];
                saved.child.name = "nested";
                saved.list = new Array();
                saved.list.add(7);
                saved.list.add("item");
                (Dictionary.saveStruct incontextof saved)(textPath, "c");
                (Dictionary.saveStruct incontextof saved)(zipPath, "z1");
                (Dictionary.saveStruct incontextof saved)(binPath, "b");
                var c = Scripts.evalStorage(textPath, "c");
                var z = Scripts.evalStorage(zipPath, "z1");
                var be = Scripts.evalStorage(binPath, "b");
                var b = new Dictionary();
                (Dictionary.loadStruct incontextof b)(binPath, "b");
                return c.child.name + ":" + z.list[1] + ":" + b.answer + ":" + be.child.name;
                "#,
            )
            .expect("save and load modes");

        assert_eq!(result, Variant::String("nested:item:42:nested".to_string()));
        let bytes = fs::read(root.join("savedata/vars_b.ksd")).expect("binary struct");
        assert!(bytes.starts_with(b"KBAD100\0"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn structured_persistence_writes_krkr2_struct_details() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project root");
        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        engine
            .execute_script(
                "inline.tjs",
                r#"
                var textPath = System.dataPath + "vars_text.ksd";
                var binPath = System.dataPath + "vars_bin.ksd";
                var saved = %["negative" => -1, "real" => 1.5];
                saved.list = [1, 2];
                (Dictionary.saveStruct incontextof saved)(textPath, "");
                (Dictionary.saveStruct incontextof %["negative" => -1])(binPath, "b");
                "#,
            )
            .expect("save structs");

        let bytes = fs::read(root.join("savedata/vars_text.ksd")).expect("text struct");
        assert_eq!(&bytes[0..2], &[0xff, 0xfe]);
        let units = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        let text = String::from_utf16(&units).expect("utf16 struct");
        assert!(text.contains("\"real\" => 0x1.8000000000000p0 /* 1.5 */"));
        assert!(!text.contains(",\n]"));

        let bytes = fs::read(root.join("savedata/vars_bin.ksd")).expect("binary struct");
        assert!(bytes.starts_with(b"KBAD100\0"));
        assert!(
            bytes.ends_with(&[0xd0, 0xff]),
            "negative integers must use KRKR2-compatible int8 encoding"
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn layer_save_layer_image_writes_krkr_bmp24_thumbnail() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project root");
        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.setImageSize(2, 1);
                layer.fillRect(0, 0, 2, 1, 0x204080);
                layer.saveLayerImage(System.dataPath + "thumb.bmp", "bmp24");
                "#,
            )
            .expect("save thumbnail");

        let bytes = fs::read(root.join("savedata/thumb.bmp")).expect("bmp");
        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(bytes.len(), 54 + 8);
        assert_eq!(u16::from_le_bytes([bytes[28], bytes[29]]), 24);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn layer_thumbnail_pipeline_writes_scaled_piled_bmp24() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project root");
        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        engine
            .execute_script(
                "inline.tjs",
                r#"
                var base = new Layer();
                base.visible = true;
                base.setSize(4, 2);
                base.setImageSize(4, 2);
                base.fillRect(0, 0, 4, 2, 0xff0000ff);

                var child = new Layer(null, base);
                child.visible = true;
                child.setPos(2, 0);
                child.setSize(2, 2);
                child.setImageSize(2, 2);
                child.fillRect(0, 0, 2, 2, 0xffff0000);

                var snapshot = new Layer();
                snapshot.setImageSize(4, 2);
                snapshot.face = dfAlpha;
                snapshot.piledCopy(0, 0, base, 0, 0, 4, 2);

                var thumb = new Layer();
                thumb.setImageSize(4, 1);
                thumb.face = dfAlpha;
                thumb.stretchCopy(
                    0, 0, 4, 1, snapshot,
                    0, 0, snapshot.imageWidth, snapshot.imageHeight, stLinear);
                thumb.saveLayerImage(System.dataPath + "thumb-pipeline.bmp", "bmp24");
                "#,
            )
            .expect("save thumbnail");

        let bytes = fs::read(root.join("savedata/thumb-pipeline.bmp")).expect("bmp");
        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(bytes.len(), 54 + 12);
        // The pile is the opaque blue base with the child alpha-blended over its
        // right half (`BltImage` picks `bmAlpha` for an `ltAlpha` child on a
        // non-alpha destination), whose `alpha_blend` packed rounding lands a
        // full-alpha blend on 254 (`blend_functor_c.h:64`). The vertical 2:1
        // shrink resamples each column to the exact average of its two rows:
        // blue, blue, 254-red, 254-red (BGR24).
        assert_eq!(
            &bytes[54..66],
            &[255, 0, 0, 255, 0, 0, 0, 0, 254, 0, 0, 254]
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn window_inner_size_updates_preferred_viewport_size() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Window();
                kag.setInnerSize(1280, 720);
                return kag.width + ":" + kag.height + ":" + kag.innerWidth + ":" + kag.innerHeight;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1280:720:1280:720".to_string()));
        assert_eq!(
            engine.preferred_viewport_size(),
            Some(Size::new(1280.0, 720.0))
        );
    }

    #[test]
    fn window_fullscreen_tracks_runtime_window_state() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Window();
                var initial = kag.fullScreen;
                kag.fullScreen = true;
                var enabled = kag.fullScreen;
                kag.fullScreen = false;
                return initial + ":" + enabled + ":" + kag.fullScreen;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("0:1:0".to_string()));
        assert!(!engine.window_fullscreen());

        engine
            .execute_script("inline.tjs", "kag.fullScreen = true;")
            .expect("script");
        assert!(engine.window_fullscreen());
    }

    #[test]
    fn window_fullscreen_falls_back_to_main_window() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                window.fullScreen = true;
                // `Window.mainWindow` hands out `tTJSVariant(dsp, dsp)`
                // (`WindowIntf.cpp:1803`), so the comparison against the object
                // itself is the identity comparison TJS2's `==` performs; `===`
                // would also compare ObjThis, which the engine's `new` does not
                // bind yet (`call_function`'s `VM_NEW` result is a plain
                // object here).
                return Window.mainWindow == window;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::Integer(1));
        assert!(engine.window_fullscreen());
    }

    #[test]
    fn window_add_tracks_children_and_primary_layer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.layer = new Layer(window, null);
                window.add(layer);
                window.add(layer);
                "#,
            )
            .expect("add");

        let window = object_handle(&engine, "window");
        let layer = object_handle(&engine, "layer");
        // `Window.children` has no official counterpart (M2 §5a): the child
        // registry is engine-internal, reached through the host.
        assert_eq!(engine.host().native_window_children(window), vec![layer]);
        assert_eq!(
            engine.host().native_window_primary_layer(window),
            Some(layer)
        );
        assert_eq!(engine.host().native_window_focused_layer(window), None);

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var before =
                    (window.primaryLayer == layer) + ":" +
                    (window.focusedLayer === null) + ":" +
                    (Window.mainWindow == window);
                window.remove(layer);
                var noLayer = "";
                // `GetPrimaryLayer()` throws `TVPWindowHasNoLayer`
                // (`WindowIntf.cpp:1862`, "Window has no layer") once the
                // window's primary layer is gone.
                try { window.primaryLayer; } catch (e) { noLayer = e.message; }
                return before + ":" + noLayer + ":" + (window.focusedLayer === null);
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String("1:1:1:Window has no layer:1".to_string())
        );
        assert!(engine.host().native_window_children(window).is_empty());
        assert_eq!(engine.host().native_window_primary_layer(window), None);
        assert_eq!(engine.host().native_window_focused_layer(window), None);
        assert_eq!(engine.host().native_layer_window(layer), Some(window));
    }

    /// `Window.add`/`remove` take their argument through
    /// `AsObjectClosureNoAddRef()` (`WindowIntf.cpp:829-847`), so the
    /// self-bound values this engine hands out for `parent`/`children[i]` name
    /// the layer they bind.
    #[test]
    fn window_add_and_remove_accept_self_bound_layer_values() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.root = new Layer(window, null);
                global.child = new Layer(window, root);
                // `child.parent` and `root.children[0]` are both
                // `tTJSVariant(dsp, dsp)` (`LayerIntf.cpp:8490`, `:665`).
                window.add(child.parent);
                window.add(root.children[0]);
                "#,
            )
            .expect("add");

        let window = object_handle(&engine, "window");
        let root = object_handle(&engine, "root");
        let child = object_handle(&engine, "child");
        assert_eq!(
            engine.host().native_window_children(window),
            vec![root, child]
        );
        assert_eq!(engine.host().native_layer_window(root), Some(window));
        assert_eq!(engine.host().native_layer_window(child), Some(window));

        engine
            .execute_script("inline.tjs", "window.remove(child.parent);")
            .expect("remove");
        assert_eq!(engine.host().native_window_children(window), vec![child]);
        assert_eq!(engine.host().native_window_primary_layer(window), None);
    }

    #[test]
    fn layer_children_is_a_cached_array_rebuilt_from_the_tree() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                var parent = new Layer(window, null);
                var child = new Layer(window, parent);
                var first = parent.children;
                var joined = first.count;
                child.parent = null;
                // `GetChildrenArrayObjectNoAddRef` (`LayerIntf.cpp:630`):
                // the same array object is handed out, rebuilt in place.
                var detached = parent.children.count;
                var stable = (first === parent.children) + ":" + first.count;
                // Script writes into the array never reach the tree and are
                // dropped by the next rebuild (`LayerIntf.cpp:659`).
                first.push(99);
                var polluted = first.count;
                var sibling = new Layer(window, parent);
                return joined + ":" + detached + ":" + stable + ":" + polluted +
                    ":" + parent.children.count;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1:0:1:0:1:1".to_string()));
    }

    #[test]
    fn layer_children_survives_reconstruction_and_rejects_writes() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class ShellLayer extends Layer {
                    // Two `super.Layer()` calls: the first ownerless one makes
                    // the native instance, the second joins the real parent.
                    function ShellLayer(window, parent) {
                        super.Layer();
                        super.Layer(window, parent);
                    }
                }

                var window = new Window();
                var parent = new ShellLayer(window, null);
                var first = parent.children;
                var before = first.count;
                var child = new Layer(window, parent);
                // Only a `parent.children` read rebuilds the cached array
                // (`GetChildrenArrayObjectNoAddRef`, `LayerIntf.cpp:630`), and
                // the rebuild reaches a reference the script already holds.
                var lazy = first.count;
                var refreshed = parent.children.count;
                var stable = (first === parent.children) + ":" + first.count;
                // `TJS_DENY_NATIVE_PROP_SETTER` (`LayerIntf.cpp:8532`): the
                // write fails with `TJS_E_ACCESSDENYED` (-1007) and the
                // official text.
                var denied = "";
                try { parent.children = 0; } catch (e) { denied = e.message; }
                return before + ":" + lazy + ":" + refreshed + ":" + stable +
                    ":" + denied;
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String(
                "0:0:1:1:1:Invalid operation for Read-only or Write-only property".to_string()
            )
        );
    }

    /// Every official read-only `tTJSNI_BaseLayer` property is denied to
    /// script (`TJS_DENY_NATIVE_PROP_SETTER`), and the refusal carries
    /// `TJS_E_ACCESSDENYED` (-1007) with the reference's text.
    #[test]
    fn layer_read_only_properties_reject_script_writes() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.layer = new Layer(window, null);
                layer.setSize(5, 6);
                var names = [
                    "children",
                    "nodeVisible",
                    "window",
                    "isPrimary",
                    "prevFocusable",
                    "nextFocusable",
                    "nodeFocusable",
                    "focused",
                    "nodeEnabled",
                    "mainImageBuffer",
                    "mainImageBufferForWrite",
                    "mainImageBufferPitch",
                    "provinceImageBuffer",
                    "provinceImageBufferForWrite",
                    "provinceImageBufferPitch",
                    "font"
                ];
                var denied = 0;
                var message = "";
                for (var i = 0; i < names.count; i++) {
                    try { layer[names[i]] = 0; } catch (e) {
                        denied++;
                        message = e.message;
                    }
                }
                // The `TJS_IGNOREPROP` store skips the property object and
                // overwrites the member (`tTJSCustomObject::PropSet`,
                // `tjsObject.cpp:1519-1552`), which is how KAGEX installs a
                // font hook (`&a2.font = this`).
                var hook = %[kind: "FontHook"];
                &layer.font = hook;
                var hookRead = layer.font.kind == "FontHook";
                // The denied writes left every value where it was: the layer
                // still belongs to its window and keeps its size.
                return denied + ":" + message + ":" + (layer.window == window) + ":" +
                    layer.width + ":" + typeof layer.mainImageBuffer + ":" + hookRead;
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String(
                "16:Invalid operation for Read-only or Write-only property:1:5:void:1".to_string()
            )
        );
        let window = object_handle(&engine, "window");
        let layer = object_handle(&engine, "layer");
        assert_eq!(engine.host().native_layer_window(layer), Some(window));
    }

    #[test]
    fn window_and_bitmap_read_only_properties_reject_script_writes() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                var layer = new Layer(window, null);
                var bitmap = new Bitmap();
                var denied = 0;
                var message = "";
                var windowNames = ["mainWindow", "primaryLayer", "layerTreeOwnerInterface"];
                for (var i = 0; i < windowNames.count; i++) {
                    try { window[windowNames[i]] = 0; } catch (e) {
                        denied++;
                        message = e.message;
                    }
                }
                // The class-level `mainWindow` is read-only too.
                try { Window.mainWindow = 0; } catch (e) {
                    denied++;
                    message = e.message;
                }
                var bitmapNames = ["buffer", "bufferForWrite", "bufferPitch", "loading"];
                for (var j = 0; j < bitmapNames.count; j++) {
                    try { bitmap[bitmapNames[j]] = 0; } catch (e) {
                        denied++;
                        message = e.message;
                    }
                }
                return denied + ":" + message + ":" + (window.primaryLayer == layer) + ":" +
                    (Window.mainWindow == window) + ":" + bitmap.width;
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String(
                "8:Invalid operation for Read-only or Write-only property:1:1:0".to_string()
            )
        );
    }

    /// The `tTJSVariant(dsp, dsp)` members hand out a value that carries its own
    /// object as ObjThis (`LayerIntf.cpp:665`, `:8490`, `:8527`, `:8683`), so a
    /// write through such a value runs on that object rather than on the
    /// writer's `this`.
    #[test]
    fn layer_self_bound_members_carry_their_object_as_this() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class Writer {
                    var holder;
                    function Writer(holder) { this.holder = holder; }
                    function poke() {
                        this.holder.parent.parentMarker = 1;
                        this.holder.window.windowMarker = 2;
                        this.holder.childrenArray.arrayMarker = 3;
                        this.holder.childrenArray[0].elementMarker = 4;
                    }
                }
                var window = new Window();
                var parent = new Layer(window, null);
                var child = new Layer(window, parent);
                global.writer = new Writer(%[
                    parent: child.parent,
                    window: child.window,
                    childrenArray: parent.children
                ]);
                writer.poke();
                return parent.parentMarker + ":" + window.windowMarker + ":" +
                    parent.children.arrayMarker + ":" + child.elementMarker + ":" +
                    (typeof writer.parentMarker) + ":" + (typeof writer.windowMarker) + ":" +
                    (typeof writer.arrayMarker);
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String("1:2:3:4:undefined:undefined:undefined".to_string())
        );
    }

    /// `Layer.order`/`Layer.absolute` are `GetOrderIndex()` and
    /// `GetAbsoluteOrderIndex()` (`LayerIntf.cpp:8541`, `:8561`): live indices,
    /// not stored numbers, with absolute order mode switching `absolute` to the
    /// index the script set.
    #[test]
    fn layer_order_and_absolute_are_live_indices() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parent = new Layer();
                var first = new Layer(null, parent);
                var second = new Layer(null, parent);
                var third = new Layer(null, parent);
                var before = first.order + ":" + second.order + ":" + third.order;
                var absoluteBefore = first.absolute;
                first.order = 2;
                var after = first.order + ":" + second.order + ":" + third.order;
                // No parent: both indices report 0 (`LayerIntf.h:278`,
                // `LayerIntf.cpp:1256`).
                var detached = (new Layer()).order + ":" + (new Layer()).absolute;
                parent.absoluteOrderMode = 1;
                var mode = second.order + ":" + second.absolute;
                second.absolute = 5;
                var absoluteAfter = second.absolute + ":" + parent.absoluteOrderMode;
                // Writing either index needs a parent:
                // `TVPCannotMovePrimaryOrSiblingless` (`LayerIntf.cpp:1221`,
                // `:1264`; `string_table_en.rc:128`).
                var siblinglessOrder = "";
                try { (new Layer()).order = 0; } catch (e) { siblinglessOrder = e.message; }
                var siblinglessAbsolute = "";
                try { (new Layer()).absolute = 1; } catch (e) {
                    siblinglessAbsolute = e.message;
                }
                return before + ":" + absoluteBefore + ":" + after + ":" + detached + ":" +
                    mode + ":" + absoluteAfter + ":" + siblinglessOrder + ":" +
                    siblinglessAbsolute;
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String(
                "0:1:2:0:2:0:1:0:0:0:0:5:1:Cannot move primary or siblingless:\
                 Cannot move primary or siblingless"
                    .to_string()
            )
        );
    }

    /// A layer placed with an absolute index keeps that index until an `order`
    /// write; the render apply then has to follow the sibling order rather than
    /// snap back to the stale absolute value (`apply_layer_properties_to_node`
    /// prefers `absolute`).
    #[test]
    fn layer_order_write_clears_a_stale_absolute_placement() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                var parent = new Layer(window, null);
                global.first = new Layer(null, parent);
                global.second = new Layer(null, parent);
                global.third = new Layer(null, parent);
                first.absolute = 500;
                first.order = 0;
                // Any window move re-applies every layer to the render tree,
                // which is when the stored keys are read back.
                window.left = 5;
                return first.order + ":" + second.order + ":" + third.order;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("0:1:2".to_string()));
    }

    /// `nodeFocusable`, `prevFocusable`, `nextFocusable` and `focused` are
    /// computed on read (`LayerIntf.h:622`, `LayerIntf.cpp:3218`, `:3261`,
    /// `:3300`) and follow the focus chain as it changes.
    #[test]
    fn layer_focus_chain_members_are_computed() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                var root = new Layer(window, null);
                var first = new Layer(window, root);
                var second = new Layer(window, root);
                var third = new Layer(window, root);
                root.visible = true;
                first.visible = true;
                second.visible = true;
                third.visible = true;
                first.focusable = true;
                second.focusable = true;
                third.focusable = true;
                // The reference walks the window's overall order, wrapping
                // around (`LayerIntf.cpp:3243-3298`), so the last focusable
                // layer's `nextFocusable` is the first and vice versa.
                var chain = root.nodeFocusable + ":" + first.nodeFocusable + ":" +
                    (second.prevFocusable == first) + ":" + (second.nextFocusable == third) +
                    ":" + (third.nextFocusable == first) + ":" +
                    (first.prevFocusable == third) + ":" + (root.nextFocusable == first);
                second.focus();
                var focus = first.focused + ":" + second.focused + ":" + third.focused;
                first.focus();
                var moved = first.focused + ":" + second.focused;
                // `order` moves the layer inside the children vector, and the
                // focus search walks that vector (`tTVPLayerManager::AllNodes`,
                // `LayerManager.cpp:181-190`), not the creation order: after
                // moving `first` to index 1 the cycle is
                // `root, second, first, third`.
                first.order = 1;
                var reordered = (first.nextFocusable == third) + ":" +
                    (first.prevFocusable == second) + ":" +
                    (second.nextFocusable == first) + ":" +
                    (third.nextFocusable == second);
                // Leaving the focus chain removes a layer from the search.
                second.joinFocusChain = false;
                third.joinFocusChain = false;
                var lonely = (first.nextFocusable === null) + ":" +
                    (first.prevFocusable === null);
                return chain + ":" + focus + ":" + moved + ":" + reordered + ":" + lonely;
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String("0:1:1:1:1:1:1:0:1:0:1:0:1:1:1:1:1:1".to_string())
        );
    }

    /// `GetNodeVisible()` is `GetParentVisible() && Visible` (`LayerIntf.h:308`)
    /// and, like `nodeEnabled` (`:651`), is computed on every read.
    #[test]
    fn layer_node_visible_follows_the_ancestor_chain() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parent = new Layer();
                var child = new Layer(null, parent);
                parent.visible = true;
                child.visible = true;
                var before = child.nodeVisible;
                parent.visible = false;
                var hiddenParent = child.nodeVisible;
                parent.visible = true;
                child.visible = false;
                var hiddenSelf = child.nodeVisible;
                return before + ":" + hiddenParent + ":" + hiddenSelf;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1:0:0".to_string()));
    }

    /// `Window.mainWindow` reads back on an instance (`WindowIntf.cpp:1796`),
    /// and `Window.primaryLayer` throws `TVPWindowHasNoLayer` when the window
    /// has none (`:1862`, "Window has no layer").
    #[test]
    fn window_main_window_and_primary_layer_reads() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                var second = new Window();
                var missing = "";
                try { second.primaryLayer; } catch (e) { missing = e.message; }
                var layer = new Layer(window, null);
                return (second.mainWindow == window) + ":" + (Window.mainWindow == window) +
                    ":" + missing + ":" + (window.primaryLayer == layer);
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String("1:1:Window has no layer:1".to_string())
        );
    }

    /// `Window.children` has no official counterpart (M2 §5a): script reads
    /// fail with `Member "children" does not exist` while the engine keeps the
    /// registry internally.
    #[test]
    fn window_children_is_not_a_script_member() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.layer = new Layer(window, null);
                window.add(layer);
                var read = "";
                try { window.children; } catch (e) { read = e.message; }
                // TJS2 lets a script add a data member of that name, which is
                // what the reference does for an unknown member of an ordinary
                // object; it never becomes the engine's child registry.
                window.children = 7;
                return read + "|" + window.children;
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String("Member \"children\" does not exist|7".to_string())
        );
        let window = object_handle(&engine, "window");
        let layer = object_handle(&engine, "layer");
        assert_eq!(engine.host().native_window_children(window), vec![layer]);
    }

    /// `LayerIntf.cpp:7835`: `assignImages` checks `numparams` before it looks
    /// at the argument, so a zero-argument call reports `TJS_E_BADPARAMCOUNT`
    /// (`Invalid argument count`) and a present non-Layer value reports
    /// `TVPSpecifyLayer` (`Specify Layer class object`).
    #[test]
    fn layer_assign_images_checks_the_argument_count_first() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var dest = new Layer();
                var noArgs = "";
                try { dest.assignImages(); } catch (e) { noArgs = e.message; }
                var notALayer = "";
                try { dest.assignImages(1); } catch (e) { notALayer = e.message; }
                var voidArg = "";
                try { dest.assignImages(void); } catch (e) { voidArg = e.message; }
                return noArgs + "|" + notALayer + "|" + voidArg;
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String(
                "Invalid argument count|Specify Layer class object|Specify Layer class object"
                    .to_string()
            )
        );
    }

    #[test]
    fn window_add_remove_updates_backing_primary_layer_and_keeps_focus_empty() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.layer = new Layer(window, null);
                window.add(layer);
                "#,
            )
            .expect("script");
        let window = object_handle(&engine, "window");
        let layer = object_handle(&engine, "layer");
        assert_eq!(
            engine.host().native_window_primary_layer(window),
            Some(layer)
        );
        assert_eq!(engine.host().native_window_focused_layer(window), None);
        assert_eq!(engine.host().native_layer_window(layer), Some(window));

        engine
            .execute_script("inline.tjs", "window.remove(layer);")
            .expect("remove");
        assert_eq!(engine.host().native_window_primary_layer(window), None);
        assert_eq!(engine.host().native_window_focused_layer(window), None);
    }

    #[test]
    fn storage_reads_startup_script_from_project_root() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("startup.tjs"), "return 42;").expect("write startup");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        assert_eq!(
            engine.execute_startup().expect("startup"),
            Variant::Integer(42)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn start_project_uses_startup_dispatcher_without_guessing_kag_names() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("startup.tjs"),
            "global.startupMarker = 1; return 0;",
        )
        .expect("write startup");
        fs::write(root.join("first.ks"), b"*first\n[test]").expect("write decoy");
        fs::write(root.join("title.ks"), b"*title\n[test]").expect("write decoy");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.start_project().expect("start project");

        assert_eq!(
            engine.tjs_runtime().global_member("startupMarker"),
            Variant::Integer(1)
        );
        assert!(!engine.has_kag_scenario());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn start_project_falls_back_to_startup_ks_only_when_no_tjs_exists() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("startup.ks"), b"*start\n[test]").expect("write startup");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.start_project().expect("start project");

        assert!(engine.has_kag_scenario());
        assert_eq!(engine.kag_location().storage.as_deref(), Some("startup.ks"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn storage_executes_bytecode_startup_from_project_root() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("startup.tjs"), integer_return_bytecode(42)).expect("write startup");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        assert_eq!(
            engine.execute_startup().expect("startup"),
            Variant::Integer(42)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn scripts_eval_assigns_through_star_of_a_property_proxy_call() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        assert_eq!(
            engine
                .execute_script(
                    "inline.tjs",
                    r#"
                    global.box = %[];
                    Scripts.eval("(*box) = 4");
                    return *box;
                    "#,
                )
                .expect("eval"),
            Variant::Integer(4)
        );
    }

    #[test]
    fn scripts_eval_empty_string_returns_void() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(
            engine
                .execute_script("inline.tjs", r#"return Scripts.eval("");"#)
                .expect("eval"),
            Variant::Void
        );
    }

    #[test]
    fn scripts_eval_supports_kirikiriz_conditional_compile_probe() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(
            engine
                .execute_script(
                    "inline.tjs",
                    r#"return Scripts.eval("@if(kirikiriz)1@endif");"#,
                )
                .expect("eval"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn string_case_methods_match_krkr2_ascii_behavior() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(
            engine
                .execute_script(
                    "inline.tjs",
                    r#"return "AbC123".toLowerCase() + ":" + "aBc123".toUpperCase();"#,
                )
                .expect("script"),
            Variant::String("abc123:ABC123".to_string())
        );
    }

    #[test]
    fn array_shift_and_unshift_match_krkr2_behavior() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(
            engine
                .execute_script(
                    "inline.tjs",
                    r#"
                    var a = [2, 3];
                    var len = a.unshift(0, 1);
                    var first = a.shift();
                    var second = a.shift();
                    var last = (new Array()).shift();
                    return len + ":" + first + ":" + second + ":" + a[0] + ":" + last;
                    "#,
                )
                .expect("script"),
            Variant::String("4:0:1:2:".to_string())
        );
    }

    #[test]
    fn scripts_exec_storage_accepts_string_argument_from_script_function() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("child.tjs"), "global.__child_loaded = 1;").expect("write child");
        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        engine
            .execute_script(
                "inline.tjs",
                r#"
                function KAGLoadScript(name) {
                    Scripts.execStorage(name);
                }
                KAGLoadScript("child.tjs");
                "#,
            )
            .expect("script");

        assert_eq!(
            engine.tjs_runtime().global_member("__child_loaded"),
            Variant::Integer(1)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn script_function_argument_preserves_string_type() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(
            engine
                .execute_script(
                    "inline.tjs",
                    r#"
                    function id(name) {
                        return typeof name + ":" + name;
                    }
                    return id("child.tjs");
                    "#,
                )
                .expect("script"),
            Variant::String("String:child.tjs".to_string())
        );
    }

    #[test]
    fn storage_reads_startup_from_unpacked_project_layers() {
        let root = temp_root();
        fs::create_dir_all(root.join("data/system")).expect("create data");
        fs::create_dir_all(root.join("patch3")).expect("create patch");
        fs::write(
            root.join("data/startup.tjs"),
            r#"
            Storages.addAutoPath(System.exePath + "system/");
            return Scripts.evalStorage("Config.tjs");
            "#,
        )
        .expect("write startup");
        fs::write(root.join("data/system/Config.tjs"), "1").expect("write config");
        fs::write(root.join("patch3/Config.tjs"), "2").expect("write patch config");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        assert_eq!(
            engine.execute_startup().expect("startup"),
            Variant::Integer(2)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn storage_decodes_legacy_project_text() {
        let root = temp_root();
        fs::create_dir_all(root.join("data")).expect("create data");
        fs::create_dir_all(root.join("patch3")).expect("create patch");
        fs::write(root.join("data/startup.tjs"), b"// \x82\xa0\nreturn 7;")
            .expect("write shift-jis startup");
        fs::write(root.join("patch3/gbk.tjs"), b"// \xc4\xe3\n8").expect("write gbk storage");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        assert_eq!(
            engine.execute_startup().expect("startup"),
            Variant::Integer(7)
        );
        assert_eq!(
            engine.eval_storage("gbk.tjs").expect("gbk"),
            Variant::Integer(8)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn storages_set_text_encoding_updates_host_and_scripts_member() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                Storages.setTextEncoding("gbk");
                return Scripts.textEncoding;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("gbk".to_string()));
        assert_eq!(engine.host().text_encoding(), "gbk");
    }

    #[test]
    fn storages_path_helpers_match_krkr2_storage_syntax() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var ext = Storages.extractStorageExt;
                return ext("arc.xp3>dir/file.tjs") + ":" +
                    Storages.extractStorageName("arc.xp3>dir/file.tjs") + ":" +
                    Storages.extractStoragePath("arc.xp3>dir/file.tjs") + ":" +
                    Storages.chopStorageExt("arc.xp3>dir/file.tjs") + ":" +
                    Storages.extractStorageExt("noext") + ":" +
                    Storages.chopStorageExt("noext");
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String(".tjs:file.tjs:arc.xp3>dir/:arc.xp3>dir/file::noext".to_string())
        );
    }

    #[test]
    fn native_methods_match_function_instance_class() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                return (Storages.extractStorageExt instanceof "Function") + ":" +
                    (Layer.releaseCapture instanceof "Function");
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1:1".to_string()));
    }

    #[test]
    fn string_replace_supports_kag_startup_lock_pattern() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"return "a/b-c".replace(/[^A-Za-z]/g, "_");"#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("a_b_c".to_string()));
    }

    #[test]
    fn string_index_methods_use_tjs_offsets() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"return "a😀b😀".indexOf("😀", 2) + ":" + "a😀b😀".lastIndexOf("😀");"#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("4:4".to_string()));
    }

    #[test]
    fn class_methods_access_class_var_members() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class C {
                    var value = 3;
                    function getValue() { return value; }
                    function setValue(next) { value = next; }
                }
                var c = new C();
                c.setValue(9);
                return c.getValue();
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::Integer(9));
    }

    #[test]
    fn class_super_constructor_initializes_current_instance() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class Base {
                    var value;
                    function Base(v) { value = v; }
                    function getValue() { return value; }
                }
                class Child extends Base {
                    function Child() { super.Base(11); }
                }
                var child = new Child();
                return child.getValue();
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::Integer(11));
    }

    #[test]
    fn external_resource_pending_suspends_and_resumes_tjs_instruction() {
        let storage = ProjectStorage::from_memory(Vec::<(String, Vec<u8>)>::new());
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.set_external_resource_catalog(["lazy.ks"]);

        let result = engine.execute_script(
            "lazy-loader.tjs",
            r#"
                var lines = new Array();
                lines.load("lazy.ks");
            "#,
        );
        assert!(
            result.is_ok(),
            "resource suspension is not a script error: {result:?}"
        );
        assert!(engine.is_script_suspended());
        assert_eq!(
            engine.take_external_resource_requests(),
            vec![("lazy.ks".to_string(), krkr_core::AssetKind::Text)]
        );

        engine
            .provide_external_resource("lazy.ks", b"first\nsecond\n".to_vec())
            .expect("resume resource");
        assert!(!engine.is_script_suspended());
        assert!(engine.take_external_resource_requests().is_empty());
    }

    /// A callback parked on a pending resource load must surface as a KAG wait
    /// at the frame boundary. Games that drive KAG from their own TJS
    /// conductor (`parser.getNextTag()` in script, e.g. GINKA's logo
    /// sequence) never move the engine session out of `Finished`, so the
    /// session state alone cannot tell a host that the runtime is stuck
    /// waiting for a decode.
    #[test]
    fn script_parked_on_a_resource_reports_a_kag_resource_wait() {
        let storage = ProjectStorage::from_memory(Vec::<(String, Vec<u8>)>::new());
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.set_external_resource_catalog(["lazy.ks"]);
        engine
            .execute_script(
                "lazy-loader.tjs",
                "var lines = new Array();\nlines.load(\"lazy.ks\");",
            )
            .expect("resource suspension is not a script error");
        assert!(engine.is_script_suspended());
        assert_eq!(*engine.kag_state(), KagTaskState::Finished);

        let input = || {
            EngineInput::new(
                FrameInput::new(Size::new(320.0, 240.0), 1.0 / 60.0),
                Vec::new(),
            )
        };
        let frame = engine
            .update(input(), Duration::from_millis(16))
            .expect("frame while parked");
        assert_eq!(frame.tick.state, KagTaskState::WaitingResource);
        assert_eq!(*engine.kag_state(), KagTaskState::WaitingResource);

        engine
            .provide_external_resource("lazy.ks", b"first\nsecond\n".to_vec())
            .expect("resume resource");
        assert!(!engine.is_script_suspended());
        engine
            .update(input(), Duration::from_millis(16))
            .expect("frame after resume");
        // The mask is lifted: a parser-less session reports `Finished` again.
        assert_eq!(*engine.kag_state(), KagTaskState::Finished);
    }

    /// The parked-VM mark must not replace a scenario wait the session is
    /// holding. `WaitingClick` (like the timer/audio/transition/modal waits)
    /// lives only in the session state, so masking it would let `update_wait`
    /// turn the session `Running` once the parked work lands and silently drop
    /// a click gate the scenario still needs.
    #[test]
    fn parked_resource_keeps_a_pending_scenario_wait() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "AB[p]C").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.set_external_resource_catalog(["lazy.ks"]);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");
        assert_eq!(tick.state, KagTaskState::WaitingClick);

        // A callback parks the VM while that wait is pending. `update` still
        // pumps in this state (`waiting_for_resource` is false), which is how
        // a running game reaches the overlap.
        engine
            .execute_script(
                "lazy-loader.tjs",
                "var lines = new Array();\nlines.load(\"lazy.ks\");",
            )
            .expect("resource suspension is not a script error");
        assert!(engine.is_script_suspended());

        let input = || {
            EngineInput::new(
                FrameInput::new(Size::new(320.0, 240.0), 1.0 / 60.0),
                Vec::new(),
            )
        };
        for _ in 0..3 {
            let frame = engine
                .update(input(), Duration::from_millis(16))
                .expect("frame while parked");
            assert_eq!(frame.tick.state, KagTaskState::WaitingClick);
            assert_eq!(*engine.kag_state(), KagTaskState::WaitingClick);
        }

        engine
            .provide_external_resource("lazy.ks", b"first\n".to_vec())
            .expect("resume resource");
        assert!(!engine.is_script_suspended());
        engine
            .update(input(), Duration::from_millis(16))
            .expect("frame after resume");
        assert_eq!(*engine.kag_state(), KagTaskState::WaitingClick);

        // The gate still works after the park: a click releases it.
        engine.signal_kag_click();
        assert_eq!(*engine.kag_state(), KagTaskState::Running);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_parser_retries_expanded_call_after_remote_storage_arrives() {
        let storage = ProjectStorage::from_memory([(
            "first.ks",
            b"[emb escape=false exp=global.generated]\n".to_vec(),
        )]);
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.set_external_resource_catalog(["custom.ks"]);

        engine
            .execute_script(
                "kag-lazy-call.tjs",
                r#"
                    global.generated = "[call storage='custom.ks']";
                    global.parser = new KAGParser();
                    parser.loadScenario("first.ks");
                    global.tag = parser.getNextTag();
                "#,
            )
            .expect("initial parser call suspends on custom.ks");
        assert!(engine.is_script_suspended());
        assert_eq!(
            engine.take_external_resource_requests(),
            vec![("custom.ks".to_string(), krkr_core::AssetKind::Text)]
        );

        engine
            .provide_external_resource("custom.ks", b"[opening]\n".to_vec())
            .expect("resume parser call");
        assert!(!engine.is_script_suspended());
        let tag = match engine.tjs_runtime().global_member("tag") {
            Variant::Object(handle) => handle,
            value => panic!("unexpected tag value: {value:?}"),
        };
        assert_eq!(
            engine.tjs_runtime().object_member(tag, "tagname"),
            Variant::String("opening".to_string())
        );
    }

    #[test]
    fn external_image_pending_resumes_native_layer_call_continuation() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        let image_path = root.join("sprite.png");
        write_png(image_path.clone(), 1, 1, &[255, 0, 0, 255]);
        let bytes = fs::read(&image_path).expect("read image");
        fs::remove_file(image_path).expect("remove image");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.set_external_resource_catalog(["sprite.png"]);
        engine
            .execute_script(
                "image-loader.tjs",
                r#"
                    var window = new Window();
                    global.layer = new Layer(window, null);
                    layer.visible = true;
                    layer.loadImages("sprite.png");
                    global.afterImage = 1;
                "#,
            )
            .expect("initial script");
        assert!(engine.is_script_suspended());
        assert_eq!(
            engine.take_external_resource_requests(),
            vec![("sprite.png".to_string(), krkr_core::AssetKind::Image)]
        );
        engine
            .provide_external_resource("sprite.png", bytes)
            .expect("resume image");
        assert!(!engine.is_script_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("afterImage"),
            Variant::Integer(1)
        );
        let layer = object_handle(&engine, "layer");
        let layer_id = engine.host().native_layer(layer).expect("native layer");
        let node = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .expect("layer node");
        assert!(node.image.is_some());
        assert!(node.visible);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn nested_exec_storage_resumes_outer_script_after_remote_child() {
        let storage = ProjectStorage::from_memory(Vec::<(String, Vec<u8>)>::new());
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.set_external_resource_catalog(["child.tjs"]);
        engine
            .execute_script(
                "outer.tjs",
                r#"Scripts.execStorage("child.tjs"); global.afterChild = 7;"#,
            )
            .expect("initial script");
        assert!(engine.is_script_suspended());
        assert_eq!(
            engine.take_external_resource_requests(),
            vec![("child.tjs".to_string(), krkr_core::AssetKind::Binary)]
        );

        engine
            .provide_external_resource("child.tjs", b"global.childLoaded = 1;".to_vec())
            .expect("resume child");
        assert!(!engine.is_script_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("afterChild"),
            Variant::Integer(7)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("childLoaded"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn deeply_nested_exec_storage_preserves_all_callers() {
        let storage = ProjectStorage::from_memory(Vec::<(String, Vec<u8>)>::new());
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.set_external_resource_catalog(["child.tjs", "grand.tjs"]);
        engine
            .execute_script(
                "outer.tjs",
                r#"Scripts.execStorage("child.tjs"); global.outerDone = 1;"#,
            )
            .expect("outer script");
        engine
            .provide_external_resource(
                "child.tjs",
                b"Scripts.execStorage(\"grand.tjs\"); global.childDone = 1;".to_vec(),
            )
            .expect("child script");
        assert!(engine.is_script_suspended());
        engine
            .provide_external_resource("grand.tjs", b"global.grandDone = 1;".to_vec())
            .expect("grand script");
        assert!(!engine.is_script_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("grandDone"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("childDone"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("outerDone"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn nested_exec_storage_inside_helper_preserves_helper_callers() {
        let storage = ProjectStorage::from_memory(Vec::<(String, Vec<u8>)>::new());
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.set_external_resource_catalog(["child.tjs"]);
        engine
            .execute_script(
                "outer.tjs",
                r#"
                    function loadChild() { Scripts.execStorage("child.tjs"); global.helperDone = 1; }
                    loadChild();
                    global.outerDone = 1;
                "#,
            )
            .expect("outer script");
        assert!(engine.is_script_suspended());
        engine
            .provide_external_resource("child.tjs", b"global.childLoaded = 1;".to_vec())
            .expect("resume child");
        assert!(!engine.is_script_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("childLoaded"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("helperDone"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("outerDone"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn async_lazy_property_getter_retries_after_remote_script_load() {
        let storage = ProjectStorage::from_memory(Vec::<(String, Vec<u8>)>::new());
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.set_external_resource_catalog(["child.tjs"]);

        engine
            .execute_script(
                "lazy-property.tjs",
                r#"
                    property LazyChild {
                        getter() {
                            Scripts.execStorage("child.tjs");
                            return global.Child;
                        }
                    }
                    property LazyCall {
                        getter() {
                            Scripts.execStorage("child.tjs");
                            return global.childFn;
                        }
                    }
                    global.lazyType = typeof LazyChild;
                    global.lazyCallResult = LazyCall();
                "#,
            )
            .expect("initial script suspends at lazy getter");
        assert!(engine.is_script_suspended());
        assert_eq!(
            engine.take_external_resource_requests(),
            vec![("child.tjs".to_string(), krkr_core::AssetKind::Binary)]
        );

        engine
            .provide_external_resource(
                "child.tjs",
                b"class Child {} function childFn() { return 9; }".to_vec(),
            )
            .expect("resume lazy getter");
        assert!(!engine.is_script_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("lazyType"),
            Variant::String("Object".to_string())
        );
        assert_eq!(
            engine.tjs_runtime().global_member("lazyCallResult"),
            Variant::Integer(9)
        );
    }

    #[test]
    fn kag_scenario_jump_waits_for_remote_storage() {
        let storage = ProjectStorage::from_memory([(
            "startup.ks",
            b"*start\n[jump storage=lazy.ks target=*start]".to_vec(),
        )]);
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.set_external_resource_catalog(["lazy.ks"]);
        engine.load_kag_scenario("startup.ks").expect("scenario");

        let input = EngineInput::new(
            FrameInput::new(Size::new(320.0, 200.0), 1.0 / 60.0),
            Vec::new(),
        );
        let frame = engine
            .update(input, Duration::from_millis(16))
            .expect("tick");
        assert_eq!(frame.tick.state, KagTaskState::WaitingResource);
        assert_eq!(
            engine.take_external_resource_requests(),
            vec![("lazy.ks".to_string(), krkr_core::AssetKind::Text)]
        );

        engine
            .provide_external_resource("lazy.ks", b"*start\n".to_vec())
            .expect("provide scenario");
        let input = EngineInput::new(
            FrameInput::new(Size::new(320.0, 200.0), 1.0 / 60.0),
            Vec::new(),
        );
        let frame = engine
            .update(input, Duration::from_millis(16))
            .expect("resume tick");
        assert_ne!(
            frame.tick.state,
            KagTaskState::Error {
                message: String::new()
            }
        );
    }

    #[test]
    fn native_super_constructor_initializes_current_instance() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class GameWindow extends Window {
                    function GameWindow() { super.Window(); }
                }
                var window = new GameWindow();
                return typeof window.menu != "undefined";
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::Integer(1));
    }

    #[test]
    fn nested_native_super_constructor_initializes_leaf_instance() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class BaseWindow extends Window {
                    function BaseWindow() { super.Window(); }
                }
                class GameWindow extends BaseWindow {
                    function GameWindow() { super.BaseWindow(); }
                    function hasMenu() { return typeof menu != "undefined"; }
                }
                var window = new GameWindow();
                return window.hasMenu() + ":" + (typeof window.menu != "undefined");
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("1:1".to_string()));
    }

    #[test]
    fn native_window_finalize_is_visible_to_script_subclasses() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.trace = "";
                class DialogWindow extends Window {
                    function DialogWindow() { super.Window(); }
                    function finalize() {
                        global.trace += "D";
                        super.finalize(...);
                        global.trace += "F";
                    }
                }
                var win = new DialogWindow();
                invalidate win;
                return global.trace + ":" + (isvalid win);
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("DF:0".to_string()));
    }

    #[test]
    fn native_menu_item_constructor_preserves_script_click_handlers() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "menu_item_override.tjs",
                r#"
                global.trace = "";
                class KAGMenuItem extends MenuItem {
                    function KAGMenuItem(owner, caption) {
                        super.MenuItem(owner, caption);
                    }
                    function click() { global.trace += "C"; }
                    function onClick() {
                        super.onClick(...);
                        global.trace += "O";
                        click();
                    }
                }
                var item = new KAGMenuItem(global, "History");
                item.click();
                item.onClick();
                return global.trace;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("COC".to_string()));
    }

    #[test]
    fn native_timer_empty_finalize_is_visible_to_script_subclasses() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "timer_finalize.tjs",
                r#"
                global.trace = "";
                class ConductorTimer extends Timer {
                    function ConductorTimer() { super.Timer(function() {}, ""); }
                    function finalize() {
                        global.trace += "D";
                        super.finalize(...);
                        global.trace += "F";
                    }
                }
                var timer = new ConductorTimer();
                invalidate timer;
                return global.trace + ":" + (isvalid timer);
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("DF:0".to_string()));
    }

    #[test]
    fn native_window_show_modal_suspends_and_resumes_script() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.trace = "before";
                global.modal = new Window();
                modal.showModal();
                global.trace += ":after";
                "#,
            )
            .expect("script");

        assert!(engine.tjs_runtime().is_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("before".to_string())
        );

        let modal = object_handle(&engine, "modal");
        engine
            .tjs_runtime_mut()
            .call_object_method(modal, "close", Vec::new())
            .expect("close modal");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("resume frame");

        assert!(!engine.tjs_runtime().is_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("before:after".to_string())
        );
    }

    #[test]
    fn native_window_close_hides_owned_layers() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.main = new Window();
                global.dialog = new Window();
                global.dialogLayer = new Layer(dialog, null);
                dialog.add(dialogLayer);
                dialogLayer.setSize(40, 20);
                dialogLayer.setImageSize(40, 20);
                dialogLayer.visible = true;
                dialog.visible = true;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("visible frame");

        let layer = object_handle(&engine, "dialogLayer");
        let layer_id = engine.host().native_layer(layer).expect("native layer");
        assert!(
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .expect("layer node")
                .visible
        );

        let dialog = object_handle(&engine, "dialog");
        engine
            .tjs_runtime_mut()
            .call_object_method(dialog, "close", Vec::new())
            .expect("close dialog");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("hidden frame");

        assert!(
            !engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .expect("layer node")
                .visible
        );
    }

    #[test]
    fn suspended_modal_pauses_tjs_timer_events_until_resume() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.trace = "";
                global.main = new Window();
                global.modal = new Window();
                global.timerProbe = new Timer(function() {
                    global.trace += "T";
                    global.timerProbe.enabled = false;
                }, "");
                timerProbe.interval = 1000;
                timerProbe.enabled = true;
                modal.showModal();
                global.trace += "R";
                "#,
            )
            .expect("script");
        assert!(engine.tjs_runtime().is_suspended());
        let timer_probe = object_handle(&engine, "timerProbe");
        force_timer_due(&mut engine, timer_probe);

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("modal frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String(String::new())
        );

        let modal = object_handle(&engine, "modal");
        engine
            .tjs_runtime_mut()
            .call_object_method(modal, "close", Vec::new())
            .expect("close modal");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("resume frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("R".to_string())
        );

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("timer frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("RT".to_string())
        );
    }

    #[test]
    fn event_pump_stops_when_tjs_event_opens_modal() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.trace = "";
                global.modal = new Window();
                global.firstTimer = new Timer(function() {
                    global.trace += "A";
                    global.firstTimer.enabled = false;
                    modal.showModal();
                    global.trace += "B";
                }, "");
                firstTimer.interval = 1000;
                firstTimer.enabled = true;
                global.secondTimer = new Timer(function() {
                    global.trace += "X";
                    global.secondTimer.enabled = false;
                }, "");
                secondTimer.interval = 1000;
                secondTimer.enabled = true;
                "#,
            )
            .expect("script");
        let first_timer = object_handle(&engine, "firstTimer");
        let second_timer = object_handle(&engine, "secondTimer");
        force_timer_due(&mut engine, first_timer);
        force_timer_due(&mut engine, second_timer);

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("modal frame");
        assert!(engine.tjs_runtime().is_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("A".to_string())
        );

        let modal = object_handle(&engine, "modal");
        engine
            .tjs_runtime_mut()
            .call_object_method(modal, "close", Vec::new())
            .expect("close modal");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("resume frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("AB".to_string())
        );

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("second timer frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("ABX".to_string())
        );
    }

    #[test]
    fn native_window_set_pos_does_not_move_primary_layer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.root = new Layer(window, null);
                window.add(root);
                root.setSize(80, 60);
                root.setImageSize(80, 60);
                root.visible = true;
                window.setInnerSize(80, 60);
                window.setPos(20, 30);
                "#,
            )
            .expect("script");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");

        let window = object_handle(&engine, "window");
        assert_eq!(
            engine.tjs_runtime().object_member(window, "left"),
            Variant::Integer(20)
        );
        assert_eq!(
            engine.tjs_runtime().object_member(window, "top"),
            Variant::Integer(30)
        );
        let root = object_handle(&engine, "root");
        let layer_id = engine.host().native_layer(root).expect("native layer");
        let position = engine
            .host()
            .layer_tree()
            .absolute_position(layer_id)
            .expect("absolute position");
        assert_eq!(position, Point::new(0.0, 0.0));
    }

    #[test]
    fn dialog_window_position_translates_its_layer_subtree() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.main = new Window();
                main.setInnerSize(1024, 576);
                // KAG centers the game window on the desktop at startup, so
                // the frame origin is the main window's client origin.
                main.setPos(100, 50);
                global.mainLayer = new Layer(main, null);
                main.add(mainLayer);
                mainLayer.setSize(1024, 576);
                mainLayer.setImageSize(1024, 576);
                mainLayer.fillRect(0, 0, 1024, 576, 0xffffffff);
                mainLayer.visible = true;

                global.dialog = new Window();
                global.dialogRoot = new Layer(dialog, null);
                dialog.add(dialogRoot);
                dialogRoot.setSize(200, 100);
                dialogRoot.setImageSize(200, 100);
                dialogRoot.fillRect(0, 0, 200, 100, 0xffffffff);
                dialogRoot.visible = true;

                global.dialogButton = new Layer(dialog, dialogRoot);
                dialogButton.setPos(10, 20);
                dialogButton.setSize(30, 20);
                dialogButton.setImageSize(30, 20);
                dialogButton.fillRect(0, 0, 30, 20, 0xffffffff);
                dialogButton.visible = true;

                dialog.setInnerSize(200, 100);
                // `system/YesNoDialog.tjs`: center inside the main window.
                var win = Window.mainWindow;
                dialog.setPos((win.width - dialog.width >> 1) + win.left, (win.height - dialog.height >> 1) + win.top);
                dialog.visible = true;
                global.dialogRootLeft = dialogRoot.left;
                "#,
            )
            .expect("script");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(1024.0, 576.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("frame");

        let dialog_root = object_handle(&engine, "dialogRoot");
        let dialog_root_id = engine
            .host()
            .native_layer(dialog_root)
            .expect("native layer");
        // Desktop (512, 288) minus the main window origin (100, 50).
        assert_eq!(
            engine.host().layer_tree().absolute_position(dialog_root_id),
            Some(Point::new(412.0, 238.0))
        );
        // The window translation is a render projection: the layer's own
        // position stays untouched (`tTJSNI_BaseWindow::SetPosition`).
        assert_eq!(
            engine.tjs_runtime().global_member("dialogRootLeft"),
            Variant::Integer(0)
        );
        let main_layer = object_handle(&engine, "mainLayer");
        let main_layer_id = engine
            .host()
            .native_layer(main_layer)
            .expect("native layer");
        assert_eq!(
            engine.host().layer_tree().absolute_position(main_layer_id),
            Some(Point::new(0.0, 0.0))
        );

        let texture = engine
            .host()
            .layer_tree()
            .layer(dialog_root_id)
            .and_then(|layer| layer.image.as_ref())
            .map(|image| image.upload.texture_id);
        let dialog_rect = frame
            .output
            .draw_commands
            .iter()
            .find_map(|command| match command {
                krkr_core::DrawCommand::Image(image) if Some(image.texture_id) == texture => {
                    Some(image.rect)
                }
                _ => None,
            })
            .expect("dialog draw command");
        assert_eq!(
            dialog_rect,
            krkr_core::Rect::new(412.0, 238.0, 200.0, 100.0)
        );

        // Hit testing uses the same translated coordinates.
        let dialog_button = object_handle(&engine, "dialogButton");
        let dialog_button_id = engine
            .host()
            .native_layer(dialog_button)
            .expect("native layer");
        assert_eq!(
            engine
                .host()
                .layer_tree()
                .hit_test(Point::new(412.0 + 15.0, 238.0 + 25.0)),
            Some(dialog_button_id)
        );
    }

    #[test]
    fn native_layer_get_layer_at_uses_its_window_primary_and_disabled_semantics() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.firstWindow = new Window();
                global.firstRoot = new Layer(firstWindow, null);
                firstWindow.add(firstRoot);
                firstRoot.setSize(80, 60);
                firstRoot.setImageSize(80, 60);
                firstRoot.fillRect(0, 0, 80, 60, 0xffffffff);
                firstRoot.visible = true;

                global.lower = new Layer(firstWindow, firstRoot);
                lower.setSize(30, 20);
                lower.setImageSize(30, 20);
                lower.fillRect(0, 0, 30, 20, 0xffffffff);
                lower.visible = true;

                global.disabledTop = new Layer(firstWindow, firstRoot);
                disabledTop.setSize(30, 20);
                disabledTop.setImageSize(30, 20);
                disabledTop.fillRect(0, 0, 30, 20, 0xffffffff);
                disabledTop.visible = true;
                disabledTop.enabled = false;

                // This root is created later and therefore visually above
                // firstRoot in a global tree. Layer.getLayerAt must not let
                // it leak across firstWindow's layer manager.
                global.secondWindow = new Window();
                global.secondRoot = new Layer(secondWindow, null);
                secondWindow.add(secondRoot);
                secondRoot.setSize(80, 60);
                secondRoot.setImageSize(80, 60);
                secondRoot.fillRect(0, 0, 80, 60, 0xffffffff);
                secondRoot.visible = true;

                return (firstRoot.getLayerAt(5, 5) === null) + ":" +
                    (firstRoot.getLayerAt(5, 5, false, true) === disabledTop);
                "#,
            )
            .expect("script");
        assert_eq!(result, Variant::String("1:1".to_string()));

        assert!(
            engine
                .execute_expression("inline.tjs", "firstRoot.getLayerAt()")
                .is_err()
        );
    }

    #[test]
    fn script_property_override_does_not_reach_render_node() {
        // KRKR2 semantics: a script subclass overriding a Layer property with
        // a TJS property getter/setter only shadows it for script access; the
        // native render/hit-test node keeps the value set through the native
        // setter.  A game's world system (AffineLayer) may override left/top/width/
        // height with world-space computed values that must never move the
        // native layer, while its ButtonLayer overrides hitThreshold with a
        // script-side value that likewise stays script-side.
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class ScriptHitLayer extends Layer {
                    function ScriptHitLayer(parent) {
                        super.Layer(null, parent);
                        // The native backing property deliberately differs
                        // from this script-visible override.
                        super.hitThreshold = 256;
                        super.left = 10;
                    }
                    property hitThreshold {
                        getter { return 0; }
                        setter(value) { super.hitThreshold = value; }
                    }
                    property left {
                        getter { return -700; }
                        setter(value) { }
                    }
                }
                global.hits = 0;
                global.root = new Layer();
                root.setSize(40, 30);
                root.setImageSize(40, 30);
                root.fillRect(0, 0, 40, 30, 0xffffffff);
                root.visible = true;
                global.button = new ScriptHitLayer(root);
                button.setSize(20, 10);
                button.setImageSize(20, 10);
                button.fillRect(0, 0, 20, 10, 0xffffffff);
                button.visible = true;
                button.onMouseUp = function() { global.hits++; };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        let button = object_handle(&engine, "button");
        let button_id = engine.host().native_layer(button).expect("native button");
        let node = engine
            .host()
            .layer_tree()
            .layer(button_id)
            .expect("button node")
            .clone();
        // The render node reflects the native property values, not the
        // script-side getter overrides.
        assert_eq!(node.hit_threshold, 256);
        assert_eq!(node.left, 10.0);
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 5.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("input frame");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "hits")
                .expect("hits"),
            Variant::Integer(0)
        );
    }

    #[test]
    fn native_layer_affine_copy_maps_source_rect_to_destination_points() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.setImageSize(2, 1);
                source.fillRect(0, 0, 1, 1, 0xff0000ff);
                source.fillRect(1, 0, 1, 1, 0xff00ff00);
                global.dest = new Layer();
                dest.setImageSize(1, 1);
                dest.affineCopy(source, 1, 0, 1, 1, false, 0, 0, 1, 0, 0, 1);

                global.identity = new Layer();
                identity.setImageSize(2, 1);
                identity.affineCopy(source, 0, 0, 2, 1, false,
                    -0.5, -0.5, 1.5, -0.5, -0.5, 0.5);
                "#,
            )
            .expect("script");
        let source = object_handle(&engine, "source");
        let dest = object_handle(&engine, "dest");
        let identity = object_handle(&engine, "identity");
        let source_id = engine.host().native_layer(source).expect("native source");
        let dest_id = engine
            .host()
            .native_layer(dest)
            .expect("native destination");
        let source_image = engine
            .host()
            .layer_tree()
            .layer(source_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("source image");
        let dest_image = engine
            .host()
            .layer_tree()
            .layer(dest_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("destination image");
        assert_eq!(
            &dest_image.upload.rgba[..4],
            &source_image.upload.rgba[4..8]
        );
        let identity_id = engine
            .host()
            .native_layer(identity)
            .expect("native identity destination");
        let identity_image = engine
            .host()
            .layer_tree()
            .layer(identity_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("identity image");
        assert_eq!(
            &identity_image.upload.rgba[..8],
            &source_image.upload.rgba[..8]
        );
    }

    #[test]
    fn inherited_super_layer_property_assignment_updates_render_node() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(
            root.join("button.png"),
            6,
            1,
            &[
                255, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0,
                0, 255, 255,
            ],
        );

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class MiddleLayer extends Layer {
                    function MiddleLayer(window, parent) { super.Layer(window, parent); }
                }
                class ThreeStateLayer extends MiddleLayer {
                    function ThreeStateLayer(window, parent) {
                        super.MiddleLayer(window, parent);
                    }
                    function load(storage) {
                        super.loadImages(storage);
                        super.width = imageWidth \ 3;
                        super.height = imageHeight;
                    }
                }
                global.button = new ThreeStateLayer(null, null);
                button.load("button.png");
                button.visible = true;
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.width == 2.0
                        && image.rect.height == 1.0
                        && image.source_rect.width == 2.0
                        && image.texture_size.width == 6.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn modal_pointer_events_do_not_click_through_to_underlying_layers() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.trace = "";
                global.main = new Window();
                global.under = new Layer(main, null);
                under.setSize(120, 80);
                under.setImageSize(120, 80);
                under.fillRect(0, 0, 120, 80, 0xffffffff);
                under.visible = true;
                // The under layer has to be the topmost root for this test to
                // mean anything. A parentless layer cannot be moved with
                // `order` (`TVPCannotMovePrimaryOrSiblingless`,
                // `LayerIntf.cpp:1220`), and `bringToFront` is the engine's
                // pre-existing lenient path for exactly this fixture.
                under.bringToFront();
                under.onClick = function(x, y) { global.trace += "U"; };

                global.modal = new Window();
                global.modalRoot = new Layer(modal, null);
                modalRoot.setSize(80, 60);
                modalRoot.setImageSize(80, 60);
                modalRoot.fillRect(0, 0, 80, 60, 0xffffffff);
                modalRoot.visible = true;

                global.modalButton = new Layer(modal, modalRoot);
                modalButton.setPos(10, 10);
                modalButton.setSize(30, 20);
                modalButton.setImageSize(30, 20);
                modalButton.fillRect(0, 0, 30, 20, 0xffffffff);
                modalButton.visible = true;
                modalButton.onClick = function(x, y) {
                    global.trace += "M";
                    modal.close();
                };

                modal.showModal();
                global.trace += "R";
                "#,
            )
            .expect("script");
        assert!(engine.tjs_runtime().is_suspended());

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(20.0, 20.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click modal");

        assert!(!engine.tjs_runtime().is_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("MR".to_string())
        );
    }

    #[test]
    fn kag_eval_modal_suspends_scenario_until_window_closes() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            r#"[eval exp="(function(){ global.trace = 'A'; global.modal = new Window(); modal.showModal(); global.trace += 'B'; })()"]C[s]"#,
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("modal frame");
        assert_eq!(frame.tick.state, KagTaskState::WaitingModal);
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("A".to_string())
        );
        assert!(engine.message_layer().lines.is_empty());

        let modal = object_handle(&engine, "modal");
        engine
            .tjs_runtime_mut()
            .call_object_method(modal, "close", Vec::new())
            .expect("close modal");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("resume frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("AB".to_string())
        );
        assert_eq!(frame.message_layer.lines, vec!["C".to_string()]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_parser_super_methods_are_visible() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class Parser extends KAGParser {
                    function Parser() { super.KAGParser(); }
                    function hasLoadScenario() {
                        return typeof super.loadScenario != "undefined";
                    }
                }
                var parser = new Parser();
                return parser.hasLoadScenario();
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::Integer(1));
    }

    #[test]
    fn top_level_functions_are_visible_to_later_storages() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("startup.tjs"),
            r#"
            Scripts.execStorage("a.tjs");
            return Scripts.execStorage("b.tjs");
            "#,
        )
        .expect("write startup");
        fs::write(root.join("a.tjs"), "function helper() { return 4; }").expect("write a");
        fs::write(root.join("b.tjs"), "return helper();").expect("write b");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        assert_eq!(
            engine.execute_startup().expect("startup"),
            Variant::Integer(4)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn engine_loads_kag_scenario_from_project_storage() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[emb exp=\"1 + 2\"]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");

        let tag = engine
            .next_kag_tag()
            .expect("next tag")
            .expect("embedded text tag");
        assert_eq!(tag.tagname, "ch");
        assert_eq!(tag.literal_attr("text"), Some("3"));
        assert!(engine.next_kag_tag().expect("eof").is_none());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn engine_loads_active_kag_parser_object_from_session() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "*start\nA[s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        let location = engine.kag_location();

        assert_eq!(
            engine.tjs_runtime().object_member(parser, "curStorage"),
            Variant::String(location.storage.clone().unwrap_or_default())
        );
        assert_eq!(
            engine.tjs_runtime().object_member(parser, "curLine"),
            Variant::Integer(location.line.unwrap_or_default().saturating_sub(1) as i64)
        );
        assert_eq!(
            engine.tjs_runtime().object_member(parser, "curLabel"),
            Variant::String(location.label.clone().unwrap_or_default())
        );

        engine.next_kag_tag().expect("tag").expect("first tag");
        let location = engine.kag_location();
        assert_eq!(location.label.as_deref(), Some("*start"));
        assert_eq!(
            engine.tjs_runtime().object_member(parser, "curLabel"),
            Variant::String("*start".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_active_parser_control_methods_drive_engine_tick() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "*start\nA[s]\n*middle\nB[s]\n*sub\nC[return]",
        )
        .expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        let snapshot = engine
            .tjs_runtime_mut()
            .call_object_method(parser, "store", Vec::new())
            .expect("store");

        engine
            .tjs_runtime_mut()
            .call_object_method(
                parser,
                "goToLabel",
                vec![Variant::String("*middle".to_string())],
            )
            .expect("goToLabel");
        assert_eq!(
            engine.tick().expect("middle tick").state,
            KagTaskState::Finished
        );
        assert_eq!(engine.message_layer().lines, vec!["B".to_string()]);

        engine
            .tjs_runtime_mut()
            .call_object_method(parser, "restore", vec![snapshot.clone()])
            .expect("restore");
        assert_eq!(
            engine.tick().expect("restore tick").state,
            KagTaskState::Finished
        );
        assert_eq!(engine.message_layer().lines, vec!["BA".to_string()]);

        engine
            .tjs_runtime_mut()
            .call_object_method(parser, "restore", vec![snapshot])
            .expect("restore for call");
        engine
            .tjs_runtime_mut()
            .call_object_method(
                parser,
                "callLabel",
                vec![Variant::String("*sub".to_string())],
            )
            .expect("callLabel");
        assert_eq!(
            engine.tick().expect("call tick").state,
            KagTaskState::Finished
        );
        assert_eq!(engine.message_layer().lines, vec!["BACA".to_string()]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn engine_active_parser_callbacks_use_parser_owner_context() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A[s]").expect("write first scenario");
        fs::write(
            root.join("second.ks"),
            "*start|Page\n[iscript]\nglobal.scriptRan = 1;\n[endscript]\n[call target=*sub]A[s]\n*sub\n[return]",
        )
        .expect("write second scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        engine
            .tjs_runtime_mut()
            .set_global_member("activeParser", Variant::Object(parser));
        engine
            .execute_script(
                "callbacks.tjs",
                r#"
                activeParser.seen = "";
                activeParser.onScenarioLoad = function(storage) {
                    this.seen += "load:" + storage + ";";
                    return true;
                };
                activeParser.onScenarioLoaded = function(storage) {
                    this.seen += "loaded:" + storage + ";";
                };
                activeParser.onLabel = function(label, page) {
                    this.seen += "label:" + label + ":" + (page === void ? "" : page) + ";";
                };
                activeParser.onScript = function(script, storage, start) {
                    this.seen += "script:" + storage + ";";
                    Scripts.exec(script);
                };
                activeParser.onCall = function(elm) {
                    this.seen += "call:" + elm.target + ";";
                    return true;
                };
                activeParser.onReturn = function(elm) {
                    this.seen += "return:" + this.callStackDepth + ";";
                    return true;
                };
                activeParser.onAfterReturn = function() {
                    this.seen += "after;";
                };
                "#,
            )
            .expect("install callbacks");

        engine
            .load_kag_scenario("second.ks")
            .expect("reload scenario");
        assert_eq!(engine.tick().expect("tick").state, KagTaskState::Finished);
        assert_eq!(engine.message_layer().lines, vec!["A".to_string()]);
        assert_eq!(
            engine.tjs_runtime().global_member("scriptRan"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().object_member(parser, "seen"),
            Variant::String(
                "load:second.ks;loaded:second.ks;label:*start:Page;script:second.ks;call:*sub;label:*sub:;return:1;after;"
                    .to_string()
            )
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn active_parser_store_restore_does_not_fork_engine_parser_state() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "AB[s]\n*later\nZ[s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        let first = engine
            .next_kag_tag()
            .expect("first tag")
            .expect("character");
        assert_eq!(first.literal_attr("text"), Some("A"));

        let snapshot = engine
            .tjs_runtime_mut()
            .call_object_method(parser, "store", Vec::new())
            .expect("store");
        engine
            .tjs_runtime_mut()
            .call_object_method(
                parser,
                "goToLabel",
                vec![Variant::String("*later".to_string())],
            )
            .expect("goToLabel");
        engine
            .tjs_runtime_mut()
            .call_object_method(parser, "restore", vec![snapshot])
            .expect("restore");

        let next = engine
            .next_kag_tag()
            .expect("next tag")
            .expect("restored character");
        assert_eq!(next.literal_attr("text"), Some("A"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn active_parser_store_uses_krkr2_dictionary_fields() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "AB[s]\n*later\nZ[s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        engine
            .next_kag_tag()
            .expect("first tag")
            .expect("character");
        let snapshot = engine
            .tjs_runtime_mut()
            .call_object_method(parser, "store", Vec::new())
            .expect("store");
        engine
            .tjs_runtime_mut()
            .set_global_member("savedSnapshot", snapshot);

        let result = engine
            .execute_script(
                "store-shape.tjs",
                r#"
                return (savedSnapshot.snapshot === void) &&
                    savedSnapshot.storageName == "first.ks" &&
                    savedSnapshot.storageShortName == "first.ks" &&
                    savedSnapshot.curLine == 0 &&
                    savedSnapshot.curPos == 1 &&
                    savedSnapshot.callStack.count == 0 &&
                    savedSnapshot.macros !== void &&
                    savedSnapshot.ExcludeLevel == -1;
                "#,
            )
            .expect("inspect store shape");

        assert_eq!(result, Variant::Integer(1));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn active_parser_store_from_label_callback_uses_current_parser_state() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "*start|Page\nA[s]\n*later|Next\nB[s]",
        )
        .expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        engine
            .tjs_runtime_mut()
            .set_global_member("activeParser", Variant::Object(parser));
        engine
            .execute_script(
                "label-store-callback.tjs",
                r#"
                activeParser.onLabel = function(label, page) {
                    global.callbackLabel = label;
                    global.callbackParserLabel = this.curLabel;
                    global.callbackStored = this.store();
                };
                "#,
            )
            .expect("install callback");

        let first = engine.next_kag_tag().expect("next tag").expect("character");
        assert_eq!(first.literal_attr("text"), Some("A"));

        let stored = object_handle(&engine, "callbackStored");
        assert_eq!(
            engine.tjs_runtime().global_member("callbackLabel"),
            Variant::String("*start".to_string())
        );
        assert_eq!(
            engine.tjs_runtime().global_member("callbackParserLabel"),
            Variant::String("*start".to_string())
        );
        assert_eq!(
            engine.tjs_runtime().object_member(stored, "curLabel"),
            Variant::String("*start".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn krkr2_restore_uses_cur_label_not_cur_line_pos() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "AB[s]\n*later\nZ[s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        let snapshot = {
            let runtime = engine.tjs_runtime_mut();
            let object = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(object, "Dictionary");
            let call_stack = runtime.alloc_array_object(Vec::new());
            let macros = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(macros, "Dictionary");
            runtime.set_object_member(
                object,
                "storageName",
                Variant::String("first.ks".to_string()),
            );
            runtime.set_object_member(
                object,
                "storageShortName",
                Variant::String("first.ks".to_string()),
            );
            runtime.set_object_member(object, "curLine", Variant::Integer(0));
            runtime.set_object_member(object, "curPos", Variant::Integer(1));
            runtime.set_object_member(object, "curLabel", Variant::String("*later".to_string()));
            runtime.set_object_member(object, "callStack", Variant::Object(call_stack));
            runtime.set_object_member(object, "macros", Variant::Object(macros));
            runtime.set_object_member(object, "ExcludeLevel", Variant::Integer(-1));
            runtime.set_object_member(object, "IfLevel", Variant::Integer(0));
            runtime.set_object_member(object, "ExcludeLevelStack", Variant::String(String::new()));
            runtime.set_object_member(
                object,
                "IfLevelExecutedStack",
                Variant::String(String::new()),
            );
            Variant::Object(object)
        };
        engine
            .tjs_runtime_mut()
            .call_object_method(parser, "restore", vec![snapshot])
            .expect("restore");

        let next = engine
            .next_kag_tag()
            .expect("next tag")
            .expect("restored character");
        assert_eq!(next.literal_attr("text"), Some("Z"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn active_parser_store_survives_struct_save_load() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "AB[s]\n*later\nZ[s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        let first = engine
            .next_kag_tag()
            .expect("first tag")
            .expect("character");
        assert_eq!(first.literal_attr("text"), Some("A"));
        let snapshot = engine
            .tjs_runtime_mut()
            .call_object_method(parser, "store", Vec::new())
            .expect("store");
        engine
            .tjs_runtime_mut()
            .set_global_member("activeParser", Variant::Object(parser));
        engine
            .tjs_runtime_mut()
            .set_global_member("savedSnapshot", snapshot);
        engine
            .execute_script(
                "persist.tjs",
                r#"
                var data = %[mainConductor: savedSnapshot];
                (Dictionary.saveStruct incontextof data)("savedata/bookmark.ksd", "");
                var loaded = Scripts.evalStorage("savedata/bookmark.ksd");
                activeParser.goToLabel("*later");
                activeParser.restore(loaded.mainConductor);
                "#,
            )
            .expect("persist restore");

        let next = engine
            .next_kag_tag()
            .expect("next tag")
            .expect("restored character");
        assert_eq!(next.literal_attr("text"), Some("A"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn persistent_parser_snapshot_loads_call_stack_storages() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[call storage=\"second.ks\"]A[s]")
            .expect("write first scenario");
        fs::write(root.join("second.ks"), "B[return]C[s]").expect("write second scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        let first = engine
            .next_kag_tag()
            .expect("first tag")
            .expect("character");
        assert_eq!(first.literal_attr("text"), Some("B"));
        let snapshot = engine
            .tjs_runtime_mut()
            .call_object_method(parser, "store", Vec::new())
            .expect("store");
        engine
            .tjs_runtime_mut()
            .set_global_member("savedSnapshot", snapshot);
        engine
            .execute_script(
                "persist-call-stack.tjs",
                r#"
                var data = %[mainConductor: savedSnapshot];
                (Dictionary.saveStruct incontextof data)("savedata/bookmark.ksd", "");
                var loaded = Scripts.evalStorage("savedata/bookmark.ksd");
                var fresh = new KAGParser();
                fresh.restore(loaded.mainConductor);
                var tag = fresh.getNextTag();
                global.restoredText = tag.text;
                "#,
            )
            .expect("persist call stack restore");

        assert_eq!(
            engine.tjs_runtime().global_member("restoredText"),
            Variant::String("B".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn active_session_preserves_macro_call_return_and_interrupt() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "[macro name=m]M[endmacro][m][call target=*sub]A[s]\n*sub\nB[return]",
        )
        .expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let parser = engine
            .active_kag_parser_handle()
            .expect("active parser handle");
        engine
            .tjs_runtime_mut()
            .call_object_method(parser, "interrupt", Vec::new())
            .expect("interrupt");

        let interrupt = engine
            .next_kag_tag()
            .expect("interrupt tag")
            .expect("interrupt");
        assert_eq!(interrupt.tagname, "interrupt");

        assert_eq!(engine.tick().expect("tick").state, KagTaskState::Finished);
        assert_eq!(engine.message_layer().lines, vec!["MBA".to_string()]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_tempload_restores_session_message_wait_and_pending_state() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A[s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        engine.kag_session.message_layer.append_text("S");
        engine.kag_session.state = KagTaskState::WaitingClick;
        engine.kag_session.clear_page_on_click = true;
        engine
            .kag_session
            .pending_tags
            .push_back(test_tag("ch", &[("text", "P")]));

        let tempsave = test_tag("tempsave", &[("place", "9")]);
        engine
            .kag_session
            .with_parser_for_tjs(
                &mut engine.tjs_runtime,
                |parser, session, runtime, owner| {
                    session.process_native_fallback_tag(parser, runtime, owner, &tempsave)
                },
            )
            .expect("tempsave");

        engine.kag_session.message_layer.clear();
        engine.kag_session.state = KagTaskState::Running;
        engine.kag_session.clear_page_on_click = false;
        engine.kag_session.pending_tags.clear();

        let tempload = test_tag("tempload", &[("place", "9")]);
        engine
            .kag_session
            .with_parser_for_tjs(
                &mut engine.tjs_runtime,
                |parser, session, runtime, owner| {
                    session.process_native_fallback_tag(parser, runtime, owner, &tempload)
                },
            )
            .expect("tempload");

        assert_eq!(engine.kag_session.state, KagTaskState::WaitingClick);
        assert_eq!(engine.message_layer().lines, vec!["S".to_string()]);
        assert!(engine.kag_session.clear_page_on_click);
        assert_eq!(engine.kag_session.pending_tags.len(), 1);
        assert_eq!(
            engine
                .kag_session
                .pending_tags
                .front()
                .and_then(|tag| tag.literal_attr("text")),
            Some("P")
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_tick_processes_multiple_tags_until_click_wait() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "AB[p]C").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");

        let tick = engine.tick().expect("tick");
        assert_eq!(tick.tags_processed, 3);
        assert_eq!(tick.state, KagTaskState::WaitingClick);
        assert_eq!(
            tick.reason,
            KagYieldReason::Waiting(KagTaskState::WaitingClick)
        );

        let blocked = engine.tick().expect("blocked tick");
        assert_eq!(blocked.tags_processed, 0);
        assert_eq!(blocked.reason, KagYieldReason::AlreadyBlocked);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_debugger_stops_at_line_breakpoint_and_tag_step() {
        use std::sync::{Arc, Mutex};

        use krkr_tjs2::debug::{DebugAction, DebugUi, Pause, StopReason};

        struct MockUi {
            stops: Arc<Mutex<Vec<(bool, Option<usize>)>>>,
            actions: std::collections::VecDeque<DebugAction>,
        }

        impl DebugUi<KrkrHost> for MockUi {
            fn on_pause(&mut self, pause: &mut Pause<'_, KrkrHost>) -> DebugAction {
                if let StopReason::Kag { line, stepped, .. } = pause.reason() {
                    self.stops.lock().expect("stops").push((*stepped, *line));
                }
                self.actions.pop_front().unwrap_or(DebugAction::Continue)
            }
        }

        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A\n[p]\nC").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");

        let stops = Arc::new(Mutex::new(Vec::new()));
        {
            let runtime = engine.tjs_runtime_mut();
            runtime
                .enable_debugger()
                .add_kag_line_breakpoint("first.ks", 2);
            runtime.set_debug_ui(Box::new(MockUi {
                stops: Arc::clone(&stops),
                actions: vec![DebugAction::KagStep].into(),
            }));
        }

        engine.tick().expect("first tick");
        engine.signal_kag_click();
        engine.tick().expect("second tick");

        // [p] on line 2 triggers the line breakpoint; the following text tag
        // on line 3 triggers the queued KAG tag step.
        assert_eq!(
            *stops.lock().expect("stops"),
            vec![(false, Some(2)), (true, Some(3))]
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_back_page_layers_are_hidden_until_transition() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("back.png"), 1, 1, &[0, 255, 0, 255]);
        fs::write(
            root.join("first.ks"),
            "[image storage=back.png layer=base page=back][wait time=1]",
        )
        .expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert!(matches!(
            frame.tick.state,
            KagTaskState::WaitingTimer { .. }
        ));
        assert!(
            !frame
                .output
                .draw_commands
                .iter()
                .any(|command| matches!(command, krkr_core::DrawCommand::Image(_)))
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_click_signal_resumes_after_page_wait() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A[p]B").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        assert_eq!(
            engine.tick().expect("first tick").state,
            KagTaskState::WaitingClick
        );

        engine.signal_kag_click();
        let tick = engine.tick().expect("resumed tick");
        assert_eq!(tick.tags_processed, 1);
        assert_eq!(tick.state, KagTaskState::Finished);
        assert_eq!(tick.reason, KagYieldReason::Finished);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_tempsave_and_tempload_restore_builtin_parser_state() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "AB[tempsave place=1]C[s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        assert_eq!(
            engine.tick().expect("first tick").state,
            KagTaskState::Finished
        );
        assert_eq!(engine.message_layer().lines, vec!["ABC".to_string()]);

        let tag = test_tag("tempload", &[("place", "1")]);
        let action = engine
            .kag_session
            .with_parser_for_tjs(
                &mut engine.tjs_runtime,
                |parser, session, runtime, owner| {
                    session.process_native_fallback_tag(parser, runtime, owner, &tag)
                },
            )
            .expect("tempload");
        assert_eq!(action, TagAction::Continue);
        assert_eq!(engine.message_layer().lines, vec!["AB".to_string()]);

        assert_eq!(
            engine.tick().expect("restored tick").state,
            KagTaskState::Finished
        );
        assert_eq!(engine.message_layer().lines, vec!["ABC".to_string()]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_rclick_secondary_press_jumps_to_config_target() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "*start\n[rclick jump=true target=*config enabled=true]A[s]\n*config\nC[s]",
        )
        .expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        assert_eq!(
            engine.tick().expect("first tick").state,
            KagTaskState::Finished
        );
        assert_eq!(engine.message_layer().lines, vec!["A".to_string()]);

        let frame = engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::PointerInput {
                        button: PointerButton::Secondary,
                        state: ButtonState::Pressed,
                    }],
                ),
                Duration::ZERO,
            )
            .expect("right click update");

        assert_eq!(frame.tick.state, KagTaskState::Finished);
        assert_eq!(engine.message_layer().lines, vec!["AC".to_string()]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn secondary_release_does_not_repeat_script_right_click_handler() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.toggles = 0;
                global.kag = new Dictionary();
                kag.onPrimaryRightClick = function() { global.toggles++; };

                var window = new Window();
                var root = new Layer(window, null);
                var layer = new Layer(window, root);
                layer.setSize(100, 100);
                layer.hitThreshold = 0;
                layer.visible = true;
                layer.onMouseDown = function(x, y, button, shift) {
                    if(button == mbRight) kag.onPrimaryRightClick();
                };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(10.0, 10.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Secondary,
                            state: ButtonState::Pressed,
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Secondary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("right click update");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "toggles")
                .expect("toggles"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn kag_laycount_allocates_builtin_visual_and_message_layers() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[laycount layers=2 messages=3][s]")
            .expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        assert_eq!(engine.tick().expect("tick").state, KagTaskState::Finished);

        let names = engine
            .host()
            .layer_tree()
            .layers()
            .map(|layer| layer.name.as_str())
            .collect::<Vec<_>>();
        for expected in ["kag:0", "kag:1", "kag:message0", "kag:message2"] {
            assert!(names.contains(&expected), "{expected} should be allocated");
        }

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_tick_budget_yields_and_next_tick_continues() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "ABC[p]").expect("write scenario");

        let storage = ProjectStorage::for_root(&root).expect("storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            kag_budget: KagRunBudget {
                max_tags_per_tick: 2,
                max_wall_time: Duration::from_secs(1),
            },
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");

        let first = engine.tick().expect("first tick");
        assert_eq!(first.tags_processed, 2);
        assert_eq!(first.state, KagTaskState::Running);
        assert_eq!(first.reason, KagYieldReason::BudgetExhausted);

        let second = engine.tick().expect("second tick");
        assert_eq!(second.tags_processed, 2);
        assert_eq!(second.state, KagTaskState::WaitingClick);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_tick_enters_finished_at_scenario_end() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "AB").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");

        let tick = engine.tick().expect("tick");
        assert_eq!(tick.tags_processed, 2);
        assert_eq!(tick.state, KagTaskState::Finished);
        assert_eq!(tick.reason, KagYieldReason::Finished);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_tick_uses_tjs_unknown_tag_and_script_bridges() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "[iscript]\nf.value = 7;\n[endscript]\n[foo value=9]A",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let handler = engine
            .execute_script(
                "inline.tjs",
                r#"
                var f = new Dictionary();
                var handler = new Dictionary();
                handler.seen = "";
                handler.onUnknownTag = function(name, elm) {
                    this.seen = name + ":" + elm.value;
                    return 0;
                };
                return handler;
                "#,
            )
            .expect("handler")
            .object_handle()
            .unwrap_or_else(|| panic!("expected handler object"));

        engine.set_kag_handler(handler);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");

        assert_eq!(tick.state, KagTaskState::Finished);
        assert_eq!(
            engine.tjs_runtime().object_member(handler, "seen"),
            Variant::String("foo:9".to_string())
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "f.value")
                .expect("f value"),
            Variant::Integer(7)
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_unknown_tag_handler_may_complete_with_void() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[syspage storage=title][s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        let handler = engine
            .execute_script(
                "inline.tjs",
                r#"
                var handler = new Dictionary();
                handler.seen = "";
                handler.onUnknownTag = function(name, elm) {
                    this.seen = name + ":" + elm.storage;
                };
                return handler;
                "#,
            )
            .expect("handler")
            .object_handle()
            .unwrap_or_else(|| panic!("expected handler object"));
        engine.set_kag_handler(handler);

        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");

        assert_eq!(tick.state, KagTaskState::Finished);
        assert_eq!(
            engine.tjs_runtime().object_member(handler, "seen"),
            Variant::String("syspage:title".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_on_tag_handler_prevents_native_fallback_for_builtin_tags() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("bg.png"), 1, 1, &[255, 0, 0, 255]);
        fs::write(
            root.join("first.ks"),
            "[ch text=A][image storage=bg.png layer=base page=fore][trans time=1000][s]",
        )
        .expect("write scenario");

        let mut engine = image_test_engine(&root);
        let handler = engine
            .execute_script(
                "inline.tjs",
                r#"
                var handler = new Dictionary();
                handler.seen = "";
                handler.onTag = function(elm) {
                    this.seen += elm.tagname + ";";
                    return 0;
                };
                return handler;
                "#,
            )
            .expect("handler")
            .object_handle()
            .unwrap_or_else(|| panic!("expected handler object"));
        engine.set_kag_handler(handler);

        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(frame.tick.state, KagTaskState::Finished);
        assert_eq!(engine.message_layer().lines, Vec::<String>::new());
        assert!(frame.output.image_uploads.is_empty());
        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine.tjs_runtime().object_member(handler, "seen"),
            Variant::String("ch;image;trans;s;".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_handler_can_request_native_fallback_for_builtin_tag() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[ch text=A][s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        let handler = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var handler = new Dictionary();
                    handler.onTag = function(elm) {{
                        if(elm.tagname == "ch") return {};
                        return 0;
                    }};
                    return handler;
                    "#,
                    TJS_NATIVE_FALLBACK_STEP
                ),
            )
            .expect("handler")
            .object_handle()
            .unwrap_or_else(|| panic!("expected handler object"));
        engine.set_kag_handler(handler);

        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");

        assert_eq!(tick.state, KagTaskState::Finished);
        assert_eq!(engine.message_layer().lines, vec!["A".to_string()]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_without_tjs_handler_uses_native_fallback_message_path() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A[r]B[p]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");

        assert_eq!(tick.state, KagTaskState::WaitingClick);
        assert_eq!(
            engine.message_layer().lines,
            vec!["A".to_string(), "B".to_string()]
        );
        assert!(engine.message_layer().waiting_for_click);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_native_fallback_auto_mode_uses_line_and_page_timers() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A[l]B[p]C[s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine
            .execute_script(
                "auto_mode.tjs",
                r#"
                global.kag = new Dictionary();
                kag.autoMode = true;
                kag.autoModeLineWait = 7;
                kag.autoModePageWait = 9;
                "#,
            )
            .expect("auto setup");

        engine.load_kag_scenario("first.ks").expect("load scenario");
        let first = engine.tick().expect("first tick");
        assert_eq!(
            first.state,
            KagTaskState::WaitingTimer {
                remaining: Duration::from_millis(7)
            }
        );
        assert_eq!(
            engine.message_layer().lines,
            vec!["A".to_string(), String::new()]
        );
        assert!(!engine.message_layer().waiting_for_click);

        let second = engine.advance(Duration::from_millis(7)).expect("line wait");
        assert_eq!(
            second.state,
            KagTaskState::WaitingTimer {
                remaining: Duration::from_millis(9)
            }
        );
        assert!(!engine.message_layer().waiting_for_click);

        let third = engine.advance(Duration::from_millis(9)).expect("page wait");
        assert_eq!(third.state, KagTaskState::Finished);
        assert_eq!(engine.message_layer().lines, vec!["C".to_string()]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_native_cancel_auto_mode_tag_restores_click_wait() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[cancelautomode]A[p]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine
            .execute_script(
                "auto_mode.tjs",
                r#"
                global.kag = new Dictionary();
                kag.autoMode = true;
                kag.autoModePageWait = 1;
                "#,
            )
            .expect("auto setup");

        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");
        assert_eq!(tick.state, KagTaskState::WaitingClick);
        assert!(engine.message_layer().waiting_for_click);
        assert_eq!(
            engine
                .execute_expression("auto_mode_check.tjs", "kag.autoMode")
                .expect("auto mode"),
            Variant::Integer(0)
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_native_audio_fallback_does_not_route_to_unknown_handler() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("music.ogg"), b"bgm bytes").expect("write bgm");
        fs::write(root.join("first.ks"), "[playbgm storage=\"music.ogg\"][s]")
            .expect("write scenario");

        let mut engine = image_test_engine(&root);
        let handler = engine
            .execute_script(
                "inline.tjs",
                r#"
                var handler = new Dictionary();
                handler.seen = "";
                handler.onUnknownTag = function(name, elm) {
                    this.seen += name + ";";
                    return 0;
                };
                return handler;
                "#,
            )
            .expect("handler")
            .object_handle()
            .unwrap_or_else(|| panic!("expected handler object"));
        engine.set_kag_handler(handler);

        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");

        assert_eq!(tick.state, KagTaskState::Finished);
        assert_eq!(
            engine.tjs_runtime().object_member(handler, "seen"),
            Variant::String(String::new())
        );
        let commands = engine.host_mut().take_audio_commands();
        assert!(matches!(
            &commands[..],
            [AudioCommand::Play {
                bus: AudioBus::Bgm,
                source,
                ..
            }] if source.storage() == "music.ogg"
        ));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_global_conductor_handles_unknown_tag_without_explicit_session_handler() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[custom value=12][s]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.conductor = new Dictionary();
                kag.conductor.seen = "";
                kag.conductor.onUnknownTag = function(name, elm) {
                    this.seen = name + ":" + elm.value;
                    return 0;
                };
                "#,
            )
            .expect("setup kag conductor");

        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");

        assert_eq!(tick.state, KagTaskState::Finished);
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "kag.conductor.seen")
                .expect("seen"),
            Variant::String("custom:12".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_tjs_handler_positive_step_waits_and_resumes() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[waitclick][ch text=A]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        let handler = engine
            .execute_script(
                "inline.tjs",
                r#"
                var handler = new Dictionary();
                handler.onTag = function(elm) {
                    if(elm.tagname == "waitclick") return 5;
                    return 0;
                };
                return handler;
                "#,
            )
            .expect("handler")
            .object_handle()
            .unwrap_or_else(|| panic!("expected handler object"));
        engine.set_kag_handler(handler);

        engine.load_kag_scenario("first.ks").expect("load scenario");
        let first = engine.tick().expect("first tick");
        assert!(matches!(first.state, KagTaskState::WaitingTimer { .. }));
        assert_eq!(engine.message_layer().lines, Vec::<String>::new());

        let second = engine
            .advance(Duration::from_millis(5))
            .expect("resume after handler wait");
        assert_eq!(second.state, KagTaskState::Finished);
        assert_eq!(engine.message_layer().lines, Vec::<String>::new());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_tjs_handler_error_sets_task_state_error() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[ch text=A]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        let handler = engine
            .execute_script(
                "inline.tjs",
                r#"
                var handler = new Dictionary();
                // KAGEX writes handlers as `function (...) { ... } incontextof
                // global` (Override.tjs does this for every dictionary-stored
                // callback): a bare name inside a function assigned to a
                // Dictionary is read against that Dictionary first, and
                // `tTJSDictionaryObject::PropGet` answers void instead of
                // falling through to the global object
                // (`tjsDictionary.cpp:721`).
                handler.onTag = function(elm) {
                    throw new Exception("tag boom");
                } incontextof global;
                return handler;
                "#,
            )
            .expect("handler")
            .object_handle()
            .unwrap_or_else(|| panic!("expected handler object"));
        engine.set_kag_handler(handler);

        engine.load_kag_scenario("first.ks").expect("load scenario");
        let error = engine.tick().expect_err("handler error");

        assert!(
            error.to_string().contains("uncaught exception"),
            "unexpected error: {error}"
        );
        assert!(matches!(
            engine.kag_state(),
            KagTaskState::Error { message } if message.contains("uncaught exception")
        ));
        assert_eq!(engine.message_layer().lines, Vec::<String>::new());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_eval_tag_executes_tjs_expression_before_embedded_text() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "[eval exp=\"f.value = 7\"][emb exp=\"f.value\"]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script("inline.tjs", "var f = new Dictionary();")
            .expect("setup globals");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");

        assert_eq!(tick.state, KagTaskState::Finished);
        assert_eq!(engine.message_layer().lines, vec!["7".to_string()]);
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "f.value")
                .expect("f value"),
            Variant::Integer(7)
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_clear_message_tags_clear_default_message_text() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A[cm]B[er]C[ct]D").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let tick = engine.tick().expect("tick");

        assert_eq!(tick.state, KagTaskState::Finished);
        assert_eq!(engine.message_layer().lines, vec!["D".to_string()]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_update_renders_text_waits_for_click_and_continues() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "*start\nA[p]B[p]").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(960.0, 600.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("first update");
        assert_eq!(frame.tick.state, KagTaskState::WaitingClick);
        assert_eq!(frame.message_layer.lines, vec!["A".to_string()]);
        assert!(frame.message_layer.waiting_for_click);
        assert_eq!(frame.location.storage.as_deref(), Some("first.ks"));
        assert_eq!(frame.location.label.as_deref(), Some("*start"));
        assert!(frame.location.line.is_some());
        assert_eq!(frame.location.page, 1);
        assert!(frame.output.draw_commands.iter().any(
            |command| matches!(command, krkr_core::DrawCommand::Text(text) if text.text == "A")
        ));

        let frame = engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(960.0, 600.0), 0.016),
                    vec![EngineEvent::PointerInput {
                        button: PointerButton::Primary,
                        state: ButtonState::Released,
                    }],
                ),
                Duration::from_millis(16),
            )
            .expect("click update");
        assert_eq!(frame.tick.state, KagTaskState::WaitingClick);
        assert_eq!(frame.message_layer.lines, vec!["B".to_string()]);
        assert!(frame.message_layer.waiting_for_click);
        assert_eq!(frame.location.page, 2);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_image_tags_populate_core_layer_tree_and_frame_uploads() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("bg.png"), 2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
        fs::write(
            root.join("first.ks"),
            "[image storage=bg.png layer=base page=fore][layopt layer=base page=fore left=5 top=7 opacity=200][s]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(frame.tick.state, KagTaskState::Finished);
        assert_eq!(frame.output.image_uploads.len(), 1);
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 5.0
                        && image.rect.y == 7.0
                        && image.rect.width == 2.0
                        && image.rect.height == 1.0
                        && (image.opacity - (200.0 / 255.0)).abs() < 0.001
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_title_image_tags_create_ordered_render_layers() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("bg.png"), 2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
        write_png(
            root.join("logo.png"),
            2,
            1,
            &[0, 255, 0, 255, 0, 255, 0, 255],
        );
        fs::write(
            root.join("first.ks"),
            "[title_image file=bg.png zorder=100][title_logo file=logo.png zorder=120][s]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(frame.tick.state, KagTaskState::Finished);
        let layers = engine.host().layer_tree().layers().collect::<Vec<_>>();
        let background = layers
            .iter()
            .find(|layer| layer.name == "kag:__title_image")
            .expect("title background layer");
        let logo = layers
            .iter()
            .find(|layer| layer.name == "kag:__title_logo")
            .expect("title logo layer");
        assert!(background.z_order < logo.z_order);
        assert_eq!(
            background.image.as_ref().map(|image| image.upload.width),
            Some(2)
        );
        assert_eq!(
            logo.image.as_ref().map(|image| image.upload.height),
            Some(1)
        );
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(command, krkr_core::DrawCommand::Image(image) if image.texture_id == background.image.as_ref().expect("background image").upload.texture_id)
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_transition_tags_apply_immediately_without_blocking() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("back.png"), 1, 1, &[0, 255, 0, 255]);
        fs::write(
            root.join("first.ks"),
            "[image storage=back.png layer=base page=back][trans][wt][s]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(frame.tick.state, KagTaskState::Finished);
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(command, krkr_core::DrawCommand::Image(image) if image.rect.width == 1.0)
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_timed_transition_freezes_fore_applies_back_and_waits() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("fore.png"), 2, 1, &[255; 8]);
        write_png(root.join("back.png"), 4, 3, &[0; 48]);
        fs::write(
            root.join("first.ks"),
            concat!(
                "[image storage=fore.png layer=base page=fore]",
                "[image storage=back.png layer=base page=back]",
                "[trans method=crossfade time=1000][wt][s]"
            ),
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("start transition");

        assert_eq!(frame.tick.state, KagTaskState::WaitingTransition);
        let transition = frame.output.transitions.first().expect("transition");
        assert_eq!(transition.method, "crossfade");
        assert_eq!(transition.progress, 0.0);
        assert!(transition.frozen_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.width == 2.0 && image.rect.height == 1.0
            )
        }));
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.width == 4.0 && image.rect.height == 3.0
            )
        }));

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.5), Vec::new()),
                Duration::from_millis(500),
            )
            .expect("mid transition");
        assert_eq!(frame.tick.state, KagTaskState::WaitingTransition);
        assert_eq!(
            frame
                .output
                .transitions
                .first()
                .map(|transition| transition.progress),
            Some(0.5)
        );

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.6), Vec::new()),
                Duration::from_millis(600),
            )
            .expect("finish transition");
        assert_eq!(frame.tick.state, KagTaskState::Finished);
        assert!(frame.output.transitions.is_empty());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_universal_transition_carries_rule_image_and_options() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("fore.png"), 1, 1, &[255, 255, 255, 255]);
        write_png(root.join("back.png"), 1, 1, &[0, 0, 0, 255]);
        write_png(
            root.join("rule.png"),
            2,
            1,
            &[0, 0, 0, 255, 255, 255, 255, 255],
        );
        fs::write(
            root.join("first.ks"),
            concat!(
                "[image storage=fore.png layer=base page=fore]",
                "[image storage=back.png layer=base page=back]",
                "[trans method=universal rule=rule vague=32 time=1000][wt][s]"
            ),
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("start transition");

        let transition = frame.output.transitions.first().expect("transition");
        assert_eq!(transition.method, "universal");
        assert_eq!(
            transition.params.method,
            krkr_core::TransitionMethod::Universal
        );
        assert_eq!(transition.params.vague, 32.0);
        assert!(transition.rule_texture_id.is_some());
        assert!(transition.rule_image_upload.is_some());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_extrans_transition_methods_carry_effect_options() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("fore.png"), 1, 1, &[255, 255, 255, 255]);
        write_png(root.join("back.png"), 1, 1, &[0, 0, 0, 255]);
        fs::write(
            root.join("first.ks"),
            concat!(
                "[image storage=fore.png layer=base page=fore]",
                "[image storage=back.png layer=base page=back]",
                "[trans method=wave wavetype=2 maxh=20 maxomega=0.1 bgcolor1=0x80ff0000 bgcolor2=0x000000ff time=1000][wt][s]"
            ),
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("start transition");

        let transition = frame.output.transitions.first().expect("transition");
        assert_eq!(transition.method, "wave");
        assert_eq!(transition.params.method, krkr_core::TransitionMethod::Wave);
        assert_eq!(transition.params.wave_type, 2.0);
        assert_eq!(transition.params.max_h, 20.0);
        assert!((transition.params.max_omega - 0.1).abs() < 0.001);
        // `bgcolor*` are `tjs_uint32` ARGB values (`wave.cpp:345`/`:348`): the
        // top byte is alpha, so `0x000000ff` is a *transparent* blue.
        assert_eq!(
            transition.params.bg_color1,
            Color::new(1.0, 0.0, 0.0, 0x80 as f32 / 255.0)
        );
        assert_eq!(transition.params.bg_color2, Color::new(0.0, 0.0, 1.0, 0.0));
        // The tag's `time` is the handler's own millisecond clock.
        assert_eq!(transition.params.duration_millis, 1000.0);

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The `[trans]` tag's `time` reaches the kernels as the handler clock
    /// (`TransitionParams::duration_millis`) and every provider clamps it to the
    /// reference's 2 ms floor, so `time=1` runs a 2 ms transition and the
    /// kernels can never see a clock their ramps divide by zero.
    #[test]
    fn kag_transition_time_is_clamped_to_the_reference_floor() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("fore.png"), 1, 1, &[255, 255, 255, 255]);
        write_png(root.join("back.png"), 1, 1, &[0, 0, 0, 255]);
        fs::write(
            root.join("first.ks"),
            concat!(
                "[image storage=fore.png layer=base page=fore]",
                "[image storage=back.png layer=base page=back]",
                "[trans method=wave time=1][wt][s]"
            ),
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("start transition");
        let transition = frame.output.transitions.first().expect("transition");
        assert_eq!(transition.params.duration_millis, 2.0);

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 1.0), Vec::new()),
                Duration::from_millis(1),
            )
            .expect("mid transition");
        assert_eq!(
            frame
                .output
                .transitions
                .first()
                .map(|transition| transition.progress),
            Some(0.5)
        );

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 2.0), Vec::new()),
                Duration::from_millis(1),
            )
            .expect("finish transition");
        assert!(frame.output.transitions.is_empty());

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// An unknown `method` stops the scenario with the reference's message:
    /// KAG3 calls `beginTransition` directly from the tag
    /// (`MainWindow.tjs:5482-5487`), so the exception the provider lookup
    /// throws (`TransIntf.cpp:354`) reaches the Conductor instead of degrading
    /// the transition to a crossfade.
    #[test]
    fn kag_transition_unknown_method_reports_the_official_error() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("fore.png"), 1, 1, &[255, 255, 255, 255]);
        write_png(root.join("back.png"), 1, 1, &[0, 0, 0, 255]);
        fs::write(
            root.join("first.ks"),
            concat!(
                "[image storage=fore.png layer=base page=fore]",
                "[image storage=back.png layer=base page=back]",
                "[trans method=nope time=100][wt][s]"
            ),
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let error = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect_err("unknown transition method");
        assert!(
            error
                .to_string()
                .contains("Cannot find transition handler nope"),
            "unexpected error: {error}"
        );
        // The tag never started a transition; the error is what stops the
        // scenario (`process_native_fallback_tag` -> `advance`), the engine's
        // projection of the Conductor's catch (`Conductor.tjs:55-186`).
        assert!(engine.host().active_transition_count() == 0);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_transition_methods_include_krkr2_and_extrans_names() {
        for (method, expected) in [
            ("crossfade", krkr_core::TransitionMethod::Crossfade),
            ("universal", krkr_core::TransitionMethod::Universal),
            ("scroll", krkr_core::TransitionMethod::Scroll),
            ("wave", krkr_core::TransitionMethod::Wave),
            ("mosaic", krkr_core::TransitionMethod::Mosaic),
            ("turn", krkr_core::TransitionMethod::Turn),
            ("rotatezoom", krkr_core::TransitionMethod::RotateZoom),
            ("rotatevanish", krkr_core::TransitionMethod::RotateVanish),
            ("rotateswap", krkr_core::TransitionMethod::RotateSwap),
            ("ripple", krkr_core::TransitionMethod::Ripple),
        ] {
            let root = temp_root();
            fs::create_dir_all(&root).expect("create temp root");
            write_png(root.join("fore.png"), 1, 1, &[255, 255, 255, 255]);
            write_png(root.join("back.png"), 1, 1, &[0, 0, 0, 255]);
            write_png(root.join("rule.png"), 1, 1, &[0, 0, 0, 255]);
            fs::write(
                root.join("first.ks"),
                format!(
                    "[image storage=fore.png layer=base page=fore]\
                     [image storage=back.png layer=base page=back]\
                     [trans method={method} rule=rule.png time=1000][wt][s]"
                ),
            )
            .expect("write scenario");

            let mut engine = KrkrEngine::for_project(&root).expect("engine");
            engine.load_kag_scenario("first.ks").expect("load scenario");
            let frame = engine
                .update(
                    EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                    Duration::ZERO,
                )
                .expect("start transition");
            let transition = frame.output.transitions.first().expect("transition");
            assert_eq!(transition.method, method);
            assert_eq!(transition.params.method, expected);

            fs::remove_dir_all(root).expect("cleanup");
        }
    }

    #[test]
    fn kag_backlay_copies_fore_layers_for_transition() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 2, 1, &[255; 8]);
        fs::write(
            root.join("first.ks"),
            concat!(
                "[image storage=sprite.png layer=0 page=fore left=5 top=7 visible=true]",
                "[backlay]",
                "[trans method=crossfade time=1000][wt][s]"
            ),
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("start transition");

        assert_eq!(frame.tick.state, KagTaskState::WaitingTransition);
        assert!(!frame.output.transitions.is_empty());
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 5.0 && image.rect.y == 7.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_image_tag_replaces_previous_layer_size_when_geometry_is_implicit() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("small.png"), 2, 1, &[255; 8]);
        write_png(root.join("large.png"), 4, 3, &[0; 48]);
        fs::write(
            root.join("first.ks"),
            "[image storage=small.png layer=base page=fore][image storage=large.png layer=base page=fore][s]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.width == 4.0 && image.rect.height == 3.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_transition_uses_replacement_image_size() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("small.png"), 2, 1, &[255; 8]);
        write_png(root.join("large.png"), 4, 3, &[0; 48]);
        fs::write(
            root.join("first.ks"),
            "[image storage=small.png layer=base page=back][trans][wt][image storage=large.png layer=base page=back][trans][wt][s]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.width == 4.0 && image.rect.height == 3.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_current_supplies_default_visual_layer_target() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 1, 1, &[255, 0, 255, 255]);
        fs::write(
            root.join("first.ks"),
            "[current layer=1 page=fore][image storage=sprite.png][layopt left=31 top=37][s]",
        )
        .expect("write scenario");

        let mut engine = image_test_engine(&root);
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(frame.tick.state, KagTaskState::Finished);
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 31.0 && image.rect.y == 37.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_load_images_uses_storage_decode_path() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(
            root.join("sprite.png"),
            1,
            2,
            &[255, 255, 0, 255, 0, 255, 255, 255],
        );

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.loadImages("sprite.png");
                layer.visible = true;
                layer.left = 11;
                layer.top = 13;
                layer.opacity = 128;
                return layer.width + ":" + layer.height;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("1:2".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert_eq!(frame.output.image_uploads.len(), 1);
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 11.0
                        && image.rect.y == 13.0
                        && (image.opacity - (128.0 / 255.0)).abs() < 0.001
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_load_images_preserves_hidden_temporary_layer() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("thumb.png"), 2, 2, &[255; 16]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var win = new Window();
                var primary = new Layer(win, null);
                var temp = new Layer(win, primary);
                temp.loadImages("thumb.png");
                return temp.visible + ":" + temp.imageWidth + ":" + temp.imageHeight;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("0:2:2".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(!frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 0.0 && image.rect.y == 0.0 && image.rect.width == 2.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn later_layer_super_constructor_reparents_the_native_layer() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("child.png"), 1, 1, &[255, 255, 255, 255]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class DeferredLayer extends Layer {
                    function DeferredLayer(owner, parent) {
                        super.Layer();
                        super.Layer(owner, parent);
                    }
                }
                global.ownerProbe = new Window();
                global.rootProbe = new Layer(ownerProbe, null);
                global.parentProbe = new Layer(ownerProbe, rootProbe);
                global.childProbe = new DeferredLayer(ownerProbe, parentProbe);
                ownerProbe.visible = true;
                rootProbe.visible = true;
                parentProbe.visible = false;
                childProbe.loadImages("child.png");
                childProbe.visible = true;
                "#,
            )
            .expect("script");

        let parent = object_handle(&engine, "parentProbe");
        let child = object_handle(&engine, "childProbe");
        let parent_id = engine
            .host()
            .native_layer(parent)
            .expect("parent native layer");
        let child_id = engine
            .host()
            .native_layer(child)
            .expect("child native layer");
        assert_eq!(
            engine
                .host()
                .layer_tree()
                .layer(child_id)
                .expect("child node")
                .parent,
            Some(parent_id)
        );

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("frame");
        // `new Layer()` always carries the ctor's 32×32 transparent holder
        // (`AllocateDefaultImage`, `LayerIntf.cpp:404`), so the visible
        // `rootProbe` contributes an image command. The 1×1 child under the
        // hidden parent must not.
        assert!(
            !frame.output.draw_commands.iter().any(|command| matches!(
                command,
                krkr_core::DrawCommand::Image(image) if image.texture_size.width == 1.0
            ))
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_font_methods_measure_and_layer_draw_text_updates_pixels() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var font = new Font();
                font.height = 24;
                global.textLayer = new Layer();
                textLayer.setSize(96, 48);
                textLayer.font.height = 24;
                textLayer.drawText(4, 4, "A", 0xffffff, 255);
                global.imageFunctionLayer = new Layer();
                imageFunctionLayer.setSize(96, 48);
                ImageFunction.drawText(imageFunctionLayer, 4, 4, "B", 0xffffff, 255);
                return font.getTextWidth("A") + ":" + font.getTextHeight("A") + ":" + textLayer.__nativeLayerId + ":" + imageFunctionLayer.__nativeLayerId;
                "#,
            )
            .expect("script");
        let Variant::String(result) = result else {
            panic!("expected string result");
        };
        let parts = result.split(':').collect::<Vec<_>>();
        assert!(parts[0].parse::<i64>().expect("width") > 0);
        assert!(parts[1].parse::<i64>().expect("height") > 0);
        let layer_id = parts[2].parse::<u64>().expect("layer id");
        let image_function_layer_id = parts[3].parse::<u64>().expect("image function layer id");
        for layer_id in [layer_id, image_function_layer_id] {
            let image = engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .and_then(|layer| layer.image.as_ref())
                .expect("drawText image");
            assert!(image.upload.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
        }
    }

    #[test]
    fn native_font_methods_treat_negative_height_as_pixel_size() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var font = new Font();
                font.height = -24;
                global.textLayer = new Layer();
                textLayer.setSize(96, 48);
                textLayer.font.height = -24;
                textLayer.drawText(4, 4, "A", 0xffffff, 255);
                return font.getTextHeight("A") + ":" + textLayer.__nativeLayerId;
                "#,
            )
            .expect("script");
        let Variant::String(result) = result else {
            panic!("expected string result");
        };
        let parts = result.split(':').collect::<Vec<_>>();
        assert!(parts[0].parse::<i64>().expect("height") >= 20);
        let layer_id = parts[1].parse::<u64>().expect("layer id");
        let image = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("drawText image");
        let alpha_pixels = image
            .upload
            .rgba
            .chunks_exact(4)
            .filter(|pixel| pixel[3] != 0)
            .count();
        assert!(alpha_pixels > 20, "alpha_pixels={alpha_pixels}");
    }

    #[test]
    fn native_layer_draw_text_applies_edge_pixels() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.textLayer = new Layer();
                textLayer.setSize(96, 48);
                textLayer.font.height = -24;
                textLayer.drawText(8, 8, "A", 0x000000, 255, true, 512, 0x0080ff, 1, 0, 0);
                return textLayer.__nativeLayerId;
                "#,
            )
            .expect("script");
        let layer_id = result.to_integer().expect("layer id") as u64;
        let image = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("drawText image");
        let colored_pixels = image
            .upload
            .rgba
            .chunks_exact(4)
            .filter(|pixel| pixel[3] != 0 && (pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0))
            .count();
        assert!(colored_pixels > 0, "colored_pixels={colored_pixels}");
    }

    #[test]
    fn script_class_can_call_grandparent_native_constructor() {
        let mut engine = KrkrEngine::for_project(&temp_root()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                class KAGLayer extends Layer { var moveObject; }
                class SliderLayer extends KAGLayer {
                    function SliderLayer(win, par) { super.Layer(win, par); width = 5; }
                }
                class SliderLayer2 extends KAGLayer {
                    function SliderLayer2(win, par) { global.KAGLayer.Layer(win, par); width = 6; }
                }
                var window = new Window();
                var a = new SliderLayer(window, null);
                var b = new SliderLayer2(window, null);
                return a.width + ":" + b.width;
                "#,
            )
            .expect("script");
        assert_eq!(result, Variant::String("5:6".to_string()));
    }

    #[test]
    fn nested_class_reusing_native_class_name_calls_the_native_constructor() {
        // Same shadowing pattern as the bytecode-class case, but against a
        // native base: `super.Layer()` must run the native initializer rather
        // than the derived constructor regmember copied onto the instance.
        let mut engine = KrkrEngine::for_project(&temp_root()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                class Outer {
                    function Outer() {}
                    class Layer extends Layer {
                        function Layer(win, par) { super.Layer(win, par); width = 7; }
                    }
                    function make(win) { return new this.Layer(win, null); }
                }
                var window = new Window();
                var layer = (new Outer()).make(window);
                return layer.width;
                "#,
            )
            .expect("script");
        assert_eq!(result, Variant::Integer(7));
    }

    #[test]
    fn native_layer_load_images_keeps_native_this_through_nested_super_calls() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 1, 1, &[255, 0, 0, 255]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.calls = "";
                class KAGLayer extends Layer {
                    function KAGLayer(win, par) { super.Layer(win, par); }
                    function loadImages(storage) {
                        global.calls += "K";
                        return super.loadImages(storage);
                    }
                }
                class AnimationLayer extends KAGLayer {
                    function AnimationLayer(win, par) { super.KAGLayer(win, par); }
                    function loadImages(elm) {
                        global.calls += "A";
                        return super.loadImages(elm.storage);
                    }
                }
                class GraphicLayer extends AnimationLayer {
                    function GraphicLayer(win, par) { super.AnimationLayer(win, par); }
                    function loadImages(elm) {
                        global.calls += "G";
                        return super.loadImages(elm);
                    }
                }
                class BaseLayer extends GraphicLayer {
                    function BaseLayer(win, par) { super.GraphicLayer(win, par); }
                    function loadImages(elm) {
                        global.calls += "B";
                        return super.loadImages(elm);
                    }
                }
                var window = new Window();
                var layer = new BaseLayer(window, null);
                layer.visible = true;
                layer.left = 11;
                layer.loadImages(%[storage : "sprite.png"]);
                return global.calls + ":" + layer.imageWidth + ":" + layer.imageHeight;
                "#,
            )
            .expect("script");
        assert_eq!(result, Variant::String("BGAK:1:1".to_string()));

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 11.0 && image.rect.width == 1.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Official `tTJSNI_BaseLayer::LoadImages` (`LayerIntf.cpp:2494`) ends with
    /// `InternalSetImageSize(MainImage->GetWidth(), MainImage->GetHeight())`,
    /// whose first step is `if(width < Rect.get_width()) SetWidth(width)`
    /// (`LayerIntf.cpp:2353`). A smaller loaded image therefore shrinks the
    /// layer rectangle; callers that need a fixed viewport set the size after
    /// loading.
    #[test]
    fn native_layer_load_images_shrinks_the_layer_rect_like_internal_set_image_size() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 2, 3, &[255; 24]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(1280, 720);
                layer.setSizeToImageSize();
                layer.loadImages("sprite.png");
                return layer.width + ":" + layer.height + ":" +
                    layer.imageWidth + ":" + layer.imageHeight;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("2:3:2:3".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_super_size_assignment_updates_bound_instance() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("button.png"), 6, 2, &[255; 48]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                class ButtonLikeLayer extends Layer {
                    function ButtonLikeLayer() { super.Layer(...); }
                    function loadButtonImage(storage) {
                        super.loadImages(storage);
                        super.width = imageWidth \ 3;
                        super.height = imageHeight;
                    }
                }
                var layer = new ButtonLikeLayer();
                layer.loadButtonImage("button.png");
                return layer.width + ":" + layer.height + ":" + layer.imageWidth;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("2:2:6".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `tTJSNI_BaseLayer` image semantics, straight from `LayerIntf.cpp`:
    /// ctor `AllocateDefaultImage` (`:404`) installs the 32×32 transparent
    /// holder, `SetWidth` → `ImageLayerSizeChanged` (`:2388`) grows the bitmap,
    /// `SetImageWidth` (`:2296`) shrinks the Rect before `ChangeImageSize`,
    /// `SetHasImage(false)` (`:2228`) deallocates, and reading `imageWidth`
    /// without a bitmap throws `TVPNotDrawableLayerType` (`:2321`).
    #[test]
    fn native_layer_image_size_matches_official_main_image() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                var initial = layer.hasImage + ":" + layer.width + ":" + layer.height +
                    ":" + layer.imageWidth + ":" + layer.imageHeight;
                layer.width = 64;
                var grown = layer.width + ":" + layer.imageWidth;
                layer.imageWidth = 10;
                var shrunk = layer.width + ":" + layer.imageWidth;
                layer.hasImage = false;
                var freed = layer.hasImage;
                var thrown = "";
                try { thrown += layer.imageWidth; } catch(e) { thrown = "get"; }
                layer.hasImage = true;
                var restored = layer.hasImage + ":" + layer.width + ":" + layer.imageWidth;
                return initial + ":" + grown + ":" + shrunk + ":" + freed + ":" +
                    thrown + ":" + restored;
                "#,
            )
            .expect("script");

        assert_eq!(
            result,
            Variant::String("1:32:32:32:32:64:64:10:10:0:get:1:10:10".to_string())
        );
    }

    #[test]
    fn native_layer_constructor_preserves_script_property_setters() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                class SliderLikeLayer extends Layer {
                    var changes = 0;
                    function SliderLikeLayer() { super.Layer(...); }
                    property width {
                        setter(x) {
                            super.width = x;
                            imageWidth = x;
                            changes = changes + 1;
                        }
                        getter { return super.width; }
                    }
                    property height {
                        setter(y) {
                            super.height = y;
                            imageHeight = y;
                        }
                        getter { return super.height; }
                    }
                }
                var layer = new SliderLikeLayer();
                var initial = layer.width + ":" + layer.imageWidth;
                layer.width = 430;
                layer.height = 24;
                layer.width = 320;
                return initial + ":" + layer.width + ":" + layer.imageWidth + ":" +
                    layer.height + ":" + layer.imageHeight + ":" + layer.changes;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("32:32:320:320:24:24:2".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_set_size_to_image_size_uses_script_image_members() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.visible = true;
                layer.imageWidth = 10;
                layer.imageHeight = 12;
                layer.setSizeToImageSize();
                layer.fillRect(0, 0, 10, 12, 0xffffffff);
                return layer.width + ":" + layer.height + ":" +
                    layer.imageWidth + ":" + layer.imageHeight;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("10:12:10:12".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.width == 10.0 && image.rect.height == 12.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_set_pos_accepts_optional_size() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.visible = true;
                layer.setImageSize(20, 30);
                layer.fillRect(0, 0, 20, 30, 0xffffffff);
                layer.setPos(12, 34, 20, 30);
                return layer.left + ":" + layer.top + ":" +
                    layer.width + ":" + layer.height;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("12:34:20:30".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 12.0
                        && image.rect.y == 34.0
                        && image.rect.width == 20.0
                        && image.rect.height == 30.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_load_images_accepts_kag_dictionary_options() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 1, 1, &[0, 0, 255, 255]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.loadImages(%[
                    storage: "sprite.png",
                    visible: false,
                    left: 17,
                    top: 19,
                    opacity: 64
                ]);
                return layer.imageWidth + ":" + layer.visible + ":" + layer.left + ":" + layer.opacity;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("1:0:17:64".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_load_images_for_kag_target_replaces_previous_layer_size() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("small.png"), 2, 1, &[255; 8]);
        write_png(root.join("large.png"), 4, 3, &[0; 48]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.__nativeLayerId = 0;
                layer.loadImages(%[storage: "small.png", page: "fore", layer: "base"]);
                layer.loadImages(%[storage: "large.png", page: "fore", layer: "base"]);
                return "done";
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.width == 4.0 && image.rect.height == 3.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_kag_back_message_draw_text_is_staged_until_transition() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.back.messages[0] = new Layer();
                kag.back.messages[0].visible = true;
                kag.back.messages[0].setSize(160, 48);
                kag.back.messages[0].font.height = -24;
                kag.back.messages[0].drawText(4, 4, "BACK", 0xffffff, 255);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("pre-transition update");
        assert_eq!(image_command_count(&frame), 0);

        engine.host_mut().apply_immediate_transition();
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("post-transition update");
        assert_eq!(image_command_count(&frame), 1);
    }

    #[test]
    fn native_kag_fore_message_draw_text_renders_once() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.fore.messages[0] = new Layer();
                kag.fore.messages[0].visible = true;
                kag.fore.messages[0].setSize(160, 48);
                kag.fore.messages[0].font.height = -24;
                kag.fore.messages[0].drawText(4, 4, "FORE", 0xffffff, 255);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert_eq!(image_command_count(&frame), 1);
    }

    #[test]
    fn native_kag_slot_mapping_tracks_fore_back_base_layers_and_messages() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(null, kag.fore.base), layers: [], messages: []];
                kag.fore.layers[0] = new Layer(null, kag.fore.base);
                kag.back.layers[0] = new Layer(null, kag.back.base);
                kag.fore.messages[0] = new Layer(null, kag.fore.base);
                kag.back.messages[0] = new Layer(null, kag.back.base);
                kag.back.layers[0].left = 42;
                kag.back.messages[0].visible = true;
                "#,
            )
            .expect("script");

        let kag = object_handle(&engine, "kag");
        let fore = member_object(&engine, kag, "fore");
        let back = member_object(&engine, kag, "back");
        let fore_base = member_object(&engine, fore, "base");
        let back_layers = member_object(&engine, back, "layers");
        let back_layer0 = member_object(&engine, back_layers, "0");
        let back_messages = member_object(&engine, back, "messages");
        let back_message0 = member_object(&engine, back_messages, "0");

        let fore_base_slot = engine.host().kag_layer_slot(fore_base).expect("fore slot");
        assert_eq!(fore_base_slot.page, "fore");
        assert_eq!(fore_base_slot.layer, "base");
        let back_layer_slot = engine
            .host()
            .kag_layer_slot(back_layer0)
            .expect("back layer slot");
        assert_eq!(back_layer_slot.page, "back");
        assert_eq!(back_layer_slot.layer, "0");
        let back_message_slot = engine
            .host()
            .kag_layer_slot(back_message0)
            .expect("back message slot");
        assert_eq!(back_message_slot.page, "back");
        assert_eq!(back_message_slot.layer, "message0");
        assert_eq!(
            engine.host().kag_layer("back", "0").map(|layer| layer.left),
            Some(42.0)
        );
        let native_back_layer = engine
            .host()
            .native_layer(back_layer0)
            .expect("native layer");
        assert!(
            !engine
                .host()
                .layer_tree()
                .layer(native_back_layer)
                .expect("native layer node")
                .renderable
        );
    }

    #[test]
    fn native_kag_backlay_stages_back_state_without_polluting_fore_object_properties() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(null, kag.fore.base), layers: [], messages: []];
                kag.fore.layers[0] = new Layer(null, kag.fore.base);
                kag.back.layers[0] = new Layer(null, kag.back.base);
                kag.fore.layers[0].left = 5;
                kag.fore.layers[0].top = 7;
                kag.fore.layers[0].visible = true;
                "#,
            )
            .expect("setup");
        engine.host_mut().backlay_kag_layers(Some("0"));
        engine
            .execute_script(
                "inline.tjs",
                r#"
                kag.back.layers[0].left = 40;
                kag.back.layers[0].top = 50;
                "#,
            )
            .expect("stage back");

        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "kag.fore.layers[0].left + ':' + kag.fore.layers[0].top",
                )
                .expect("fore props"),
            Variant::String("5:7".to_string())
        );
        let back = engine.host().kag_layer("back", "0").expect("back layer");
        assert_eq!(back.left, 40.0);
        assert_eq!(back.top, 50.0);
    }

    #[test]
    fn native_kag_fore_base_keeps_native_tree_for_hit_testing() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.clicks = 0;
                kag.onPrimaryClick = function() { this.clicks++; };
                kag.fore.base.visible = true;
                kag.fore.base.setSize(100, 100);
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(5.0, 5.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "kag.clicks")
                .expect("clicks"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn native_kag_exchange_restores_fore_tree_rendering_and_hit_testing() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(null, kag.fore.base), layers: [], messages: []];
                kag.fore.base.visible = true;
                kag.fore.base.setSize(100, 100);
                kag.back.base.visible = true;
                kag.back.base.setSize(100, 100);
                kag.back.messages[0] = new Layer(null, kag.back.base);
                kag.back.messages[0].visible = true;
                kag.back.messages[0].setSize(20, 20);
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync back staging");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var tmp = kag.fore;
                kag.fore = kag.back;
                kag.back = tmp;
                kag.clicks = 0;
                kag.fore.messages[0].setImageSize(20, 20);
                kag.fore.messages[0].colorRect(0, 0, 20, 20, 0xffffff, 255);
                kag.fore.messages[0].onMouseDown = function(x, y, shift) {
                    kag.clicks++;
                };
                "#,
            )
            .expect("exchange");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync fore");
        assert_eq!(content_image_command_count(&engine, &frame), 1);

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(5.0, 5.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "kag.clicks")
                .expect("clicks"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn native_sourced_transition_exchanges_tree_so_children_follow_their_layer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                global.win = new Window();
                kag.fore = %[base: new Layer(win), layers: [], messages: []];
                kag.back = %[base: new Layer(win, kag.fore.base), layers: [], messages: []];
                kag.fore.base.comp = kag.back.base;
                kag.back.base.comp = kag.fore.base;
                kag.fore.base.visible = true;
                kag.fore.base.setSize(100, 100);
                kag.back.base.visible = false;
                kag.back.base.setSize(100, 100);
                kag.fore.messages[0] = new Layer(null, kag.fore.base);
                kag.back.messages[0] = new Layer(null, kag.back.base);
                kag.fore.messages[0].visible = true;
                kag.fore.messages[0].setSize(20, 20);
                kag.back.messages[0].visible = true;
                kag.back.messages[0].setSize(20, 20);
                kag.back.messages[0].setImageSize(20, 20);
                kag.back.messages[0].colorRect(0, 0, 20, 20, 0xffffff, 255);
                kag.back.messages[0].focusable = true;
                kag.back.messages[0].focus();
                kag.clicks = 0;
                kag.back.messages[0].onMouseDown = function(x, y, shift) {
                    kag.clicks++;
                };
                kag.fore.base.onTransitionCompleted = function(dest, src) {
                    var tmp = kag.fore;
                    kag.fore = kag.back;
                    kag.back = tmp;
                };
                "#,
            )
            .expect("setup");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync");
        engine
            .execute_script(
                "inline.tjs",
                r#"kag.fore.base.beginTransition("crossfade", true, kag.back.base, %[time: 1]);"#,
            )
            .expect("begin transition");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                // The crossfade family floors `time` at 2 ms
                // (`TransIntf.cpp:530`).
                Duration::from_millis(2),
            )
            .expect("complete transition");
        assert_eq!(content_image_command_count(&engine, &frame), 1);
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    // `parent` and `focusedLayer` are `tTJSVariant(dsp, dsp)`
                    // reads (`LayerIntf.cpp:8490`, `WindowIntf.cpp:1824`), so
                    // identity is what `==` reports.
                    "kag.fore.messages[0].parent == kag.fore.base && kag.fore.base.visible && win.focusedLayer === null"
                )
                .expect("message stays with its base and exchange blurs the old focus"),
            Variant::Integer(1)
        );

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(5.0, 5.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "kag.clicks")
                .expect("clicks"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn native_kag_base_transition_carries_back_children_as_source_face() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 4, 4, &[255; 64]);
        write_png(root.join("new.png"), 4, 4, &[0, 255, 0, 255].repeat(16));

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.fore.base.visible = true;
                kag.fore.base.setSize(200, 200);
                kag.back.base.visible = true;
                kag.back.base.setSize(200, 200);
                kag.fore.layers[0] = new Layer(null, kag.fore.base);
                kag.back.layers[0] = new Layer(null, kag.back.base);
                kag.fore.layers[0].loadImages(%[
                    storage: "old.png",
                    visible: true,
                    left: 5,
                    top: 7
                ]);
                kag.back.layers[0].loadImages(%[
                    storage: "new.png",
                    visible: true,
                    left: 40,
                    top: 50
                ]);
                "#,
            )
            .expect("setup");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync");
        // `Layer.window` has no setter (`LayerIntf.cpp:8689`), so the KAG
        // `transCount` relay is attached through the host the way the engine's
        // own internals do.
        engine
            .execute_script("inline.tjs", "global.transWindow = %[transCount: 1];")
            .expect("relay");
        attach_layer_window(&mut engine, "kag.fore.base", "transWindow");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                kag.fore.base.inTransition = true;
                kag.fore.base.beginTransition(
                    "crossfade",
                    true,
                    kag.back.base,
                    %[time: 1000]
                );
                "#,
            )
            .expect("begin transition");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("transition frame");
        let transition = frame.output.transitions.first().expect("transition");
        assert!(transition.frozen_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 5.0 && image.rect.y == 7.0
            )
        }));
        // The incoming page is the transition's own source face, not the live
        // tree: official writes neither layer while the transition runs
        // (`tTransDrawable::DrawCompleted`, `LayerIntf.cpp:6567-6681`).  The
        // staged layer appears exactly once -- it is both a page layer and (in
        // this engine) an independent object, and a duplicate root would double
        // every semi-transparent pixel.
        let face = transition
            .source_draw_commands
            .iter()
            .filter_map(|command| match command {
                krkr_core::DrawCommand::Image(image) => Some(image.rect),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            face,
            vec![
                krkr_core::Rect::new(0.0, 0.0, 200.0, 200.0),
                krkr_core::Rect::new(40.0, 50.0, 4.0, 4.0),
            ]
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_kag_base_transition_uses_unsynced_back_child_geometry() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 4, 4, &[255; 64]);
        write_png(root.join("new.png"), 4, 4, &[0, 255, 0, 255].repeat(16));

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.fore.base.visible = true;
                kag.fore.base.setSize(200, 200);
                kag.back.base.visible = true;
                kag.back.base.setSize(200, 200);
                kag.fore.layers[0] = new Layer(null, kag.fore.base);
                kag.back.layers[0] = new Layer(null, kag.back.base);
                kag.fore.layers[0].loadImages("old.png");
                kag.fore.layers[0].setSizeToImageSize();
                kag.fore.layers[0].setPos(5, 7, 4, 4);
                kag.fore.layers[0].visible = true;
                "#,
            )
            .expect("setup");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync fore");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                kag.back.layers[0].loadImages("new.png");
                kag.back.layers[0].setSizeToImageSize();
                kag.back.layers[0].left = 40;
                kag.back.layers[0].top = 50;
                kag.back.layers[0].visible = true;
                kag.fore.base.beginTransition(
                    "crossfade",
                    true,
                    kag.back.base,
                    %[time: 1000]
                );
                "#,
            )
            .expect("begin transition");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("transition frame");
        let transition = frame.output.transitions.first().expect("transition");
        assert!(transition.frozen_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 5.0 && image.rect.y == 7.0
            )
        }));
        // The staging page's own geometry (`left`/`top` written after
        // `loadImages`) reaches the transition through the page model.
        assert!(transition.source_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 40.0 && image.rect.y == 50.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_kag_base_transition_uses_unsynced_back_child_visibility() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 4, 4, &[255; 64]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.fore.base.visible = true;
                kag.fore.base.setSize(200, 200);
                kag.back.base.visible = true;
                kag.back.base.setSize(200, 200);
                kag.fore.layers[0] = new Layer(null, kag.fore.base);
                kag.back.layers[0] = new Layer(null, kag.back.base);
                kag.fore.layers[0].loadImages("sprite.png");
                kag.fore.layers[0].setSizeToImageSize();
                kag.fore.layers[0].setPos(5, 7, 4, 4);
                kag.fore.layers[0].visible = true;
                "#,
            )
            .expect("setup");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync fore");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                kag.back.layers[0].loadImages("sprite.png");
                kag.back.layers[0].setSizeToImageSize();
                kag.back.layers[0].left = 5;
                kag.back.layers[0].top = 7;
                kag.back.layers[0].visible = false;
                kag.fore.base.beginTransition(
                    "crossfade",
                    true,
                    kag.back.base,
                    %[time: 1000]
                );
                "#,
            )
            .expect("begin transition");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("transition frame");
        let transition = frame.output.transitions.first().expect("transition");
        assert!(transition.frozen_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 5.0 && image.rect.y == 7.0
            )
        }));
        // An invisible staging child stays invisible in the source face
        // (`InternalComplete2` draws only visible children).
        // An invisible staging child stays invisible in the source face
        // (`InternalComplete2` draws only visible children).
        assert!(!transition.source_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 5.0 && image.rect.y == 7.0
            )
        }));
        // The destination keeps drawing its own content.
        assert_eq!(content_image_command_count(&engine, &frame), 1);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_kag_base_transition_keeps_live_base_visible_when_back_base_hidden() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 4, 4, &[255; 64]);
        write_png(root.join("new.png"), 4, 4, &[0, 255, 0, 255].repeat(16));

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.fore.base.loadImages("old.png");
                kag.fore.base.setSizeToImageSize();
                kag.fore.base.visible = true;
                kag.back.base.loadImages("new.png");
                kag.back.base.setSizeToImageSize();
                kag.back.base.visible = false;
                "#,
            )
            .expect("setup");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync fore");
        assert_eq!(image_command_count(&frame), 1);

        engine
            .execute_script(
                "inline.tjs",
                r#"
                kag.fore.base.beginTransition(
                    "crossfade",
                    true,
                    kag.back.base,
                    %[time: 1000]
                );
                "#,
            )
            .expect("begin transition");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("transition frame");
        let transition = frame.output.transitions.first().expect("transition");
        assert!(transition.frozen_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 0.0
                        && image.rect.y == 0.0
                        && image.rect.width == 4.0
                        && image.rect.height == 4.0
            )
        }));
        assert_eq!(image_command_count(&frame), 1);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_kag_base_transition_exchanges_back_children_into_fore() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 4, 4, &[255; 64]);
        write_png(root.join("new.png"), 4, 4, &[0, 255, 0, 255].repeat(16));

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.fore.base.visible = true;
                kag.fore.base.setSize(200, 200);
                kag.back.base.visible = true;
                kag.back.base.setSize(200, 200);
                kag.fore.layers[0] = new Layer(null, kag.fore.base);
                kag.back.layers[0] = new Layer(null, kag.back.base);
                kag.fore.layers[0].loadImages("old.png");
                kag.fore.layers[0].setSizeToImageSize();
                kag.fore.layers[0].setPos(5, 7, 4, 4);
                kag.fore.layers[0].visible = true;
                kag.fore.base.onTransitionCompleted = function(dest, src) {
                    var tmp = kag.fore;
                    kag.fore = kag.back;
                    kag.back = tmp;
                };
                "#,
            )
            .expect("setup");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync fore");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                kag.back.layers[0].loadImages("new.png");
                kag.back.layers[0].setSizeToImageSize();
                kag.back.layers[0].left = 40;
                kag.back.layers[0].top = 50;
                kag.back.layers[0].visible = true;
                kag.fore.base.beginTransition(
                    "crossfade",
                    true,
                    kag.back.base,
                    %[time: 1000]
                );
                "#,
            )
            .expect("begin transition");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("transition frame");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 1.0), Vec::new()),
                Duration::from_millis(1000),
            )
            .expect("complete transition");
        assert!(frame.output.transitions.is_empty());
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 40.0 && image.rect.y == 50.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The save-screen close (`sysscn/save.ks *return`) is `[backlay]` ->
    /// `[syspage free page=back]` -> `[systrans save.close]`.  The only
    /// clearing operation in the whole path hides the *back* page's message
    /// layer, and the transition then publishes that page (`KAGWindow.
    /// onTransitionEnd` copies the message layers across the pages and swaps
    /// them).  The hidden state written to the back page has to survive the
    /// exchange: the page the transition publishes must not draw the outgoing
    /// save image again.
    #[test]
    fn kag_save_close_message_layer_hidden_on_the_back_page_stays_hidden_after_the_exchange() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .host_mut()
            .set_transition_policy(crate::TransitionPolicy::Immediate);
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                global.win = new Window();
                kag.fore = %[base: new Layer(win), layers: [], messages: []];
                kag.back = %[base: new Layer(win), layers: [], messages: []];
                kag.fore.base.comp = kag.back.base;
                kag.back.base.comp = kag.fore.base;
                kag.fore.base.visible = true;
                kag.back.base.visible = true;
                kag.fore.messages[1] = new Layer(win, kag.fore.base);
                kag.back.messages[1] = new Layer(win, kag.back.base);
                kag.fore.messages[1].comp = kag.back.messages[1];
                kag.back.messages[1].comp = kag.fore.messages[1];
                // The save screen is drawn on the visible page's message1.
                kag.fore.messages[1].setSize(64, 48);
                kag.fore.messages[1].fillRect(0, 0, 64, 48, 0x2244ff);
                kag.fore.messages[1].visible = true;
                // [backlay]: the back page takes the visible page's content.
                kag.back.messages[1].assignImages(kag.fore.messages[1]);
                kag.back.messages[1].setSize(64, 48);
                kag.back.messages[1].visible = true;
                // [syspage free page=back]: the close's clearing op.
                kag.back.messages[1].visible = false;
                kag.fore.base.onTransitionCompleted = function(dest, src) {
                    // KAGWindow.onTransitionEnd: the outgoing page's message
                    // layer takes the incoming page's state, then the pages
                    // swap.
                    kag.fore.messages[1].assignImages(kag.back.messages[1]);
                    kag.fore.messages[1].visible = kag.back.messages[1].visible;
                    var tmp = kag.fore;
                    kag.fore = kag.back;
                    kag.back = tmp;
                };
                kag.fore.base.beginTransition("crossfade", true, kag.back.base, %[time: 2]);
                "#,
            )
            .expect("close path");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "kag.fore.messages[1].visible")
                .expect("published page message layer"),
            Variant::Integer(0),
            "the published page keeps the hidden message state"
        );
        let Variant::Integer(node_id) = engine
            .execute_expression("inline.tjs", "kag.fore.messages[1].__nativeLayerId")
            .expect("native layer id")
        else {
            panic!("message layer has no native id");
        };
        let node = engine
            .host()
            .layer_tree()
            .layer(node_id as u64)
            .expect("message layer node");
        assert!(
            !node.visible,
            "the published page's message layer node must be hidden"
        );

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(
            !frame.output.draw_commands.iter().any(|command| {
                matches!(
                    command,
                    krkr_core::DrawCommand::Image(image)
                        if image.rect.width == 64.0 && image.rect.height == 48.0
                )
            }),
            "the outgoing save image must not be republished on the visible page"
        );
    }

    #[test]
    fn native_layer_exchange_info_swaps_staged_comp_backing_before_page_exchange() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(
            root.join("new.png"),
            2,
            1,
            &[0, 255, 0, 255, 0, 255, 0, 255],
        );

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                global.win = new Window();
                kag.fore = %[base: new Layer(win), layers: [], messages: []];
                kag.back = %[base: new Layer(win, kag.fore.base), layers: [], messages: []];
                kag.fore.base.comp = kag.back.base;
                kag.back.base.comp = kag.fore.base;
                kag.fore.base.visible = true;
                kag.fore.base.setSize(2, 1);
                kag.fore.base.setImageSize(2, 1);
                kag.back.base.loadImages("new.png");
                // The crossfade family requires `time` (`TransIntf.cpp:528`)
                // and completes on a later tick, so the page swap runs in the
                // completion callback.
                kag.fore.base.onTransitionCompleted = function(dest, src) {
                    var tmp = kag.fore;
                    kag.fore = kag.back;
                    kag.back = tmp;
                    global.foreBasePrimary = kag.fore.base.isPrimary;
                    global.backBasePrimary = kag.back.base.isPrimary;
                };
                kag.fore.base.beginTransition("crossfade", true, kag.back.base, %[time: 2]);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(2),
            )
            .expect("update");

        assert!(frame.output.image_uploads.iter().any(|upload| {
            upload.width == 2
                && upload.height == 1
                && upload.rgba.chunks_exact(4).all(|pixel| pixel[1] == 255)
        }));
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "foreBasePrimary")
                .expect("fore primary"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "backBasePrimary")
                .expect("back primary"),
            Variant::Integer(0)
        );

        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.clicks = 0;
                kag.fore.base.onMouseDown = function(x, y, button, shift) {
                    global.clicks += isPrimary ? 1 : 10;
                };
                "#,
            )
            .expect("install click handler");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(0.0, 0.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click current fore base");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "clicks")
                .expect("clicks"),
            Variant::Integer(1)
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_paired_transition_completion_exchanges_primary_state_before_script_callback() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                global.win = new Window();
                kag.fore = %[base: new Layer(win), layers: [], messages: []];
                kag.back = %[base: new Layer(win, kag.fore.base), layers: [], messages: []];
                kag.fore.base.comp = kag.back.base;
                kag.back.base.comp = kag.fore.base;
                kag.fore.base.visible = true;
                kag.fore.base.setSize(10, 10);
                kag.back.base.visible = true;
                kag.back.base.setSize(10, 10);
                kag.fore.base.onTransitionCompleted = function(dest, src) {
                    var tmp = kag.fore;
                    kag.fore = kag.back;
                    kag.back = tmp;
                    global.forePrimaryAfterTransition = kag.fore.base.isPrimary;
                    global.backPrimaryAfterTransition = kag.back.base.isPrimary;
                };
                "#,
            )
            .expect("setup");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync");
        engine
            .execute_script(
                "inline.tjs",
                r#"kag.fore.base.beginTransition("crossfade", true, kag.back.base, %[time: 1]);"#,
            )
            .expect("begin transition");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                // The crossfade family floors `time` at 2 ms
                // (`TransIntf.cpp:530`).
                Duration::from_millis(2),
            )
            .expect("complete transition");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "forePrimaryAfterTransition")
                .expect("fore primary"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "backPrimaryAfterTransition")
                .expect("back primary"),
            Variant::Integer(0)
        );
    }

    #[test]
    fn native_layer_completion_paints_before_assigning_images() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.paintCount = 0;
                class PaintedLayer extends Layer {
                    function PaintedLayer() {
                        super.Layer();
                        setImageSize(2, 1);
                    }
                    function onPaint() {
                        global.paintCount++;
                        fillRect(0, 0, 2, 1, 0xff00ff00);
                    }
                }

                var source = new PaintedLayer();
                var dest = new Layer();
                dest.visible = true;
                source.update();
                var wasPending = source.callOnPaint;
                dest.assignImages(source);
                return wasPending + ":" + source.callOnPaint + ":" +
                    paintCount + ":" + dest.imageWidth + "x" + dest.imageHeight;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("1:0:1:2x1".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.image_uploads.iter().any(|upload| {
            upload.width == 2
                && upload.height == 1
                && upload.rgba.as_ref() == [0, 255, 0, 255, 0, 255, 0, 255]
        }));
    }

    #[test]
    fn native_layer_assign_images_copies_source_image_to_destination() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 1, 1, &[255, 0, 0, 255]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.loadImages("sprite.png");
                source.visible = true;
                source.setPos(0, 0, 1, 1);
                var dest = new Layer();
                dest.visible = true;
                dest.setPos(5, 0, 1, 1);
                dest.assignImages(source);
                // `AssignImages` shares the source's bitmap
                // (`MainImage->Assign`); the write forks it
                // (`tTVPNativeBaseBitmap::Independ`) and leaves the
                // destination with the assigned pixels.
                source.fillRect(0, 0, 1, 1, 0xff00ff00);
                return dest.imageWidth + ":" + dest.imageHeight;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("1:1".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        let mut pixels_by_x = BTreeMap::new();
        for command in &frame.output.draw_commands {
            let krkr_core::DrawCommand::Image(image) = command else {
                continue;
            };
            let Some(upload) = frame
                .output
                .image_uploads
                .iter()
                .find(|upload| upload.texture_id == image.texture_id)
            else {
                continue;
            };
            pixels_by_x.insert(image.rect.x as i64, upload.rgba.as_ref().to_vec());
        }
        assert_eq!(
            pixels_by_x.get(&5).map(Vec::as_slice),
            Some([255, 0, 0, 255].as_slice())
        );
        assert_eq!(
            pixels_by_x.get(&0).map(Vec::as_slice),
            Some([0, 255, 0, 255].as_slice())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_assign_images_moves_the_province_plane() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setProvincePixel(0, 0, 7);
                var dest = new Layer();
                dest.assignImages(source);
                var assigned = dest.getProvincePixel(0, 0);
                // A source without a plane drops the destination's
                // (`DeallocateProvinceImage`, `LayerIntf.cpp:2150`).
                var bare = new Layer();
                dest.assignImages(bare);
                return assigned + ":" + dest.getProvincePixel(0, 0);
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("7:0".to_string()));
    }

    #[test]
    fn native_layer_has_image_controls_and_guards_image_assignment() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 1, 1, &[255, 0, 0, 255]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.loadImages("sprite.png");
                var dest = new Layer();
                dest.visible = true;
                var loaded = source.hasImage;
                if(source.hasImage) dest.assignImages(source);
                source.hasImage = false;
                return loaded + ":" + source.hasImage + ":" + dest.hasImage +
                    ":" + dest.imageWidth + "x" + dest.imageHeight;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("1:0:1:1x1".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert_eq!(frame.output.image_uploads.len(), 1);
        assert_eq!(
            frame.output.image_uploads[0].rgba.as_ref(),
            [255, 0, 0, 255]
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_neutral_color_matches_krkr2_defaults_and_blend_types() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.normal = new Layer();
                normal.setImageSize(1, 1);
                var alphaNeutral = normal.neutralColor;
                normal.fillRect(0, 0, 1, 1, normal.neutralColor);
                var win = new Window();
                var primary = new Layer(win);
                var primaryNeutral = primary.neutralColor;
                normal.type = 3;
                return alphaNeutral + ":" + primaryNeutral + ":" + normal.neutralColor;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("16777215:4294967295:0".to_string()));
        let layer_id = engine
            .execute_expression("inline.tjs", "normal.__nativeLayerId")
            .expect("layer id")
            .to_integer()
            .expect("integer layer id") as u64;
        let image = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("layer image");
        assert_eq!(image.upload.rgba.as_ref(), [255, 255, 255, 0]);
    }

    #[test]
    fn native_layer_operate_rect_marks_image_modified_for_script_clear() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parent = new Layer();
                parent.visible = true;
                parent.setSize(32, 32);
                parent.setImageSize(32, 32);
                parent.imageModified = false;

                var line = new Layer();
                line.visible = false;
                line.setImageSize(4, 4);
                line.colorRect(0, 0, 4, 4, 0xffffff, 255);

                parent.operateRect(0, 0, line, 0, 0, 4, 4);
                var wasModified = parent.imageModified;
                // `colorRect` on a dfAlpha face fills opaquely at opacity 255
                // (`PartialBlendColor` ORs in 0xff000000), so clearing back to
                // a transparent plane is `fillRect`'s job.
                if(parent.imageModified) parent.fillRect(0, 0, 32, 32, 0);
                parent.imageModified = false;
                return wasModified;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::Integer(1));

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        let parent_upload = frame
            .output
            .image_uploads
            .iter()
            .find(|upload| upload.width == 32 && upload.height == 32)
            .expect("parent upload");
        assert!(
            parent_upload
                .rgba
                .chunks_exact(4)
                .all(|pixel| pixel[3] == 0)
        );
    }

    /// `tTJSNI_BaseLayer::ColorRect` dispatches on the resolved draw face.
    /// A `new Layer()` is ltAlpha, so dfAuto resolves to dfAlpha and the fill
    /// paints the alpha plane too; setting `face` to dfOpaque switches to
    /// `FillColor`, which "always holds destination alpha". The KAG message
    /// window paints its background through the second path, so a face-blind
    /// fill would either erase it or leave it fully opaque.
    #[test]
    fn native_layer_color_rect_follows_the_resolved_draw_face() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.alpha = new Layer();
                alpha.setImageSize(1, 1);
                alpha.colorRect(0, 0, 1, 1, 0x204060);

                global.opaque = new Layer();
                opaque.setImageSize(1, 1);
                opaque.face = 1; // dfOpaque
                opaque.colorRect(0, 0, 1, 1, 0x204060);

                global.mask = new Layer();
                mask.setImageSize(1, 1);
                mask.face = 2; // dfMask
                mask.colorRect(0, 0, 1, 1, 0x807f);
                "#,
            )
            .expect("script");

        let pixel = |engine: &mut KrkrEngine, name: &str| -> Vec<u8> {
            let layer_id = engine
                .execute_expression("inline.tjs", &format!("{name}.__nativeLayerId"))
                .expect("layer id")
                .to_integer()
                .expect("integer layer id") as u64;
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .and_then(|layer| layer.image.as_ref())
                .expect("layer image")
                .upload
                .rgba
                .to_vec()
        };

        // dfAlpha at opacity 255 is `PartialBlendColor`'s opaque path, which
        // ORs 0xff000000 into the fill colour.
        assert_eq!(pixel(&mut engine, "alpha"), vec![0x20, 0x40, 0x60, 255]);
        // dfOpaque is `FillColor`, which holds the destination alpha.
        assert_eq!(pixel(&mut engine, "opaque"), vec![0x20, 0x40, 0x60, 0]);
        // dfMask is `FillMask(color & 0xff)`; the colour planes keep the
        // ctor holder's transparent white (`AllocateDefaultImage`,
        // `LayerIntf.cpp:404`) because `TVPFillMask` is `const_alpha_copy`
        // (`gl/blend_functor_c.h:674`): `(d & 0x00ffffff) + (a << 24)`.
        assert_eq!(pixel(&mut engine, "mask"), vec![255, 255, 255, 0x7f]);
    }

    /// `tTJSNI_BaseLayer::FillRect` writes the 32-bit colour as-is on dfAlpha
    /// (`MainImage->Fill`). A 24-bit RGB value therefore keeps alpha 0; only
    /// an explicit 0xAARRGGBB high byte, or dfOpaque+HoldAlpha (`FillColor`),
    /// produces an opaque plate. Forcing that alpha to 255 is what turned
    /// GINKA stand canvases into solid maroon rectangles.
    #[test]
    fn native_layer_fill_rect_follows_the_resolved_draw_face() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.alpha = new Layer();
                alpha.setImageSize(1, 1);
                alpha.fillRect(0, 0, 1, 1, 0x5b2e2e);

                global.opaque_hold = new Layer();
                opaque_hold.setImageSize(1, 1);
                opaque_hold.face = 1; // dfOpaque
                opaque_hold.holdAlpha = 1;
                opaque_hold.fillRect(0, 0, 1, 1, 0x5b2e2e);

                global.mask = new Layer();
                mask.setImageSize(1, 1);
                mask.face = 2; // dfMask
                mask.fillRect(0, 0, 1, 1, 0x807f);

                global.explicit_alpha = new Layer();
                explicit_alpha.setImageSize(1, 1);
                explicit_alpha.fillRect(0, 0, 1, 1, 0xff5b2e2e);
                "#,
            )
            .expect("script");

        let pixel = |engine: &mut KrkrEngine, name: &str| -> Vec<u8> {
            let layer_id = engine
                .execute_expression("inline.tjs", &format!("{name}.__nativeLayerId"))
                .expect("layer id")
                .to_integer()
                .expect("integer layer id") as u64;
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .and_then(|layer| layer.image.as_ref())
                .expect("layer image")
                .upload
                .rgba
                .to_vec()
        };

        assert_eq!(pixel(&mut engine, "alpha"), vec![0x5b, 0x2e, 0x2e, 0]);
        assert_eq!(pixel(&mut engine, "opaque_hold"), vec![0x5b, 0x2e, 0x2e, 0]);
        // `FillMask` only writes the alpha byte (`TVPFillMask` =
        // `const_alpha_copy`, `gl/blend_functor_c.h:674`), so the ctor holder's
        // transparent white keeps its colour planes.
        assert_eq!(pixel(&mut engine, "mask"), vec![255, 255, 255, 0x7f]);
        assert_eq!(
            pixel(&mut engine, "explicit_alpha"),
            vec![0x5b, 0x2e, 0x2e, 255]
        );
    }

    /// Official TJS `operateRect` (`LayerIntf.cpp:7207`) resolves `omAuto`
    /// from the source layer type. `ltOpaque` → `omOpaque` → `bmCopyOnAlpha`
    /// on a dfAlpha dest (`TVPCopyOpaqueImage` ORs `0xff000000`).
    #[test]
    fn native_layer_operate_rect_om_auto_follows_source_type() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.dest = new Layer();
                dest.setImageSize(1, 1);
                dest.fillRect(0, 0, 1, 1, 0);

                global.src_opaque = new Layer();
                src_opaque.type = 1; // ltOpaque
                src_opaque.setImageSize(1, 1);
                src_opaque.fillRect(0, 0, 1, 1, 0x5b2e2e);

                dest.operateRect(0, 0, src_opaque, 0, 0, 1, 1);
                "#,
            )
            .expect("script");

        let layer_id = engine
            .execute_expression("inline.tjs", "dest.__nativeLayerId")
            .expect("layer id")
            .to_integer()
            .expect("integer layer id") as u64;
        let rgba = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("dest image")
            .upload
            .rgba
            .to_vec();
        assert_eq!(rgba, vec![0x5b, 0x2e, 0x2e, 255]);
    }

    /// `tTJSNI_BaseLayer::OperateRect` forwards `HoldAlpha` as `Blt`'s `hda`
    /// ("hold destination alpha", `LayerIntf.cpp:4380`,
    /// `LayerBitmapIntf.cpp:1088`); the PS blend modes have dedicated `_HDA`
    /// variants (`LayerBitmapIntf.cpp:1454`). GINKA's PSD compositing keeps
    /// `holdAlpha = 1` while tiling an opaque colour over the stand canvas
    /// with `omPsSoftLight` (`psdlayer.tjs:1963`); without the hold the tile's
    /// alpha turned the whole 863×960 rect into a solid maroon plate.
    #[test]
    fn native_layer_operate_rect_hold_alpha_keeps_destination_alpha() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.held = new Layer();
                held.setImageSize(2, 1);
                held.fillRect(0, 0, 2, 1, 0);
                held.holdAlpha = 1;

                global.freed = new Layer();
                freed.setImageSize(2, 1);
                freed.fillRect(0, 0, 2, 1, 0);

                global.src = new Layer();
                src.setImageSize(1, 1);
                src.fillRect(0, 0, 1, 1, 0xff684140);

                held.operateRect(0, 0, src, 0, 0, 1, 1, 20);
                freed.operateRect(0, 0, src, 0, 0, 1, 1, 20);
                "#,
            )
            .expect("script");

        let alpha = |engine: &mut KrkrEngine, name: &str| -> u8 {
            let layer_id = engine
                .execute_expression("inline.tjs", &format!("{name}.__nativeLayerId"))
                .expect("layer id")
                .to_integer()
                .expect("integer layer id") as u64;
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .and_then(|layer| layer.image.as_ref())
                .expect("layer image")
                .upload
                .rgba[3]
        };

        // `omPsSoftLight` with HoldAlpha keeps the cleared canvas transparent.
        assert_eq!(alpha(&mut engine, "held"), 0);
        // The PS functors return colour only, so the official `normal_op`
        // wrapper (no HoldAlpha) writes alpha 0 (`blend_variation.h:25`).
        assert_eq!(alpha(&mut engine, "freed"), 0);
    }

    /// `tTVPBBStretchType` (`LayerBitmapIntf.h:61`): `stLinear` interpolates,
    /// while `stNearest` replicates the nearest source column.
    #[test]
    fn native_layer_stretch_copy_honours_the_stretch_type() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let pixels = |engine: &mut KrkrEngine, name: &str| -> Vec<u8> {
            let layer_id = engine
                .execute_expression("inline.tjs", &format!("{name}.__nativeLayerId"))
                .expect("layer id")
                .to_integer()
                .expect("integer layer id") as u64;
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .and_then(|layer| layer.image.as_ref())
                .expect("layer image")
                .upload
                .rgba
                .to_vec()
        };
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var src = new Layer();
                src.setImageSize(2, 1);
                src.fillRect(0, 0, 1, 1, 0xffff0000);
                src.fillRect(1, 0, 1, 1, 0xff0000ff);

                global.nearest = new Layer();
                nearest.setImageSize(4, 1);
                nearest.stretchCopy(0, 0, 4, 1, src, 0, 0, 2, 1, stNearest);

                global.linear = new Layer();
                linear.setImageSize(4, 1);
                linear.stretchCopy(0, 0, 4, 1, src, 0, 0, 2, 1, stLinear);
                "#,
            )
            .expect("script");

        let nearest = pixels(&mut engine, "nearest");
        assert_eq!(&nearest[..4], &[255, 0, 0, 255]);
        assert_eq!(&nearest[4..8], &[255, 0, 0, 255]);
        assert_eq!(&nearest[8..12], &[0, 0, 255, 255]);
        assert_eq!(&nearest[12..16], &[0, 0, 255, 255]);

        // Bilinear's tap window is the reference's
        // `left = floor(cx - range)` … `right - 1 = floor(cx + range) - 1`
        // (`gl/ResampleImage.cpp:303-317`), so for a 2x magnification the first
        // two destination pixels stay on the first source pixel and the third
        // mixes both; the channel values are truncated, not rounded (`:428`).
        let linear = pixels(&mut engine, "linear");
        assert_eq!(&linear[..4], &[255, 0, 0, 255]);
        assert_eq!(&linear[4..8], &[255, 0, 0, 255]);
        assert_eq!(&linear[8..12], &[63, 0, 191, 255]);
        assert_eq!(&linear[12..16], &[0, 0, 255, 255]);
    }

    /// `Window.setZoom` recomputes the paint box from the logical inner size
    /// (`WindowFormUnit.cpp:681`).
    #[test]
    fn window_set_zoom_recomputes_the_paint_box() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var win = new Window();
                win.setZoom(200, 100);
                win.setInnerSize(100, 50);
                return win.zoomNumer + ":" + win.zoomDenom + ":" + win.innerWidth + ":" +
                    win.innerHeight + ":" + win.width + ":" + win.height;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("2:1:100:50:200:100".to_string()));
    }

    /// `tTJSNI_BaseLayer::GetMainPixel`/`SetMainPixel` (`LayerIntf.cpp:2587`)
    /// expose the 24-bit colour only, and the mask accessors expose the alpha
    /// channel; both setters honour `ClipRect`.
    #[test]
    fn native_layer_pixel_accessors_match_official_contracts() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.setImageSize(2, 1);
                layer.setMainPixel(0, 0, 0x336699);
                layer.setMaskPixel(0, 0, 0x80);
                layer.setMainPixel(1, 0, 0x112233);
                return layer.getMainPixel(0, 0) + ":" + layer.getMaskPixel(0, 0) + ":" +
                    layer.getMainPixel(1, 0);
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("3368601:128:1122867".to_string()));
    }

    /// `tTJSNI_BaseLayer::SetClip` / `ResetClip` (`LayerIntf.cpp:7207`): every
    /// fill and blit is clipped to the layer-local `ClipRect`, and `setClip()`
    /// without four arguments restores the layer rectangle.
    #[test]
    fn native_layer_set_clip_limits_fills_and_blits() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 1);
                layer.setClip(1, 0, 2, 1);
                layer.fillRect(0, 0, 4, 1, 0xffff0000);
                layer.setClip();
                layer.fillRect(3, 0, 1, 1, 0xff00ff00);
                "#,
            )
            .expect("script");

        let layer_id = engine
            .execute_expression("inline.tjs", "layer.__nativeLayerId")
            .expect("layer id")
            .to_integer()
            .expect("integer layer id") as u64;
        let rgba = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("layer image")
            .upload
            .rgba
            .to_vec();
        assert_eq!(
            rgba,
            vec![
                // Outside the clip: the ctor holder's transparent white.
                255, 255, 255, 0,
                255, 0, 0, 255, // inside the clip
                255, 0, 0, 255, // inside the clip
                0, 255, 0, 255, // drawn after ResetClip
            ]
        );
    }

    /// Official `tTJSNI_BaseLayer` stays allocated for the TJS object's life
    /// (`SetHasImage` → `AllocateImage` in `LayerIntf.cpp:2228`). If Kirakira
    /// drops the tree node, `hasImage = 1` must recreate it so GINKA
    /// `PSDLayer.updateDisp` can composite stand parts.
    #[test]
    fn native_layer_has_image_restores_a_dropped_tree_node() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.setSize(4, 3);
                return layer.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;

        engine.host_mut().layer_tree_mut().remove_layer(layer_id);
        assert!(engine.host().layer_tree().layer(layer_id).is_none());

        engine
            .execute_script("inline.tjs", "layer.hasImage = 1;")
            .expect("hasImage");

        let layer = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .expect("restored layer node");
        let image = layer.image.as_ref().expect("allocated image");
        assert_eq!(image.upload.width, 4);
        assert_eq!(image.upload.height, 3);
    }

    /// Official writes neither layer during a transition
    /// (`tTransDrawable::DrawCompleted`, `LayerIntf.cpp:6567-6681`): the
    /// destination keeps its own bitmap and rect for the whole transition, the
    /// incoming content rides in the transition's own source face
    /// (`tTVPDivisibleData::Src2`, `:6611`), and the stop's `Exchange`
    /// (`tTJSNI_BaseLayer::Exchange`, `:716`) only re-parents the two layers,
    /// so both sides keep their own content.
    #[test]
    fn native_layer_begin_transition_keeps_the_destination_image_its_own() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 1, 1, &[0, 255, 0, 255]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.loadImages("sprite.png");
                // `Layer.window` is read-only in the reference
                // (`TJS_DENY_NATIVE_PROP_SETTER`, `LayerIntf.cpp:8689`), so the
                // layer takes its window (here: the KAG `transCount` relay) in
                // the constructor, exactly as KAG does.
                global.transWindow = %[transCount: 1];
                global.dest = new Layer(transWindow, null);
                dest.visible = true;
                dest.setSize(1, 1);
                dest.inTransition = true;
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                return dest.window.transCount + ":" + dest.inTransition + ":"
                    + dest.imageWidth + ":" + dest.hasImage;
                "#,
            )
            .expect("script");

        // `dest` never carries the source bitmap: it keeps its own allocated
        // image (1x1 rect, 32x32 default bitmap), and `source` keeps the sprite.
        assert_eq!(result, Variant::String("1:1:32:1".to_string()));
        engine
            .execute_script("inline.tjs", "dest.update();")
            .expect("update pass");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("transition");
        let mut texture_id = |expression: &str| {
            let layer_id = engine
                .execute_expression("inline.tjs", expression)
                .expect("layer id")
                .to_integer()
                .expect("layer id") as u64;
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .and_then(|layer| layer.image.as_ref())
                .map(|image| image.upload.texture_id)
                .expect("layer image")
        };
        let source_texture = texture_id("source.__nativeLayerId");
        let dest_texture = texture_id("dest.__nativeLayerId");
        assert_ne!(source_texture, dest_texture);
        // The sprite is the transition's incoming face, positioned where the
        // destination draws.
        assert!(transition.source_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 0.0
                        && image.rect.y == 0.0
                        && image.texture_id == source_texture
            )
        }));
        // The destination's own bitmap never enters the transition.
        assert!(!transition.source_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image) if image.texture_id == dest_texture
            )
        }));
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.imageWidth + ':' + source.imageWidth"
                )
                .expect("images"),
            Variant::String("32:1".to_string())
        );

        // Completion exchanges the two layers' positions and visibility
        // (`InternalStopTransition`, `LayerIntf.cpp:6364`) but leaves each
        // side's own content alone.
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 1.0), Vec::new()),
                Duration::from_millis(1000),
            )
            .expect("complete");
        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.window.transCount + ':' + dest.inTransition + ':' + dest.imageWidth + ':' + source.imageWidth"
                )
                .expect("completion"),
            Variant::String("0:0:32:1".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_begin_transition_enforces_official_checks() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var dest = new Layer();
                var source = new Layer();
                var missing = "";
                try { dest.beginTransition("crossfade", true); }
                catch (e) { missing = e.message; }
                var notALayer = "";
                try { dest.beginTransition("crossfade", true, %[]); }
                catch (e) { notALayer = e.message; }
                var mismatch = "";
                source.setSize(2, 1);
                try { dest.beginTransition("crossfade", true, source, %[time: 10]); }
                catch (e) { mismatch = e.message; }
                var imageless = "";
                var bare = new Layer();
                dest.freeImage();
                bare.freeImage();
                try { dest.beginTransition("crossfade", false, bare, %[time: 10]); }
                catch (e) { imageless = e.message; }
                // The crossfade family requires `time` (`TransIntf.cpp:528`)
                // and `universal` requires a rule graphic (`:777`).
                var pair = new Layer();
                var other = new Layer();
                var noTime = "";
                try { pair.beginTransition("crossfade", true, other, %[]); }
                catch (e) { noTime = e.message; }
                var noRule = "";
                try { pair.beginTransition("universal", true, other, %[time: 10]); }
                catch (e) { noRule = e.message; }
                // The provider lookup is exact and case-sensitive
                // (`TransIntf.cpp:341-359`): a name no provider registered is
                // the official message, and `Wave` is not `wave`.
                var unknownName = "";
                try { pair.beginTransition("nope", true, other, %[time: 10]); }
                catch (e) { unknownName = e.message; }
                var wrongCase = "";
                try { pair.beginTransition("Wave", true, other, %[time: 10]); }
                catch (e) { wrongCase = e.message; }
                // A plugin-provided name has no provider while its plugin is
                // not linked, which is the reference's miss as well.
                var unlinkedPlugin = "";
                try { pair.beginTransition("blurfade", true, other, %[time: 10]); }
                catch (e) { unlinkedPlugin = e.message; }
                var resolved = 0;
                try { pair.beginTransition("wave", true, other, %[time: 10]); }
                catch (e) { resolved = 1; }
                return missing + "|" + notALayer + "|" + mismatch + "|" + imageless +
                    "|" + noTime + "|" + noRule + "|" + unknownName + "|" + wrongCase +
                    "|" + unlinkedPlugin + "|" + resolved;
                "#,
            )
            .expect("script");

        assert_eq!(
            result,
            Variant::String(
                "Specify Layer class object|Specify Layer class object|Transition layer size mismatch 2x1 and 32x32|\
                 Transition source and destination must have image|Specify option time|\
                 Specify option rule|Cannot find transition handler nope|\
                 Cannot find transition handler Wave|Cannot find transition handler blurfade|0"
                    .to_string()
            )
        );
    }

    /// The provider lookup of `Layer.beginTransition` consults the plugin host
    /// (mission M54): a name a *linked* plugin shim would provide is not unknown
    /// -- the shim has no kernel, so it degrades to `crossfade`
    /// (`PLUGIN_TRANSITION_NAMES`) -- and an unlinked plugin leaves the name
    /// unknown exactly as the reference does.
    #[test]
    fn native_layer_begin_transition_consults_linked_plugin_providers() {
        struct ExtNaganoShim;

        impl KrkrPlugin for ExtNaganoShim {
            fn name(&self) -> &str {
                "extNagano.dll"
            }

            fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
                Ok(())
            }
        }

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let unlinked = engine
            .execute_script(
                "inline.tjs",
                r#"
                var dest = new Layer();
                var source = new Layer();
                var message = "";
                try { dest.beginTransition("blurfade", true, source, %[time: 10]); }
                catch (e) { message = e.message; }
                return message;
                "#,
            )
            .expect("script");
        assert_eq!(
            unlinked,
            Variant::String("Cannot find transition handler blurfade".to_string())
        );

        engine
            .register_plugin(ExtNaganoShim)
            .expect("register plugin");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.dest = new Layer();
                global.source = new Layer();
                dest.visible = true;
                dest.beginTransition("blurfade", true, source, %[time: 10]);
                "#,
            )
            .expect("script");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert_eq!(
            frame.output.transitions.first().expect("transition").method,
            "crossfade"
        );
    }

    /// Mission M54: the script `time` option is the handler's own millisecond
    /// clock (`TransitionParams::duration_millis`) and **every** provider clamps
    /// it to the reference's 2 ms floor (`TRANSITION_MIN_MILLIS`), so the
    /// kernels never receive a duration that makes their ramps divide by a zero
    /// clock.  `bgcolor*` decode as `tjs_uint32` ARGB (top byte = alpha).
    #[test]
    fn native_layer_begin_transition_clamps_the_clock_and_decodes_argb_colors() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.dest = new Layer();
                global.source = new Layer();
                dest.visible = true;
                dest.beginTransition("wave", true, source,
                    %[time: 1, bgcolor1: 0x7f00ff00, bgcolor2: 0xff000000]);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("transition");
        assert_eq!(transition.method, "wave");
        // The requested 1 ms is raised to the reference's 2 ms clock, and that
        // is the value the kernels read instead of the untimed fallback.
        assert_eq!(transition.params.duration_millis, 2.0);
        assert_eq!(
            transition.params.bg_color1,
            Color::new(0.0, 1.0, 0.0, 0x7f as f32 / 255.0)
        );
        assert_eq!(transition.params.bg_color2, Color::new(0.0, 0.0, 0.0, 1.0));
        assert!(transition.progress.is_finite());

        // The clamp is the clock the transition runs on: one millisecond in,
        // half of the 2 ms clock has elapsed.
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 1.0), Vec::new()),
                Duration::from_millis(1),
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("transition");
        assert_eq!(transition.progress, 0.5);
        assert!(transition.progress.is_finite());
        assert!(transition.params.duration_millis.is_finite());

        // The second millisecond ends it; a 1 ms request never ran a 1 ms
        // clock that could feed the kernels a zero-time ramp.
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 2.0), Vec::new()),
                Duration::from_millis(1),
            )
            .expect("update");
        assert!(frame.output.transitions.is_empty());

        // An explicit `0` sits on the same floor: it is a 2 ms transition and
        // no longer an instantaneous exchange (KAG's own layer maps a specified
        // `0` to `1` before the provider raises it to 2, `KAGLayer.tjs:198-201`).
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.dest2 = new Layer();
                global.source2 = new Layer();
                dest2.visible = true;
                dest2.beginTransition("wave", true, source2, %[time: 0]);
                "#,
            )
            .expect("script");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 3.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("transition");
        assert_eq!(transition.params.duration_millis, 2.0);
    }

    #[test]
    fn native_layer_transition_stops_when_the_destination_hides() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.transWindow = %[completed: 0];
                global.dest = new Layer(transWindow, null);
                global.source = new Layer();
                dest.visible = true;
                dest.inTransition = true;
                dest.onTransitionCompleted = function(d, s) {
                    this.window.completed++;
                    this.inTransition = false;
                };
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                return dest.inTransition;
                "#,
            )
            .expect("script");
        assert_eq!(result, Variant::Integer(1));

        engine
            .execute_script("inline.tjs", "dest.visible = false;")
            .expect("hide");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.window.completed + ':' + dest.inTransition"
                )
                .expect("completion"),
            Variant::String("1:0".to_string())
        );
    }

    /// The visibility stop is driven by the destination layer, not by the
    /// script-side completion payload, so the engine's own KAG page transition
    /// obeys it too (`tTJSNI_BaseLayer::InvokeTransition`, `LayerIntf.cpp:6463`).
    #[test]
    fn kag_transition_stops_when_the_destination_layer_hides() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.host_mut().mutate_kag_layer("back", "base", |layer| {
            layer.visible = true;
            layer.width = 320.0;
            layer.height = 240.0;
        });
        engine.host_mut().begin_kag_transition(
            Duration::from_millis(1000),
            TransitionParams::default(),
            None,
            None,
        );
        assert!(engine.host().has_active_transition());

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.1), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("update");
        assert_eq!(frame.output.transitions.len(), 1);

        let dest_layer = engine
            .host_mut()
            .ensure_kag_layer("fore", "base");
        engine
            .host_mut()
            .layer_tree_mut()
            .layer_mut(dest_layer)
            .expect("kag base layer")
            .visible = false;
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.2), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("update");
        assert!(frame.output.transitions.is_empty());
        assert!(!engine.host().has_active_transition());
    }

    /// Official `tTJSNI_BaseLayer::StopTransitionByHandler` (`LayerIntf.cpp:6440`)
    /// with `TVPEventDisabled` set (`System.eventDisabled`): the handler cannot
    /// stop the transition yet, so the completion is deferred to the next update
    /// that runs with event dispatching enabled (`InvokeTransition`, `:6455`).
    #[test]
    fn handler_stop_is_deferred_while_events_are_disabled() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 2, 1, &[255; 8]);
        write_png(root.join("new.png"), 2, 1, &[0; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.loadImages("new.png");
                source.visible = true;
                global.transWindow = %[completed: 0];
                global.dest = new Layer(transWindow, null);
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.onTransitionCompleted = function(d, s) {
                    this.inTransition = false;
                    this.window.completed++;
                };
                dest.beginTransition("crossfade", true, source, %[time: 100]);
                System.eventDisabled = true;
                "#,
            )
            .expect("script");

        // The transition reaches its time while dispatching is disabled: the
        // stop is prevented and nothing is fired.
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.2), Vec::new()),
                Duration::from_millis(200),
            )
            .expect("update");
        assert_eq!(frame.output.transitions.len(), 1);
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "dest.window.completed")
                .expect("completion"),
            Variant::Integer(0)
        );

        // A second disabled pass keeps it frozen instead of advancing.
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.3), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("still running");
        assert_eq!(transition.progress, 1.0);

        engine
            .execute_script("inline.tjs", "System.eventDisabled = false;")
            .expect("enable events");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.4), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.window.completed + ':' + dest.inTransition"
                )
                .expect("completion"),
            Variant::String("1:0".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `stopTransition()` runs `InternalStopTransition` directly
    /// (`LayerIntf.cpp:6437`), so a disabled event gate does not defer the stop;
    /// the `onTransitionCompleted` it posts with `TVP_EPT_IMMEDIATE`
    /// (`:6420`) is dropped while dispatching is disabled
    /// (`TVPPostEvent`, `EventIntf.cpp:230-258`) and delivered again once the
    /// script re-enables it.
    #[test]
    fn manual_stop_transition_stops_but_drops_the_event_while_disabled() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 2, 1, &[255; 8]);
        write_png(root.join("new.png"), 2, 1, &[0; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.loadImages("new.png");
                source.visible = true;
                global.transWindow = %[completed: 0];
                global.dest = new Layer(transWindow, null);
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.onTransitionCompleted = function(d, s) {
                    this.inTransition = false;
                    this.window.completed++;
                };
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                System.eventDisabled = true;
                dest.stopTransition();
                return dest.window.completed + ":" + dest.inTransition;
                "#,
            )
            .expect("script");
        // The transition is gone at once, but no event reached the script.
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.window.completed + ':' + dest.inTransition"
                )
                .expect("completion"),
            Variant::String("0:1".to_string())
        );
        assert_eq!(engine.host().active_transition_count(), 0);

        // With dispatching enabled again the same stop does deliver it.
        let delivered = engine
            .execute_script(
                "inline.tjs",
                r#"
                System.eventDisabled = false;
                dest.inTransition = true;
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                dest.stopTransition();
                return dest.window.completed + ":" + dest.inTransition;
                "#,
            )
            .expect("script");
        assert_eq!(delivered, Variant::String("1:0".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Official `UpdateTransDestinationOnSelfUpdate` (`LayerIntf.cpp:4828`):
    /// updating the *source* layer drives the destination's completion pass, so
    /// a self-updated transition moves when the source asks for an update too.
    #[test]
    fn selfupdate_transition_advances_from_the_source_layer_update() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 2, 1, &[255; 8]);
        write_png(root.join("new.png"), 2, 1, &[0; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.loadImages("new.png");
                source.visible = true;
                global.transWindow = %[completed: 0];
                global.dest = new Layer(transWindow, null);
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.onTransitionCompleted = function(d, s) {
                    this.inTransition = false;
                    this.window.completed++;
                };
                dest.beginTransition("crossfade", true, source, %[
                    time: 1000,
                    selfupdate: true
                ]);
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 1.0), Vec::new()),
                Duration::from_millis(600),
            )
            .expect("update");
        // The frame clock alone never moves it.
        engine
            .execute_script("inline.tjs", "source.update();")
            .expect("drive from source");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 2.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        let progress = frame
            .output
            .transitions
            .first()
            .expect("running")
            .progress;
        assert!((progress - 0.6).abs() < 0.001, "progress was {progress}");

        // The destination's own update pass finishes it.
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 3.0), Vec::new()),
                Duration::from_millis(400),
            )
            .expect("update");
        engine
            .execute_script("inline.tjs", "dest.update();")
            .expect("drive from destination");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 4.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.window.completed + ':' + dest.inTransition"
                )
                .expect("completion"),
            Variant::String("1:0".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `GetTransTick` runs once per `InvokeTransition` (`LayerIntf.cpp:6459`);
    /// `BeforeCompletion` only asks the callback again under `TransSelfUpdate`
    /// (`:5056`), so a script `update()` on a callback-driven transition must
    /// not sample it twice in one frame.
    #[test]
    fn transition_callback_runs_once_per_frame() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 2, 1, &[255; 8]);
        write_png(root.join("new.png"), 2, 1, &[0; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.ticks = 0;
                global.source = new Layer();
                source.loadImages("new.png");
                source.visible = true;
                global.dest = new Layer();
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.beginTransition("crossfade", true, source, %[
                    time: 1000,
                    callback: function() { ticks++; return ticks * 250; }
                ]);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 1.0), Vec::new()),
                Duration::from_millis(1000),
            )
            .expect("update");
        assert_eq!(
            frame.output.transitions.first().expect("running").progress,
            0.25
        );
        engine
            .execute_script("inline.tjs", "dest.update();")
            .expect("script update");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 2.0), Vec::new()),
                Duration::from_millis(1000),
            )
            .expect("update");
        // One sample per frame: a `Layer.update()` pass only drives a
        // self-updating transition, so the two `dest.update()` calls do not
        // sample the callback again.
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "ticks")
                .expect("ticks"),
            Variant::Integer(2)
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `[backlay]` with no `layer=` stages every page layer
    /// (`MainWindow.tjs:3166-3183`) and the script path never clears the
    /// staging buffer, so a later single-layer `[trans layer=N]` must build its
    /// incoming face from that one layer: the stale page's opaque background
    /// would otherwise be drawn over the incoming sprite.
    #[test]
    fn transition_source_face_ignores_a_stale_staged_page() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("bg.png"), 200, 200, &[255; 160_000]);
        write_png(root.join("old.png"), 4, 3, &[255; 48]);
        write_png(root.join("new.png"), 4, 3, &[0; 48]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.fore.base.visible = true;
                kag.fore.base.setSize(200, 200);
                kag.back.base.visible = true;
                kag.back.base.setSize(200, 200);
                kag.fore.layers[0] = new Layer(null, kag.fore.base);
                kag.back.layers[0] = new Layer(null, kag.back.base);
                // A full `[backlay]`: the page base is staged with its
                // background, which stays in the buffer afterwards.
                kag.back.base.loadImages("bg.png");
                kag.back.base.setSizeToImageSize();
                kag.fore.layers[0].loadImages("old.png");
                kag.fore.layers[0].setSizeToImageSize();
                kag.fore.layers[0].setPos(5, 7, 4, 3);
                kag.fore.layers[0].visible = true;
                kag.back.layers[0].visible = true;
                "#,
            )
            .expect("setup");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync");

        engine
            .execute_script(
                "inline.tjs",
                r#"
                kag.back.layers[0].loadImages("new.png");
                kag.back.layers[0].setSizeToImageSize();
                kag.back.layers[0].left = 40;
                kag.back.layers[0].top = 50;
                kag.fore.layers[0].beginTransition(
                    "crossfade",
                    true,
                    kag.back.layers[0],
                    %[time: 1000]
                );
                "#,
            )
            .expect("begin transition");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("transition frame");
        let transition = frame.output.transitions.first().expect("transition");
        // The incoming face is the source layer alone, aligned with the
        // destination (`LayerIntf.cpp:6604`): the staged sprite lands where the
        // destination draws, and the stale staged page contributes nothing.
        let face = transition
            .source_draw_commands
            .iter()
            .filter_map(|command| match command {
                krkr_core::DrawCommand::Image(image) => Some(image.rect),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(face, vec![krkr_core::Rect::new(5.0, 7.0, 4.0, 3.0)]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `[wt]` waits while *any* transition is running: the conductor must only
    /// resume once the whole set is empty, not when the page transition it
    /// started is over (`NativeFallbackTag::Wt` -> `has_active_transition`).
    #[test]
    fn kag_wait_covers_every_running_transition() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("fore.png"), 2, 1, &[255; 8]);
        write_png(root.join("back.png"), 4, 3, &[0; 48]);
        fs::write(
            root.join("first.ks"),
            concat!(
                "[image storage=fore.png layer=base page=fore]",
                "[image storage=back.png layer=base page=back]",
                "[trans method=crossfade time=100][wt][s]"
            ),
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("start transition");
        assert_eq!(frame.tick.state, KagTaskState::WaitingTransition);
        assert_eq!(frame.output.transitions.len(), 1);

        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.other = new Layer();
                other.loadImages("back.png");
                other.visible = true;
                global.otherSource = new Layer();
                otherSource.loadImages("back.png");
                otherSource.visible = true;
                other.beginTransition("crossfade", true, otherSource, %[time: 1000]);
                "#,
            )
            .expect("second transition");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.2), Vec::new()),
                Duration::from_millis(200),
            )
            .expect("update");
        // The page transition is over; the unrelated one keeps `[wt]` waiting.
        assert_eq!(frame.tick.state, KagTaskState::WaitingTransition);
        assert_eq!(frame.output.transitions.len(), 1);

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 1.3), Vec::new()),
                Duration::from_millis(1100),
            )
            .expect("update");
        assert_eq!(frame.tick.state, KagTaskState::Finished);
        assert!(frame.output.transitions.is_empty());

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Two transitions share one KAG window counter: each completion has to
    /// balance it exactly once, including the net-zero case where the callback
    /// consumes the completion and immediately starts a chained transition,
    /// putting `transCount` back where it started (`KAGLayer.tjs:241`,
    /// `MainWindow.tjs:3231`).
    #[test]
    fn concurrent_transitions_balance_the_window_counter() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("face.png"), 2, 1, &[255; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = %[transCount: 2, completed: 0, chained: 0];
                // `Layer.window` is read-only (`LayerIntf.cpp:8689`); layers
                // take their window in the constructor, as KAG's do.
                function make() {
                    var layer = new Layer(window, null);
                    layer.loadImages("face.png");
                    layer.visible = true;
                    return layer;
                }
                global.firstDest = make();
                global.firstSrc = make();
                global.secondDest = make();
                global.secondSrc = make();
                global.chainDest = make();
                global.chainSrc = make();
                firstDest.onTransitionCompleted = function(d, s) {
                    this.inTransition = false;
                    this.window.completed++;
                    this.window.transCount--;
                    // A chained transition restores the counter: the engine's
                    // own bookkeeping must not decrement it again.
                    this.window.chained++;
                    this.window.transCount++;
                    chainDest.inTransition = true;
                    chainDest.beginTransition("crossfade", true, chainSrc, %[time: 1000]);
                };
                secondDest.onTransitionCompleted = function(d, s) {
                    this.inTransition = false;
                    this.window.completed++;
                    this.window.transCount--;
                };
                chainDest.onTransitionCompleted = function(d, s) {
                    this.inTransition = false;
                    this.window.completed++;
                    this.window.transCount--;
                };
                firstDest.inTransition = true;
                firstDest.beginTransition("crossfade", true, firstSrc, %[time: 10]);
                secondDest.inTransition = true;
                secondDest.beginTransition("crossfade", true, secondSrc, %[time: 20]);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.01), Vec::new()),
                Duration::from_millis(10),
            )
            .expect("first completion");
        assert_eq!(frame.output.transitions.len(), 2);
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "window.completed + ':' + window.transCount")
                .expect("state"),
            Variant::String("1:2".to_string())
        );

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.02), Vec::new()),
                Duration::from_millis(10),
            )
            .expect("second completion");
        // The second completion consumed its entry (2 -> 1 -> 0 for the
        // callback, +1 for the chain it started), and only the chained
        // transition is left running.
        assert_eq!(frame.output.transitions.len(), 1);
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "window.completed + ':' + window.transCount + ':' + window.chained"
                )
                .expect("state"),
            Variant::String("2:1:1".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `callback` replaces the idle hook's tick (`GetTransTick`,
    /// `LayerIntf.cpp:6683`): `StartTransition` runs a dummy `StartProcess(0)`
    /// for it (`:6318`), so the phase is `tick / time` and the frame clock is
    /// not consulted at all.
    #[test]
    fn transition_callback_option_supplies_the_tick() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 2, 1, &[255; 8]);
        write_png(root.join("new.png"), 2, 1, &[0; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.ticks = 0;
                global.source = new Layer();
                source.loadImages("new.png");
                source.visible = true;
                global.transWindow = %[completed: 0];
                global.dest = new Layer(transWindow, null);
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.onTransitionCompleted = function(d, s) {
                    this.inTransition = false;
                    this.window.completed++;
                };
                dest.beginTransition("crossfade", true, source, %[
                    time: 1000,
                    callback: function() { ticks += 250; return ticks; }
                ]);
                "#,
            )
            .expect("script");

        // The frame delta is far larger than the callback's step, so only the
        // callback can be driving the phase.
        for expected in [0.25_f32, 0.5, 0.75] {
            let frame = engine
                .update(
                    EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 5.0), Vec::new()),
                    Duration::from_millis(5000),
                )
                .expect("update");
            let transition = frame.output.transitions.first().expect("running");
            assert_eq!(transition.progress, expected);
        }

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 5.0), Vec::new()),
                Duration::from_millis(5000),
            )
            .expect("update");
        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "ticks + ':' + dest.window.completed + ':' + dest.inTransition"
                )
                .expect("completion"),
            Variant::String("1000:1:0".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `selfupdate: true` (`tTJSNI_BaseLayer::TransSelfUpdate`,
    /// `LayerIntf.cpp:6211`) keeps the transition off the idle hook: only the
    /// layer's own `update()` pass (`BeforeCompletion`, `:5056`) moves its
    /// phase, which is how KAG3's `TransLayer.tjs` drives a transition.
    #[test]
    fn selfupdate_transition_advances_only_on_layer_update() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 2, 1, &[255; 8]);
        write_png(root.join("new.png"), 2, 1, &[0; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.loadImages("new.png");
                source.visible = true;
                global.transWindow = %[completed: 0];
                global.dest = new Layer(transWindow, null);
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.onTransitionCompleted = function(d, s) {
                    this.inTransition = false;
                    this.window.completed++;
                };
                dest.beginTransition("crossfade", true, source, %[
                    time: 1000,
                    selfupdate: true
                ]);
                "#,
            )
            .expect("script");

        // The frame clock alone never moves a self-updated transition.
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 1.0), Vec::new()),
                Duration::from_millis(400),
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("running");
        assert_eq!(transition.progress, 0.0);

        // `layer.update()` applies the time that passed since the last call.
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 2.0), Vec::new()),
                Duration::from_millis(400),
            )
            .expect("update");
        engine
            .execute_script("inline.tjs", "dest.update();")
            .expect("drive");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 2.001), Vec::new()),
                Duration::from_millis(200),
            )
            .expect("update");
        let progress = frame
            .output
            .transitions
            .first()
            .expect("running")
            .progress;
        assert!((progress - 0.8).abs() < 0.001, "progress was {progress}");
        assert!(
            engine
                .execute_expression("inline.tjs", "dest.window.completed")
                .expect("completion")
                .eq(&Variant::Integer(0))
        );

        engine
            .execute_script("inline.tjs", "dest.update();")
            .expect("drive");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 2.002), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.window.completed + ':' + dest.inTransition"
                )
                .expect("completion"),
            Variant::String("1:0".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }


    /// KAG sizes `fore.base` to the screen (`MainWindow.tjs`,
    /// `setImageSize(scWidth, scHeight); setSizeToImageSize();`), so a page
    /// transition's destination rectangle is the whole frame and the composite
    /// still covers every pixel -- the single-transition rendering is unchanged.
    #[test]
    fn kag_page_transition_destination_covers_the_frame() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.host_mut().mutate_kag_layer("back", "base", |layer| {
            layer.visible = true;
            layer.width = 320.0;
            layer.height = 240.0;
        });
        let dest_layer = engine.host_mut().ensure_kag_layer("fore", "base");
        if let Some(node) = engine.host_mut().layer_tree_mut().layer_mut(dest_layer) {
            node.visible = true;
            node.width = 320.0;
            node.height = 240.0;
        }
        engine.host_mut().begin_kag_transition(
            Duration::from_millis(1000),
            TransitionParams::default(),
            None,
            None,
        );

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.1), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("transition");
        assert_eq!(
            transition.dest_rect,
            Some(krkr_core::Rect::new(0.0, 0.0, 320.0, 240.0))
        );
    }

    /// A transition never leaves its destination layer's rectangle
    /// (`tTransDrawable::DrawCompleted`, `LayerIntf.cpp:6575`), so the frame
    /// carries the destination's own bounds and unrelated layers stay visible.
    #[test]
    fn native_layer_transition_is_confined_to_its_destination_rect() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 2, 1, &[255; 8]);
        write_png(root.join("new.png"), 2, 1, &[0; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.loadImages("new.png");
                source.visible = true;
                global.dest = new Layer();
                dest.loadImages("old.png");
                dest.visible = true;
                dest.setPos(30, 40, 2, 1);
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.1), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("transition");
        assert_eq!(
            transition.dest_rect,
            Some(krkr_core::Rect::new(30.0, 40.0, 2.0, 1.0))
        );

        // `tTransDrawable::DrawCompleted` aligns the source bitmap with the
        // destination's (`LayerIntf.cpp:6604`), so the face moves to where the
        // destination draws.
        assert!(
            transition.source_draw_commands.iter().any(|command| {
                matches!(
                    command,
                    krkr_core::DrawCommand::Image(image)
                        if image.rect.x == 30.0 && image.rect.y == 40.0
                )
            }),
            "source face should follow the destination origin"
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `withchildren=false` blends the two main images only: the incoming face
    /// is the source layer's own bitmap and none of its children
    /// (`tTJSNI_BaseLayer::DrawSelf`, `LayerIntf.cpp:5400-5424`).
    #[test]
    fn native_layer_transition_without_children_carries_only_the_main_image() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("face.png"), 2, 1, &[255; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.loadImages("face.png");
                source.setSizeToImageSize();
                source.visible = true;
                var sourceChild = new Layer(null, source);
                sourceChild.loadImages("face.png");
                sourceChild.setSizeToImageSize();
                sourceChild.setPos(9, 11, 2, 1);
                sourceChild.visible = true;
                global.dest = new Layer();
                dest.loadImages("face.png");
                dest.setSizeToImageSize();
                dest.visible = true;
                dest.setPos(3, 4, 2, 1);
                dest.beginTransition("crossfade", false, source, %[time: 1000]);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.1), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("transition");
        let face = transition
            .source_draw_commands
            .iter()
            .filter_map(|command| match command {
                krkr_core::DrawCommand::Image(image) => Some(image.rect),
                _ => None,
            })
            .collect::<Vec<_>>();
        // Only the source's own 2x1 main image, at the destination's position.
        assert_eq!(face, vec![krkr_core::Rect::new(3.0, 4.0, 2.0, 1.0)]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The engine's `[trans]` projection stands in for KAG's own
    /// `kag.fore.base.beginTransition` (`KAGLayer.tjs`), so the page base object
    /// carries that transition's `InTransition`
    /// (`tTJSNI_BaseLayer::StartTransition`, `LayerIntf.cpp:6188`): the script
    /// cannot start a second transition on it, and `stopTransition` stops the
    /// projection.
    #[test]
    fn kag_projection_owns_the_page_base_layer() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("face.png"), 2, 1, &[255; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Dictionary();
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(), layers: [], messages: []];
                kag.fore.base.visible = true;
                kag.fore.base.setSize(200, 200);
                kag.back.base.visible = true;
                kag.back.base.setSize(200, 200);
                kag.back.base.loadImages("face.png");
                kag.back.base.setSizeToImageSize();
                "#,
            )
            .expect("setup");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync");

        let fore_base = engine
            .execute_expression("inline.tjs", "kag.fore.base")
            .expect("page base")
            .object_handle()
            .expect("page base is a layer object");
        engine.host_mut().begin_kag_transition(
            Duration::from_millis(1000),
            TransitionParams::default(),
            None,
            Some(fore_base),
        );
        assert!(engine.host().has_active_transition());
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.1), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("update");
        assert_eq!(frame.output.transitions.len(), 1);
        // The projection takes the page base *object*'s own rectangle, which is
        // the layer KAG sized, rather than the engine's bare page slot.
        assert_eq!(
            frame.output.transitions.first().expect("transition").dest_rect,
            Some(krkr_core::Rect::new(0.0, 0.0, 200.0, 200.0))
        );

        let thrown = engine
            .execute_script(
                "inline.tjs",
                r#"
                try {
                    kag.fore.base.beginTransition(
                        "crossfade",
                        true,
                        kag.back.base,
                        %[time: 10]
                    );
                    return "";
                } catch(e) {
                    return e.message;
                }
                "#,
            )
            .expect("guard");
        assert_eq!(
            thrown,
            Variant::String("Current transition must be stopping".to_string())
        );
        assert_eq!(engine.host().active_transition_count(), 1);

        engine
            .execute_script("inline.tjs", "kag.fore.base.stopTransition();")
            .expect("stop projection");
        assert!(!engine.host().has_active_transition());

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Official re-renders the source's own cache on every pass
    /// (`InvokeTransition` updates both layers, `LayerIntf.cpp:6455-6490`, and
    /// `DrawCompleted` calls `TransSrc->Complete(destrect)` per completion,
    /// `:6604`), so the incoming face follows the layer instead of freezing at
    /// the start of the transition.
    #[test]
    fn transition_source_face_follows_the_source_layer() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        // The crossfade family requires both faces to share one size
        // (`TransIntf.cpp:508`).
        write_png(root.join("first.png"), 4, 1, &[255; 16]);
        write_png(root.join("second.png"), 4, 1, &[0; 16]);
        write_png(root.join("dest.png"), 4, 1, &[255; 16]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.loadImages("first.png");
                source.setSizeToImageSize();
                source.visible = true;
                global.dest = new Layer();
                dest.loadImages("dest.png");
                dest.setSizeToImageSize();
                dest.visible = true;
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                "#,
            )
            .expect("script");

        let face_texture = |engine: &mut KrkrEngine| {
            let frame = engine
                .update(
                    EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.1), Vec::new()),
                    Duration::ZERO,
                )
                .expect("update");
            let transition = frame.output.transitions.first().expect("transition");
            assert_eq!(
                transition
                    .source_draw_commands
                    .iter()
                    .filter_map(|command| match command {
                        krkr_core::DrawCommand::Image(image) => Some(image.rect),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                vec![krkr_core::Rect::new(0.0, 0.0, 4.0, 1.0)]
            );
            transition
                .source_draw_commands
                .iter()
                .find_map(|command| match command {
                    krkr_core::DrawCommand::Image(image) => Some(image.texture_id),
                    _ => None,
                })
                .expect("face image")
        };
        let first = face_texture(&mut engine);

        engine
            .execute_script(
                "inline.tjs",
                "source.loadImages('second.png'); source.setSizeToImageSize();",
            )
            .expect("redraw source");
        assert_ne!(face_texture(&mut engine), first);

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The incoming face is a transparent bitmap: official blends it *into* the
    /// destination layer's bitmap (`TVPConstAlphaBlend_SD`, `TransIntf.cpp:667`),
    /// so a pixel the source does not cover has to keep the destination's
    /// content.  The renderer draws the face over the destination's own pass
    /// for exactly that reason
    /// (`krkr-render::transition_source_target_passes`); this pins the two
    /// properties the engine contributes: the face carries a texture with
    /// transparent pixels, and the destination's pass is there to sit under it.
    #[test]
    fn transition_source_face_keeps_the_destination_under_transparent_pixels() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("dest.png"), 2, 2, &[255, 0, 0, 255].repeat(4));
        // Two opaque green pixels, two fully transparent ones.
        write_png(
            root.join("sprite.png"),
            2,
            2,
            &[
                0, 255, 0, 255, 0, 255, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        );

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.loadImages("sprite.png");
                source.setSizeToImageSize();
                source.visible = true;
                global.dest = new Layer();
                dest.loadImages("dest.png");
                dest.setSizeToImageSize();
                dest.visible = true;
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.1), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("update");
        let transition = frame.output.transitions.first().expect("transition");
        let texture_id = |commands: &[krkr_core::DrawCommand]| {
            commands.iter().find_map(|command| match command {
                krkr_core::DrawCommand::Image(image) => Some(image.texture_id),
                _ => None,
            })
        };
        let source_texture = texture_id(&transition.source_draw_commands).expect("source face");
        let dest_texture = texture_id(&transition.frozen_draw_commands).expect("destination");
        // The incoming face is drawn over the scene *without* the destination
        // layer's subtree: official blends the two layer bitmaps before the
        // layer manager composites the result (`const_alpha_blend_functor`,
        // `blend_functor_c.h:584-594`), so a pixel the source does not cover
        // fades the destination's own pixel out by `1 - progress` and lets the
        // scene beneath show through.
        assert!(
            !transition
                .under_draw_commands
                .iter()
                .any(|command| matches!(command, krkr_core::DrawCommand::Image(image) if image.texture_id == dest_texture)),
            "the under-content must not contain the destination layer"
        );
        assert_ne!(source_texture, dest_texture);
        let upload = transition
            .source_image_uploads
            .iter()
            .chain(frame.output.image_uploads.iter())
            .find(|upload| upload.texture_id == source_texture)
            .expect("source upload");
        assert!(
            upload.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0),
            "the source face has to be a transparent bitmap"
        );
        // The destination's own pass is the base the face is drawn over.
        assert!(
            transition
                .frozen_draw_commands
                .iter()
                .any(|command| matches!(command, krkr_core::DrawCommand::Image(image) if image.texture_id == dest_texture))
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_timed_transition_completes_through_update() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        // The crossfade family requires both faces to have the same size
        // (`TransIntf.cpp:508`), so the source and destination layers share
        // one.
        write_png(
            root.join("old.png"),
            2,
            1,
            &[255, 0, 0, 255, 255, 0, 0, 255],
        );
        write_png(
            root.join("new.png"),
            2,
            1,
            &[0, 255, 0, 255, 0, 255, 0, 255],
        );

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.loadImages("new.png");
                global.transWindow = %[transCount: 1, completed: 0];
                global.dest = new Layer(transWindow, null);
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.onTransitionCompleted = function(destLayer, srcLayer) {
                    this.inTransition = false;
                    this.window.transCount--;
                    this.window.completed++;
                    this.window.completedWidth = this.imageWidth;
                    this.window.sourceWidth = srcLayer.imageWidth;
                };
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                return dest.inTransition + ":" + dest.window.transCount + ":" + dest.imageWidth;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("1:1:2".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.5), Vec::new()),
                Duration::from_millis(500),
            )
            .expect("mid update");
        assert!(!frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "dest.window.transCount")
                .expect("trans count"),
            Variant::Integer(1)
        );

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.5), Vec::new()),
                Duration::from_millis(500),
            )
            .expect("finish update");
        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.inTransition + ':' + dest.window.transCount + ':' + dest.window.completed + ':' + dest.window.completedWidth + ':' + dest.window.sourceWidth"
                )
                .expect("completion"),
            Variant::String("0:0:1:2:2".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_transition_notifies_secondary_tjs_base_class() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                class TransitionHook {
                    function onTransitionCompleted(dest, src) {
                        this.window.completed++;
                    }
                }
                class TransitionLayer extends Layer, TransitionHook {
                    function TransitionLayer(window) { super.Layer(window); }
                }

                global.window = %[transCount: 1, completed: 0, inTransition: 0];
                // The constructor already gave `dest` its window
                // (`Layer.window` has no setter, `LayerIntf.cpp:8689`).
                global.dest = new TransitionLayer(window);
                var source = new Layer();
                dest.inTransition = true;
                dest.beginTransition("crossfade", false, source, %[time: 0]);
                return window.completed + ":" + dest.inTransition + ":" + window.transCount;
                "#,
            )
            .expect("script");

        // `time` is floored at 2 ms (`TransIntf.cpp:530`), so the completion
        // event lands on the next tick.
        assert_eq!(result, Variant::String("0:1:1".to_string()));
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(2),
            )
            .expect("complete transition");
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "window.completed + ':' + dest.inTransition + ':' + window.transCount"
                )
                .expect("completion"),
            Variant::String("1:0:0".to_string())
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Official `tTJSNI_BaseLayer::StartTransition` (`LayerIntf.cpp:6188`): a
    /// transition already running on the destination throws
    /// `TVPCurrentTransitionMustBeStopping`, and the running one is untouched.
    #[test]
    fn native_layer_second_begin_transition_throws_while_one_is_running() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 1, 1, &[255, 0, 0, 255]);
        write_png(
            root.join("mid.png"),
            2,
            1,
            &[0, 255, 0, 255, 0, 255, 0, 255],
        );
        write_png(
            root.join("new.png"),
            3,
            1,
            &[0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255],
        );

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.transWindow = %[transCount: 0, completed: 0, lastSourceWidth: 0];
                global.dest = new Layer(transWindow, null);
                dest.loadImages("old.png");
                dest.visible = true;
                dest.onTransitionCompleted = function(destLayer, srcLayer) {
                    this.window.completed++;
                    this.window.lastSourceWidth = srcLayer.imageWidth;
                    this.window.transCount--;
                    this.inTransition = this.window.transCount > 0;
                };
                function startTransition(storage) {
                    var source = new Layer();
                    source.loadImages(storage);
                    // KAG's `assign` copies the destination's visible state
                    // onto the source before the transition
                    // (`assignVisibleState`, KAGLayer.tjs:172), which is what
                    // keeps both faces the same size for the crossfade family
                    // (`TransIntf.cpp:508`).
                    source.setPos(dest.left, dest.top, dest.width, dest.height);
                    dest.inTransition = true;
                    dest.window.transCount++;
                    dest.beginTransition("crossfade", true, source, %[time: 1000]);
                }
                startTransition("mid.png");
                var thrown = "";
                try {
                    startTransition("new.png");
                } catch(e) {
                    thrown = e.message;
                }
                return dest.inTransition + ":" + dest.window.transCount + ":" + dest.window.completed + ":" + thrown;
                "#,
            )
            .expect("script");

        assert_eq!(
            result,
            Variant::String("1:2:0:Current transition must be stopping".to_string())
        );

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 1.0), Vec::new()),
                Duration::from_millis(1000),
            )
            .expect("finish update");
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.inTransition + ':' + dest.window.transCount + ':' + dest.window.completed + ':' + dest.window.lastSourceWidth"
                )
                .expect("completion"),
            Variant::String("1:1:1:2".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Official `tTJSNI_BaseLayer::StartTransition` (`LayerIntf.cpp:6193`):
    /// starting a transition whose source is itself the destination of a
    /// running transition throws `TVPTransitionMutualSource`.
    #[test]
    fn native_layer_mutual_source_transition_throws() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("face.png"), 2, 1, &[255; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.front = new Layer();
                front.loadImages("face.png");
                front.visible = true;
                global.back = new Layer();
                back.loadImages("face.png");
                back.visible = true;
                front.beginTransition("crossfade", true, back, %[time: 1000]);
                var thrown = "";
                try {
                    back.beginTransition("crossfade", true, front, %[time: 1000]);
                } catch(e) {
                    thrown = e.message;
                }
                return thrown;
                "#,
            )
            .expect("script");

        assert_eq!(
            result,
            Variant::String("Transition mutual source".to_string())
        );
        assert_eq!(engine.host().active_transition_count(), 1);

        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Two unrelated layers run their own transition at the same time
    /// (`tTJSNI_BaseLayer::InTransition` is per layer, `LayerIntf.cpp:6334`),
    /// each completing on its own clock.
    #[test]
    fn native_layers_transition_concurrently_on_separate_clocks() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("face.png"), 2, 1, &[255; 8]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = %[completed: 0, firstDone: -1, secondDone: -1];
                function make() {
                    // `Layer.window` is read-only (`LayerIntf.cpp:8689`): the
                    // window arrives in the constructor.
                    var layer = new Layer(window, null);
                    layer.loadImages("face.png");
                    layer.visible = true;
                    layer.inTransition = true;
                    return layer;
                }
                global.shortDest = make();
                global.shortSrc = make();
                shortDest.onTransitionCompleted = function(dest, src) {
                    this.inTransition = false;
                    this.window.completed++;
                    this.window.firstDone = this.window.completed;
                };
                global.longDest = make();
                global.longSrc = make();
                longDest.onTransitionCompleted = function(dest, src) {
                    this.inTransition = false;
                    this.window.completed++;
                    this.window.secondDone = this.window.completed;
                };
                shortDest.beginTransition("crossfade", true, shortSrc, %[time: 100]);
                longDest.beginTransition("crossfade", true, longSrc, %[time: 500]);
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.05), Vec::new()),
                Duration::from_millis(50),
            )
            .expect("first update");
        assert_eq!(frame.output.transitions.len(), 2);

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.10), Vec::new()),
                Duration::from_millis(50),
            )
            .expect("second update");
        // Only the short transition is over; the long one keeps running on its
        // own destination layer.
        assert_eq!(frame.output.transitions.len(), 1);
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "window.completed + ':' + shortDest.inTransition + ':' + longDest.inTransition"
                )
                .expect("state"),
            Variant::String("1:0:1".to_string())
        );

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.55), Vec::new()),
                Duration::from_millis(450),
            )
            .expect("third update");
        assert!(frame.output.transitions.is_empty());
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "window.completed + ':' + window.firstDone + ':' + window.secondDone"
                )
                .expect("state"),
            Variant::String("2:1:2".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_stop_transition_completes_active_transition() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        // Same size on both faces (`TransIntf.cpp:508`).
        write_png(
            root.join("old.png"),
            2,
            1,
            &[255, 0, 0, 255, 255, 0, 0, 255],
        );
        write_png(
            root.join("new.png"),
            2,
            1,
            &[0, 255, 0, 255, 0, 255, 0, 255],
        );

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.loadImages("new.png");
                global.transWindow = %[transCount: 1, completed: 0];
                global.dest = new Layer(transWindow, null);
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.onTransitionCompleted = function(destLayer, srcLayer) {
                    this.inTransition = false;
                    this.window.transCount--;
                    this.window.completed++;
                };
                dest.beginTransition("crossfade", true, source, %[time: 1000]);
                dest.stopTransition();
                return dest.inTransition + ":" + dest.window.transCount + ":" + dest.window.completed;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("0:0:1".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.transitions.is_empty());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_free_image_clears_uploaded_image() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 1, 1, &[0, 255, 0, 255]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.visible = true;
                layer.loadImages("sprite.png");
                layer.freeImage();
                try {
                    return layer.imageWidth + ":" + layer.imageHeight + ":" + layer.width;
                } catch(e) {
                    return "thrown:" + layer.hasImage + ":" + layer.width;
                }
                "#,
            )
            .expect("script");

        // `LoadImages` shrinks the Rect to the decoded size
        // (`InternalSetImageSize`, `LayerIntf.cpp:2494`), so after
        // `freeImage` the layer is 1×1 with no bitmap.
        assert_eq!(result, Variant::String("thrown:0:1".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(frame.output.image_uploads.is_empty());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_set_image_size_preserves_existing_pixels() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.setImageSize(2, 2);
                layer.fillRect(0, 0, 2, 2, 0xffff0000);
                layer.setImageSize(3, 2);
                return layer.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;

        let image = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("layer image");
        assert_eq!(image.upload.width, 3);
        assert_eq!(image.upload.height, 2);
        assert_eq!(
            image.upload.rgba.as_ref(),
            &[
                255, 0, 0, 255, 255, 0, 0, 255, 255, 255, 255, 0, 255, 0, 0, 255, 255, 0, 0, 255,
                255, 255, 255, 0,
            ]
        );
    }

    #[test]
    fn native_layer_set_image_size_resizes_the_province_plane() {
        // `ChangeImageSize` resizes both planes (`LayerIntf.cpp:2043-2047`):
        // the province values that survive the size change are kept and the
        // expanded band is zero-filled.
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.setImageSize(2, 2);
                layer.setProvincePixel(1, 1, 9);
                layer.setImageSize(3, 2);
                return layer.getProvincePixel(1, 1) + ":" +
                    layer.getProvincePixel(2, 1) + ":" +
                    layer.imageWidth + ":" + layer.imageHeight;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("9:0:3:2".to_string()));
    }

    #[test]
    fn array_index_read_past_the_end_is_void_like_krkr() {
        // KAGEX's `system.tjs` `_applyEntries` splits a config line and reads
        // `l2[1]` even when the split produced a single element; official
        // `ARRAY_GET_VAL` (`tjsArray.cpp:1404`) answers void there, while a
        // `typeof` of the same index must report `undefined` because the
        // opcode passes TJS_MEMBERMUSTEXIST (`tjsInterCodeExec.cpp:1296`).
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var single = "flag".split("=");
                var pair = "name=value".split("=");
                return (single.count)
                    + "/" + (single[1] === void)
                    + "/" + (typeof single[1])
                    + "/" + pair[1]
                    + "/" + single[-1];
                "#,
            )
            .expect("script");
        assert_eq!(
            result.to_tjs_string().expect("string"),
            "1/1/undefined/value/flag"
        );
    }

    #[test]
    fn property_getter_reads_a_void_instance_member_like_krkr() {
        // KAGEX's `KAGWindow` sets `this._BookMarkIO = void` in its class body
        // and its `BookMarkIO` property getter reads that bare name. Inside a
        // method a bare name compiles against `%-2`, the objthis proxy the VM
        // builds when it enters a context (`tjsInterCodeExec.cpp:791`): the
        // instance first, the global object second.
        // `tTJSObjectProxy::PropGet` (`tjsInterCodeExec.cpp:284`) falls through
        // to the second object only for TJS_E_MEMBERNOTFOUND, so an instance
        // member that exists and holds void must be the result instead of
        // being treated as absent.
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                class Probe {
                    this.g = void;
                    property G {
                        getter() { if (g === void) { g = 5; } return g; }
                        setter(value) { g = value; }
                    }
                    function Probe() { }
                }
                global.probe = new Probe();
                var first = probe.G;
                probe.G = 9;
                return first + "/" + probe.G;
                "#,
            )
            .expect("script");
        assert_eq!(result.to_tjs_string().expect("string"), "5/9");
    }

    #[test]
    fn tvp_global_constants_match_the_reference_init_script() {
        // `TVPInitTJSScript` (`ScriptMgnIntf.cpp:58`) is evaluated at script
        // engine init, so the stock KAG `Config.tjs` line
        // `;System.graphicCacheLimit = gcsAuto;` -- where `;` is TJS2's empty
        // statement (`tjs.y:243`), not a comment -- reads a name that must
        // exist. The dictionary and a handful of values stand in for the whole
        // table: `fsfUseFontFace` and `fsfIgnoreSymbol` are easy to swap,
        // `ltPsExclusion` is the end of the Photoshop blend family.
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                return gcsAuto
                    + "/" + imageTagLayerType.addalpha.type
                    + "/" + imageTagLayerType.psexcl.type
                    + "/" + fsfUseFontFace
                    + "/" + fsfIgnoreSymbol
                    + "/" + VK_F1
                    + "/" + dtnAuto;
                "#,
            )
            .expect("script");
        assert_eq!(
            result.to_tjs_string().expect("string"),
            "-1/12/28/256/16/112/0"
        );
    }

    #[test]
    fn layer_font_reads_back_bound_to_the_font_like_krkr() {
        // `tTJSNI_BaseLayer`'s font getter returns `tTJSVariant(dsp, dsp)`
        // (LayerIntf.cpp:8875). TJS2 resolves a write's objthis with
        // `Object.ObjThis ? Object.ObjThis : ra[-1]`, so a write through
        // `layer.font` must reach the font. KAGEX relies on exactly that:
        // `objectHookInjection` gives the font a property whose setter
        // forwards through `this`, and the setter has to see the font.
        // Writing from another object's method makes the caller's `this` the
        // writer, which is what the write falls back to when the font value
        // carries no ObjThis.
        //
        // The two store shapes follow the reference: `layer.font = x` meets
        // the denied setter (`TJS_DENY_NATIVE_PROP_SETTER`, `:9449`), while
        // the `TJS_IGNOREPROP` store `&layer.font = x` skips the property and
        // overwrites the member -- which is how KAGEX replaces a layer's font
        // (`sysscn/prerenderfontex.tjs`, `spds` at bytecode 58).  The stored
        // value carries its own `ObjThis` (`tTJSVariant(objthis, objthis)`),
        // so the hook's setter still runs against the font, not the writer.
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let seen = engine
            .execute_script(
                "inline.tjs",
                r#"
                class FontLike {
                    var seen;
                    function FontLike() { this.seen = "init"; }
                }
                class Writer {
                    var holder;
                    function Writer(holder) { this.holder = holder; }
                    function poke() { this.holder.font.probe = 456; }
                }
                global.hooked = function(v) { this.seen = "set:" + v; };
                var build = function() {
                    ("property probe { setter(v) { (global.hooked incontextof this)(v); } }")!;
                    return this.probe;
                } incontextof (new Dictionary());
                global.fontLike = new FontLike();
                fontLike.probe = build() incontextof null;
                global.layer = new Layer();
                var denied = 0;
                try { layer.font = fontLike; } catch (e) {
                    denied = (e.message == "Invalid operation for Read-only or Write-only property");
                }
                &layer.font = fontLike;
                global.writer = new Writer(layer);
                writer.poke();
                return denied + "/" + fontLike.seen + "/" + (typeof writer.seen);
                "#,
            )
            .expect("script");
        assert_eq!(seen.to_tjs_string().expect("string"), "1/set:456/undefined");
    }

    /// The KAGEX font-hook shape end to end, with a real native font behind the
    /// hook: `&layer.font = hook` installs it, a write from another object's
    /// method (`layer.font.height = v`) runs the hook's injected setter with
    /// the *hook* as `this` -- so its unqualified `fontSetter(...)` resolves --
    /// and the engine's own font resolution reads back through the hook.
    #[test]
    fn layer_font_hook_installed_with_ignore_prop_runs_on_the_hook() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class FontHook {
                    var seen;
                    var inner;
                    function FontHook(inner) { this.inner = inner; }
                    function fontSetter(v) { inner.height = v; this.seen = "set:" + v; }
                    property height {
                        getter { this.seen = "get:" + inner.height; return inner.height; }
                        setter(v) { fontSetter(v); }
                    }
                }
                class Writer {
                    var holder;
                    function Writer(holder) { this.holder = holder; }
                    function paint(v) { this.holder.font.height = v; }
                }
                var inner = new Font();
                inner.height = 24;
                global.hook = new FontHook(inner);
                global.textLayer = new Layer();
                textLayer.setSize(96, 48);
                var denied = 0;
                try { textLayer.font = hook; } catch (e) {
                    denied = (e.message == "Invalid operation for Read-only or Write-only property");
                }
                &textLayer.font = hook;
                var installed = (textLayer.font === hook);
                (new Writer(textLayer)).paint(-32);
                var afterSet = hook.seen + ":" + inner.height;
                textLayer.drawText(4, 4, "A", 0xffffff, 255);
                return denied + ":" + installed + ":" + afterSet + ":" + hook.seen;
                "#,
            )
            .expect("script");
        assert_eq!(
            value.to_tjs_string().expect("string"),
            "1:1:set:-32:-32:get:-32"
        );
    }

    #[test]
    fn native_class_constructor_member_runs_on_the_callers_object() {
        // `TJS_END_NATIVE_CONSTRUCTOR_DECL(Layer)` ends in
        // `RegisterNCM("Layer", new NCM_Layer())` (tjsNative.h:233): the
        // class object carries a member named after the class, stored with no
        // ObjThis. VM_CALLD hands the callee `clo.ObjThis ? clo.ObjThis :
        // ra[-1]` (tjsInterCodeExec.cpp:2015), so `Layer.Layer(win, this)`
        // from a class whose *first* parent is not Layer -- KAGEX's
        // `GUIAnimButtonObjectBase` and this game's title buttons -- runs the
        // layer initializer on the caller's object. Missing the member made
        // the call fail with `void is not callable`; binding it to the class
        // object would have attached the window to the class instead of the
        // instance.
        //
        // The call itself answers void: `tTJSNativeClassConstructor::FuncCall`
        // clears the result before it runs `Process` (`tjsNative.cpp:113-134`)
        // and the constructor never writes it (`LayerIntf.cpp:6717-6720`), so
        // the script asks whether the value is void rather than comparing it
        // with the caller's object.
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.win = new Window();
                class Holder {
                    var window;
                    function Holder() { this.window = global.win; }
                }
                class Mixin { function Mixin() {} }
                class Base extends Mixin, Layer {
                    var _Layer = global.Layer;
                    var resultIsVoid = 0;
                    function Base(a0) {
                        var t1 = a0.window;
                        var r = _Layer.Layer(t1, a0);
                        this.resultIsVoid = (r === void);
                    }
                }
                global.holder = new Holder();
                global.base = new Base(holder);
                // `window`/`parent` read back as `tTJSVariant(dsp, dsp)`
                // (`LayerIntf.cpp:8490`, `:8683`), whose `===` also compares
                // ObjThis; identity is what `==` reports.
                return base.resultIsVoid + "/" + (base.window == win) + "/"
                    + (base.parent == holder) + "/" + (base instanceof "Layer");
                "#,
            )
            .expect("script")
            .to_tjs_string()
            .expect("string");
        assert_eq!(value, "1/1/1/1");
    }

    #[test]
    fn native_layer_property_setters_update_render_tree_without_frame_sync() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.left = 12;
                layer.top = 34;
                layer.width = 56;
                layer.height = 78;
                layer.visible = true;
                layer.opacity = 123;
                return layer.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;

        let layer = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .expect("native layer");
        assert_eq!(layer.left, 12.0);
        assert_eq!(layer.top, 34.0);
        assert_eq!(layer.width, 56.0);
        assert_eq!(layer.height, 78.0);
        assert!(layer.visible);
        assert_eq!(layer.opacity, 123);
    }

    #[test]
    fn native_layer_parent_setter_updates_render_tree_and_hit_testing_immediately() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.root1 = new Layer();
                root1.setPos(10, 10);
                root1.setSize(100, 100);
                root1.visible = true;

                global.root2 = new Layer();
                root2.setPos(200, 10);
                root2.setSize(100, 100);
                root2.visible = true;

                global.child = new Layer(null, root1);
                child.setPos(5, 5);
                child.setSize(10, 10);
                child.setImageSize(10, 10);
                child.fillRect(0, 0, 10, 10, 0xffffffff);
                child.visible = true;
                "#,
            )
            .expect("script");
        let child = object_handle(&engine, "child");
        let child_layer = engine.host().native_layer(child).expect("child layer");

        assert_eq!(
            engine.host().layer_tree().hit_test(Point::new(16.0, 16.0)),
            Some(child_layer)
        );

        engine
            .execute_script("inline.tjs", "child.parent = root2;")
            .expect("reparent");
        assert_eq!(
            engine.host().layer_tree().absolute_position(child_layer),
            Some(Point::new(205.0, 15.0))
        );
        assert_eq!(
            engine.host().layer_tree().hit_test(Point::new(206.0, 16.0)),
            Some(child_layer)
        );
        assert_eq!(
            engine.host().layer_tree().hit_test(Point::new(16.0, 16.0)),
            None
        );
    }

    #[test]
    fn native_layer_invalidate_removes_child_from_render_tree_and_hit_testing() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var parentLayer = new Layer();
                parentLayer.setPos(10, 20);
                parentLayer.setSize(100, 100);
                parentLayer.visible = true;

                global.buttonClicks = 0;
                global.buttonLayer = new Layer(null, parentLayer);
                buttonLayer.setPos(3, 4);
                buttonLayer.setSize(5, 6);
                buttonLayer.setImageSize(5, 6);
                buttonLayer.fillRect(0, 0, 5, 6, 0xffffffff);
                buttonLayer.visible = true;
                buttonLayer.onMouseUp = function(x, y, button, shift) {
                    global.buttonClicks += 1;
                };
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("initial frame");
        // `new Layer()` always owns the ctor's 32×32 holder
        // (`AllocateDefaultImage`, `LayerIntf.cpp:404`), and `setSize(100, 100)`
        // grows the parent's bitmap (`ImageLayerSizeChanged`, `:2388`), so the
        // parent uploads a neutral 100×100 image as well.
        assert_eq!(frame.output.image_uploads.len(), 2);
        assert!(
            frame
                .output
                .image_uploads
                .iter()
                .any(|upload| upload.width == 5 && upload.height == 6)
        );

        engine
            .execute_script("cleanup.tjs", "invalidate buttonLayer;")
            .expect("invalidate");
        let frame = engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(14.0, 25.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("post-invalidate frame");

        assert_eq!(frame.output.image_uploads.len(), 0);
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "buttonClicks")
                .expect("buttonClicks"),
            Variant::Integer(0)
        );
    }

    #[test]
    fn native_layer_invalidate_parts_children_instead_of_destroying_them() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.root = new Layer();
                global.child = new Layer(null, root);
                child.update();
                "#,
            )
            .expect("script");
        let root = object_handle(&engine, "root");
        let child = object_handle(&engine, "child");
        assert!(engine.host().native_layer(root).is_some());
        assert!(engine.host().native_layer(child).is_some());
        assert!(engine.host().has_pending_window_update(child));

        engine
            .execute_script("cleanup.tjs", "invalidate root;")
            .expect("invalidate");
        // Official `Invalidate` only `Part()`s children (`LayerIntf.cpp:513`).
        assert!(engine.host().native_layer(root).is_none());
        assert!(engine.host().native_layer(child).is_some());
        engine
            .execute_script(
                "draw.tjs",
                r#"
                child.setImageSize(2, 2);
                child.fillRect(0, 0, 2, 2, 0xff00ff00);
                return child.hasImage;
                "#,
            )
            .expect("child still drawable");
        let child_id = engine
            .host()
            .native_layer(child)
            .expect("child native layer");
        let image = engine
            .host()
            .layer_tree()
            .layer(child_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("child image");
        assert_eq!(image.upload.rgba.as_ref()[..4], [0, 255, 0, 255]);
    }

    /// Draw and hit test walk from each manager's primary layer
    /// (`tTVPLayerManager::RecreateOverallOrderIndex`, `LayerManager.cpp:188`),
    /// so a child that `Invalidate` `Part()`s from its parent
    /// (`LayerIntf.cpp:514`) leaves the tree and stops being drawn.  Kirakira
    /// materializes parentless nodes as render roots, and KAGEX keeps writing
    /// layer properties right after tearing a page down; that re-applies the
    /// instance and used to put the parted child back on screen at the
    /// coordinates its parent's clip used to hide.
    #[test]
    fn invalidated_parent_keeps_partied_child_off_screen_after_reapply() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let cell_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.page = new Layer(window, null);
                page.setSize(320, 240);
                page.hasImage = 0;
                global.container = new Layer(window, page);
                container.setSize(320, 240);
                container.hasImage = 0;
                global.cell = new Layer(window, container);
                cell.setSize(64, 64);
                cell.left = 400; // past the page edge: only the parent's clip hides it
                cell.hasImage = 1;
                return cell.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("parented frame");
        let texture_id = engine
            .host()
            .layer_tree()
            .layer(cell_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("cell image")
            .upload
            .texture_id;
        let draws_cell = |frame: &EngineFrame| {
            frame
                .output
                .draw_commands
                .iter()
                .any(|command| matches!(command, krkr_core::DrawCommand::Image(image) if image.texture_id == texture_id))
        };
        assert!(!draws_cell(&frame), "the page clip hides the cell");

        engine
            .execute_script("cleanup.tjs", "invalidate container; cell.left = 401;")
            .expect("invalidate and re-apply");
        assert!(
            !engine
                .host()
                .layer_tree()
                .layer(cell_id)
                .expect("parted cell node")
                .renderable
        );
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("parted frame");
        assert!(!draws_cell(&frame), "a parted layer must stay off-screen");

        // `Join` back into the tree makes it draw again.
        engine
            .execute_script("cleanup.tjs", "cell.parent = page;")
            .expect("rejoin");
        assert!(
            engine
                .host()
                .layer_tree()
                .layer(cell_id)
                .expect("rejoined cell node")
                .renderable
        );
    }

    /// GINKA's stand pool keeps `StandLayer` objects after `invalidate` and
    /// still calls `hasImage = 1` / `fillRect` / `copyRect` on them.
    #[test]
    fn native_layer_drawing_reattaches_after_invalidate() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.dest = new Layer();
                dest.setSize(4, 4);
                dest.setImageSize(4, 4);
                invalidate dest;
                dest.hasImage = 1;
                dest.fillRect(0, 0, 4, 4, 0xff112233);
                return dest.hasImage;
                "#,
            )
            .expect("script");
        let dest = object_handle(&engine, "dest");
        let dest_id = engine
            .host()
            .native_layer(dest)
            .expect("reattached native layer");
        let image = engine
            .host()
            .layer_tree()
            .layer(dest_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("reattached image");
        assert_eq!(image.upload.rgba.as_ref()[..4], [0x11, 0x22, 0x33, 255]);
    }

    #[test]
    fn native_layer_fill_rect_creates_solid_rgba_image() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.visible = true;
                layer.setImageSize(2, 2);
                layer.setSizeToImageSize();
                layer.fillRect(1, 0, 1, 2, 0x80402010);
                return layer.imageWidth + ":" + layer.imageHeight + ":" + layer.width;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("2:2:2".to_string()));
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert_eq!(frame.output.image_uploads.len(), 1);
        assert_eq!(
            frame.output.image_uploads[0].rgba.as_ref(),
            &[
                255, 255, 255, 0, 0x40, 0x20, 0x10, 0x80, 255, 255, 255, 0, 0x40, 0x20, 0x10, 0x80
            ]
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_stretch_copy_scales_source_pixels() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(2, 2);
                source.fillRect(0, 0, 1, 1, 0xffff0000);
                source.fillRect(1, 0, 1, 1, 0xff00ff00);
                source.fillRect(0, 1, 1, 1, 0xff0000ff);
                source.fillRect(1, 1, 1, 1, 0xffffffff);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.stretchCopy(0, 0, 4, 4, source, 0, 0, 2, 2, 0);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;

        let image = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("layer image");
        let red = [255, 0, 0, 255];
        let green = [0, 255, 0, 255];
        let blue = [0, 0, 255, 255];
        let white = [255, 255, 255, 255];
        let mut expected = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                expected.extend_from_slice(match (x >= 2, y >= 2) {
                    (false, false) => &red,
                    (true, false) => &green,
                    (false, true) => &blue,
                    (true, true) => &white,
                });
            }
        }
        assert_eq!(image.upload.rgba.as_ref(), expected.as_slice());
    }

    #[test]
    fn image_less_child_layer_wins_cursor_over_its_parent() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                var win = new Window();
                var message = new Layer(win, null);
                message.name = "message";
                message.setPos(0, 0);
                message.setSize(320, 240);
                message.setImageSize(320, 240);
                message.fillRect(0, 0, 320, 240, 0xff808080);
                message.hitType = htMask;
                message.hitThreshold = 16;
                message.visible = true;
                message.onMouseMove = function(x, y, shift) { global.events += "message;"; };

                var hack = new Layer(win, message);
                hack.name = "hack";
                hack.setPos(0, 0);
                hack.setSize(320, 240);
                hack.hasImage = 0;
                hack.hitType = htMask;
                hack.hitThreshold = 256;
                hack.hitThreshold = 0;
                hack.visible = 1;
                hack.onMouseMove = function(x, y, shift) {
                    global.events += (this === hack ? "hack;" : "other;");
                };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::CursorMoved {
                        position: Point::new(100.0, 100.0),
                    }],
                ),
                Duration::ZERO,
            )
            .expect("move frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "events")
                .expect("events"),
            Variant::String("hack;".to_string())
        );
    }

    #[test]
    fn script_subclass_with_call_missing_receives_cursor_as_itself() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                var win = new Window();
                var foreBase = new Layer(win, null);
                foreBase.setPos(0, 0);
                foreBase.setSize(320, 240);
                foreBase.setImageSize(320, 240);
                foreBase.fillRect(0, 0, 320, 240, 0xff000000);
                foreBase.visible = true;

                var message = new Layer(win, foreBase);
                message.name = "message";
                message.setPos(0, 0);
                message.setSize(320, 240);
                message.setImageSize(320, 240);
                message.fillRect(0, 0, 320, 240, 0xff808080);
                message.hitType = htMask;
                message.hitThreshold = 16;
                message.visible = true;
                message.onMouseMove = function(x, y, shift) { global.events += "message;"; };

                class ParentHackLayer extends Layer {
                    function ParentHackLayer(a0, a1) {
                        super.Layer(a0, a1);
                        name = "ParentHackLayer";
                        hasImage = 0;
                        hitType = htMask;
                        hitThreshold = 256;
                        Scripts.setCallMissing(this);
                    }
                    property __hackTarget {
                        getter() { return this.parent; }
                    }
                    function missing(a0, a1, a2) {
                        var l0;
                        if (this isvalid) {
                            l0 = this.__hackTarget;
                            if (typeof this.__hackTarget == "Object" && l0 && l0 isvalid) {
                                if (!(typeof l0[a1] == "undefined")) {
                                    if (a0) {
                                        l0[a1] = *a2;
                                    } else {
                                        var t1 = l0[a1];
                                        *a2 = t1;
                                    }
                                    return 1;
                                }
                            }
                        }
                        return 0;
                    }
                }

                var hack = new ParentHackLayer(win, message);
                hack.setPos(0, 0);
                hack.setSize(320, 240);
                hack.visible = 1;
                hack.hitThreshold = 0;
                hack.dragHookOwner = "owner";
                hack.onMouseMove = function(x, y, shift) {
                    global.events += "wrapper:" + this.dragHookOwner + ";";
                };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::CursorMoved {
                        position: Point::new(100.0, 100.0),
                    }],
                ),
                Duration::ZERO,
            )
            .expect("move frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "events")
                .expect("events"),
            Variant::String("wrapper:owner;".to_string())
        );
    }

    #[test]
    fn native_layer_piled_copy_composites_child_layers() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var base = new Layer();
                base.visible = true;
                base.setSize(4, 4);
                base.setImageSize(4, 4);
                base.fillRect(0, 0, 4, 4, 0xff202020);

                var child = new Layer(null, base);
                child.visible = true;
                child.setPos(1, 1);
                child.setSize(2, 2);
                child.setImageSize(2, 2);
                child.fillRect(0, 0, 2, 2, 0xffff0000);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.piledCopy(0, 0, base, 0, 0, 4, 4);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;

        let image = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("layer image");
        let base = [0x20, 0x20, 0x20, 255];
        // Official packed blend rounding (`>> 8`) turns full alpha into 254.
        let red = [254, 0, 0, 255];
        let mut expected = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                if (1..3).contains(&x) && (1..3).contains(&y) {
                    expected.extend_from_slice(&red);
                } else {
                    expected.extend_from_slice(&base);
                }
            }
        }
        assert_eq!(image.upload.rgba.as_ref(), expected.as_slice());
    }

    /// `tTJSNI_BaseLayer::convertType` (`LayerIntf.cpp:7624`) →
    /// `ConvertLayerType` (`:1703-1727`): the `dfAlpha`/`dfAddAlpha` pixel
    /// rewrites, their round trip and where the conversion is lossy.
    #[test]
    fn native_layer_convert_type_rewrites_alpha_representations() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var layer = new Layer();
                layer.setImageSize(2, 1);
                layer.setMainPixel(0, 0, 0x00123456);
                layer.setMaskPixel(0, 0, 0x80);
                layer.setMainPixel(1, 0, 0x00ffffff);
                layer.setMaskPixel(1, 0, 0xff);

                layer.type = ltAddAlpha;                  // dfAddAlpha destination
                layer.convertType(dfAlpha);               // premultiply
                var premultiplied = layer.getMainPixel(0, 0) + ":" +
                    layer.getMaskPixel(0, 0) + ":" + layer.getMainPixel(1, 0);

                // The target representation is the layer's own draw face, so
                // the way back switches the layer type first (`:1707-1717`).
                layer.type = ltAlpha;
                layer.convertType(dfAddAlpha);            // unpremultiply again
                var roundtrip = layer.getMainPixel(0, 0) + ":" +
                    layer.getMaskPixel(0, 0) + ":" + layer.getMainPixel(1, 0);
                return premultiplied + "/" + roundtrip;
                "#,
            )
            .expect("script")
            .to_tjs_string()
            .expect("string");
        // 0x12/0x34/0x56 at alpha 0x80 scale to 0x09/0x1a/0x2b. The `>> 8`
        // truncation makes the premultiply lossy even at full alpha (0xff
        // becomes 0xfe, `blend_functor_c.h:871-876`), and coming back through
        // `TVPDivTable` (`visual/tvpgl.c:251-260`) gives 9*255/128 = 17,
        // 26*255/128 = 51 and 43*255/128 = 85.
        assert_eq!(
            value,
            "596523:128:16711422/1127253:128:16711422".to_string()
        );
    }

    /// `ConvertLayerType` throws `TVPCannotConvertLayerTypeUsingGivenDirection`
    /// for every pairing that is not `dfAlpha -> dfAddAlpha` or back
    /// (`LayerIntf.cpp:1718-1722`), needs an argument (`:7628`), and does
    /// nothing but flag the layer on a layer whose image was freed (`:1710`).
    #[test]
    fn native_layer_convert_type_error_surface() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(1, 1);
                "#,
            )
            .expect("script");
        for call in [
            "layer.convertType();",
            // The default `new Layer()` is `ltAlpha` (`dfAlpha`), so asking it
            // to convert a `dfAlpha` image has no direction.
            "layer.convertType(dfAlpha);",
        ] {
            let error = engine.execute_script("inline.tjs", call).expect_err(call);
            assert!(
                error.message.contains("Cannot convert layer type")
                    || error.message.contains("Invalid argument count"),
                "{call}: {}",
                error.message
            );
        }
        // `ltOpaque` resolves to `dfOpaque`, which has no conversion either way.
        let error = engine
            .execute_script(
                "inline.tjs",
                "layer.type = ltOpaque; layer.convertType(dfAlpha);",
            )
            .expect_err("dfOpaque has no conversion");
        assert!(
            error.message.contains("Cannot convert layer type"),
            "{}",
            error.message
        );
        // A freed image is not converted and not resurrected.
        engine
            .execute_script(
                "inline.tjs",
                "layer.type = ltAddAlpha; layer.freeImage(); layer.convertType(dfAlpha);",
            )
            .expect("an image-less layer converts nothing");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "layer.hasImage")
                .expect("hasImage")
                .to_integer()
                .expect("integer"),
            0
        );
    }

    /// Every blit family refuses a destination whose image was freed instead of
    /// allocating one (`TVPNotDrawableLayerType`; `LayerIntf.cpp:4111`
    /// `PiledCopy`, `:4159` `CopyRect`, `:4245` `StretchCopy`, `:4287`
    /// `AffineCopy`, `:4376` `OperateRect`, `:4417` `OperateStretch`, `:4453`
    /// `OperateAffine`).
    #[test]
    fn native_layer_blits_reject_a_destination_without_an_image() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.setImageSize(2, 2);
                source.fillRect(0, 0, 2, 2, 0xffff0000);

                global.target = new Layer();
                target.setSize(4, 4);
                target.freeImage();
                "#,
            )
            .expect("script");
        for call in [
            "target.copyRect(0, 0, source, 0, 0, 2, 2);",
            "target.operateRect(0, 0, source, 0, 0, 2, 2, omAlpha);",
            "target.stretchCopy(0, 0, 4, 4, source, 0, 0, 2, 2, stNearest);",
            "target.operateStretch(0, 0, 4, 4, source, 0, 0, 2, 2, omAlpha);",
            "target.affineCopy(source, 0, 0, 2, 2, false, 0, 0, 4, 0, 0, 4);",
            "target.operateAffine(source, 0, 0, 2, 2, false, 0, 0, 4, 0, 0, 4, omAlpha);",
            "target.piledCopy(0, 0, source, 0, 0, 2, 2);",
            // The fill family takes the same check (`:3866`, `:3936`).
            "target.fillRect(0, 0, 1, 1, 0xffff0000);",
            "target.colorRect(0, 0, 1, 1, 0xffff0000);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("a freed bitmap cannot be a blit target");
            assert!(
                error.message.contains("Not drawable layer type"),
                "{call}: {}",
                error.message
            );
        }
        // None of those calls may have fabricated a bitmap.
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "target.hasImage")
                .expect("hasImage")
                .to_integer()
                .expect("integer"),
            0
        );
    }

    /// `piledCopy` requires a main image on the destination *and* on the source
    /// layer (`LayerIntf.cpp:4111-4112`).
    #[test]
    fn native_layer_piled_copy_requires_both_images() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.dest = new Layer();
                dest.setImageSize(2, 2);
                global.source = new Layer();
                source.setSize(2, 2);
                source.freeImage();
                "#,
            )
            .expect("script");
        let error = engine
            .execute_script("inline.tjs", "dest.piledCopy(0, 0, source, 0, 0, 2, 2);")
            .expect_err("an image-less source cannot be piled");
        assert!(
            error.message.contains("Source layer has no image"),
            "{}",
            error.message
        );
    }

    /// `BltImage`'s per-child rules inside a pile (`LayerIntf.cpp:5164-5364`):
    /// the blt method follows the child's `DisplayType` while the
    /// `OnAlpha`/`hda` selection follows the *destination* bitmap's layer type
    /// (`:5191-5203`, `:5255-5257`), and an `ltBinder` child draws nothing
    /// (`:5185-5187`).
    #[test]
    fn native_layer_piled_copy_uses_the_destination_layer_type() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var base = new Layer();
                base.type = ltOpaque;               // a non-alpha destination
                base.visible = true;
                base.setSize(2, 2);
                base.setImageSize(2, 2);
                base.fillRect(0, 0, 2, 2, 0xff00ff00);

                var child = new Layer(null, base);
                child.type = ltOpaque;
                child.visible = true;
                child.setSize(1, 1);
                child.setImageSize(1, 1);
                child.fillRect(0, 0, 1, 1, 0x80ff0000);   // half-transparent red

                var binder = new Layer(null, base);
                binder.visible = true;
                binder.setPos(1, 1);
                binder.setSize(1, 1);
                binder.setImageSize(1, 1);
                binder.fillRect(0, 0, 1, 1, 0xff0000ff);
                binder.type = ltBinder;                   // must not be blitted

                var dest = new Layer();
                dest.setImageSize(2, 2);
                dest.piledCopy(0, 0, base, 0, 0, 2, 2);
                return dest.getMainPixel(0, 0) + ":" + dest.getMaskPixel(0, 0) + ":" +
                    dest.getMainPixel(1, 1) + ":" + dest.getMaskPixel(1, 1);
                "#,
            )
            .expect("script")
            .to_tjs_string()
            .expect("string");
        // `bmCopy` (`LayerIntf.cpp:5189-5196`) copies the child's planes
        // verbatim for an `ltOpaque` child over a non-alpha parent, so the
        // half-transparent red keeps its 0x80 alpha (the old `DF_ALPHA`
        // hard-coding kept the destination's opaque alpha instead), and the
        // `ltBinder` child contributes nothing.
        assert_eq!(value, "16711680:128:65280:255".to_string());
    }

    /// The pile is composited over *transparency* and then copied plane for
    /// plane into the destination
    /// (`MainImage->CopyRect(..., TVP_BB_COPY_MAIN|TVP_BB_COPY_MASK)`,
    /// `LayerIntf.cpp:4120-4122`), so a transparent pile pixel clears whatever
    /// the destination had there.
    #[test]
    fn native_layer_piled_copy_overwrites_the_destination() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.visible = true;
                source.setSize(2, 1);
                source.setImageSize(2, 1);
                source.fillRect(0, 0, 1, 1, 0xff112233);
                source.fillRect(1, 0, 1, 1, 0x00000000);   // fully transparent

                var dest = new Layer();
                dest.setImageSize(2, 1);
                dest.fillRect(0, 0, 2, 1, 0xff00ff00);
                dest.piledCopy(0, 0, source, 0, 0, 2, 1);
                return dest.getMainPixel(0, 0) + ":" + dest.getMaskPixel(0, 0) + ":" +
                    dest.getMainPixel(1, 0) + ":" + dest.getMaskPixel(1, 0);
                "#,
            )
            .expect("script")
            .to_tjs_string()
            .expect("string");
        assert_eq!(value, "1122867:255:0:0".to_string());
    }

    /// The source layer's own `Opacity` never reaches a `piledCopy` pile:
    /// `PiledCopy` copies the completed bitmap raw (`LayerIntf.cpp:4120-4122`)
    /// and `tCompleteDrawable::DrawCompleted` ignores the opacity it is handed
    /// (`:6119-6128`). A binder's opacity is never applied either — a binder
    /// draws no bitmap and forwards its children's own `type`/`opacity`
    /// (`:5848-5854`).
    #[test]
    fn native_layer_piled_copy_ignores_source_and_binder_opacity() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var base = new Layer();
                base.visible = true;
                base.setSize(2, 1);
                base.setImageSize(2, 1);
                base.fillRect(0, 0, 2, 1, 0xff000000);
                base.opacity = 128;                        // must not reach the child

                var child = new Layer(null, base);
                child.type = ltOpaque;
                child.visible = true;
                child.setPos(1, 0);
                child.setSize(1, 1);
                child.setImageSize(1, 1);
                child.fillRect(0, 0, 1, 1, 0xffff0000);

                var dest = new Layer();
                dest.setImageSize(2, 1);
                dest.piledCopy(0, 0, base, 0, 0, 2, 1);
                var first = dest.getMainPixel(0, 0) + ":" + dest.getMaskPixel(0, 0) + ":" +
                    dest.getMainPixel(1, 0) + ":" + dest.getMaskPixel(1, 0);

                // A binder's own opacity must not *scale* its children's blits
                // (every child is blitted with its own `Opacity`, so the
                // grandchild lands at full strength), but its `IsSeen()` gate
                // still applies: `Opacity == 0` hides the whole subtree
                // (`LayerIntf.h:304`, `LayerIntf.cpp:5537`/`:5600`).
                var root2 = new Layer();
                root2.visible = true;
                root2.setSize(2, 1);
                root2.setImageSize(2, 1);
                root2.fillRect(0, 0, 2, 1, 0xff000000);

                var binder = new Layer(null, root2);
                binder.visible = true;
                binder.setPos(1, 0);
                binder.setSize(1, 1);
                binder.opacity = 128;
                binder.type = ltBinder;

                var sub = new Layer(null, binder);
                sub.type = ltOpaque;
                sub.visible = true;
                sub.setSize(1, 1);
                sub.setImageSize(1, 1);
                sub.fillRect(0, 0, 1, 1, 0xff00ff00);

                var second = new Layer();
                second.setImageSize(2, 1);
                second.piledCopy(0, 0, root2, 0, 0, 2, 1);
                var scaled = second.getMainPixel(0, 0) + ":" + second.getMaskPixel(0, 0) + ":" +
                    second.getMainPixel(1, 0) + ":" + second.getMaskPixel(1, 0);

                binder.opacity = 0;
                var third = new Layer();
                third.setImageSize(2, 1);
                third.piledCopy(0, 0, root2, 0, 0, 2, 1);
                var hidden = third.getMainPixel(0, 0) + ":" + third.getMaskPixel(0, 0) + ":" +
                    third.getMainPixel(1, 0) + ":" + third.getMaskPixel(1, 0);

                return first + "/" + scaled + "/" + hidden;
                "#,
            )
            .expect("script")
            .to_tjs_string()
            .expect("string");
        // The base's own opaque black is copied raw and the `ltOpaque` child is
        // copied verbatim on top — not faded to 128, which is what folding the
        // source's opacity into the child's blit would do. The binder at 128
        // leaves its grandchild at full green (its opacity is a gate, not a
        // scale factor), while at 0 the same gate hides the subtree and the
        // pixel keeps the root's black.
        assert_eq!(
            value,
            "0:255:16711680:255/0:255:65280:255/0:255:0:255".to_string()
        );
    }

    /// The filtered stretch path uses the reference resampler's kernel: the tap
    /// window is `left = floor(cx - range)` … `floor(cx + range) - 1`
    /// (`visual/gl/ResampleImage.cpp:301-317`), shrinking widens it by the
    /// ratio and scales the distances (`:328`, `:357`), and the channels are
    /// truncated rather than rounded (`:428-431`).
    #[test]
    fn native_layer_stretch_copy_uses_the_reference_resampler() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(4, 1);
                source.fillRect(0, 0, 2, 1, 0xffff0000);
                source.fillRect(2, 0, 2, 1, 0xff0000ff);

                global.dest = new Layer();
                dest.setImageSize(2, 1);
                dest.stretchCopy(0, 0, 2, 1, source, 0, 0, 4, 1, stLinear);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        let image = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("layer image");
        // Destination 0 samples taps -1, 0 and 1 of a 2x-wide window:
        // 0.25 at tap -1 (folded onto tap 0), 0.75 at tap 0 and 0.75 at tap 1,
        // normalized to [0.5, 0.375, 0.125]; the channel values truncate, so
        // the red 0.875 * 255 = 223 and the blue 0.125 * 255 = 31. Destination 1
        // samples taps 1..3, i.e. [0.125, 0.375, 0.5] of red, blue, blue.
        assert_eq!(&image.upload.rgba[..4], &[223, 0, 31, 255]);
        assert_eq!(&image.upload.rgba[4..8], &[31, 0, 223, 255]);
    }

    /// `ClipDestPointAndSrcRect` (`LayerIntf.cpp:3762-3779`) moves the clipped
    /// destination point *and* trims the source rectangle by the same amount, so
    /// a `piledCopy` whose destination starts outside `ClipRect` keeps its
    /// alignment.
    #[test]
    fn native_layer_piled_copy_clips_the_source_with_the_destination_point() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.visible = true;
                source.setSize(2, 1);
                source.setImageSize(2, 1);
                source.fillRect(0, 0, 1, 1, 0xffff0000);
                source.fillRect(1, 0, 1, 1, 0xff0000ff);

                var dest = new Layer();
                dest.setImageSize(2, 1);
                dest.setClip(0, 0, 2, 1);
                dest.piledCopy(-1, 0, source, 0, 0, 2, 1);
                return dest.getMainPixel(0, 0) + ":" + dest.getMaskPixel(0, 0) + ":" +
                    dest.getMainPixel(1, 0) + ":" + dest.getMaskPixel(1, 0);
                "#,
            )
            .expect("script")
            .to_tjs_string()
            .expect("string");
        // Destination pixel 0 is where the source rectangle's pixel 1 lands, so
        // it receives the blue pixel; pixel 1 stays outside the copied width.
        assert_eq!(value, "255:255:16777215:0".to_string());
    }

    /// The reference resampler filters in two passes and quantizes the vertical
    /// one to 8 bits before the horizontal pass (`visual/gl/ResampleImage.cpp:
    /// 407-436` `samplingVertical`, its cast at `:428-431`; `:439-466`
    /// `samplingHorizontal`; the pair driven at `:610-653`), so a 2-D filtered
    /// stretch differs from a single f64 pass by up to 1 per channel. This case
    /// is built so the two disagree: a 2x2 source whose red values alternate
    /// 0/2 stretched to 1x4 has its third destination row's vertical averages
    /// at 1.5 and 0.5, which truncate to 1 and 0 (two-pass: 0.5 -> 0) instead
    /// of adding up to 1.
    #[test]
    fn native_layer_stretch_copy_truncates_the_vertical_pass() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(2, 2);
                source.fillRect(0, 0, 2, 2, 0xff000000);   // opaque black
                source.setMainPixel(0, 0, 0x000000);       // RGB only, alpha kept
                source.setMainPixel(0, 1, 0x020000);
                source.setMainPixel(1, 0, 0x020000);
                source.setMainPixel(1, 1, 0x000000);

                global.dest = new Layer();
                dest.setImageSize(1, 4);
                dest.stretchCopy(0, 0, 1, 4, source, 0, 0, 2, 2, stLinear);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        let image = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("layer image");
        let reds = (0..4)
            .map(|row| image.upload.rgba[row * 4])
            .collect::<Vec<_>>();
        assert_eq!(reds, vec![1, 1, 0, 1]);
    }

    /// `affineCopy`'s matrix form builds its points from the source rectangle's
    /// *corners* (`(-0.5,-0.5)`, `(rp-0.5,-0.5)`, `(-0.5,bp-0.5)`,
    /// `LayerBitmapIntf.cpp:3494-3513`), which `InternalAffineBlt` reads as
    /// `refrect.*.65536 - 32768` (`:2711-2718`); a destination pixel's sample is
    /// `floor(u * sw)` over that frame.
    #[test]
    fn native_layer_affine_copy_matrix_uses_the_source_corner_convention() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let read = |engine: &mut KrkrEngine, name: &str| -> Vec<u8> {
            let layer_id = engine
                .execute_expression("inline.tjs", &format!("{name}.__nativeLayerId"))
                .expect("layer id")
                .to_integer()
                .expect("integer layer id") as u64;
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .and_then(|layer| layer.image.as_ref())
                .expect("layer image")
                .upload
                .rgba
                .to_vec()
        };
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(2, 1);
                source.fillRect(0, 0, 1, 1, 0xffff0000);
                source.fillRect(1, 0, 1, 1, 0xff0000ff);

                // A half-pixel translation: only destination pixels 0 and 1
                // fall inside the transformed source rectangle.
                global.half = new Layer();
                half.setImageSize(4, 1);
                half.affineCopy(source, 0, 0, 2, 1, true, 1, 0, 0, 1, 0.5, 0, stNearest);
                "#,
            )
            .expect("script");
        let half = read(&mut engine, "half");
        assert_eq!(&half[..4], &[255, 0, 0, 255]);
        assert_eq!(&half[4..8], &[0, 0, 255, 255]);
        // `setImageSize` fills the rest with the layer's `neutralColor`
        // (`0x00ffffff` for a non-primary layer, `LayerIntf.cpp:635-644`).
        assert_eq!(&half[8..12], &[255, 255, 255, 0]);
        assert_eq!(&half[12..16], &[255, 255, 255, 0]);
    }

    #[test]
    fn update_dispatches_primary_pointer_release_to_top_native_layer() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var testLayer = new Layer();
                testLayer.clicked = "";
                testLayer.setPos(10, 20);
                testLayer.setSize(30, 40);
                testLayer.hitThreshold = 0;
                testLayer.visible = true;
                testLayer.onMouseUp = function(x, y, button, shift) {
                    this.clicked = "" + x + ":" + y + ":" + button + ":" + shift;
                };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 26.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click frame");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "testLayer.clicked")
                .expect("clicked"),
            Variant::String("5:6:0:0".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn primary_pointer_press_release_dispatches_layer_click() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                var layer = new Layer();
                layer.setPos(10, 20);
                layer.setSize(30, 40);
                layer.hitThreshold = 0;
                layer.visible = true;
                layer.onMouseDown = function(x, y, button, shift) { global.events += "down:" + x + ":" + y + ";"; };
                layer.onMouseUp = function(x, y, button, shift) { global.events += "up:" + x + ":" + y + ";"; };
                layer.onClick = function(x, y) { global.events += "click:" + x + ":" + y; };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 26.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "events")
                .expect("events"),
            Variant::String("down:5:6;up:5:6;click:5:6".to_string())
        );
    }

    #[test]
    fn native_layer_super_on_click_bubbles_to_window_action() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.action = "";
                var win = new Window();
                var root = new Layer(win, null);
                root.setSize(100, 100);
                root.setImageSize(100, 100);
                root.fillRect(0, 0, 100, 100, 0xffffffff);
                root.visible = true;
                class ActionButton extends Layer {
                    function ActionButton(win, parent) {
                        super.Layer(win, parent);
                    }
                    function onClick(x, y) {
                        super.onClick(...);
                    }
                }
                var button = new ActionButton(win, root);
                button.setPos(10, 10);
                button.setSize(30, 20);
                button.setImageSize(30, 20);
                button.fillRect(0, 0, 30, 20, 0xffffffff);
                button.visible = true;
                win.action = function(ev) {
                    global.action = ev.type + ":" + (ev.target === button);
                };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 15.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click frame");

        assert_eq!(
            engine.tjs_runtime().global_member("action"),
            Variant::String("onClick:1".to_string())
        );
    }

    #[test]
    fn primary_pointer_press_release_dispatches_click_only_layer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                global.kag = new Dictionary();
                kag.clicks = 0;
                kag.onPrimaryClick = function() { this.clicks++; };
                var layer = new Layer();
                layer.setPos(10, 20);
                layer.setSize(30, 40);
                layer.hitThreshold = 0;
                layer.visible = true;
                layer.onClick = function(x, y) { global.events += "click:" + x + ":" + y; };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 26.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "events")
                .expect("events"),
            Variant::String("click:5:6".to_string())
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "kag.clicks")
                .expect("clicks"),
            Variant::Integer(0)
        );
    }

    #[test]
    fn primary_pointer_press_fires_kag_primary_click_for_primary_layer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.kag = new Window();
                kag.clicks = 0;
                kag.onPrimaryClick = function() { this.clicks++; };
                var primary = new Layer(kag, null);
                primary.setSize(100, 100);
                primary.visible = true;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 26.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "kag.clicks")
                .expect("clicks"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn layer_focus_updates_focused_layer_and_events() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                var root = new Layer(window, null);
                var child = new Layer(window, root);
                root.focusable = true;
                child.focusable = true;
                child.visible = true;
                root.events = "";
                child.events = "";
                root.onBlur = function() { events += "blur"; };
                child.onFocus = function() { events += "focus"; };
                root.focus();
                child.focus();
                // `focusedLayer` is `tTJSVariant(dsp, dsp)`
                // (`WindowIntf.cpp:1824`); `Layer.focused` reports
                // `GetFocusedLayer() == this` (`LayerIntf.cpp:3218`).
                return (window.focusedLayer == child) + ":" + root.focused + ":" +
                    child.focused + ":" + root.events + ":" + child.events;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1:0:1:blur:focus".to_string()));
    }

    /// The focus events hand their layer arguments out as
    /// `tTJSVariant(Owner, Owner)`: the candidate and the previously focused
    /// layer for `onBeforeFocus` (`LayerIntf.cpp:3385-3401`), the newly focused
    /// layer for `onBlur` (`:3353-3365`), the old one for `onFocus`
    /// (`:3369-3381`).  The candidate a handler answers with is read back
    /// through its binding, so an override that calls
    /// `onSearchNextFocusable(layer)` (`SetFocusWork`, `LayerIntf.h:608-609`)
    /// moves the focus.
    #[test]
    fn focus_events_hand_out_self_bound_layers() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                var root = new Layer(window, null);
                var first = new Layer(window, root);
                var second = new Layer(window, root);
                root.visible = true;
                first.visible = true;
                second.visible = true;
                root.focusable = true;
                first.focusable = true;
                second.focusable = true;
                global.record = "";
                first.onBlur = function(focused) {
                    global.record += "/blur:" + (focused === second) + ":" +
                        (root.children.find(focused) != -1);
                };
                second.onFocus = function(prev, direction) {
                    global.record += "/focus:" + (prev === first) + ":" + direction;
                };
                second.onBeforeFocus = function(layer, prev, direction) {
                    global.record += "/before:" + (layer === second) + ":" +
                        (prev === first) + ":" + direction;
                    layer.onSearchNextFocusable(second);
                };
                first.focus();
                second.focus();
                return global.record + "/focused:" + second.focused;
                "#,
            )
            .expect("script");

        assert_eq!(
            value,
            Variant::String("/before:1:1:1/blur:1:1/focus:1:1/focused:1".to_string())
        );
    }

    /// `NotifyPart` -> `RemoveTreeModalState` (`LayerManager.cpp:168`,
    /// `:894`): a dialog that leaves the tree must give up the layers it
    /// disabled, otherwise the screen stays unclickable with nothing on it.
    #[test]
    fn parting_a_modal_subtree_releases_its_modal_state() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.root = new Layer(window, null);
                global.page = new Layer(window, root);
                global.dialog = new Layer(window, page);
                global.lower = new Layer(window, root);
                page.visible = true;
                dialog.visible = true;
                lower.visible = true;
                dialog.setMode();
                "#,
            )
            .expect("script");

        let window = object_handle(&engine, "window");
        let lower = object_handle(&engine, "lower");
        let lower_id = engine.host().native_layer(lower).expect("lower layer");
        assert!(engine.host().current_modal_layer(Some(window)).is_some());
        assert!(engine.host().layer_tree().is_disabled_by_mode(lower_id));

        engine
            .execute_script("inline.tjs", "page.parent = null;")
            .expect("part");

        assert_eq!(engine.host().current_modal_layer(Some(window)), None);
        assert!(!engine.host().layer_tree().is_disabled_by_mode(lower_id));
    }

    /// `NotifyPart` -> `LeaveMouseFromTree`: the layer under the cursor gets
    /// its `onMouseLeave` when its page leaves the screen.
    #[test]
    fn parting_a_page_takes_the_mouse_off_its_children() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.root = new Layer(window, null);
                global.page = new Layer(window, root);
                global.button = new Layer(window, page);
                page.setSize(200, 200);
                page.setImageSize(200, 200);
                page.fillRect(0, 0, 200, 200, 0xffffffff);
                button.setPos(10, 10);
                button.setSize(50, 50);
                button.setImageSize(50, 50);
                button.fillRect(0, 0, 50, 50, 0xffffffff);
                page.hitType = htMask;
                button.hitType = htMask;
                page.hitThreshold = 0;
                button.hitThreshold = 0;
                page.visible = true;
                button.visible = true;
                global.leaves = 0;
                button.onMouseLeave = function() { global.leaves++; };
                "#,
            )
            .expect("script");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::CursorMoved {
                        position: Point::new(20.0, 20.0),
                    }],
                ),
                Duration::ZERO,
            )
            .expect("hover");
        assert!(engine.host().hovered_layer().is_some());

        engine
            .execute_script("inline.tjs", "page.parent = null;")
            .expect("part");

        assert_eq!(engine.host().hovered_layer(), None);
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "leaves")
                .expect("leaves"),
            Variant::Integer(1)
        );
    }

    /// `NotifyPart` -> `BlurTree` -> `GetNextFocusable` (`LayerManager.cpp:692`):
    /// focus moves to the next focusable layer rather than vanishing with the
    /// page that held it.
    #[test]
    fn parting_the_focused_layer_moves_focus_to_the_next_focusable() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                global.root = new Layer(window, null);
                global.page = new Layer(window, root);
                global.other = new Layer(window, root);
                root.focusable = false;
                page.focusable = true;
                other.focusable = true;
                page.visible = true;
                other.visible = true;
                root.events = "";
                page.events = "";
                other.events = "";
                global.blurs = 0;
                global.focuses = 0;
                page.onBlur = function() { global.blurs++; };
                other.onFocus = function() { global.focuses++; };
                page.focus();
                page.parent = null;
                return (window.focusedLayer == other) + ":" + global.blurs + ":" + global.focuses;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1:1:1".to_string()));
    }

    #[test]
    fn layer_hit_testing_uses_alpha_mask_and_province_transparency() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.hits = 0;
                global.layer = new Layer();
                layer.setSize(2, 1);
                layer.setImageSize(2, 1);
                layer.fillRect(1, 0, 1, 1, 0xffffffff);
                layer.visible = true;
                layer.hitType = htMask;
                layer.hitThreshold = 0;
                layer.onMouseUp = function(x, y, button, shift) { global.hits++; };
                "#,
            )
            .expect("script");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");

        for position in [Point::new(0.2, 0.5), Point::new(1.2, 0.5)] {
            engine
                .update(
                    EngineInput::new(
                        FrameInput::new(Size::new(320.0, 240.0), 0.0),
                        vec![
                            EngineEvent::CursorMoved { position },
                            EngineEvent::PointerInput {
                                button: PointerButton::Primary,
                                state: ButtonState::Released,
                            },
                        ],
                    ),
                    Duration::ZERO,
                )
                .expect("hit frame");
        }
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "hits")
                .expect("hits"),
            Variant::Integer(2)
        );

        engine
            .execute_script("inline.tjs", "layer.hitThreshold = 1;")
            .expect("threshold");
        for position in [Point::new(0.2, 0.5), Point::new(1.2, 0.5)] {
            engine
                .update(
                    EngineInput::new(
                        FrameInput::new(Size::new(320.0, 240.0), 0.0),
                        vec![
                            EngineEvent::CursorMoved { position },
                            EngineEvent::PointerInput {
                                button: PointerButton::Primary,
                                state: ButtonState::Released,
                            },
                        ],
                    ),
                    Duration::ZERO,
                )
                .expect("threshold frame");
        }
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "hits")
                .expect("hits"),
            Variant::Integer(3)
        );

        engine
            .execute_script("inline.tjs", "layer.hitThreshold = 256;")
            .expect("threshold 256");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(1.2, 0.5),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("threshold 256 frame");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "hits")
                .expect("hits"),
            Variant::Integer(3)
        );

        engine
            .execute_script(
                "inline.tjs",
                "layer.freeImage(); layer.hitType = htMask; layer.hitThreshold = 1;",
            )
            .expect("no image threshold");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(1.2, 0.5),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("no image threshold frame");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "hits")
                .expect("hits"),
            Variant::Integer(3)
        );

        engine
            .execute_script("inline.tjs", "layer.hitThreshold = 0;")
            .expect("no image zero threshold");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(1.2, 0.5),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("no image zero threshold frame");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "hits")
                .expect("hits"),
            Variant::Integer(4)
        );

        engine
            .execute_script("inline.tjs", "layer.hitType = htProvince;")
            .expect("province");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(1.2, 0.5),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("province frame");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "hits")
                .expect("hits"),
            Variant::Integer(4)
        );
    }

    #[test]
    fn layer_province_plane_loads_pixels_and_drives_province_hit_testing() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        let mut samples = vec![0u8; 32 * 32];
        samples[32 + 2] = 7;
        write_province_png(root.join("province.png"), 32, 32, &samples);
        let mut indices = vec![0u8; 32 * 32];
        indices[32 + 3] = 200;
        write_province_palette_png(root.join("palette.png"), 32, 32, &indices);
        write_province_png(root.join("small.png"), 16, 16, &vec![0u8; 16 * 16]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.hits = 0;
                global.layer = new Layer();
                layer.setSize(32, 32);
                layer.setImageSize(32, 32);
                layer.loadProvinceImage("province.png");
                global.gray = layer.getProvincePixel(2, 1);
                global.outside = layer.getProvincePixel(100, 100);
                layer.loadProvinceImage("palette.png");
                global.index = layer.getProvincePixel(3, 1);
                layer.loadProvinceImage("province.png");
                layer.setProvincePixel(5, 5, 9);
                global.set_pixel = layer.getProvincePixel(5, 5);
                layer.hitType = htProvince;
                layer.visible = true;
                layer.onMouseUp = function(x, y, button, shift) { global.hits++; };

                // A province-face fill writes the colour's low byte
                // (`LayerIntf.cpp:3883`), ignoring the opacity argument.
                global.rect = new Layer();
                rect.setSize(32, 32);
                rect.setImageSize(32, 32);
                rect.face = dfProvince;
                rect.colorRect(1, 1, 2, 2, 0x2a);
                global.rect_pixel = rect.getProvincePixel(1, 1);
                global.rect_zero = rect.getProvincePixel(10, 10);
                rect.independProvinceImage();
                global.after_independ = rect.getProvincePixel(1, 1);
                // Filling the whole plane with 0 deallocates it.
                rect.colorRect(0, 0, 32, 32, 0);
                global.cleared = rect.getProvincePixel(1, 1);
                "#,
            )
            .expect("script");
        for (name, expected) in [
            ("gray", 7),
            ("outside", 0),
            ("index", 200),
            ("set_pixel", 9),
            ("rect_pixel", 0x2a),
            ("rect_zero", 0),
            ("after_independ", 0x2a),
            ("cleared", 0),
        ] {
            assert_eq!(
                engine
                    .execute_expression("inline.tjs", name)
                    .expect("expression"),
                Variant::Integer(expected),
                "{name}"
            );
        }

        // Only the province pixel at (2, 1) is non-zero.
        for position in [Point::new(2.0, 1.0), Point::new(6.0, 6.0)] {
            engine
                .update(
                    EngineInput::new(
                        FrameInput::new(Size::new(320.0, 240.0), 0.0),
                        vec![
                            EngineEvent::CursorMoved { position },
                            EngineEvent::PointerInput {
                                button: PointerButton::Primary,
                                state: ButtonState::Released,
                            },
                        ],
                    ),
                    Duration::ZERO,
                )
                .expect("province hit frame");
        }
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "hits")
                .expect("hits"),
            Variant::Integer(1)
        );

        // `TVPProvinceSizeMismatch`: the plane must match the main image.
        let error = engine
            .execute_script("inline.tjs", "layer.loadProvinceImage(\"small.png\");")
            .expect_err("size mismatch");
        assert!(
            error
                .to_string()
                .contains("Province image small.png size mismatch"),
            "{error}"
        );
        // The failed load deallocates the plane (`LayerIntf.cpp:2578`).
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "layer.getProvincePixel(2, 1)")
                .expect("expression"),
            Variant::Integer(0)
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn captured_layer_receives_drag_move_until_release() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.moves = "";
                var layer = new Layer();
                layer.setPos(10, 10);
                layer.setSize(10, 10);
                layer.hitThreshold = 0;
                layer.visible = true;
                layer.onMouseMove = function(x, y, shift) { global.moves = "" + x + ":" + y; };
                layer.onMouseUp = function(x, y, button, shift) { global.moves += ":up:" + x + ":" + y; };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 15.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                        EngineEvent::CursorMoved {
                            position: Point::new(50.0, 52.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("drag frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "moves")
                .expect("moves"),
            Variant::String("40:42:up:40:42".to_string())
        );
    }

    #[test]
    fn layer_cursor_position_tracks_mouse_in_layer_coordinates() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var parent = new Layer();
                parent.setPos(40, 30);
                parent.setSize(200, 200);
                parent.visible = true;
                var child = new Layer(void, parent);
                child.setPos(10, 5);
                child.setSize(50, 50);
                child.visible = true;
                global.parent = parent;
                global.child = child;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::CursorMoved {
                        position: Point::new(57.0, 48.0),
                    }],
                ),
                Duration::ZERO,
            )
            .expect("cursor frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "child.cursorX + \":\" + child.cursorY")
                .expect("child cursor"),
            Variant::String("7:13".to_string())
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "parent.cursorX + \":\" + parent.cursorY")
                .expect("parent cursor"),
            Variant::String("17:18".to_string())
        );
    }

    #[test]
    fn slider_with_non_hittable_pin_receives_drag_move() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                var root = new Layer();
                root.setSize(200, 40);
                root.visible = true;

                var slider = new Layer(void, root);
                slider.setSize(100, 20);
                slider.setImageSize(100, 20);
                slider.fillRect(0, 0, 100, 20, 0);
                slider.hitThreshold = 0;
                slider.visible = true;
                slider.dragging = false;
                slider.onMouseDown = function(x, y, button, shift) {
                    this.dragging = true;
                    global.events += "down:" + x + ":" + y + ";";
                };
                slider.onMouseMove = function(x, y, shift) {
                    if (this.dragging) global.events += "move:" + x + ":" + y + ";";
                };
                slider.onMouseUp = function(x, y, button, shift) {
                    this.dragging = false;
                    global.events += "up:" + x + ":" + y;
                };

                var pin = new Layer(void, root);
                pin.setPos(10, 0);
                pin.setSize(24, 20);
                pin.setImageSize(24, 20);
                pin.fillRect(0, 0, 24, 20, 0xffffffff);
                pin.hitType = htMask;
                pin.hitThreshold = 256;
                pin.visible = true;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(12.0, 10.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                        EngineEvent::CursorMoved {
                            position: Point::new(50.0, 10.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("drag frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "events")
                .expect("events"),
            Variant::String("down:12:10;move:50:10;up:50:10".to_string())
        );
    }

    #[test]
    fn invalidating_slider_finalizes_sibling_tab_layer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var root = new Layer();
                root.setSize(200, 40);
                root.visible = true;

                class SliderWithTab extends Layer {
                    var SliderTab;
                    function SliderWithTab(parent) {
                        super.Layer(void, parent);
                    }
                    function finalize() {
                        invalidate SliderTab if SliderTab !== void;
                        super.finalize(...);
                    }
                    function createTab() {
                        SliderTab = new Layer(void, parent);
                        SliderTab.setPos(20, 0);
                        SliderTab.setSize(10, 20);
                        SliderTab.setImageSize(10, 20);
                        SliderTab.fillRect(0, 0, 10, 20, 0xffffffff);
                        SliderTab.visible = true;
                    }
                }

                global.slider = new SliderWithTab(root);
                slider.setSize(100, 20);
                slider.visible = true;
                slider.createTab();
                global.sliderLayerId = slider.__nativeLayerId;
                global.tabLayerId = slider.SliderTab.__nativeLayerId;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");

        let slider_layer_id = engine
            .execute_expression("inline.tjs", "sliderLayerId")
            .expect("slider id")
            .to_integer()
            .expect("slider id integer") as u64;
        let tab_layer_id = engine
            .execute_expression("inline.tjs", "tabLayerId")
            .expect("tab id")
            .to_integer()
            .expect("tab id integer") as u64;

        assert!(
            engine
                .tjs_runtime
                .host()
                .layer_tree()
                .layer(slider_layer_id)
                .is_some()
        );
        assert!(
            engine
                .tjs_runtime
                .host()
                .layer_tree()
                .layer(tab_layer_id)
                .is_some()
        );

        engine
            .execute_script("cleanup.tjs", "invalidate slider;")
            .expect("invalidate");

        assert!(
            engine
                .tjs_runtime
                .host()
                .layer_tree()
                .layer(slider_layer_id)
                .is_none()
        );
        assert!(
            engine
                .tjs_runtime
                .host()
                .layer_tree()
                .layer(tab_layer_id)
                .is_none()
        );
    }

    #[test]
    fn transparent_native_overlay_does_not_block_script_mouse_handler() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                var root = new Layer();
                root.setSize(100, 100);
                root.visible = true;

                var control = new Layer(void, root);
                control.setPos(10, 10);
                control.setSize(20, 20);
                control.hitThreshold = 0;
                control.visible = true;
                control.onMouseDown = function(x, y, button, shift) {
                    global.events += "down:" + x + ":" + y + ";";
                };
                control.onMouseUp = function(x, y, button, shift) {
                    global.events += "up:" + x + ":" + y;
                };

                var overlay = new Layer(void, root);
                overlay.setSize(100, 100);
                overlay.setImageSize(100, 100);
                overlay.fillRect(0, 0, 100, 100, 0);
                overlay.visible = true;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 15.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "events")
                .expect("events"),
            Variant::String("down:5:5;up:5:5".to_string())
        );
    }

    #[test]
    fn layer_absolute_property_updates_render_order() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                // `SetAbsoluteOrderIndex` needs a parent
                // (`TVPCannotMovePrimaryOrSiblingless`, `LayerIntf.cpp:1263`)
                // and switches that parent to absolute order mode.
                global.parent = new Layer();
                global.layer = new Layer(null, parent);
                layer.absolute = 2000000;
                "#,
            )
            .expect("script");

        let layer_id = engine
            .execute_expression("inline.tjs", "layer.__nativeLayerId")
            .expect("layer id")
            .to_integer()
            .expect("layer id integer") as u64;
        let z_order = engine
            .tjs_runtime
            .host()
            .layer_tree()
            .layer(layer_id)
            .expect("layer node")
            .z_order;

        assert_eq!(z_order, 2_000_000);
    }

    #[test]
    fn message_layer_overlay_passes_pointer_to_lower_control_layer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                var root = new Layer();
                root.setSize(100, 100);
                root.visible = true;

                class SliderLayer extends Layer {
                    function SliderLayer(parent) { super.Layer(void, parent); }
                    function onMouseDown(x, y, button, shift) {
                        global.events += "slider:" + x + ":" + y;
                    }
                }
                class MessageLayer extends Layer {
                    function MessageLayer(parent) { super.Layer(void, parent); }
                    function onHitTest(x, y, hit) {
                        return super.onHitTest(x, y, false);
                    }
                    function onMouseDown(x, y, button, shift) {
                        global.events += "message";
                    }
                }

                var slider = new SliderLayer(root);
                slider.setPos(10, 10);
                slider.setSize(20, 20);
                slider.setImageSize(20, 20);
                slider.fillRect(0, 0, 20, 20, 0);
                slider.hitThreshold = 0;
                slider.visible = true;

                var message = new MessageLayer(root);
                message.setSize(100, 100);
                message.visible = true;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 15.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "events")
                .expect("events"),
            Variant::String("slider:5:5".to_string())
        );
    }

    #[test]
    fn focused_layer_receives_key_down_up_and_can_click() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                var window = new Window();
                var root = new Layer(window, null);
                var button = new Layer(window, root);
                button.setSize(20, 10);
                button.visible = true;
                button.focusable = true;
                button.pressed = false;
                button.onKeyDown = function(key, shift, process) {
                    if(process && key == VK_RETURN) {
                        this.pressed = true;
                        global.events += "down:" + shift + ":" + process + ";";
                    }
                };
                button.onKeyUp = function(key, shift, process) {
                    if(process && key == VK_RETURN) {
                        var pressed = this.pressed;
                        this.pressed = false;
                        if(pressed) this.onClick(this.width \ 2, this.height \ 2);
                    }
                };
                button.onClick = function(x, y) {
                    global.events += "click:" + x + ":" + y;
                };
                button.focus();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::KeyboardInput {
                            key: EngineKey::Enter,
                            state: ButtonState::Pressed,
                            repeat: false,
                        },
                        EngineEvent::KeyboardInput {
                            key: EngineKey::Enter,
                            state: ButtonState::Released,
                            repeat: false,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("key frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "events")
                .expect("events"),
            Variant::String("down:0:1;click:10:5".to_string())
        );
    }

    #[test]
    fn keyboard_primary_click_still_fires_without_script_layer_handler() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                var root = new Layer(window, null);
                global.kag = new Dictionary();
                kag.clicks = 0;
                kag.onPrimaryClickByKey = function() { this.clicks += 10; };
                kag.onPrimaryClick = function() { this.clicks += 1; };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::KeyboardInput {
                        key: EngineKey::Enter,
                        state: ButtonState::Pressed,
                        repeat: false,
                    }],
                ),
                Duration::ZERO,
            )
            .expect("key frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "kag.clicks")
                .expect("clicks"),
            Variant::Integer(10)
        );
    }

    #[test]
    fn primary_layer_does_not_block_window_keyboard_shortcuts() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                global.kag = new Window();
                kag.innerWidth = 320;
                var root = new Layer(kag, null);
                kag.add(root);
                kag.onKeyDown = function(key, shift) {
                    if(focusedLayer === null && key == VK_CONTROL) {
                        global.events += "ctrl:" + System.getKeyState(VK_CONTROL);
                    }
                };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::KeyboardInput {
                        key: EngineKey::Control,
                        state: ButtonState::Pressed,
                        repeat: false,
                    }],
                ),
                Duration::ZERO,
            )
            .expect("key frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "events")
                .expect("events"),
            Variant::String("ctrl:1".to_string())
        );
    }

    #[test]
    fn native_layer_key_default_moves_focus_to_next_layer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.window = new Window();
                var root = new Layer(window, null);
                global.first = new Layer(window, root);
                global.second = new Layer(window, root);
                first.visible = true;
                second.visible = true;
                first.focusable = true;
                second.focusable = true;
                first.focus();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::KeyboardInput {
                        key: EngineKey::Right,
                        state: ButtonState::Pressed,
                        repeat: false,
                    }],
                ),
                Duration::ZERO,
            )
            .expect("key frame");

        assert_eq!(
            engine
                .execute_script(
                    "inline.tjs",
                    "return (window.focusedLayer == second) + ':' + first.focused + ':' + second.focused;"
                )
                .expect("focused"),
            Variant::String("1:0:1".to_string())
        );
    }

    #[test]
    fn system_get_key_state_tracks_runtime_keyboard_events() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.states = "";
                var window = new Window();
                var root = new Layer(window, null);
                var layer = new Layer(window, root);
                layer.visible = true;
                layer.focusable = true;
                layer.onKeyDown = function(key, shift, process) {
                    global.states += System.getKeyState(key);
                };
                layer.onKeyUp = function(key, shift, process) {
                    global.states += ":" + System.getKeyState(key);
                };
                layer.focus();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::KeyboardInput {
                            key: EngineKey::Left,
                            state: ButtonState::Pressed,
                            repeat: false,
                        },
                        EngineEvent::KeyboardInput {
                            key: EngineKey::Left,
                            state: ButtonState::Released,
                            repeat: false,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("key frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "states")
                .expect("states"),
            Variant::String("1:0".to_string())
        );
    }

    #[test]
    fn keyboard_events_are_sent_to_window_and_focused_layer_with_shift_flags() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.windowEvents = "";
                global.layerEvents = "";
                var window = new Window();
                window.onKeyDown = function(key, shift) {
                    global.windowEvents += "down:" + key + ":" + shift + ";";
                };
                window.onKeyUp = function(key, shift) {
                    global.windowEvents += "up:" + key + ":" + shift + ";";
                };
                var root = new Layer(window, null);
                var layer = new Layer(window, root);
                layer.visible = true;
                layer.focusable = true;
                layer.onKeyDown = function(key, shift, process) {
                    global.layerEvents += "down:" + key + ":" + shift + ":" + process + ";";
                };
                layer.onKeyUp = function(key, shift, process) {
                    global.layerEvents += "up:" + key + ":" + shift + ":" + process + ";";
                };
                layer.focus();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::KeyboardInput {
                            key: EngineKey::Control,
                            state: ButtonState::Pressed,
                            repeat: false,
                        },
                        EngineEvent::KeyboardInput {
                            key: EngineKey::Character('A'),
                            state: ButtonState::Pressed,
                            repeat: true,
                        },
                        EngineEvent::KeyboardInput {
                            key: EngineKey::Escape,
                            state: ButtonState::Pressed,
                            repeat: false,
                        },
                        EngineEvent::KeyboardInput {
                            key: EngineKey::Control,
                            state: ButtonState::Released,
                            repeat: false,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("key frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "windowEvents")
                .expect("window events"),
            Variant::String("down:17:4;down:65:132;down:27:4;up:17:0;".to_string())
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "layerEvents")
                .expect("layer events"),
            Variant::String("down:17:4:1;down:65:132:1;down:27:4:1;up:17:0:1;".to_string())
        );
    }

    #[test]
    fn keyboard_primary_click_fallback_does_not_repeat_window_handler() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.clicks = 0;
                global.kag = new Dictionary();
                kag.onPrimaryClickByKey = function() { global.clicks++; };

                var window = new Window();
                window.onKeyDown = function(key, shift) {
                    if(key == VK_RETURN) kag.onPrimaryClickByKey();
                };
                var root = new Layer(window, null);
                var layer = new Layer(window, root);
                layer.visible = true;
                layer.focusable = true;
                layer.focus();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::KeyboardInput {
                        key: EngineKey::Enter,
                        state: ButtonState::Pressed,
                        repeat: false,
                    }],
                ),
                Duration::ZERO,
            )
            .expect("key frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "clicks")
                .expect("clicks"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn escape_input_reports_unhandled_only_without_script_handler() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                var root = new Layer(window, null);
                var layer = new Layer(window, root);
                layer.visible = true;
                layer.focusable = true;
                layer.focus();
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::KeyboardInput {
                        key: EngineKey::Escape,
                        state: ButtonState::Pressed,
                        repeat: false,
                    }],
                ),
                Duration::ZERO,
            )
            .expect("escape frame");
        assert!(frame.input.unhandled_escape_pressed);

        engine
            .execute_script(
                "escape_handler.tjs",
                "window.onKeyDown = function(key, shift) { global.escapeHandled = key == VK_ESCAPE; };",
            )
            .expect("handler");
        let frame = engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::KeyboardInput {
                        key: EngineKey::Escape,
                        state: ButtonState::Pressed,
                        repeat: false,
                    }],
                ),
                Duration::ZERO,
            )
            .expect("handled escape frame");
        assert!(!frame.input.unhandled_escape_pressed);
        assert_eq!(
            engine
                .execute_expression("escape_handler.tjs", "escapeHandled")
                .expect("escape handled"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn mouse_wheel_events_are_sent_to_window_with_position_delta_and_shift() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.wheel = "";
                var window = new Window();
                window.onMouseWheel = function(shift, delta, x, y) {
                    global.wheel += "" + shift + ":" + delta + ":" + x + ":" + y;
                };
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::KeyboardInput {
                            key: EngineKey::Shift,
                            state: ButtonState::Pressed,
                            repeat: false,
                        },
                        EngineEvent::CursorMoved {
                            position: Point::new(12.4, 34.6),
                        },
                        EngineEvent::MouseWheel { delta: 120 },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("wheel frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "wheel")
                .expect("wheel"),
            Variant::String("1:120:12:35".to_string())
        );
    }

    #[test]
    fn primary_pointer_press_fires_kag_primary_click_for_non_link_layer() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var kag = new Dictionary();
                kag.clicks = 0;
                kag.onPrimaryClick = function() {
                    this.clicks++;
                };
                var rootLayer = new Layer();
                rootLayer.setSize(100, 100);
                rootLayer.visible = true;
                var childLayer = new Layer(void, rootLayer);
                childLayer.setPos(10, 20);
                childLayer.setSize(30, 40);
                childLayer.visible = true;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 26.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "kag.clicks")
                .expect("clicks"),
            Variant::Integer(1)
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn layer_subclass_mouse_handler_is_not_shadowed_by_native_stub() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class ProbeLayer extends Layer {
                    function ProbeLayer() {
                        super.Layer(...);
                    }
                    function onMouseUp(x, y, button, shift) {
                        clicked = "" + x + ":" + y + ":" + button + ":" + shift;
                    }
                }
                var testLayer = new ProbeLayer();
                testLayer.clicked = "";
                testLayer.setPos(10, 20);
                testLayer.setSize(30, 40);
                testLayer.hitThreshold = 0;
                testLayer.visible = true;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(15.0, 26.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("click frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "testLayer.clicked")
                .expect("clicked"),
            Variant::String("5:6:0:0".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn layer_update_defers_on_paint_until_after_mouse_handler_returns() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class ProbeLayer extends Layer {
                    var hot = false;
                    function ProbeLayer() {
                        super.Layer(...);
                        setPos(10, 20);
                        setSize(2, 4);
                        setImageSize(6, 4);
                        fillRect(0, 0, 6, 4, 0xffffffff);
                        visible = true;
                    }
                    function onMouseEnter() {
                        update();
                        hot = true;
                    }
                    function onPaint() {
                        imageLeft = hot ? -4 : 0;
                    }
                }
                global.testLayer = new ProbeLayer();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("sync frame");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::CursorMoved {
                        position: Point::new(11.0, 22.0),
                    }],
                ),
                Duration::ZERO,
            )
            .expect("hover frame");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "testLayer.imageLeft")
                .expect("imageLeft"),
            Variant::Integer(-4)
        );
    }

    #[test]
    fn wave_sound_buffer_methods_are_available_on_global_class_object() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                WaveSoundBuffer.fade(0, 100, 0);
                WaveSoundBuffer.stopFade();
                WaveSoundBuffer.stop();
                "#,
            )
            .expect("script");
    }

    #[test]
    fn wave_sound_buffer_get_sample_plugin_compat_members_exist() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                WaveSoundBuffer.setDefaultCounts(256);
                WaveSoundBuffer.setDefaultAheads(512);
                var buffer = new WaveSoundBuffer();
                buffer.sampleCount = 128;
                return buffer.sampleValue + ":" + buffer.sampleCount + ":" + buffer.sampleAhead;
                "#,
            )
            .expect("script");
        // `sampleValue` is a real in the official getSample plugin, and
        // official TJS2 renders positive zero as `+0.0`.
        assert_eq!(result, Variant::String("+0.0:128:512".to_string()));
    }

    #[test]
    fn wave_sound_buffer_exposes_mutable_loop_flags() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var buffer = new WaveSoundBuffer();
                buffer.flags[0] = buffer.looping ? 0 : 1;
                return buffer.flags[0];
                "#,
            )
            .expect("script");
        assert_eq!(result, Variant::Integer(1));
    }

    #[test]
    fn wave_sound_buffer_queues_audio_commands() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("sound.wav"), b"not real audio").expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var buffer = new WaveSoundBuffer();
                buffer.open("sound.wav");
                buffer.looping = 1;
                buffer.play();
                buffer.fade(50000, 250);
                buffer.stop();
                "#,
            )
            .expect("script");

        let commands = engine.host_mut().take_audio_commands();
        assert_eq!(commands.len(), 4);
        match &commands[0] {
            AudioCommand::Preload {
                source,
                load_policy,
            } => {
                assert_eq!(source.storage(), "sound.wav");
                assert_eq!(*load_policy, AudioLoadPolicy::Auto);
            }
            command => panic!("expected preload command, got {command:?}"),
        }
        match &commands[1] {
            AudioCommand::Play {
                bus,
                source,
                load_policy,
                looping,
                volume,
                ..
            } => {
                assert_eq!(*bus, AudioBus::Bgm);
                assert_eq!(*load_policy, AudioLoadPolicy::Auto);
                assert_eq!(source.storage(), "sound.wav");
                assert!(*looping);
                assert_eq!(*volume, 1.0);
            }
            command => panic!("expected play command, got {command:?}"),
        }
        assert!(matches!(
            commands[2],
            AudioCommand::SetVolume {
                volume: 0.5,
                fade_seconds: 0.25,
                ..
            }
        ));
        assert!(matches!(commands[3], AudioCommand::Stop { .. }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn wave_sound_buffer_completion_restarts_script_conductor_wait() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("voice.ogg"), b"voice bytes").expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.waitHandlerRan = 0;
                global.conductorResumed = 0;
                global.asyncProbe = new AsyncTrigger(function() {
                    global.conductorResumed++;
                }, "");

                // KRKR binds a class method to its instance while copying the
                // class members (`regmember` -> ChangeClosureObjThis, krkrz
                // tjsInterCodeExec.cpp:3033); a plain member assigned from an
                // expression function keeps no ObjThis, and a call on it runs
                // on `clo.ObjThis ? clo.ObjThis : ra[-1]` -- the *caller's*
                // this -- so the conductor and its owner are classes here,
                // exactly like KAG's Conductor and MainWindow.
                class TestConductor {
                    var status;
                    var waitUntil;

                    function TestConductor() {
                        this.status = 2;
                        this.waitUntil = new Dictionary();
                    }

                    function run() {
                        this.status = 1;
                        global.asyncProbe.trigger();
                    }

                    function trigger(name) {
                        if(this.status != 2) return false;
                        var func = this.waitUntil[name];
                        if(func === void) return false;
                        func();
                        this.waitUntil = new Dictionary();
                        this.run();
                        return true;
                    }
                }

                class TestOwner {
                    var conductor;

                    function TestOwner() {
                        this.conductor = new TestConductor();
                    }

                    function onSESoundBufferStop(id) {
                        this.conductor.trigger("sestop" + id);
                    }
                }

                global.owner = new TestOwner();
                owner.conductor.waitUntil["sestop7"] = function() {
                    global.waitHandlerRan++;
                };

                class SESoundBuffer extends WaveSoundBuffer
                {
                    var prevstatus = "unload";
                    var id = 7;
                    var owner;

                    function SESoundBuffer(owner)
                    {
                        super.WaveSoundBuffer();
                        this.owner = owner;
                    }

                    function onStatusChanged()
                    {
                        var ps = prevstatus;
                        var cs = status;
                        prevstatus = cs;
                        if(ps == "play" && cs == "stop")
                            owner.onSESoundBufferStop(id);
                    }
                }

                owner.conductor.waitUntil.sestop7 = function() {
                    global.waitHandlerRan++;
                };
                global.buffer = new SESoundBuffer(owner);
                buffer.open("voice.ogg");
                buffer.play();
                "#,
            )
            .expect("script");

        let commands = engine.host_mut().take_audio_commands();
        let play_id = match &commands[..] {
            [AudioCommand::Preload { .. }, AudioCommand::Play { id, .. }] => *id,
            commands => panic!("expected preload and play, got {commands:?}"),
        };
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "buffer.prevstatus")
                .expect("prevstatus"),
            Variant::String("play".to_string())
        );

        engine
            .notify_audio_stopped(play_id)
            .expect("audio completion");
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "buffer.status + ':' + buffer.prevstatus + ':' + waitHandlerRan",
                )
                .expect("wait handler"),
            Variant::String("stop:stop:1".to_string())
        );

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "conductorResumed")
                .expect("resumed"),
            Variant::Integer(1)
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn wave_sound_buffer_notifies_secondary_tjs_base_status_handler() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("voice.ogg"), b"voice bytes").expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                class KAGSoundBuffer {
                    function onStatusChanged() { this.statusChanges++; }
                }
                class KAGWaveSoundBuffer extends WaveSoundBuffer, KAGSoundBuffer {
                    function KAGWaveSoundBuffer() { super.WaveSoundBuffer(); }
                }
                global.buffer = new KAGWaveSoundBuffer();
                buffer.statusChanges = 0;
                buffer.open("voice.ogg");
                buffer.play();
                return buffer.statusChanges;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::Integer(1));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn audio_completion_resumes_native_wait_audio_fallback() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");

        let mut engine = image_test_engine(&root);
        engine.kag_session.state = KagTaskState::WaitingAudio;
        engine
            .notify_audio_stopped(AudioInstanceId(999))
            .expect("audio");

        assert_eq!(engine.kag_session.state, KagTaskState::Running);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn wave_sound_buffer_volume2_and_global_volume_affect_playback_volume() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("sound.wav"), b"not real audio").expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var buffer = new WaveSoundBuffer();
                buffer.open("sound.wav");
                buffer.volume = 50000;
                buffer.volume2 = 50000;
                WaveSoundBuffer.globalVolume = 50000;
                buffer.play();
                buffer.volume2 = 25000;
                return buffer.volume + ":" + buffer.volume2 + ":" + WaveSoundBuffer.globalVolume;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("50000:25000:50000".to_string()));
        let commands = engine.host_mut().take_audio_commands();
        match &commands[..] {
            [
                AudioCommand::Preload { .. },
                AudioCommand::Play { volume, .. },
                AudioCommand::SetVolume {
                    volume: updated, ..
                },
            ] => {
                assert_eq!(*volume, 0.125);
                assert_eq!(*updated, 0.0625);
            }
            commands => panic!("expected play then volume update, got {commands:?}"),
        }

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn wave_sound_buffer_subclass_direct_volume2_assignment_updates_audio() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("voice.ogg"), b"voice bytes").expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                class ConfigSoundBuffer extends WaveSoundBuffer
                {
                    function ConfigSoundBuffer()
                    {
                        super.WaveSoundBuffer();
                    }

                    function setConfigVolume(pos)
                    {
                        volume2 = pos * 1000;
                    }
                }

                var buffer = new ConfigSoundBuffer();
                buffer.open("voice.ogg");
                buffer.play();
                buffer.setConfigVolume(40);
                return buffer.volume2;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::Integer(40000));
        let commands = engine.host_mut().take_audio_commands();
        match &commands[..] {
            [
                AudioCommand::Preload { .. },
                AudioCommand::Play { volume, .. },
                AudioCommand::SetVolume {
                    volume: updated, ..
                },
            ] => {
                assert_eq!(*volume, 1.0);
                assert_eq!(*updated, 0.4);
            }
            commands => panic!("expected play then subclass volume2 update, got {commands:?}"),
        }

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn wave_sound_buffer_script_subclass_play_overrides_native_base_method() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("voice.ogg"), b"voice bytes").expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class SESoundBuffer extends WaveSoundBuffer
                {
                    function SESoundBuffer()
                    {
                        super.WaveSoundBuffer();
                    }

                    function play(elm)
                    {
                        super.open(elm.storage);
                        super.volume = 60000;
                        super.play();
                    }
                }

                var buffer = new SESoundBuffer();
                buffer.play(%[ storage: "voice.ogg" ]);
                buffer.fade(30000, 250);
                "#,
            )
            .expect("script");

        let commands = engine.host_mut().take_audio_commands();
        assert_eq!(commands.len(), 3);
        match &commands[0] {
            AudioCommand::Preload {
                source,
                load_policy,
            } => {
                assert_eq!(source.storage(), "voice.ogg");
                assert_eq!(*load_policy, AudioLoadPolicy::Auto);
            }
            command => panic!("expected preload command, got {command:?}"),
        }
        match &commands[1] {
            AudioCommand::Play {
                bus,
                source,
                load_policy,
                looping,
                volume,
                ..
            } => {
                assert_eq!(*bus, AudioBus::SoundEffect);
                assert_eq!(*load_policy, AudioLoadPolicy::Auto);
                assert_eq!(source.storage(), "voice.ogg");
                assert!(!*looping);
                assert_eq!(*volume, 0.6);
            }
            command => panic!("expected play command, got {command:?}"),
        }
        assert!(matches!(
            commands[2],
            AudioCommand::SetVolume {
                volume: 0.3,
                fade_seconds: 0.25,
                ..
            }
        ));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn se_sound_buffer_play_uses_script_volume_scale() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("voice.ogg"), b"voice bytes").expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class SESoundBuffer extends WaveSoundBuffer
                {
                    var currentVolume = 100;

                    function SESoundBuffer()
                    {
                        super.WaveSoundBuffer();
                    }

                    function play(elm)
                    {
                        super.open(elm.storage);
                        super.volume = currentVolume * 1000;
                        super.play();
                    }

                    property volume
                    {
                        setter(x)
                        {
                            currentVolume = x;
                            super.volume = x * 1000;
                        }
                        getter
                        {
                            return super.volume \ 1000;
                        }
                    }
                }

                var buffer = new SESoundBuffer();
                buffer.volume = 100;
                buffer.play(%[ storage: "voice.ogg" ]);
                "#,
            )
            .expect("script");

        let commands = engine.host_mut().take_audio_commands();
        match &commands[..] {
            [
                AudioCommand::Preload { .. },
                AudioCommand::Play { source, volume, .. },
            ] => {
                assert_eq!(source.storage(), "voice.ogg");
                assert_eq!(*volume, 1.0);
            }
            commands => panic!("expected one play command, got {commands:?}"),
        }

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn wave_sound_buffer_class_object_fade_uses_current_instance() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("music.ogg"), b"music bytes").expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class KAGSoundBuffer
                {
                    var sbclass;
                    function KAGSoundBuffer(sbclass)
                    {
                        this.sbclass = sbclass;
                    }
                    function fadeOutAndStop(time)
                    {
                        sbclass.fade(0, time, 0);
                    }
                }

                class KAGWaveSoundBuffer extends WaveSoundBuffer, KAGSoundBuffer
                {
                    function KAGWaveSoundBuffer()
                    {
                        super.WaveSoundBuffer();
                        KAGSoundBuffer(global.WaveSoundBuffer);
                    }
                }

                var buffer = new KAGWaveSoundBuffer();
                buffer.open("music.ogg");
                buffer.looping = 1;
                buffer.play();
                buffer.fadeOutAndStop(1000);
                "#,
            )
            .expect("script");

        let commands = engine.host_mut().take_audio_commands();
        match &commands[..] {
            [
                AudioCommand::Preload { .. },
                AudioCommand::Play { id: play_id, .. },
                AudioCommand::SetVolume {
                    id,
                    volume,
                    fade_seconds,
                },
            ] => {
                assert_eq!(id, play_id);
                assert_eq!(*volume, 0.0);
                assert_eq!(*fade_seconds, 1.0);
            }
            commands => panic!("expected play then fade command, got {commands:?}"),
        }

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn wave_sound_buffer_fade_completion_runs_script_callback() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("music.ogg"), b"music bytes").expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                class KAGSoundBuffer
                {
                    var sbclass;
                    var inFadeAndStop = false;

                    function KAGSoundBuffer(sbclass)
                    {
                        this.sbclass = sbclass;
                    }

                    function fadeOutAndStop(time)
                    {
                        inFadeAndStop = true;
                        sbclass.fade(0, time, 0);
                    }

                    function onFadeCompleted()
                    {
                        if(inFadeAndStop)
                        {
                            sbclass.stop();
                            inFadeAndStop = false;
                        }
                    }
                }

                class KAGWaveSoundBuffer extends WaveSoundBuffer, KAGSoundBuffer
                {
                    function KAGWaveSoundBuffer()
                    {
                        super.WaveSoundBuffer();
                        KAGSoundBuffer(global.WaveSoundBuffer);
                    }
                }

                var buffer = new KAGWaveSoundBuffer();
                buffer.open("music.ogg");
                buffer.looping = 1;
                buffer.play();
                buffer.fadeOutAndStop(1);
                "#,
            )
            .expect("script");

        engine.host_mut().advance_clock(Duration::from_millis(1));
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        let commands = engine.host_mut().take_audio_commands();
        match &commands[..] {
            [
                AudioCommand::Preload { .. },
                AudioCommand::Play { id: play_id, .. },
                AudioCommand::SetVolume { id: fade_id, .. },
                AudioCommand::Stop { id: stop_id, .. },
            ] => {
                assert_eq!(fade_id, play_id);
                assert_eq!(stop_id, play_id);
            }
            commands => panic!("expected play, fade, then stop, got {commands:?}"),
        }

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn wave_sound_buffer_play_appends_audio_extension_for_dotted_storage_names() {
        let root = temp_root();
        fs::create_dir_all(root.join("sound")).expect("create sound dir");
        fs::write(
            root.join("sound/09.clock_like_name.ogg"),
            b"dotted audio bytes",
        )
        .expect("write audio bytes");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                Storages.addAutoPath("sound/");
                var buffer = new WaveSoundBuffer();
                buffer.open("09.clock_like_name");
                buffer.play();
                "#,
            )
            .expect("script");

        let commands = engine.host_mut().take_audio_commands();
        match &commands[..] {
            [
                AudioCommand::Preload { source, .. },
                AudioCommand::Play {
                    source: play_source,
                    ..
                },
            ] => {
                assert_eq!(source.storage(), "09.clock_like_name");
                assert_eq!(play_source.storage(), "09.clock_like_name");
            }
            commands => panic!("expected one play command, got {commands:?}"),
        }
        let provider = engine
            .host()
            .resource_provider()
            .expect("resource provider");
        assert!(provider.exists("09.clock_like_name"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn kag_audio_tags_queue_audio_commands() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("music.ogg"), b"bgm bytes").expect("write bgm");
        fs::write(root.join("click.wav"), b"se bytes").expect("write se");
        fs::write(
            root.join("scenario.ks"),
            "[playbgm storage=\"music.ogg\" volume=80]\n[playse storage=\"click.wav\"]\n[stopbgm time=250]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("scenario.ks").expect("scenario");
        engine
            .tick()
            .expect("tick should process audio tags without backend");

        let commands = engine.host_mut().take_audio_commands();
        assert_eq!(commands.len(), 3);
        assert!(matches!(
            &commands[0],
            AudioCommand::Play {
                bus: AudioBus::Bgm,
                load_policy: AudioLoadPolicy::Streaming,
                source,
                looping: true,
                volume,
                ..
            } if source.storage() == "music.ogg" && (*volume - 0.8).abs() < f32::EPSILON
        ));
        assert!(matches!(
            &commands[1],
            AudioCommand::Play {
                bus: AudioBus::SoundEffect,
                load_policy: AudioLoadPolicy::StaticCached,
                source,
                looping: false,
                ..
            } if source.storage() == "click.wav"
        ));
        assert!(matches!(
            commands[2],
            AudioCommand::StopBus {
                bus: AudioBus::Bgm,
                fade_seconds: 0.25,
            }
        ));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_returns_tag_dictionaries() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A\n").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("first.ks");
                var first = parser.getNextTag();
                var second = parser.getNextTag();
                return first.tagname + ":" + first.text + ":" + second.tagname + ":" + second.eol;
                "#,
            )
            .expect("script");
        // Official KAGParser keeps attribute values strings, and the `[r]`
        // it synthesises at end of line carries `eol` as the string "true".
        assert_eq!(value, Variant::String("ch:A:r:true".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_interrupts_before_next_tag() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("first.ks");
                parser.interrupt();
                return parser.getNextTag().tagname;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("interrupt".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_uses_scenario_load_callbacks() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.onScenarioLoad = function(storage) {
                    this.loadedName = storage;
                    return "A";
                };
                parser.onScenarioLoaded = function(storage) {
                    this.loadedDone = storage;
                };
                parser.loadScenario("virtual.ks");
                var tag = parser.getNextTag();
                return tag.text + ":" + parser.loadedName + ":" + parser.loadedDone;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("A:virtual.ks:virtual.ks".to_string())
        );
    }

    #[test]
    fn tjs_kag_parser_treats_true_scenario_load_callback_as_normal_load() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.onScenarioLoad = function(storage) { return true; };
                parser.loadScenario("first.ks");
                var tag = parser.getNextTag();
                return tag.tagname + ":" + tag.text;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("ch:A".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_fires_label_and_script_callbacks() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "*start|Opening\n[iscript]\nf.value = 7;\n[endscript]\nA",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var f = new Dictionary();
                var parser = new KAGParser();
                parser.onLabel = function(label, page) {
                    this.seenLabel = label;
                    this.seenPage = page;
                };
                parser.onScript = function(script, storage, start) {
                    this.seenScript = script;
                    this.seenScriptStorage = storage;
                    this.seenScriptStart = start;
                    Scripts.exec(script);
                };
                parser.loadScenario("first.ks");
                var tag = parser.getNextTag();
                return parser.seenLabel + ":" + parser.seenPage + ":" +
                    parser.seenScriptStorage + ":" + f.value + ":" + tag.text;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("*start:Opening:first.ks:7:A".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_allows_store_during_callbacks() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "*start\nA").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.onLabel = function(label, page) {
                    this.snapshot = this.store();
                };
                parser.loadScenario("first.ks");
                parser.getNextTag();
                return parser.snapshot.snapshot === void &&
                    parser.snapshot.storageName == "first.ks";
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::Integer(1));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_passes_void_for_labels_without_page_name() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "*start\nA").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.onLabel = function(label, page) {
                    this.seenPage = page;
                };
                parser.loadScenario("first.ks");
                parser.getNextTag();
                return parser.seenPage === void ? "void" : parser.seenPage;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("void".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_assign_accepts_bound_source_proxy() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class Parser extends KAGParser {
                    function Parser() { super.KAGParser(); }
                    function proxy() { return super; }
                    function copyFrom(src) { super.assign(src); }
                }
                var source = new Parser();
                source.onScenarioLoad = function(storage) { return "A\nB"; };
                source.loadScenario("virtual.ks");
                source.getNextTag();
                source.getNextTag();
                var target = new Parser();
                target.copyFrom(source.proxy());
                return target.getNextTag().text;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("B".to_string()));
    }

    #[test]
    fn tjs_array_count_truncation_removes_invalidated_animation_conductors() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class BaseConductor extends KAGParser {
                    function BaseConductor() { super.KAGParser(); }
                    function assign(src) { super.assign(src); }
                }
                class AnimationConductor extends BaseConductor {
                    function AnimationConductor(owner) { super.BaseConductor(); }
                    function assign(src) { super.assign(src); }
                }
                class AnimationLayer {
                    var Anim_segments = [];
                    function AnimationLayer() {
                        Anim_segments[0] = new AnimationConductor(this);
                    }
                    function assign(src) {
                        for(var i = Anim_segments.count - 1; i >= 0; i--) {
                            var seg = Anim_segments[i];
                            invalidate Anim_segments[i] if seg !== void;
                        }
                        var srcanimseg = src.Anim_segments;
                        var animseg = Anim_segments;
                        for(var i = srcanimseg.count - 1; i >= 0; i--) {
                            var seg = srcanimseg[i];
                            animseg[i] = void;
                            if(seg !== void) {
                                animseg[i] = new AnimationConductor(this);
                                animseg[i].assign(seg);
                            }
                        }
                        animseg.count = srcanimseg.count;
                    }
                }
                var source = new AnimationLayer();
                source.Anim_segments[0].onScenarioLoad = function(storage) { return "A\nB"; };
                source.Anim_segments[0].loadScenario("virtual.ks");
                source.Anim_segments[0].getNextTag();
                source.Anim_segments[0].getNextTag();
                var target = new AnimationLayer();
                target.Anim_segments[1] = new AnimationConductor(target);
                target.assign(source);
                var second = new AnimationLayer();
                second.assign(target);
                return second.Anim_segments.count + ":" + second.Anim_segments[0].getNextTag().text;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("1:B".to_string()));
    }

    #[test]
    fn tjs_kag_parser_process_callbacks_can_cancel_control_tags() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A[jump target=*end]B\n*end\nC").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.onJump = function(dic) {
                    this.jumpTarget = dic.target;
                    return false;
                };
                parser.loadScenario("first.ks");
                var a = parser.getNextTag();
                var b = parser.getNextTag();
                return a.text + b.text + ":" + parser.jumpTarget;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("AB:*end".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_skips_command_iscript_inside_if_block() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "@if exp=\"true\"\n@iscript\nvar x = 1;\n@endscript\n@endif\n[wait]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("first.ks");
                var tag = parser.getNextTag();
                return tag.tagname;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("wait".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_fires_call_return_callbacks() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "[call target=*sub]X\n*sub\n[return]Y",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.onCall = function(dic) {
                    this.callTarget = dic.target;
                    return true;
                };
                parser.onReturn = function(dic) {
                    this.returned = "yes";
                    return true;
                };
                parser.onAfterReturn = function() {
                    this.afterReturn = "done";
                };
                parser.loadScenario("first.ks");
                var tag = parser.getNextTag();
                return tag.text + ":" + parser.callTarget + ":" +
                    parser.returned + ":" + parser.afterReturn;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("X:*sub:yes:done".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_stops_when_return_callback_interrupts_at_outer_call() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("system.ks"),
            "*sys_config\n[call storage=\"config.ks\" target=*menu]A[return]B\n*sys_menu\nC",
        )
        .expect("write system scenario");
        fs::write(root.join("config.ks"), "*menu\n[return]").expect("write config scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.seen = "";
                var tags = "";
                parser.onReturn = function(elm) {
                    this.seen += this.callStackDepth;
                    if(this.callStackDepth == 1) {
                        return false;
                    }
                    return true;
                };
                parser.loadScenario("system.ks");
                parser.callLabel("*sys_config");
                for(var i = 0; i < 4; i++) {
                    var tag = parser.getNextTag();
                    if(tag === void) {
                        tags += "void";
                        break;
                    }
                    tags += tag.tagname;
                    if(tag.text !== void) tags += ":" + tag.text;
                    tags += ",";
                    if(tag.tagname == "interrupt") break;
                }
                return parser.seen + ":" + tags;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("21:ch:A,interrupt,".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_exposes_pop_macro_args() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "[macro name=m][font face=%face][wait][endmacro][m face=serif]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("first.ks");
                parser.getNextTag();
                var before = parser.macroParams.face;
                parser.popMacroArgs();
                return before + ":" + parser.macroParams.face;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("serif:".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_syncs_macros_copied_through_dictionary_members() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("define.ks"),
            "[macro name=button_chgimage][eval exp=\"global.changed = 1\"][endmacro]",
        )
        .expect("write macro scenario");
        fs::write(root.join("config.ks"), "[button_chgimage][s]").expect("write config scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var main = new KAGParser();
                main.loadScenario("define.ks");
                main.getNextTag();

                var extra = new KAGParser();
                (Dictionary.assign incontextof extra.macros)(main.macros);
                extra.clearCallStack();
                extra.loadScenario("config.ks");

                var expanded = extra.getNextTag();
                return expanded.tagname + ":" + expanded.exp;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("eval:global.changed = 1".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn plugin_registry_tracks_registered_and_linked_plugins() {
        struct TestPlugin;

        impl KrkrPlugin for TestPlugin {
            fn name(&self) -> &str {
                "test-plugin"
            }

            fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
                runtime.set_global_member("PluginValue", Variant::Integer(9));
                Ok(())
            }
        }

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(TestPlugin).expect("register plugin");

        assert_eq!(engine.plugin_count(), 1);
        assert_eq!(
            engine.tjs_runtime().global_member("PluginValue"),
            Variant::Integer(9)
        );
        assert!(
            engine
                .host()
                .linked_plugins()
                .any(|name| name == "test-plugin")
        );
    }

    /// A media a test plugin registers: `mock://` over an in-memory file table
    /// that can change while the engine runs, the way a `steam` cloud file or a
    /// `proxy` mapping does.
    struct MockMedia {
        files: Mutex<BTreeMap<String, Vec<u8>>>,
    }

    impl MockMedia {
        fn new() -> Self {
            Self {
                files: Mutex::new(BTreeMap::new()),
            }
        }

        fn publish(&self, path: &str, bytes: &[u8]) {
            self.files
                .lock()
                .expect("mock media lock")
                .insert(path.to_string(), bytes.to_vec());
        }

        fn file(&self, path: &str) -> Option<Vec<u8>> {
            self.files
                .lock()
                .expect("mock media lock")
                .get(path)
                .cloned()
        }
    }

    impl StorageMediaProvider for MockMedia {
        fn media_name(&self) -> &str {
            "mock"
        }

        fn exists(&self, name: &str) -> bool {
            self.files
                .lock()
                .expect("mock media lock")
                .contains_key(name)
        }

        fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
            let files = self.files.lock().expect("mock media lock");
            let bytes = files.get(name).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no entry `{name}`"))
            })?;
            Ok(Box::new(io::Cursor::new(bytes.clone())))
        }

        fn write(&self, name: &str, _mode: &str, bytes: &[u8]) -> io::Result<()> {
            self.publish(name, bytes);
            Ok(())
        }
    }

    /// A media-carrying plugin: `register` runs at boot and again when the
    /// first `Plugins.link` installs the module, and both calls hand the
    /// registry the same `Arc` (design A.3.2 of
    /// `docs/plugins/plugin-facing-engine-facilities.md`).
    struct MediaPlugin {
        media: Arc<MockMedia>,
        register_calls: Arc<AtomicU64>,
        unregister_calls: Arc<AtomicU64>,
    }

    impl KrkrPlugin for MediaPlugin {
        fn name(&self) -> &str {
            "media-plugin"
        }

        fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
            self.register_calls.fetch_add(1, Ordering::Relaxed);
            storage::register_storage_media(
                runtime,
                Arc::clone(&self.media) as Arc<dyn StorageMediaProvider>,
            )
        }

        fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
            self.unregister_calls.fetch_add(1, Ordering::Relaxed);
            storage::unregister_storage_media(runtime, self.media.media_name());
            Ok(())
        }
    }

    fn media_read(engine: &KrkrEngine, name: &str) -> io::Result<Vec<u8>> {
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let data = storage.read_binary_storage(name)?;
        Ok(data.as_bytes()?.into_owned())
    }

    fn media_write(engine: &KrkrEngine, name: &str, bytes: &[u8]) -> io::Result<()> {
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .map_err(|error| io::Error::other(error.to_string()))?;
        storage.write_binary_storage(name, "", bytes)
    }

    #[test]
    fn plugin_media_registers_at_boot_again_on_link_and_leaves_on_unlink() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project root");
        let media = Arc::new(MockMedia::new());
        let register_calls = Arc::new(AtomicU64::new(0));
        let unregister_calls = Arc::new(AtomicU64::new(0));
        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        engine
            .register_plugin(MediaPlugin {
                media: Arc::clone(&media),
                register_calls: Arc::clone(&register_calls),
                unregister_calls: Arc::clone(&unregister_calls),
            })
            .expect("the plugin registers its media at boot");

        assert_eq!(register_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            engine.tjs_runtime().host().storage_media_names(),
            vec!["mock".to_string()]
        );
        media.publish("./data.bin", b"mock bytes");
        assert_eq!(
            media_read(&engine, "mock://./data.bin").expect("media read"),
            b"mock bytes"
        );
        media_write(&engine, "mock://./saved.bin", b"saved bytes").expect("media write");
        assert_eq!(
            media.file("./saved.bin").as_deref(),
            Some(&b"saved bytes"[..])
        );

        // The first explicit link installs the module again with the same
        // `Arc`; the registry dedupes it instead of failing the script with
        // `TVPMediaNameHadAlreadyBeenRegistered`.
        engine
            .execute_script("link.tjs", r#"Plugins.link("media-plugin");"#)
            .expect("linking again must not be a duplicate registration");
        assert_eq!(register_calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            engine.tjs_runtime().host().storage_media_names(),
            vec!["mock".to_string()]
        );
        assert_eq!(
            media_read(&engine, "mock://./data.bin").expect("media read after link"),
            b"mock bytes"
        );
        // A second link is the reference's early return for an already-loaded
        // module: nothing is installed a third time.
        engine
            .execute_script("link.tjs", r#"Plugins.link("media-plugin");"#)
            .expect("second link");
        assert_eq!(register_calls.load(Ordering::Relaxed), 2);

        // `Plugins.unlink` runs the plugin's `unregister`, which drops the
        // media; the scheme then keeps the pre-registry behaviour.
        engine
            .execute_script("unlink.tjs", r#"Plugins.unlink("media-plugin");"#)
            .expect("unlink");
        assert_eq!(unregister_calls.load(Ordering::Relaxed), 1);
        assert!(engine.tjs_runtime().host().storage_media_names().is_empty());
        assert!(media_read(&engine, "mock://./data.bin").is_err());

        // Unlinking forgets the module, so a later link installs it again
        // (Open question A.5.5: `script_linked_plugins` is cleared with it).
        engine
            .execute_script("relink.tjs", r#"Plugins.link("media-plugin");"#)
            .expect("relink after unlink");
        assert_eq!(register_calls.load(Ordering::Relaxed), 3);
        assert_eq!(
            engine.tjs_runtime().host().storage_media_names(),
            vec!["mock".to_string()]
        );
        assert_eq!(
            media_read(&engine, "mock://./data.bin").expect("media read after relink"),
            b"mock bytes"
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_timer_callback_runs_from_update() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "timer.tjs",
                r#"
                global.timerProbeCount = 0;
                var timerProbe = new Timer(function() {
                    global.timerProbeCount++;
                    timerProbe.enabled = false;
                }, "");
                global.timerProbe = timerProbe;
                timerProbe.interval = 1000;
                timerProbe.enabled = true;
                "#,
            )
            .expect("script");
        assert_eq!(
            engine
                .tjs_runtime()
                .host()
                .scheduler()
                .timer_handles()
                .len(),
            1
        );
        let timer = object_handle(&engine, "timerProbe");
        force_timer_due(&mut engine, timer);

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(
            engine.tjs_runtime().global_member("timerProbeCount"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().object_member(timer, "enabled"),
            Variant::Integer(0)
        );
    }

    #[test]
    fn native_timer_interval_zero_does_not_fire() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "timer_zero.tjs",
                r#"
                global.timerProbeCount = 0;
                global.timerProbe = new Timer(function() {
                    global.timerProbeCount++;
                }, "");
                timerProbe.interval = 0;
                timerProbe.enabled = true;
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(
            engine.tjs_runtime().global_member("timerProbeCount"),
            Variant::Integer(0)
        );

        engine.host_mut().advance_clock(Duration::from_millis(1));
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("next clock turn");

        assert_eq!(
            engine.tjs_runtime().global_member("timerProbeCount"),
            Variant::Integer(0)
        );
    }

    #[test]
    fn idle_async_trigger_is_not_preempted_by_zero_interval_timer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "timer_zero_async.tjs",
                r#"
                global.trace = "";
                global.timerProbe = new Timer(function() {
                    global.trace += "T";
                }, "");
                global.asyncProbe = new AsyncTrigger(function() {
                    global.trace += "A";
                }, "");
                timerProbe.interval = 0;
                timerProbe.enabled = true;
                asyncProbe.mode = 2;
                asyncProbe.trigger();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("A".to_string())
        );
    }

    #[test]
    fn stale_timer_event_is_skipped_after_async_callback_disables_it() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "timer_async.tjs",
                r#"
                global.timerProbeCount = 0;
                function stopTimer() {
                    global.timerProbeCount++;
                    timerProbe.enabled = false;
                }
                var timerProbe = new Timer(stopTimer, "");
                var asyncProbe = new AsyncTrigger(stopTimer, "");
                global.timerProbe = timerProbe;
                global.asyncProbe = asyncProbe;
                timerProbe.interval = 1000;
                timerProbe.enabled = true;
                asyncProbe.trigger();
                "#,
            )
            .expect("script");
        let timer_probe = object_handle(&engine, "timerProbe");
        force_timer_due(&mut engine, timer_probe);

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(
            engine.tjs_runtime().global_member("timerProbeCount"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn async_trigger_invokes_named_owner_action_with_method_receiver() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "async_action_owner.tjs",
                r#"
                global.actionOwner = new Dictionary();
                actionOwner.count = 0;
                actionOwner.namedAction = function() { this.count += 1; };
                actionOwner.action = function() { this.count += 10; };
                global.namedTrigger = new AsyncTrigger(actionOwner, "namedAction");
                global.defaultTrigger = new AsyncTrigger(actionOwner);
                namedTrigger.trigger();
                defaultTrigger.trigger();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        let owner = object_handle(&engine, "actionOwner");
        assert_eq!(
            engine.tjs_runtime().object_member(owner, "count"),
            Variant::Integer(11)
        );
    }

    #[test]
    fn system_continuous_handler_runs_once_per_update_until_removed() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "continuous.tjs",
                r#"
                global.continuousCount = 0;
                function continuousProbe() { global.continuousCount++; }
                System.addContinuousHandler(continuousProbe);
                "#,
            )
            .expect("script");

        let input = EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new());
        engine
            .update(input.clone(), Duration::from_millis(16))
            .expect("first update");
        engine
            .update(input, Duration::from_millis(16))
            .expect("second update");
        assert_eq!(
            engine.tjs_runtime().global_member("continuousCount"),
            Variant::Integer(2)
        );

        engine
            .execute_script(
                "continuous.tjs",
                "System.removeContinuousHandler(continuousProbe);",
            )
            .expect("remove handler");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(16),
            )
            .expect("third update");
        assert_eq!(
            engine.tjs_runtime().global_member("continuousCount"),
            Variant::Integer(2)
        );
    }

    #[test]
    fn system_continuous_handler_preserves_method_receiver_and_tick_argument() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "continuous_method.tjs",
                r#"
                class ContinuousOwner {
                    var count = 0;
                    var lastTick = -1;
                    function ContinuousOwner() {
                        System.addContinuousHandler(callback);
                    }
                    function callback(tick) {
                        count++;
                        lastTick = tick;
                    }
                }
                global.continuousOwner = new ContinuousOwner();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(16),
            )
            .expect("update");
        let owner = object_handle(&engine, "continuousOwner");
        assert_eq!(
            engine.tjs_runtime().object_member(owner, "count"),
            Variant::Integer(1)
        );
        assert!(
            engine
                .tjs_runtime()
                .object_member(owner, "lastTick")
                .to_integer()
                .is_ok_and(|tick| tick >= 0)
        );
    }

    #[test]
    fn system_exit_sets_host_termination_request() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script("system.tjs", "System.exit();")
            .expect("exit");

        assert!(engine.host().termination_requested());
    }

    #[test]
    fn system_compatibility_helpers_match_krkr_contracts() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "system_compat.tjs",
                r#"
                System.setArgument("profile", "demo");
                var first = System.getArgument("profile");
                var second = System.createUUID();
                var third = System.createUUID();
                return first + ":" + (second != third) + ":" + System.toActualColor(0x112233);
                "#,
            )
            .expect("system compatibility script");
        assert_eq!(value, Variant::String("demo:1:1122867".to_string()));
    }

    #[test]
    fn persist_runtime_state_calls_kag_save_system_variables() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "persist.tjs",
                r#"
                global.saved = 0;
                global.kag = new Window();
                kag.saveSystemVariables = function() { global.saved++; };
                "#,
            )
            .expect("script");

        engine.persist_runtime_state().expect("persist");

        assert_eq!(
            engine.tjs_runtime().global_member("saved"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn persist_runtime_state_syncs_kag_system_audio_state() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "persist_audio.tjs",
                r#"
                global.saved = "";
                global.kag = new Window();
                kag.fullScreen = false;
                kag.fullScreened = true;
                kag.scflags = %[];
                kag.sflags = %[];
                kag.sflags.bgm_vol = void;
                kag.sflags.se_vol = void;
                kag.sflags.cv_vol = void;
                kag.bgm = %[];
                kag.bgm.currentBuffer = new WaveSoundBuffer(kag);
                kag.bgm.currentBuffer.volume2 = 35000;
                kag.se = [];
                var se0 = new WaveSoundBuffer(kag);
                se0.volume2 = 55000;
                kag.se.add(se0);
                var se1 = new WaveSoundBuffer(kag);
                se1.volume2 = 56000;
                kag.se.add(se1);
                var se2 = new WaveSoundBuffer(kag);
                se2.volume2 = 57000;
                kag.se.add(se2);
                var se3 = new WaveSoundBuffer(kag);
                se3.volume2 = 60000;
                kag.se.add(se3);
                kag.saveSystemVariables = function() {
                    global.saved = scflags.fullScreen + ":" +
                        scflags.bgm.globalVolume + ":" + scflags.se[0].globalVolume + ":" +
                        sflags.bgm_vol + ":" + sflags.se_vol + ":" + sflags.cv_vol;
                };
                "#,
            )
            .expect("script");

        engine.persist_runtime_state().expect("persist");

        assert_eq!(
            engine.tjs_runtime().global_member("saved"),
            Variant::String("0:35000:55000:35:55:60".to_string())
        );
        assert_eq!(
            engine
                .execute_expression("persist_audio_count.tjs", "kag.scflags.se.count")
                .expect("se count"),
            Variant::Integer(4)
        );
    }

    #[test]
    fn scripts_eval_storage_normalizes_legacy_kag_se_null_entries() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("datasc.ksd"),
            r#"(const) %[
 "se" => (const) [
  null,
  (const) %[
   "globalVolume" => 12000,
  ],
  null,
 ],
]"#,
        )
        .expect("write scflags");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_expression(
                "normalize_scflags.tjs",
                r#"
                (function() {
                    var d = Scripts.evalStorage("datasc.ksd");
                    return (d.se[0] === void ? 1 : 0) + ":" +
                        d.se.count + ":" + d.se[1].globalVolume;
                })()
                "#,
            )
            .expect("eval storage");

        assert_eq!(value, Variant::String("1:2:12000".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn system_metrics_override_screen_size_globals() {
        let engine = KrkrEngine::new(EngineConfig {
            system_metrics: SystemMetrics {
                screen_width: 2560,
                screen_height: 1440,
                desktop_left: 10,
                desktop_top: 20,
                desktop_width: 2500,
                desktop_height: 1400,
            },
            ..EngineConfig::default()
        })
        .expect("engine");

        let system = object_handle(&engine, "System");
        assert_eq!(
            engine.tjs_runtime().object_member(system, "screenWidth"),
            Variant::Integer(2560)
        );
        assert_eq!(
            engine.tjs_runtime().object_member(system, "screenHeight"),
            Variant::Integer(1440)
        );
    }

    #[test]
    fn system_touch_images_uses_graphic_cache_entry_points() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 1, 1, &[255, 0, 0, 255]);
        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        let value = engine
            .execute_expression(
                "touch_images.tjs",
                r#"
                (function() {
                    System.touchImages(["sprite.png"], 0, 1000);
                    System.clearGraphicCache();
                    return "ok";
                })()
                "#,
            )
            .expect("touch images");

        assert_eq!(value, Variant::String("ok".to_string()));
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|message| message.contains("touched 1 image(s)"))
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn request_runtime_close_uses_script_close_then_native_window_close() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "close.tjs",
                r#"
                global.saved = 0;
                class GameWindow extends Window {
                    function GameWindow() { super.Window(); }
                    function close() {
                        global.saved++;
                        super.close(...);
                    }
                }
                global.kag = new GameWindow();
                "#,
            )
            .expect("script");

        engine.request_runtime_close().expect("close");

        assert_eq!(
            engine.tjs_runtime().global_member("saved"),
            Variant::Integer(1)
        );
        assert!(engine.host().termination_requested());
    }

    #[test]
    fn window_on_close_query_respects_can_close_argument() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let denied = engine
            .execute_script(
                "close_query.tjs",
                r#"
                global.kag = new Window();
                kag.onCloseQuery(false);
                return kag.__nativeCanClose + ":" + (typeof kag.__nativeClosed == "undefined" ? 1 : 0);
                "#,
            )
            .expect("deny close");

        assert_eq!(denied, Variant::String("0:1".to_string()));
        assert!(!engine.host().termination_requested());

        engine
            .execute_script("close_query_accept.tjs", "kag.onCloseQuery(true);")
            .expect("accept close");

        assert!(engine.host().termination_requested());
    }

    #[test]
    fn native_async_trigger_callback_runs_from_update() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "async.tjs",
                r#"
                global.asyncProbeCount = 0;
                var asyncProbe = new AsyncTrigger(function() {
                    global.asyncProbeCount++;
                }, "");
                global.asyncProbe = asyncProbe;
                asyncProbe.trigger();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(
            engine.tjs_runtime().global_member("asyncProbeCount"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn event_exception_handler_receives_escaped_tjs_exception() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "exception-handler.tjs",
                r#"
                    class ConductorException extends Exception {
                        function ConductorException() { super.Exception(...); }
                    }
                    global.handled = 0;
                    global.classSeen = false;
                    global.baseClassSeen = false;
                    global.errorMessage = "";
                    System.exceptionHandler = function(e) {
                        global.handled++;
                        global.classSeen = e instanceof "ConductorException";
                        global.baseClassSeen = e instanceof "Exception";
                        global.errorMessage = e.message;
                        return true;
                    };
                    global.exc = new ConductorException("boom");
                    global.trigger = new AsyncTrigger(function() {
                        throw global.exc;
                    }, "");
                    trigger.trigger();
                "#,
            )
            .expect("install exception handler");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("handled event error");

        assert_eq!(
            engine.tjs_runtime().global_member("handled"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("classSeen"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("baseClassSeen"),
            Variant::Integer(1)
        );
        let Variant::String(error_message) = engine.tjs_runtime().global_member("errorMessage")
        else {
            panic!("exception handler did not receive a string message");
        };
        assert_eq!(error_message, "boom");
    }

    #[test]
    fn exclusive_async_trigger_runs_before_queued_input() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "exclusive_input.tjs",
                r#"
                global.waitReady = false;
                global.clickSawWait = false;
                global.kag = new Dictionary();
                kag.innerWidth = 320;
                kag.onPrimaryClick = function() {
                    global.clickSawWait = global.waitReady;
                };
                var asyncProbe = new AsyncTrigger(function() {
                    global.waitReady = true;
                }, "");
                asyncProbe.mode = atmExclusive;
                asyncProbe.trigger();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::PointerInput {
                        button: PointerButton::Primary,
                        state: ButtonState::Pressed,
                    }],
                ),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(
            engine.tjs_runtime().global_member("waitReady"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("clickSawWait"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn scheduler_runs_exclusive_input_then_normal_script_events() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "scheduler_order.tjs",
                r#"
                global.trace = "";
                global.kag = new Dictionary();
                kag.innerWidth = 320;
                kag.onPrimaryClick = function() {
                    global.trace += "I";
                };
                var normalProbe = new AsyncTrigger(function() {
                    global.trace += "N";
                }, "");
                var exclusiveProbe = new AsyncTrigger(function() {
                    global.trace += "E";
                }, "");
                exclusiveProbe.mode = atmExclusive;
                normalProbe.trigger();
                exclusiveProbe.trigger();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::PointerInput {
                        button: PointerButton::Primary,
                        state: ButtonState::Pressed,
                    }],
                ),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("EIN".to_string())
        );
    }

    #[test]
    fn scheduler_keeps_async_trigger_exclusive_normal_idle_order_stable() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "async_order.tjs",
                r#"
                global.trace = "";
                var exclusiveProbe = new AsyncTrigger(function() {
                    global.trace += "E";
                }, "");
                var normalProbe = new AsyncTrigger(function() {
                    global.trace += "N";
                }, "");
                var idleProbe = new AsyncTrigger(function() {
                    global.trace += "I";
                }, "");
                function continuousProbe() {
                    global.trace += "C";
                    System.removeContinuousHandler(continuousProbe);
                }
                exclusiveProbe.mode = atmExclusive;
                idleProbe.mode = atmAtIdle;
                System.addContinuousHandler(continuousProbe);
                normalProbe.trigger();
                idleProbe.trigger();
                exclusiveProbe.trigger();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("ENIC".to_string())
        );
    }

    #[test]
    fn at_idle_async_trigger_self_reschedule_waits_for_next_frame() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "idle_self_reschedule.tjs",
                r#"
                global.idleCount = 0;
                global.idleProbe = new AsyncTrigger(function() {
                    global.idleCount++;
                    if(global.idleCount < 3) global.idleProbe.trigger();
                }, "");
                idleProbe.mode = atmAtIdle;
                idleProbe.cached = true;
                idleProbe.trigger();
                "#,
            )
            .expect("script");

        for expected in 1..=3 {
            engine
                .update(
                    EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                    Duration::ZERO,
                )
                .expect("update");
            assert_eq!(
                engine.tjs_runtime().global_member("idleCount"),
                Variant::Integer(expected)
            );
        }
    }

    #[test]
    fn scheduler_limits_layer_update_reposts_during_on_paint() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "paint_repost.tjs",
                r#"
                global.paintCount = 0;
                global.trace = "";
                class RepaintLayer extends Layer {
                    function RepaintLayer() {
                        super.Layer(...);
                        setSize(10, 10);
                        setImageSize(10, 10);
                        visible = true;
                    }
                    function onPaint() {
                        global.paintCount++;
                        global.trace += "P";
                        update();
                    }
                }
                global.probeLayer = new RepaintLayer();
                probeLayer.update();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("paint frame");

        assert_eq!(
            engine.tjs_runtime().global_member("paintCount"),
            Variant::Integer(2)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("PP".to_string())
        );

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("second frame");
        assert_eq!(
            engine.tjs_runtime().global_member("paintCount"),
            Variant::Integer(2)
        );
    }

    #[test]
    fn scheduler_defers_events_posted_by_handler_until_after_window_update() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "sequence_window_update.tjs",
                r#"
                global.trace = "";
                class PaintLayer extends Layer {
                    function PaintLayer() {
                        super.Layer(...);
                        setSize(10, 10);
                        setImageSize(10, 10);
                        visible = true;
                    }
                    function onPaint() {
                        global.trace += "P";
                    }
                }
                global.probeLayer = new PaintLayer();
                var secondProbe = new AsyncTrigger(function() {
                    global.trace += "B";
                }, "");
                var firstProbe = new AsyncTrigger(function() {
                    global.trace += "A";
                    secondProbe.trigger();
                    probeLayer.update();
                }, "");
                firstProbe.trigger();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");

        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("AP".to_string())
        );
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("next delivery turn");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("APB".to_string())
        );
    }

    #[test]
    fn scheduler_preserves_script_event_order_across_modal_resume() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "modal_order.tjs",
                r#"
                global.trace = "";
                global.modal = new Window();
                var firstProbe = new AsyncTrigger(function() {
                    global.trace += "A";
                    modal.showModal();
                    global.trace += "R";
                }, "");
                var secondProbe = new AsyncTrigger(function() {
                    global.trace += "B";
                }, "");
                firstProbe.trigger();
                secondProbe.trigger();
                "#,
            )
            .expect("script");

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("modal frame");
        assert!(engine.tjs_runtime().is_suspended());
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("A".to_string())
        );

        let modal = object_handle(&engine, "modal");
        engine
            .tjs_runtime_mut()
            .call_object_method(modal, "close", Vec::new())
            .expect("close modal");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("resume frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("AR".to_string())
        );

        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("queued event frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("ARB".to_string())
        );
    }

    #[test]
    fn timer_and_async_trigger_require_an_action_owner() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let timer_error = engine
            .execute_script("new_timer.tjs", "new Timer();")
            .expect_err("Timer requires an owner");
        assert!(
            timer_error.message.contains("requires an action owner"),
            "{timer_error:?}"
        );
        let trigger_error = engine
            .execute_script("new_async.tjs", "new AsyncTrigger();")
            .expect_err("AsyncTrigger requires an owner");
        assert!(
            trigger_error.message.contains("requires an action owner"),
            "{trigger_error:?}"
        );
        engine
            .execute_script("async_owner.tjs", "new AsyncTrigger(function() {}, \"\");")
            .expect("AsyncTrigger with owner stays valid");
    }

    #[test]
    fn timer_modes_route_exclusive_and_idle_callbacks() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "timer_modes.tjs",
                r#"
                global.trace = "";
                global.exclusiveTimer = new Timer(function() {
                    global.trace += "E";
                    exclusiveTimer.enabled = false;
                }, "");
                global.idleTimer = new Timer(function() {
                    global.trace += "I";
                    idleTimer.enabled = false;
                }, "");
                exclusiveTimer.mode = 1;
                idleTimer.mode = 2;
                exclusiveTimer.interval = 10;
                idleTimer.interval = 10;
                exclusiveTimer.enabled = true;
                idleTimer.enabled = true;
                "#,
            )
            .expect("script");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("arm timers");
        engine.host_mut().advance_clock(Duration::from_millis(10));
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("EI".to_string())
        );
    }

    #[test]
    fn event_disabled_drops_idle_timer_and_cursor_move() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "event_disabled.tjs",
                r#"
                global.trace = "";
                global.idleTimer = new Timer(function() {
                    global.trace += "T";
                    idleTimer.enabled = false;
                }, "");
                idleTimer.mode = 2;
                idleTimer.interval = 10;
                idleTimer.enabled = true;
                System.eventDisabled = true;
                "#,
            )
            .expect("script");
        engine.host_mut().advance_clock(Duration::from_millis(10));
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![EngineEvent::CursorMoved {
                        position: Point::new(12.0, 8.0),
                    }],
                ),
                Duration::ZERO,
            )
            .expect("disabled frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("".to_string())
        );
        assert_eq!(engine.host().cursor_position(), None);

        engine
            .execute_script("enable.tjs", "System.eventDisabled = false;")
            .expect("enable events");
        engine.host_mut().advance_clock(Duration::from_millis(10));
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("enabled frame");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("T".to_string())
        );
    }

    #[test]
    fn timer_interval_change_restarts_cadence() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "timer_interval.tjs",
                r#"
                global.fires = 0;
                global.timerProbe = new Timer(function() {
                    global.fires++;
                    timerProbe.enabled = false;
                }, "");
                timerProbe.interval = 50;
                timerProbe.enabled = true;
                "#,
            )
            .expect("script");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("arm timer");
        engine.host_mut().advance_clock(Duration::from_millis(50));
        engine
            .execute_script("change_interval.tjs", "timerProbe.interval = 1000;")
            .expect("change interval");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("after interval change");
        assert_eq!(
            engine.tjs_runtime().global_member("fires"),
            Variant::Integer(0)
        );
        engine.host_mut().advance_clock(Duration::from_millis(1000));
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("after new interval");
        assert_eq!(
            engine.tjs_runtime().global_member("fires"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn timer_catch_up_caps_late_periods_and_honors_capacity() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "timer_catchup.tjs",
                r#"
                global.fires = 0;
                global.timerProbe = new Timer(function() {
                    global.fires++;
                }, "");
                timerProbe.interval = 10;
                timerProbe.capacity = 2;
                timerProbe.enabled = true;
                "#,
            )
            .expect("script");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("arm timer");
        engine.host_mut().advance_clock(Duration::from_millis(80));
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("catch-up frame");
        assert_eq!(
            engine.tjs_runtime().global_member("fires"),
            Variant::Integer(2)
        );
    }

    #[test]
    fn continuous_handler_removed_mid_pass_is_skipped() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "continuous_live.tjs",
                r#"
                global.trace = "";
                function first() {
                    global.trace += "A";
                    System.removeContinuousHandler(second);
                }
                function second() {
                    global.trace += "B";
                }
                System.addContinuousHandler(first);
                System.addContinuousHandler(second);
                "#,
            )
            .expect("script");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert_eq!(
            engine.tjs_runtime().global_member("trace"),
            Variant::String("A".to_string())
        );
    }

    #[test]
    fn wave_pause_status_fade_delay_and_set_pos_match_krkr() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "wave_compat.tjs",
                r#"
                global.statusChanges = 0;
                global.buffer = new WaveSoundBuffer();
                buffer.onStatusChanged = function() { global.statusChanges++; };
                buffer.open("voice.ogg");
                buffer.play();
                buffer.play();
                buffer.paused = true;
                buffer.setPos(1, 2, 3);
                buffer.fade(10000, 50, 25);
                global.status = buffer.status;
                global.paused = buffer.paused;
                "#,
            )
            .expect("script");
        assert_eq!(
            engine.tjs_runtime().global_member("statusChanges"),
            Variant::Integer(1)
        );
        assert_eq!(
            engine.tjs_runtime().global_member("status"),
            Variant::String("play".to_string())
        );
        assert_eq!(
            engine.tjs_runtime().global_member("paused"),
            Variant::Integer(1)
        );
        let buffer = object_handle(&engine, "buffer");
        assert_eq!(
            engine.tjs_runtime().object_member(buffer, "posX"),
            Variant::Real(1.0)
        );
        assert_eq!(
            engine.tjs_runtime().object_member(buffer, "posY"),
            Variant::Real(2.0)
        );
        assert_eq!(
            engine.tjs_runtime().object_member(buffer, "posZ"),
            Variant::Real(3.0)
        );
        let commands = engine.host_mut().take_audio_commands();
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, AudioCommand::Pause { .. })),
            "expected a pause command, got {commands:?}"
        );
        assert!(engine.host().scheduler().has_audio_fade_completion(buffer));
        engine.host_mut().advance_clock(Duration::from_millis(74));
        let now = engine.tjs_runtime.host_mut().now_millis();
        engine
            .tjs_runtime
            .host_mut()
            .scheduler_mut()
            .post_due_audio_fade_completions(now);
        assert!(engine.host().scheduler().has_audio_fade_completion(buffer));
        engine.host_mut().advance_clock(Duration::from_millis(1));
        let now = engine.tjs_runtime.host_mut().now_millis();
        engine
            .tjs_runtime
            .host_mut()
            .scheduler_mut()
            .post_due_audio_fade_completions(now);
        assert!(!engine.host().scheduler().has_audio_fade_completion(buffer));
    }

    fn test_tag(name: &str, attrs: &[(&str, &str)]) -> Tag {
        Tag::new(
            name,
            attrs
                .iter()
                .map(|(name, value)| {
                    Attribute::named(*name, AttributeValue::Literal((*value).to_string()))
                })
                .collect(),
            krkr_kag::TagOrigin::Bracket,
            krkr_kag::SourceSpan::empty(0),
            krkr_kag::SourceLocation::default(),
        )
    }

    #[test]
    fn kag_tag_dictionary_preserves_void_attributes() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let tag = Tag::new(
            "bgmopt",
            vec![Attribute::named("gvolume", AttributeValue::Void)],
            krkr_kag::TagOrigin::Bracket,
            krkr_kag::SourceSpan::empty(0),
            krkr_kag::SourceLocation::default(),
        );

        let tag_object = tag_to_dictionary(engine.tjs_runtime_mut(), &tag).expect("tag object");

        assert_eq!(
            engine.tjs_runtime().object_member(tag_object, "gvolume"),
            Variant::Void
        );
    }

    /// KAGEX tag handlers receive the parser's tag dictionary and copy it
    /// (`var dict = new Dictionary(); (Dictionary.assign incontextof dict)(elm,
    /// 1)`) before testing attributes.  A bare `[syspage free]` attribute has to
    /// reach that copy as the truthy string `"true"` the reference parser stores
    /// (`kagparser.cpp` writes `TJS_W("true")` for an attribute without a value),
    /// otherwise the handler's `free` arm — the only layer-clearing step of the
    /// save screen's close — is skipped and the outgoing save image survives the
    /// close transition.
    ///
    /// The arm chain that follows inside the game's handler (`if (dict.uiload)
    /// ... else if (dict.clear) ... else if (dict.free)`) is pinned by krkr-tjs2:
    /// a Dictionary instance carries no members of its own, so this copy answers
    /// `clear` with void and the chain reaches the `free` attribute (see
    /// `runtime::dictionary_tests::kagex_attribute_chain_reaches_the_free_arm`,
    /// fixed by commit f4fd407 "Match Dictionary.assign to the reference").
    #[test]
    fn kag_bare_attribute_reaches_the_handler_dictionary_copy_as_true() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[syspage free page=back]").expect("write scenario");

        let mut engine = image_test_engine(&root);
        // The engine hands `onTag` to the game's own handler object.  A class
        // instance answers a missing member with member-not-found, so the
        // handler's unqualified global reads fall back to the global object the
        // way the game's KAGEX handlers do.
        let handler = engine
            .execute_script(
                "inline.tjs",
                r#"
                class SyspageHandler {
                    var seen = "";
                    var copy = "";
                    function onTag(elm) {
                        this.seen = ("" + elm.free) + "/" + (typeof elm.free) + "/" + (elm.free ? "T" : "F") + "/" + ("" + elm.page);
                        var dict = new Dictionary();
                        (Dictionary.assign incontextof dict)(elm, 1);
                        if (dict.layer === void) { dict.layer = "message1"; }
                        this.copy = ("" + dict.free) + "/" + (dict.free ? "T" : "F") + "/" + ("" + dict.page) + "/" + ("" + dict.layer);
                        return 1;
                    }
                }
                return new SyspageHandler();
                "#,
            )
            .expect("handler")
            .object_handle()
            .unwrap_or_else(|| panic!("expected handler object"));
        engine.set_kag_handler(handler);

        engine.load_kag_scenario("first.ks").expect("load scenario");
        engine.tick().expect("tick");

        assert_eq!(
            engine.tjs_runtime().object_member(handler, "seen"),
            Variant::String("true/String/T/back".to_string()),
            "the engine's tag dictionary carries a bare attribute as the truthy string \"true\""
        );
        assert_eq!(
            engine.tjs_runtime().object_member(handler, "copy"),
            Variant::String("true/T/back/message1".to_string()),
            "the handler's Dictionary.copy keeps the attribute and the layer default"
        );
        // The branch decision that follows in the game's handler is pinned by
        // krkr-tjs2's Dictionary fix (commit f4fd407): a Dictionary instance has
        // no members of its own, so this copy answers `clear` with void and the
        // KAGEX arm chain reaches its `free` arm.  When this test landed,
        // `Dictionary.assign` still kept the destination's native method members,
        // so the chain answered `clear` — the copy's own method — instead of the
        // `free` attribute (the reference clears the destination first,
        // `tTJSDictionaryNI::Assign` -> `Owner->Clear()`).

        fs::remove_dir_all(root).expect("cleanup");
    }

    fn force_timer_due(engine: &mut KrkrEngine, timer: ObjectHandle) {
        engine
            .tjs_runtime
            .host_mut()
            .scheduler_mut()
            .set_timer_next_fire_millis(timer, Some(0));
    }

    /// Attaches `window` to `layer` the way the engine's own internals do.
    ///
    /// A script write to `Layer.window` is refused with `TJS_E_ACCESSDENYED`
    /// (`TJS_DENY_NATIVE_PROP_SETTER`, `LayerIntf.cpp:8689`), and KAG never
    /// writes it either -- a layer receives its window in the constructor. The
    /// transition tests use a plain object where KAG has its `MainWindow`, so
    /// they can watch the `transCount` relay the engine keeps balanced there,
    /// and wire it through the host storage the property getter reads.
    fn attach_layer_window(engine: &mut KrkrEngine, layer: &str, window: &str) {
        let layer = expression_object(engine, layer);
        let window = expression_object(engine, window);
        engine
            .host_mut()
            .set_native_layer_window(layer, Some(window), Variant::Object(window));
    }

    /// The object a test expression evaluates to. Script values that came from
    /// `this` or `new` are self-bound (`Variant::Closure`), so the tests' own
    /// readers unwrap the binding the way the engine's readers do
    /// (`Variant::object_handle`).
    fn expression_object(engine: &mut KrkrEngine, expression: &str) -> ObjectHandle {
        engine
            .execute_expression("inline.tjs", expression)
            .unwrap_or_else(|error| panic!("{expression}: {error}"))
            .object_handle()
            .unwrap_or_else(|| panic!("{expression} is not an object"))
    }

    fn temp_root() -> PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "Kirakira-engine-{}-{nanos}-{id}",
            std::process::id()
        ))
    }

    fn integer_return_bytecode(value: i32) -> Vec<u8> {
        let data_payload = data_pool(value);
        let object_payload = code_object(vec![8, 0], vec![1, 0, 0, 118, 0, 119], 1);
        let mut objects_payload = Vec::new();
        push_i32(&mut objects_payload, 0);
        push_i32(&mut objects_payload, 1);
        objects_payload.extend_from_slice(b"TJS2");
        push_i32(&mut objects_payload, object_payload.len() as i32);
        objects_payload.extend_from_slice(&object_payload);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"TJS2100\0");
        push_i32(&mut bytes, 0);
        bytes.extend_from_slice(b"DATA");
        push_i32(&mut bytes, (data_payload.len() + 8) as i32);
        bytes.extend_from_slice(&data_payload);
        bytes.extend_from_slice(b"OBJS");
        push_i32(&mut bytes, (objects_payload.len() + 8) as i32);
        bytes.extend_from_slice(&objects_payload);
        let size = bytes.len() as i32;
        bytes[8..12].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    fn data_pool(value: i32) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 1);
        push_i32(&mut bytes, value);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 1);
        push_utf16_string(&mut bytes, "global");
        push_i32(&mut bytes, 0);
        bytes
    }

    fn code_object(data_slots: Vec<i16>, code_words: Vec<i16>, max_frame_count: i32) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 2);
        push_i32(&mut bytes, max_frame_count);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, code_words.len() as i32);
        for word in &code_words {
            push_i16(&mut bytes, *word);
        }
        if code_words.len() % 2 == 1 {
            push_i16(&mut bytes, 0);
        }
        push_i32(&mut bytes, (data_slots.len() / 2) as i32);
        for word in data_slots {
            push_i16(&mut bytes, word);
        }
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        bytes
    }

    fn push_utf16_string(bytes: &mut Vec<u8>, text: &str) {
        let units = text.encode_utf16().collect::<Vec<_>>();
        push_i32(bytes, units.len() as i32);
        for unit in &units {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        if units.len() % 2 == 1 {
            push_i16(bytes, 0);
        }
    }

    fn push_i32(bytes: &mut Vec<u8>, value: i32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i16(bytes: &mut Vec<u8>, value: i16) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn write_png(path: PathBuf, width: u32, height: u32, rgba: &[u8]) {
        let image = image::RgbaImage::from_raw(width, height, rgba.to_vec()).expect("rgba image");
        image.save(path).expect("write png");
    }

    /// 8-bit grayscale PNG: the sample is the province value.
    fn write_province_png(path: PathBuf, width: u32, height: u32, samples: &[u8]) {
        let file = std::fs::File::create(path).expect("create province png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(samples).expect("png samples");
    }

    /// 8-bit palette PNG: the province value is the palette index, not the
    /// palette colour.
    fn write_province_palette_png(path: PathBuf, width: u32, height: u32, indices: &[u8]) {
        let file = std::fs::File::create(path).expect("create province palette png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        let palette: Vec<u8> = (0..=255u8).flat_map(|value| [value, value, value]).collect();
        encoder.set_palette(palette);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(indices).expect("png indices");
    }

    /// The object a global holds, for the tests' own readers: a script value
    /// stored from `this` or `new` is self-bound (`Variant::Closure`), so
    /// unwrap the binding the way the engine's readers do.
    fn object_handle(engine: &KrkrEngine, name: &str) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    /// The object a member holds, for the tests' own readers.
    fn member_object(
        engine: &KrkrEngine,
        object: ObjectHandle,
        name: &str,
    ) -> ObjectHandle {
        engine
            .tjs_runtime()
            .object_member(object, name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    fn image_command_count(frame: &EngineFrame) -> usize {
        frame
            .output
            .draw_commands
            .iter()
            .filter(|command| matches!(command, krkr_core::DrawCommand::Image(_)))
            .count()
    }

    /// Official `tTJSNI_BaseLayer` always owns a `MainImage`
    /// (`LayerIntf.cpp:404` `AllocateDefaultImage`), so a visible layer
    /// contributes an image command even when the script never drew into it.
    /// Count only commands whose texture has any non-transparent pixel, which
    /// is what a scene actually shows.
    fn content_image_command_count(engine: &KrkrEngine, frame: &EngineFrame) -> usize {
        let mut textures = std::collections::BTreeMap::new();
        for layer in engine.host().layer_tree().layers() {
            if let Some(image) = &layer.image {
                textures.insert(image.upload.texture_id, &image.upload.rgba);
            }
        }
        frame
            .output
            .draw_commands
            .iter()
            .filter(|command| match command {
                krkr_core::DrawCommand::Image(image) => textures
                    .get(&image.texture_id)
                    .is_some_and(|rgba| rgba.chunks_exact(4).any(|pixel| pixel[3] != 0)),
                _ => false,
            })
            .count()
    }

    fn image_test_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("storage");
        KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            kag_budget: KagRunBudget {
                max_tags_per_tick: 1000,
                max_wall_time: Duration::from_secs(1),
            },
            ..EngineConfig::default()
        })
        .expect("engine")
    }

    fn layer_rgba(engine: &KrkrEngine, layer_id: u64) -> Vec<u8> {
        engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .expect("layer image")
            .upload
            .rgba
            .to_vec()
    }

    /// `operateAffine`'s parameters are `src, sx, sy, sw, sh, affine, a, b, c,
    /// d, tx, ty, mode, opa, type` with a trailing deprecated `hda`
    /// (`LayerIntf.cpp:7490-7507`) and **no** clear argument; reading them one
    /// slot late made an explicit mode throw (`opa` was consumed as the mode)
    /// and let any non-zero `opa` clear the destination outside the quad.
    #[test]
    fn native_layer_operate_affine_takes_mode_and_opacity_from_slots_12_and_13() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(4, 4);
                source.fillRect(0, 0, 4, 4, 0xffff0000);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.face = 1; // dfOpaque
                dest.fillRect(0, 0, 4, 4, 0xff0000ff);
                // The quad covers the top-left 2x2 only, so the rest of the
                // destination shows whether the call cleared it.
                dest.operateAffine(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2, omOpaque, 255, 0);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        let rgba = layer_rgba(&engine, layer_id);
        let pixel = |x: usize, y: usize| &rgba[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(pixel(0, 0), [255, 0, 0, 255], "omOpaque blits the source");
        assert_eq!(pixel(1, 1), [255, 0, 0, 255], "inside the quad");
        assert_eq!(
            pixel(3, 3),
            [0, 0, 255, 255],
            "outside the quad keeps the destination: opa is not a clear flag"
        );
    }

    /// The opacity of `operateAffine` is `param[13]` and the stretch type
    /// `param[14]` (`LayerIntf.cpp:7490-7496`).
    #[test]
    fn native_layer_operate_affine_reads_opacity_and_stretch_type_slots() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(4, 4);
                source.fillRect(0, 0, 4, 4, 0xffff0000);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.face = 1; // dfOpaque
                dest.fillRect(0, 0, 4, 4, 0xffffffff);
                dest.operateAffine(source, 0, 0, 4, 4, false, 0, 0, 4, 0, 0, 4, omAlpha, 128, 0);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        let rgba = layer_rgba(&engine, layer_id);
        // `bmAlpha` at opa 128 over opaque white: 0x00ff8080.
        assert_eq!(&rgba[..4], [255, 128, 128, 0]);

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(2, 1);
                source.fillRect(0, 0, 1, 1, 0xffff0000);
                source.fillRect(1, 0, 1, 1, 0xff0000ff);

                global.dest = new Layer();
                dest.setImageSize(4, 1);
                dest.face = 1; // dfOpaque
                dest.fillRect(0, 0, 4, 1, 0xff00ff00);
                // type = stLinear (2) in slot 14 interpolates; reading slot 15
                // would fall back to nearest.
                dest.operateAffine(source, 0, 0, 2, 1, false, 0, 0, 4, 0, 0, 1, omOpaque, 255, 2);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        let rgba = layer_rgba(&engine, layer_id);
        assert_eq!(
            &rgba[..4],
            [255, 0, 0, 255],
            "first column stays source red"
        );
        assert_eq!(
            &rgba[12..16],
            [0, 0, 255, 255],
            "last column stays source blue"
        );
        let mixed = (1..3).any(|x| {
            let p = &rgba[x * 4..x * 4 + 4];
            p[0] > 0 && p[2] > 0
        });
        assert!(mixed, "stLinear blends the interior columns: {rgba:?}");
    }

    /// `bmCopyOnAddAlpha`/`bmAddAlpha`/`bmAddAlphaOnAddAlpha`
    /// (`LayerBitmapIntf.cpp:1360-1415`): the `dfAddAlpha` family is
    /// premultiplied, so the destination is scaled by `255 - src_alpha` and the
    /// raw source is added instead of the straight-alpha lerp.
    #[test]
    fn native_layer_add_alpha_face_uses_the_premultiplied_blends() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(4, 4);
                source.fillRect(0, 0, 4, 4, 0xffff0000);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.face = 1; // dfOpaque
                dest.fillRect(0, 0, 4, 4, 0xffffffff);
                // omAddAlpha on dfOpaque is bmAddAlpha: an opaque source
                // replaces the destination outright.
                dest.operateRect(0, 0, source, 0, 0, 4, 4, omAddAlpha, 255);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        assert_eq!(
            &layer_rgba(&engine, layer_id)[..4],
            [255, 0, 0, 255],
            "TVPAdditiveAlphaBlend replaces an opaque destination"
        );

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(4, 4);
                source.fillRect(0, 0, 4, 4, 0xffff0000);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.face = 1; // dfOpaque
                dest.fillRect(0, 0, 4, 4, 0xffffffff);
                dest.operateRect(0, 0, source, 0, 0, 4, 4, omAddAlpha, 128);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        assert_eq!(
            &layer_rgba(&engine, layer_id)[..4],
            [254, 127, 127, 127],
            "TVPAdditiveAlphaBlend_o scales both source and destination"
        );

        // A premultiplied destination: omAlpha is `bmAlphaOnAddAlpha`
        // (TVPAlphaBlend_a/_ao), omAddAlpha is `bmAddAlphaOnAddAlpha`.
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(4, 4);
                source.fillRect(0, 0, 4, 4, 0x80ff0000);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.face = 4; // dfAddAlpha
                dest.fillRect(0, 0, 4, 4, 0x80808080);
                dest.operateRect(0, 0, source, 0, 0, 4, 4, omAlpha, 128);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        assert_eq!(
            &layer_rgba(&engine, layer_id)[..4],
            [158, 95, 95, 160],
            "bmAlphaOnAddAlpha premultiplies the source"
        );

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(4, 4);
                source.fillRect(0, 0, 4, 4, 0x80ff0000);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.face = 4; // dfAddAlpha
                dest.fillRect(0, 0, 4, 4, 0x80808080);
                dest.operateRect(0, 0, source, 0, 0, 4, 4, omAddAlpha, 255);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        assert_eq!(
            &layer_rgba(&engine, layer_id)[..4],
            [255, 63, 63, 192],
            "bmAddAlphaOnAddAlpha adds the source into the scaled destination"
        );
    }

    /// `bmAddAlphaOnAlpha` is the reference's "Not yet implemented" no-op
    /// (`LayerBitmapIntf.cpp:1383-1386`): `omAddAlpha` onto a plain alpha face
    /// changes nothing.
    #[test]
    fn native_layer_add_alpha_on_alpha_is_a_no_op() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(4, 4);
                source.fillRect(0, 0, 4, 4, 0x80ff0000);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.fillRect(0, 0, 4, 4, 0xff204080);
                dest.operateRect(0, 0, source, 0, 0, 4, 4, omAddAlpha, 255);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        assert_eq!(&layer_rgba(&engine, layer_id)[..4], [32, 64, 128, 255]);
    }

    /// The `nsa` family (`add/sub/mul/dodge/darken/lighten/screen`) never
    /// consults the source alpha (`DEFINE_BLEND_MIN_VARIATION`,
    /// `blend_functor_c.h:30-33`): a half-transparent source still blends its
    /// raw colour.
    #[test]
    fn native_layer_nsa_blends_ignore_the_source_alpha() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(4, 1);
                source.fillRect(0, 0, 1, 1, 0x00abcdef);
                source.fillRect(1, 0, 1, 1, 0x80ff0000);

                global.dest = new Layer();
                dest.setImageSize(4, 1);
                dest.face = 1; // dfOpaque
                dest.fillRect(0, 0, 4, 1, 0xff808080);
                dest.operateRect(0, 0, source, 0, 0, 1, 1, omAdditive, 255);
                dest.operateRect(1, 0, source, 1, 0, 1, 1, omScreen, 255);
                dest.operateRect(2, 0, source, 1, 0, 1, 1, omMultiplicative, 255);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        let rgba = layer_rgba(&engine, layer_id);
        assert_eq!(
            &rgba[0..4],
            [0xff, 0xff, 0xff, 0xff],
            "raw add of a fully transparent colour still adds"
        );
        assert_eq!(
            &rgba[4..8],
            [255, 129, 129, 255],
            "screen of the raw half-transparent colour"
        );
        assert_eq!(
            &rgba[8..12],
            [127, 0, 0, 0],
            "multiplicative of the raw colour, alpha byte not blended"
        );
    }

    /// `FillColorOnAddAlpha` (`LayerIntf.cpp:3948-3958`) uses
    /// `TVPConstColorAlphaBlend_a` on a `dfAddAlpha` face and refuses a
    /// negative opacity.
    #[test]
    fn native_layer_color_rect_on_add_alpha_face() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.dest = new Layer();
                dest.setImageSize(2, 1);
                dest.face = 4; // dfAddAlpha
                dest.fillRect(0, 0, 2, 1, 0xff804020);
                dest.colorRect(0, 0, 1, 1, 0x00112233, 128);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        assert_eq!(&layer_rgba(&engine, layer_id)[..4], [88, 48, 23, 255]);

        // `FillColorOnAddAlpha` runs `BlendColor`, whose `opa == 0` case is a
        // no-op (`LayerBitmapIntf.cpp:408-420`): the pixel keeps its value.
        engine
            .execute_script("inline.tjs", "dest.colorRect(0, 0, 1, 1, 0x00112233, 0);")
            .expect("zero opacity");
        assert_eq!(&layer_rgba(&engine, layer_id)[..4], [88, 48, 23, 255]);

        let error = engine
            .execute_script("inline.tjs", "dest.colorRect(0, 0, 1, 1, 0x00112233, -1);")
            .expect_err("negative opacity is refused on dfAddAlpha");
        assert!(error.message.contains("Negative opacity"), "{error:?}");
    }

    /// The shipped x86 build installs the SSE2 `bmScreen` kernels
    /// (`TVPGL_SSE2_Init`, `blend_function_sse2.cpp:1364`), whose `_o` body
    /// carries the complement the pure-C fallback (`blend_functor_c.h:301`)
    /// omits: a faded screen layer lightens toward white instead of collapsing
    /// to near-black.
    #[test]
    fn native_layer_screen_with_opacity_uses_the_shipped_kernel() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(1, 1);
                source.fillRect(0, 0, 1, 1, 0x80ff0000);

                global.dest = new Layer();
                dest.setImageSize(1, 1);
                dest.face = 1; // dfOpaque
                dest.fillRect(0, 0, 1, 1, 0xff808080);
                dest.operateRect(0, 0, source, 0, 0, 1, 1, omScreen, 128);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        assert_eq!(
            &layer_rgba(&engine, layer_id)[..4],
            [0xc0, 0x81, 0x81, 0xff],
            "sse2_screen_blend_o_functor at opa 128"
        );
    }

    /// `CopyRect`'s `dfProvince` branch (`LayerIntf.cpp:4179-4195`) copies the
    /// source's province plane, and zero-fills the destination plane when the
    /// source has none.
    #[test]
    fn native_layer_copy_rect_on_a_province_face() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.setImageSize(4, 1);
                source.setProvincePixel(0, 0, 5);
                source.setProvincePixel(1, 0, 6);

                global.dest = new Layer();
                dest.setImageSize(4, 1);
                dest.face = 3; // dfProvince
                dest.setProvincePixel(0, 0, 9);
                dest.setProvincePixel(1, 0, 9);
                dest.copyRect(0, 0, source, 0, 0, 4, 1);
                "#,
            )
            .expect("script");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "dest.getProvincePixel(0, 0)")
                .expect("province pixel")
                .to_integer()
                .expect("integer"),
            5
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "dest.getProvincePixel(1, 0)")
                .expect("province pixel")
                .to_integer()
                .expect("integer"),
            6
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "dest.getProvincePixel(2, 0)")
                .expect("province pixel")
                .to_integer()
                .expect("integer"),
            0
        );

        // A source without a province plane zero-fills the destination rect.
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.plain = new Layer();
                plain.setImageSize(4, 1);
                dest.setProvincePixel(0, 0, 9);
                dest.setProvincePixel(1, 0, 9);
                dest.copyRect(0, 0, plain, 0, 0, 4, 1);
                "#,
            )
            .expect("script");
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "dest.getProvincePixel(0, 0) + ':' + dest.getProvincePixel(1, 0)"
                )
                .expect("province pixels")
                .to_tjs_string()
                .expect("string"),
            "0:0"
        );
    }

    /// Every other blit refuses a `dfProvince` face: `stretchCopy`,
    /// `affineCopy`, `operateStretch`, `operateAffine` and `operateRect` reach
    /// `GetBltMethodFromOperationModeAndDrawFace`, which has no province case
    /// (`LayerIntf.cpp:4260-4262`, `:4303-4305`, `:4411-4415`, `:4447-4452`,
    /// `:4370-4374`).
    #[test]
    fn native_layer_province_face_refuses_other_blits() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.setImageSize(2, 2);
                global.dest = new Layer();
                dest.setImageSize(2, 2);
                dest.face = 3; // dfProvince
                "#,
            )
            .expect("script");
        for call in [
            "dest.stretchCopy(0, 0, 2, 2, source, 0, 0, 2, 2, 0);",
            "dest.affineCopy(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2);",
            "dest.operateStretch(0, 0, 2, 2, source, 0, 0, 2, 2, omOpaque, 255);",
            "dest.operateAffine(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2, omOpaque);",
            "dest.operateRect(0, 0, source, 0, 0, 2, 2, omOpaque);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("province face has no blit method");
            assert!(
                error.message.contains("Not drawable face type"),
                "{call}: {error:?}"
            );
        }
        // `piledCopy` is not face-dispatched in the reference
        // (`LayerIntf.cpp:4102-4142`), so it still composes.
        engine
            .execute_script(
                "inline.tjs",
                "dest.face = 3; dest.piledCopy(0, 0, source, 0, 0, 2, 2);",
            )
            .expect("piledCopy runs on a province face");
        // The operate family has no face switch in the reference: it throws
        // only when `GetBltMethodFromOperationModeAndDrawFace` has no method
        // for the mode (`LayerIntf.cpp:4357-4393`), and the universal modes
        // resolve on every face. `omAlpha` is face-dependent, so it throws.
        let error = engine
            .execute_script(
                "inline.tjs",
                "dest.face = 3; dest.operateRect(0, 0, source, 0, 0, 2, 2, omAlpha, 255);",
            )
            .expect_err("dfProvince has no omAlpha method");
        assert!(
            error.message.contains("Not drawable face type"),
            "{error:?}"
        );
    }

    /// The operate family's universal modes blend into the main image on a
    /// `dfProvince` face (`LayerIntf.cpp:4357-4393`: the method lookup is the
    /// only face check those calls have).
    #[test]
    fn native_layer_operate_blends_into_a_province_layer_main_image() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.setImageSize(2, 2);
                source.fillRect(0, 0, 2, 2, 0xff404040);

                global.dest = new Layer();
                dest.setImageSize(2, 2);
                dest.fillRect(0, 0, 2, 2, 0xff808080);
                dest.face = 3; // dfProvince
                dest.operateRect(0, 0, source, 0, 0, 2, 2, omAdditive, 255);
                "#,
            )
            .expect("omAdditive resolves on a province face");
        let layer_id = engine
            .execute_expression("inline.tjs", "dest.__nativeLayerId")
            .expect("layer id")
            .to_integer()
            .expect("integer") as u64;
        assert_eq!(
            &layer_rgba(&engine, layer_id)[..4],
            [0xc0, 0xc0, 0xc0, 0xff],
            "the raw add lands in the layer's main image"
        );
    }

    /// `clipLeft`/`clipTop`/`clipWidth`/`clipHeight` are live views of the
    /// `ClipRect` (`LayerIntf.cpp:8927-9005`), which `SetClip` clamps to the
    /// image (`:3718-3732`).
    #[test]
    fn native_layer_clip_properties_are_live_and_clamped() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(8, 8);
                layer.setClip(2, 3, 4, 5);
                "#,
            )
            .expect("script");
        let read = |engine: &mut KrkrEngine, expression: &str| -> i64 {
            engine
                .execute_expression("inline.tjs", expression)
                .expect("expression")
                .to_integer()
                .expect("integer")
        };
        assert_eq!(read(&mut engine, "layer.clipLeft"), 2);
        assert_eq!(read(&mut engine, "layer.clipTop"), 3);
        assert_eq!(read(&mut engine, "layer.clipWidth"), 4);
        assert_eq!(read(&mut engine, "layer.clipHeight"), 5);

        // The property is the same rectangle the fills are clipped to.
        engine
            .execute_script(
                "inline.tjs",
                r#"
                layer.clipWidth = 1;
                layer.fillRect(0, 0, 8, 8, 0xff123456);
                "#,
            )
            .expect("script");
        let layer_id = read(&mut engine, "layer.__nativeLayerId") as u64;
        let rgba = layer_rgba(&engine, layer_id);
        let pixel = |x: usize, y: usize| &rgba[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
        assert_eq!(pixel(2, 3), [18, 52, 86, 255], "inside the clip");
        assert_eq!(
            pixel(3, 3),
            [255, 255, 255, 0],
            "outside the clip keeps the holder"
        );

        // Clamping: negative origins, image bounds and the right >= left rule.
        engine
            .execute_script(
                "inline.tjs",
                r#"
                layer.setClip(-5, -5, 10, 10);
                global.first = layer.clipLeft + "," + layer.clipTop + "," +
                    layer.clipWidth + "," + layer.clipHeight;
                layer.setClip(100, 0, 10, 10);
                global.second = layer.clipLeft + "," + layer.clipWidth + "," + layer.clipHeight;
                layer.setClip();
                global.third = layer.clipLeft + "," + layer.clipWidth;
                "#,
            )
            .expect("script");
        let text = |engine: &mut KrkrEngine, expression: &str| -> String {
            engine
                .execute_expression("inline.tjs", expression)
                .expect("expression")
                .to_tjs_string()
                .expect("string")
        };
        assert_eq!(text(&mut engine, "first"), "0,0,5,5");
        assert_eq!(text(&mut engine, "second"), "100,0,8");
        assert_eq!(text(&mut engine, "third"), "0,8", "setClip() resets");

        // Without an image the rectangle cannot be set, but it stays readable:
        // `DeallocateImage` does not touch `ClipRect`
        // (`LayerIntf.cpp:2079-2085`), so the layer keeps the `ResetClip`
        // rectangle it had (the 8x8 image).
        let error = engine
            .execute_script(
                "inline.tjs",
                "layer.freeImage(); layer.setClip(0, 0, 1, 1);",
            )
            .expect_err("no image");
        assert!(
            error.message.contains("Not drawable layer type"),
            "{error:?}"
        );
        assert_eq!(read(&mut engine, "layer.clipLeft"), 0);
        assert_eq!(read(&mut engine, "layer.clipWidth"), 8);
    }

    /// `setClip` takes no arguments (reset) or at least four; 1..3 is the
    /// reference's `TJS_E_BADPARAMCOUNT` (`LayerIntf.cpp:6997-7021`).
    #[test]
    fn native_layer_set_clip_rejects_partial_arguments() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 4);
                layer.setClip(1, 1, 2, 2);
                "#,
            )
            .expect("script");
        for call in [
            "layer.setClip(1);",
            "layer.setClip(1, 2);",
            "layer.setClip(1, 2, 3);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("partial setClip arguments are refused");
            assert!(
                error.message.contains("Invalid argument count"),
                "{call}: {error:?}"
            );
        }
        // The rejected calls left the rectangle alone.
        let width = engine
            .execute_expression("inline.tjs", "layer.clipWidth")
            .expect("clip width")
            .to_integer()
            .expect("integer");
        assert_eq!(width, 2);
    }

    /// `SetMainPixel`/`SetMaskPixel` require a main image
    /// (`LayerIntf.cpp:2594-2596`, `:2621`): a write after `freeImage` throws
    /// instead of fabricating a bitmap.
    #[test]
    fn native_layer_pixel_setters_require_an_image() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 4);
                layer.freeImage();
                global.hasImage = layer.hasImage;
                "#,
            )
            .expect("script");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "hasImage")
                .expect("expression")
                .to_integer()
                .expect("integer"),
            0
        );
        for call in [
            "layer.setMainPixel(0, 0, 0xff0000);",
            "layer.setMaskPixel(0, 0, 128);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("a freed bitmap cannot be written");
            assert!(
                error.message.contains("Not drawable layer type"),
                "{call}: {error:?}"
            );
        }
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "layer.hasImage")
                .expect("expression")
                .to_integer()
                .expect("integer"),
            0
        );
    }

    /// `SetType` (`LayerIntf.cpp:1446-1466`) allocates or frees the image with
    /// the type, and `SetHasImage` refuses one on `ltBinder`/`ltEffect`/
    /// `ltFilter` (`:2228-2235`).
    #[test]
    fn native_layer_type_change_allocates_and_frees_the_image() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 4);
                layer.type = 0; // ltBinder
                global.binder = layer.hasImage;
                layer.type = 2; // ltAlpha allocates again
                global.alpha = layer.hasImage;
                layer.type = 0;
                "#,
            )
            .expect("script");
        let read = |engine: &mut KrkrEngine, expression: &str| -> i64 {
            engine
                .execute_expression("inline.tjs", expression)
                .expect("expression")
                .to_integer()
                .expect("integer")
        };
        assert_eq!(read(&mut engine, "binder"), 0);
        assert_eq!(read(&mut engine, "alpha"), 1);
        let error = engine
            .execute_script("inline.tjs", "layer.hasImage = 1;")
            .expect_err("ltBinder cannot have an image");
        assert!(error.message.contains("cannot have image"), "{error:?}");
    }

    /// `InternalAffineBlt` throws `TVPOutOfRectangle` when the source
    /// rectangle leaves the source bitmap (`LayerBitmapIntf.cpp:2704-2709`);
    /// a stretch with a type at or above `stLinear` goes to the resampler and
    /// only clips.
    #[test]
    fn native_layer_blits_validate_the_source_rectangle() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.setImageSize(2, 2);
                global.dest = new Layer();
                dest.setImageSize(2, 2);
                "#,
            )
            .expect("script");
        for call in [
            "dest.affineCopy(source, 1, 0, 2, 2, false, 0, 0, 2, 0, 0, 2);",
            "dest.operateAffine(source, 1, 0, 2, 2, false, 0, 0, 2, 0, 0, 2, omOpaque);",
            "dest.stretchCopy(0, 0, 2, 2, source, 1, 1, 2, 2, 0);",
            "dest.operateStretch(0, 0, 2, 2, source, 1, 1, 2, 2, omOpaque, 255, 0);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("source rectangle leaves the bitmap");
            assert!(
                error.message.contains("Out of rectangle"),
                "{call}: {error:?}"
            );
        }
        engine
            .execute_script(
                "inline.tjs",
                "dest.stretchCopy(0, 0, 2, 2, source, 1, 1, 2, 2, 2);",
            )
            .expect("a resampled stretch clips instead of throwing");
    }

    /// A negative destination extent is the reference's mirrored blit: the
    /// affine path receives a rectangle whose right/bottom edge precedes its
    /// left/top one (`LayerBitmapIntf.cpp:1867-1875`).
    #[test]
    fn native_layer_negative_stretch_extents_mirror() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer_id = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(2, 1);
                source.fillRect(0, 0, 1, 1, 0xffff0000);
                source.fillRect(1, 0, 1, 1, 0xff0000ff);

                global.dest = new Layer();
                dest.setImageSize(2, 1);
                dest.face = 1; // dfOpaque
                dest.fillRect(0, 0, 2, 1, 0xff00ff00);
                dest.stretchCopy(2, 0, -2, 1, source, 0, 0, 2, 1, 0);
                return dest.__nativeLayerId;
                "#,
            )
            .expect("script")
            .to_integer()
            .expect("layer id") as u64;
        let rgba = layer_rgba(&engine, layer_id);
        assert_eq!(&rgba[0..4], [0, 0, 255, 255], "mirrored: blue first");
        assert_eq!(&rgba[4..8], [255, 0, 0, 255], "mirrored: red last");
     }
}
