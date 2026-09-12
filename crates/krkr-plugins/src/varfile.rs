//! `varfile.dll` — the `var` storage media, fed by a script dictionary.
//!
//! The reference has **no TJS surface at all**: its pre-registration callback
//! builds one `VarStorage` and hands it to `TVPRegisterStorageMedia`
//! (`Main.cpp:456-462`), the post-unregistration callback takes it away
//! (`Main.cpp:467-474`), and the media declares no class, member or option —
//! scripts only ever address `var://./…` names (`readme.txt`: "the path
//! hierarchy is the variable hierarchy below the global"). What the media
//! serves is the script's own variable space:
//!
//! * A name is split at its first `/`: the part before it is the domain, the
//!   rest the path (`Main.cpp:383-395`). The domain must be `.`
//!   (`no such domain:%1`), and a name with no `/` at all is
//!   `invalid path:%1`.
//! * The path then *walks* the script global object: every component but the
//!   last is looked up as a member that must be an object (numeric components
//!   index Arrays), and an empty component clears the base
//!   (`Main.cpp:397-422`). A trailing `/` addresses the object itself — a
//!   directory.
//! * The leaf must be an octet (`isFile`, `Main.cpp:14-17`): that octet *is*
//!   the file, and reads reference it in place (`Main.cpp:38-41`, `:93-116`).
//! * A write opens a stream over a copy of the octet
//!   (create/update/append, `Main.cpp:43-61`) and stores the resulting octet
//!   back into the variable when the stream closes
//!   (`PropSet(TJS_MEMBERENSURE)`, `Main.cpp:187-207`).
//! * `CheckExistentStorage` answers whether the leaf is an octet
//!   (`:324-326`); `Open` raises `cannot open memfile:%1` when the walk found
//!   no directory or no leaf (`:331-348`); `GetListAt` lists the *octet*
//!   members of the addressed object, skipping hidden members (`:239-273`,
//!   filter `:259-263`); `GetLocallyAccessibleName` answers `""` (`:363-365`);
//!   `NormalizeDomainName`/`NormalizePathName` are no-ops (`:314-321`).
//!
//! # What this port keeps, and where it diverges
//!
//! The engine hands a media `domain/path` and lets it own a scheme
//! (`krkr-core/src/media.rs:52-63`), which is exactly the reference's shape:
//! the `var` media is registered on the project storage through
//! `plugin_api::storage` and `Plugins.unlink` takes it away again. What a
//! `Send + Sync` media cannot do is read the TJS heap: the engine mirrors the
//! **strings** of one watched global dictionary into a thread-safe
//! [`StorageScriptTable`] on the script thread
//! (`plugin_api::storage::watch_storage_dictionary`), and that mirror is the
//! media's only view of the script. Four consequences, all deliberate:
//!
//! 1. **The namespace root is a table, not the global object.** The plugin
//!    publishes a dictionary global, [`TABLE_GLOBAL`], because the seam
//!    watches a *named* global and the reference's root — the script global
//!    object — has no name to watch. It is the port's only script surface;
//!    the reference has none.
//! 2. **Keys are flat paths.** A key `"dir/name"` stands for the reference's
//!    `global.dir.name`; the mirror carries only the watched dictionary's
//!    direct members, so a path that would walk two objects is spelled into
//!    one key and nested dictionaries are not reachable.
//! 3. **Values are strings.** The reference demands an octet; the mirror
//!    copies strings only (an octet, integer or `void` value is dropped), so
//!    an entry is a file whose bytes are the string's UTF-8 encoding — and an
//!    octet entry, the reference's file type, is *invisible* here. This is
//!    the gap the M84 seam review named ("`varfile` would need … an
//!    octet-valued mirror").
//! 4. **Writes cannot come back.** A write through `var://` is refused with
//!    an explicit error: the reference stores the octet back into the script
//!    variable on stream close, and nothing in the engine can push data from
//!    a media into TJS.
//!
//! Everything else is the reference's: the fixed domain and its errors, the
//! leaf rule (`exists` is true exactly for a file), the two directory forms
//! (`var://./dir/` and `var://./`), `cannot open memfile:%1` for everything
//! the walk does not resolve, and a listing of direct *files* only. Three
//! smaller divergences ride on the seam: the listing is sorted where the
//! reference walks the dictionary's own enumeration order; the mirror cannot
//! see the hidden-member flag, so a hidden *string* member would be listed
//! where the reference skips it; and `exists` has no error channel, so a bad
//! domain answers "not here" (a miss, and the resolver keeps walking) where
//! the reference's `CheckExistentStorage` throws — `open` and a media write
//! still report the reference's own texts.
//!
//! The media owns its bytes and never resolves anything through the built-in
//! stack, so unlike `lzfs`/`proxy` it takes no `attach_storage` handle.

