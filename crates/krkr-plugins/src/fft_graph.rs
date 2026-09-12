//! `fftgraph.dll` placeholder.
//!
//! Real plugin: FFT spectrum analysis of playing audio for visualisers.
//! Upstream: https://github.com/krkrz/fftgraph
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "FFT spectrum graph of playing audio",
    notes: "Not implemented; the engine already exposes sample data through getSample's stub, not through FFT.",
    install: |engine| engine.register_plugin(FftGraphPlugin),
};

crate::placeholder::placeholder_plugin!(FftGraphPlugin, "fftgraph.dll");
