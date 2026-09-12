//! `layerExSave.dll` placeholder.
//!
//! Real plugin: Saves layer images and reports the saveable formats.
//! Upstream: https://github.com/wtnbgo/layerExSave
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(LayerExSavePlugin, "layerExSave.dll");
