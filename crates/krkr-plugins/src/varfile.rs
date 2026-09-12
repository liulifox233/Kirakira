//! `varfile.dll` placeholder.
//!
//! Real plugin: Virtual/bundled data files mounted as storages.
//! Upstream: https://github.com/wtnbgo/varfile
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Virtual/bundled data files mounted as storages",
    notes: "Not implemented; overlaps the engine's external-resource provision path.",
    install: |engine| engine.register_plugin(VarfilePlugin),
};

crate::placeholder::placeholder_plugin!(VarfilePlugin, "varfile.dll");
