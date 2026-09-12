//! `fstat.dll` placeholder.
//!
//! Real plugin: File statistics for storages and placed files.
//! Upstream: https://github.com/wtnbgo/fstat
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "File statistics for storages and placed files",
    notes: "Not implemented; the engine's storage layer exposes existence and listing, not the plugin's stat members.",
    install: |engine| engine.register_plugin(FstatPlugin),
};

crate::placeholder::placeholder_plugin!(FstatPlugin, "fstat.dll");
