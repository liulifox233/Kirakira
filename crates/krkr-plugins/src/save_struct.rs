//! `savestruct.dll` placeholder.
//!
//! Real plugin: TJS object graph (de)serialization for save data.
//! Upstream: https://github.com/wtnbgo/saveStruct
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "TJS object graph (de)serialization",
    notes: "Not implemented; games that use it cannot save nested structures until it lands.",
    install: |engine| engine.register_plugin(SaveStructPlugin),
};

crate::placeholder::placeholder_plugin!(SaveStructPlugin, "savestruct.dll");
