//! `layerExSave.dll` placeholder.
//!
//! Real plugin: Saves layer images and reports the saveable formats.
//! Upstream: https://github.com/wtnbgo/layerExSave
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Save layer images to image files",
    notes: "Not implemented; image encoding has no path from layer pixels yet.",
    install: |engine| engine.register_plugin(LayerExSavePlugin),
};

crate::placeholder::placeholder_plugin!(LayerExSavePlugin, "layerExSave.dll");
