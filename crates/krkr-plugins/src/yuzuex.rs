//! `yuzuex.dll` — the `ProxyStorageMap` global dictionary and the `proxy`
//! storage media (proxyfs repackaged).
//!
//! The shipped binary *is* proxyfs: its export table names `proxyfs.dll`
//! (`docs/plugins/yuzuex.md:19`), and its registration function builds exactly
//! two things (`yuzuex.md:29-33`): a TJS dictionary published as the global
//! `ProxyStorageMap` with `TVPRegisterGlobalObject`, and a storage media for
//! the `proxy` domain registered with `TVPRegisterStorageMedia`, whose open
//! failure reads `cannot open proxyfile:%1` (`yuzuex.md:22`). That is also the
//! whole script-visible surface: scripts fill the dictionary, the media
//! consults it, and there are no TJS classes (`yuzuex.md:38-39`).
//!
//! # What the media does
//!
//! Both halves are real here. `ProxyStorageMap` is a TJS `Dictionary`
//! instance published at registration and empty until a script fills it —
//! what `TJSCreateDictionaryObject` hands the reference, and an ordinary
//! `Dictionary` instance in this runtime already has the reference's
//! miss-reads-as-void dispatch (`tjsDictionary.cpp:720-731`,
//! `vm/dispatch.rs`). The `proxy` media is [`ProxyMedia`], and the reference
//! DLL's own algorithm is recoverable from its decompilation (Ghidra,
//! `ProxyStorage` vtable at `0x1000b144`):
//!
//! * `GetName` (`0x10001770`) answers `proxy`; `NormalizeDomainName` /
//!   `NormalizePathName` (`0x10001320`) are no-ops.
//! * `CheckExistentStorage` (`0x10001b70`) and `Open` (`0x10001bb0`) both call
//!   the same redirect helper (`0x10001910`), which **lower-cases** the name
//!   it was handed (`tTJSString::ToLowerCase` — ASCII `A-Z` only,
//!   `krkrz/src/core/tjs2/tjsString.cpp:144-157`) and reads it out of the
//!   dictionary as one exact key (`PropGet`). Only a **string** value maps:
//!   the helper tests `tTJSVariant::Type() == tvtString` and copies the value
//!   out as the target name — no concatenation, no prefix repair, so the
//!   mapping is *exact*, not a prefix replacement.
//! * `Open` then opens the mapped target through the engine's own storage
//!   search (`TVPCreateIStream`) and raises `cannot open proxyfile:%1`
//!   (`yuzuex.md:22`) when nothing is mapped or the target will not open.
//! * `GetListAt` (`0x100017a0`) builds a `DictMemberGetCaller` over the same
//!   dictionary, i.e. a prefix scan of its keys. That lister is what the
//!   reference's auto-path rebuild reads
//!   (`TVPRebuildAutoPathTable`, `StorageIntf.cpp:1035-1144`).
//!
//! # PARQUET's use of it (the evidence for the mapping policy)
//!
//! PARQUET's `custom.tjs` (in `main.xp3`) both links the plugin and fills the
//! map, then makes the mapping reachable through an **auto path**
//! (`custom.tjs:545-552`):
//!
//! ```tjs
//! Plugins.link("proxyfs.dll");
//! if (typeof global.ProxyStorageMap == "Object") {
//!     var krm = "krmovie.dll";
//!     ProxyStorageMap["./"+krm] = System.exePath+"plugin/"+krm;
//!     Storages.addAutoPath("proxy://./");
//! }
//! ```
//!
//! So the key is the *media-relative* name as the media will see it
//! (`./krmovie.dll`, the `./` prefix included), the value is a full storage
//! name, and the prefix the dossier guessed at is not part of the mapping at
//! all: it is the auto path `proxy://./`, whose reference rebuild lists the
//! media at `./` (the lister above) and thus turns a plain `krmovie.dll`
//! request into `proxy://./krmovie.dll` for the media to map. The dictionary
//! is also used by `uistand.tjs` as a thumbnail cache that stores **octet
//! data** under `./thumb/…` keys and returns `"proxy://" + name` handles;
//! such values are not mappings — in this port and in the reference, whose
//! redirect helper demands a string.
//!
//! # The engine seam
//!
//! A media is `Send + Sync` and may be called from the resource worker
//! (`krkr-engine/src/resource_manager.rs`), while the dictionary lives in the
//! single-threaded TJS heap — the reference reads it live from whatever
//! thread resolves, which is exactly the hazard this port does not copy. The
//! engine mirrors the dictionary's strings into a [`StorageScriptTable`]
//! (`plugin_api::storage::watch_storage_dictionary`), refreshed on the script
//! thread at every tick/step, and the media consults only that table
//! (design Part A.3.5,
//! `docs/plugins/plugin-facing-engine-facilities.md`).
//!
//! # What remains open
//!
//! * **Media auto paths do not reach providers yet.** PARQUET's one real use
//!   of the mapping is the auto path above, and the engine's auto-path table
//!   is built from the filesystem/archive mounts only
//!   (`krkr-assets/src/storage.rs`, `media.rs`'s "media auto paths" note) —
//!   there is no `TVPRebuildAutoPathTable` for media, so a plain
//!   `krmovie.dll` request does not become `proxy://./krmovie.dll` here yet.
//!   A direct `proxy://./krmovie.dll` read does resolve. That discovery is
//!   engine-side follow-up work (design open question A.5.3).
//! * `GetListAt` is not implemented: the prefix scan's child-name form is not
//!   fully pinned by the decompilation, and nothing in the engine asks a
//!   media for a listing yet (the auto-path rebuild that does in the
//!   reference is the same missing piece).
//! * The three option-descriptor categories this DLL also embeds
//!   (`yuzuex.md:34-36`) are deliberately not registered from this module:
//!   they duplicate the categories `kagexopt.dll` carries, and the engine
//!   merges duplicate categories, so a second copy would have to stay
//!   idempotent for no gain (`docs/plugins/kagexopt.md:112`).

