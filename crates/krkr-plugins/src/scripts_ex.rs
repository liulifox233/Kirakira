//! `scriptsEx.dll` placeholder.
//!
//! Real plugin: Extra Scripts/System helper methods.
//! Upstream: https://github.com/wtnbgo/scriptsEx
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Extra Scripts/System helper methods",
    notes: "Not implemented; PackinOne bundles part of this surface already.",
    install: |engine| engine.register_plugin(ScriptsExPlugin),
};

crate::placeholder::placeholder_plugin!(ScriptsExPlugin, "scriptsEx.dll");
