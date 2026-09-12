//! `layerExBTOA.dll` placeholder.
//!
//! Real plugin: Layer image operator/alpha blit from wtnbgo's layerEx family.
//! Upstream: https://github.com/wtnbgo/layerExBTOA
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Layer image operator/alpha blit",
    notes: "Not implemented; exact member list still to be confirmed from the census.",
    install: |engine| engine.register_plugin(LayerExBtoaPlugin),
};

crate::placeholder::placeholder_plugin!(LayerExBtoaPlugin, "layerExBTOA.dll");
