//! `minizip.dll` placeholder.
//!
//! Real plugin: ZIP archive reading and writing exposed to scripts.
//! Upstream: https://github.com/wtnbgo/minizip
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "ZIP archive reading and writing",
    notes: "Not implemented; ZIP support would have to reach the storage layer, not just the script surface.",
    install: |engine| engine.register_plugin(MinizipPlugin),
};

crate::placeholder::placeholder_plugin!(MinizipPlugin, "minizip.dll");
