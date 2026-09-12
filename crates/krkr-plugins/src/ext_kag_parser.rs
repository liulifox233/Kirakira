//! `ExtKAGParser.dll` placeholder.
//!
//! Real plugin: Extra KAGParser helpers (tag dictionary extensions).
//! Upstream: http://keepcreating.g2.xrea.com/krkrplugins/ExtKAGParser/ExtKAGParser-0143.zip
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Extra KAGParser helpers",
    notes: "Not implemented; check whether PARQUET's KAG scripts need its tag dictionary extensions.",
    install: |engine| engine.register_plugin(ExtKagParserPlugin),
};

crate::placeholder::placeholder_plugin!(ExtKagParserPlugin, "ExtKAGParser.dll");
