//! `getabout.dll` placeholder.
//!
//! Real plugin: About/build information surface shown by launcher scripts.
//! Upstream: (no public source; in the kirikiroid2 plugin list)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "About/build information surface",
    notes: "Not implemented; launcher scripts that print build info will not find its members.",
    install: |engine| engine.register_plugin(GetAboutPlugin),
};

crate::placeholder::placeholder_plugin!(GetAboutPlugin, "getabout.dll");
