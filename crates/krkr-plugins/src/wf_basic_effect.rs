//! `wfBasicEffect.dll` placeholder.
//!
//! Real plugin: PARQUET-shipped effect plugin (surface not identified yet).
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "GraphicEqualizer / StkFreeVerb / WaveDelay filters on WaveSoundBuffer",
    notes: "Not implemented; needs DSP hooks in krkr-audio that WaveSoundBuffer can drive.",
    install: |engine| engine.register_plugin(WfBasicEffectPlugin),
};

crate::placeholder::placeholder_plugin!(WfBasicEffectPlugin, "wfBasicEffect.dll");
