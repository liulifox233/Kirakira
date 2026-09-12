//! `yuzuex.dll` placeholder.
//!
//! Real plugin: PARQUET-shipped KAG/game-side extension bundle (surface not identified yet).
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "ProxyStorageMap / proxyfs storage remapping",
    notes: "Not implemented; it is a storage-media plugin (no TJS surface), so the remap has to happen where archives are resolved.",
    install: |engine| engine.register_plugin(YuzuExPlugin),
};

crate::placeholder::placeholder_plugin!(YuzuExPlugin, "yuzuex.dll");
