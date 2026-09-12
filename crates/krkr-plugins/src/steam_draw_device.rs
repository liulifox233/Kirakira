//! `SteamDrawDevice.dll` placeholder.
//!
//! Real plugin: Steam-overlay compatible draw device.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(SteamDrawDevicePlugin, "SteamDrawDevice.dll");
