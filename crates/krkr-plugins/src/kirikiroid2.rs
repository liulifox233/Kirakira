//! `kirikiroid2.dll` placeholder.
//!
//! Real plugin: kirikiroid2 (Android port) runtime module.
//! Upstream: (no public source; in the kirikiroid2 plugin list)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(Kirikiroid2Plugin, "kirikiroid2.dll");
