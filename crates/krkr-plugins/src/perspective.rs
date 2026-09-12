//! `perspective.dll` placeholder.
//!
//! Real plugin: Four-corner perspective transform of layer images.
//! Upstream: https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExPerspective
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Four-corner perspective transform of layer images",
    notes: "Not implemented; krkr-render has no perspective blit yet.",
    install: |engine| engine.register_plugin(PerspectivePlugin),
};

crate::placeholder::placeholder_plugin!(PerspectivePlugin, "perspective.dll");
