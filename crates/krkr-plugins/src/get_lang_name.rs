//! `getLangName.dll` placeholder.
//!
//! Real plugin: Reports the OS/user language name.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "OS/user language name",
    notes: "Not implemented; PARQUET ships it, so a language-selection path may depend on it.",
    install: |engine| engine.register_plugin(GetLangNamePlugin),
};

crate::placeholder::placeholder_plugin!(GetLangNamePlugin, "getLangName.dll");
