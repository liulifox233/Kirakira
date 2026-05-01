use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};

use krkr_core::{
    ButtonState, Engine as CoreEngine, EngineConfig as CoreEngineConfig, EngineEvent, EngineKey,
    FrameInput, FrameOutput, LayerId, MessageLayerModel, Point, PointerButton, Size,
};
use krkr_kag::{Attribute, AttributeValue, KagParser, Tag};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::{
    globals::install_tvp_globals,
    host::KrkrHost,
    kag::EngineKagHost,
    native::classes::finish_completed_native_transitions,
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
            max_wall_time: Duration::from_millis(2),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EngineConfig {
    pub project_root: Option<PathBuf>,
    pub kag_budget: KagRunBudget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KagTaskState {
    Running,
    WaitingClick,
    WaitingTimer { remaining: Duration },
    WaitingTransition,
    WaitingAudio,
    WaitingResource,
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
}

impl KrkrEngine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let host = match config.project_root {
            Some(root) => KrkrHost::for_project(root)?,
            None => KrkrHost::default(),
        };
        let mut tjs_runtime = Runtime::with_host(host);
        install_tvp_globals(&mut tjs_runtime);
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
        let Variant::Object(window) = self.tjs_runtime.global_member("kag") else {
            return None;
        };
        let width = object_positive_i64(&self.tjs_runtime, window, "innerWidth")
            .or_else(|| object_positive_i64(&self.tjs_runtime, window, "width"))?;
        let height = object_positive_i64(&self.tjs_runtime, window, "innerHeight")
            .or_else(|| object_positive_i64(&self.tjs_runtime, window, "height"))?;
        Some(Size::new(width as f32, height as f32))
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
        self.handle_input_events(&input.events)?;
        self.pump_tjs_events()?;
        let tick = self.advance(delta)?;
        self.pump_layer_paints()?;
        self.sync_native_layers_from_tjs()?;
        let suppressed_images = self.tjs_runtime.host().suppressed_transition_live_images();
        let output = self
            .core_engine
            .tick_running_with_layers_suppressing_images(
                input.frame,
                self.tjs_runtime.host().layer_tree(),
                &self.message_layer,
                &suppressed_images,
            )
            .with_transition(self.tjs_runtime.host().frame_transition());
        Ok(EngineFrame {
            output,
            tick,
            message_layer: self.message_layer.clone(),
            location: self.kag_location(),
        })
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
        self.tjs_runtime.host_mut().advance_transition(delta);
        finish_completed_native_transitions(&mut self.tjs_runtime)?;
        let transition_active = self.tjs_runtime.host().has_active_transition();
        self.kag_task.update_wait(delta, transition_active);
        self.kag_task.run_until_yield(
            &mut self.kag_parser,
            &mut self.tjs_runtime,
            &mut self.message_layer,
            self.kag_budget,
        )
    }

    fn pump_tjs_events(&mut self) -> Result<()> {
        const MAX_NATIVE_EVENT_PASSES: usize = 1024;

        for _ in 0..MAX_NATIVE_EVENT_PASSES {
            let events = self.collect_due_tjs_events()?;
            if events.is_empty() {
                return Ok(());
            }
            for event in events {
                self.fire_tjs_event(event)?;
            }
        }

        self.tjs_runtime
            .host_mut()
            .log("native event pump reached its per-frame pass budget; remaining events deferred");
        Ok(())
    }

    fn collect_due_tjs_events(&mut self) -> Result<Vec<TjsEvent>> {
        let now = self.tjs_runtime.host_mut().now_millis();
        let mut events = self
            .tjs_runtime
            .host_mut()
            .take_pending_async_triggers()
            .into_iter()
            .map(|handle| TjsEvent {
                handle,
                kind: TjsEventKind::AsyncTrigger,
            })
            .collect::<Vec<_>>();

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

    fn handle_input_events(&mut self, events: &[EngineEvent]) -> Result<()> {
        for event in events {
            match event {
                EngineEvent::CursorMoved { position } => {
                    self.cursor_position = Some(*position);
                    self.dispatch_layer_cursor_move(*position)?;
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Pressed,
                } => {
                    let target = self.dispatch_layer_pointer_event("onMouseDown")?;
                    if self.should_fire_primary_click(target) {
                        self.fire_kag_primary_click(false)?;
                    }
                }
                EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Released,
                } => {
                    self.dispatch_layer_pointer_event("onMouseUp")?;
                    self.signal_kag_click();
                }
                EngineEvent::KeyboardInput {
                    key: EngineKey::Enter | EngineKey::Space,
                    state: ButtonState::Pressed,
                } => {
                    self.fire_kag_primary_click(true)?;
                    self.signal_kag_click();
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn dispatch_layer_pointer_event(&mut self, method: &str) -> Result<Option<ObjectHandle>> {
        let Some(position) = self.cursor_position else {
            return Ok(None);
        };
        let Some(layer_id) = self.tjs_runtime.host().layer_tree().hit_test(position) else {
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
        self.tjs_runtime
            .call_object_method(
                object,
                method,
                vec![
                    Variant::Integer(x),
                    Variant::Integer(y),
                    Variant::Integer(0),
                    Variant::Integer(0),
                ],
            )
            .map(|_| Some(object))
    }

    fn should_fire_primary_click(&self, target: Option<ObjectHandle>) -> bool {
        let Some(target) = target else {
            return false;
        };
        matches!(
            self.tjs_runtime.object_member(target, "linkNum"),
            Variant::Void
        ) && !self
            .tjs_runtime
            .object_member(target, "isPrimary")
            .is_truthy()
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

    fn dispatch_layer_cursor_move(&mut self, position: Point) -> Result<()> {
        let hit_layer = self.tjs_runtime.host().layer_tree().hit_test(position);
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
                    Variant::Integer(0),
                ],
            )?;
        }
        Ok(())
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
            let absolute = object_optional_i64(&self.tjs_runtime, handle, "absolute")?;
            let order = object_optional_i64(&self.tjs_runtime, handle, "order")?;

            let kag_target = self.kag_object_layer_target(handle);
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
                if let Some(z_order) = absolute.or(order) {
                    layer.z_order = z_order.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                }
            }
        }
        Ok(())
    }

    fn kag_object_layer_target(&self, handle: ObjectHandle) -> Option<(String, String)> {
        let handle = self.tjs_runtime.bound_this(handle).unwrap_or(handle);
        let Variant::Object(kag) = self.tjs_runtime.global_member("kag") else {
            return None;
        };

        for page in ["fore", "back"] {
            let Variant::Object(page_object) = self.tjs_runtime.object_member(kag, page) else {
                continue;
            };
            if self.same_object(self.tjs_runtime.object_member(page_object, "base"), handle) {
                return Some((page.to_string(), "base".to_string()));
            }
            if let Some(index) = self.kag_layer_array_index(page_object, "layers", handle) {
                return Some((page.to_string(), index.to_string()));
            }
            if let Some(index) = self.kag_layer_array_index(page_object, "messages", handle) {
                return Some((page.to_string(), format!("message{index}")));
            }
        }

        None
    }

    fn kag_layer_array_index(
        &self,
        page_object: ObjectHandle,
        member: &str,
        handle: ObjectHandle,
    ) -> Option<i64> {
        let Variant::Object(array) = self.tjs_runtime.object_member(page_object, member) else {
            return None;
        };
        let Ok(count) = self.tjs_runtime.object_member(array, "count").to_integer() else {
            return None;
        };
        (0..count.max(0)).find(|index| {
            self.same_object(
                self.tjs_runtime.object_member(array, &index.to_string()),
                handle,
            )
        })
    }

    fn same_object(&self, value: Variant, handle: ObjectHandle) -> bool {
        let Variant::Object(candidate) = value else {
            return false;
        };
        self.tjs_runtime.bound_this(candidate).unwrap_or(candidate) == handle
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
struct TjsEvent {
    handle: ObjectHandle,
    kind: TjsEventKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TjsEventKind {
    Timer,
    AsyncTrigger,
}

#[derive(Clone, Debug)]
struct KagRuntimeTask {
    state: KagTaskState,
    handler: Option<ObjectHandle>,
    pending_tags: VecDeque<Tag>,
    loaded: bool,
    clear_page_on_click: bool,
}

impl KagRuntimeTask {
    fn new() -> Self {
        Self {
            state: KagTaskState::Finished,
            handler: None,
            pending_tags: VecDeque::new(),
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

    fn update_wait(&mut self, delta: Duration, transition_active: bool) {
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
            let action = self.process_tag(runtime, message_layer, tag)?;
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

        let default_action = self.process_builtin_tag(runtime, message_layer, &tag)?;
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
            "image" => {
                apply_image_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            "layopt" | "position" => {
                apply_layer_options_tag(runtime, tag)?;
                Ok(TagAction::Continue)
            }
            "freeimage" => {
                apply_freeimage_tag(runtime, tag);
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
            "wq" | "wf" | "wb" | "wm" => Ok(self.wait(KagTaskState::WaitingAudio)),
            "waitload" | "waittrig" => Ok(self.wait(KagTaskState::WaitingResource)),
            "s" => {
                self.state = KagTaskState::Finished;
                Ok(TagAction::Yield(KagYieldReason::Finished))
            }
            _ => Ok(TagAction::Continue),
        }
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

fn object_i64(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> Result<i64> {
    runtime.object_member(object, name).to_integer()
}

fn object_optional_i64(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Result<Option<i64>> {
    match runtime.object_member(object, name) {
        Variant::Void => Ok(None),
        value => value.to_integer().map(Some),
    }
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

fn apply_image_tag(runtime: &mut Runtime<KrkrHost>, tag: &Tag) -> Result<()> {
    let storage = tag
        .literal_attr("storage")
        .ok_or_else(|| TjsError::runtime("KAG image tag requires storage"))?;
    let (page, layer_name) = kag_target(runtime, tag);
    let image = runtime.host_mut().load_image_storage(storage)?;
    let image_size = image.size();
    let has_explicit_width = tag.literal_attr("width").is_some();
    let has_explicit_height = tag.literal_attr("height").is_some();
    runtime
        .host_mut()
        .mutate_kag_layer(&page, &layer_name, |layer| {
            layer.set_image(image);
            if !has_explicit_width {
                layer.width = image_size.width;
            }
            if !has_explicit_height {
                layer.height = image_size.height;
            }
            layer.visible = kag_bool_attr(tag, "visible").unwrap_or(true);
            apply_tag_geometry(layer, tag)
        })
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
            | "image"
            | "layopt"
            | "position"
            | "freeimage"
            | "current"
            | "trans"
            | "wt"
            | "wq"
            | "wf"
            | "wb"
            | "wm"
            | "waitload"
            | "waittrig"
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
                var layer = new Layer();
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
                var layer = new Layer();
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
                kag.fore = %[base: new Layer(), layers: [], messages: []];
                kag.back = %[base: new Layer(null, kag.fore.base), layers: [], messages: []];
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

        fs::remove_dir_all(root).expect("cleanup");
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
        assert_eq!(frame.output.image_uploads.len(), 2);
        assert_eq!(
            frame
                .output
                .draw_commands
                .iter()
                .filter(|command| matches!(command, krkr_core::DrawCommand::Image(_)))
                .count(),
            2
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

    fn temp_root() -> PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "krkr-ruri-engine-{}-{nanos}-{id}",
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
        })
        .expect("engine")
    }
}
