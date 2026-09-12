//! Runtime-facing Emote player state.
//!
//! This module keeps the public player/control surface alive while the original
//! mesh deformation path is still being recovered from `StepFrameMeshChain`.
//! Variable writes are accepted, queued, progressed, and queryable. Timeline
//! variable tracks from `metadata/timelineControl` are evaluated every frame and
//! feed the same variable map used by the renderer-side mesh deformation path.

use crate::api::{EmotePlayerControl, TimelinePlayMode, VariableWrite};
use crate::{load_emote_static_scene, EmoteSceneBounds, EmoteSchemaError, EmoteStaticScene, PsbFile, PsbValue};
use std::collections::BTreeMap;
#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(debug_assertions)]
static LOOP_WARNING_PRINTED: AtomicBool = AtomicBool::new(false);
#[cfg(debug_assertions)]
static MIRROR_WARNING_PRINTED: AtomicBool = AtomicBool::new(false);
#[cfg(debug_assertions)]
static OPAQUE_CONTROL_WARNING_PRINTED: AtomicBool = AtomicBool::new(false);

const CONTROL_METADATA_KEYS: &[&str] = &[
    "bustControl",
    "hairControl",
    "partsControl",
    "eyeControl",
    "eyebrowControl",
    "mouthControl",
    "transitionControl",
    "selectorControl",
    "clampControl",
    "loopControl",
    "mirrorControl",
    "stereovisionControl",
];

