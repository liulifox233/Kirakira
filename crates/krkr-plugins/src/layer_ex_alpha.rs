//! `layerExAlpha.dll` placeholder.
//!
//! Real plugin: Layer alpha channel operations.
//! Upstream: (no public source; in the kirikiroid2 plugin list)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Layer alpha channel operations",
    notes: "Not implemented; alpha extract/replace has no engine-side entry point.",
    install: |engine| engine.register_plugin(LayerExAlphaPlugin),
};

crate::placeholder::placeholder_plugin!(LayerExAlphaPlugin, "layerExAlpha.dll");
