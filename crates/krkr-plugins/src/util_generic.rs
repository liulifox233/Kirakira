//! `util_generic.dll` placeholder.
//!
//! Real plugin: Utility plugin from the kirikiroid2 list (surface not identified yet).
//! Upstream: (no public source; in the kirikiroid2 plugin list)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(UtilGenericPlugin, "util_generic.dll");
