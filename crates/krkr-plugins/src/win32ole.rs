//! `win32ole.dll` placeholder.
//!
//! Real plugin: Win32 OLE automation access from scripts.
//! Upstream: (no public source; PARQUET ships it)
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(Win32OlePlugin, "win32ole.dll");
