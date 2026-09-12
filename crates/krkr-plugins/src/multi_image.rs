//! `multiimage.dll` placeholder.
//!
//! Real plugin: Loads an image sheet into several layer images at once.
//! Upstream: (no public source; in the kirikiroid2 plugin list)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Image sheet split into several layer images",
    notes: "Not implemented; sheet splitting is expressible with the engine's existing blits.",
    install: |engine| engine.register_plugin(MultiImagePlugin),
};

crate::placeholder::placeholder_plugin!(MultiImagePlugin, "multiimage.dll");
