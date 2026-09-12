//! `xp3filter.dll` placeholder.
//!
//! Real plugin: Archive filter that hooks XP3 reads.
//! Upstream: (no public source; in the kirikiroid2 plugin list)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "XP3 read filtering",
    notes: "Not implemented; archive filtering would have to happen inside krkr-xp3's read path.",
    install: |engine| engine.register_plugin(Xp3FilterPlugin),
};

crate::placeholder::placeholder_plugin!(Xp3FilterPlugin, "xp3filter.dll");
