//! `shrinkCopy.dll` placeholder.
//!
//! Real plugin: Downscaled blit onto layers.
//! Upstream: https://github.com/wtnbgo/shrinkCopy
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Downscaled blit onto layers",
    notes: "Not implemented; the engine's stretch blits cover the pixels, the plugin's own members are absent.",
    install: |engine| engine.register_plugin(ShrinkCopyPlugin),
};

crate::placeholder::placeholder_plugin!(ShrinkCopyPlugin, "shrinkCopy.dll");
