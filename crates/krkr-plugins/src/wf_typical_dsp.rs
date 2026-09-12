//! `wfTypicalDSP.dll` placeholder.
//!
//! Real plugin: PARQUET-shipped DSP plugin (surface not identified yet).
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "WaveDSPFilter (tTJSNC_WaveDSPFilter / tTJSNI_WaveDSPFilter) on WaveSoundBuffer",
    notes: "Not implemented; PARQUET ships it, so check whether the game uses it before the DSP chain is built.",
    install: |engine| engine.register_plugin(WfTypicalDspPlugin),
};

crate::placeholder::placeholder_plugin!(WfTypicalDspPlugin, "wfTypicalDSP.dll");
