//! Storage media registration for plugins (`TVPRegisterStorageMedia`).
//!
//! A media is the plugin-facing way to own a URI scheme: `psbfile` serves
//! `psb://container.psb/entry`, `lzfs` serves `lzfs://./data/file.bin`,
//! `yuzuex`'s `proxy` re-resolves mapped paths, `krkrsteam` serves
//! `steam://./save.dat`, `minizip` serves `zip://` and `varfile` serves
//! `var://` — all through the ordinary `TVPCreateStream`/`Storages.*` entry
//! points, with no script-visible surface.
//!
//! Call these from `KrkrPlugin::register`/`KrkrPlugin::unregister`:
//!
//! ```ignore
//! fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
//!     let media = self
//!         .media
//!         .get_or_init(|| Arc::new(ProxyMedia::new()) as Arc<dyn StorageMediaProvider>);
//!     plugin_api::storage::register_storage_media(runtime, Arc::clone(media))
//! }
//!
//! fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
//!     plugin_api::storage::unregister_storage_media(runtime, "proxy");
//!     Ok(())
//! }
//! ```
//!
//! Two rules the engine's lifecycle imposes on a media-carrying plugin:
//!
//! * `KrkrPlugin::register` runs at boot (`KrkrEngine::register_plugin`) and
//!   again when the first `Plugins.link` installs the module, and the second
//!   call is intentional (it re-installs class members a patch script may have
//!   shadowed). Keep the media in a `OnceLock` and hand back the **same
//!   `Arc`**; the registry treats an identical `Arc` as a no-op, while a
//!   different provider under a name already taken fails the way
//!   `TVPMediaNameHadAlreadyBeenRegistered` does
//!   (`StorageIntf.cpp:224-236`). Every other side effect of `register`
//!   (logging, options) has to be idempotent too.
//! * The engine fills in `StorageMediaProvider::attach_storage` with a weak
//!   handle to the project storage before the media is inserted, so a wrapping
//!   media (`lzfs`, `proxy`) can resolve inner names through the built-in
//!   stack without a reference cycle.

use std::sync::Arc;

use krkr_core::StorageMediaProvider;
use krkr_tjs2::{Result, runtime::Runtime};

use crate::host::KrkrHost;

/// Registers `media` on the project storage — the counterpart of
/// `TVPRegisterStorageMedia` (`StorageIntf.cpp:530-538`).
///
/// Fails when no project storage is configured (a host that only runs scripts)
/// or when the storage backend has no media registry — the browser and test
/// hosts answer `Unsupported` instead of panicking.
pub fn register_storage_media(
    runtime: &mut Runtime<KrkrHost>,
    media: Arc<dyn StorageMediaProvider>,
) -> Result<()> {
    runtime.host_mut().register_storage_media(media)
}

/// Unregisters the media registered under `media_name` — the counterpart of
/// `TVPUnregisterStorageMedia` (`StorageIntf.cpp:535-538`). Returns whether a
/// media was registered; the name is matched case-insensitively.
///
/// A plugin drops its media here, from `KrkrPlugin::unregister`
/// (`Plugins.unlink`), the way the reference unregisters around module unlink
/// (`varfile/Main.cpp:451-477`).
pub fn unregister_storage_media(runtime: &mut Runtime<KrkrHost>, media_name: &str) -> bool {
    runtime.host_mut().unregister_storage_media(media_name)
}

/// Names of the registered storage media, sorted. Diagnostics and tests only;
/// the built-in `file` media is implicit and never listed.
pub fn storage_media_names(runtime: &Runtime<KrkrHost>) -> Vec<String> {
    runtime.host().storage_media_names()
}
