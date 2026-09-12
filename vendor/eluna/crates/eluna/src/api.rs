//! Public Emote-facing control surface that must stay stable while the
//! renderer/parser internals are replaced.
//!
//! This module intentionally mirrors the observed SDK usage shape instead of
//! exposing PSB internals. The implementation layer can later bind these calls
//! to parsed PSB data, motion timelines, and the WebGPU renderer.

/// One original E-mote time unit is one 1/60-second tick.
pub const EMOTE_TICKS_PER_SECOND: f32 = 60.0;

/// Converts milliseconds to the original driver time unit.
pub fn milliseconds_to_emote_ticks(ms: f32) -> f32 {
    ms * EMOTE_TICKS_PER_SECOND / 1000.0
}

/// Converts the original driver time unit to milliseconds.
pub fn emote_ticks_to_milliseconds(ticks: f32) -> f32 {
    ticks * 1000.0 / EMOTE_TICKS_PER_SECOND
}

/// Timeline play mode value as passed by the public API.
///
/// Keep this as a raw value until the exact enum constants are recovered from
/// the original header or verified vtable call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelinePlayMode(pub i32);

impl TimelinePlayMode {
    pub const Once: Self = Self(0);
    pub const Loop: Self = Self(1);
}

/// A single variable/parameter write request.
///
/// Public samples call this through `SetVariable(name, value)` and
/// `SetVariable(name, value, time, easing)`. The `time_ticks` field uses the
/// original 1/60-second unit, not milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct VariableWrite {
    pub name: String,
    pub value: f32,
    pub time_ticks: f32,
    pub easing: f32,
}

impl VariableWrite {
    pub fn immediate(name: impl Into<String>, value: f32) -> Self {
        Self {
            name: name.into(),
            value,
            time_ticks: 0.0,
            easing: 0.0,
        }
    }

    pub fn timed(name: impl Into<String>, value: f32, time_ticks: f32, easing: f32) -> Self {
        Self {
            name: name.into(),
            value,
            time_ticks,
            easing,
        }
    }
}

/// Stable high-level player API to preserve while replacing the original DLL.
///
/// This deliberately uses the original public method names as semantic anchors:
/// `Show`, `Progress`, `Render`, `SetVariable`, `PlayTimeline`, and related
/// transform accessors.
pub trait EmotePlayerControl {
    fn show(&mut self);
    fn hide(&mut self);
    fn progress_ticks(&mut self, delta_ticks: f32);
    fn render(&mut self);

    fn coord(&self) -> [f32; 2];
    fn set_coord(&mut self, x: f32, y: f32);

    fn scale(&self) -> f32;
    fn set_scale(&mut self, scale: f32);

    fn rot(&self) -> f32;
    fn set_rot(&mut self, rot: f32);

    fn set_variable(&mut self, name: &str, value: f32) {
        self.set_variable_timed(name, value, 0.0, 0.0);
    }

    fn set_variable_timed(&mut self, name: &str, value: f32, time_ticks: f32, easing: f32);

    fn play_timeline(&mut self, name: &str, mode: TimelinePlayMode);
    fn stop_timeline(&mut self, name: &str);
    fn skip(&mut self);
}
