//! `layerExMovie.dll` placeholder.
//!
//! Real plugin: Draws a movie into a layer image.
//! Upstream: https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExMovie
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(LayerExMoviePlugin, "layerExMovie.dll");
