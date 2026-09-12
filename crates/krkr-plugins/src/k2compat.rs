//! `k2compat.dll` placeholder.
//!
//! Real plugin: kirikiroid2 compatibility layer for Android-era scripts.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "kirikiroid2 compatibility layer",
    notes: "Not implemented; relevant because PARQUET also ships kirikiroid2.dll and may expect its compat globals.",
    install: |engine| engine.register_plugin(K2CompatPlugin),
};

crate::placeholder::placeholder_plugin!(K2CompatPlugin, "k2compat.dll");
