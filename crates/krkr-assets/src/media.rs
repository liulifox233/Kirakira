//! Storage media providers (`iTVPStorageMedia`).
//!
//! KRKR plugins extend the storage namespace by registering a *media* — a
//! handler for one URI scheme. `psbfile.dll` registers `psb`, `lzfs.dll`
//! registers `lzfs`, `yuzuex.dll`/`packinone.dll` register `proxy`,
//! `krkrsteam.dll` registers `steam`, `minizip` registers `zip` and
//! `varfile` registers `var`. A game then reads
//! `psb://container.psb/entry`, `lzfs://./data/file.bin`, `steam://./save.dat`
//! and so on through the ordinary `TVPCreateStream`/`Storages.*` entry points.
//!
//! The rules below are pinned from krkrz `src/core/base/StorageIntf.cpp` and
//! `src/core/base/win32/StorageImpl.cpp`:
//!
//! * A storage name is `media://domain/path`. The media name is the leading
//!   run of ASCII letters before `:` (`StorageIntf.cpp:299-318`) and is
//!   lowercased before it is looked up (`StorageIntf.cpp:366-374`). The media
//!   itself receives everything after `://` — `tMediaRecord::GetDomainAndPath`
//!   skips `strlen(media) + 3` characters (`StorageIntf.cpp:164-168`), so
//!   `psb://container.psb/entry` hands `container.psb/entry` to the `psb`
//!   media, which splits the container at the first `/`
//!   (`minizip/storage.cpp:521-536` splits `zip://domain/name` the same way).
//! * Backslashes are unified to `/` before the media split
//!   (`StorageIntf.cpp:271-277`); per-media domain/path normalization belongs
//!   to the media (`iTVPStorageMedia::NormalizeDomainName`/`NormalizePathName`).
//! * Registration is a media-name → handler table; a duplicate name throws
//!   `TVPMediaNameHadAlreadyBeenRegistered` (`StorageIntf.cpp:224-236`) and
//!   the `file` media is already registered by the manager's constructor
//!   (`StorageIntf.cpp:200-205`), so no plugin may claim it.
//! * An in-archive name is split at `>` *before* any media dispatch
//!   (`StorageIntf.cpp:804-827`): media names never carry `>`.
//! * `TVPGetPlacedPath`/`TVPIsExistentStorage` probe the media first and only
//!   then the auto-path table (`StorageIntf.cpp:1153-1223`); `TVPCreateStream`
//!   opens through the media's `Open` when the placed name has no `>`
//!   (`StorageIntf.cpp:1279-1289`).
//!
//! Divergences we keep on purpose:
//!
//! * The reference throws `TVPUnsupportedMediaName` for a scheme that is not
//!   registered (`StorageIntf.cpp:211-222`). Before this registry existed a
//!   name such as `psb://x` simply resolved through the built-in stack, and
//!   scripts use `Storages.isExistentStorage` as a predicate, so an
//!   unregistered scheme keeps falling through instead of throwing.
//! * The reference rejects `media://name` (a domain with no path) as
//!   `TVPInvalidPathName` (`StorageIntf.cpp:341-343`); we hand `name` to the
//!   registered media, which owns its own namespace validation.
//! * The reference searches only the auto-path table after a media miss; we
//!   fall back to the whole built-in resolver (filesystem layers, XP3, memory
//!   overlay, catalogue, auto paths), which is that stack plus more.
//!
//! The provider trait and the name helpers live in `krkr_core::media` — a
//! plugin implements them against that path, through
//! `krkr_engine::plugin_api` — and are re-exported here so this module keeps
//! the parsing rules and the divergences they explain.
//!
//! What a provider can rely on from the engine:
//!
//! * A media that *wraps* the built-in stack — `lzfs` resolves the name it is
//!   handed through the ordinary search path — gets a weak handle to the
//!   project storage in `StorageMediaProvider::attach_storage`, which the
//!   engine fills in before the media is inserted. It is `Weak` because the
//!   registry lives inside the storage, so a strong handle would be a
//!   reference cycle.
//!
//! One thing a provider cannot rely on yet:
//!
//! * Media *auto paths* (`Storages.addAutoPath("psb://container.psb/")`) do not
//!   reach a provider: the auto-path machinery folds `media://` into `media:/`
//!   before the provider could see it. The reference discovers those entries by
//!   listing each auto path instead (`TVPRebuildAutoPathTable`,
//!   `StorageIntf.cpp:1035-1144`).

