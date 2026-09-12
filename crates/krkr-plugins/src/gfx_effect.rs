//! `gfxEffect.dll` placeholder.
//!
//! Real plugin: Kaede's layer effects (blur/glow family) beyond the stock Layer API.
//! Upstream: http://kaede-software.com/krlm/plugin/gfx_effect.zip
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(GfxEffectPlugin, "gfxEffect.dll");
