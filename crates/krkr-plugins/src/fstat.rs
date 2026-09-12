//! `fstat.dll` placeholder.
//!
//! Real plugin: File statistics for storages and placed files.
//! Upstream: https://github.com/wtnbgo/fstat
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(FstatPlugin, "fstat.dll");
