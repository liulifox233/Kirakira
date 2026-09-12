//! `gfxEffect.dll` placeholder.
//!
//! Real plugin: Kaede's layer effects (blur/glow family) beyond the stock Layer API.
//! Upstream: http://kaede-software.com/krlm/plugin/gfx_effect.zip
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Kaede layer effects (blur/glow family)",
    notes: "Not implemented; the engine's Layer has no effect pipeline to hang these on yet.",
    install: |engine| engine.register_plugin(GfxEffectPlugin),
};

crate::placeholder::placeholder_plugin!(GfxEffectPlugin, "gfxEffect.dll");