use std::{
    io,
    sync::{Arc, Mutex, OnceLock, Weak},
};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::{
        self, ProjectStoragePort, ResourceStream, StorageMediaProvider, storage::StorageScriptTable,
    },
};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "`ProxyStorageMap` global dictionary and the `proxy` storage media it feeds",
    notes: "Real: `register()` publishes `ProxyStorageMap` as a TJS Dictionary instance (the reference's \
            TJSCreateDictionaryObject + TVPRegisterGlobalObject) and registers the `proxy` media \
            (TVPRegisterStorageMedia); `unregister()` takes the media away. The media's recovered semantics \
            are the DLL's: the media-relative name is lower-cased (ASCII) and read out of the dictionary as \
            one exact key, only a string value maps, the value is the target name opened through the \
            engine's own storage, and `cannot open proxyfile:%1` is raised when nothing is mapped or the \
            target will not open — an exact mapping, not the prefix replacement the dossier guessed at \
            (PARQUET pairs the exact key `./krmovie.dll` with a `proxy://./` auto path, custom.tjs:545-552). \
            Scripts never touch TJS from the media: the engine mirrors the dictionary into a thread-safe \
            table refreshed at tick/step. Not implemented: media auto-path discovery, so PARQUET's own \
            `Storages.addAutoPath(\"proxy://./\")` route does not reach the media yet (direct `proxy://` \
            reads do), and `GetListAt` listing (its child-name form is not fully pinned).",
    install: |engine| engine.register_plugin(YuzuExPlugin::new()),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what a game's
/// `Plugins.link("yuzuex.dll")` resolves through [`crate::catalog`]. The
/// catalog also carries the DLL's own export-table name, `proxyfs.dll`, as an
/// alias (`yuzuex.md:19`).
pub(crate) const NAME: &str = "yuzuex.dll";

/// The global the reference's registration publishes with
/// `TVPRegisterGlobalObject` (`yuzuex.md:29-30`).
pub(crate) const GLOBAL_NAME: &str = "ProxyStorageMap";

/// The storage media the reference registers with `TVPRegisterStorageMedia`
/// (`yuzuex.md:32`), the name space proxyfs owns.
pub(crate) const MEDIA_NAME: &str = "proxy";

/// What that media raises when a name it is handed cannot be opened
/// (`yuzuex.md:22`, `:33`). `%1` is the message system's placeholder for that
/// name — the same `cannot open <media>file:%1` form the `steam` media carries
/// (`krkr-assets/src/media.rs:125`).
pub(crate) const OPEN_ERROR: &str = "cannot open proxyfile:%1";

