//! `win32ole.dll` placeholder.
//!
//! Real plugin: Win32 OLE automation access from scripts.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Win32 OLE automation",
    notes: "Not implemented; Windows-only by nature, so other platforms can only degrade.",
    install: |engine| engine.register_plugin(Win32OlePlugin),
};

crate::placeholder::placeholder_plugin!(Win32OlePlugin, "win32ole.dll");
