//! `layerExMovie.dll` placeholder.
//!
//! Real plugin: Draws a movie into a layer image.
//! Upstream: https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExMovie
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "Movie drawn into a layer image",
    notes: "Not implemented; would sit on top of the engine's native video decode.",
    install: |engine| engine.register_plugin(LayerExMoviePlugin),
};

crate::placeholder::placeholder_plugin!(LayerExMoviePlugin, "layerExMovie.dll");
