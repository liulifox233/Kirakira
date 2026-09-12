//! `httprequest.dll` placeholder.
//!
//! Real plugin: HTTP(S) requests from scripts.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "HTTP(S) requests from scripts",
    notes: "Not implemented; needs a network stack and a decision about what a game's requests should do offline.",
    install: |engine| engine.register_plugin(HttpRequestPlugin),
};

crate::placeholder::placeholder_plugin!(HttpRequestPlugin, "httprequest.dll");
