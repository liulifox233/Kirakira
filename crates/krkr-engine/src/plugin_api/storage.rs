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
//!
//! # Script-fed tables
//!
//! Two of the reference media read their state out of a TJS dictionary at
//! resolve time: `var` resolves its whole namespace from the script global
//! (`varfile/Main.cpp:33-41, 324-358, 381-445`) and `yuzuex`'s `proxy` reads
//! `ProxyStorageMap` (`docs/plugins/yuzuex.md:29-33`). The reference does that
//! straight from whatever thread resolution runs on; here a provider is
//! `Send + Sync` and may be called from a resource worker, so a TJS handle
//! must never reach one. [`watch_storage_dictionary`] instead mirrors the
//! global dictionary's **strings** into a plain [`StorageScriptTable`] on the
//! script thread, and [`refresh_storage_tables`] re-reads the watched globals
//! at every engine tick/step and at every script-thread `Storages.*` read-path
//! probe; the media consults the table and stays TJS-free. The design is Part
//! A.3.5 of `docs/plugins/plugin-facing-engine-facilities.md`.
//!
//! What a mapping *means* — exact entry or prefix, which case keys are stored
//! and looked up in, what an unmapped name resolves to — stays the media's
//! policy, exactly as it is the reference DLL's; the table only mirrors what
//! the script wrote.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use krkr_core::StorageMediaProvider;
use krkr_tjs2::{
    Result,
    runtime::{Runtime, Variant},
};

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

/// The mirror of one watched global dictionary: the `key -> value` string
/// mappings a script wrote, readable from any thread.
///
/// This is the point of the type: a media may run on the script thread, on a
/// resource worker or on a media thread (`StorageMediaProvider` is
/// `Send + Sync`, `krkr-core/src/media.rs`), while the dictionary it mirrors
/// lives in the single-threaded TJS heap. The engine copies the strings out on
/// the script thread ([`refresh_storage_tables`]) and the media reads them
/// here; no TJS handle crosses a thread boundary.
///
/// Keys and values are exactly what the script wrote. Only string values are
/// copied — an octet, integer, object or `void` value is not a name, and the
/// reference's own readers demand a string (the `proxy` media treats a
/// non-string `ProxyStorageMap` value as unmapped; `varfile` is the one
/// reference reader that accepts octets, for a *file* value rather than a name
/// mapping). The lookup is exact; a media that wants prefix semantics scans
/// [`entries`](Self::entries), the way the reference media's lister prefixes
/// its dictionary scan.
#[derive(Debug, Default)]
pub struct StorageScriptTable {
    entries: RwLock<BTreeMap<String, String>>,
}

