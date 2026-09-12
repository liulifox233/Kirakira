//! `psd.dll` placeholder.
//!
//! Real plugin: Photoshop PSD loading (layers with names and opacity).
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Photoshop PSD loading",
    notes: "Not implemented; PSD parsing plus layer reconstruction is needed before layer names/opacity can be honoured.",
    install: |engine| engine.register_plugin(PsdPlugin),
};

crate::placeholder::placeholder_plugin!(PsdPlugin, "psd.dll");