pub struct YuzuExPlugin {
    /// The media the registration installs. Kept here so the double
    /// `register` (boot, then the first `Plugins.link`) hands the registry
    /// the *same* `Arc` both times; the registry treats an identical `Arc` as
    /// a no-op.
    media: OnceLock<Arc<ProxyMedia>>,
}

impl YuzuExPlugin {
    pub fn new() -> Self {
        Self {
            media: OnceLock::new(),
        }
    }
}

impl Default for YuzuExPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KrkrPlugin for YuzuExPlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // `ProxyStorageMap = TJSCreateDictionaryObject()` plus
        // `TVPRegisterGlobalObject` (`yuzuex.md:29-30`): `watch` publishes a
        // genuine TJS Dictionary when the global is absent and adopts the one
        // a script (or a re-registration) already made, so linking the module
        // twice never wipes a filled map.
        let table = plugin_api::storage::watch_storage_dictionary(runtime, GLOBAL_NAME);

        let first_register = self.media.get().is_none();
        let media = self.media.get_or_init(|| Arc::new(ProxyMedia::new(table)));
        let registered = plugin_api::storage::register_storage_media(
            runtime,
            Arc::clone(media) as Arc<dyn StorageMediaProvider>,
        );
        match registered {
            Ok(()) => {
                if first_register {
                    for line in registration_log() {
                        runtime.host_mut().log(&line);
                    }
                }
                Ok(())
            }
            // A host with no project storage (a script-only engine, a browser
            // host) has nothing a media could be inserted into. The
            // reference's `V2Link` fails registration outright, but failing a
            // whole engine boot over a media such a host cannot address is
            // not this port's call: the dictionary stays, the media does not,
            // and the log carries the reason.
            Err(error) if runtime.host().project_storage().is_err() => {
                runtime.host_mut().log(&format!(
                    "WARN yuzuex: media `{MEDIA_NAME}` not registered: {error}"
                ));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        plugin_api::storage::unregister_storage_media(runtime, MEDIA_NAME);
        Ok(())
    }
}

/// What `register` reports: the dictionary the script owns and the media that
/// reads it.
fn registration_log() -> [String; 2] {
    [
        format!(
            "yuzuex: global TJS Dictionary `{GLOBAL_NAME}` registered (proxyfs; a script fills it \
             with the virtual-to-real storage names the `{MEDIA_NAME}` media resolves through).",
        ),
        format!(
            "yuzuex: storage media `{MEDIA_NAME}` registered — a `{MEDIA_NAME}://` name is \
             lower-cased and looked up in `{GLOBAL_NAME}` as one exact key, a string value is the \
             target name opened through the built-in storage, and an unmapped name is a miss that \
             keeps resolving unchanged (`{OPEN_ERROR}` is raised when a mapped target will not open).",
        ),
    ]
}

/// The `proxy` media (`ProxyStorage` in the DLL, `proxyfs` in the wild): every
/// name it serves is a key of the watched `ProxyStorageMap` dictionary, and
/// the value under that key is the storage name the request is re-resolved to.
///
/// The media owns no bytes and no name space of its own — the target is opened
/// through the engine's built-in stack, which the engine hands over with
/// [`StorageMediaProvider::attach_storage`] before inserting the media.
struct ProxyMedia {
    /// The engine-owned mirror of `ProxyStorageMap`, refreshed on the script
    /// thread. The media never touches TJS, so this is what makes resolve-time
    /// lookups safe from the resource worker.
    table: Arc<StorageScriptTable>,

    /// The built-in stack, attached by the engine. `Weak` because the registry
    /// lives inside the storage — a strong handle here would be a reference
    /// cycle (and the target is always resolved through the built-in stack,
    /// never back into this media's own scheme).
    storage: Mutex<Option<Weak<dyn ProjectStoragePort>>>,
}

impl ProxyMedia {
    fn new(table: Arc<StorageScriptTable>) -> Self {
        Self {
            table,
            storage: Mutex::new(None),
        }
    }

    /// The name `name` is mapped to, or `None` when the script mapped nothing
    /// there.
    ///
    /// The DLL lower-cases the name before its dictionary read
    /// (`0x10001910` calls `tTJSString::ToLowerCase`, which folds ASCII
    /// `A-Z`), so the lookup is exact and case-insensitive for the ASCII names
    /// a storage path is made of.
    fn resolve(&self, name: &str) -> Option<String> {
        self.table.get(&name.to_ascii_lowercase())
    }

