//! `minizip.dll` placeholder.
//!
//! Real plugin: ZIP archive reading and writing exposed to scripts.
//! Upstream: https://github.com/wtnbgo/minizip
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(MinizipPlugin, "minizip.dll");
