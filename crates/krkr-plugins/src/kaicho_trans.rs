//! `KaichoTrans.dll` placeholder.
//!
//! Real plugin: Extra Layer.beginTransition methods (kaicho family).
//! Upstream: http://keepcreating.g2.xrea.com/krkrplugins/KaichoTrans/KaichoTrans.zip
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Extra Layer.beginTransition methods (kaicho family)",
    notes: "Not implemented; transition names currently degrade to crossfade in krkr-core, so scripts keep running with the wrong effect.",
    install: |engine| engine.register_plugin(KaichoTransPlugin),
};

crate::placeholder::placeholder_plugin!(KaichoTransPlugin, "KaichoTrans.dll");
