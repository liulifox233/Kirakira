//! `layerExImage.dll` placeholder.
//!
//! Real plugin: Layer image load/blit helpers from wtnbgo's layerEx family.
//! Upstream: https://github.com/wtnbgo/layerExImage
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Layer image load/blit helpers",
    notes: "Not implemented; the two spellings in the plugin list (`LayerExImage.dll`, `layerExImage.dll`) are one plugin.",
    install: |engine| engine.register_plugin(LayerExImagePlugin),
};

crate::placeholder::placeholder_plugin!(LayerExImagePlugin, "layerExImage.dll");
