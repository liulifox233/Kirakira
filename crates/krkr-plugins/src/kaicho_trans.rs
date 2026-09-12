//! `KaichoTrans.dll` placeholder.
//!
//! Real plugin: Extra Layer.beginTransition methods (kaicho family).
//! Upstream: http://keepcreating.g2.xrea.com/krkrplugins/KaichoTrans/KaichoTrans.zip
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(KaichoTransPlugin, "KaichoTrans.dll");
