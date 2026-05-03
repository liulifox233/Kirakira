use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

use krkr_core::{
    AudioBus, AudioCommand, AudioLoadPolicy, ButtonState, Engine as CoreEngine,
    EngineConfig as CoreEngineConfig, EngineEvent, EngineKey, FrameInput, FrameOutput, LayerId,
    MessageLayerModel, Point, PointerButton, Size,
};
use krkr_kag::{Attribute, AttributeValue, KagParser, ParserSnapshot, Tag};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::{
    globals::install_tvp_globals,
    host::{ImageLoadRequest, ImageLoadState, ImageLoadTarget, KrkrHost},
    kag::EngineKagHost,
    native::classes::{
        apply_completed_image_load, apply_completed_resource_loads,
        finish_completed_native_transitions,
    },
    plugin::KrkrPlugin,
    script::{execute_expression_on_runtime, execute_script_on_runtime},
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
    kag_parser: KagParser,
    kag_task: KagRuntimeTask,
    core_engine: CoreEngine,
    message_layer: MessageLayerModel,
    kag_budget: KagRunBudget,
    plugins: Vec<Box<dyn KrkrPlugin>>,
    cursor_position: Option<Point>,
    hovered_layer: Option<LayerId>,
    pressed_layer: Option<LayerId>,
    captured_layer: Option<LayerId>,
    pending_input_events: VecDeque<EngineEvent>,
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
            kag_parser: KagParser::new(),
            kag_task: KagRuntimeTask::new(),
            core_engine: CoreEngine::new(CoreEngineConfig::default()),
            message_layer: MessageLayerModel::default(),
            kag_budget: config.kag_budget,
            plugins: Vec::new(),
            cursor_position: None,
            hovered_layer: None,
            pressed_layer: None,
            captured_layer: None,
            pending_input_events: VecDeque::new(),
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

    pub fn kag_parser(&self) -> &KagParser {
        &self.kag_parser
    }

    pub fn kag_parser_mut(&mut self) -> &mut KagParser {
        &mut self.kag_parser
    }

    pub fn execute_script(&mut self, source_name: &str, source: &str) -> Result<Variant> {
        execute_script_on_runtime(&mut self.tjs_runtime, source_name, source)
    }

    pub fn execute_expression(&mut self, source_name: &str, source: &str) -> Result<Variant> {
        execute_expression_on_runtime(&mut self.tjs_runtime, source_name, source)
    }

    pub fn execute_storage(&mut self, name: &str) -> Result<Variant> {
        let source = self.tjs_runtime.host().read_text_storage(name)?;
        execute_script_on_runtime(&mut self.tjs_runtime, name, &source)
    }

    pub fn eval_storage(&mut self, name: &str) -> Result<Variant> {
        let source = self.tjs_runtime.host().read_text_storage(name)?;
        execute_expression_on_runtime(&mut self.tjs_runtime, name, &source)
    }

    pub fn execute_startup(&mut self) -> Result<Variant> {
        self.execute_storage("startup.tjs")
    }

    pub fn load_kag_scenario(&mut self, storage: &str) -> krkr_kag::Result<()> {
        let mut host = EngineKagHost::new(&mut self.tjs_runtime);
        self.kag_parser.load_scenario_with(storage, &mut host)?;
        self.message_layer.clear();
        self.kag_task.start();
        Ok(())
    }

    pub fn next_kag_tag(&mut self) -> krkr_kag::Result<Option<Tag>> {
        let mut host = EngineKagHost::new(&mut self.tjs_runtime);
        self.kag_parser.next_tag_with(&mut host)
    }

    pub fn kag_state(&self) -> &KagTaskState {
        self.kag_task.state()
    }

    pub fn has_kag_scenario(&self) -> bool {
        self.kag_task.loaded()
    }

    pub fn message_layer(&self) -> &MessageLayerModel {
        &self.message_layer
    }

    pub fn kag_location(&self) -> KagLocation {
        KagLocation {
            storage: self.kag_parser.cur_storage().map(str::to_string),
            label: self.kag_parser.cur_label().map(str::to_string),
            line: self.kag_parser.cur_line(),
            page: self.message_layer.page,
        }
    }

    pub fn set_kag_handler(&mut self, handler: ObjectHandle) {
        self.kag_task.set_handler(handler);
    }

    pub fn clear_kag_handler(&mut self) {
        self.kag_task.clear_handler();
    }

    pub fn signal_kag_click(&mut self) {
        self.kag_task.signal_click(&mut self.message_layer);
    }

    pub fn tick(&mut self) -> Result<EngineTickResult> {
        self.advance(Duration::ZERO)
    }

    pub fn update(&mut self, input: EngineInput, delta: Duration) -> Result<EngineFrame> {
        self.input_result = EngineInputResult::default();
        self.pending_input_events.extend(input.events);
        self.pump_tjs_events()?;
        self.resume_modal_call_if_ready()?;
        let tick = self.advance(delta)?;
        self.pump_layer_paints()?;
        self.sync_native_layers_from_tjs()?;
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
                &self.message_layer,
                &suppressed_images,
                transition,
            );
        Ok(EngineFrame {
            output,
            tick,
            input: self.input_result,
            message_layer: self.message_layer.clone(),
            location: self.kag_location(),
        })
    }

    fn resume_modal_call_if_ready(&mut self) -> Result<()> {
        while let Some(window) = self.tjs_runtime.host().current_modal_window() {
            let closed = !self.tjs_runtime.object_valid(window)
                || !self
                    .tjs_runtime
                    .object_member(window, "visible")
                    .is_truthy()
                || self
                    .tjs_runtime
                    .object_member(window, "__nativeClosed")
                    .is_truthy();
            if !closed {
                break;
            }

            self.tjs_runtime.host_mut().pop_modal_window(window);
            self.tjs_runtime.resume_suspended()?;
            if self.kag_task.state == KagTaskState::WaitingModal && !self.tjs_runtime.is_suspended()
            {
                self.kag_task.state = KagTaskState::Running;
            }
        }
        Ok(())
    }

    fn pump_layer_paints(&mut self) -> Result<()> {
        const MAX_LAYER_PAINT_PASSES: usize = 1024;

        for _ in 0..MAX_LAYER_PAINT_PASSES {
            let layers = self.tjs_runtime.host_mut().take_pending_layer_paints();
            if layers.is_empty() {
                return Ok(());
            }
            for layer in layers {
                if !self.tjs_runtime.object_valid(layer) {
                    continue;
                }
                if matches!(
                    self.tjs_runtime.object_member(layer, "onPaint"),
                    Variant::Void
                ) {
                    continue;
                }
                self.tjs_runtime
                    .call_object_method(layer, "onPaint", Vec::new())?;
            }
        }

        self.tjs_runtime
            .host_mut()
            .log("layer paint pump reached its per-frame pass budget; remaining paints deferred");
        Ok(())
    }

    fn advance(&mut self, delta: Duration) -> Result<EngineTickResult> {
        apply_completed_resource_loads(&mut self.tjs_runtime)?;
        self.tjs_runtime.host_mut().advance_transition(delta);
        finish_completed_native_transitions(&mut self.tjs_runtime)?;
        let transition_active = self.tjs_runtime.host().has_active_transition();
        let resource_pending = self.tjs_runtime.host().has_pending_resource_loads();
        self.kag_task
            .update_wait(delta, transition_active, resource_pending);
        self.kag_task.run_until_yield(
            &mut self.kag_parser,
            &mut self.tjs_runtime,
            &mut self.message_layer,
            self.kag_budget,
        )
    }

    fn runtime_window_object(&self) -> Option<ObjectHandle> {
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

    fn pump_tjs_events(&mut self) -> Result<()> {
        const MAX_NATIVE_EVENT_PASSES: usize = 1024;

        self.fire_continuous_handlers()?;

        for _ in 0..MAX_NATIVE_EVENT_PASSES {
            if self.fire_due_tjs_events(TjsEventPriority::Exclusive)? {
                continue;
            }

            if let Some(event) = self.pending_input_events.pop_front() {
                self.handle_input_event(event)?;
                continue;
            }

            if self.fire_due_tjs_events(TjsEventPriority::Normal)? {
                continue;
            }

            if self.fire_due_tjs_events(TjsEventPriority::Idle)? {
                continue;
            }

            return Ok(());
        }

        self.tjs_runtime
            .host_mut()
            .log("native event pump reached its per-frame pass budget; remaining events deferred");
        Ok(())
    }

    fn fire_continuous_handlers(&mut self) -> Result<()> {
        for handler in self.tjs_runtime.host().continuous_handlers() {
            if matches!(handler, Variant::Void) {
                continue;
            }
            self.tjs_runtime.call_function(handler, Vec::new())?;
        }
        Ok(())
    }

    fn fire_due_tjs_events(&mut self, priority: TjsEventPriority) -> Result<bool> {
        let events = self.collect_due_tjs_events(priority)?;
        if events.is_empty() {
            return Ok(false);
        }
        for event in events {
            self.fire_tjs_event(event)?;
        }
        Ok(true)
    }

    fn collect_due_tjs_events(&mut self, priority: TjsEventPriority) -> Result<Vec<TjsEvent>> {
        let now = self.tjs_runtime.host_mut().now_millis();
        let pending_async_triggers = self.tjs_runtime.host_mut().take_pending_async_triggers();
        let mut events = Vec::new();
        let mut deferred_async_triggers = Vec::new();
        for handle in pending_async_triggers {
            if self.async_trigger_priority(handle)? == priority {
                events.push(TjsEvent {
                    handle,
                    kind: TjsEventKind::AsyncTrigger,
                });
            } else {
                deferred_async_triggers.push(handle);
            }
        }
        for handle in deferred_async_triggers {
            self.tjs_runtime.host_mut().trigger_async(handle);
        }
        if priority != TjsEventPriority::Normal {
            return Ok(events);
        }

        events.extend(
            self.tjs_runtime
                .host_mut()
                .take_due_audio_fade_completions()
                .into_iter()
                .map(|handle| TjsEvent {
                    handle,
                    kind: TjsEventKind::AudioFadeCompleted,
                }),
        );

        for handle in self.tjs_runtime.host().timer_handles() {
            let enabled = self
                .tjs_runtime
                .object_member(handle, "enabled")
                .is_truthy();
            if !enabled {
                self.tjs_runtime
                    .host_mut()
                    .set_timer_next_fire_millis(handle, None);
                continue;
            }

            let interval = self
                .tjs_runtime
                .object_member(handle, "interval")
                .to_integer()?
                .max(0);
            let next_fire = match self.tjs_runtime.host().timer_next_fire_millis(handle) {
                Some(next_fire) => next_fire,
                None => {
                    let next_fire = now.saturating_add(interval);
                    self.tjs_runtime
                        .host_mut()
                        .set_timer_next_fire_millis(handle, Some(next_fire));
                    next_fire
                }
            };

            if now >= next_fire {
                self.tjs_runtime
                    .host_mut()
                    .set_timer_next_fire_millis(handle, None);
                events.push(TjsEvent {
                    handle,
                    kind: TjsEventKind::Timer,
                });
            }
        }

        Ok(events)
    }

    fn async_trigger_priority(&self, handle: ObjectHandle) -> Result<TjsEventPriority> {
        Ok(
            match self
                .tjs_runtime
                .object_member(handle, "mode")
                .to_integer()?
            {
                1 => TjsEventPriority::Exclusive,
                2 => TjsEventPriority::Idle,
                _ => TjsEventPriority::Normal,
            },
        )
    }

    fn fire_tjs_event(&mut self, event: TjsEvent) -> Result<()> {
        if event.kind == TjsEventKind::Timer
            && !self
                .tjs_runtime
                .object_member(event.handle, "enabled")
                .is_truthy()
        {
            return Ok(());
        }

        let callback = self.tjs_runtime.object_member(event.handle, "__callback");
        if !matches!(callback, Variant::Void) {
            return self
                .tjs_runtime
                .call_function(callback, Vec::new())
                .map(|_| ());
        }

        let method = match event.kind {
            TjsEventKind::Timer => "onTimer",
            TjsEventKind::AsyncTrigger => "onFire",
            TjsEventKind::AudioFadeCompleted => "onFadeCompleted",
        };
        if matches!(
            self.tjs_runtime.object_member(event.handle, method),
            Variant::Void
        ) {
            return Ok(());
        }
        self.tjs_runtime
            .call_object_method(event.handle, method, Vec::new())
            .map(|_| ())
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
                    self.dispatch_layer_cursor_move(*position)?;
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Pressed,
                } => {
                    self.dispatch_window_pointer_event("onMouseDown", 0)?;
                    let raw_target = self.layer_at_cursor()?;
                    let handled_by_script = raw_target.is_some_and(|layer_id| {
                        self.layer_has_script_handler(layer_id, "onMouseDown")
                    });
                    self.dispatch_layer_pointer_event("onMouseDown", 0, raw_target)?;
                    self.pressed_layer = raw_target;
                    self.captured_layer = raw_target;
                    if !handled_by_script && self.should_fire_primary_click(raw_target) {
                        self.fire_kag_primary_click(false)?;
                    }
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Released,
                } => {
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
                    self.signal_kag_click();
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Secondary,
                    state: ButtonState::Pressed,
                } => {
                    let handled_by_window = self.dispatch_window_pointer_event("onMouseDown", 1)?;
                    let raw_target = self.layer_at_cursor()?;
                    let handled_by_layer = raw_target.is_some_and(|layer_id| {
                        self.layer_has_script_handler(layer_id, "onMouseDown")
                    });
                    self.dispatch_layer_pointer_event("onMouseDown", 1, raw_target)?;
                    self.captured_layer = raw_target;
                    if !handled_by_window && !handled_by_layer {
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
        let Variant::Object(focused_layer) = self.tjs_runtime.object_member(window, "focusedLayer")
        else {
            return Ok(false);
        };
        let focused_layer = self
            .tjs_runtime
            .bound_this(focused_layer)
            .unwrap_or(focused_layer);
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

        self.kag_task.fire_right_click(
            &mut self.kag_parser,
            &mut self.tjs_runtime,
            &mut self.message_layer,
        )
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
        let Variant::Object(focused_layer) = self.tjs_runtime.object_member(window, "focusedLayer")
        else {
            return Ok(());
        };
        let focused_layer = self
            .tjs_runtime
            .bound_this(focused_layer)
            .unwrap_or(focused_layer);
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
        let mut hits = Vec::new();
        for layer_id in candidates {
            if self.layer_accepts_script_hit_test(layer_id, position)? {
                hits.push(layer_id);
            }
        }
        Ok(hits)
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
        self.tjs_runtime
            .call_object_method(object, method, args)
            .map(|_| ())
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

    fn sync_native_layers_from_tjs(&mut self) -> Result<()> {
        let entries = self.tjs_runtime.host().native_layer_entries();
        let kag_targets = self.collect_kag_layer_targets();
        for (handle, layer_id) in entries {
            if !self.tjs_runtime.object_valid(handle) {
                continue;
            }
            let parent = match self.tjs_runtime.object_member(handle, "parent") {
                Variant::Object(parent) => {
                    let parent = self.tjs_runtime.bound_this(parent).unwrap_or(parent);
                    self.tjs_runtime.host().native_layer(parent)
                }
                _ => None,
            };
            let left = object_i64(&self.tjs_runtime, handle, "left")?;
            let top = object_i64(&self.tjs_runtime, handle, "top")?;
            let width = object_i64(&self.tjs_runtime, handle, "width")?;
            let height = object_i64(&self.tjs_runtime, handle, "height")?;
            let image_left = object_i64(&self.tjs_runtime, handle, "imageLeft")?;
            let image_top = object_i64(&self.tjs_runtime, handle, "imageTop")?;
            let image_width = object_i64(&self.tjs_runtime, handle, "imageWidth")?;
            let image_height = object_i64(&self.tjs_runtime, handle, "imageHeight")?;
            let visible = object_i64(&self.tjs_runtime, handle, "visible")? != 0;
            let opacity = object_i64(&self.tjs_runtime, handle, "opacity")?.clamp(0, 255) as u8;
            let enabled = object_i64(&self.tjs_runtime, handle, "enabled")? != 0;
            let node_enabled = object_i64(&self.tjs_runtime, handle, "nodeEnabled")? != 0;
            let layer_type = object_i64(&self.tjs_runtime, handle, "type")? as i32;
            let face = object_i64(&self.tjs_runtime, handle, "face")? as i32;
            let hit_type = object_i64(&self.tjs_runtime, handle, "hitType")? as i32;
            let hit_threshold = object_i64(&self.tjs_runtime, handle, "hitThreshold")?
                .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            let absolute = object_optional_i64(&self.tjs_runtime, handle, "absolute")?;
            let order = object_optional_i64(&self.tjs_runtime, handle, "order")?;

            let target_handle = self.tjs_runtime.bound_this(handle).unwrap_or(handle);
            let kag_target = kag_targets.get(&target_handle).cloned();
            if let Some((page, layer_name)) = kag_target.as_ref().filter(|(page, _)| page == "back")
            {
                let source = self
                    .tjs_runtime
                    .host()
                    .layer_tree()
                    .layer(layer_id)
                    .cloned();
                if let Some(layer) = self
                    .tjs_runtime
                    .host_mut()
                    .layer_tree_mut()
                    .layer_mut(layer_id)
                {
                    layer.renderable = false;
                }
                self.tjs_runtime
                    .host_mut()
                    .mutate_kag_layer(page, layer_name, |layer| {
                        if layer.image.is_none()
                            && let Some(source) = &source
                            && source.image.is_some()
                        {
                            layer.image = source.image.clone();
                        }
                        layer.left = left as f32;
                        layer.top = top as f32;
                        layer.width = width.max(0) as f32;
                        layer.height = height.max(0) as f32;
                        layer.image_left = image_left as f32;
                        layer.image_top = image_top as f32;
                        layer.image_width = image_width.max(0) as f32;
                        layer.image_height = image_height.max(0) as f32;
                        layer.visible = visible;
                        layer.opacity = opacity;
                        layer.enabled = enabled;
                        layer.node_enabled = node_enabled;
                        layer.layer_type = layer_type;
                        layer.face = face;
                        layer.hit_type = hit_type;
                        layer.hit_threshold = hit_threshold;
                        if let Some(z_order) = absolute.or(order) {
                            layer.z_order = z_order.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                        }
                    });
                continue;
            }

            let layer_tree = self.tjs_runtime.host_mut().layer_tree_mut();
            let render_parent = if matches!(kag_target.as_ref(), Some((page, layer_name)) if page == "fore" && layer_name == "base")
            {
                None
            } else {
                parent
            };
            layer_tree.set_parent(layer_id, render_parent);
            if let Some(layer) = layer_tree.layer_mut(layer_id) {
                layer.left = left as f32;
                layer.top = top as f32;
                layer.width = width.max(0) as f32;
                layer.height = height.max(0) as f32;
                layer.image_left = image_left as f32;
                layer.image_top = image_top as f32;
                layer.image_width = image_width.max(0) as f32;
                layer.image_height = image_height.max(0) as f32;
                layer.visible = visible;
                if matches!(kag_target.as_ref(), Some((page, _)) if page == "fore") {
                    layer.renderable = true;
                }
                layer.opacity = opacity;
                layer.enabled = enabled;
                layer.node_enabled = node_enabled;
                layer.layer_type = layer_type;
                layer.face = face;
                layer.hit_type = hit_type;
                layer.hit_threshold = hit_threshold;
                if let Some(z_order) = absolute.or(order) {
                    layer.z_order = z_order.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                }
            }
        }
        Ok(())
    }

    fn collect_kag_layer_targets(&self) -> BTreeMap<ObjectHandle, (String, String)> {
        let mut targets = BTreeMap::new();
        let Variant::Object(kag) = self.tjs_runtime.global_member("kag") else {
            return targets;
        };

        for page in ["fore", "back"] {
            let Variant::Object(page_object) = self.tjs_runtime.object_member(kag, page) else {
                continue;
            };
            if let Variant::Object(base) = self.tjs_runtime.object_member(page_object, "base") {
                targets.insert(
                    self.tjs_runtime.bound_this(base).unwrap_or(base),
                    (page.to_string(), "base".to_string()),
                );
            }
            self.collect_kag_layer_array_targets(&mut targets, page, page_object, "layers", false);
            self.collect_kag_layer_array_targets(&mut targets, page, page_object, "messages", true);
        }

        targets
    }

    fn collect_kag_layer_array_targets(
        &self,
        targets: &mut BTreeMap<ObjectHandle, (String, String)>,
        page: &str,
        page_object: ObjectHandle,
        member: &str,
        message_layers: bool,
    ) {
        let Variant::Object(array) = self.tjs_runtime.object_member(page_object, member) else {
            return;
        };
        if let Some(elements) = self.tjs_runtime.array_elements(array) {
            for (index, value) in elements.iter().enumerate() {
                self.insert_kag_layer_target(targets, page, index, message_layers, value);
            }
            return;
        }
        let Ok(count) = self.tjs_runtime.object_member(array, "count").to_integer() else {
            return;
        };
        for index in 0..count.max(0) {
            let value = self.tjs_runtime.object_member(array, &index.to_string());
            self.insert_kag_layer_target(targets, page, index as usize, message_layers, &value);
        }
    }

    fn insert_kag_layer_target(
        &self,
        targets: &mut BTreeMap<ObjectHandle, (String, String)>,
        page: &str,
        index: usize,
        message_layer: bool,
        value: &Variant,
    ) {
        let Variant::Object(candidate) = value else {
            return;
        };
        let handle = self
            .tjs_runtime
            .bound_this(*candidate)
            .unwrap_or(*candidate);
        let layer = if message_layer {
            format!("message{index}")
        } else {
            index.to_string()
        };
        targets.insert(handle, (page.to_string(), layer));
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
struct TjsEvent {
    handle: ObjectHandle,
    kind: TjsEventKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TjsEventKind {
    Timer,
    AsyncTrigger,
    AudioFadeCompleted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TjsEventPriority {
    Exclusive,
    Normal,
    Idle,
}

#[derive(Clone, Debug)]
struct KagRuntimeTask {
    state: KagTaskState,
    handler: Option<ObjectHandle>,
    pending_tags: VecDeque<Tag>,
    temp_snapshots: BTreeMap<i64, KagTempSnapshot>,
    right_click: RightClickAction,
    loaded: bool,
    clear_page_on_click: bool,
}

#[derive(Clone, Debug)]
struct KagTempSnapshot {
    parser: ParserSnapshot,
    message_layer: MessageLayerModel,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RightClickAction {
    enabled: bool,
    call: bool,
    jump: bool,
    storage: Option<String>,
    target: Option<String>,
}

impl KagRuntimeTask {
    fn new() -> Self {
        Self {
            state: KagTaskState::Finished,
            handler: None,
            pending_tags: VecDeque::new(),
            temp_snapshots: BTreeMap::new(),
            right_click: RightClickAction::default(),
            loaded: false,
            clear_page_on_click: false,
        }
    }

    fn state(&self) -> &KagTaskState {
        &self.state
    }

    fn loaded(&self) -> bool {
        self.loaded
    }

    fn start(&mut self) {
        self.state = KagTaskState::Running;
        self.pending_tags.clear();
        self.temp_snapshots.clear();
        self.right_click = RightClickAction::default();
        self.loaded = true;
        self.clear_page_on_click = false;
    }

    fn set_handler(&mut self, handler: ObjectHandle) {
        self.handler = Some(handler);
    }

    fn clear_handler(&mut self) {
        self.handler = None;
    }

    fn signal_click(&mut self, message_layer: &mut MessageLayerModel) {
        if self.state == KagTaskState::WaitingClick {
            if self.clear_page_on_click {
                message_layer.clear_text();
                self.clear_page_on_click = false;
            }
            message_layer.waiting_for_click = false;
            self.state = KagTaskState::Running;
        }
    }

    fn update_wait(&mut self, delta: Duration, transition_active: bool, resource_pending: bool) {
        if let KagTaskState::WaitingTimer { remaining } = self.state.clone() {
            self.state = if delta >= remaining {
                KagTaskState::Running
            } else {
                KagTaskState::WaitingTimer {
                    remaining: remaining - delta,
                }
            };
        } else if self.state == KagTaskState::WaitingTransition && !transition_active {
            self.state = KagTaskState::Running;
        } else if self.state == KagTaskState::WaitingResource && !resource_pending {
            self.state = KagTaskState::Running;
        }
    }

    fn run_until_yield(
        &mut self,
        parser: &mut KagParser,
        runtime: &mut Runtime<KrkrHost>,
        message_layer: &mut MessageLayerModel,
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
                    let mut host = EngineKagHost::new(runtime);
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
            let action = self.process_tag(parser, runtime, message_layer, tag)?;
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
        message_layer: &mut MessageLayerModel,
        tag: Tag,
    ) -> Result<TagAction> {
        if let Some(handler) = self.handler
            && !matches!(runtime.object_member(handler, "onTag"), Variant::Void)
        {
            let tag_object = tag_variant(runtime, &tag)?;
            let value = self.call_handler(runtime, handler, "onTag", vec![tag_object])?;
            return self.apply_handler_step(tag, value);
        }

        let default_action = self.process_builtin_tag(parser, runtime, message_layer, &tag)?;
        if matches!(default_action, TagAction::Continue)
            && let Some(handler) = self.handler
            && !is_builtin_tag(&tag.tagname)
            && !matches!(
                runtime.object_member(handler, "onUnknownTag"),
                Variant::Void
            )
        {
            let tag_object = tag_variant(runtime, &tag)?;
            let value = call_tag_handler(
                runtime,
                handler,
                "onUnknownTag",
                vec![Variant::String(tag.tagname.clone()), tag_object],
            )
            .inspect_err(|error| {
                self.state = KagTaskState::Error {
                    message: error.to_string(),
                };
            })?;
            return self.apply_handler_step(tag, value);
        }
        Ok(default_action)
    }

    fn call_handler(
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

    fn process_builtin_tag(
        &mut self,
        parser: &mut KagParser,
        runtime: &mut Runtime<KrkrHost>,
        message_layer: &mut MessageLayerModel,
        tag: &Tag,
    ) -> Result<TagAction> {
        match tag.tagname.as_str() {
            "ch" => {
                if let Some(text) = tag.literal_attr("text") {
                    message_layer.append_text(text);
                }
                Ok(TagAction::Continue)
            }
            "r" => {
                message_layer.newline();
                Ok(TagAction::Continue)
            }
            "l" => {
                message_layer.newline();
                Ok(self.wait_click(message_layer, false))
            }
            "p" => {
                message_layer.page_break();
                Ok(self.wait_click(message_layer, true))
            }
            "font" | "deffont" => {
                apply_message_font_tag(message_layer, tag)?;
                Ok(TagAction::Continue)
            }
            "resetfont" => {
                message_layer.font = krkr_core::FontSpec::default();
                message_layer.style = krkr_core::TextStyle::default();
                Ok(TagAction::Continue)
            }
            "style" => {
                apply_message_style_tag(message_layer, tag);
                Ok(TagAction::Continue)
            }
            "locate" => {
                if let Some(x) = tag_i64(tag, "x")? {
                    message_layer.cursor_x = x as i32;
                }
                if let Some(y) = tag_i64(tag, "y")? {
                    message_layer.cursor_y = y as i32;
                }
                Ok(TagAction::Continue)
            }
            "ptext" => {
                if let Some(text) = tag.literal_attr("text") {
                    message_layer.append_text(text);
                }
                Ok(TagAction::Continue)
            }
            "waitclick" => Ok(self.wait_click(message_layer, false)),
            "wait" => Ok(match tag_millis(tag, "time") {
                Some(duration) => self.wait(KagTaskState::WaitingTimer {
                    remaining: duration,
                }),
                None => self.wait_click(message_layer, false),
            }),
            "eval" => {
                execute_eval_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            "trace" => {
                execute_trace_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            "cm" | "ct" | "er" => {
                message_layer.clear_text();
                Ok(TagAction::Continue)
            }
            "image" => {
                if apply_image_tag(runtime, tag)? {
                    Ok(self.wait(KagTaskState::WaitingResource))
                } else {
                    Ok(TagAction::Continue)
                }
            }
            "layopt" | "position" => {
                apply_layer_options_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            "freeimage" => {
                apply_freeimage_tag(runtime, tag);
                Ok(TagAction::Continue)
            }
            "backlay" => {
                let layer = tag.literal_attr("layer");
                runtime.host_mut().backlay_kag_layers(layer);
                Ok(TagAction::Continue)
            }
            "rclick" => {
                self.apply_right_click_tag(tag);
                Ok(TagAction::Continue)
            }
            "tempsave" => {
                let place = tag_i64(tag, "place")?.unwrap_or(0);
                self.temp_snapshots.insert(
                    place,
                    KagTempSnapshot {
                        parser: parser.store(),
                        message_layer: message_layer.clone(),
                    },
                );
                Ok(TagAction::Continue)
            }
            "tempload" => {
                let place = tag_i64(tag, "place")?.unwrap_or(0);
                if let Some(snapshot) = self.temp_snapshots.get(&place).cloned() {
                    parser
                        .restore(snapshot.parser)
                        .map_err(|error| TjsError::runtime(error.to_string()))?;
                    *message_layer = snapshot.message_layer;
                    self.pending_tags.clear();
                    self.state = KagTaskState::Running;
                }
                Ok(TagAction::Continue)
            }
            "commit" | "history" => Ok(TagAction::Continue),
            "gotostart" => {
                if let Some(storage) = parser.cur_storage().map(str::to_string) {
                    parser
                        .set_cur_storage(storage)
                        .map_err(|error| TjsError::runtime(error.to_string()))?;
                    message_layer.clear();
                    self.pending_tags.clear();
                    self.state = KagTaskState::Running;
                }
                Ok(TagAction::Continue)
            }
            "laycount" => {
                apply_laycount_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            "current" => {
                apply_current_tag(runtime, tag);
                Ok(TagAction::Continue)
            }
            "trans" => {
                let method = tag.literal_attr("method").unwrap_or("crossfade");
                let duration = tag_millis(tag, "time").unwrap_or(Duration::ZERO);
                runtime.host_mut().begin_kag_transition(method, duration);
                Ok(TagAction::Continue)
            }
            "wt" => {
                if runtime.host().has_active_transition() {
                    Ok(self.wait(KagTaskState::WaitingTransition))
                } else {
                    Ok(TagAction::Continue)
                }
            }
            "playbgm" => {
                play_kag_audio_tag(
                    runtime,
                    tag,
                    AudioBus::Bgm,
                    AudioLoadPolicy::Streaming,
                    true,
                )?;
                Ok(TagAction::Continue)
            }
            "playse" => {
                play_kag_audio_tag(
                    runtime,
                    tag,
                    AudioBus::SoundEffect,
                    AudioLoadPolicy::StaticCached,
                    false,
                )?;
                Ok(TagAction::Continue)
            }
            "playvoice" => {
                play_kag_audio_tag(
                    runtime,
                    tag,
                    AudioBus::SoundEffect,
                    AudioLoadPolicy::Streaming,
                    false,
                )?;
                Ok(TagAction::Continue)
            }
            "stopbgm" => {
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
            "stopse" | "stopvoice" => {
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
            "wq" | "wf" | "wb" | "wm" => Ok(self.wait(KagTaskState::WaitingAudio)),
            "waitload" | "waittrig" => Ok(self.wait(KagTaskState::WaitingResource)),
            "s" => {
                self.state = KagTaskState::Finished;
                Ok(TagAction::Yield(KagYieldReason::Finished))
            }
            "defstyle" | "resetstyle" | "ruby" => Ok(TagAction::Continue),
            _ => Ok(TagAction::Continue),
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

    fn fire_right_click(
        &mut self,
        parser: &mut KagParser,
        runtime: &mut Runtime<KrkrHost>,
        message_layer: &mut MessageLayerModel,
    ) -> Result<()> {
        if !self.right_click.enabled {
            return Ok(());
        }
        let storage = self.right_click.storage.clone();
        let target = self.right_click.target.clone();
        if storage.is_none() && target.is_none() {
            return Ok(());
        }

        let mut host = EngineKagHost::new(runtime);
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
        message_layer.waiting_for_click = false;
        Ok(())
    }

    fn apply_handler_step(&mut self, tag: Tag, value: Variant) -> Result<TagAction> {
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
        Ok(match step {
            0 => TagAction::Continue,
            -5 => {
                self.pending_tags.push_front(tag);
                TagAction::Yield(KagYieldReason::HandlerYield)
            }
            -4 => TagAction::Yield(KagYieldReason::HandlerYield),
            -3 => {
                self.pending_tags.push_front(tag);
                TagAction::Yield(KagYieldReason::HandlerYield)
            }
            -2 => TagAction::Yield(KagYieldReason::HandlerYield),
            -1 => {
                self.state = KagTaskState::Finished;
                TagAction::Yield(KagYieldReason::Finished)
            }
            n if n > 0 => {
                self.state = KagTaskState::WaitingTimer {
                    remaining: Duration::from_millis(n as u64),
                };
                TagAction::Yield(KagYieldReason::Waiting(self.state.clone()))
            }
            _ => TagAction::Yield(KagYieldReason::HandlerYield),
        })
    }

    fn wait(&mut self, state: KagTaskState) -> TagAction {
        self.state = state;
        TagAction::Yield(KagYieldReason::Waiting(self.state.clone()))
    }

    fn wait_click(
        &mut self,
        message_layer: &mut MessageLayerModel,
        clear_page_on_click: bool,
    ) -> TagAction {
        message_layer.waiting_for_click = true;
        self.clear_page_on_click = clear_page_on_click;
        self.wait(KagTaskState::WaitingClick)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TagAction {
    Continue,
    Yield(KagYieldReason),
}

fn call_tag_handler(
    runtime: &mut Runtime<KrkrHost>,
    handler: ObjectHandle,
    name: &str,
    args: Vec<Variant>,
) -> Result<Variant> {
    runtime.call_object_method(handler, name, args)
}

fn tag_variant(runtime: &mut Runtime<KrkrHost>, tag: &Tag) -> Result<Variant> {
    let object = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(object, "Dictionary");
    runtime.set_object_member(object, "tagname", Variant::String(tag.tagname.clone()));
    for attribute in &tag.attributes {
        if let Attribute::Named { name, value } = attribute {
            runtime.set_object_member(object, name, attribute_value_to_variant(value));
        }
    }
    Ok(Variant::Object(object))
}

fn attribute_value_to_variant(value: &AttributeValue) -> Variant {
    raw_attribute_value_to_variant(value.raw())
}

fn raw_attribute_value_to_variant(value: &str) -> Variant {
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") {
        Variant::Integer(1)
    } else if value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no") {
        Variant::Integer(0)
    } else {
        Variant::String(value.to_string())
    }
}

fn tag_millis(tag: &Tag, name: &str) -> Option<Duration> {
    tag.attr(name)
        .and_then(|value| value.raw().parse::<u64>().ok())
        .map(Duration::from_millis)
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
    execute_expression_on_runtime(runtime, "kag eval", expression).map(|_| ())
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

fn object_i64(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> Result<i64> {
    layer_object_member(runtime, object, name).to_integer()
}

fn object_optional_i64(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Result<Option<i64>> {
    match layer_object_member(runtime, object, name) {
        Variant::Void => Ok(None),
        value => value.to_integer().map(Some),
    }
}

fn layer_object_member(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> Variant {
    let direct = runtime.object_member(object, name);
    if !runtime.variant_is_property(&direct) && !matches!(direct, Variant::Void) {
        return direct;
    }
    let stored = runtime.object_member(object, &layer_property_backing_key(name));
    if !matches!(stored, Variant::Void) {
        return stored;
    }
    if runtime.variant_is_property(&direct) {
        Variant::Void
    } else {
        direct
    }
}

fn layer_property_backing_key(name: &str) -> Cow<'static, str> {
    match name {
        "window" => Cow::Borrowed("__nativeLayerProperty$window"),
        "parent" => Cow::Borrowed("__nativeLayerProperty$parent"),
        "children" => Cow::Borrowed("__nativeLayerProperty$children"),
        "order" => Cow::Borrowed("__nativeLayerProperty$order"),
        "absoluteOrderMode" => Cow::Borrowed("__nativeLayerProperty$absoluteOrderMode"),
        "visible" => Cow::Borrowed("__nativeLayerProperty$visible"),
        "nodeVisible" => Cow::Borrowed("__nativeLayerProperty$nodeVisible"),
        "opacity" => Cow::Borrowed("__nativeLayerProperty$opacity"),
        "isPrimary" => Cow::Borrowed("__nativeLayerProperty$isPrimary"),
        "left" => Cow::Borrowed("__nativeLayerProperty$left"),
        "top" => Cow::Borrowed("__nativeLayerProperty$top"),
        "width" => Cow::Borrowed("__nativeLayerProperty$width"),
        "height" => Cow::Borrowed("__nativeLayerProperty$height"),
        "imageLeft" => Cow::Borrowed("__nativeLayerProperty$imageLeft"),
        "imageTop" => Cow::Borrowed("__nativeLayerProperty$imageTop"),
        "imageWidth" => Cow::Borrowed("__nativeLayerProperty$imageWidth"),
        "imageHeight" => Cow::Borrowed("__nativeLayerProperty$imageHeight"),
        "type" => Cow::Borrowed("__nativeLayerProperty$type"),
        "face" => Cow::Borrowed("__nativeLayerProperty$face"),
        "hitType" => Cow::Borrowed("__nativeLayerProperty$hitType"),
        "hitThreshold" => Cow::Borrowed("__nativeLayerProperty$hitThreshold"),
        "cursor" => Cow::Borrowed("__nativeLayerProperty$cursor"),
        "hint" => Cow::Borrowed("__nativeLayerProperty$hint"),
        "showParentHint" => Cow::Borrowed("__nativeLayerProperty$showParentHint"),
        "enabled" => Cow::Borrowed("__nativeLayerProperty$enabled"),
        "nodeEnabled" => Cow::Borrowed("__nativeLayerProperty$nodeEnabled"),
        "font" => Cow::Borrowed("__nativeLayerProperty$font"),
        _ => Cow::Owned(format!("__nativeLayerProperty${name}")),
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
            apply_completed_image_load(runtime, completion)?;
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

fn is_builtin_tag(tagname: &str) -> bool {
    matches!(
        tagname,
        "ch" | "r"
            | "p"
            | "l"
            | "font"
            | "deffont"
            | "resetfont"
            | "style"
            | "locate"
            | "ptext"
            | "wait"
            | "waitclick"
            | "eval"
            | "trace"
            | "cm"
            | "ct"
            | "er"
            | "image"
            | "layopt"
            | "position"
            | "freeimage"
            | "backlay"
            | "rclick"
            | "tempsave"
            | "tempload"
            | "commit"
            | "history"
            | "gotostart"
            | "laycount"
            | "current"
            | "trans"
            | "wt"
            | "wq"
            | "wf"
            | "wb"
            | "wm"
            | "waitload"
            | "waittrig"
            | "defstyle"
            | "resetstyle"
            | "ruby"
            | "s"
    )
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
    use krkr_tjs2::{
        Result,
        runtime::{Runtime, Variant},
    };

    use super::*;
    use crate::{KrkrHost, KrkrPlugin};

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
                    (window.focusedLayer === layer) + ":" +
                    (Window.mainWindow === window);
                window.add(layer);
                var deduped = window.children.count;
                window.remove(layer);
                return before + ":" + deduped + ":" + window.children.count + ":" +
                    (window.primaryLayer === void) + ":" +
                    (window.focusedLayer === void);
                "#,
            )
            .expect("script");

        assert_eq!(value, Variant::String("1:1:1:1:1:0:1:1".to_string()));
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
        assert_eq!(engine.message_layer.lines, vec!["ABC".to_string()]);

        let tag = test_tag("tempload", &[("place", "1")]);
        let action = engine
            .kag_task
            .process_builtin_tag(
                &mut engine.kag_parser,
                &mut engine.tjs_runtime,
                &mut engine.message_layer,
                &tag,
            )
            .expect("tempload");
        assert_eq!(action, TagAction::Continue);
        assert_eq!(engine.message_layer.lines, vec!["AB".to_string()]);

        assert_eq!(
            engine.tick().expect("restored tick").state,
            KagTaskState::Finished
        );
        assert_eq!(engine.message_layer.lines, vec!["ABC".to_string()]);

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
        assert_eq!(engine.message_layer.lines, vec!["A".to_string()]);

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
        assert_eq!(engine.message_layer.lines, vec!["AC".to_string()]);

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
                return parser.snapshot.curStorage;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("first.ks".to_string()));

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
                timerProbe.interval = 0;
                timerProbe.enabled = true;
                "#,
            )
            .expect("script");
        assert_eq!(engine.host().timer_handles().len(), 1);

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
        let Variant::Object(timer) = engine.tjs_runtime().global_member("timerProbe") else {
            panic!("timerProbe should be an object");
        };
        assert_eq!(
            engine.tjs_runtime().object_member(timer, "enabled"),
            Variant::Integer(0)
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
                timerProbe.interval = 0;
                timerProbe.enabled = true;
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
            engine.tjs_runtime().global_member("timerProbeCount"),
            Variant::Integer(1)
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
        assert_eq!(engine.host_mut().take_pending_async_triggers().len(), 1);
        let async_probe = match engine.tjs_runtime().global_member("asyncProbe") {
            Variant::Object(handle) => handle,
            _ => panic!("asyncProbe should be an object"),
        };
        engine.host_mut().trigger_async(async_probe);

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