impl StorageScriptTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// The value mapped to `key`, or `None` when the script mapped nothing
    /// under it. Exact, case- and separator-preserving: policy belongs to the
    /// media.
    pub fn get(&self, key: &str) -> Option<String> {
        self.read().get(key).cloned()
    }

    /// A snapshot of every mapping, sorted by key.
    pub fn entries(&self) -> Vec<(String, String)> {
        self.read()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, String>> {
        // A panic while a script-thread refresh held the write lock must not
        // take the engine's storage resolution down with it: the map is a
        // mirror and a half-written refresh is simply replaced by the next
        // one, so a poisoned lock is recovered rather than propagated.
        self.entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn replace(&self, entries: BTreeMap<String, String>) {
        let mut guard = self
            .entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *guard != entries {
            *guard = entries;
        }
    }
}

/// Starts mirroring the global `global_name` into a [`StorageScriptTable`] and
/// returns the table a media holds.
///
/// The global is adopted as it stands when it is an object — the way
/// `yuzuex` publishes its own `Dictionary` before watching it, mirroring the
/// reference's `TJSCreateDictionaryObject` + `TVPRegisterGlobalObject`
/// (`docs/plugins/yuzuex.md:29-30`). When the global is absent or is not an
/// object at all, a genuine TJS `Dictionary` instance
/// (`Runtime::alloc_dictionary_object`) is published under the name, which is
/// the object the reference declares. The table starts filled with whatever
/// the dictionary already holds.
///
/// Calling this twice for one name — the engine runs every plugin's `register`
/// at boot *and* again on the first `Plugins.link` — returns the **same**
/// table, so a media built once around the first call keeps seeing every
/// refresh. Script-thread only.
pub fn watch_storage_dictionary(
    runtime: &mut Runtime<KrkrHost>,
    global_name: &str,
) -> Arc<StorageScriptTable> {
    publish_dictionary_global(runtime, global_name);
    let table = runtime
        .host_mut()
        .watch_storage_table(global_name, Arc::new(StorageScriptTable::new()));
    refresh_storage_table(runtime, global_name);
    table
}

/// Re-reads every watched global dictionary into its table.
///
/// The engine runs this at the top of every frame/turn boundary
/// (`KrkrEngine::tick`/`step`), so a mapping written by a script is visible to
/// resolutions — script-thread and worker-thread alike — from the next turn
/// on. The `Storages.*` read-path natives (`isExistentStorage`,
/// `isExistentDirectory`, `dirlist`, `getPlacedPath`) also run it at entry,
/// so a mapping written earlier in the **same** script block is live for a
/// script-thread probe — the reference's "fill the map, then read through it"
/// usage.
///
/// A global a script replaced with a non-object clears its table, and so does
/// a global that no longer exists. Script-thread only: it reads the TJS heap.
pub fn refresh_storage_tables(runtime: &mut Runtime<KrkrHost>) {
    for name in runtime.host().storage_script_table_names() {
        refresh_storage_table(runtime, &name);
    }
}

/// Publishes `global_name` as a TJS `Dictionary` when the global is absent or
/// is not an object, and leaves an existing object alone (it is the script's
/// own map, or the plugin's).
fn publish_dictionary_global(runtime: &mut Runtime<KrkrHost>, global_name: &str) {
    if runtime.global_member(global_name).object_handle().is_some() {
        return;
    }
    let dictionary = runtime.alloc_dictionary_object();
    runtime.set_global_member(global_name, Variant::Object(dictionary));
}

fn refresh_storage_table(runtime: &Runtime<KrkrHost>, global_name: &str) {
    let Some(table) = runtime.host().storage_script_table(global_name) else {
        return;
    };
    table.replace(script_string_members(runtime, global_name));
}

/// The string members of the watched global, as a fresh map. A non-object
/// global (including `void`) has none.
fn script_string_members(
    runtime: &Runtime<KrkrHost>,
    global_name: &str,
) -> BTreeMap<String, String> {
    let Some(object) = runtime.global_member(global_name).object_handle() else {
        return BTreeMap::new();
    };
    runtime
        .object_members(object)
        .into_iter()
        .filter_map(|(key, value)| match value {
            Variant::String(text) => Some((key, text)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{EngineConfig, KrkrEngine};

    use super::*;

    fn engine() -> KrkrEngine {
        KrkrEngine::new(EngineConfig::default()).expect("engine")
    }

    fn dictionary_handle(
        engine: &KrkrEngine,
        name: &str,
    ) -> Option<krkr_tjs2::runtime::ObjectHandle> {
        engine.tjs_runtime().global_member(name).object_handle()
    }

    /// Watching publishes a genuine TJS `Dictionary` when the global is absent
    /// — the object the reference's `TVPRegisterGlobalObject` declares — and
    /// adopting it later must not replace a dictionary a script already made
    /// (the engine runs every plugin's `register` twice).
    #[test]
    fn watch_publishes_or_adopts_the_global_and_keeps_one_table() {
        let mut engine = engine();
        let table = watch_storage_dictionary(engine.tjs_runtime_mut(), "WatchTarget");
        let handle = dictionary_handle(&engine, "WatchTarget").expect("global object");
        assert!(engine.tjs_runtime().is_dictionary_instance(handle));

        // A second watch of the same name returns the same table, so the media
        // built around the first one keeps seeing every refresh.
        let again = watch_storage_dictionary(engine.tjs_runtime_mut(), "WatchTarget");
        assert!(Arc::ptr_eq(&table, &again));
        assert_eq!(
            engine.tjs_runtime().host().storage_script_table_names(),
            vec!["WatchTarget".to_string()],
        );

        // A script-written dictionary is adopted, not replaced: replacing the
        // global with the script's own object keeps the same table, and the
        // table follows the new object.
        engine
            .execute_script("adopt.tjs", r#"WatchTarget = %["a" => "b"];"#)
            .expect("script");
        let adopted = watch_storage_dictionary(engine.tjs_runtime_mut(), "WatchTarget");
        assert!(Arc::ptr_eq(&table, &adopted));
        assert_eq!(adopted.get("a"), Some("b".to_string()));
        let adopted_handle = dictionary_handle(&engine, "WatchTarget").expect("global object");
        assert!(
            engine.tjs_runtime().is_dictionary_instance(adopted_handle),
            "the adopted global is the script's own dictionary",
        );
    }

    /// Only string values are mirrored: an octet, integer or object value is
    /// not a storage name (`yuzuex`'s `uistand` keeps octet thumbnails in the
    /// same dictionary, and the reference media demands a string).
    #[test]
    fn refresh_mirrors_string_members_only() {
        let mut engine = engine();
        let table = watch_storage_dictionary(engine.tjs_runtime_mut(), "Mixed");
        engine
            .execute_script(
                "mixed.tjs",
                r#"Mixed["s"] = "target"; Mixed["o"] = <% 01 %>; Mixed["i"] = 7;
                   Mixed["a"] = %["k" => 1];"#,
            )
            .expect("script");
        refresh_storage_tables(engine.tjs_runtime_mut());

        assert_eq!(
            table.entries(),
            vec![("s".to_string(), "target".to_string())]
        );
        assert_eq!(table.get("o"), None);
        assert_eq!(table.get("i"), None);
        assert_eq!(table.len(), 1);
    }

    /// The refresh re-reads the global *by name*: replacing it follows the new
    /// object, and a non-object global (a script can assign anything) clears
    /// the table rather than leaving stale mappings that resolve.
    #[test]
    fn a_non_object_global_clears_the_table() {
        let mut engine = engine();
        let table = watch_storage_dictionary(engine.tjs_runtime_mut(), "Replaced");
        engine
            .execute_script("old.tjs", r#"Replaced["./x"] = "./y";"#)
            .expect("script");
        refresh_storage_tables(engine.tjs_runtime_mut());
        assert_eq!(table.get("./x"), Some("./y".to_string()));

        engine
            .execute_script("new.tjs", r#"Replaced = %["./x" => "./z"];"#)
            .expect("script");
        refresh_storage_tables(engine.tjs_runtime_mut());
        assert_eq!(table.get("./x"), Some("./z".to_string()));

        engine
            .execute_script("gone.tjs", r#"Replaced = 3;"#)
            .expect("script");
        refresh_storage_tables(engine.tjs_runtime_mut());
        assert!(table.is_empty());
    }

    /// A refresh with nothing watched touches no TJS state at all — the engine
    /// calls it every frame, including for games with no media.
    #[test]
    fn refresh_without_watches_is_a_no_op() {
        let mut engine = engine();
        refresh_storage_tables(engine.tjs_runtime_mut());
        assert!(
            engine
                .tjs_runtime()
                .host()
                .storage_script_table_names()
                .is_empty()
        );
    }
}
