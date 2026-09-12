//! `xp3filter.dll` placeholder.
//!
//! Real plugin: Archive filter that hooks XP3 reads.
//! Upstream: (no public source; in the kirikiroid2 plugin list)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(Xp3FilterPlugin, "xp3filter.dll");
