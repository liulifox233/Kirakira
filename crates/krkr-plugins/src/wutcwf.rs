//! `wutcwf.dll` placeholder.
//!
//! Real plugin: Sample plugin from the krkrz SamplePlugin tree (surface not identified yet).
//! Upstream: https://github.com/krkrz/SamplePlugin/tree/master/wutcwf
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "(surface still to be read from the plugin's source)",
    notes: "Not implemented; source exists in krkrz's SamplePlugin tree (plus a Kirikiroid2 port), so read it before implementing.",
    install: |engine| engine.register_plugin(WutcwfPlugin),
};

crate::placeholder::placeholder_plugin!(WutcwfPlugin, "wutcwf.dll");
