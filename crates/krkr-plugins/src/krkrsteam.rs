//! `krkrsteam.dll` placeholder.
//!
//! Real plugin: Steamworks integration (achievements, Steam API calls).
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Steamworks integration",
    notes: "Not implemented; achievements/API calls need a Steamworks binding.",
    install: |engine| engine.register_plugin(KrkrSteamPlugin),
};

crate::placeholder::placeholder_plugin!(KrkrSteamPlugin, "krkrsteam.dll");
