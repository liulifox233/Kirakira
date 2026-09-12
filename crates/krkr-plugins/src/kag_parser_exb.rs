//! `KAGParserExb.dll` placeholder.
//!
//! Real plugin: KAGParserExb variant of the KAG parser extension.
//! Upstream: https://github.com/sakano/krkr_archives/tree/master/kagex_plugin/KAGParserExb
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(KagParserExbPlugin, "KAGParserExb.dll");
