//! `gamepad.dll` placeholder.
//!
//! Real plugin: Gamepad (XInput/DirectInput) input surface.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Gamepad input",
    notes: "Not implemented; input reaches the engine through krkr-core's input events only.",
    install: |engine| engine.register_plugin(GamepadPlugin),
};

crate::placeholder::placeholder_plugin!(GamepadPlugin, "gamepad.dll");