use std::{
    io::{self, Cursor},
    sync::{Arc, OnceLock},
};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::{self, ResourceStream, StorageMediaProvider, storage::StorageScriptTable},
};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "the `var` storage media (script variables served as `var://./path` names)",
    notes: "Real: `register()` publishes the script table global and registers the `var` media \
            (TVPRegisterStorageMedia, Main.cpp:456-462); `unregister()` takes the media away \
            (Main.cpp:467-474). A `var://./path` name splits at the fixed `.` domain (Main.cpp:383-395); \
            an exact table key is a file whose string contents are served as UTF-8 bytes; a key prefix is \
            a directory (`var://./dir/`); an absent, void or non-string entry is a miss, like the \
            reference's non-octet leaf; `cannot open memfile:%1` (Main.cpp:345) is raised for an \
            unresolved name; `GetListAt` lists the direct files only (Main.cpp:259-263). Deliberate gaps \
            against the reference: values must be strings (the engine's script-table mirror copies \
            strings only, so an octet entry — the reference's file type — is invisible; the M84 seam \
            review named this), keys are flat paths (nested dictionaries are not mirrored), the root is a \
            port-only dictionary global instead of the script global object, writes through `var://` are \
            refused because the mirror cannot write back into the script (the reference stores the octet \
            on stream close), and listings are sorted where the reference walks dictionary order.",
    install: |engine| engine.register_plugin(VarfilePlugin::new()),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what a game's
/// `Plugins.link("varfile.dll")` resolves through [`crate::catalog`].
pub(crate) const NAME: &str = "varfile.dll";

/// The media name the reference registers with `TVPRegisterStorageMedia`
/// (`BASENAME L"var"`, `Main.cpp:7`; `GetName` `:306-308`).
pub(crate) const MEDIA_NAME: &str = "var";

/// The dictionary global the media consults. **Port-only**: the reference
/// roots its namespace at the script global object (`Main.cpp:395-397`),
/// which the engine's script-table mirror cannot watch, so the plugin
/// publishes this dictionary as the root stand-in (divergence 1 in the
/// module docs).
pub(crate) const TABLE_GLOBAL: &str = "VarStorageMap";

/// `Open`'s failure message (`Main.cpp:345`). `%1` is the reference's
/// placeholder for the name the media was handed, domain included.
pub(crate) const OPEN_ERROR: &str = "cannot open memfile:%1";

/// `getParentName`'s domain error (`Main.cpp:388`); `%1` is the domain.
pub(crate) const NO_SUCH_DOMAIN_ERROR: &str = "no such domain:%1";

/// `getParentName`'s path error (`Main.cpp:391`); `%1` is the media name.
pub(crate) const INVALID_PATH_ERROR: &str = "invalid path:%1";

/// `VarStorage`'s plugin shell: the pre-registration callback registers the
/// one media, the post-unregistration callback takes it away
/// (`Main.cpp:451-477`).
pub struct VarfilePlugin {
    /// The media instance. `register` runs at boot and again when the first
    /// `Plugins.link` installs the module, and the storage registry accepts a
    /// second registration only when it is the *same* `Arc` — a different
    /// provider under a name already taken is refused the way
    /// `TVPMediaNameHadAlreadyBeenRegistered` is. Keeping the media here makes
    /// both calls hand back one instance.
    media: OnceLock<Arc<VarMedia>>,
}

impl VarfilePlugin {
    pub fn new() -> Self {
        Self {
            media: OnceLock::new(),
        }
    }
}