    /// The engine's built-in stack, or `None` once it is gone.
    fn storage(&self) -> Option<Arc<dyn ProjectStoragePort>> {
        self.storage.lock().ok()?.as_ref().and_then(Weak::upgrade)
    }
}

impl StorageMediaProvider for ProxyMedia {
    fn media_name(&self) -> &str {
        MEDIA_NAME
    }

    /// `CheckExistentStorage` (`0x10001b70`): the reference answers "is this
    /// name mapped", and nothing else — the redirect helper only probes the
    /// dictionary. A mapped name whose target is missing is the `Open`
    /// failure below, not a miss, so the media stays authoritative for its own
    /// scheme; an unmapped name is a miss and the resolver falls through to
    /// the built-in stack.
    fn exists(&self, name: &str) -> bool {
        self.resolve(name).is_some()
    }

    /// `Open` (`0x10001bb0`): resolve the name, open the target through the
    /// engine's own storage search, and raise the reference's
    /// `cannot open proxyfile:%1` when either step fails.
    fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
        let target = self.resolve(name).ok_or_else(|| open_error(name))?;
        let storage = self.storage().ok_or_else(|| no_storage_error())?;
        storage.open(&target).map_err(|_| open_error(name))
    }

    /// The write half of `Open`: the reference hands a write to
    /// `TVPCreateIStream` on the *mapped* target, so a write through the
    /// scheme lands on the real file. A name the script did not map has no
    /// target and fails with the media's own open error, exactly as it does
    /// there.
    fn write(&self, name: &str, mode: &str, bytes: &[u8]) -> io::Result<()> {
        let target = self.resolve(name).ok_or_else(|| open_error(name))?;
        let storage = self.storage().ok_or_else(|| no_storage_error())?;
        storage
            .write_binary_storage(&target, mode, bytes)
            .map_err(|_| open_error(name))
    }

    /// The engine attaches a weak handle to the built-in stack before the
    /// media is inserted (`KrkrHost::register_storage_media`); the target of a
    /// mapping is resolved through it, never through this media's own scheme
    /// (a mapping of `proxy://x` to `proxy://y` would re-enter and loop).
    fn attach_storage(&self, storage: Weak<dyn ProjectStoragePort>) {
        if let Ok(mut slot) = self.storage.lock() {
            *slot = Some(storage);
        }
    }
}

/// `Open`'s failure message with the name filled in for `%1` (`yuzuex.md:22`).
fn open_error(name: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, OPEN_ERROR.replace("%1", name))
}

