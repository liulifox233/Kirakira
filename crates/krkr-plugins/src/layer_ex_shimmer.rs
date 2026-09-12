//! `layerExShimmer.dll` placeholder.
//!
//! Real plugin: Shimmer/hologram style layer effect.
//! Upstream: http://keepcreating.g2.xrea.com/krkrplugins/ShimmerPlugin/layerExShimmer.zip
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(LayerExShimmerPlugin, "layerExShimmer.dll");
