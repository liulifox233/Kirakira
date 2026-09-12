//! Plugin-facing engine facilities.
//!
//! A Rust plugin implements [`crate::KrkrPlugin`] in a crate whose production
//! dependencies are `krkr-engine` and `krkr-tjs2` only, so everything the
//! plugin needs from the engine has to be reachable through this crate. This
//! module is that surface:
//!
//! * [`storage`] registers a storage media (`psb`, `lzfs`, `proxy`, `steam`,
//!   `var`, `zip`) on the project storage — the engine-side counterpart of
//!   `TVPRegisterStorageMedia` (`krkrz/src/core/base/StorageIntf.cpp:530-538`).
//!
//! The core types a plugin has to name — the storage port a wrapping media
//! resolves its inner names through, and the media trait it implements — are
//! re-exported here so no plugin crate depends on `krkr-assets` or `krkr-core`
//! directly.
//!
//! Engine-owned, never plugin-side: the registry, the scheme split and
//! lowercasing, the resolve order, the write dispatch, the weak storage handle
//! and the unregistration lifecycle. The plugin owns its container format, its
//! bytes, its caching and its error strings.

pub mod storage;

pub use krkr_core::{ProjectStoragePort, ResourceData, ResourceStream, StorageMediaProvider};
