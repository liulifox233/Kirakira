//! `sqlite3.dll` placeholder.
//!
//! Real plugin: SQLite database access from scripts.
//! Upstream: https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/sqlite3
//!
//! No implementation yet: this module installs no TJS surface and reports
//! itself through the engine log when it is registered. See
//! [`crate::catalog`] for the plugin's entry.

crate::placeholder::placeholder_plugin!(Sqlite3Plugin, "sqlite3.dll");
