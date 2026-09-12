//! `kirikiroid2.dll` placeholder.
//!
//! Real plugin: kirikiroid2 (Android port) runtime module.
//! Upstream: (no public source; in the kirikiroid2 plugin list)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "kirikiroid2 runtime module",
    notes: "Not implemented; Kirakira is a desktop engine, so the port-specific globals it adds are only needed if a game probes for them.",
    install: |engine| engine.register_plugin(Kirikiroid2Plugin),
};

crate::placeholder::placeholder_plugin!(Kirikiroid2Plugin, "kirikiroid2.dll");
