//! `SteamDrawDevice.dll` placeholder.
//!
//! Real plugin: Steam-overlay compatible draw device.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Draw device (DrawDeviceClass<tTVPBasicDrawDevice<K2/KZInterfaceTypes>>)",
    notes: "Not implemented; only matters if PARQUET's Steam build requires it for overlay rendering.",
    install: |engine| engine.register_plugin(SteamDrawDevicePlugin),
};

crate::placeholder::placeholder_plugin!(SteamDrawDevicePlugin, "SteamDrawDevice.dll");
