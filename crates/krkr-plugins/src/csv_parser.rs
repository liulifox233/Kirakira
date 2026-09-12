//! `csvParser.dll` placeholder.
//!
//! Real plugin: CSVParser TJS class (the standalone plugin; PackinOne bundles the same class).
//! Upstream: https://github.com/wtnbgo/csvParser
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "CSVParser class",
    notes: "Not implemented as a plugin: PackinOne already bundles a working CSVParser, so linking this file would add nothing new.",
    install: |engine| engine.register_plugin(CsvParserPlugin),
};

crate::placeholder::placeholder_plugin!(CsvParserPlugin, "csvParser.dll");
