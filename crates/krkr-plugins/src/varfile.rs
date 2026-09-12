//! `varfile.dll` placeholder.
//!
//! Real plugin: Virtual/bundled data files mounted as storages.
//! Upstream: https://github.com/wtnbgo/varfile
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(VarfilePlugin, "varfile.dll");
