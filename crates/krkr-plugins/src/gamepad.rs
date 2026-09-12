//! `gamepad.dll` placeholder.
//!
//! Real plugin: Gamepad (XInput/DirectInput) input surface.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(GamepadPlugin, "gamepad.dll");
