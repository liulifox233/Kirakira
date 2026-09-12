//! `DrawDeviceD3DZ.dll` placeholder.
//!
//! Real plugin: krkrz Direct3D draw device variant.
//! Upstream: https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/drawdeviceD3D
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(DrawDeviceD3DZPlugin, "DrawDeviceD3DZ.dll");
