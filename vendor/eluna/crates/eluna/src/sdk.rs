//! Stable third-party facade for loading and driving Emote models.
//!
//! This module keeps callers away from PSB traversal details. It intentionally
//! exposes the runtime result as drawFrameInfo plus texture resources, because
//! the original renderer contract is draw-list oriented.

use crate::api::{EmotePlayerControl, TimelinePlayMode};
use crate::{
    collect_emote_runtime_pipeline, collect_emote_timelines, collect_emote_variables,
    EmoteDrawFrameInfo, EmoteModelSchema, EmoteMotionInfo, EmoteRuntimePipeline,
    EmoteSceneBounds, EmoteSchemaError, EmoteStaticScene, EmoteStaticSprite, EmoteTextureSource,
    ElunaPlayer, PsbDecryptionKey, PsbError, PsbFile, PsbNormalizeOptions,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct EmoteLoadOptions {
    pub normalize: PsbNormalizeOptions,
    pub motion: Option<String>,
    pub timeline: Option<String>,
    pub autoplay_timeline: bool,
}

impl Default for EmoteLoadOptions {
    fn default() -> Self {
        Self {
            normalize: PsbNormalizeOptions::default(),
            motion: None,
            timeline: None,
            autoplay_timeline: true,
        }
    }
}

impl EmoteLoadOptions {
    pub fn with_emote_key(mut self, key: u32) -> Self {
        self.normalize.decrypt_key = Some(PsbDecryptionKey::emote_key(key));
        self
    }

    pub fn with_motion(mut self, motion: impl Into<String>) -> Self {
        self.motion = Some(motion.into());
        self
    }

    pub fn with_timeline(mut self, timeline: impl Into<String>) -> Self {
        self.timeline = Some(timeline.into());
        self
    }
}

#[derive(Debug)]
pub enum EmoteRuntimeError {
    Io(io::Error),
    Psb(PsbError),
    Schema(EmoteSchemaError),
    MissingActiveMotion,
    MissingTimeline(String),
    RuntimeControl(String),
}

impl fmt::Display for EmoteRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmoteRuntimeError::Io(err) => write!(f, "{err}"),
            EmoteRuntimeError::Psb(err) => write!(f, "{err}"),
            EmoteRuntimeError::Schema(err) => write!(f, "{err}"),
            EmoteRuntimeError::MissingActiveMotion => write!(f, "Emote model has no active motion"),
            EmoteRuntimeError::MissingTimeline(name) => write!(f, "timeline '{name}' does not exist"),
            EmoteRuntimeError::RuntimeControl(message) => write!(f, "{message}"),
        }
    }
}

impl Error for EmoteRuntimeError {}

impl From<io::Error> for EmoteRuntimeError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<PsbError> for EmoteRuntimeError {
    fn from(value: PsbError) -> Self {
        Self::Psb(value)
    }
}

