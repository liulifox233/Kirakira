//! `menu.dll` placeholder.
//!
//! Real plugin: Native menu (menu bar / context menu) support.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Native menu (menu bar / context menu)",
    notes: "Not implemented; the windowed shells have no menu surface yet.",
    install: |engine| engine.register_plugin(MenuPlugin),
};

crate::placeholder::placeholder_plugin!(MenuPlugin, "menu.dll");