impl Default for VarfilePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KrkrPlugin for VarfilePlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The media resolves every name out of the engine-owned mirror of the
        // script table (design Part A.3.5 of
        // `docs/plugins/plugin-facing-engine-facilities.md`): the engine
        // copies the dictionary's strings on the script thread at every
        // tick/step and at every `Storages.*` read-path probe, so the media
        // never touches the single-threaded TJS heap.
        let table = plugin_api::storage::watch_storage_dictionary(runtime, TABLE_GLOBAL);

        let first_register = self.media.get().is_none();
        let media = self.media.get_or_init(|| Arc::new(VarMedia::new(table)));
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
            // host) has nothing a media could be inserted into and no way to
            // address `var://` at all — the same shape every media plugin
            // here handles. The reference's `V2Link` fails registration
            // outright, but failing a whole engine boot over a media such a
            // host cannot use is not this port's call: the table stays, the
            // media does not, and the log carries the reason.
            Err(error) if runtime.host().project_storage().is_err() => {
                runtime.host_mut().log(&format!(
                    "WARN varfile: media `{MEDIA_NAME}` not registered: {error}"
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

/// What `register` reports: the table the script fills and the media that
/// reads it.
fn registration_log() -> [String; 2] {
    [
        format!(
            "varfile: global TJS Dictionary `{TABLE_GLOBAL}` registered (the port's namespace root: \
             the reference reads the script global object itself, which the engine's script-table \
             mirror cannot watch; a script fills the table with `path` -> file-contents entries).",
        ),
        format!(
            "varfile: storage media `{MEDIA_NAME}` registered — a `{MEDIA_NAME}://./path` name splits at \
             the fixed `.` domain, an exact `{TABLE_GLOBAL}` key is a file (string contents served as \
             UTF-8 bytes), a key prefix is a directory, and an unresolved name raises `{OPEN_ERROR}`.",
        ),
    ]
}

/// The `var` media (`VarStorage` in the reference): a read-only view of the
/// script table, shaped like the reference's walk of the script global.
struct VarMedia {
    /// The engine-owned mirror of [`TABLE_GLOBAL`], refreshed on the script
    /// thread. The media never touches TJS, so this is what makes resolve-time
    /// lookups safe from the resource worker.
    table: Arc<StorageScriptTable>,
}

impl VarMedia {
    fn new(table: Arc<StorageScriptTable>) -> Self {
        Self { table }
    }
}

impl StorageMediaProvider for VarMedia {
    fn media_name(&self) -> &str {
        MEDIA_NAME
    }

    /// `CheckExistentStorage` (`Main.cpp:324-326`): true exactly when the walk
    /// finds a leaf that is a file. A directory (the reference's object), the
    /// root, a blocked path and every unresolved name answer false; so does a
    /// bad domain, which the reference's probe *throws* for — this trait's
    /// probe has no error channel, and a `false` here is the miss the resolver
    /// then treats as "keep searching" (`krkr-core/src/media.rs:69-73`).
    fn exists(&self, name: &str) -> bool {
        let Ok(path) = split_domain(name) else {
            return false;
        };
        match shape_of(path) {
            VarShape::File { key, .. } => self.table.get(&key).is_some(),
            VarShape::Directory { .. } | VarShape::Blocked => false,
        }
    }

    /// `Open` (`Main.cpp:331-348`): `getParentName` first — its two errors are
    /// raised before anything else — then the leaf read out of the table. The
    /// reference reads the octet in place; here the entry's UTF-8 bytes are
    /// served from memory (divergence 3).
    fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
        let path = split_domain(name)?;
        match shape_of(path) {
            VarShape::File { key, .. } => match self.table.get(&key) {
                Some(contents) => Ok(Box::new(Cursor::new(contents.into_bytes()))),
                None => Err(open_error(name)),
            },
            // A directory (or the root) is not a file, and a blocked walk left
            // no base: both are the reference's `cannot open memfile:%1`
            // (`Main.cpp:344-347`).
            VarShape::Directory { .. } | VarShape::Blocked => Err(open_error(name)),
        }
    }

    /// The entry's length without opening it. Same resolution and same error
    /// as [`Self::open`].
    fn byte_len(&self, name: &str) -> io::Result<Option<u64>> {
        let path = split_domain(name)?;
        match shape_of(path) {
            VarShape::File { key, .. } => match self.table.get(&key) {
                Some(contents) => Ok(Some(contents.len() as u64)),
                None => Err(open_error(name)),
            },
            VarShape::Directory { .. } | VarShape::Blocked => Err(open_error(name)),
        }
    }

    /// `GetListAt` (`Main.cpp:351-358`): the addressed name must be a
    /// directory (the root, or a path with a trailing `/`), and what it lists
    /// are its direct *files* — the reference's `GetLister` passes only octet
    /// members on, so nested objects are invisible in a listing
    /// (`:259-263`). A name this media does not serve as a directory is the
    /// trait's `NotFound` miss; a bad domain is the reference's throw.
    fn list(&self, name: &str) -> io::Result<Vec<String>> {
        let path = split_domain(name)?;
        match shape_of(path) {
            VarShape::Directory { path } if is_directory(self.table.as_ref(), &path) => {
                Ok(list_files(self.table.as_ref(), &path))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("storage media `{MEDIA_NAME}` does not list `{name}`"),
            )),
        }
    }

    /// The write gap (divergence 4): a name the reference's `Open` would
    /// *reject* fails with the reference's own message, and a write the
    /// reference would have accepted is refused with the reason, instead of
    /// silently vanishing. The reference clones the target octet, lets the
    /// engine write, and stores the result back into the script variable when
    /// the stream closes (`Main.cpp:43-61`, `:187-207`); the script-table
    /// mirror is one-way, so that store has nowhere to go.
    ///
    /// One corner of the flat model shows here: `parent` exists when some key
    /// is a file inside it, so a write into an existing-but-empty dictionary
    /// (which the reference would accept) is reported as the reference's open
    /// failure, and a write into a *directory that does not exist yet* is
    /// reported as the port gap — both are refusals either way.
    fn write(&self, name: &str, mode: &str, bytes: &[u8]) -> io::Result<()> {
        let _ = (mode, bytes);
        let path = split_domain(name)?;
        match shape_of(path) {
            VarShape::File { parent, .. } if is_directory(self.table.as_ref(), &parent) => {
                Err(write_gap_error(name))
            }
            _ => Err(open_error(name)),
        }
    }
}

/// What the reference's `getParentName`/`getFile` walk can resolve a `var://`
/// name to, given this port's flat table (`Main.cpp:381-445`).
#[derive(Debug, PartialEq, Eq)]
enum VarShape {
    /// A file-shaped name: `key` is the exact table key the reference's leaf
    /// would be read from, `parent` the directory it lives in (empty = the
    /// root).
    File { key: String, parent: String },
    /// The reference's directory form: a path with a trailing slash, or the
    /// empty path (`var://./`) — an object there. `path` has no trailing
    /// slash; empty is the root.
    Directory { path: String },
    /// The walk cleared its base (`Main.cpp:403-420`): an empty component, so
    /// nothing resolves here at all.
    Blocked,
}

/// `getParentName`'s domain half (`Main.cpp:383-395`): the media is handed
/// `domain/path`, the domain must be `.`, and a name with no `/` is not a
/// path at all.
fn split_domain(media_name: &str) -> io::Result<&str> {
    match media_name.split_once('/') {
        None => Err(invalid_path_error(media_name)),
        Some((domain, _)) if domain != "." => Err(no_such_domain_error(domain)),
        Some((_, path)) => Ok(path),
    }
}

/// The path after the domain, classified the way the reference's walk would
/// classify it (`Main.cpp:397-422`).
fn shape_of(path: &str) -> VarShape {
    if path.is_empty() {
        // `var://./` addresses the root object itself.
        return VarShape::Directory {
            path: String::new(),
        };
    }
    if let Some(directory) = path.strip_suffix('/') {
        return if is_well_formed_path(directory) {
            VarShape::Directory {
                path: directory.to_string(),
            }
        } else {
            VarShape::Blocked
        };
    }
    if !is_well_formed_path(path) {
        return VarShape::Blocked;
    }
    match path.rsplit_once('/') {
        Some((parent, _)) => VarShape::File {
            key: path.to_string(),
            parent: parent.to_string(),
        },
        None => VarShape::File {
            key: path.to_string(),
            parent: String::new(),
        },
    }
}

/// A path spelling the reference's walk can descend: every component is
/// non-empty (`Main.cpp:403-406` clears the base on an empty one), so no
/// leading or trailing slash and no `//`.
fn is_well_formed_path(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.ends_with('/') && !path.contains("//")
}

/// Whether `path` is a directory in the table: the root always, and any other
/// path when some key is a file inside it — the reference's object with
/// members (`Main.cpp:412-420`).
fn is_directory(table: &StorageScriptTable, path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    let prefix = format!("{path}/");
    table
        .entries()
        .iter()
        .any(|(key, _)| key.starts_with(prefix.as_str()))
}

/// The direct files of `path`, in the order the reference's `GetLister` would
/// pass them on: the members that are octets, without a trailing slash
/// (`Main.cpp:259-263`). The mirror is a sorted map, so this list is sorted
/// where the reference walks dictionary order (module docs).
fn list_files(table: &StorageScriptTable, path: &str) -> Vec<String> {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}/")
    };
    table
        .entries()
        .into_iter()
        .filter_map(|(key, _)| {
            let name = key.strip_prefix(prefix.as_str())?;
            (!name.is_empty() && !name.contains('/')).then(|| name.to_string())
        })
        .collect()
}

