//! `DrawDeviceD3D.dll` placeholder.
//!
//! Real plugin: Direct3D draw device (Window/Viewport rendering backend).
//! Upstream: https://github.com/krkrz/krkr2/tree/master/kirikiri2/trunk/kirikiri2/src/plugins/win32/drawdeviceD3D
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(DrawDeviceD3DPlugin, "DrawDeviceD3D.dll");