fn is_auto_control_timeline_name(name: &str) -> bool {
    name.starts_with("@control/")
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteVariableInfo {
    pub name: String,
    pub default_value: f32,
    pub min_value: Option<f32>,
    pub max_value: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteVariableState {
    pub info: EmoteVariableInfo,
    pub value: f32,
    pub target: Option<EmoteVariableTarget>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteVariableTarget {
    pub start_value: f32,
    pub target_value: f32,
    pub elapsed_ticks: f32,
    pub duration_ticks: f32,
    pub easing: f32,
}


#[derive(Debug, Clone, PartialEq)]
pub struct EmoteTimelineFrame {
    pub time_ticks: f32,
    pub value: f32,
    pub easing: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteTimelineVariable {
    pub name: String,
    pub frames: Vec<EmoteTimelineFrame>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteTimeline {
    pub name: String,
    pub path: Option<String>,
    pub duration_ticks: f32,
    pub variables: Vec<EmoteTimelineVariable>,
}

#[derive(Debug, Clone, PartialEq)]
struct ActiveTimelineState {
    mode: TimelinePlayMode,
    elapsed_ticks: f32,
}

/// Runtime state for one EPBustControl spring (one entry per bustControl item).
/// Fields confirmed from sub_101D4300 decompilation (see docs/emote.sqlite).
#[derive(Debug, Clone, PartialEq)]
pub struct BustPhysicsState {
    /// Current bob position in model (screen Y-down) coordinates.
    pub bob: [f32; 3],
    /// Current bob velocity.
    pub vel: [f32; 3],
    /// Equilibrium anchor-to-bob Y displacement (negative of param.ofs).
    /// At rest: (anchor.y - bob.y) == ofs, so var_ud == 0.
    pub ofs: f32,
    /// Set on the first tick to record the initial anchor offset.
    pub first_tick: bool,
    /// Stored offset from world anchor (used to track anchor movement).
    pub anchor_offset: [f32; 2],
}

/// Runtime state for one EPPendControl two-segment pendulum (per hairControl item).
/// Fields confirmed from sub_10201AB0 decompilation (see docs/emote.sqlite).
#[derive(Debug, Clone, PartialEq)]
pub struct HairPhysicsState {
    /// Bob position for each of the 2 segments.
    pub bob: [[f32; 3]; 2],
    /// Velocity for each segment.
    pub vel: [[f32; 3]; 2],
    /// Equilibrium ud displacement stored from pre-computation.
    pub ofs: f32,
    /// Set on the first tick to record initial anchor offset.
    pub first_tick: bool,
    /// Stored offset from world anchor.
    pub anchor_offset: [f32; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElunaPlayer {
    shown: bool,
    coord: [f32; 2],
    scale: f32,
    rot: f32,
    elapsed_ticks: f32,
    pub paused: bool,
    pub physics_enabled: bool,
    scene: EmoteStaticScene,
    variables: BTreeMap<String, EmoteVariableState>,
    timelines: BTreeMap<String, EmoteTimeline>,
    pending_writes: Vec<VariableWrite>,
    active_timelines: BTreeMap<String, TimelinePlayMode>,
    active_timeline_states: BTreeMap<String, ActiveTimelineState>,
    runtime_pipeline: EmoteRuntimePipeline,
    bust_states: Vec<BustPhysicsState>,
    hair_states: Vec<HairPhysicsState>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmoteRuntimePipeline {
    pub instant_variables: Vec<String>,
    pub selector_controls: Vec<SelectorControl>,
    pub clamp_controls: Vec<ClampControl>,
    pub loop_controls: Vec<LoopControl>,
    pub mirror_control: Option<MirrorControl>,
    pub transition_controls: Vec<TransitionControl>,
    pub physics_controls: Vec<PhysicsControl>,
    pub parts_controls: Vec<OpaqueControl>,
    pub eye_controls: Vec<OpaqueControl>,
    pub eyebrow_controls: Vec<OpaqueControl>,
    pub mouth_controls: Vec<OpaqueControl>,
    pub unsupported_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectorControl {
    pub label: String,
    pub enabled: bool,
    pub option_list: Vec<SelectorOption>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectorOption {
    pub label: String,
    pub off_value: f32,
    pub on_value: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClampControl {
    pub label: String,
    pub enabled: bool,
    pub kind: i64,
    pub var_lr: String,
    pub var_ud: String,
    pub min: f32,
    pub max: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoopControl {
    pub label: Option<String>,
    pub enabled: bool,
    pub var_loop: Option<String>,
    pub transition_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirrorControl {
    pub variable_match_list: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransitionControl {
    pub label: String,
    pub enabled: bool,
    pub fade: Option<f32>,
    pub diff: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PhysicsControl {
    Bust(PhysicsControlDefinition),
    Hair(PhysicsControlDefinition),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicsControlDefinition {
    pub label: String,
    pub enabled: bool,
    pub base_layer: Option<String>,
    pub parameter: Option<String>,
    pub var_lr: Option<String>,
    pub var_ud: Option<String>,
    pub var_lrm: Option<String>,
    pub fields: BTreeMap<String, PsbValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpaqueControl {
    pub label: Option<String>,
    pub enabled: bool,
    pub fields: BTreeMap<String, PsbValue>,
}

impl ElunaPlayer {
    pub fn from_psb(psb: &PsbFile) -> Result<Self, EmoteSchemaError> {
        let (_schema, scene) = load_emote_static_scene(psb)?;
        Ok(Self::from_scene_variables_timelines_runtime(
            scene,
            collect_emote_variables(psb),
            collect_emote_timelines(psb),
            collect_emote_runtime_pipeline(psb),
        ))
    }

    pub fn from_scene(scene: EmoteStaticScene) -> Self {
        Self::from_scene_and_variables(scene, Vec::new())
    }

    pub fn scene(&self) -> &EmoteStaticScene {
        &self.scene
    }

    pub fn replace_scene(&mut self, scene: EmoteStaticScene) {
        self.scene = scene;
    }

    pub fn bounds(&self) -> Option<EmoteSceneBounds> {
        self.scene.bounds
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    pub fn elapsed_ticks(&self) -> f32 {
        self.elapsed_ticks
    }

    pub fn variables(&self) -> &BTreeMap<String, EmoteVariableState> {
        &self.variables
    }

    pub fn timelines(&self) -> &BTreeMap<String, EmoteTimeline> {
        &self.timelines
    }

    pub fn default_timeline_name(&self) -> Option<&str> {
        self.timelines.keys().next().map(String::as_str)
    }

    pub fn variable_value(&self, name: &str) -> Option<f32> {
        self.variables.get(name).map(|state| state.value)
    }

    pub fn pending_writes(&self) -> &[VariableWrite] {
        &self.pending_writes
    }

    pub fn active_timelines(&self) -> &BTreeMap<String, TimelinePlayMode> {
        &self.active_timelines
    }

    pub fn apply_write(&mut self, write: VariableWrite) {
        self.set_variable_timed(&write.name, write.value, write.time_ticks, write.easing);
    }

    pub fn from_scene_and_variables(scene: EmoteStaticScene, infos: Vec<EmoteVariableInfo>) -> Self {
        Self::from_scene_variables_timelines(scene, infos, Vec::new())
    }

    pub fn from_scene_variables_timelines(
        scene: EmoteStaticScene,
        infos: Vec<EmoteVariableInfo>,
        timelines: Vec<EmoteTimeline>,
    ) -> Self {
        Self::from_scene_variables_timelines_runtime(scene, infos, timelines, EmoteRuntimePipeline::default())
    }

    pub fn from_scene_variables_timelines_runtime(
        scene: EmoteStaticScene,
        infos: Vec<EmoteVariableInfo>,
        timelines: Vec<EmoteTimeline>,
        runtime_pipeline: EmoteRuntimePipeline,
    ) -> Self {
        let mut variables = BTreeMap::new();
        for info in infos {
            let name = info.name.clone();
            variables.entry(name).or_insert_with(|| EmoteVariableState {
                value: info.default_value,
                info,
                target: None,
            });
        }

        let mut timeline_map = BTreeMap::new();
        for timeline in timelines {
            for variable in &timeline.variables {
                let first_value = variable.frames.first().map(|f| f.value).unwrap_or(0.0);
                variables.entry(variable.name.clone()).or_insert_with(|| EmoteVariableState {
                    info: EmoteVariableInfo {
                        name: variable.name.clone(),
                        default_value: first_value,
                        min_value: None,
                        max_value: None,
                    },
                    value: first_value,
                    target: None,
                });
                if let Some(state) = variables.get_mut(&variable.name) {
                    if (state.value - state.info.default_value).abs() <= f32::EPSILON {
                        state.value = clamp_variable_value(&state.info, first_value);
                    }
                }
            }
            timeline_map.insert(timeline.name.clone(), timeline);
        }

        let mut active_timelines = BTreeMap::new();
        let mut active_timeline_states = BTreeMap::new();
        for (name, timeline) in &timeline_map {
            if is_auto_control_timeline_name(name) {
                active_timelines.insert(name.clone(), TimelinePlayMode::Loop);
                active_timeline_states.insert(name.clone(), ActiveTimelineState {
                    mode: TimelinePlayMode::Loop,
                    elapsed_ticks: 0.0,
                });
                for variable in &timeline.variables {
                    let value = evaluate_timeline_variable(variable, 0.0);
                    let state = variables.entry(variable.name.clone()).or_insert_with(|| EmoteVariableState {
                        info: EmoteVariableInfo {
                            name: variable.name.clone(),
                            default_value: value,
                            min_value: None,
                            max_value: None,
                        },
                        value,
                        target: None,
                    });
                    state.value = clamp_variable_value(&state.info, value);
                    state.target = None;
                }
            }
        }

        let bust_states = init_bust_states(&runtime_pipeline);
        let hair_states = init_hair_states(&runtime_pipeline);

        let mut player = Self {
            shown: true,
            coord: [0.0, 0.0],
            scale: 1.0,
            rot: 0.0,
            elapsed_ticks: 0.0,
            paused: false,
            physics_enabled: true,
            scene,
            variables,
            timelines: timeline_map,
            pending_writes: Vec::new(),
            active_timelines,
            active_timeline_states,
            runtime_pipeline,
            bust_states,
            hair_states,
        };
        player.evaluate_runtime_pipeline(0.0);
        player
    }

    pub fn runtime_pipeline(&self) -> &EmoteRuntimePipeline {
        &self.runtime_pipeline
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn set_physics_enabled(&mut self, enabled: bool) {
        self.physics_enabled = enabled;
    }

    pub fn is_physics_enabled(&self) -> bool {
        self.physics_enabled
    }

    pub fn set_variable_immediate(&mut self, name: &str, value: f32) {
        if let Some(state) = self.variables.get_mut(name) {
            state.value = clamp_variable_value(&state.info, value);
            state.target = None;
        }
    }

    pub fn reset_variable_to_default(&mut self, name: &str) {
        if let Some(state) = self.variables.get_mut(name) {
            let default = state.info.default_value;
            state.value = default;
            state.target = None;
        }
    }

    pub fn reset_physics(&mut self) {
        self.bust_states = init_bust_states(&self.runtime_pipeline);
        self.hair_states = init_hair_states(&self.runtime_pipeline);
    }

    pub fn timeline_elapsed_ticks(&self, name: &str) -> f32 {
        self.active_timeline_states.get(name).map(|s| s.elapsed_ticks).unwrap_or(0.0)
    }

    pub fn set_timeline_time(&mut self, name: &str, ticks: f32) -> Result<(), String> {
        let Some(timeline) = self.timelines.get(name).cloned() else {
            return Err(format!("timeline not found: {name}"));
        };
        let duration = timeline.duration_ticks.max(0.0);
        let local_time = if duration > 0.0 {
            ticks.clamp(0.0, duration)
        } else {
            ticks.max(0.0)
        };
        let mode = self
            .active_timeline_states
            .get(name)
            .map(|state| state.mode)
            .or_else(|| self.active_timelines.get(name).copied())
            .unwrap_or(TimelinePlayMode::Once);
        self.active_timelines.insert(name.to_owned(), mode);
        self.active_timeline_states.insert(name.to_owned(), ActiveTimelineState {
            mode,
            elapsed_ticks: local_time,
        });
        self.apply_timeline_at(&timeline, local_time);
        self.evaluate_runtime_pipeline(0.0);
        Ok(())
    }

    pub fn set_selector_option(&mut self, label: &str, option_index: usize) -> Result<(), String> {
        let Some(control) = self
            .runtime_pipeline
            .selector_controls
            .iter()
            .find(|control| control.label == label)
        else {
            return Err(format!("selectorControl not found: {label}"));
        };
        if option_index >= control.option_list.len() {
            return Err(format!(
                "selectorControl {label} option index {option_index} out of range {}",
                control.option_list.len()
            ));
        }
        self.set_variable_immediate(label, option_index as f32);
        self.evaluate_runtime_pipeline(0.0);
        Ok(())
    }

    pub fn reset_all_variables_to_default(&mut self) {
        for state in self.variables.values_mut() {
            state.value = state.info.default_value;
            state.target = None;
        }
        self.evaluate_runtime_pipeline(0.0);
    }

    fn ensure_variable(&mut self, name: &str) -> &mut EmoteVariableState {
        self.variables.entry(name.to_owned()).or_insert_with(|| EmoteVariableState {
            info: EmoteVariableInfo {
                name: name.to_owned(),
                default_value: 0.0,
                min_value: None,
                max_value: None,
            },
            value: 0.0,
            target: None,
        })
    }

    fn progress_active_timelines(&mut self, delta_ticks: f32) {
        let names: Vec<String> = self.active_timeline_states.keys().cloned().collect();
        for name in names {
            let Some(timeline) = self.timelines.get(&name).cloned() else {
                self.active_timeline_states.remove(&name);
                self.active_timelines.remove(&name);
                continue;
            };
            let Some(state) = self.active_timeline_states.get_mut(&name) else {
                continue;
            };

            state.elapsed_ticks += delta_ticks;
            let mut local_time = state.elapsed_ticks;
            let duration = timeline.duration_ticks.max(0.0);
            if duration > 0.0 && local_time > duration {
                if state.mode == TimelinePlayMode::Once {
                    local_time = duration;
                    state.elapsed_ticks = duration;
                } else {
                    local_time = local_time.rem_euclid(duration);
                    state.elapsed_ticks = local_time;
                }
            }

            self.apply_timeline_at(&timeline, local_time);
        }
    }

    fn apply_timeline_at(&mut self, timeline: &EmoteTimeline, local_time: f32) {
        for variable in &timeline.variables {
            let value = evaluate_timeline_variable(variable, local_time);
            let state = self.ensure_variable(&variable.name);
            state.value = clamp_variable_value(&state.info, value);
            state.target = None;
        }
    }

    fn evaluate_runtime_pipeline(&mut self, delta_ticks: f32) {
        let pipeline = self.runtime_pipeline.clone();
        for control in &pipeline.selector_controls {
            evaluate_selector_control(control, &mut self.variables);
        }
        for control in &pipeline.clamp_controls {
            evaluate_clamp_control(control, &mut self.variables);
        }
        for control in &pipeline.loop_controls {
            evaluate_loop_control(control, &mut self.variables);
        }
        if let Some(control) = &pipeline.mirror_control {
            evaluate_mirror_control(control, &mut self.variables);
        }
        for control in &pipeline.transition_controls {
            evaluate_transition_control(control, &mut self.variables);
        }
        evaluate_opaque_controls("partsControl", &pipeline.parts_controls);
        evaluate_opaque_controls("eyeControl", &pipeline.eye_controls);
        evaluate_opaque_controls("eyebrowControl", &pipeline.eyebrow_controls);
        evaluate_opaque_controls("mouthControl", &pipeline.mouth_controls);

        if self.physics_enabled {
            let mut bust_idx = 0;
            let mut hair_idx = 0;
            for control in &pipeline.physics_controls {
                match control {
                    PhysicsControl::Bust(def) => {
                        if let Some(state) = self.bust_states.get_mut(bust_idx) {
                            step_bust_physics(state, def, delta_ticks, &mut self.variables);
                        }
                        bust_idx += 1;
                    }
                    PhysicsControl::Hair(def) => {
                        if let Some(state) = self.hair_states.get_mut(hair_idx) {
                            step_hair_physics(state, def, delta_ticks, &mut self.variables);
                        }
                        hair_idx += 1;
                    }
                }
            }
        }
    }
}

impl EmotePlayerControl for ElunaPlayer {
    fn show(&mut self) {
        self.shown = true;
    }

    fn hide(&mut self) {
        self.shown = false;
    }

    fn progress_ticks(&mut self, delta_ticks: f32) {
        if !delta_ticks.is_finite() || delta_ticks <= 0.0 {
            return;
        }
        self.elapsed_ticks += delta_ticks;

        if !self.paused {
            self.progress_active_timelines(delta_ticks);
        }
        self.evaluate_runtime_pipeline(delta_ticks);

        for state in self.variables.values_mut() {
            let Some(mut target) = state.target.take() else {
                continue;
            };

            target.elapsed_ticks = (target.elapsed_ticks + delta_ticks).min(target.duration_ticks.max(0.0));
            let t = if target.duration_ticks <= 0.0 {
                1.0
            } else {
                (target.elapsed_ticks / target.duration_ticks).clamp(0.0, 1.0)
            };
            let eased = preview_easing(t, target.easing);
            let mut value = target.start_value + (target.target_value - target.start_value) * eased;
            if let Some(min) = state.info.min_value {
                value = value.max(min);
            }
            if let Some(max) = state.info.max_value {
                value = value.min(max);
            }
            state.value = value;

            if t < 1.0 {
                state.target = Some(target);
            }
        }
    }

    fn render(&mut self) {}

    fn coord(&self) -> [f32; 2] {
        self.coord
    }

    fn set_coord(&mut self, x: f32, y: f32) {
        self.coord = [x, y];
    }

    fn scale(&self) -> f32 {
        self.scale
    }

    fn set_scale(&mut self, scale: f32) {
        if scale.is_finite() && scale > 0.0 {
            self.scale = scale;
        }
    }

    fn rot(&self) -> f32 {
        self.rot
    }

    fn set_rot(&mut self, rot: f32) {
        if rot.is_finite() {
            self.rot = rot;
        }
    }

    fn set_variable_timed(&mut self, name: &str, value: f32, time_ticks: f32, easing: f32) {
        if name.is_empty() || !value.is_finite() {
            return;
        }

        let state = self.ensure_variable(name);
        let mut target_value = value;
        if let Some(min) = state.info.min_value {
            target_value = target_value.max(min);
        }
        if let Some(max) = state.info.max_value {
            target_value = target_value.min(max);
        }

        if time_ticks <= 0.0 || !time_ticks.is_finite() {
            state.value = target_value;
            state.target = None;
        } else {
            state.target = Some(EmoteVariableTarget {
                start_value: state.value,
                target_value,
                elapsed_ticks: 0.0,
                duration_ticks: time_ticks,
                easing,
            });
        }

        self.pending_writes.push(VariableWrite::timed(name, target_value, time_ticks.max(0.0), easing));
    }

    fn play_timeline(&mut self, name: &str, mode: TimelinePlayMode) {
        if !name.is_empty() {
            self.active_timelines.insert(name.to_owned(), mode);
            self.active_timeline_states.insert(name.to_owned(), ActiveTimelineState {
                mode,
                elapsed_ticks: 0.0,
            });
            if let Some(timeline) = self.timelines.get(name).cloned() {
                self.apply_timeline_at(&timeline, 0.0);
                self.evaluate_runtime_pipeline(0.0);
            }
        }
    }

    fn stop_timeline(&mut self, name: &str) {
        self.active_timelines.remove(name);
        self.active_timeline_states.remove(name);
    }

    fn skip(&mut self) {
        for state in self.variables.values_mut() {
            if let Some(target) = state.target.take() {
                state.value = target.target_value;
            }
        }
    }
}

fn preview_easing(t: f32, easing: f32) -> f32 {
    if !easing.is_finite() || easing == 0.0 {
        return t;
    }

    // A conservative preview curve for interactive testing. The exact original
    // easing mapping still needs the shared SetVariable/easing path.
    if easing > 0.0 {
        t * t * (3.0 - 2.0 * t)
    } else {
        1.0 - (1.0 - t) * (1.0 - t)
    }
}


fn clamp_variable_value(info: &EmoteVariableInfo, mut value: f32) -> f32 {
    if let Some(min) = info.min_value {
        value = value.max(min);
    }
    if let Some(max) = info.max_value {
        value = value.min(max);
    }
    value
}

fn evaluate_timeline_variable(variable: &EmoteTimelineVariable, time_ticks: f32) -> f32 {
    if variable.frames.is_empty() {
        return 0.0;
    }
    if time_ticks <= variable.frames[0].time_ticks {
        return variable.frames[0].value;
    }

    let mut prev = &variable.frames[0];
    for next in &variable.frames[1..] {
        if time_ticks <= next.time_ticks {
            let span = (next.time_ticks - prev.time_ticks).max(0.0);
            if span <= f32::EPSILON {
                return next.value;
            }
            let t = ((time_ticks - prev.time_ticks) / span).clamp(0.0, 1.0);
            let eased = preview_easing(t, next.easing);
            return prev.value + (next.value - prev.value) * eased;
        }
        prev = next;
    }
    prev.value
}

pub fn collect_emote_runtime_pipeline(psb: &PsbFile) -> EmoteRuntimePipeline {
    let mut pipeline = EmoteRuntimePipeline::default();
    let Some(metadata) = psb.root.field("metadata") else {
        return pipeline;
    };

    pipeline.instant_variables = metadata
        .field("instantVariableList")
        .and_then(PsbValue::as_list)
        .map(|items| items.iter().filter_map(PsbValue::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    pipeline.selector_controls = parse_selector_controls(metadata.field("selectorControl"));
    pipeline.clamp_controls = parse_clamp_controls(metadata.field("clampControl"));
    pipeline.loop_controls = parse_loop_controls(metadata.field("loopControl"));
    pipeline.mirror_control = parse_mirror_control(metadata.field("mirrorControl"));
    pipeline.transition_controls = parse_transition_controls(metadata.field("transitionControl"));
    pipeline.physics_controls.extend(parse_physics_controls(metadata.field("bustControl"), true));
    pipeline.physics_controls.extend(parse_physics_controls(metadata.field("hairControl"), false));
    pipeline.parts_controls = parse_opaque_controls(metadata.field("partsControl"));
    pipeline.eye_controls = parse_opaque_controls(metadata.field("eyeControl"));
    pipeline.eyebrow_controls = parse_opaque_controls(metadata.field("eyebrowControl"));
    pipeline.mouth_controls = parse_opaque_controls(metadata.field("mouthControl"));

    if metadata.field("physicsVariableList").is_none() {
        pipeline.unsupported_fields.push("metadata.physicsVariableList is absent in this PSB".to_owned());
    }
    pipeline
}

fn parse_selector_controls(value: Option<&PsbValue>) -> Vec<SelectorControl> {
    value
        .and_then(PsbValue::as_list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|control| {
            let label = control.field_str("label")?.to_owned();
            let option_list = control
                .field("optionList")
                .and_then(PsbValue::as_list)
                .unwrap_or(&[])
                .iter()
                .filter_map(|option| {
                    Some(SelectorOption {
                        label: option.field_str("label")?.to_owned(),
                        off_value: option.field_f32("offValue")?,
                        on_value: option.field_f32("onValue")?,
                    })
                })
                .collect();
            Some(SelectorControl {
                label,
                enabled: control.field_i64("enabled").unwrap_or(1) != 0,
                option_list,
            })
        })
        .collect()
}

fn parse_clamp_controls(value: Option<&PsbValue>) -> Vec<ClampControl> {
    value
        .and_then(PsbValue::as_list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|control| {
            Some(ClampControl {
                label: control.field_str("label").unwrap_or("").to_owned(),
                enabled: control.field_i64("enabled").unwrap_or(1) != 0,
                kind: control.field_i64("type")?,
                var_lr: control.field_str("var_lr")?.to_owned(),
                var_ud: control.field_str("var_ud")?.to_owned(),
                min: control.field_f32("min")?,
                max: control.field_f32("max")?,
            })
        })
        .collect()
}

fn parse_loop_controls(value: Option<&PsbValue>) -> Vec<LoopControl> {
    value
        .and_then(PsbValue::as_list)
        .unwrap_or(&[])
        .iter()
        .map(|control| {
            let transition_labels = control
                .field("transitionList")
                .and_then(PsbValue::as_list)
                .unwrap_or(&[])
                .iter()
                .filter_map(|item| item.field_str("label").or_else(|| item.as_str()))
                .map(str::to_owned)
                .collect();
            LoopControl {
                label: control.field_str("label").map(str::to_owned),
                enabled: control.field_i64("enabled").unwrap_or(1) != 0,
                var_loop: control.field_str("var_loop").map(str::to_owned),
                transition_labels,
            }
        })
        .collect()
}

fn parse_mirror_control(value: Option<&PsbValue>) -> Option<MirrorControl> {
    let value = value?;
    let variable_match_list = value
        .field("variableMatchList")
        .and_then(PsbValue::as_list)
        .unwrap_or(&[])
        .iter()
        .filter_map(PsbValue::as_str)
        .map(str::to_owned)
        .collect();
    Some(MirrorControl { variable_match_list })
}

fn parse_transition_controls(value: Option<&PsbValue>) -> Vec<TransitionControl> {
    value
        .and_then(PsbValue::as_list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|control| {
            Some(TransitionControl {
                label: control.field_str("label")?.to_owned(),
                enabled: control.field_i64("enabled").unwrap_or(1) != 0,
                fade: control.field_f32("fade"),
                diff: control.field_f32("diff"),
            })
        })
        .collect()
}

fn parse_physics_controls(value: Option<&PsbValue>, bust: bool) -> Vec<PhysicsControl> {
    value
        .and_then(PsbValue::as_list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|control| {
            let definition = PhysicsControlDefinition {
                label: control.field_str("label").unwrap_or("").to_owned(),
                enabled: control.field_i64("enabled").unwrap_or(1) != 0,
                base_layer: control.field_str("baseLayer").map(str::to_owned),
                parameter: control.field_str("parameter").map(str::to_owned),
                var_lr: control.field_str("var_lr").map(str::to_owned),
                var_ud: control.field_str("var_ud").map(str::to_owned),
                var_lrm: control.field_str("var_lrm").map(str::to_owned),
                fields: object_to_map(control)?,
            };
            Some(if bust {
                PhysicsControl::Bust(definition)
            } else {
                PhysicsControl::Hair(definition)
            })
        })
        .collect()
}

fn parse_opaque_controls(value: Option<&PsbValue>) -> Vec<OpaqueControl> {
    value
        .and_then(PsbValue::as_list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|control| {
            Some(OpaqueControl {
                label: control.field_str("label").map(str::to_owned),
                enabled: control.field_i64("enabled").unwrap_or(1) != 0,
                fields: object_to_map(control)?,
            })
        })
        .collect()
}

fn object_to_map(value: &PsbValue) -> Option<BTreeMap<String, PsbValue>> {
    Some(value.as_object()?.iter().cloned().collect())
}

/// Initialize bust physics states from the pipeline's bust controls.
/// Initial bob position from param.p, velocity from param.pv.
/// ofs (equilibrium displacement) = -(param.ofs): when bob.y = anchor.y + param.ofs
/// (screen Y-down), displacement.y = -param.ofs = ofs, so var_ud = 0 at rest.
fn init_bust_states(pipeline: &EmoteRuntimePipeline) -> Vec<BustPhysicsState> {
    let mut out = Vec::new();
    for control in &pipeline.physics_controls {
        let PhysicsControl::Bust(def) = control else { continue };
        let param = def.fields.get("param");
        let p = parse_vec3_field(param, "p");
        let pv = parse_vec3_field(param, "pv");
        let param_ofs = param.and_then(|v| v.field_f32("ofs")).unwrap_or(0.0);
        out.push(BustPhysicsState {
            bob: p,
            vel: pv,
            ofs: -param_ofs,
            first_tick: true,
            anchor_offset: [0.0, 0.0],
        });
    }
    out
}

/// Initialize hair physics states from the pipeline's hair controls.
fn init_hair_states(pipeline: &EmoteRuntimePipeline) -> Vec<HairPhysicsState> {
    let mut out = Vec::new();
    for control in &pipeline.physics_controls {
        let PhysicsControl::Hair(def) = control else { continue };
        let param = def.fields.get("param");
        // p is a list of 2 vec3s for the 2 segments
        let p0 = parse_vec3_from_list(param, "p", 0);
        let p1 = parse_vec3_from_list(param, "p", 1);
        let pv0 = parse_vec3_from_list(param, "pv", 0);
        let pv1 = parse_vec3_from_list(param, "pv", 1);
        let param_ofs = param.and_then(|v| v.field_f32("ofs")).unwrap_or(0.0);
        out.push(HairPhysicsState {
            bob: [p0, p1],
            vel: [pv0, pv1],
            ofs: param_ofs,
            first_tick: true,
            anchor_offset: [0.0, 0.0],
        });
    }
    out
}

fn parse_vec3_field(parent: Option<&PsbValue>, key: &str) -> [f32; 3] {
    let Some(v) = parent.and_then(|p| p.field(key)) else { return [0.0; 3] };
    [
        v.field_f32("x").unwrap_or(0.0),
        v.field_f32("y").unwrap_or(0.0),
        v.field_f32("z").unwrap_or(0.0),
    ]
}

fn parse_vec3_from_list(parent: Option<&PsbValue>, key: &str, index: usize) -> [f32; 3] {
    let Some(list) = parent.and_then(|p| p.field(key)).and_then(PsbValue::as_list) else { return [0.0; 3] };
    let Some(item) = list.get(index) else { return [0.0; 3] };
    [
        item.field_f32("x").unwrap_or(0.0),
        item.field_f32("y").unwrap_or(0.0),
        item.field_f32("z").unwrap_or(0.0),
    ]
}

fn evaluate_selector_control(control: &SelectorControl, variables: &mut BTreeMap<String, EmoteVariableState>) {
    if !control.enabled {
        return;
    }
    let selected = variables
        .get(&control.label)
        .map(|state| state.value.round() as isize)
        .unwrap_or(0);
    for (index, option) in control.option_list.iter().enumerate() {
        let value = if selected == index as isize {
            option.on_value
        } else {
            option.off_value
        };
        set_evaluated_variable(variables, &option.label, value);
    }
}

fn evaluate_clamp_control(control: &ClampControl, variables: &mut BTreeMap<String, EmoteVariableState>) {
    if !control.enabled {
        return;
    }
    let lr = variables.get(&control.var_lr).map(|state| state.value).unwrap_or(0.0);
    let ud = variables.get(&control.var_ud).map(|state| state.value).unwrap_or(0.0);
    let (next_lr, next_ud) = if control.kind == 0 {
        (lr.clamp(control.min, control.max), ud.clamp(control.min, control.max))
    } else {
        let radius = lr.hypot(ud);
        let max_radius = control.max.abs().max(f32::EPSILON);
        if radius > max_radius {
            let scale = max_radius / radius;
            (lr * scale, ud * scale)
        } else {
            (lr, ud)
        }
    };
    set_evaluated_variable(variables, &control.var_lr, next_lr);
    set_evaluated_variable(variables, &control.var_ud, next_ud);
}

fn evaluate_loop_control(control: &LoopControl, _variables: &mut BTreeMap<String, EmoteVariableState>) {
    if control.enabled && control.var_loop.is_some() {
        #[cfg(debug_assertions)]
        if !LOOP_WARNING_PRINTED.swap(true, Ordering::Relaxed) {
            eprintln!("loopControl is parsed; transition semantics are not confirmed");
        }
    }
}

fn evaluate_mirror_control(control: &MirrorControl, _variables: &mut BTreeMap<String, EmoteVariableState>) {
    if !control.variable_match_list.is_empty() {
        #[cfg(debug_assertions)]
        if !MIRROR_WARNING_PRINTED.swap(true, Ordering::Relaxed) {
            eprintln!("mirrorControl variableMatchList is parsed; mirror application semantics are not confirmed");
        }
    }
}

fn evaluate_transition_control(_control: &TransitionControl, _variables: &mut BTreeMap<String, EmoteVariableState>) {}

fn evaluate_opaque_controls(name: &str, controls: &[OpaqueControl]) {
    #[cfg(debug_assertions)]
    if !controls.is_empty() && !OPAQUE_CONTROL_WARNING_PRINTED.swap(true, Ordering::Relaxed) {
        let labels: Vec<&str> = controls.iter().filter_map(|control| control.label.as_deref()).take(8).collect();
        eprintln!("{name} is parsed and ticked through runtime; original side-effect semantics are not fully confirmed: labels={labels:?}");
    }
    #[cfg(not(debug_assertions))]
    let _ = (name, controls);
}

/// EPBustControl spring integration per tick.
/// Algorithm confirmed from sub_101D4300 decompilation (docs/emote.sqlite).
/// anchor = world position of baseLayer (we use 0,0 since we don't track it per-frame).
/// Screen Y-down coordinates: gravity direction = (0, +1, 0).
/// Equilibrium: spring * ofs_dist = gravity → ofs_dist = gravity / spring.
fn step_bust_physics(
    state: &mut BustPhysicsState,
    def: &PhysicsControlDefinition,
    delta_ticks: f32,
    variables: &mut BTreeMap<String, EmoteVariableState>,
) {
    if !def.enabled || delta_ticks <= 0.0 {
        return;
    }
    let dt = delta_ticks;
    let gravity = def.fields.get("gravity").and_then(|v| v.as_f32()).unwrap_or(0.0);
    let spring = def.fields.get("spring").and_then(|v| v.as_f32()).unwrap_or(0.0);
    let friction = def.fields.get("friction").and_then(|v| v.as_f32()).unwrap_or(0.0);
    let scale_x = def.fields.get("scale_x").and_then(|v| v.as_f32()).unwrap_or(1.0);
    let scale_y = def.fields.get("scale_y").and_then(|v| v.as_f32()).unwrap_or(1.0);

    // anchor = world position of baseLayer. We don't track per-layer world positions
    // in ElunaPlayer, so we use (0,0,0). This is correct for a stationary idle pose.
    let anchor = [0.0f32, 0.0, 0.0];

    // displacement = anchor - bob (spring restoring vector)
    let disp = [
        anchor[0] - state.bob[0],
        anchor[1] - state.bob[1],
        anchor[2] - state.bob[2],
    ];

    // gravity direction in screen Y-down coords: (0, +1, 0)
    // Confirmed: at rest with bob.y = ofs_dist below anchor, spring * ofs_dist = gravity.
    let grav_x = 0.0f32;
    let grav_y = 1.0f32;

    // vel += (spring * disp + gravity * grav_dir) * dt
    state.vel[0] += (spring * disp[0] + gravity * grav_x) * dt;
    state.vel[1] += (spring * disp[1] + gravity * grav_y) * dt;
    state.vel[2] += (spring * disp[2]) * dt;

    // vel *= (1 - friction * dt)
    let damp = 1.0 - friction * dt;
    state.vel[0] *= damp;
    state.vel[1] *= damp;
    state.vel[2] *= damp;

    // bob += vel * dt
    state.bob[0] += state.vel[0] * dt;
    state.bob[1] += state.vel[1] * dt;
    state.bob[2] += state.vel[2] * dt;

    // output displacement = anchor - bob (updated)
    let out_disp_x = anchor[0] - state.bob[0];
    let out_disp_y = anchor[1] - state.bob[1];

    // var_lr = -out_disp.x * scale_x
    // var_ud = -(out_disp.y - ofs) * scale_y  (ofs = equilibrium displacement.y)
    let var_lr = -out_disp_x * scale_x;
    let var_ud = -(out_disp_y - state.ofs) * scale_y;

    if let Some(name) = &def.var_lr {
        set_evaluated_variable(variables, name, var_lr);
    }
    if let Some(name) = &def.var_ud {
        set_evaluated_variable(variables, name, var_ud);
    }
}

/// EPPendControl two-segment pendulum per tick.
/// Algorithm confirmed from sub_10201AB0 decompilation (docs/emote.sqlite).
/// Each segment is a length-constrained pendulum with gravity and per-axis friction.
fn step_hair_physics(
    state: &mut HairPhysicsState,
    def: &PhysicsControlDefinition,
    delta_ticks: f32,
    variables: &mut BTreeMap<String, EmoteVariableState>,
) {
    if !def.enabled || delta_ticks <= 0.0 {
        return;
    }
    let dt = delta_ticks;
    let gravity = def.fields.get("gravity").and_then(|v| v.as_f32()).unwrap_or(0.0);
    let friction_x = def.fields.get("friction_x").and_then(|v| v.as_f32()).unwrap_or(0.0);
    let friction_y = def.fields.get("friction_y").and_then(|v| v.as_f32()).unwrap_or(0.0);
    let lengths = parse_f32_list_2(&def.fields, "length");
    let scale_x = parse_f32_list_2(&def.fields, "scale_x");
    let scale_y = parse_f32_list_2(&def.fields, "scale_y");
    let ud_eft = def.fields.get("ud_eft").and_then(|v| v.as_i64()).unwrap_or(0) as usize;

    // Hinge positions: seg0 hinge = anchor, seg1 hinge = seg0 bob
    let anchor = [0.0f32, 0.0, 0.0];

    for j in 0..2 {
        // Step 1: gravity (+Y = down in screen coords)
        state.vel[j][1] += gravity * dt;

        // Step 2: per-axis friction
        state.vel[j][0] -= state.vel[j][0] * friction_x * dt;
        state.vel[j][1] -= state.vel[j][1] * friction_y * dt;
        state.vel[j][2] -= state.vel[j][2] * friction_y * dt;

        // Step 3: integrate
        state.bob[j][0] += state.vel[j][0] * dt;
        state.bob[j][1] += state.vel[j][1] * dt;
        state.bob[j][2] += state.vel[j][2] * dt;

        // Step 4: inextensible length constraint — correct position then project out
        // the radial velocity component (velocity along hinge→bob direction).
        // Without velocity projection gravity accumulates unbounded outward speed.
        let hinge_now = if j == 0 { anchor } else { state.bob[0] };
        let diff = [
            state.bob[j][0] - hinge_now[0],
            state.bob[j][1] - hinge_now[1],
            state.bob[j][2] - hinge_now[2],
        ];
        let dist = (diff[0]*diff[0] + diff[1]*diff[1] + diff[2]*diff[2]).sqrt();
        let seg_len = lengths[j];
        if dist > seg_len && dist > f32::EPSILON {
            let scale = seg_len / dist;
            state.bob[j][0] = hinge_now[0] + diff[0] * scale;
            state.bob[j][1] = hinge_now[1] + diff[1] * scale;
            state.bob[j][2] = hinge_now[2] + diff[2] * scale;
            // Remove velocity component pointing outward (away from hinge)
            let dir = [diff[0]/dist, diff[1]/dist, diff[2]/dist];
            let radial_vel = state.vel[j][0]*dir[0] + state.vel[j][1]*dir[1] + state.vel[j][2]*dir[2];
            if radial_vel > 0.0 {
                state.vel[j][0] -= radial_vel * dir[0];
                state.vel[j][1] -= radial_vel * dir[1];
                state.vel[j][2] -= radial_vel * dir[2];
            }
        }

        // Output displacement (hinge - bob, as in EPBustControl convention)
        let hinge_final = if j == 0 { anchor } else { state.bob[0] };
        let disp_out = [
            hinge_final[0] - state.bob[j][0],
            hinge_final[1] - state.bob[j][1],
            hinge_final[2] - state.bob[j][2],
        ];

        // var_lr (j==0) and var_lrm (j==1): x-displacement * scale_x[j]
        let var_val = -disp_out[0] * scale_x[j];
        if j == 0 {
            if let Some(name) = &def.var_lr {
                set_evaluated_variable(variables, name, var_val);
            }
        } else if let Some(name) = &def.var_lrm {
            set_evaluated_variable(variables, name, var_val);
        }

        // var_ud: from the segment indicated by ud_eft
        if j == ud_eft {
            let var_ud = (state.ofs - disp_out[1]) * scale_y[j];
            if let Some(name) = &def.var_ud {
                set_evaluated_variable(variables, name, var_ud);
            }
        }
    }
}

fn parse_f32_list_2(fields: &BTreeMap<String, PsbValue>, key: &str) -> [f32; 2] {
    let Some(val) = fields.get(key) else { return [0.0; 2]; };
    match val {
        PsbValue::List(items) => {
            let a = items.first().and_then(|v| v.as_f32()).unwrap_or(0.0);
            let b = items.get(1).and_then(|v| v.as_f32()).unwrap_or(0.0);
            [a, b]
        }
        _ => {
            let v = val.as_f32().unwrap_or(0.0);
            [v, v]
        }
    }
}

fn set_evaluated_variable(variables: &mut BTreeMap<String, EmoteVariableState>, name: &str, value: f32) {
    if name.is_empty() || !value.is_finite() {
        return;
    }
    let state = variables.entry(name.to_owned()).or_insert_with(|| EmoteVariableState {
        info: EmoteVariableInfo {
            name: name.to_owned(),
            default_value: value,
            min_value: None,
            max_value: None,
        },
        value,
        target: None,
    });
    state.value = clamp_variable_value(&state.info, value);
    state.target = None;
}

pub fn collect_emote_timelines(psb: &PsbFile) -> Vec<EmoteTimeline> {
    let mut out = Vec::new();
    let aliases = collect_variable_frame_aliases(psb);
    if let Some(metadata) = psb.root.field("metadata") {
        if let Some(timeline_root) = metadata.field("timelineControl") {
            collect_timeline_nodes(timeline_root, "", &aliases, &mut out);
        }
        for key in CONTROL_METADATA_KEYS {
            if let Some(control_root) = metadata.field(key) {
                let path = format!("@control/{key}");
                collect_timeline_nodes(control_root, &path, &aliases, &mut out);
            }
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name.as_str() == b.name.as_str());
    out
}

fn collect_timeline_nodes(
    value: &PsbValue,
    path: &str,
    aliases: &BTreeMap<String, BTreeMap<String, f32>>,
    out: &mut Vec<EmoteTimeline>,
) {
    match value {
        PsbValue::List(items) => {
            for (index, item) in items.iter().enumerate() {
                let next_path = join_timeline_path(path, &index.to_string());
                collect_timeline_nodes(item, &next_path, aliases, out);
            }
        }
        PsbValue::Object(fields) => {
            if value.field_str("type") == Some("folder") {
                let label = value.field_str("label").unwrap_or("");
                let folder_path = join_timeline_path(path, label);
                if let Some(children) = value.field("children") {
                    collect_timeline_nodes(children, &folder_path, aliases, out);
                }
                return;
            }

            if let Some(variable_list) = value.field("variableList").and_then(PsbValue::as_list) {
                let label = value
                    .field_str("label")
                    .or_else(|| value.field_str("name"))
                    .or_else(|| value.field_str("id"))
                    .unwrap_or("timeline");
                let hinted_path = value
                    .field_str("path_hint")
                    .or_else(|| value.field_str("hint_path"))
                    .map(str::to_owned);
                let full_name = hinted_path.clone().unwrap_or_else(|| join_timeline_path(path, label));
                let name = if full_name.is_empty() { label.to_owned() } else { full_name };

                let mut variables = Vec::new();
                let mut duration = value
                    .field_f32("lastTime")
                    .or_else(|| value.field_f32("loopTime"))
                    .unwrap_or(0.0)
                    .max(0.0);

                for variable in variable_list {
                    let Some(var_name) = variable_object_name(variable) else {
                        continue;
                    };
                    let Some(frame_list) = variable.field("frameList").and_then(PsbValue::as_list) else {
                        continue;
                    };
                    let mut frames = Vec::new();
                    for frame in frame_list {
                        let time = frame.field_f32("time").unwrap_or(0.0).max(0.0);
                        let Some(content) = frame.field("content") else {
                            continue;
                        };
                        let value = timeline_content_value(&var_name, content, aliases).unwrap_or(0.0);
                        let easing = content.field_f32("easing").or_else(|| frame.field_f32("easing")).unwrap_or(0.0);
                        if value.is_finite() {
                            duration = duration.max(time);
                            frames.push(EmoteTimelineFrame {
                                time_ticks: time,
                                value,
                                easing,
                            });
                        }
                    }
                    frames.sort_by(|a, b| a.time_ticks.total_cmp(&b.time_ticks));
                    if !frames.is_empty() {
                        variables.push(EmoteTimelineVariable {
                            name: var_name,
                            frames,
                        });
                    }
                }

                if !variables.is_empty() {
                    out.push(EmoteTimeline {
                        name,
                        path: hinted_path,
                        duration_ticks: duration,
                        variables,
                    });
                }
            }

            for (key, child) in fields {
                if matches!(key.as_str(), "variableList" | "frameList" | "content" | "variable") {
                    continue;
                }
                let label = child
                    .field_str("label")
                    .or_else(|| child.field_str("name"))
                    .or_else(|| child.field_str("id"))
                    .unwrap_or(key);
                let next_path = join_timeline_path(path, label);
                collect_timeline_nodes(child, &next_path, aliases, out);
            }
        }
        _ => {}
    }
}

fn timeline_content_value(
    variable_name: &str,
    content: &PsbValue,
    aliases: &BTreeMap<String, BTreeMap<String, f32>>,
) -> Option<f32> {
    for key in ["value", "frame", "curValue", "targetValue", variable_name] {
        if let Some(value) = content.field(key) {
            if let Some(n) = value.as_f32() {
                return Some(n);
            }
            if let Some(label) = value.as_str() {
                if let Ok(n) = label.parse::<f32>() {
                    return Some(n);
                }
                if let Some(n) = aliases.get(variable_name).and_then(|map| map.get(label)).copied() {
                    return Some(n);
                }
            }
        }
    }
    if let Some(label) = content.field_str("label") {
        if let Some(n) = aliases.get(variable_name).and_then(|map| map.get(label)).copied() {
            return Some(n);
        }
    }
    None
}

fn collect_variable_frame_aliases(psb: &PsbFile) -> BTreeMap<String, BTreeMap<String, f32>> {
    let mut out = BTreeMap::<String, BTreeMap<String, f32>>::new();
    if let Some(metadata) = psb.root.field("metadata") {
        collect_variable_frame_alias_nodes(metadata.field("variableList"), &mut out);
        collect_variable_frame_alias_nodes(metadata.field("instantVariableList"), &mut out);
        collect_variable_frame_alias_nodes(metadata.field("physicsVariableList"), &mut out);
        if let Some(timeline_control) = metadata.field("timelineControl") {
            collect_variable_frame_alias_nodes(timeline_control.field("variableList"), &mut out);
        }
        for key in CONTROL_METADATA_KEYS {
            if let Some(control) = metadata.field(key) {
                collect_control_variable_aliases(control, &mut out);
            }
        }
    }
    out
}

fn collect_variable_frame_alias_nodes(
    value: Option<&PsbValue>,
    out: &mut BTreeMap<String, BTreeMap<String, f32>>,
) {
    let Some(value) = value else {
        return;
    };
    match value {
        PsbValue::List(items) => {
            for item in items {
                collect_one_variable_frame_alias(item, out);
            }
        }
        PsbValue::Object(fields) => {
            for (_key, child) in fields {
                collect_one_variable_frame_alias(child, out);
            }
        }
        _ => {}
    }
}

fn collect_control_variable_aliases(
    value: &PsbValue,
    out: &mut BTreeMap<String, BTreeMap<String, f32>>,
) {
    collect_one_variable_frame_alias(value, out);
    collect_variable_frame_alias_nodes(value.field("variableList"), out);
    collect_variable_frame_alias_nodes(value.field("variables"), out);
    if let Some(variable) = value.field("variable") {
        collect_one_variable_frame_alias(variable, out);
    }

    match value {
        PsbValue::List(items) => {
            for item in items {
                collect_control_variable_aliases(item, out);
            }
        }
        PsbValue::Object(fields) => {
            for (key, child) in fields {
                if matches!(key.as_str(), "variableList" | "variables" | "variable" | "frameList" | "content") {
                    continue;
                }
                collect_control_variable_aliases(child, out);
            }
        }
        _ => {}
    }
}

fn collect_one_variable_frame_alias(
    variable: &PsbValue,
    out: &mut BTreeMap<String, BTreeMap<String, f32>>,
) {
    let Some(name) = variable_object_name(variable) else {
        return;
    };
    let Some(frame_list) = variable.field("frameList").and_then(PsbValue::as_list) else {
        return;
    };
    let map = out.entry(name).or_default();
    for frame in frame_list {
        let label = frame
            .field_str("label")
            .or_else(|| frame.field("content").and_then(|content| content.field_str("label")));
        let value = frame
            .field_f32("frame")
            .or_else(|| frame.field("content").and_then(|content| content.field_f32("frame")))
            .or_else(|| frame.field_f32("value"))
            .or_else(|| frame.field("content").and_then(|content| content.field_f32("value")));
        if let (Some(label), Some(value)) = (label, value) {
            if !label.is_empty() && value.is_finite() {
                map.insert(label.to_owned(), value);
            }
        }
    }
}

fn join_timeline_path(path: &str, label: &str) -> String {
    match (path.is_empty(), label.is_empty()) {
        (true, true) => String::new(),
        (true, false) => label.to_owned(),
        (false, true) => path.to_owned(),
        (false, false) => format!("{path}/{label}"),
    }
}

pub fn collect_emote_variables(psb: &PsbFile) -> Vec<EmoteVariableInfo> {
    let mut out = BTreeMap::<String, EmoteVariableInfo>::new();

    if let Some(metadata) = psb.root.field("metadata") {
        collect_variable_list(metadata.field("variableList"), &mut out);
        collect_variable_list(metadata.field("instantVariableList"), &mut out);
        collect_variable_list(metadata.field("physicsVariableList"), &mut out);
        if let Some(timeline_control) = metadata.field("timelineControl") {
            collect_variable_list(timeline_control.field("variableList"), &mut out);
            collect_control_variables(timeline_control, &mut out);
        }
        for key in CONTROL_METADATA_KEYS {
            if let Some(control) = metadata.field(key) {
                collect_control_variables(control, &mut out);
            }
        }
    }

    collect_variable_list(psb.root.field("parameter"), &mut out);
    collect_variable_list(psb.root.field("parameters"), &mut out);
    collect_parameter_variables_recursive(&psb.root, &mut out);
    collect_mesh_combinator_variables(&psb.root, &mut out);

    out.into_values().collect()
}

fn collect_parameter_variables_recursive(value: &PsbValue, out: &mut BTreeMap<String, EmoteVariableInfo>) {
    if let Some(parameter_list) = value.field("parameter") {
        collect_variable_list(Some(parameter_list), out);
    }
    match value {
        PsbValue::List(values) => {
            for child in values {
                collect_parameter_variables_recursive(child, out);
            }
        }
        PsbValue::Object(fields) => {
            for (key, child) in fields {
                if key == "parameter" {
                    continue;
                }
                collect_parameter_variables_recursive(child, out);
            }
        }
        _ => {}
    }
}

fn collect_control_variables(value: &PsbValue, out: &mut BTreeMap<String, EmoteVariableInfo>) {
    collect_one_variable(value, out);
    collect_variable_list(value.field("variableList"), out);
    collect_variable_list(value.field("variables"), out);
    if let Some(variable) = value.field("variable") {
        collect_one_variable(variable, out);
    }

    match value {
        PsbValue::List(values) => {
            for child in values {
                collect_control_variables(child, out);
            }
        }
        PsbValue::Object(fields) => {
            for (key, child) in fields {
                if matches!(key.as_str(), "variableList" | "variables" | "variable" | "frameList" | "content") {
                    continue;
                }
                collect_control_variables(child, out);
            }
        }
        _ => {}
    }
}

fn collect_mesh_combinator_variables(value: &PsbValue, out: &mut BTreeMap<String, EmoteVariableInfo>) {
    if let Some(combinators) = value
        .field("meshCombinator")
        .and_then(|mesh_combinator| mesh_combinator.field("combinatorList"))
        .and_then(PsbValue::as_list)
    {
        for combinator in combinators {
            if let Some(variable) = combinator.field("variable") {
                collect_one_variable(variable, out);
            }
        }
    }

    match value {
        PsbValue::List(values) => {
            for child in values {
                collect_mesh_combinator_variables(child, out);
            }
        }
        PsbValue::Object(fields) => {
            for (_key, child) in fields {
                collect_mesh_combinator_variables(child, out);
            }
        }
        _ => {}
    }
}

fn collect_variable_list(value: Option<&PsbValue>, out: &mut BTreeMap<String, EmoteVariableInfo>) {
    let Some(value) = value else {
        return;
    };

    match value {
        PsbValue::List(values) => {
            for child in values {
                collect_one_variable(child, out);
            }
        }
        PsbValue::Object(fields) => {
            for (_key, child) in fields {
                collect_one_variable(child, out);
            }
        }
        _ => {}
    }
}

fn collect_one_variable(value: &PsbValue, out: &mut BTreeMap<String, EmoteVariableInfo>) {
    let Some(name) = variable_object_name(value) else {
        return;
    };
    out.entry(name.clone()).or_insert_with(|| EmoteVariableInfo {
        name,
        default_value: value
            .field_f32("default")
            .or_else(|| value.field_f32("defaultValue"))
            .or_else(|| value.field_f32("initial"))
            .or_else(|| value.field_f32("curValue"))
            .or_else(|| value.field_f32("value"))
            .unwrap_or(0.0),
        min_value: value
            .field_f32("min")
            .or_else(|| value.field_f32("minValue"))
            .or_else(|| value.field_f32("rangeBegin")),
        max_value: value
            .field_f32("max")
            .or_else(|| value.field_f32("maxValue"))
            .or_else(|| value.field_f32("rangeEnd")),
    });
}

fn variable_object_name(obj: &PsbValue) -> Option<String> {
    for key in ["id", "key", "name", "label"] {
        if let Some(value) = obj.field_str(key).filter(|s| !s.is_empty()) {
            return Some(value.to_owned());
        }
    }
    None
}
