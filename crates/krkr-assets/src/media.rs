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
//! Two things a provider cannot rely on yet, so the engine-side follow-up
//! knows what to add:
//!
//! * A media that *wraps* the built-in stack — `lzfs` resolves the name it is
//!   handed through the ordinary search path — has to call back into
//!   `ProjectStorage`. It must not hold a strong `ProjectStorage` to do that:
//!   the registry lives inside the storage, so that is a reference cycle. The
//!   engine should give such a provider a weak handle, or resolve through a
//!   callback, when it registers it.
//! * Media *auto paths* (`Storages.addAutoPath("psb://container.psb/")`) do not
//!   reach a provider: the auto-path machinery folds `media://` into `media:/`
//!   before the provider could see it. The reference discovers those entries by
//!   listing each auto path instead (`TVPRebuildAutoPathTable`,
//!   `StorageIntf.cpp:1035-1144`).

use std::io::{self, Read, Seek, SeekFrom};

use krkr_core::{ResourceData, ResourceStream};

/// The media name the reference has already registered for the built-in
/// filesystem resolver (`StorageIntf.cpp:200-205`).
pub const FILE_MEDIA_NAME: &str = "file";

/// Whether `name` can be registered as a media name. The reference recognizes
/// a media name as the run of ASCII letters before `:`
/// (`StorageIntf.cpp:299-318`), so anything else can never be addressed by a
/// storage name.
pub fn is_valid_media_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphabetic())
}

/// Splits `media://domain/path` into the media name and the name space the
/// media owns (`domain/path`). Returns `None` for names that are not
/// media-qualified, so an ordinary relative path always stays on the built-in
/// resolver.
///
/// A bare `media://` is *not* a media name here: the reference rejects it as
/// `TVPInvalidPathName` (`StorageIntf.cpp:341-343`), and inventing a new error
/// for a name the pre-registry engine accepted as an ordinary path would be a
/// regression.
pub fn split_media_name(name: &str) -> Option<(&str, &str)> {
    let (media_name, media_path) = name.split_once("://")?;
    if !is_valid_media_name(media_name) || media_path.is_empty() {
        return None;
    }
    Some((media_name, media_path))
}

/// A registered storage media, mirroring the operations the engine uses from
/// `iTVPStorageMedia` (`StorageIntf.h:107-143`).
///
/// Implementations are shared between threads: the engine resolves storage
/// from the script thread, from resource workers and from media threads, so
/// `Send + Sync` is required and interior mutability must be the provider's
/// own concern (the reference uses critical sections the same way).
///
/// `name` is always the media's own name space — everything after
/// `media://`, with `\` already unified to `/`. A provider that mounts a
/// container the way `psb`/`zip`/`var` do splits its container or domain at the
/// first `/`; `lzfs`/`steam` treat the whole string as one path.
pub trait StorageMediaProvider: Send + Sync {
    /// The media name, e.g. `psb`, `lzfs`, `proxy`, `steam`
    /// (`iTVPStorageMedia::GetName`, `StorageIntf.h:113`).
    fn media_name(&self) -> &str;

    /// `iTVPStorageMedia::CheckExistentStorage` (`StorageIntf.h:128`): whether
    /// this media serves `name`. A `false` return is a *miss* — the resolver
    /// then tries the built-in stack — so a provider must not report an error
    /// this way.
    fn exists(&self, name: &str) -> bool;

    /// Opens `name` as a stream (`iTVPStorageMedia::Open`,
    /// `StorageIntf.h:131`). The error is the media's own failure and is
    /// surfaced verbatim, the way the reference propagates
    /// `TVPThrowExceptionMessage` out of `Open` (for example
    /// `cannot open steamfile:%1`, krkr2 `plugins/win32/steam/Storages.cpp:394`).
    fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>>;

    /// Reads `name` whole. The default streams it through [`Self::open`] the
    /// way `TVPCreateStream` plus a read-to-end does.
    fn read(&self, name: &str) -> io::Result<ResourceData> {
        let mut stream = self.open(name)?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        Ok(ResourceData::from_vec(bytes))
    }

    /// Length of `name` when the media can answer it cheaply. The default
    /// opens the entry and seeks to its end.
    fn byte_len(&self, name: &str) -> io::Result<Option<u64>> {
        let mut stream = self.open(name)?;
        let current = stream.stream_position().ok();
        let len = stream.seek(SeekFrom::End(0)).ok();
        if let Some(position) = current {
            let _ = stream.seek(SeekFrom::Start(position));
        }
        Ok(len)
    }

    /// `iTVPStorageMedia::GetListAt` (`StorageIntf.h:136`), in the spelling
    /// `Storages.dirlist`/`getDirList` promise: immediate children only, with a
    /// trailing `/` on directories (`fstat/Main.cpp:469-472`,
    /// `dirlist/Main.cpp:56-64`).
    ///
    /// `ErrorKind::NotFound` or `ErrorKind::Unsupported` mean "this media does
    /// not serve a directory here" — the resolver then falls back to the
    /// built-in stack, exactly like a failed existence probe. Any other error
    /// is a provider failure and is surfaced.
    fn list(&self, name: &str) -> io::Result<Vec<String>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "storage media `{}` does not list `{name}`",
                self.media_name()
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use krkr_core::StoragePort;

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
