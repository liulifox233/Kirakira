//! `krmovie.dll` placeholder.
//!
//! Real plugin: MoviePlayer/VideoOverlay movie plugin.
//! Upstream: https://github.com/krkrz/krkrz
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "MoviePlayer / VideoOverlay",
    notes: "Not implemented; movies already play through the engine's native VideoOverlay (krkr-video), but the plugin's TJS classes are not installed.",
    install: |engine| engine.register_plugin(KrmoviePlugin),
};

crate::placeholder::placeholder_plugin!(KrmoviePlugin, "krmovie.dll");