impl From<EmoteSchemaError> for EmoteRuntimeError {
    fn from(value: EmoteSchemaError) -> Self {
        Self::Schema(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmoteRuntimeParityReport {
    pub confirmed: Vec<&'static str>,
    pub partial: Vec<&'static str>,
    pub missing: Vec<&'static str>,
}

pub fn emote_runtime_parity_report() -> EmoteRuntimeParityReport {
    EmoteRuntimeParityReport {
        confirmed: vec![
            "PSB/MDF/LZ4/key normalization",
            "metadata control loading order from MEmotePlayer::Init",
            "texture resource and atlas/icon extraction",
            "timelineControl variable track interpolation",
            "drawFrameInfo draw-list carrier",
            "coordinate.z based draw ordering",
            "dynamic per-frame scene/drawFrameInfo rebuild",
        ],
        partial: vec![
            "selectorControl optionList/offValue/onValue variable output",
            "clampControl var_lr/var_ud clamping",
            "partsControl/eyeControl/eyebrowControl/mouthControl parsing and tick visibility diagnostics",
            "meshCombinator/rawMeshList frame mesh patch application",
            "stencilCompositeMaskLayerList and stencilType propagation into drawFrameInfo",
            "renderer pass separation for mask-generation layers",
        ],
        missing: vec![
            "original transitionControl fade/diff state machine",
            "original loopControl transition scheduler",
            "original mirrorControl variable/mesh mirror application",
            "original bustControl/hairControl physics integration",
            "full MMotionPlayer::StepFrameMeshChain parent/child mesh chain",
            "confirmed meshSyncChildMask bit semantics",
            "confirmed joinTarget binding semantics",
            "true alpha-mask/stencil/composite/filter wgpu passes",
            "binary-compatible IEmotePlayer/PEmotePlayer ABI",
        ],
    }
}

#[derive(Debug, Clone)]
pub struct EmoteRuntime {
    normalized_data: Vec<u8>,
    psb: PsbFile,
    schema: EmoteModelSchema,
    active_motion: Option<String>,
    player: ElunaPlayer,
}

impl EmoteRuntime {
    pub fn from_path(path: impl AsRef<Path>, options: EmoteLoadOptions) -> Result<Self, EmoteRuntimeError> {
        let data = fs::read(path)?;
        Self::from_bytes(&data, options)
    }

    pub fn from_bytes(data: &[u8], options: EmoteLoadOptions) -> Result<Self, EmoteRuntimeError> {
        let (normalized_data, psb) = PsbFile::parse_normalized(data, &options.normalize)?;
        let schema = EmoteModelSchema::from_psb(&psb)?;
        let active_motion = options
            .motion
            .clone()
            .or_else(|| schema.default_motion_name(&psb).ok().flatten());
        let variables = collect_emote_variables(&psb);
        let timelines = collect_emote_timelines(&psb);
        let initial_values = initial_variable_values(&variables, &timelines);
        let runtime_pipeline = collect_emote_runtime_pipeline(&psb);
        let scene = match active_motion.as_deref() {
            Some(motion) => schema.build_motion_scene_at_with_resources_and_variables(
                &psb,
                &normalized_data,
                motion,
                0.0,
                &initial_values,
            )?,
            None => schema.build_static_scene(&psb)?,
        };
        let mut player = ElunaPlayer::from_scene_variables_timelines_runtime(scene, variables, timelines, runtime_pipeline);

        if options.autoplay_timeline {
            if let Some(name) = options.timeline.clone().or_else(|| {
                active_motion
                    .as_deref()
                    .and_then(|motion| find_timeline_for_motion(&player, motion))
                    .or_else(|| player.timelines().keys().find(|name| !name.starts_with("@control/")).cloned())
            }) {
                if !player.timelines().contains_key(&name) {
                    return Err(EmoteRuntimeError::MissingTimeline(name));
                }
                player.play_timeline(&name, TimelinePlayMode::Loop);
            }
        }

        let mut runtime = Self {
            normalized_data,
            psb,
            schema,
            active_motion,
            player,
        };
        runtime.rebuild_scene()?;
        Ok(runtime)
    }

    pub fn progress_ticks(&mut self, delta_ticks: f32) -> Result<(), EmoteRuntimeError> {
        self.player.progress_ticks(delta_ticks);
        self.rebuild_scene()
    }

    pub fn progress_seconds(&mut self, delta_seconds: f32) -> Result<(), EmoteRuntimeError> {
        self.progress_ticks(delta_seconds * crate::EMOTE_TICKS_PER_SECOND)
    }

    pub fn rebuild_scene(&mut self) -> Result<(), EmoteRuntimeError> {
        let Some(motion) = self.active_motion.as_deref() else {
            return Ok(());
        };
        let variables = self.variable_values();
        let scene = self.schema.build_motion_scene_at_with_resources_and_variables(
            &self.psb,
            &self.normalized_data,
            motion,
            self.player.elapsed_ticks(),
            &variables,
        )?;
        self.player.replace_scene(scene);
        Ok(())
    }

    pub fn show(&mut self) {
        self.player.show();
    }

    pub fn hide(&mut self) {
        self.player.hide();
    }

    pub fn set_variable(&mut self, name: &str, value: f32) {
        self.player.set_variable(name, value);
    }

    pub fn set_variable_immediate(&mut self, name: &str, value: f32) -> Result<(), EmoteRuntimeError> {
        self.player.set_variable_immediate(name, value);
        self.rebuild_scene()
    }

    pub fn set_variable_timed(&mut self, name: &str, value: f32, time_ticks: f32, easing: f32) {
        self.player.set_variable_timed(name, value, time_ticks, easing);
    }

    pub fn play_timeline(&mut self, name: &str, mode: TimelinePlayMode) -> Result<(), EmoteRuntimeError> {
        if !self.player.timelines().contains_key(name) {
            return Err(EmoteRuntimeError::MissingTimeline(name.to_owned()));
        }
        self.player.play_timeline(name, mode);
        self.rebuild_scene()
    }

    pub fn set_timeline_time(&mut self, name: &str, ticks: f32) -> Result<(), EmoteRuntimeError> {
        self.player
            .set_timeline_time(name, ticks)
            .map_err(EmoteRuntimeError::RuntimeControl)?;
        self.rebuild_scene()
    }

    pub fn stop_timeline(&mut self, name: &str) -> Result<(), EmoteRuntimeError> {
        self.player.stop_timeline(name);
        self.rebuild_scene()
    }

    pub fn pause(&mut self) {
        self.player.set_paused(true);
    }

    pub fn play(&mut self) {
        self.player.set_paused(false);
    }

    pub fn set_selector_option(&mut self, label: &str, option_index: usize) -> Result<(), EmoteRuntimeError> {
        self.player
            .set_selector_option(label, option_index)
            .map_err(EmoteRuntimeError::RuntimeControl)?;
        self.rebuild_scene()
    }

    pub fn reset_all_variables(&mut self) -> Result<(), EmoteRuntimeError> {
        self.player.reset_all_variables_to_default();
        self.rebuild_scene()
    }

    pub fn set_physics_enabled(&mut self, enabled: bool) {
        self.player.set_physics_enabled(enabled);
    }

    pub fn reset_physics(&mut self) -> Result<(), EmoteRuntimeError> {
        self.player.reset_physics();
        self.rebuild_scene()
    }

    pub fn set_coord(&mut self, x: f32, y: f32) {
        self.player.set_coord(x, y);
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.player.set_scale(scale);
    }

    pub fn set_rot(&mut self, rot: f32) {
        self.player.set_rot(rot);
    }

    pub fn motion_names(&self) -> Result<Vec<String>, EmoteRuntimeError> {
        Ok(self.schema.motion_infos(&self.psb)?.into_iter().map(|motion| motion.name).collect())
    }

    pub fn motion_infos(&self) -> Result<Vec<EmoteMotionInfo>, EmoteRuntimeError> {
        Ok(self.schema.motion_infos(&self.psb)?)
    }

    pub fn active_motion(&self) -> Option<&str> {
        self.active_motion.as_deref()
    }

    pub fn set_motion(&mut self, motion: impl Into<String>) -> Result<(), EmoteRuntimeError> {
        self.active_motion = Some(motion.into());
        self.player.skip();
        self.rebuild_scene()
    }

    pub fn timeline_names(&self) -> Vec<&str> {
        self.player.timelines().keys().map(String::as_str).collect()
    }

    pub fn variable_names(&self) -> Vec<&str> {
        self.player.variables().keys().map(String::as_str).collect()
    }

    pub fn variable_value(&self, name: &str) -> Option<f32> {
        self.player.variable_value(name)
    }

    pub fn variable_values(&self) -> BTreeMap<String, f32> {
        self.player
            .variables()
            .iter()
            .map(|(name, state)| (name.clone(), state.value))
            .collect()
    }

    pub fn scene(&self) -> &EmoteStaticScene {
        self.player.scene()
    }

    pub fn sprites(&self) -> &[EmoteStaticSprite] {
        &self.player.scene().sprites
    }

    pub fn draw_frame_info(&self) -> &[EmoteDrawFrameInfo] {
        &self.player.scene().draw_frame_info
    }

    pub fn bounds(&self) -> Option<EmoteSceneBounds> {
        self.player.bounds()
    }

    pub fn texture_sources(&self) -> &BTreeMap<String, EmoteTextureSource> {
        &self.schema.textures
    }

    pub fn texture_bytes(&self, resource_index: u32) -> Option<&[u8]> {
        self.psb.resource_bytes(&self.normalized_data, resource_index as usize)
    }

    pub fn runtime_pipeline(&self) -> &EmoteRuntimePipeline {
        self.player.runtime_pipeline()
    }

    pub fn inner_player(&self) -> &ElunaPlayer {
        &self.player
    }

    pub fn inner_player_mut(&mut self) -> &mut ElunaPlayer {
        &mut self.player
    }
}

fn find_timeline_for_motion(player: &ElunaPlayer, motion: &str) -> Option<String> {
    if player.timelines().contains_key(motion) {
        return Some(motion.to_owned());
    }
    let suffix = format!("/{motion}");
    player
        .timelines()
        .keys()
        .filter(|name| !name.starts_with("@control/"))
        .find(|name| name.ends_with(&suffix))
        .cloned()
}

fn initial_variable_values(
    infos: &[crate::EmoteVariableInfo],
    timelines: &[crate::EmoteTimeline],
) -> BTreeMap<String, f32> {
    let mut values: BTreeMap<String, f32> = infos
        .iter()
        .map(|info| (info.name.clone(), info.default_value))
        .collect();
    for timeline in timelines {
        for variable in &timeline.variables {
            if let Some(first) = variable.frames.first() {
                values.insert(variable.name.clone(), first.value);
            }
        }
    }
    values
}