/// `Open`'s failure message with the name filled in for `%1`
/// (`Main.cpp:345`).
fn open_error(name: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, OPEN_ERROR.replace("%1", name))
}

/// `getParentName`'s domain error (`Main.cpp:388`).
fn no_such_domain_error(domain: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        NO_SUCH_DOMAIN_ERROR.replace("%1", domain),
    )
}

/// `getParentName`'s path error (`Main.cpp:391`).
fn invalid_path_error(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        INVALID_PATH_ERROR.replace("%1", name),
    )
}

/// The port's write gap, in the reference's `cannot open memfile:%1` shape so
/// a script's catch sees the name it asked for plus the reason.
fn write_gap_error(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "cannot open memfile:{name} for writing: this port cannot store a `{MEDIA_NAME}://` \
             write back into the script table (the reference writes the octet into the variable \
             when the stream closes)",
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        io::Read,
        path::{Path, PathBuf},
        time::SystemTime,
    };

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths};

    use super::*;
    use crate::catalog;

    fn temp_root(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-varfile-{prefix}-{}-{unique}",
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
            .register_plugin(VarfilePlugin::new())
            .expect("register the varfile plugin");
        engine
    }

    /// Fills the script table and lets one host frame refresh the mirror, the
    /// way a game's startup script would.
    fn fill_table(engine: &mut KrkrEngine, script: &str) {
        engine.execute_script("fill.tjs", script).expect("script");
        engine.tick().expect("tick");
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
        data.as_bytes().expect("entry bytes").into_owned()
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

    /// The reference's whole script surface is the media itself
    /// (`Main.cpp:451-477` registers no class and no member), and its decoded
    /// member table is empty. The port adds exactly one global — the namespace
    /// root the seam must watch by name (divergence 1) — and it is a genuine
    /// TJS `Dictionary`, not a class or a member anywhere else.
    #[test]
    fn the_only_new_surface_is_the_watched_table_global() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let before = global_names(&engine);
        engine
            .register_plugin(VarfilePlugin::new())
            .expect("register");

        assert_eq!(
            global_names(&engine)
                .difference(&before)
                .cloned()
                .collect::<Vec<_>>(),
            vec![TABLE_GLOBAL.to_string()],
            "the plugin installs more than the one table global the seam requires",
        );
        let table = engine
            .tjs_runtime()
            .object_member(engine.tjs_runtime().global_handle(), TABLE_GLOBAL);
        let handle = table
            .object_handle()
            .unwrap_or_else(|| panic!("`{TABLE_GLOBAL}` is not an object: {table:?}"));
        assert!(
            engine.tjs_runtime().is_dictionary_instance(handle),
            "`{TABLE_GLOBAL}` is not a Dictionary instance",
        );
    }

    /// The plugin registers under the catalog's canonical name, installs
    /// exactly the `var` media (`GetName` → `BASENAME`, `Main.cpp:7`,
    /// `:306-308`), and its log separates the table it watches from the media
    /// it installs (with the reference's own open-failure text quoted).
    #[test]
    fn registers_under_the_catalog_name_and_logs_the_table_and_media() {
        let engine = project_engine(&temp_root("logs"));

        assert_eq!(catalog::canonical_name(NAME), Some(NAME));
        assert!(
            engine.host().linked_plugins().any(|linked| linked == NAME),
            "the plugin does not register the name the catalog resolves",
        );
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()],
            "the reference registers exactly the `var` media",
        );

        let logs = engine.host().logs();
        assert!(
            logs.iter()
                .any(|line| line.contains(TABLE_GLOBAL) && line.contains("Dictionary")),
            "no line describing the watched table: {logs:?}",
        );
        assert!(
            logs.iter().any(|line| line.contains(MEDIA_NAME)
                && line.contains("registered")
                && line.contains(OPEN_ERROR)),
            "no line describing the registered media: {logs:?}",
        );
    }

    /// The end-to-end shape the reference documents (`readme.txt`): a script
    /// fills the variable space, the engine refreshes the mirror at the next
    /// frame boundary, and a `var://./path` read serves the entry's bytes.
    /// Strings are the port's value type (divergence 3), so their UTF-8 bytes
    /// are what a file read returns — including for non-ASCII text and for an
    /// empty entry, which is still a file.
    #[test]
    fn a_script_written_key_resolves_through_the_table() {
        let mut engine = project_engine(&temp_root("resolve"));
        fill_table(
            &mut engine,
            r#"
            VarStorageMap["data/hello.bin"] = "hello, var://";
            VarStorageMap["data/日本語.txt"] = "こんにちは";
            VarStorageMap["empty.bin"] = "";
            "#,
        );

        assert_eq!(read(&engine, "var://./data/hello.bin"), b"hello, var://");
        assert_eq!(
            read(&engine, "var://./data/日本語.txt"),
            "こんにちは".as_bytes(),
        );
        assert_eq!(read(&engine, "var://./empty.bin"), Vec::<u8>::new());
        assert!(exists(&engine, "var://./data/hello.bin"));
        // A name the script never wrote is a miss, so the resolver falls
        // through to the built-in stack rather than failing the scheme.
        assert!(!exists(&engine, "var://./data/missing.bin"));
        assert!(
            engine
                .tjs_runtime()
                .host()
                .project_storage()
                .expect("project storage")
                .read_binary_storage("var://./data/missing.bin")
                .is_err()
        );
    }

    /// The script-thread refresh point: an entry written earlier in the *same*
    /// script block is already live for a `Storages.*` probe, because those
    /// natives refresh the watched dictionary at entry — the reference's
    /// "write the variables, then read through the scheme" usage.
    #[test]
    fn a_storages_probe_sees_an_entry_written_in_the_same_block() {
        let root = temp_root("same-block");
        let mut engine = project_engine(&root);

        let value = engine
            .execute_script(
                "same-block.tjs",
                r#"
                VarStorageMap["x.bin"] = "payload";
                VarStorageMap["dir/y.bin"] = "y";
                return (Storages.isExistentStorage("var://./x.bin") ? "file" : "missing") + "/" +
                    (Storages.isExistentDirectory("var://./dir/") ? "dir" : "nodir") + "/" +
                    (Storages.isExistentStorage("var://./dir/") ? "file" : "notfile");
                "#,
            )
            .expect("script");
        assert_eq!(value.to_tjs_string().expect("string"), "file/dir/notfile");

        // The engine-side read (no `Storages.*` call to refresh it) sees the
        // entry once the frame boundary has passed.
        engine.tick().expect("tick");
        assert_eq!(read(&engine, "var://./x.bin"), b"payload");
    }

    /// A missing entry behaves the way the reference's non-file leaf does:
    /// `CheckExistentStorage` false, `Open` — and so the media read — raising
    /// `cannot open memfile:%1` with the media-relative name. `void` and
    /// non-string entries land here too: `void` because the reference's leaf
    /// rule rejects it, an octet because the string-only mirror drops it
    /// (divergence 3 — the reference would serve that octet).
    #[test]
    fn missing_and_void_entries_are_misses_like_the_reference() {
        let mut engine = project_engine(&temp_root("missing"));
        fill_table(
            &mut engine,
            r#"
            VarStorageMap["void.bin"] = void;
            VarStorageMap["number.bin"] = 5;
            VarStorageMap["octet.bin"] = <% 01 02 03 %>;
            "#,
        );

        for name in ["void.bin", "number.bin", "octet.bin", "absent.bin"] {
            let storage_name = format!("var://./{name}");
            assert!(!exists(&engine, &storage_name), "`{name}` must be a miss");
            assert!(
                engine
                    .tjs_runtime()
                    .host()
                    .project_storage()
                    .expect("project storage")
                    .read_binary_storage(&storage_name)
                    .is_err(),
                "`{name}` must not read",
            );
        }

        // The media's own failure: the reference's text with the name it was
        // handed (`Main.cpp:345`), and a directory form is not a file either.
        let table = engine
            .tjs_runtime()
            .host()
            .storage_script_table(TABLE_GLOBAL)
            .expect("the plugin watches the table");
        let media = VarMedia::new(table);
        assert!(!media.exists("./void.bin"));
        assert!(!media.exists("./"));
        let error = media
            .open("./absent.bin")
            .map(|_| ())
            .expect_err("an unresolved name must fail");
        assert_eq!(error.to_string(), "cannot open memfile:./absent.bin");
        let error = media
            .open("./")
            .map(|_| ())
            .expect_err("the root is not a file");
        assert_eq!(error.to_string(), "cannot open memfile:./");
    }

    /// `getParentName`'s two errors are raised by `Open` before anything else
    /// (`Main.cpp:383-395`): a non-`.` domain is `no such domain:%1`, a name
    /// with no domain at all is `invalid path:%1`, and an empty component
    /// clears the walk's base, which `Open` reports as the open failure.
    #[test]
    fn the_domain_is_fixed_and_blocked_paths_fail_like_the_reference() {
        let engine = project_engine(&temp_root("domain"));
        let table = engine
            .tjs_runtime()
            .host()
            .storage_script_table(TABLE_GLOBAL)
            .expect("the plugin watches the table");
        let media = VarMedia::new(table);

        let error = media
            .open("proxy/x")
            .map(|_| ())
            .expect_err("a foreign domain must fail");
        assert_eq!(error.to_string(), "no such domain:proxy");
        assert!(!media.exists("proxy/x"));

        let error = media
            .open("x")
            .map(|_| ())
            .expect_err("a name without a domain must fail");
        assert_eq!(error.to_string(), "invalid path:x");
        assert!(!media.exists("x"));

        for blocked in [".//x", "./a//b", "./a//"] {
            let error = media
                .open(blocked)
                .map(|_| ())
                .expect_err("an empty component must fail");
            assert_eq!(
                error.to_string(),
                format!("cannot open memfile:{blocked}"),
                "`{blocked}` must fail as the reference's cleared base",
            );
            assert!(!media.exists(blocked));
        }
    }

    /// `GetListAt` (`Main.cpp:351-358`) lists a directory's direct *files* —
    /// object members are invisible to the reference's lister (`:259-263`) —
    /// and addresses the root as `var://./`. A name the table cannot serve as
    /// a directory is the trait's miss, and a bad domain is the reference's
    /// own error.
    #[test]
    fn the_listing_is_the_reference_file_only_scan() {
        let mut engine = project_engine(&temp_root("listing"));
        fill_table(
            &mut engine,
            r#"
            VarStorageMap["root.bin"] = "root";
            VarStorageMap["dir/a.bin"] = "a";
            VarStorageMap["dir/b.bin"] = "b";
            VarStorageMap["dir/sub/c.bin"] = "c";
            "#,
        );
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");

        assert_eq!(
            storage.list_directory("var://./").expect("root listing"),
            vec!["root.bin"],
        );
        assert_eq!(
            storage
                .list_directory("var://./dir/")
                .expect("directory listing"),
            vec!["a.bin", "b.bin"],
        );
        // Only direct files: `sub` is a directory, and the reference's lister
        // never reports one.
        assert_eq!(
            storage
                .list_directory("var://./dir/sub/")
                .expect("nested listing"),
            vec!["c.bin"],
        );
        assert!(storage.is_directory("var://./"));
        assert!(storage.is_directory("var://./dir/"));
        assert!(storage.is_directory("var://./dir/sub/"));
        assert!(!storage.is_directory("var://./nope/"));
        assert!(
            storage.list_directory("var://./nope/").is_err(),
            "a directory the table does not have is a miss",
        );
        assert!(
            storage.list_directory("var://./dir/a.bin").is_err(),
            "a file is not a directory",
        );

        // The media's own listing, including the domain error the engine's
        // probe path cannot surface (`exists` has no error channel).
        let table = engine
            .tjs_runtime()
            .host()
            .storage_script_table(TABLE_GLOBAL)
            .expect("the plugin watches the table");
        let media = VarMedia::new(table);
        assert_eq!(
            media.list("./dir/").expect("listing"),
            vec!["a.bin", "b.bin"]
        );
        assert_eq!(media.list("./").expect("root listing"), vec!["root.bin"]);
        let error = media.list("proxy/dir/").expect_err("a bad domain fails");
        assert_eq!(error.to_string(), "no such domain:proxy");
    }

    /// The mirror is refreshed on the script thread by the engine: an entry
    /// written in one script block is invisible until the next tick/step
    /// boundary, visible after it, and the table follows the global even when
    /// a script replaces it — with a non-object global clearing it.
    #[test]
    fn the_table_follows_the_dictionary_at_tick_boundaries() {
        let root = temp_root("refresh");
        let mut engine = project_engine(&root);

        engine
            .execute_script("map.tjs", r#"VarStorageMap["x.bin"] = "payload";"#)
            .expect("script");
        assert!(
            !exists(&engine, "var://./x.bin"),
            "an entry must not be visible before the next refresh",
        );

        engine.tick().expect("tick");
        assert!(exists(&engine, "var://./x.bin"));
        assert_eq!(read(&engine, "var://./x.bin"), b"payload");

        // Replacing the global follows the new object.
        engine
            .execute_script("replace.tjs", r#"VarStorageMap = new Dictionary();"#)
            .expect("script");
        engine.tick().expect("tick");
        assert!(!exists(&engine, "var://./x.bin"));

        // A non-object global clears the table rather than leaving a stale
        // entry that still resolves.
        engine
            .execute_script("gone.tjs", r#"VarStorageMap = 3;"#)
            .expect("script");
        engine.tick().expect("tick");
        assert!(!exists(&engine, "var://./x.bin"));
    }

    /// The media serves entries without opening a stream twice: `byte_len` is
    /// the entry's length and resolves (and fails) exactly like `open`.
    #[test]
    fn byte_len_reports_the_entry_length() {
        let mut engine = project_engine(&temp_root("len"));
        fill_table(&mut engine, r#"VarStorageMap["len.bin"] = "1234567";"#);
        let table = engine
            .tjs_runtime()
            .host()
            .storage_script_table(TABLE_GLOBAL)
            .expect("the plugin watches the table");
        let media = VarMedia::new(table);

        assert_eq!(media.byte_len("./len.bin").expect("length"), Some(7));
        let error = media
            .byte_len("./other.bin")
            .expect_err("an unresolved name has no length");
        assert_eq!(error.to_string(), "cannot open memfile:./other.bin");
    }

    /// Writes through the scheme are the port's deliberate gap: a name the
    /// reference's `Open` would reject fails with the reference's message, and
    /// a name it would have accepted is refused with the reason instead of
    /// silently dropping the bytes (divergence 4).
    #[test]
    fn writes_are_refused_with_the_port_gap() {
        let mut engine = project_engine(&temp_root("writes"));
        fill_table(&mut engine, r#"VarStorageMap["dir/x.bin"] = "old";"#);
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");

        let error = storage
            .write_binary_storage("var://./dir/x.bin", "", b"payload")
            .expect_err("the reference's update cannot round-trip");
        assert!(
            error
                .to_string()
                .contains("cannot open memfile:./dir/x.bin")
                && error.to_string().contains("writing"),
            "unexpected error: {error}",
        );
        // The entry is untouched.
        assert_eq!(read(&engine, "var://./dir/x.bin"), b"old");

        // A directory that exists accepts a new name in the reference; here
        // that is the gap, and a directory that does not exist is the
        // reference's own open failure.
        let error = storage
            .write_binary_storage("var://./dir/new.bin", "", b"payload")
            .expect_err("write-back is not implemented");
        assert!(error.to_string().contains("writing"), "unexpected: {error}");
        let error = storage
            .write_binary_storage("var://./nope/new.bin", "", b"payload")
            .expect_err("the parent directory does not exist");
        assert!(
            error
                .to_string()
                .ends_with("cannot open memfile:./nope/new.bin"),
            "unexpected error: {error}",
        );
        let error = storage
            .write_binary_storage("var://./dir/", "", b"payload")
            .expect_err("a directory is not a file");
        assert!(
            error.to_string().ends_with("cannot open memfile:./dir/"),
            "unexpected error: {error}",
        );
    }

    /// `Plugins.link` installs the module a second time (the engine calls
    /// `register` again) and `Plugins.unlink` runs the reference's
    /// post-unregistration callback (`Main.cpp:467-474`); linking it back must
    /// register the media again.
    #[test]
    fn linking_again_keeps_one_media_and_unlinking_unregisters_it() {
        let root = temp_root("link");
        let mut engine = project_engine(&root);
        fill_table(&mut engine, r#"VarStorageMap["x.bin"] = "payload";"#);
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );

        engine
            .execute_expression("inline.tjs", "Plugins.link(\"varfile.dll\")")
            .expect("link the module again");
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );
        assert_eq!(read(&engine, "var://./x.bin"), b"payload");

        engine
            .execute_expression("inline.tjs", "Plugins.unlink(\"varfile.dll\")")
            .expect("unlink the module");
        assert!(engine.host().storage_media_names().is_empty());
        assert!(
            engine
                .tjs_runtime()
                .host()
                .project_storage()
                .expect("project storage")
                .read_binary_storage("var://./x.bin")
                .is_err(),
            "an unregistered scheme falls through to the built-in stack",
        );

        engine
            .execute_expression("inline.tjs", "Plugins.link(\"varfile.dll\")")
            .expect("link the module back");
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );
    }

    /// A host without a project storage installs the plugin without a media
    /// and says so, instead of failing the boot (see `register`).
    #[test]
    fn a_host_without_storage_stays_installable() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(VarfilePlugin::new())
            .expect("install into a script-only host");
        assert!(engine.host().storage_media_names().is_empty());
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("varfile") && line.contains("not registered")),
            "the failed registration must be visible in the log",
        );
    }

    /// The table and the media carry no TJS handle, which is the whole point
    /// of the mirror: a resolution on the resource worker reads the same table
    /// the script thread refreshes.
    #[test]
    fn the_media_and_its_table_are_usable_from_another_thread() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StorageScriptTable>();
        assert_send_sync::<VarMedia>();

        let root = temp_root("threads");
        let mut engine = project_engine(&root);
        fill_table(&mut engine, r#"VarStorageMap["x.bin"] = "payload";"#);

        let table = engine
            .tjs_runtime()
            .host()
            .storage_script_table(TABLE_GLOBAL)
            .expect("the plugin watches the table");
        let worker = std::thread::spawn(move || {
            (
                VarMedia::new(Arc::clone(&table)).exists("./x.bin"),
                table.get("x.bin"),
            )
        });
        assert_eq!(
            worker.join().expect("worker"),
            (true, Some("payload".to_string())),
        );
    }

    /// The media without the engine: the lookup is the table's, a served file
    /// streams its bytes, and both failure paths carry the reference's texts.
    #[test]
    fn the_media_streams_the_entry_and_reports_the_reference_errors() {
        let root = temp_root("unit");
        let mut engine = project_engine(&root);
        fill_table(
            &mut engine,
            r#"
            VarStorageMap["data/x.bin"] = "payload";
            VarStorageMap["data/y.bin"] = "yy";
            "#,
        );
        let table = engine
            .tjs_runtime()
            .host()
            .storage_script_table(TABLE_GLOBAL)
            .expect("the plugin watches the table");
        let media = VarMedia::new(table);

        assert_eq!(media.media_name(), MEDIA_NAME);
        assert!(media.exists("./data/x.bin"));
        let mut stream = media.open("./data/x.bin").expect("open the entry");
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).expect("read the entry");
        assert_eq!(bytes, b"payload");

        let error = media
            .open("./data/missing.bin")
            .map(|_| ())
            .expect_err("a missing entry must fail");
        assert_eq!(error.to_string(), "cannot open memfile:./data/missing.bin");
    }
}
