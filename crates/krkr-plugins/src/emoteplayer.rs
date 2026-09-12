//! `emoteplayer.dll` placeholder.
//!
//! Real plugin: EmotePlayer, MPEG-based emote playback for emote/chat scenes.
//! Upstream: (no public source; in the kirikiroid2 plugin list)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "EmotePlayer (MPEG emote playback)",
    notes: "Not implemented; the motionplayer shim already installs a Motion.EmotePlayer class, so scripts see the name but not the playback.",
    install: |engine| engine.register_plugin(EmotePlayerPlugin),
};

crate::placeholder::placeholder_plugin!(EmotePlayerPlugin, "emoteplayer.dll");
