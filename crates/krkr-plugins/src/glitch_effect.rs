//! `GlitchEffect.dll` placeholder.
//!
//! Real plugin: Glitch/CRT style layer effect.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Glitch/CRT layer effect",
    notes: "Not implemented; PARQUET ships it, so its usage should be checked before the effect is built.",
    install: |engine| engine.register_plugin(GlitchEffectPlugin),
};

crate::placeholder::placeholder_plugin!(GlitchEffectPlugin, "GlitchEffect.dll");
