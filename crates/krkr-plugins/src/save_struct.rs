//! `savestruct.dll` placeholder.
//!
//! Real plugin: TJS object graph (de)serialization for save data.
//! Upstream: https://github.com/wtnbgo/saveStruct
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(SaveStructPlugin, "savestruct.dll");
