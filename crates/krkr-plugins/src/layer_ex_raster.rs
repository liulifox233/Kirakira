//! `layerExRaster.dll` placeholder.
//!
//! Real plugin: Rasterisation (polygon/line) onto a layer image.
//! Upstream: https://github.com/wtnbgo/layerExRaster
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Rasterisation (polygon/line) onto a layer image",
    notes: "Not implemented; overlaps the drawing surface layerExDraw still lacks.",
    install: |engine| engine.register_plugin(LayerExRasterPlugin),
};

crate::placeholder::placeholder_plugin!(LayerExRasterPlugin, "layerExRaster.dll");
