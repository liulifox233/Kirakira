use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

use krkr_core::{
    AudioBus, AudioCommand, AudioInstanceId, AudioLoadPolicy, ButtonState, Color,
    Engine as CoreEngine, EngineConfig as CoreEngineConfig, EngineEvent, EngineKey, FrameInput,
    FrameOutput, ImageUpload, LayerId, MessageLayerModel, Point, PointerButton, Size,
    TransitionMethod, TransitionParams, TransitionScrollFrom, TransitionScrollStay,
};
use krkr_kag::{KagError, KagParser, ParserSnapshot, Tag};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::{
    globals::install_tvp_globals,
    host::{ImageLoadRequest, ImageLoadState, ImageLoadTarget, KrkrHost},
    kag::{EngineKagHost, tag_to_dictionary},
    native::classes::{
        apply_completed_image_load, apply_completed_resource_loads, call_wave_status_changed,
        complete_layer_before_draw, complete_pending_layer_paints,
        finish_completed_native_transitions, register_kag_layer_slots_from_tjs,
    },
    native::{create_kag_parser_object, refresh_kag_parser_object},
    plugin::KrkrPlugin,
    scheduler::{
        ASYNC_TRIGGER_EVENT_NAME, AUDIO_FADE_COMPLETED_EVENT_NAME, AsyncTriggerMode, IdleEvent,
        ScriptEvent, ScriptEventKind, ScriptEventSelection, TIMER_EVENT_NAME,
    },
    script::{
        execute_bytecode_if_present_on_runtime, execute_expression_on_runtime,
        execute_script_on_runtime,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KagRunBudget {
    pub max_tags_per_tick: usize,
    pub max_wall_time: Duration,
}

impl Default for KagRunBudget {
    fn default() -> Self {
        Self {
            max_tags_per_tick: 1000,
            max_wall_time: Duration::from_millis(16),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EngineConfig {
    pub project_root: Option<PathBuf>,
    pub kag_budget: KagRunBudget,
    pub system_metrics: SystemMetrics,
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
}

impl EngineInput {
    pub fn new(frame: FrameInput, events: Vec<EngineEvent>) -> Self {
        Self { frame, events }
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

pub struct KrkrEngine {
    tjs_runtime: Runtime<KrkrHost>,
    kag_session: KagSession,
    core_engine: CoreEngine,
    kag_budget: KagRunBudget,
    plugins: Vec<Box<dyn KrkrPlugin>>,
    cursor_position: Option<Point>,
    hovered_layer: Option<LayerId>,
    pressed_layer: Option<LayerId>,
    captured_layer: Option<LayerId>,
    input_result: EngineInputResult,
}

impl KrkrEngine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let host = match config.project_root {
            Some(root) => KrkrHost::for_project(root)?,
            None => KrkrHost::default(),
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
            hovered_layer: None,
            pressed_layer: None,
            captured_layer: None,
            input_result: EngineInputResult::default(),
        })
    }

    pub fn for_project(root: impl Into<PathBuf>) -> Result<Self> {
        Self::new(EngineConfig {
            project_root: Some(root.into()),
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

    pub fn preferred_viewport_size(&self) -> Option<Size> {
        let window = self.runtime_window_object()?;
        let width = object_positive_i64(&self.tjs_runtime, window, "innerWidth")
            .or_else(|| object_positive_i64(&self.tjs_runtime, window, "width"))?;
        let height = object_positive_i64(&self.tjs_runtime, window, "innerHeight")
            .or_else(|| object_positive_i64(&self.tjs_runtime, window, "height"))?;
        Some(Size::new(width as f32, height as f32))
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
        let Variant::Object(scflags) = self.tjs_runtime.object_member(window, "scflags") else {
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
        let Variant::Object(bgm) = self.tjs_runtime.object_member(window, "bgm") else {
            return;
        };
        let Variant::Object(buffer) = self.tjs_runtime.object_member(bgm, "currentBuffer") else {
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
        let Variant::Object(se) = self.tjs_runtime.object_member(window, "se") else {
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
            let Variant::Object(buffer) = self.tjs_runtime.object_member(se, &index.to_string())
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
        let Variant::Object(sflags) = self.tjs_runtime.object_member(window, "sflags") else {
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
        let Variant::Object(bgm) = self.tjs_runtime.object_member(window, "bgm") else {
            return None;
        };
        let Variant::Object(buffer) = self.tjs_runtime.object_member(bgm, "currentBuffer") else {
            return None;
        };
        self.tjs_runtime
            .host()
            .native_audio_buffer(buffer)
            .map(|buffer| buffer.volume2)
    }

    fn kag_se_volume2(&self, window: ObjectHandle, index: usize) -> Option<i64> {
        let Variant::Object(se) = self.tjs_runtime.object_member(window, "se") else {
            return None;
        };
        let Variant::Object(buffer) = self.tjs_runtime.object_member(se, &index.to_string()) else {
            return None;
        };
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
        let bytes = self.tjs_runtime.host().read_binary_storage(name)?;
        if let Some(result) =
            execute_bytecode_if_present_on_runtime(&mut self.tjs_runtime, name, &bytes)?
        {
            return self.sync_kag_slots_after_ok(Ok(result));
        }
        let source = self.tjs_runtime.host().read_text_storage(name)?;
        let result = execute_script_on_runtime(&mut self.tjs_runtime, name, &source);
        self.sync_kag_slots_after_ok(result)
    }

    pub fn eval_storage(&mut self, name: &str) -> Result<Variant> {
        let bytes = self.tjs_runtime.host().read_binary_storage(name)?;
        if let Some(result) =
            execute_bytecode_if_present_on_runtime(&mut self.tjs_runtime, name, &bytes)?
        {
            return self.sync_kag_slots_after_ok(Ok(result));
        }
        let source = self.tjs_runtime.host().read_text_storage(name)?;
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

    pub fn has_kag_scenario(&self) -> bool {
        self.kag_session.loaded()
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

        self.tjs_runtime
            .set_object_member(handle, "status", Variant::String("stop".to_string()));
        self.tjs_runtime
            .set_object_member(handle, "paused", Variant::Integer(0));
        let result = call_wave_status_changed(&mut self.tjs_runtime, handle);
        self.sync_kag_slots_after_ok(result)
    }

    pub fn tick(&mut self) -> Result<EngineTickResult> {
        self.advance(Duration::ZERO)
    }

    pub fn update(&mut self, input: EngineInput, delta: Duration) -> Result<EngineFrame> {
        self.input_result = EngineInputResult::default();
        {
            let scheduler = self.tjs_runtime.host_mut().scheduler_mut();
            scheduler.begin_frame();
            for event in input.events.iter().copied() {
                scheduler.post_input_event(event);
            }
        }
        self.pump_runtime_scheduler(RuntimeSchedulerPump::Full)?;
        self.resume_modal_call_if_ready()?;
        let tick = self.advance(delta)?;
        self.pump_runtime_scheduler(RuntimeSchedulerPump::WindowUpdatesOnly)?;
        complete_pending_layer_paints(&mut self.tjs_runtime)?;
        self.tjs_runtime
            .host_mut()
            .reapply_transition_live_layer_overrides();
        let suppressed_images = self.tjs_runtime.host().suppressed_transition_live_images();
        let transition = self.tjs_runtime.host().frame_transition();
        let output = self
            .core_engine
            .tick_running_with_layers_suppressing_images_and_transition(
                input.frame,
                self.tjs_runtime.host().layer_tree(),
                self.kag_session.message_layer(),
                &suppressed_images,
                transition,
            );
        let frame = EngineFrame {
            output,
            tick,
            input: self.input_result,
            message_layer: self.kag_session.message_layer().clone(),
            location: self.kag_location(),
        };
        Ok(frame)
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
        Ok(())
    }

    fn modal_window_is_closed(&self, window: ObjectHandle) -> bool {
        !self.tjs_runtime.object_valid(window)
            || self.tjs_runtime.host().native_window_closed(window)
            || !self
                .tjs_runtime
                .host()
                .native_window_property(window, "visible")
                .unwrap_or_else(|| self.tjs_runtime.object_member(window, "visible"))
                .is_truthy()
    }

    fn advance(&mut self, delta: Duration) -> Result<EngineTickResult> {
        apply_completed_resource_loads(&mut self.tjs_runtime)?;
        self.tjs_runtime.host_mut().advance_transition(delta);
        finish_completed_native_transitions(&mut self.tjs_runtime)?;
        let transition_active = self.tjs_runtime.host().has_active_transition();
        let resource_pending = self.tjs_runtime.host().has_pending_resource_loads();
        self.kag_session
            .update_wait(delta, transition_active, resource_pending);
        self.kag_session
            .run_until_yield(&mut self.tjs_runtime, self.kag_budget)
    }

    fn runtime_window_object(&self) -> Option<ObjectHandle> {
        if let Some(window) = self.active_modal_window() {
            return Some(window);
        }

        if let Variant::Object(kag) = self.tjs_runtime.global_member("kag")
            && has_window_state_member(&self.tjs_runtime, kag)
        {
            return Some(kag);
        }

        let Variant::Object(window_class) = self.tjs_runtime.global_member("Window") else {
            return None;
        };
        let Variant::Object(window) = self.tjs_runtime.object_member(window_class, "mainWindow")
        else {
            return None;
        };
        Some(window)
    }

    fn active_modal_window(&self) -> Option<ObjectHandle> {
        let window = self.tjs_runtime.host().current_modal_window()?;
        (!self.modal_window_is_closed(window)).then_some(window)
    }

    fn pump_runtime_scheduler(&mut self, mode: RuntimeSchedulerPump) -> Result<()> {
        const MAX_NATIVE_EVENT_PASSES: usize = 1024;

        if matches!(mode, RuntimeSchedulerPump::Full) {
            self.sync_scheduler_event_disabled();
        }

        if self.tjs_runtime.is_suspended() {
            if matches!(mode, RuntimeSchedulerPump::Full) {
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

            let delivered_window_update = self.deliver_window_update_events()?;
            if delivered_window_update && self.tjs_runtime.is_suspended() {
                return Ok(());
            }

            if delivered_script_or_input || delivered_window_update {
                continue;
            }

            if matches!(mode, RuntimeSchedulerPump::Full) && self.deliver_idle_scheduler_event()? {
                if self.tjs_runtime.is_suspended() {
                    return Ok(());
                }
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
        self.tjs_runtime
            .host_mut()
            .scheduler_mut()
            .begin_script_delivery_turn();

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

        while let Some(event) = self
            .tjs_runtime
            .host_mut()
            .scheduler_mut()
            .pop_input_event()
        {
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

        Ok(delivered)
    }

    fn sync_scheduler_event_disabled(&mut self) {
        let disabled = match self.tjs_runtime.global_member("System") {
            Variant::Object(system) => self
                .tjs_runtime
                .object_member(system, "eventDisabled")
                .is_truthy(),
            _ => false,
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
                    .set_timer_next_fire_millis(handle, None);
                continue;
            }

            let interval = self
                .tjs_runtime
                .object_member(handle, "interval")
                .to_integer()?
                .max(0);
            // TVP uses a zero Timer interval as an idle continuation: it
            // must run again after the current event turn, not be disabled.
            // In particular, KAG's conductor returns -2 after a tag that
            // yields and sets its timer interval to zero so it can parse the
            // next tag on the following turn.  Treat it as one logical
            // millisecond here.  This both preserves that continuation
            // contract and keeps a zero-interval timer from re-entering
            // endlessly in the same scheduler pump.
            let interval = interval.max(1);

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
                .unwrap_or(1);
            let capacity = if capacity == 0 {
                usize::MAX
            } else {
                capacity.max(1) as usize
            };
            let queued = self.tjs_runtime.host().scheduler().count_script_events(
                handle,
                handle,
                TIMER_EVENT_NAME,
                0,
            );
            if queued < capacity {
                let scheduler = self.tjs_runtime.host_mut().scheduler_mut();
                let tag = scheduler.next_timer_tag(handle);
                scheduler.post_timer_event(handle, tag);
            }
            self.tjs_runtime
                .host_mut()
                .scheduler_mut()
                .set_timer_next_fire_millis(handle, None);
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
            complete_layer_before_draw(&mut self.tjs_runtime, layer)?;
        }
        Ok(())
    }

    fn deliver_idle_scheduler_event(&mut self) -> Result<bool> {
        let Some(event) = self.tjs_runtime.host_mut().scheduler_mut().pop_idle_event() else {
            return Ok(false);
        };

        match event {
            IdleEvent::AsyncTrigger(handle) => {
                self.tjs_runtime.host_mut().scheduler_mut().trigger_async(
                    handle,
                    AsyncTriggerMode::Normal,
                    false,
                );
            }
            IdleEvent::ContinuousHandlers(handlers) => {
                let tick = self.tjs_runtime.host_mut().now_millis();
                for handler in handlers {
                    if matches!(handler, Variant::Void) {
                        continue;
                    }
                    self.tjs_runtime
                        .call_function(handler.clone(), vec![Variant::Integer(tick)])
                        .map_err(|error| {
                            TjsError::runtime(format!(
                                "continuous handler {handler:?} failed: {error}"
                            ))
                        })?;
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
            .map_err(|error| {
                TjsError::runtime(format!(
                    "{:?} event `{method}` on object#{} failed: {error}",
                    event.kind, event.target.0
                ))
            })
            .map(|_| ());
        self.sync_kag_slots_after_ok(result)
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
                    self.dispatch_layer_cursor_move(*position)?;
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Pressed,
                } => {
                    let modal_active = self.active_modal_window().is_some();
                    self.dispatch_window_pointer_event("onMouseDown", 0)?;
                    let raw_target = self.layer_at_cursor()?;
                    let handled_by_script = raw_target.is_some_and(|layer_id| {
                        self.layer_has_script_handler(layer_id, "onMouseDown")
                    });
                    self.dispatch_layer_pointer_event("onMouseDown", 0, raw_target)?;
                    self.pressed_layer = raw_target;
                    self.captured_layer = raw_target;
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
                    self.dispatch_layer_pointer_event(
                        "onMouseUp",
                        0,
                        self.captured_layer.or(release_hit),
                    )?;
                    if let (Some(pressed), Some(release_hit)) = (self.pressed_layer, release_hit)
                        && pressed == release_hit
                    {
                        self.dispatch_window_click()?;
                        self.dispatch_layer_click(pressed)?;
                    }
                    self.pressed_layer = None;
                    self.captured_layer = None;
                    if !modal_active {
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
                    self.captured_layer = raw_target;
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
                    self.dispatch_layer_pointer_event(
                        "onMouseUp",
                        1,
                        self.captured_layer.or(release_hit),
                    )?;
                    self.captured_layer = None;
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
                EngineEvent::PointerInput { .. } => {}
            }
        }
        Ok(())
    }

    fn update_input_key_state(&mut self, event: &EngineEvent) {
        let key = match event {
            EngineEvent::KeyboardInput { key, .. } => engine_key_vk_code(*key),
            EngineEvent::PointerInput { button, .. } => pointer_button_vk_code(*button),
            EngineEvent::CursorMoved { .. } | EngineEvent::MouseWheel { .. } => None,
        };
        let state = match event {
            EngineEvent::KeyboardInput { state, .. } | EngineEvent::PointerInput { state, .. } => {
                *state
            }
            EngineEvent::CursorMoved { .. } | EngineEvent::MouseWheel { .. } => return,
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
        let focused_layer = match self.tjs_runtime.host().native_window_focused_layer(window) {
            Some(layer) => layer,
            None => {
                let Variant::Object(focused_layer) =
                    self.tjs_runtime.object_member(window, "focusedLayer")
                else {
                    return Ok(false);
                };
                self.tjs_runtime
                    .bound_this(focused_layer)
                    .unwrap_or(focused_layer)
            }
        };
        if !self.tjs_runtime.object_valid(focused_layer) {
            return Ok(false);
        }
        let handler = self.tjs_runtime.object_member(focused_layer, method);
        if matches!(handler, Variant::Void) {
            return Ok(false);
        }
        let handled_by_script = !self.tjs_runtime.variant_is_native_function(&handler);
        self.tjs_runtime
            .call_object_method(
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
        self.tjs_runtime
            .call_object_method(
                window,
                method,
                vec![Variant::Integer(key_code), Variant::Integer(shift)],
            )
            .map(|_| handled_by_script)
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
        self.tjs_runtime
            .call_object_method(
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
        self.tjs_runtime
            .call_object_method(
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
        self.tjs_runtime
            .call_object_method(
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
            None => {
                self.script_pointer_layer_at_position(position, pointer_event_methods(method))?
            }
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
        self.tjs_runtime
            .call_object_method(
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
        let Variant::Object(kag) = self.tjs_runtime.global_member("kag") else {
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
        self.tjs_runtime
            .call_object_method(kag, method, Vec::new())
            .map(|_| ())
    }

    fn fire_kag_secondary_click(&mut self) -> Result<()> {
        if let Variant::Object(kag) = self.tjs_runtime.global_member("kag")
            && !matches!(
                self.tjs_runtime.object_member(kag, "onPrimaryRightClick"),
                Variant::Void
            )
        {
            return self
                .tjs_runtime
                .call_object_method(kag, "onPrimaryRightClick", Vec::new())
                .map(|_| ());
        }

        self.kag_session.fire_right_click(&mut self.tjs_runtime)
    }

    fn dispatch_layer_cursor_move(&mut self, position: Point) -> Result<()> {
        let shift = self.current_shift_state(false);
        if let Some(captured_layer) = self.captured_layer
            && let Some((x, y)) = self.layer_local_point(captured_layer, position)
        {
            self.call_layer_event(
                captured_layer,
                "onMouseMove",
                vec![
                    Variant::Integer(x),
                    Variant::Integer(y),
                    Variant::Integer(shift),
                ],
            )?;
            return Ok(());
        }

        let hit_layer = self.script_pointer_layer_at_position(
            position,
            &["onMouseMove", "onMouseEnter", "onMouseLeave"],
        )?;
        if hit_layer != self.hovered_layer {
            if let Some(layer_id) = self.hovered_layer {
                self.call_layer_event(layer_id, "onMouseLeave", Vec::new())?;
            }
            self.hovered_layer = hit_layer;
            if let Some(layer_id) = hit_layer {
                self.call_layer_event(layer_id, "onMouseEnter", Vec::new())?;
            }
        }

        if let Some(layer_id) = hit_layer
            && let Some((x, y)) = self.layer_local_point(layer_id, position)
        {
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

    fn dispatch_layer_mouse_wheel(&mut self, delta: i32) -> Result<()> {
        let Some(position) = self.cursor_position else {
            return Ok(());
        };
        let Some(window) = self.runtime_window_object() else {
            return Ok(());
        };
        let focused_layer = match self.tjs_runtime.host().native_window_focused_layer(window) {
            Some(layer) => layer,
            None => {
                let Variant::Object(focused_layer) =
                    self.tjs_runtime.object_member(window, "focusedLayer")
                else {
                    return Ok(());
                };
                self.tjs_runtime
                    .bound_this(focused_layer)
                    .unwrap_or(focused_layer)
            }
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
        self.tjs_runtime
            .call_object_method(
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

    fn script_pointer_layer_at_position(
        &mut self,
        position: Point,
        methods: &[&str],
    ) -> Result<Option<LayerId>> {
        for layer_id in self.hit_tested_layers_at_position(position)? {
            if !methods
                .iter()
                .any(|method| self.layer_has_script_handler(layer_id, method))
            {
                continue;
            }
            return Ok(Some(layer_id));
        }
        Ok(None)
    }

    fn layer_at_position(&mut self, position: Point) -> Result<Option<LayerId>> {
        Ok(self
            .hit_tested_layers_at_position(position)?
            .into_iter()
            .next())
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
            .or_else(|| match self.tjs_runtime.object_member(object, "window") {
                Variant::Object(layer_window) => Some(
                    self.tjs_runtime
                        .bound_this(layer_window)
                        .unwrap_or(layer_window),
                ),
                _ => None,
            })
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
        if !matches!(
            self.tjs_runtime.object_member(object, "onHitTest"),
            Variant::Void
        ) {
            self.tjs_runtime.call_object_method(
                object,
                "onHitTest",
                vec![
                    Variant::Integer(x),
                    Variant::Integer(y),
                    Variant::Integer(1),
                ],
            )?;
        }
        Ok(self
            .tjs_runtime
            .object_member(object, "__nativeHitTestWork")
            .is_truthy())
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
        if matches!(
            self.tjs_runtime.object_member(object, method),
            Variant::Void
        ) {
            return Ok(());
        }
        let result = self
            .tjs_runtime
            .call_object_method(object, method, args)
            .map(|_| ());
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
        self.tjs_runtime.host_mut().register_plugin(plugin.name());
        self.plugins.push(Box::new(plugin));
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

#[derive(Clone, Debug)]
struct KagSession {
    parser: Option<ObjectHandle>,
    parser_revision: u64,
    state: KagTaskState,
    handler: Option<ObjectHandle>,
    pending_tags: VecDeque<Tag>,
    temp_snapshots: BTreeMap<i64, KagTempSnapshot>,
    right_click: RightClickAction,
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
            let result = f(&mut parser, self, runtime, handle);
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
            let result = f(&mut parser, self, runtime, handle);
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
            let started = Instant::now();
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
        let started = Instant::now();
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
            let action = self.process_tag(parser, runtime, owner, tag)?;
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
        if let Some(action) = self.try_tjs_tag_handler(runtime, owner, &tag)? {
            match action {
                TjsTagAction::Handled(action) => Ok(action),
                TjsTagAction::NativeFallback => {
                    self.process_native_fallback_tag(parser, runtime, &tag)
                }
            }
        } else {
            self.process_native_fallback_tag(parser, runtime, &tag)
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

        if let Variant::Object(kag) = runtime.global_member("kag") {
            if let Variant::Object(conductor) = runtime.object_member(kag, "conductor") {
                push_unique_handler(&mut candidates, Some(conductor));
            }
            if let Variant::Object(conductor) = runtime.object_member(kag, "mainConductor") {
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
            self.state = KagTaskState::Error {
                message: error.to_string(),
            };
        })
    }

    fn process_native_fallback_tag(
        &mut self,
        parser: &mut KagParser,
        runtime: &mut Runtime<KrkrHost>,
        tag: &Tag,
    ) -> Result<TagAction> {
        let Some(native_tag) = NativeFallbackTag::from_name(&tag.tagname) else {
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
                let duration = tag_millis(tag, "time").unwrap_or(Duration::ZERO);
                let (params, rule_image_upload) = kag_transition_spec(runtime, tag)?;
                runtime
                    .host_mut()
                    .begin_kag_transition(duration, params, rule_image_upload);
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
            self.state = KagTaskState::Error {
                message: format!("KAG handler returned void for tag `{}`", tag.tagname),
            };
            return Err(TjsError::runtime(match &self.state {
                KagTaskState::Error { message } => message.clone(),
                _ => unreachable!(),
            }));
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
    ClearText,
    Image,
    LayerOptions,
    FreeImage,
    Backlay,
    Rclick,
    TempSave,
    TempLoad,
    Noop,
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
            "cm" | "ct" | "er" => Self::ClearText,
            "image" => Self::Image,
            "layopt" | "position" => Self::LayerOptions,
            "freeimage" => Self::FreeImage,
            "backlay" => Self::Backlay,
            "rclick" => Self::Rclick,
            "tempsave" => Self::TempSave,
            "tempload" => Self::TempLoad,
            "commit" | "history" | "defstyle" | "resetstyle" | "ruby" => Self::Noop,
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
    runtime.call_object_method(handler, name, args)
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
    let Variant::Object(kag) = runtime.global_member("kag") else {
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
    let Variant::Object(kag) = runtime.global_member("kag") else {
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
    let Variant::Object(kag) = runtime.global_member("kag") else {
        return;
    };
    runtime.set_object_member(kag, "autoMode", Variant::Integer(0));
}

fn kag_transition_spec(
    runtime: &mut Runtime<KrkrHost>,
    tag: &Tag,
) -> Result<(TransitionParams, Option<ImageUpload>)> {
    let method = tag
        .literal_attr("method")
        .or_else(|| tag.literal_attr("rule").map(|_| "universal"))
        .unwrap_or("crossfade")
        .to_ascii_lowercase();
    let mut params = TransitionParams {
        method: TransitionMethod::from_name(&method),
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

fn kag_transition_color_attr(tag: &Tag, name: &str) -> Result<Option<Color>> {
    let Some([r, g, b, _]) = parse_color_attr(tag, name)? else {
        return Ok(None);
    };
    Ok(Some(Color::rgb_u8(r, g, b)))
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

fn pointer_event_methods(method: &str) -> &'static [&'static str] {
    match method {
        "onMouseDown" => &["onMouseDown"],
        "onMouseUp" => &["onMouseUp"],
        "onMouseMove" => &["onMouseMove", "onMouseEnter", "onMouseLeave"],
        _ => &[],
    }
}

fn install_system_metrics(runtime: &mut Runtime<KrkrHost>, metrics: SystemMetrics) {
    let Variant::Object(system) = runtime.global_member("System") else {
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
    if let Variant::Object(handle) = runtime.object_member(object, name) {
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
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use krkr_core::Size;
    use krkr_kag::{Attribute, AttributeValue};
    use krkr_tjs2::{
        Result,
        runtime::{Runtime, Variant},
    };

    use super::*;
    use crate::{KrkrHost, KrkrPlugin};

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

        assert!(frame.output.transition.is_none());
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "win.completed")
                .expect("completed"),
            Variant::Integer(1)
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
                b.loadStruct(binPath, "b");
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
                base.fillRect(0, 0, 4, 2, 0x0000ff);

                var child = new Layer(null, base);
                child.visible = true;
                child.setPos(2, 0);
                child.setSize(2, 2);
                child.setImageSize(2, 2);
                child.fillRect(0, 0, 2, 2, 0xff0000);

                var snapshot = new Layer();
                snapshot.setImageSize(4, 2);
                snapshot.face = dfAlpha;
                snapshot.piledCopy(0, 0, base, 0, 0, 4, 2);

                var thumb = new Layer();
                thumb.setImageSize(2, 1);
                thumb.face = dfAlpha;
                thumb.stretchCopy(
                    0, 0, 2, 1, snapshot,
                    0, 0, snapshot.imageWidth, snapshot.imageHeight, stLinear);
                thumb.saveLayerImage(System.dataPath + "thumb-pipeline.bmp", "bmp24");
                "#,
            )
            .expect("save thumbnail");

        let bytes = fs::read(root.join("savedata/thumb-pipeline.bmp")).expect("bmp");
        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(bytes.len(), 54 + 8);
        assert_eq!(&bytes[54..62], &[255, 0, 0, 0, 0, 255, 0, 0]);
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
                return Window.mainWindow === window;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::Integer(1));
        assert!(engine.window_fullscreen());
    }

    #[test]
    fn window_add_tracks_children_and_primary_layer() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var window = new Window();
                var layer = new Layer(window, null);
                window.add(layer);
                var before = window.children.count + ":" +
                    (window.primaryLayer === layer) + ":" +
                    (window.focusedLayer === null) + ":" +
                    (Window.mainWindow === window);
                window.add(layer);
                var deduped = window.children.count;
                window.remove(layer);
                return before + ":" + deduped + ":" + window.children.count + ":" +
                    (window.primaryLayer === void) + ":" +
                    (window.focusedLayer === null);
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1:1:1:1:1:0:1:1".to_string()));
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
        let Variant::Object(window) = engine.tjs_runtime().global_member("window") else {
            panic!("window missing");
        };
        let Variant::Object(layer) = engine.tjs_runtime().global_member("layer") else {
            panic!("layer missing");
        };
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

        let Variant::Object(modal) = engine.tjs_runtime().global_member("modal") else {
            panic!("modal window missing");
        };
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

        let Variant::Object(layer) = engine.tjs_runtime().global_member("dialogLayer") else {
            panic!("dialog layer missing");
        };
        let layer_id = engine.host().native_layer(layer).expect("native layer");
        assert!(
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .expect("layer node")
                .visible
        );

        let Variant::Object(dialog) = engine.tjs_runtime().global_member("dialog") else {
            panic!("dialog missing");
        };
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
        let Variant::Object(timer_probe) = engine.tjs_runtime().global_member("timerProbe") else {
            panic!("timerProbe missing");
        };
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

        let Variant::Object(modal) = engine.tjs_runtime().global_member("modal") else {
            panic!("modal window missing");
        };
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
        let Variant::Object(first_timer) = engine.tjs_runtime().global_member("firstTimer") else {
            panic!("firstTimer missing");
        };
        let Variant::Object(second_timer) = engine.tjs_runtime().global_member("secondTimer")
        else {
            panic!("secondTimer missing");
        };
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

        let Variant::Object(modal) = engine.tjs_runtime().global_member("modal") else {
            panic!("modal window missing");
        };
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
    fn native_window_set_pos_offsets_primary_layer() {
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

        let Variant::Object(window) = engine.tjs_runtime().global_member("window") else {
            panic!("window missing");
        };
        assert_eq!(
            engine.tjs_runtime().object_member(window, "left"),
            Variant::Integer(20)
        );
        assert_eq!(
            engine.tjs_runtime().object_member(window, "top"),
            Variant::Integer(30)
        );
        let Variant::Object(root) = engine.tjs_runtime().global_member("root") else {
            panic!("root layer missing");
        };
        let layer_id = engine.host().native_layer(root).expect("native layer");
        let position = engine
            .host()
            .layer_tree()
            .absolute_position(layer_id)
            .expect("absolute position");
        assert_eq!(position, Point::new(20.0, 30.0));
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
                under.order = 100;
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

        let Variant::Object(modal) = engine.tjs_runtime().global_member("modal") else {
            panic!("modal window missing");
        };
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
            Variant::Integer(location.line.unwrap_or_default() as i64)
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

        let Variant::Object(stored) = engine.tjs_runtime().global_member("callbackStored") else {
            panic!("stored snapshot should be an object");
        };
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
            .with_parser_for_tjs(&mut engine.tjs_runtime, |parser, session, runtime, _| {
                session.process_native_fallback_tag(parser, runtime, &tempsave)
            })
            .expect("tempsave");

        engine.kag_session.message_layer.clear();
        engine.kag_session.state = KagTaskState::Running;
        engine.kag_session.clear_page_on_click = false;
        engine.kag_session.pending_tags.clear();

        let tempload = test_tag("tempload", &[("place", "9")]);
        engine
            .kag_session
            .with_parser_for_tjs(&mut engine.tjs_runtime, |parser, session, runtime, _| {
                session.process_native_fallback_tag(parser, runtime, &tempload)
            })
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
            .with_parser_for_tjs(&mut engine.tjs_runtime, |parser, session, runtime, _| {
                session.process_native_fallback_tag(parser, runtime, &tag)
            })
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

        let mut engine = KrkrEngine::new(EngineConfig {
            project_root: Some(root.clone()),
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
        let handler = match engine
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
        {
            Variant::Object(handle) => handle,
            other => panic!("expected handler object, got {other}"),
        };

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
        let handler = match engine
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
        {
            Variant::Object(handle) => handle,
            other => panic!("expected handler object, got {other}"),
        };
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
        assert!(frame.output.transition.is_none());
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
        let handler = match engine
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
        {
            Variant::Object(handle) => handle,
            other => panic!("expected handler object, got {other}"),
        };
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
        let handler = match engine
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
        {
            Variant::Object(handle) => handle,
            other => panic!("expected handler object, got {other}"),
        };
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
        let handler = match engine
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
        {
            Variant::Object(handle) => handle,
            other => panic!("expected handler object, got {other}"),
        };
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
        let handler = match engine
            .execute_script(
                "inline.tjs",
                r#"
                var handler = new Dictionary();
                handler.onTag = function(elm) {
                    throw new Exception("tag boom");
                };
                return handler;
                "#,
            )
            .expect("handler")
        {
            Variant::Object(handle) => handle,
            other => panic!("expected handler object, got {other}"),
        };
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
        let transition = frame.output.transition.as_ref().expect("transition");
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
                .transition
                .as_ref()
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
        assert!(frame.output.transition.is_none());

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

        let transition = frame.output.transition.as_ref().expect("transition");
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
                "[trans method=wave wavetype=2 maxh=20 maxomega=0.1 bgcolor1=0xff0000 bgcolor2=0x0000ff time=1000][wt][s]"
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

        let transition = frame.output.transition.as_ref().expect("transition");
        assert_eq!(transition.method, "wave");
        assert_eq!(transition.params.method, krkr_core::TransitionMethod::Wave);
        assert_eq!(transition.params.wave_type, 2.0);
        assert_eq!(transition.params.max_h, 20.0);
        assert!((transition.params.max_omega - 0.1).abs() < 0.001);
        assert_eq!(transition.params.bg_color1, Color::rgb_u8(255, 0, 0));
        assert_eq!(transition.params.bg_color2, Color::rgb_u8(0, 0, 255));

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
            let transition = frame.output.transition.as_ref().expect("transition");
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
        assert!(frame.output.transition.is_some());
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

        let Variant::Object(parent) = engine.tjs_runtime().global_member("parentProbe") else {
            panic!("parent missing");
        };
        let Variant::Object(child) = engine.tjs_runtime().global_member("childProbe") else {
            panic!("child missing");
        };
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

    #[test]
    fn native_layer_load_images_preserves_existing_viewport_size() {
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

        assert_eq!(result, Variant::String("1280:720:2:3".to_string()));

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

        assert_eq!(result, Variant::String("0:0:320:320:24:24:2".to_string()));

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

        let Variant::Object(kag) = engine.tjs_runtime().global_member("kag") else {
            panic!("kag missing");
        };
        let Variant::Object(fore) = engine.tjs_runtime().object_member(kag, "fore") else {
            panic!("fore missing");
        };
        let Variant::Object(back) = engine.tjs_runtime().object_member(kag, "back") else {
            panic!("back missing");
        };
        let Variant::Object(fore_base) = engine.tjs_runtime().object_member(fore, "base") else {
            panic!("fore base missing");
        };
        let Variant::Object(back_layers) = engine.tjs_runtime().object_member(back, "layers")
        else {
            panic!("back layers missing");
        };
        let Variant::Object(back_layer0) = engine.tjs_runtime().object_member(back_layers, "0")
        else {
            panic!("back layer missing");
        };
        let Variant::Object(back_messages) = engine.tjs_runtime().object_member(back, "messages")
        else {
            panic!("back messages missing");
        };
        let Variant::Object(back_message0) = engine.tjs_runtime().object_member(back_messages, "0")
        else {
            panic!("back message missing");
        };

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
        assert_eq!(image_command_count(&frame), 1);

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
    fn native_kag_base_transition_uses_back_children_as_live_tree() {
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
        engine
            .execute_script(
                "inline.tjs",
                r#"
                kag.fore.base.window = %[transCount: 1];
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
        let transition = frame.output.transition.as_ref().expect("transition");
        assert!(transition.frozen_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 5.0 && image.rect.y == 7.0
            )
        }));
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 40.0 && image.rect.y == 50.0
            )
        }));

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
        let transition = frame.output.transition.as_ref().expect("transition");
        assert!(transition.frozen_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 5.0 && image.rect.y == 7.0
            )
        }));
        assert!(frame.output.draw_commands.iter().any(|command| {
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
        let transition = frame.output.transition.as_ref().expect("transition");
        assert!(transition.frozen_draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 5.0 && image.rect.y == 7.0
            )
        }));
        assert_eq!(image_command_count(&frame), 0);

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
        let transition = frame.output.transition.as_ref().expect("transition");
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
    fn native_kag_base_transition_materializes_back_children_for_page_exchange() {
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
        assert!(frame.output.transition.is_none());
        assert!(frame.output.draw_commands.iter().any(|command| {
            matches!(
                command,
                krkr_core::DrawCommand::Image(image)
                    if image.rect.x == 40.0 && image.rect.y == 50.0
            )
        }));

        fs::remove_dir_all(root).expect("cleanup");
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
                kag.fore.base.beginTransition("crossfade", true, kag.back.base, %[]);
                kag.fore.base.exchangeInfo();
                var tmp = kag.fore;
                kag.fore = kag.back;
                kag.back = tmp;
                global.foreBasePrimary = kag.fore.base.isPrimary;
                global.backBasePrimary = kag.back.base.isPrimary;
                "#,
            )
            .expect("script");

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
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
                Duration::from_millis(1),
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
                var dest = new Layer();
                dest.visible = true;
                dest.assignImages(source);
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
        assert_eq!(frame.output.image_uploads.len(), 1);
        let images = frame
            .output
            .draw_commands
            .iter()
            .filter_map(|command| match command {
                krkr_core::DrawCommand::Image(image) => Some(image),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(images.len(), 2);
        assert_eq!(
            images[0].texture_id,
            frame.output.image_uploads[0].texture_id
        );
        assert_eq!(
            images[1].texture_id,
            frame.output.image_uploads[0].texture_id
        );

        fs::remove_dir_all(root).expect("cleanup");
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
                if(parent.imageModified) parent.colorRect(0, 0, 32, 32, 0);
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

    #[test]
    fn native_layer_begin_transition_applies_source_image_immediately() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("sprite.png"), 1, 1, &[0, 255, 0, 255]);

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let result = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.loadImages("sprite.png");
                var dest = new Layer();
                dest.visible = true;
                dest.window = %[transCount: 1];
                dest.inTransition = true;
                dest.beginTransition("crossfade", true, source, %[]);
                return dest.window.transCount + ":" + dest.inTransition + ":" + dest.imageWidth;
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
        assert_eq!(frame.output.image_uploads.len(), 1);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_timed_transition_completes_through_update() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 1, 1, &[255, 0, 0, 255]);
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
                global.dest = new Layer();
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.window = %[transCount: 1, completed: 0];
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
        assert!(frame.output.transition.is_some());
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
        assert!(frame.output.transition.is_none());
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
                global.dest = new TransitionLayer(window);
                dest.window = window;
                dest.inTransition = true;
                dest.beginTransition("crossfade", false, null, %[time: 0]);
                return window.completed + ":" + dest.inTransition + ":" + window.transCount;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("1:0:0".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_replacing_transition_completes_previous_transition() {
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
                global.dest = new Layer();
                dest.loadImages("old.png");
                dest.visible = true;
                dest.window = %[transCount: 0, completed: 0, lastSourceWidth: 0];
                dest.onTransitionCompleted = function(destLayer, srcLayer) {
                    this.window.completed++;
                    this.window.lastSourceWidth = srcLayer.imageWidth;
                    this.window.transCount--;
                    this.inTransition = this.window.transCount > 0;
                };
                function startTransition(storage) {
                    var source = new Layer();
                    source.loadImages(storage);
                    dest.inTransition = true;
                    dest.window.transCount++;
                    dest.beginTransition("crossfade", true, source, %[time: 1000]);
                }
                startTransition("mid.png");
                startTransition("new.png");
                return dest.inTransition + ":" + dest.window.transCount + ":" + dest.window.completed + ":" + dest.imageWidth + ":" + dest.window.lastSourceWidth;
                "#,
            )
            .expect("script");

        assert_eq!(result, Variant::String("1:1:1:3:2".to_string()));

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
            Variant::String("0:0:2:3".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_layer_stop_transition_completes_active_transition() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        write_png(root.join("old.png"), 1, 1, &[255, 0, 0, 255]);
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
                global.dest = new Layer();
                dest.loadImages("old.png");
                dest.visible = true;
                dest.inTransition = true;
                dest.window = %[transCount: 1, completed: 0];
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
        assert!(frame.output.transition.is_none());

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
                return layer.imageWidth + ":" + layer.imageHeight + ":" + layer.width;
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
                layer.fillRect(0, 0, 2, 2, 0xff0000);
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
                255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 0, 0,
                0, 0,
            ]
        );
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
        let Variant::Object(child) = engine.tjs_runtime().global_member("child") else {
            panic!("child missing");
        };
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
        assert_eq!(frame.output.image_uploads.len(), 1);

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
    fn native_layer_invalidate_removes_subtree_backing_and_pending_window_updates() {
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
        let Variant::Object(root) = engine.tjs_runtime().global_member("root") else {
            panic!("root missing");
        };
        let Variant::Object(child) = engine.tjs_runtime().global_member("child") else {
            panic!("child missing");
        };
        assert!(engine.host().native_layer(root).is_some());
        assert!(engine.host().native_layer(child).is_some());
        assert!(engine.host().has_pending_window_update(child));

        engine
            .execute_script("cleanup.tjs", "invalidate root;")
            .expect("invalidate");
        assert!(engine.host().native_layer(root).is_none());
        assert!(engine.host().native_layer(child).is_none());
        assert!(!engine.host().has_pending_window_update(child));
        assert!(!engine.host().has_pending_image_load_for_owner(child));
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
                0, 0, 0, 0, 0x40, 0x20, 0x10, 0x80, 0, 0, 0, 0, 0x40, 0x20, 0x10, 0x80
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
                source.fillRect(0, 0, 1, 1, 0xff0000);
                source.fillRect(1, 0, 1, 1, 0x00ff00);
                source.fillRect(0, 1, 1, 1, 0x0000ff);
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
                base.fillRect(0, 0, 4, 4, 0x202020);

                var child = new Layer(null, base);
                child.visible = true;
                child.setPos(1, 1);
                child.setSize(2, 2);
                child.setImageSize(2, 2);
                child.fillRect(0, 0, 2, 2, 0xff0000);

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
        let red = [255, 0, 0, 255];
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
                return (window.focusedLayer === child) + ":" + root.focused + ":" +
                    child.focused + ":" + root.events + ":" + child.events;
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1:0:1:blur:focus".to_string()));
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
                global.layer = new Layer();
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
                    "return (window.focusedLayer === second) + ':' + first.focused + ':' + second.focused;"
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
        assert_eq!(result, Variant::String("0:128:512".to_string()));
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

                global.owner = new Dictionary();
                owner.conductor = new Dictionary();
                owner.conductor.status = 2;
                owner.conductor.waitUntil = new Dictionary();
                owner.conductor.run = function() {
                    this.status = 1;
                    global.asyncProbe.trigger();
                };
                owner.conductor.trigger = function(name) {
                    if(this.status != 2) return false;
                    var func = this.waitUntil[name];
                    if(func === void) return false;
                    func();
                    this.waitUntil = new Dictionary();
                    this.run();
                    return true;
                };
                owner.onSESoundBufferStop = function(id) {
                    this.conductor.trigger("sestop" + id);
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
                buffer.fadeOutAndStop(0);
                "#,
            )
            .expect("script");

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
        assert_eq!(value, Variant::String("ch:A:r:1".to_string()));

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
        let Variant::Object(timer) = engine.tjs_runtime().global_member("timerProbe") else {
            panic!("timerProbe should be an object");
        };
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
    fn native_timer_interval_zero_runs_on_the_next_clock_turn() {
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
            Variant::Integer(1)
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
        let Variant::Object(timer_probe) = engine.tjs_runtime().global_member("timerProbe") else {
            panic!("timerProbe missing");
        };
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

        let Variant::Object(owner) = engine.tjs_runtime().global_member("actionOwner") else {
            panic!("action owner missing");
        };
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
        let Variant::Object(owner) = engine.tjs_runtime().global_member("continuousOwner") else {
            panic!("continuous owner missing");
        };
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

        let Variant::Object(system) = engine.tjs_runtime().global_member("System") else {
            panic!("System should be an object");
        };
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
                return kag.__nativeCanClose + ":" + (kag.__nativeClosed === void);
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

        let Variant::Object(modal) = engine.tjs_runtime().global_member("modal") else {
            panic!("modal window missing");
        };
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

    fn force_timer_due(engine: &mut KrkrEngine, timer: ObjectHandle) {
        engine
            .tjs_runtime
            .host_mut()
            .scheduler_mut()
            .set_timer_next_fire_millis(timer, Some(0));
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

    fn image_command_count(frame: &EngineFrame) -> usize {
        frame
            .output
            .draw_commands
            .iter()
            .filter(|command| matches!(command, krkr_core::DrawCommand::Image(_)))
            .count()
    }

    fn image_test_engine(root: &Path) -> KrkrEngine {
        KrkrEngine::new(EngineConfig {
            project_root: Some(root.to_path_buf()),
            kag_budget: KagRunBudget {
                max_tags_per_tick: 1000,
                max_wall_time: Duration::from_secs(1),
            },
            ..EngineConfig::default()
        })
        .expect("engine")
    }
}
