//! `shrinkCopy.dll` placeholder.
//!
//! Real plugin: Downscaled blit onto layers.
//! Upstream: https://github.com/wtnbgo/shrinkCopy
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(ShrinkCopyPlugin, "shrinkCopy.dll");
