//! `expat.dll` placeholder.
//!
//! Real plugin: XML parsing (expat) exposed to scripts.
//! Upstream: https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/expat
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "XML (expat) parsing",
    notes: "Not implemented; needs an XML parser behind the plugin's class surface.",
    install: |engine| engine.register_plugin(ExpatPlugin),
};

crate::placeholder::placeholder_plugin!(ExpatPlugin, "expat.dll");