/// The one failure that is not the game's: the engine never attached a storage
/// handle (a host that cannot address the scheme at all). Kept distinct from
/// the media's own message so the log names the missing wire rather than
/// blaming the script's mapping.
fn no_storage_error() -> io::Error {
    io::Error::other(format!("the `{MEDIA_NAME}` media has no storage attached"))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        path::{Path, PathBuf},
        time::SystemTime,
    };

    use krkr_assets::ProjectStorage;
    use krkr_core::{FrameInput, Size};
    use krkr_engine::{EngineConfig, EngineInput, KrkrEngine, SystemPaths};

    use super::*;
    use crate::catalog;

    fn temp_root(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-yuzuex-{prefix}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    /// A project of `root` with the plugin installed, the way a host boots it.
    fn project_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("project storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine");
        engine
            .register_plugin(YuzuExPlugin::new())
            .expect("register the yuzuex plugin");
        engine
    }

    fn write_file(root: &Path, name: &str, bytes: &[u8]) {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(path, bytes).expect("write fixture");
    }

    fn read(engine: &KrkrEngine, name: &str) -> Vec<u8> {
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");
        let data = storage
            .read_binary_storage(name)
            .unwrap_or_else(|error| panic!("read `{name}`: {error}"));
        data.as_bytes().expect("fixture bytes").into_owned()
    }

    /// `Storages.isExistentStorage`'s answer for `name`, per call so the test
    /// can keep mutating the engine between probes.
    fn exists(engine: &KrkrEngine, name: &str) -> bool {
        engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage")
            .storage_exists(name)
    }

    fn global_names(engine: &KrkrEngine) -> BTreeSet<String> {
        engine
            .tjs_runtime()
            .object_members(engine.tjs_runtime().global_handle())
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// The DLL's whole script surface is the one global (`yuzuex.md:38-39`),
    /// and what it publishes is a TJS `Dictionary` instance — the kind
    /// `TJSCreateDictionaryObject` builds (`yuzuex.md:29`) — not an ordinary
    /// object and not a class, so nothing here claims a surface the reference
    /// does not have.
    #[test]
    fn publishes_exactly_one_new_global_and_it_is_a_dictionary() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let before = global_names(&engine);
        engine
            .register_plugin(YuzuExPlugin::new())
            .expect("register");

        assert_eq!(
            global_names(&engine)
                .difference(&before)
                .cloned()
                .collect::<Vec<_>>(),
            vec![GLOBAL_NAME.to_string()],
            "the plugin installs more than the one global the dossier recovers",
        );
        let map = engine
            .tjs_runtime()
            .object_member(engine.tjs_runtime().global_handle(), GLOBAL_NAME);
        let handle = map
            .object_handle()
            .unwrap_or_else(|| panic!("`{GLOBAL_NAME}` is not an object: {map:?}"));
        assert!(
            engine.tjs_runtime().is_dictionary_instance(handle),
            "`{GLOBAL_NAME}` is not a Dictionary instance",
        );
    }

    /// Scripts own the dictionary's entries, and an entry nobody wrote reads
    /// as void rather than throwing — the miss dispatch of a
    /// `tTJSDictionaryObject` (`tjsDictionary.cpp:720-731`). The map also has
    /// to survive into the next script: a game maps its paths once at startup
    /// and reads them from every scenario afterwards, and the media
    /// (`yuzuex.md:32-33`) would consult this same object.
    #[test]
    fn scripts_fill_the_map_and_the_engine_keeps_it() {
        let mut engine = project_engine(&temp_root("keeps"));
        engine
            .execute_script("startup.tjs", r#"ProxyStorageMap["data/"] = "savedata/";"#)
            .expect("script");
        let value = engine
            .execute_script(
                "scenario.tjs",
                r#"
                return (ProxyStorageMap["data/"] === "savedata/" ? "mapped" : "wrong") + "/" +
                    (ProxyStorageMap["other/"] === void ? "void" : "value");
                "#,
            )
            .expect("script");
        assert_eq!(value.to_tjs_string().expect("string"), "mapped/void");
    }

    /// The plugin registers under the catalog's canonical name, and its log
    /// separates the surface that is real (the dictionary, a `Dictionary`
    /// object) from the media it installs (with the reference's own
    /// open-failure text quoted).
    #[test]
    fn registers_under_the_catalog_name_and_logs_the_dictionary_and_media() {
        let engine = project_engine(&temp_root("logs"));

        assert_eq!(catalog::canonical_name(NAME), Some(NAME));
        assert_eq!(catalog::canonical_name("proxyfs.dll"), Some(NAME));
        assert!(
            engine.host().linked_plugins().any(|linked| linked == NAME),
            "the plugin does not register the name the catalog resolves",
        );
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()],
            "the reference registers exactly the `proxy` media",
        );

        let logs = engine.host().logs();
        assert!(
            logs.iter()
                .any(|line| line.contains(GLOBAL_NAME) && line.contains("Dictionary")),
            "no line describing the registered global: {logs:?}",
        );
        assert!(
            logs.iter().any(|line| line.contains(MEDIA_NAME)
                && line.contains("registered")
                && line.contains(OPEN_ERROR)),
            "no line describing the registered media: {logs:?}",
        );
    }

    /// The end-to-end shape PARQUET uses (`custom.tjs:545-552`): a script maps
    /// the media-relative name to a real path through `System.exePath`, the
    /// engine refreshes the mirror at the next frame boundary (`step`), and a
    /// `proxy://` read of that name serves the target through the built-in
    /// stack. A name the script did not map — and a name mapped to a
    /// non-string value, which is `uistand.tjs`'s octet thumbnail cache —
    /// stays a miss.
    #[test]
    fn a_mapped_name_resolves_through_the_dictionary() {
        let root = temp_root("mapped");
        write_file(&root, "plugin/krmovie.dll", b"dll bytes");
        let mut engine = project_engine(&root);

        engine
            .execute_script(
                "custom.tjs",
                r#"
                var krm = "krmovie.dll";
                ProxyStorageMap["./"+krm] = System.exePath+"plugin/"+krm;
                ProxyStorageMap["./thumb/x.jpg"] = <% 01 02 %>;
                "#,
            )
            .expect("script");
        // One host frame (`step` reaches `advance`, the same boundary `tick`
        // uses) refreshes the mirror.
        engine
            .step(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                std::time::Duration::ZERO,
            )
            .expect("step");

        assert_eq!(read(&engine, "proxy://./krmovie.dll"), b"dll bytes");
        // The DLL lower-cases the name before its dictionary read
        // (`0x10001910`), so the same mapping answers a differently-cased
        // request.
        assert_eq!(read(&engine, "proxy://./KRMovie.DLL"), b"dll bytes");
        // A non-string value is not a mapping: the media's own reader demands
        // a string (`tTJSVariant::Type() == tvtString`).
        assert!(!exists(&engine, "proxy://./thumb/x.jpg"));
        assert!(!exists(&engine, "proxy://./unmapped.dll"));
        // An unmapped name is a miss, so the resolver keeps its pre-registry
        // behaviour for it rather than turning every `proxy://` name into a
        // failure.
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");
        assert!(
            storage
                .read_binary_storage("proxy://./unmapped.dll")
                .is_err()
        );
    }

    /// The mirror is refreshed on the script thread by the engine: a mapping
    /// written in one script block is not visible to a resolution until the
    /// next tick/step boundary, and it is visible after it. That is the
    /// documented timing — the table exists so a worker-thread resolution never
    /// reads the dictionary itself.
    #[test]
    fn the_table_follows_the_dictionary_at_tick_boundaries() {
        let root = temp_root("refresh");
        write_file(&root, "plugin/real.bin", b"target");
        let mut engine = project_engine(&root);

        engine
            .execute_script(
                "map.tjs",
                r#"ProxyStorageMap["./x.bin"] = "./plugin/real.bin";"#,
            )
            .expect("script");
        assert!(
            !exists(&engine, "proxy://./x.bin"),
            "a mapping must not be visible before the next refresh",
        );

        engine.tick().expect("tick");
        assert!(exists(&engine, "proxy://./x.bin"));
        assert_eq!(read(&engine, "proxy://./x.bin"), b"target");

        // Replacing the global follows the new object, and a non-object
        // global clears the table (`refresh_storage_tables` re-reads the
        // global by name every time).
        engine
            .execute_script("replace.tjs", r#"ProxyStorageMap = new Dictionary();"#)
            .expect("script");
        engine.tick().expect("tick");
        assert!(!exists(&engine, "proxy://./x.bin"));
    }

    /// The script-thread refresh point: a mapping written earlier in the
    /// *same* script block is already live for a `Storages.*` probe, because
    /// those natives refresh the watched dictionaries at entry
    /// (`native/storages.rs`) — no tick in between. That is the reference's
    /// "write the map, then read through it" usage.
    #[test]
    fn a_storages_probe_sees_a_mapping_written_in_the_same_block() {
        let root = temp_root("same-block");
        write_file(&root, "plugin/real.bin", b"target");
        let mut engine = project_engine(&root);

        let value = engine
            .execute_script(
                "same-block.tjs",
                r#"
                ProxyStorageMap["./x.bin"] = "./plugin/real.bin";
                return Storages.isExistentStorage("proxy://./x.bin") ? "mapped" : "unmapped";
                "#,
            )
            .expect("script");
        assert_eq!(value.to_tjs_string().expect("string"), "mapped");

        // The engine-side read (no `Storages.*` call to refresh it) still sees
        // the mapping once the frame boundary has passed.
        engine.tick().expect("tick");
        assert_eq!(read(&engine, "proxy://./x.bin"), b"target");
    }

    /// The table is a `Send + Sync` map of plain strings and the media holds
    /// no TJS handle, which is the whole point of the mirror: a resolution on
    /// the resource worker reads the same table the script thread refreshes.
    #[test]
    fn the_media_and_its_table_are_usable_from_another_thread() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StorageScriptTable>();
        assert_send_sync::<ProxyMedia>();

        let root = temp_root("threads");
        write_file(&root, "plugin/real.bin", b"target");
        let mut engine = project_engine(&root);
        engine
            .execute_script(
                "map.tjs",
                r#"ProxyStorageMap["./x.bin"] = "./plugin/real.bin";"#,
            )
            .expect("script");
        engine.tick().expect("tick");

        let table = engine
            .tjs_runtime()
            .host()
            .storage_script_table(GLOBAL_NAME)
            .expect("the plugin watches the dictionary");
        let worker = std::thread::spawn(move || table.get("./x.bin"));
        assert_eq!(
            worker.join().expect("worker"),
            Some("./plugin/real.bin".to_string())
        );
    }

    /// A write through the scheme lands on the mapped target, the way the
    /// reference's `Open` hands the mapped name to `TVPCreateIStream` for a
    /// write; a name with no mapping fails with the media's own error.
    #[test]
    fn writes_land_on_the_mapped_target() {
        let root = temp_root("writes");
        let mut engine = project_engine(&root);
        engine
            .execute_script(
                "map.tjs",
                r#"ProxyStorageMap["./save.dat"] = "./data/real.dat";"#,
            )
            .expect("script");
        engine.tick().expect("tick");

        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");
        storage
            .write_binary_storage("proxy://./save.dat", "", b"payload")
            .expect("mapped write");
        assert_eq!(
            fs::read(root.join("data/real.dat")).expect("target"),
            b"payload"
        );

        let error = storage
            .write_binary_storage("proxy://./unmapped.dat", "", b"payload")
            .expect_err("an unmapped write has no target");
        assert!(
            error
                .to_string()
                .contains("cannot open proxyfile:./unmapped.dat"),
            "unexpected error: {error}",
        );
    }

    /// `Plugins.unlink` runs the plugin's `unregister` (the reference's
    /// `V2Unlink`), which takes the media away; the dictionary global stays,
    /// exactly as the reference's `TVPRemoveGlobalObject` removes it only with
    /// the module.
    #[test]
    fn unlink_removes_the_media() {
        let root = temp_root("unlink");
        write_file(&root, "plugin/real.bin", b"target");
        let mut engine = project_engine(&root);
        engine
            .execute_script(
                "map.tjs",
                r#"ProxyStorageMap["./x.bin"] = "./plugin/real.bin";
                   Plugins.link("yuzuex.dll");"#,
            )
            .expect("script");
        engine.tick().expect("tick");
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );

        engine
            .execute_script("unlink.tjs", r#"Plugins.unlink("yuzuex.dll");"#)
            .expect("script");
        assert!(
            engine.host().storage_media_names().is_empty(),
            "the media outlived Plugins.unlink",
        );
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");
        assert!(
            storage.read_binary_storage("proxy://./x.bin").is_err(),
            "an unregistered scheme falls through to the built-in stack",
        );
        // The target itself is still reachable by its real name.
        assert_eq!(read(&engine, "./plugin/real.bin"), b"target");
    }

    /// The media itself, without the engine: the lookup is the table's, the
    /// target is opened through the attached stack, and a name the script did
    /// not map fails with the reference's own message (`yuzuex.md:22`).
    #[test]
    fn the_media_owns_no_bytes_and_reports_the_reference_error_text() {
        let root = temp_root("unit");
        write_file(&root, "plugin/real.bin", b"target");
        let mut engine = project_engine(&root);
        engine
            .execute_script(
                "map.tjs",
                r#"ProxyStorageMap["./plugin/real.bin"] = "./plugin/real.bin";"#,
            )
            .expect("script");
        engine.tick().expect("tick");
        let table = engine
            .tjs_runtime()
            .host()
            .storage_script_table(GLOBAL_NAME)
            .expect("the plugin watches the dictionary");
        let storage: Arc<dyn ProjectStoragePort> =
            Arc::new(ProjectStorage::for_root(&root).expect("project storage"));

        let media = ProxyMedia::new(table);
        media.attach_storage(Arc::downgrade(&storage));
        assert_eq!(media.media_name(), MEDIA_NAME);
        assert!(media.exists("./plugin/real.bin"));
        let mut stream = media.open("./plugin/real.bin").expect("mapped open");
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut bytes).expect("read");
        assert_eq!(bytes, b"target");

        // Nothing is mapped in a fresh table, so every name is a miss —
        // including the target's own name, which the scheme itself never
        // serves — and opening one raises the reference's text.
        let unmapped = ProxyMedia::new(Arc::new(StorageScriptTable::new()));
        unmapped.attach_storage(Arc::downgrade(&storage));
        assert!(!unmapped.exists("./plugin/real.bin"));
        let error = unmapped
            .open("./plugin/real.bin")
            .map(|_| ())
            .expect_err("an unmapped name must fail");
        assert_eq!(error.to_string(), "cannot open proxyfile:./plugin/real.bin");
    }
}
