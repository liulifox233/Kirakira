//! `krmovie.dll` placeholder.
//!
//! Real plugin: MoviePlayer/VideoOverlay movie plugin.
//! Upstream: https://github.com/krkrz/krkrz
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(KrmoviePlugin, "krmovie.dll");
