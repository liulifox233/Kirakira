//! `csvParser.dll` placeholder.
//!
//! Real plugin: CSVParser TJS class (the standalone plugin; PackinOne bundles the same class).
//! Upstream: https://github.com/wtnbgo/csvParser
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(CsvParserPlugin, "csvParser.dll");
