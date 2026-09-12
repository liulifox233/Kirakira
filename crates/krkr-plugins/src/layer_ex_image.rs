//! `layerExImage.dll` placeholder.
//!
//! Real plugin: Layer image load/blit helpers from wtnbgo's layerEx family.
//! Upstream: https://github.com/wtnbgo/layerExImage
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(LayerExImagePlugin, "layerExImage.dll");
