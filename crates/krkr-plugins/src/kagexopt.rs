//! `kagexopt.dll` placeholder.
//!
//! Real plugin: KAG extension used by the game's KAG-based UI.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "KAG option description resource (GetOptionDesc export; vomstyle/overlay/mixer/layer/… option names)",
    notes: "Not implemented; it is a resource plugin rather than a TJS surface, so the option descriptions have to reach KAG's config handling.",
    install: |engine| engine.register_plugin(KagexOptPlugin),
};

crate::placeholder::placeholder_plugin!(KagexOptPlugin, "kagexopt.dll");
