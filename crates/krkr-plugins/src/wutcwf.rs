//! `wutcwf.dll` placeholder.
//!
//! Real plugin: Sample plugin from the krkrz SamplePlugin tree (surface not identified yet).
//! Upstream: https://github.com/krkrz/SamplePlugin/tree/master/wutcwf
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(WutcwfPlugin, "wutcwf.dll");
