//! `layerExAreaAverage.dll` placeholder.
//!
//! Real plugin: Average colour of a layer image area.
//! Upstream: https://github.com/wtnbgo/layerExAreaAverage
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Average colour of a layer image area",
    notes: "Not implemented; needs layer pixel access, which the engine already has internally.",
    install: |engine| engine.register_plugin(LayerExAreaAveragePlugin),
};

crate::placeholder::placeholder_plugin!(LayerExAreaAveragePlugin, "layerExAreaAverage.dll");