pub use krkr_core::media::{
    FILE_MEDIA_NAME, StorageMediaProvider, is_valid_media_name, split_media_name,
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{self, Read};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use krkr_core::{ResourceStream, StoragePort};

    use super::*;
    use crate::storage::ProjectStorage;

    struct FakeMedia {
        name: &'static str,
        files: BTreeMap<String, Vec<u8>>,
        dirs: BTreeMap<String, Vec<String>>,
        /// Set to fail `open` with this message after `exists` said yes.
        open_error: Option<&'static str>,
    }

    impl FakeMedia {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                files: BTreeMap::new(),
                dirs: BTreeMap::new(),
                open_error: None,
            }
        }

        fn with_file(mut self, path: &str, bytes: &[u8]) -> Self {
            self.files.insert(path.to_string(), bytes.to_vec());
            self
        }

        fn with_dir(mut self, path: &str, children: &[&str]) -> Self {
            self.dirs.insert(
                path.to_string(),
                children.iter().map(|child| child.to_string()).collect(),
            );
            self
        }

        /// Serves `path` as existing but fails every `open` with `message`,
        /// the way `SteamStorage::Open` reports a cloud file it cannot read
        /// (krkr2 `plugins/win32/steam/Storages.cpp:394`).
        fn failing(mut self, path: &str, message: &'static str) -> Self {
            self.files.insert(path.to_string(), Vec::new());
            self.open_error = Some(message);
            self
        }
    }

    impl StorageMediaProvider for FakeMedia {
        fn media_name(&self) -> &str {
            self.name
        }

        fn exists(&self, name: &str) -> bool {
            self.files.contains_key(name) || self.dirs.contains_key(name)
        }

        fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
            if let Some(message) = self.open_error {
                return Err(io::Error::other(message));
            }
            let bytes = self.files.get(name).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no entry `{name}`"))
            })?;
            Ok(Box::new(io::Cursor::new(bytes.clone())))
        }

        fn list(&self, name: &str) -> io::Result<Vec<String>> {
            let children = self.dirs.get(name).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no directory `{name}` in fake media"),
                )
            })?;
            Ok(children.clone())
        }
    }

    fn storage_with(provider: FakeMedia) -> ProjectStorage {
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        storage
            .register_media(Arc::new(provider))
            .expect("register media");
        storage
    }

    /// A media whose contents appear only after the first probe, the way a
    /// Steam cloud file uploaded mid-session does.
    struct LateMedia {
        files: std::sync::Mutex<BTreeMap<String, Vec<u8>>>,
    }

    impl LateMedia {
        fn new() -> Self {
            Self {
                files: std::sync::Mutex::new(BTreeMap::new()),
            }
        }

        fn publish(&self, path: &str, bytes: &[u8]) {
            self.files
                .lock()
                .expect("late media lock")
                .insert(path.to_string(), bytes.to_vec());
        }
    }

    impl StorageMediaProvider for LateMedia {
        fn media_name(&self) -> &str {
            "steam"
        }

        fn exists(&self, name: &str) -> bool {
            self.files
                .lock()
                .expect("late media lock")
                .contains_key(name)
        }

        fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
            let files = self.files.lock().expect("late media lock");
            let bytes = files.get(name).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no cloud file `{name}`"))
            })?;
            Ok(Box::new(io::Cursor::new(bytes.clone())))
        }
    }

    /// A media that owns writes into its scheme, the way `var`/`proxy` do. The
    /// read-only behaviour (`minizip`'s) is the trait default and is covered by
    /// the `FakeMedia` fixture, which does not override `write`.
    struct WritableMedia {
        files: std::sync::Mutex<BTreeMap<String, Vec<u8>>>,
        writes: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl WritableMedia {
        fn new() -> Self {
            Self {
                files: std::sync::Mutex::new(BTreeMap::new()),
                writes: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn with_file(self, path: &str, bytes: &[u8]) -> Self {
            self.files
                .lock()
                .expect("write media lock")
                .insert(path.to_string(), bytes.to_vec());
            self
        }

        fn written(&self, path: &str) -> Option<Vec<u8>> {
            self.files
                .lock()
                .expect("write media lock")
                .get(path)
                .cloned()
        }

        /// `(name space, mode)` of every write the engine dispatched here.
        fn writes(&self) -> Vec<(String, String)> {
            self.writes.lock().expect("write media lock").clone()
        }
    }

    impl StorageMediaProvider for WritableMedia {
        fn media_name(&self) -> &str {
            "psb"
        }

        fn exists(&self, name: &str) -> bool {
            self.files
                .lock()
                .expect("write media lock")
                .contains_key(name)
        }

        fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
            let files = self.files.lock().expect("write media lock");
            let bytes = files.get(name).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no entry `{name}`"))
            })?;
            Ok(Box::new(io::Cursor::new(bytes.clone())))
        }

        fn write(&self, name: &str, mode: &str, bytes: &[u8]) -> io::Result<()> {
            self.files
                .lock()
                .expect("write media lock")
                .insert(name.to_string(), bytes.to_vec());
            self.writes
                .lock()
                .expect("write media lock")
                .push((name.to_string(), mode.to_string()));
            Ok(())
        }
    }

    fn temp_root(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "Kirakira-assets-media-{prefix}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn splits_media_names_like_the_reference() {
        assert_eq!(
            split_media_name("psb://container.psb/inner"),
            Some(("psb", "container.psb/inner"))
        );
        assert_eq!(
            split_media_name("steam://./save/slot0.dat"),
            Some(("steam", "./save/slot0.dat"))
        );
        // Case is the lookup's business; the reference lowercases the media
        // before consulting the table (`StorageIntf.cpp:366-374`).
        assert_eq!(split_media_name("PSB://c/x"), Some(("PSB", "c/x")));
        assert_eq!(split_media_name("data/file.txt"), None);
        assert_eq!(split_media_name("psb2://c/x"), None);
        assert_eq!(split_media_name("psb://"), None);
        assert_eq!(split_media_name("://c/x"), None);
        assert!(is_valid_media_name("lzfs"));
        assert!(!is_valid_media_name("lz4s"));
        assert!(!is_valid_media_name(""));
    }

    #[test]
    fn registered_media_serves_bytes_through_the_storage_api() {
        let storage = storage_with(FakeMedia::new("psb").with_file("container.psb/inner", b"PSB"));

        assert!(storage.storage_exists("psb://container.psb/inner"));
        assert!(storage.storage_exists_exact("psb://container.psb/inner"));
        assert_eq!(
            storage
                .read_binary_vec("psb://container.psb/inner")
                .expect("media bytes"),
            b"PSB"
        );
        assert_eq!(
            storage
                .resolved_storage_name("psb://container.psb/inner")
                .as_deref(),
            Some("psb://container.psb/inner")
        );
        // The reference lowercases the media name before it consults the
        // table (`StorageIntf.cpp:366-374`), so `PSB://` reaches the provider.
        let mut stream = storage
            .open_storage("PSB://container.psb/inner")
            .expect("media stream");
        let mut text = String::new();
        stream.read_to_string(&mut text).expect("read stream");
        assert_eq!(text, "PSB");
        assert_eq!(
            storage
                .storage_byte_len("psb://container.psb/inner")
                .expect("media length"),
            Some(3)
        );
        // `GetLocallyAccessibleName` is `""` for every media the dossiers
        // cover; a media name therefore has no OS path.
        assert_eq!(storage.placed_path("psb://container.psb/inner"), None);
    }

    #[test]
    fn media_is_consulted_before_filesystem_memory_and_catalogue() {
        // The built-in stack finds a media-qualified name only after
        // `clean_relative_path` collapses `media://` into `media:/`, so a file
        // laid out that way is exactly what the media has to shadow. (XP3 is
        // ordered by the same single dispatch point; it is not exercised here
        // because the repository has no XP3 fixture and `krkr-xp3` keeps its
        // archive builder test-private.)
        let root = temp_root("order");
        fs::create_dir_all(root.join("lzfs:/data")).expect("create collapsed layer");
        fs::write(root.join("lzfs:/data/file.bin"), b"raw file").expect("write file");

        let filesystem = ProjectStorage::for_root(&root).expect("storage");
        assert_eq!(
            filesystem
                .read_binary_vec("lzfs://./data/file.bin")
                .expect("built-in filesystem fallback"),
            b"raw file"
        );

        let media = ProjectStorage::for_root(&root).expect("storage");
        media
            .register_media(Arc::new(
                FakeMedia::new("lzfs").with_file("./data/file.bin", b"lz4"),
            ))
            .expect("register media");
        assert_eq!(
            media
                .read_binary_vec("lzfs://./data/file.bin")
                .expect("media first"),
            b"lz4"
        );

        // A memory overlay and the deferred catalogue are layers of the same
        // built-in stack; a registered media shadows them too.
        let memory = storage_with(FakeMedia::new("lzfs").with_file("./data/file.bin", b"lz4"));
        memory.insert_memory("lzfs://./data/file.bin", b"overlay".to_vec());
        memory.add_catalog_paths(["lzfs://./data/file.bin"]);
        assert_eq!(
            memory
                .read_binary_vec("lzfs://./data/file.bin")
                .expect("media first"),
            b"lz4"
        );
        // Without the provider the same name still resolves as before.
        let fallback = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        fallback.insert_memory("lzfs://./data/file.bin", b"overlay".to_vec());
        assert_eq!(
            fallback
                .read_binary_vec("lzfs://./data/file.bin")
                .expect("built-in fallback"),
            b"overlay"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn media_miss_falls_through_to_the_built_in_stack() {
        // A media probe that fails hands the name on to the built-in stack
        // (`TVPGetPlacedPath` then searches the auto-path table,
        // `StorageIntf.cpp:1179-1196`). Our stack is the filesystem layers,
        // XP3, the memory overlay, the catalogue and the auto paths, and it
        // sees the name in the same spelling the pre-registry engine used —
        // which is the layer the `psb` plugin populated before this registry
        // existed.
        let storage = storage_with(FakeMedia::new("lzfs"));
        storage.insert_memory("lzfs://./data/file.bin", b"raw".to_vec());
        assert_eq!(
            storage
                .read_binary_vec("lzfs://./data/file.bin")
                .expect("built-in fallback"),
            b"raw"
        );
        assert!(storage.storage_exists("lzfs://./data/file.bin"));
        // The fall-through is not a licence to invent a match: a name the
        // built-in stack cannot serve is still a miss.
        assert!(!storage.storage_exists("lzfs://./data/other.bin"));
    }

    #[test]
    fn unregistered_media_keeps_the_pre_registry_behaviour() {
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());

        assert!(!storage.storage_exists("psb://container.psb/inner"));
        assert!(!storage.storage_exists_exact("psb://container.psb/inner"));
        let error = storage
            .read_binary_vec("psb://container.psb/inner")
            .expect_err("unregistered media is not resolved by a provider");
        assert!(error.to_string().contains("not found"), "{error}");
        assert_eq!(
            storage
                .storage_byte_len("psb://container.psb/inner")
                .expect_err("byte length of a missing storage")
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(!storage.storage_exists("a://b/c"));

        // The pre-registry engine reached `psb://` names that a plugin had
        // mounted into the memory overlay; that path must survive.
        storage.insert_memory("psb://container.psb/inner", b"mounted".to_vec());
        assert_eq!(
            storage
                .read_binary_vec("psb://container.psb/inner")
                .expect("mounted overlay"),
            b"mounted"
        );
        assert!(storage.storage_exists("psb://container.psb/inner"));

        // Unregistering restores that behaviour for a media that *was* served.
        let registered =
            storage_with(FakeMedia::new("psb").with_file("container.psb/inner", b"PSB"));
        assert!(registered.unregister_media("PSB"));
        assert!(!registered.unregister_media("psb"));
        assert!(!registered.storage_exists("psb://container.psb/inner"));
        assert!(
            registered
                .read_binary_vec("psb://container.psb/inner")
                .is_err()
        );
    }

    #[test]
    fn media_directory_listing_uses_fstat_spelling() {
        let storage = storage_with(
            FakeMedia::new("psb")
                .with_dir("container.psb/", &["inner.bin", "sub/"])
                .with_dir("container.psb/sub/", &["leaf.bin"])
                .with_file("container.psb/inner.bin", b"inner"),
        );

        assert!(storage.is_directory("psb://container.psb/"));
        assert_eq!(
            storage
                .list_directory("psb://container.psb/")
                .expect("media listing"),
            vec!["inner.bin".to_string(), "sub/".to_string()]
        );
        assert_eq!(
            storage
                .list_directory("psb://container.psb/sub/")
                .expect("nested media listing"),
            vec!["leaf.bin".to_string()]
        );
        // A media that does not serve the directory hands the name back to the
        // built-in stack, which then reports it as missing.
        let error = storage
            .list_directory("psb://container.psb/other/")
            .expect_err("unserved directory");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!storage.is_directory("psb://container.psb/other/"));

        // A media without listing support is not a directory at all.
        let unlisted = storage_with(FakeMedia::new("lzfs").with_file("./a.bin", b"raw"));
        assert!(!unlisted.is_directory("lzfs://./a.bin"));
    }

    #[test]
    fn provider_errors_surface_instead_of_reading_as_a_miss() {
        let storage = storage_with(
            FakeMedia::new("steam").failing("./slot0.dat", "cannot open steamfile: ./slot0.dat"),
        );

        // `CheckExistentStorage` said yes, so the media owns the name; its
        // `Open` failure must reach the caller verbatim
        // (krkr2 `plugins/win32/steam/Storages.cpp:375-396`).
        assert!(storage.storage_exists("steam://./slot0.dat"));
        let error = storage
            .read_binary_vec("steam://./slot0.dat")
            .expect_err("provider failure");
        assert_eq!(error.message, "cannot open steamfile: ./slot0.dat");
        let file_error = storage
            .open_storage("steam://./slot0.dat")
            .err()
            .expect("provider failure");
        assert_eq!(file_error.kind(), io::ErrorKind::Other);
        assert_eq!(file_error.to_string(), "cannot open steamfile: ./slot0.dat");
        // The name still resolves through the media; the failure is not a
        // silent fall-through to the built-in stack.
        assert_eq!(
            storage
                .resolved_storage_name("steam://./slot0.dat")
                .as_deref(),
            Some("steam://./slot0.dat")
        );
    }

    #[test]
    fn io_callers_see_the_provider_error_and_plain_misses_stay_not_found() {
        let missing = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        let error = missing
            .open_storage("steam://./slot0.dat")
            .err()
            .expect("unresolved media name");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(error.to_string(), "storage `steam://./slot0.dat` not found");

        // A provider that *is* there but fails keeps its own message: the
        // engine's io-facing storage port must not downgrade it to a miss.
        let failing =
            storage_with(FakeMedia::new("proxy").failing("./mapped.bin", "invalid path:%1"));
        let error = failing
            .data("proxy://./mapped.bin")
            .err()
            .expect("provider failure through the storage port");
        assert!(error.to_string().contains("invalid path:%1"), "{error}");
    }

    #[test]
    fn registration_rejects_duplicates_invalid_names_and_file() {
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        storage
            .register_media(Arc::new(FakeMedia::new("psb")))
            .expect("first registration");
        let duplicate = storage
            .register_media(Arc::new(FakeMedia::new("PSB")))
            .expect_err("duplicate media name");
        assert_eq!(duplicate.kind(), io::ErrorKind::AlreadyExists);
        let invalid = storage
            .register_media(Arc::new(FakeMedia::new("psb2")))
            .expect_err("media names are ASCII letters");
        assert_eq!(invalid.kind(), io::ErrorKind::InvalidInput);
        let reserved = storage
            .register_media(Arc::new(FakeMedia::new("file")))
            .expect_err("the built-in filesystem media is taken");
        assert_eq!(reserved.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(storage.media_names(), vec!["psb".to_string()]);
    }

    #[test]
    fn registering_the_same_provider_again_is_a_no_op() {
        // The engine registers a boot plugin's media from
        // `KrkrEngine::register_plugin` and again from the first
        // `Plugins.link` (`krkr-engine/src/native/plugins.rs`), handing the same
        // `Arc` both times. The second call is intentional and must not read as
        // `TVPMediaNameHadAlreadyBeenRegistered` (`StorageIntf.cpp:224-236`).
        let media: Arc<dyn StorageMediaProvider> =
            Arc::new(FakeMedia::new("psb").with_file("container.psb/inner", b"PSB"));
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        storage
            .register_media(Arc::clone(&media))
            .expect("first registration");
        storage
            .register_media(Arc::clone(&media))
            .expect("the same provider again");
        assert_eq!(storage.media_names(), vec!["psb".to_string()]);
        // A *different* provider under a name already taken still fails, and
        // the no-op registration keeps the provider that was there.
        let duplicate = storage
            .register_media(Arc::new(
                FakeMedia::new("psb").with_file("container.psb/inner", b"other"),
            ))
            .expect_err("another provider under a taken name");
        assert_eq!(duplicate.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            storage
                .read_binary_vec("psb://container.psb/inner")
                .expect("original media"),
            b"PSB"
        );
    }

    #[test]
    fn the_concrete_storage_delegates_the_port_media_methods() {
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        let port: &dyn krkr_core::ProjectStoragePort = &storage;
        let media: Arc<dyn StorageMediaProvider> =
            Arc::new(FakeMedia::new("lzfs").with_file("./a.bin", b"lz4"));

        port.register_storage_media(Arc::clone(&media))
            .expect("port registration");
        assert_eq!(port.storage_media_names(), vec!["lzfs".to_string()]);
        assert!(port.storage_exists("lzfs://./a.bin"));
        assert_eq!(
            port.read_binary_storage("lzfs://./a.bin")
                .expect("port read")
                .as_bytes()
                .expect("bytes")
                .as_ref(),
            b"lz4"
        );
        // The defaulted port method that a backend without a registry inherits
        // is not what `ProjectStorage` answers with: unregistration reports
        // whether a provider was there.
        assert!(port.unregister_storage_media("LZFS"));
        assert!(port.storage_media_names().is_empty());
        assert!(!port.unregister_storage_media("lzfs"));
    }

    #[test]
    fn a_media_receives_writes_into_its_scheme() {
        let media = Arc::new(WritableMedia::new().with_file("container.psb/entry", b"old"));
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        storage
            .register_media(Arc::clone(&media) as Arc<dyn StorageMediaProvider>)
            .expect("register media");

        storage
            .write_binary_storage("psb://container.psb/entry", "", b"new")
            .expect("media write");
        assert_eq!(
            media.writes(),
            vec![("container.psb/entry".to_string(), String::new())]
        );
        assert_eq!(
            storage
                .read_binary_vec("psb://container.psb/entry")
                .expect("media read"),
            b"new"
        );

        // The media sees its own name space: the scheme is stripped, the media
        // name lowercased and `\` unified before dispatch
        // (`StorageIntf.cpp:271-277`, `:164-168`). The write-mode string is the
        // engine's, handed through unchanged.
        storage
            .write_binary_storage("PSB://container.psb\\other", "o4", b"xy")
            .expect("offset write");
        assert_eq!(
            media.written("container.psb/other").as_deref(),
            Some(&b"xy"[..])
        );
        assert_eq!(media.writes().last().expect("last write").1, "o4");

        // Writes bypass the existence search (`StorageIntf.cpp:1236-1244`), so
        // a media is asked to create an entry that does not exist yet.
        storage
            .write_binary_storage("psb://container.psb/created", "", b"created")
            .expect("creating write");
        assert_eq!(
            storage
                .read_binary_vec("psb://container.psb/created")
                .expect("read back"),
            b"created"
        );
    }

    #[test]
    fn a_read_only_media_owns_writes_into_its_scheme() {
        let storage = storage_with(FakeMedia::new("psb").with_file("container.psb/entry", b"PSB"));

        // `FakeMedia` does not override `write`, so the trait's read-only
        // default answers — minizip's behaviour (`storage.cpp:439-457`).
        let error = storage
            .write_binary_storage("psb://container.psb/entry", "", b"x")
            .expect_err("read-only media");
        assert!(error.to_string().contains("read-only"), "{error}");
        assert_eq!(
            storage
                .read_binary_vec("psb://container.psb/entry")
                .expect("media read"),
            b"PSB"
        );

        // An unregistered scheme keeps the pre-registry write path, which for a
        // storage without a filesystem root is the memory overlay.
        storage
            .write_binary_storage("zip://./data.bin", "", b"zip")
            .expect("memory write");
        assert_eq!(
            storage
                .read_binary_vec("zip://./data.bin")
                .expect("memory read"),
            b"zip"
        );
    }

    #[test]
    fn registered_media_is_visible_to_every_clone_and_thread() {
        let storage = storage_with(FakeMedia::new("proxy").with_file("./a.bin", b"mapped"));
        let shared = storage.clone();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let clone = shared.clone();
                scope.spawn(move || {
                    for _ in 0..32 {
                        assert_eq!(
                            clone
                                .read_binary_vec("proxy://./a.bin")
                                .expect("media read from worker thread"),
                            b"mapped"
                        );
                    }
                    // Registering from a worker is legal; the second racer
                    // simply sees `TVPMediaNameHadAlreadyBeenRegistered`.
                    if let Err(error) = clone.register_media(Arc::new(
                        FakeMedia::new("steam").with_file("./cloud.dat", b"cloud"),
                    )) {
                        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
                    }
                });
            }
        });
        // Every handle shares one registry and one provider table.
        assert_eq!(
            storage
                .read_binary_vec("steam://./cloud.dat")
                .expect("cross-thread media"),
            b"cloud"
        );
        assert!(storage.media_names().contains(&"steam".to_string()));
    }

    #[test]
    fn a_live_media_is_probed_again_after_a_miss() {
        // A filesystem miss is worth remembering; a media miss is not, because
        // the media can gain entries while the engine runs (`steam` uploads a
        // cloud file, `proxy` remaps a path).
        let media = Arc::new(LateMedia::new());
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        storage
            .register_media(Arc::clone(&media) as Arc<dyn StorageMediaProvider>)
            .expect("register media");

        assert!(!storage.storage_exists("steam://./slot0.dat"));
        assert!(!storage.storage_exists_exact("steam://./slot0.dat"));
        assert!(storage.read_binary_vec("steam://./slot0.dat").is_err());

        media.publish("./slot0.dat", b"cloud");
        assert!(storage.storage_exists("steam://./slot0.dat"));
        assert_eq!(
            storage
                .read_binary_vec("steam://./slot0.dat")
                .expect("late media"),
            b"cloud"
        );
    }
}
