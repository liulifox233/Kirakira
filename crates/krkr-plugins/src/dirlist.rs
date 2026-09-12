//! `dirlist.dll` placeholder.
//!
//! Real plugin: Storage directory listing helpers.
//! Upstream: https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/dirlist
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Storage directory listing",
    notes: "Not implemented; the engine can already list storages, so this is a matter of the plugin's own member names.",
    install: |engine| engine.register_plugin(DirlistPlugin),
};

crate::placeholder::placeholder_plugin!(DirlistPlugin, "dirlist.dll");
