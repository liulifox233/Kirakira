use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    ffi::OsStr,
    fs::{self, File},
    hash::Hash,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use encoding_rs::{Encoding, GBK, SHIFT_JIS, UTF_8};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use krkr_core::{ResourceData, ResourceDataSource, ResourceStream, StoragePort, Xp3FilterRegistry};
use krkr_tjs2::{Result, TjsError};
use krkr_xp3::{Xp3OpenOptions, Xp3ResourceProvider};
use memmap2::{Mmap, MmapOptions};

use crate::media::{FILE_MEDIA_NAME, StorageMediaProvider, is_valid_media_name, split_media_name};

const RAW_CACHE_CAPACITY_BYTES: usize = 64 * 1024 * 1024;
const RAW_CACHE_MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const EXTERNAL_MEMORY_CACHE_CAPACITY_BYTES: usize = 128 * 1024 * 1024;
/// How often a lookup retries the media auto-path rebuild after a retirement
/// won the race (see `ProjectStorage::media_auto_path_table`). Retirements are
/// script-thread events, so a second attempt normally sees a stable list.
const MEDIA_AUTO_PATH_REBUILD_ATTEMPTS: usize = 3;

#[derive(Clone)]
pub struct ProjectStorage {
    inner: Arc<ProjectStorageInner>,
}

/// Read-only package view exposed to host capabilities. Save writes remain on
/// `ProjectStorage`'s journal and are never available through this type, which
/// lets audio/media/mobile adapters depend on a package mount without gaining
/// access to mutable game storage.
#[derive(Clone)]
pub struct PackageMount(ProjectStorage);

struct ProjectStorageInner {
    root: Option<PathBuf>,
    fs_layers: Vec<ProjectLayer>,
    lookup_cache: Mutex<HashMap<String, Option<LocatedResource>>>,
    /// Lookup results for graphic loads, kept apart from [`Self::lookup_cache`]
    /// because their candidate list suggests only the registered graphic
    /// handler extensions: an image load of `PageBreak` and a plain read of
    /// the same name may legitimately resolve to different resources, so they
    /// must not share one cache entry.
    image_lookup_cache: Mutex<HashMap<String, Option<LocatedResource>>>,
    case_insensitive_dir_cache: Mutex<HashMap<PathBuf, HashMap<String, PathBuf>>>,
    raw_cache: Mutex<RawDataCache>,
    xp3_provider: Option<Xp3ResourceProvider>,
    memory_files: RwLock<BTreeMap<String, Arc<[u8]>>>,
    /// Bytes supplied by an asynchronous host fetch. Unlike bootstrap files
    /// and save writes these entries are evictable; the manifest catalogue
    /// remains intact so a later read simply schedules another fetch.
    external_memory_cache: Mutex<ExternalMemoryCache>,
    /// Logical files known to a publication even when their bytes have not
    /// been fetched yet.  Browser packages populate this from manifest.json;
    /// native views also keep memory writes here so directory enumeration has
    /// one backend-neutral source of truth.
    catalog_paths: RwLock<BTreeMap<String, String>>,
    /// Index over `catalog_paths`, updated (rebuilt or extended) only while
    /// that map's write lock is held, so the two are never written apart. A
    /// reader of the index alone — `catalog_contains`, `catalog_alias` — sees
    /// one consistent generation; a reader that takes the map first and the
    /// index second (`catalog_load_path`) may pair two adjacent generations
    /// under a concurrent write, and every name either answer returns still
    /// belongs to one of them. Lock order is `catalog_paths` first, this one
    /// second: a reader must not wait for the catalogue while holding the
    /// index.
    catalog_index: RwLock<CatalogIndex>,
    /// Files written through a memory-backed storage view since the last
    /// drain. Browser hosts persist this journal in their own origin storage;
    /// native filesystem-backed views never use it.
    memory_writes: Mutex<BTreeMap<String, Arc<[u8]>>>,
    auto_paths: RwLock<Vec<String>>,
    /// The media half of the auto-path table, cached the way the reference
    /// caches it (`TVPRebuildAutoPathTable`, `StorageIntf.cpp:1035-1144`).
    /// `None` means "rebuild before the next lookup"; the auto-path mutations
    /// and media registration retire it (`TVPClearAutoPathCache`, `:978-983`,
    /// called by `TVPAddAutoPath`/`TVPRemoveAutoPath`, `:1014`, `:1032`).
    media_auto_paths: Mutex<Option<MediaAutoPathCache>>,
    /// Bumped by every retirement of [`Self::media_auto_paths`], so a rebuild
    /// that started before it can tell that the table it just built predates
    /// the new auto-path list and must not be cached (the reference keeps the
    /// two under one critical section, `StorageIntf.cpp:1040`, `:1141`).
    media_auto_paths_generation: AtomicU64,
    /// Registered storage media (`TVPRegisterStorageMedia`), keyed by the
    /// lowercased media name so `psb://` and `PSB://` reach the same provider.
    /// A `RwLock` because plugins register while the engine serves reads from
    /// several threads.
    media_providers: RwLock<BTreeMap<String, Arc<dyn StorageMediaProvider>>>,
    revision: AtomicU64,
    /// Bumped only by changes to the name-to-file layout (search path,
    /// archive set, catalogue). Plain storage writes move `revision` so the
    /// lookup and raw-byte caches stay coherent, but leave this alone:
    /// official KRKR never drops decoded graphics because a file was written.
    graphic_revision: AtomicU64,
    /// The filter slots the mounted archives consult. Handed to
    /// [`Xp3ResourceProvider`] when the archives open and exposed through
    /// [`krkr_core::ProjectStoragePort::xp3_filter_registry`], so a plugin can
    /// install a filter long after the mounts were built.
    xp3_filter_registry: Arc<Xp3FilterRegistry>,
}

/// Case-insensitive index over [`ProjectStorageInner::catalog_paths`].
///
/// `Storages.isExistentStorage` reaches `catalog_contains`, which used to walk
/// every catalogued name twice with `eq_ignore_ascii_case` plus a `rsplit('/')`
/// per entry. GINKA probes storage names while its bootstrap runs, so that
/// scan dominated the run's first seconds — 24% of the run's samples, and
/// under 1% inside the logo frames, which are renderer-bound. The same two
/// matching rules are answered from a keyed structure instead: exact
/// case-insensitive equality against every name, and a unique basename for an
/// explicitly extended bare name.
#[derive(Default)]
struct CatalogIndex {
    /// Every catalogued name, ASCII-lowercased. `eq_ignore_ascii_case` holds
    /// exactly between two strings whose lowercased forms are equal.
    names: HashSet<String>,
    /// ASCII-lowercased basename of each catalogued name. A basename claimed
    /// by one entry maps to that name; a shared basename maps to `None`, which
    /// keeps ambiguous basenames unresolved.
    basenames: HashMap<String, Option<String>>,
}

impl CatalogIndex {
    fn from_catalog(catalog: &BTreeMap<String, String>) -> Self {
        let mut index = Self::default();
        index.extend(catalog.values().map(String::as_str));
        index
    }

    fn extend<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) {
        for name in names {
            self.names.insert(name.to_ascii_lowercase());
            let basename = name
                .rsplit_once('/')
                .map_or(name, |(_, file)| file)
                .to_ascii_lowercase();
            self.basenames
                .entry(basename)
                .and_modify(|unique| *unique = None)
                .or_insert_with(|| Some(name.to_string()));
        }
    }

    /// Whether a separator-normalized name is catalogued, ignoring case.
    fn contains_name(&self, normalized_lower: &str) -> bool {
        self.names.contains(normalized_lower)
    }

    /// The catalogued name behind a lowercased basename, when exactly one
    /// entry claims it.
    fn unique_basename(&self, basename_lower: &str) -> Option<&str> {
        self.basenames
            .get(basename_lower)
            .and_then(|path| path.as_deref())
    }
}

/// The media half of the auto-path table (`TVPRebuildAutoPathTable`,
/// `StorageIntf.cpp:1035-1144`).
///
/// The reference builds it by *listing* every auto path through the media
/// manager (`TVPStorageMediaManager::GetListAt`, `:509-515`): each listed
/// child becomes a `name → auto path` entry (`:1119-1125`), a later
/// declaration replacing an earlier one for the same name
/// (`tTJSHashTable::Add`, `tjs2/tjsHashSearch.h:245-250`), and
/// `TVPGetPlacedPath` places a request with the entry's path (`*result +
/// storagename`, `:1185-1191`).
///
/// Only auto paths whose scheme a registered media owns are listed here. A
/// plain folder or an `archive.xp3>` auto path stays on the engine's
/// filesystem/XP3 candidate walk, which resolves them live and
/// case-insensitively instead of from a snapshot.
#[derive(Debug, Default)]
struct MediaAutoPathTable {
    /// Every auto path a registered media owns, with the media name space the
    /// engine hands the media (`everything after ://`, `StorageIntf.cpp:164-168`).
    media_paths: BTreeSet<String>,
    /// Listed child name → the auto path that listed it.
    entries: BTreeMap<String, String>,
    /// Media-backed auto paths whose media cannot enumerate its name space.
    /// `GetListAt` is a required member of `iTVPStorageMedia`
    /// (`StorageIntf.h:136`); a provider that answers `NotFound`/`Unsupported`
    /// has no listing here yet, so the lookup asks the media about
    /// `auto path + name` instead of consulting `entries`.
    probes: BTreeSet<String>,
}

impl MediaAutoPathTable {
    /// The name the table places `clean` with, when this auto path is the one
    /// that owns the request's storage name (`TVPGetPlacedPath` looks the
    /// table up under `TVPExtractStorageName(normalized)` and returns
    /// `*result + storagename`, `StorageIntf.cpp:1182-1192`).
    fn placed_name(&self, auto_path: &str, clean: &Path) -> Option<String> {
        if !self.media_paths.contains(auto_path) {
            return None;
        }
        let storagename = clean.file_name().and_then(OsStr::to_str)?;
        let listed = self
            .entries
            .get(storagename)
            .is_some_and(|listed_by| listed_by == auto_path);
        if !listed && !self.probes.contains(auto_path) {
            return None;
        }
        Some(join_media_auto_path(auto_path, storagename))
    }
}

/// The cached media half of the auto-path table, with the generation of the
/// auto-path list it was built from.
///
/// The generation is what keeps a [retirement](ProjectStorage::clear_media_auto_paths)
/// from being overwritten by a rebuild that was already listing the media: the
/// reference holds one critical section across both
/// (`tTJSCriticalSectionHolder cs_holder(TVPCreateStreamCS)`,
/// `StorageIntf.cpp:1040`, released after `AutoPathTableInit = true`, `:1141`;
/// `TVPAddAutoPath`/`TVPRemoveAutoPath` take the same section, `:999-1015`,
/// `:1017-1033`), so a clear can never interleave with a rebuild there.
#[derive(Debug)]
struct MediaAutoPathCache {
    generation: u64,
    table: Arc<MediaAutoPathTable>,
}

/// `*result + storagename` (`StorageIntf.cpp:1189`): the placed name is the
/// auto path with the request's storage name appended.
///
/// `Storages.addAutoPath` stores the path without the trailing delimiter the
/// reference requires (`TVPAddAutoPath` throws
/// `TVPMissingPathDelimiterAtLast` otherwise, `:1003-1005`), so the delimiter
/// is put back here — `proxy://./` reaches the media as the `./` its
/// dictionary is keyed with (`tMediaRecord::GetDomainAndPath`, `:164-168`).
fn join_media_auto_path(auto_path: &str, storagename: &str) -> String {
    let base = auto_path.trim_end_matches('/');
    if base.is_empty() {
        storagename.to_string()
    } else {
        format!("{base}/{storagename}")
    }
}

#[derive(Clone, Debug)]
pub struct ProjectLayer {
    root: PathBuf,
    encoding_hint: Option<&'static Encoding>,
}

#[derive(Clone)]
pub struct StorageData {
    pub storage_name: String,
    pub data: ResourceData,
    pub encoding_hint: Option<&'static Encoding>,
}

#[derive(Clone)]
enum LocatedResource {
    Fs {
        storage_name: String,
        path: PathBuf,
        encoding_hint: Option<&'static Encoding>,
        byte_len: u64,
    },
    Xp3 {
        storage_name: String,
        /// Set when an `archive.xp3>` auto path pinned the lookup to one
        /// archive; `None` means the member was found by scanning mounts.
        archive: Option<String>,
        entry_name: String,
        byte_len: u64,
    },
    /// A name served by a registered storage media provider. The bytes are
    /// never cached by this resolver: a media may be live (Steam cloud, a
    /// script-owned dictionary) and owns its own caching, the way `psb`'s
    /// document cache does.
    Media {
        storage_name: String,
        provider: Arc<dyn StorageMediaProvider>,
        media_path: String,
        encoding_hint: Option<&'static Encoding>,
    },
    Memory {
        storage_name: String,
        source_path: String,
        data: Arc<[u8]>,
        encoding_hint: Option<&'static Encoding>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RawCacheKey {
    revision: u64,
    /// The XP3 filter registry's generation at read time. The archive read
    /// path runs through the live filters, so bytes from before an install
    /// must not be served after it — the reference has no byte cache and
    /// re-reads through the current pointers (`XP3Archive.cpp:1047`).
    filter_generation: u64,
    source: RawCacheSource,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RawCacheSource {
    Fs(PathBuf),
    Xp3(String),
    Memory(String),
}

struct RawDataCacheEntry {
    data: ResourceData,
    bytes: usize,
}

struct RawDataCache {
    entries: HashMap<RawCacheKey, RawDataCacheEntry>,
    lru: VecDeque<RawCacheKey>,
    bytes: usize,
    capacity_bytes: usize,
    max_entry_bytes: usize,
}

struct ExternalMemoryCache {
    entries: BTreeMap<String, usize>,
    lru: VecDeque<String>,
    oversized_unread: BTreeSet<String>,
    bytes: usize,
    capacity_bytes: usize,
}

impl ExternalMemoryCache {
    fn new() -> Self {
        Self::with_capacity(EXTERNAL_MEMORY_CACHE_CAPACITY_BYTES)
    }

    fn with_capacity(capacity_bytes: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            lru: VecDeque::new(),
            oversized_unread: BTreeSet::new(),
            bytes: 0,
            capacity_bytes,
        }
    }

    fn touch(&mut self, path: &str, bytes: usize) {
        if let Some(previous) = self.entries.insert(path.to_string(), bytes) {
            self.bytes = self.bytes.saturating_sub(previous);
            self.lru.retain(|entry| entry != path);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.lru.push_back(path.to_string());
        if bytes > self.capacity_bytes {
            self.oversized_unread.insert(path.to_string());
        } else {
            self.oversized_unread.remove(path);
        }
    }

    fn pop_lru_if_over_capacity(&mut self) -> Option<String> {
        if self.bytes <= self.capacity_bytes {
            return None;
        }
        let index = self
            .lru
            .iter()
            .position(|path| !self.oversized_unread.contains(path))?;
        let path = self.lru.remove(index)?;
        let Some(bytes) = self.entries.remove(&path) else {
            return self.pop_lru_if_over_capacity();
        };
        self.bytes = self.bytes.saturating_sub(bytes);
        Some(path)
    }

    fn finish_read(&mut self, path: &str) -> bool {
        let Some(stored_path) = self
            .oversized_unread
            .iter()
            .find(|stored| stored.eq_ignore_ascii_case(path))
            .cloned()
        else {
            return false;
        };
        self.oversized_unread.remove(&stored_path);
        self.lru.retain(|entry| entry != &stored_path);
        if let Some(bytes) = self.entries.remove(&stored_path) {
            self.bytes = self.bytes.saturating_sub(bytes);
        }
        true
    }
}

struct MmapResourceData {
    mmap: Arc<Mmap>,
}

struct MmapResourceStream {
    mmap: Arc<Mmap>,
    position: u64,
}

impl ProjectStorage {
    fn touch_external_memory(&self, path: &str) {
        // `resolve_storage` may hold the memory_files read lock while calling
        // this helper. A non-blocking cache touch avoids inverting the
        // insert/eviction lock order under concurrent native reads.
        let Ok(mut cache) = self.inner.external_memory_cache.try_lock() else {
            return;
        };
        if !cache.entries.contains_key(path) {
            return;
        }
        cache.lru.retain(|entry| entry != path);
        cache.lru.push_back(path.to_string());
    }

    fn finish_external_read(&self, path: &str) {
        let should_remove = self
            .inner
            .external_memory_cache
            .lock()
            .map(|mut cache| cache.finish_read(path))
            .unwrap_or(false);
        if should_remove {
            if let Ok(mut files) = self.inner.memory_files.write() {
                files.remove(path);
            }
            self.invalidate_caches();
        }
    }

    pub fn package_mount(&self) -> PackageMount {
        PackageMount(self.clone())
    }
    pub fn for_root(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let fs_layers = project_layers(&root);
        // The archives and the plugins share this one registry: it is created
        // before the archives are opened, handed to them, and exposed through
        // `ProjectStoragePort::xp3_filter_registry`, so a filter installed
        // after the mounts (the reference's `xp3filter.dll` timing) still
        // reaches every read.
        let xp3_filter_registry = Arc::new(Xp3FilterRegistry::new());
        let xp3_provider = open_project_archives(&root, Arc::clone(&xp3_filter_registry))?;
        Ok(Self::new_with_registry(
            Some(root),
            fs_layers,
            xp3_provider,
            Vec::new(),
            BTreeMap::new(),
            xp3_filter_registry,
        ))
    }

    pub fn new(
        root: Option<PathBuf>,
        fs_layers: Vec<ProjectLayer>,
        xp3_provider: Option<Xp3ResourceProvider>,
        auto_paths: Vec<String>,
    ) -> Self {
        Self::new_with_memory(root, fs_layers, xp3_provider, auto_paths, BTreeMap::new())
    }

    fn new_with_memory(
        root: Option<PathBuf>,
        fs_layers: Vec<ProjectLayer>,
        xp3_provider: Option<Xp3ResourceProvider>,
        auto_paths: Vec<String>,
        memory_files: BTreeMap<String, Arc<[u8]>>,
    ) -> Self {
        Self::new_with_registry(
            root,
            fs_layers,
            xp3_provider,
            auto_paths,
            memory_files,
            Arc::new(Xp3FilterRegistry::new()),
        )
    }

    fn new_with_registry(
        root: Option<PathBuf>,
        fs_layers: Vec<ProjectLayer>,
        xp3_provider: Option<Xp3ResourceProvider>,
        auto_paths: Vec<String>,
        memory_files: BTreeMap<String, Arc<[u8]>>,
        xp3_filter_registry: Arc<Xp3FilterRegistry>,
    ) -> Self {
        let mut catalog_paths = memory_files
            .keys()
            .filter_map(|path| catalog_path(path).map(|key| (key, path.clone())))
            .collect::<BTreeMap<_, _>>();
        // The native XP3 provider is also a virtual filesystem.  Keep its
        // complete logical index beside the Web manifest catalogue so KRKR's
        // auto-path lookup can resolve an unqualified, explicitly named file
        // (for example `face_0.txt`) without inventing an extension or
        // choosing an arbitrary duplicate basename.
        if let Some(provider) = &xp3_provider {
            for path in provider.entry_names() {
                if let Some(key) = catalog_path(&path) {
                    catalog_paths.entry(key).or_insert(path);
                }
            }
        }
        let catalog_index = CatalogIndex::from_catalog(&catalog_paths);
        Self {
            inner: Arc::new(ProjectStorageInner {
                root,
                fs_layers,
                lookup_cache: Mutex::new(HashMap::new()),
                image_lookup_cache: Mutex::new(HashMap::new()),
                case_insensitive_dir_cache: Mutex::new(HashMap::new()),
                raw_cache: Mutex::new(RawDataCache::new(
                    RAW_CACHE_CAPACITY_BYTES,
                    RAW_CACHE_MAX_ENTRY_BYTES,
                )),
                xp3_provider,
                memory_files: RwLock::new(memory_files),
                external_memory_cache: Mutex::new(ExternalMemoryCache::new()),
                catalog_paths: RwLock::new(catalog_paths),
                catalog_index: RwLock::new(catalog_index),
                memory_writes: Mutex::new(BTreeMap::new()),
                auto_paths: RwLock::new(auto_paths),
                media_auto_paths: Mutex::new(None),
                media_auto_paths_generation: AtomicU64::new(0),
                media_providers: RwLock::new(BTreeMap::new()),
                revision: AtomicU64::new(1),
                graphic_revision: AtomicU64::new(1),
                xp3_filter_registry,
            }),
        }
    }

    /// Creates a synchronous storage view over files already downloaded by a
    /// browser host. This keeps the TJS/KAG engine synchronous while fetches
    /// happen asynchronously outside the VM.
    pub fn from_memory<I, K, V>(entries: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Vec<u8>>,
    {
        let files = entries
            .into_iter()
            .map(|(path, bytes)| {
                (
                    normalize_storage_separators(&path.into()),
                    Arc::from(bytes.into()),
                )
            })
            .collect();
        Self::new_with_memory(
            None,
            Vec::new(),
            None,
            vec![
                "data".to_string(),
                "sys".to_string(),
                "patch".to_string(),
                "patch2".to_string(),
                "patch3".to_string(),
                "special".to_string(),
            ],
            files,
        )
    }

    /// Creates a memory-backed view and records a complete logical catalogue.
    /// Catalogue entries do not download or allocate file bytes; they only
    /// make `isExistentDirectory`/`dirlist` work for a static Web package.
    pub fn from_memory_with_catalog<I, K, V, P, S>(entries: I, catalog_paths: P) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Vec<u8>>,
        P: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let files = entries
            .into_iter()
            .map(|(path, bytes)| {
                (
                    normalize_storage_separators(&path.into()),
                    Arc::from(bytes.into()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let storage = Self::new_with_memory(
            None,
            Vec::new(),
            None,
            vec![
                "data".to_string(),
                "sys".to_string(),
                "patch".to_string(),
                "patch2".to_string(),
                "patch3".to_string(),
                "special".to_string(),
            ],
            files,
        );
        storage.set_catalog_paths(catalog_paths);
        storage
    }

    /// Replaces the logical file catalogue without touching the byte cache.
    /// Paths are normalized and compared case-insensitively, matching KRKR
    /// storage lookup rules. This is deliberately a generic virtual-FS API;
    /// it contains no knowledge of append volumes or any particular game.
    pub fn set_catalog_paths<I, S>(&self, paths: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut catalog = self
            .inner
            .memory_files
            .read()
            .map(|files| {
                files
                    .keys()
                    .filter_map(|path| catalog_path(path).map(|key| (key, path.clone())))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        for path in paths {
            let path = path.into();
            if let Some(key) = catalog_path(&path) {
                catalog
                    .entry(key)
                    .or_insert_with(|| normalize_storage_separators(&path));
            }
        }
        if let Ok(mut target) = self.inner.catalog_paths.write() {
            *target = catalog;
            self.rebuild_catalog_index(&target);
        }
        // A catalogue replacement changes both positive and negative lookup
        // results. Drop the resolver/raw caches so a newly announced Web
        // resource is not hidden behind an earlier miss.
        self.invalidate_caches();
    }

    /// Adds logical files to the catalogue. Existing entries retain their
    /// first-seen spelling so directory listings remain deterministic.
    pub fn add_catalog_paths<I, S>(&self, paths: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut changed = false;
        if let Ok(mut catalog) = self.inner.catalog_paths.write() {
            let mut added = Vec::new();
            for path in paths {
                let path = path.into();
                if let Some(key) = catalog_path(&path) {
                    if let std::collections::btree_map::Entry::Vacant(entry) = catalog.entry(key) {
                        let name = normalize_storage_separators(&path);
                        entry.insert(name.clone());
                        added.push(name);
                        changed = true;
                    }
                }
            }
            if changed && let Ok(mut index) = self.inner.catalog_index.write() {
                index.extend(added.iter().map(String::as_str));
            }
        }
        if changed {
            self.invalidate_caches();
        }
    }

    /// Adds a file to a memory-backed storage view. Web resource schedulers
    /// use this after an asynchronous fetch completes; native storage views
    /// simply reject the mutation because they have no memory overlay.
    pub fn insert_memory(&self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        self.insert_memory_with_policy(path, bytes, false);
    }

    /// Inserts a resource delivered by an asynchronous scheduler. These bytes
    /// participate in a bounded LRU and may be dropped under memory pressure;
    /// the logical catalogue is deliberately retained for future refetches.
    pub fn insert_external_memory(&self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        self.insert_memory_with_policy(path, bytes, true);
    }

    fn insert_memory_with_policy(
        &self,
        path: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        external: bool,
    ) {
        let path = canonical_memory_path(&path.into());
        let catalog_entry = catalog_path(&path);
        let bytes: Arc<[u8]> = Arc::from(bytes.into());
        // Keep cache/file lock ordering consistent for persistent and
        // external inserts. This prevents an eviction racing a save write.
        let mut external_cache = self.inner.external_memory_cache.lock().ok();
        if let Ok(mut files) = self.inner.memory_files.write() {
            files.insert(path.clone(), Arc::clone(&bytes));
            if !external {
                if let Some(cache) = external_cache.as_mut()
                    && let Some(previous) = cache.entries.remove(&path)
                {
                    cache.bytes = cache.bytes.saturating_sub(previous);
                    cache.lru.retain(|entry| entry != &path);
                    cache.oversized_unread.remove(&path);
                }
            }
            self.invalidate_caches();
        }
        if external {
            if let Some(cache) = external_cache.as_mut() {
                cache.touch(&path, bytes.len());
                while let Some(evicted) = cache.pop_lru_if_over_capacity() {
                    if let Ok(mut files) = self.inner.memory_files.write() {
                        files.remove(&evicted);
                        self.invalidate_caches();
                    }
                }
            }
        }
        if let Some(path) = catalog_entry
            && let Ok(mut catalog) = self.inner.catalog_paths.write()
            && let std::collections::btree_map::Entry::Vacant(entry) = catalog.entry(path.clone())
        {
            entry.insert(path.clone());
            if let Ok(mut index) = self.inner.catalog_index.write() {
                index.extend([path.as_str()]);
            }
        }
    }

    /// Returns and clears files written to a memory-backed project. This is a
    /// synchronous hand-off point for browser persistence (IndexedDB,
    /// localStorage, or a host-provided adapter).
    pub fn drain_memory_writes(&self) -> Vec<(String, Vec<u8>)> {
        let Ok(mut writes) = self.inner.memory_writes.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *writes)
            .into_iter()
            .map(|(path, bytes)| (path, bytes.to_vec()))
            .collect()
    }

    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Relaxed)
    }

    pub fn graphic_revision(&self) -> u64 {
        self.inner.graphic_revision.load(Ordering::Relaxed)
    }

    pub fn root(&self) -> Option<&Path> {
        self.inner.root.as_deref()
    }

    pub fn add_auto_path(&self, path: &str) {
        let path = normalize_storage_separators(path);
        let Ok(mut auto_paths) = self.inner.auto_paths.write() else {
            return;
        };
        if !auto_paths.iter().any(|item| item == &path) {
            auto_paths.push(path);
            // `TVPAddAutoPath` clears the auto-path cache (`StorageIntf.cpp:1014`),
            // which is what retires the table the last lookup built.
            self.clear_media_auto_paths();
            self.invalidate_caches();
        }
    }

    pub fn remove_auto_path(&self, path: &str) -> bool {
        let normalized = normalize_storage_separators(path)
            .trim_matches('/')
            .to_ascii_lowercase();
        let Ok(mut auto_paths) = self.inner.auto_paths.write() else {
            return false;
        };
        let before = auto_paths.len();
        auto_paths.retain(|item| {
            let item_normalized = item.trim_matches('/').to_ascii_lowercase();
            item_normalized != normalized
        });
        let removed = before != auto_paths.len();
        if removed {
            // `TVPRemoveAutoPath` clears the cache too (`StorageIntf.cpp:1032`).
            self.clear_media_auto_paths();
            self.invalidate_caches();
        }
        removed
    }

    /// Retires the cached media half of the auto-path table
    /// (`TVPClearAutoPathCache`, `StorageIntf.cpp:978-983`): the next lookup
    /// that needs it lists the media-backed auto paths again.
    ///
    /// The generation is bumped *before* the cache is cleared, so a rebuild
    /// that is inside the media listing while this runs cannot store its
    /// now-stale table afterwards ([`Self::media_auto_path_table`] refuses a
    /// store whose generation moved). The reference gets the same guarantee
    /// from one critical section around the rebuild and the clear
    /// (`StorageIntf.cpp:1040`, `:1141` versus `:999-1033`).
    fn clear_media_auto_paths(&self) {
        self.inner
            .media_auto_paths_generation
            .fetch_add(1, Ordering::SeqCst);
        if let Ok(mut table) = self.inner.media_auto_paths.lock() {
            *table = None;
        }
    }

    /// The media half of the auto-path table, rebuilt on demand and cached
    /// until something retires it — what `TVPRebuildAutoPathTable` does with
    /// its `AutoPathTableInit` flag (`StorageIntf.cpp:1035-1043`).
    ///
    /// The rebuild does not hold the cache lock (it can list a container), so
    /// the pair is made atomic the way the reference's critical section makes
    /// it (`TVPCreateStreamCS`, `:1040`, `:1141`): the generation the rebuild
    /// started from is re-checked before the store, a retirement that landed
    /// meanwhile wins, and the attempt is repeated so the lookup that lost the
    /// race still answers from the retired list. Only after
    /// [`MEDIA_AUTO_PATH_REBUILD_ATTEMPTS`] lost attempts is a table returned
    /// without being cached — nothing stale can survive in the cache.
    fn media_auto_path_table(&self) -> Arc<MediaAutoPathTable> {
        for _ in 0..MEDIA_AUTO_PATH_REBUILD_ATTEMPTS {
            let generation = self
                .inner
                .media_auto_paths_generation
                .load(Ordering::SeqCst);
            if let Ok(cache) = self.inner.media_auto_paths.lock()
                && let Some(cached) = cache.as_ref()
                && cached.generation == generation
            {
                return Arc::clone(&cached.table);
            }
            let table = Arc::new(self.rebuild_media_auto_path_table());
            if let Ok(mut cache) = self.inner.media_auto_paths.lock()
                && self
                    .inner
                    .media_auto_paths_generation
                    .load(Ordering::SeqCst)
                    == generation
            {
                *cache = Some(MediaAutoPathCache {
                    generation,
                    table: Arc::clone(&table),
                });
                return table;
            }
            // A retirement landed while the media was being listed: this table
            // may predate it and must not overwrite the retirement, so it is
            // not cached and the next attempt rebuilds from the new list.
        }
        Arc::new(self.rebuild_media_auto_path_table())
    }

    /// Lists every media-backed auto path through its media
    /// (`TVPStorageMediaManager::GetListAt`, `StorageIntf.cpp:1119`), the step
    /// that makes `Storages.addAutoPath("proxy://./")` place a plain name on
    /// the media.
    ///
    /// The auto path is stored the way `Storages.addAutoPath` left it — without
    /// the trailing delimiter the reference's `TVPAddAutoPath` requires — so
    /// the media is handed the re-delimited name space the reference would
    /// have stored (`:1003-1007`): `proxy://./` lists `./`, not `.`.
    fn rebuild_media_auto_path_table(&self) -> MediaAutoPathTable {
        let mut table = MediaAutoPathTable::default();
        for auto_path in self.auto_paths() {
            // The reference's archive branch (`:1055-1105`) lists an
            // `archive.xp3>prefix` auto path out of the archive itself; those
            // keep resolving through the engine's XP3 candidate walk.
            if auto_path.contains('>') {
                continue;
            }
            let Some((media_name, media_path)) = split_media_name(&auto_path) else {
                // A plain folder: the built-in stack resolves it per lookup,
                // live and case-insensitively.
                continue;
            };
            let Some(provider) = self.media_provider(media_name) else {
                // An unregistered scheme keeps falling through to the built-in
                // stack, the divergence `crate::media` records; the reference
                // throws `TVPUnsupportedMediaName` out of the listing
                // (`StorageIntf.cpp:211-222`).
                continue;
            };
            table.media_paths.insert(auto_path.clone());
            let namespace = if media_path.ends_with('/') {
                media_path.to_string()
            } else {
                format!("{media_path}/")
            };
            match provider.list(&namespace) {
                Ok(children) => {
                    for child in children {
                        // `GetListAt` adds files, never directories
                        // (`tTVPFileMedia::GetListAt`, `base/win32/StorageImpl.cpp:132-141`),
                        // and a child that still carries a delimiter is one the
                        // reference's storage-name lookup could not reach.
                        if child.is_empty() || child.ends_with('/') || child.contains('/') {
                            continue;
                        }
                        table.entries.insert(child, auto_path.clone());
                    }
                }
                // `GetListAt` is part of `iTVPStorageMedia`
                // (`StorageIntf.h:136`); a provider that cannot list yet
                // contributes no entries, and the lookup asks it about
                // `auto path + name` instead.
                Err(_) => {
                    table.probes.insert(auto_path.clone());
                }
            }
        }
        table
    }

    pub fn clear_archive_cache(&self) -> Result<()> {
        if let Some(provider) = &self.inner.xp3_provider {
            provider
                .clear_segment_cache()
                .map_err(|error| TjsError::runtime(error.to_string()))?;
        }
        // Clearing an archive also invalidates located-resource decisions;
        // otherwise lookup_cache can continue returning a stale XP3 member
        // (or a previous miss) after the archive has been replaced.
        self.invalidate_caches();
        Ok(())
    }

    /// Registers a storage media provider — the engine-side equivalent of
    /// `TVPRegisterStorageMedia` (`krkrz/src/core/base/StorageIntf.cpp:530`).
    ///
    /// The media name must be ASCII letters, because that is the only media
    /// spelling the reference can parse out of a storage name
    /// (`StorageIntf.cpp:299-318`), and `file` is reserved for the built-in
    /// filesystem resolver the reference always has registered
    /// (`StorageIntf.cpp:200-205`). Both cases fail the way a second
    /// registration of the same media does in the reference
    /// (`TVPMediaNameHadAlreadyBeenRegistered`, `StorageIntf.cpp:224-236`).
    ///
    /// Registration never rewrites names: a scheme that is not registered keeps
    /// resolving through the built-in stack exactly as it did before any media
    /// existed. Once registered, the provider is consulted *before* the
    /// filesystem layers, XP3, the memory overlay and the catalogue — the
    /// order `TVPGetPlacedPath` uses (`StorageIntf.cpp:1153-1197`).
    pub fn register_media(&self, provider: Arc<dyn StorageMediaProvider>) -> io::Result<()> {
        let name = provider.media_name().to_ascii_lowercase();
        if !is_valid_media_name(&name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid storage media name `{}`", provider.media_name()),
            ));
        }
        if name == FILE_MEDIA_NAME {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("storage media `{name}` is already registered"),
            ));
        }
        let Ok(mut providers) = self.inner.media_providers.write() else {
            return Err(io::Error::other("storage media registry is poisoned"));
        };
        if let Some(existing) = providers.get(&name) {
            // The engine registers a boot plugin's media once from
            // `KrkrEngine::register_plugin` and again when the first
            // `Plugins.link` installs it (`native/plugins.rs`), and the second
            // call is intentional. A plugin that keeps its media in a
            // `OnceLock` hands back the same `Arc`, which is a no-op rather
            // than `TVPMediaNameHadAlreadyBeenRegistered`
            // (`StorageIntf.cpp:224-236`); a *different* provider under a name
            // already taken still fails.
            if Arc::ptr_eq(existing, &provider) {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("storage media `{name}` is already registered"),
            ));
        }
        providers.insert(name, provider);
        drop(providers);
        // A newly registered media can satisfy names that previously missed,
        // so the negative lookup entries have to go, and an auto path whose
        // scheme it now owns can be listed for the first time. The reference
        // does not clear its auto-path table at registration
        // (`TVPRegisterStorageMedia`, `StorageIntf.cpp:530-533`) because a
        // plugin registers at load time, before any game script adds a path;
        // registration here can happen mid-session (`Plugins.link`), so the
        // table is retired as well.
        self.clear_media_auto_paths();
        self.invalidate_caches();
        Ok(())
    }

    /// Unregisters a storage media provider
    /// (`TVPUnregisterStorageMedia`, `StorageIntf.cpp:535`). Returns whether a
    /// provider was registered under `media_name`.
    pub fn unregister_media(&self, media_name: &str) -> bool {
        let name = media_name.to_ascii_lowercase();
        let removed = self
            .inner
            .media_providers
            .write()
            .map(|mut providers| providers.remove(&name).is_some())
            .unwrap_or(false);
        if removed {
            // The scheme has no provider any more, so its auto-path entries
            // must go with it (see `register_media` on why this is cleared
            // here while the reference only clears on an auto-path change).
            self.clear_media_auto_paths();
            self.invalidate_caches();
        }
        removed
    }

    /// Names of the registered media, sorted. The built-in filesystem resolver
    /// is the implicit `file` media and is not listed.
    pub fn media_names(&self) -> Vec<String> {
        self.inner
            .media_providers
            .read()
            .map(|providers| providers.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn media_provider(&self, media_name: &str) -> Option<Arc<dyn StorageMediaProvider>> {
        let name = media_name.to_ascii_lowercase();
        self.inner
            .media_providers
            .read()
            .ok()?
            .get(&name)
            .map(Arc::clone)
    }

    /// Resolves a media-qualified name against the registered providers.
    ///
    /// `None` means "no media serves this name" — either the scheme is not
    /// registered, the provider's existence probe said no, or the name belongs
    /// to the in-archive branch. The caller then falls back to the built-in
    /// stack, which is the reference's auto-path step plus our filesystem,
    /// XP3, memory and catalogue layers.
    fn media_location(&self, name: &str) -> Option<LocatedResource> {
        // `TVPIsExistentStorageNoSearchNoNormalize` splits an in-archive name
        // at `>` before any media dispatch (`StorageIntf.cpp:804-827`), so the
        // media never sees an archive member. Our in-archive branch resolves
        // through the mounted XP3 providers; a media-hosted archive container
        // needs the archive-opener hook the follow-up work adds.
        if name.contains('>') {
            return None;
        }
        // The reference unifies path delimiters before it splits the media off
        // (`StorageIntf.cpp:271-277`).
        let normalized = normalize_storage_separators(name);
        let (media_name, media_path) = split_media_name(&normalized)?;
        let provider = self.media_provider(media_name)?;
        if !provider.exists(media_path) {
            return None;
        }
        Some(LocatedResource::Media {
            storage_name: format!("{}://{media_path}", media_name.to_ascii_lowercase()),
            provider,
            media_path: media_path.to_string(),
            // Text decoding uses the same layer-name heuristic the built-in
            // stack applies (`lzfs://./data/x.tjs` decodes like `data/x.tjs`).
            encoding_hint: infer_encoding_from_path(Path::new(media_path)),
        })
    }

    /// The registered media a *write* to `name` belongs to, with the name space
    /// the media owns.
    ///
    /// Writes bypass the existence search: the reference hands the normalized
    /// name straight to the media's `Open(name, TJS_BS_WRITE…)`
    /// (`_TVPCreateStream`, `StorageIntf.cpp:1236-1244`, `:1279-1289`), so a
    /// provider that cannot update in place is still asked to and owns the
    /// error. `None` keeps the write on the built-in filesystem/memory path,
    /// which is the pre-registry behaviour for an unregistered scheme.
    fn media_write_target(&self, name: &str) -> Option<(Arc<dyn StorageMediaProvider>, String)> {
        // Like the read path, an in-archive name is split at `>` before any
        // media dispatch (`StorageIntf.cpp:804-827`).
        if name.contains('>') {
            return None;
        }
        let normalized = normalize_storage_separators(name);
        let (media_name, media_path) = split_media_name(&normalized)?;
        let provider = self.media_provider(media_name)?;
        Some((provider, media_path.to_string()))
    }

    pub fn storage_exists(&self, name: &str) -> bool {
        self.resolve_storage(name).is_ok()
    }

    /// Checks a storage name without the engine's convenience extension
    /// probing.  KRKR's `Storages.isExistentStorage` uses the exact logical
    /// name (plus configured auto paths); probing `title.ks` for `title`
    /// would make UILoader mistake a scenario for its companion `title.ini`.
    pub fn storage_exists_exact(&self, name: &str) -> bool {
        // The media probe happens on the name as given, before the auto-path
        // candidates, exactly like `TVPGetPlacedPath` checks existence first
        // (`StorageIntf.cpp:1169-1177`).
        if self.media_location(name).is_some() {
            return true;
        }
        // KRKR scripts commonly construct an archive path from
        // `System.arcPath` before adding it as an auto path.  Preserve that
        // absolute-file probe instead of rejecting it while building logical
        // VFS candidates.
        if self.find_absolute_storage(name).ok().flatten().is_some() {
            return true;
        }
        let Ok(candidates) = exact_storage_candidates_with_auto_paths(
            name,
            &self.auto_paths(),
            &self.media_auto_path_table(),
        ) else {
            return false;
        };

        for candidate in candidates {
            // A media-backed auto-path candidate is itself a media name: the
            // reference places the name and then dispatches on it
            // (`StorageIntf.cpp:1189`, `:1279-1289`).
            if self.media_location(&candidate).is_some() {
                return true;
            }
            if let Some((archive, member)) = split_archive_candidate(&candidate) {
                if self
                    .inner
                    .xp3_provider
                    .as_ref()
                    .is_some_and(|provider| provider.get_entry_in(archive, member).is_some())
                {
                    return true;
                }
                continue;
            }
            let Ok(relative) = clean_relative_path(&candidate) else {
                continue;
            };
            if self
                .find_fs_candidate(&candidate, &relative)
                .ok()
                .flatten()
                .is_some()
            {
                return true;
            }
            if self
                .inner
                .xp3_provider
                .as_ref()
                .is_some_and(|provider| provider.get_entry(&candidate).is_some())
            {
                return true;
            }
            if let Ok(memory_files) = self.inner.memory_files.read()
                && memory_files
                    .keys()
                    .any(|stored| stored.eq_ignore_ascii_case(&candidate))
            {
                return true;
            }
        }
        self.catalog_contains(name)
    }

    /// Returns whether a deferred publication catalogue holds `name` exactly,
    /// even though its bytes have not been fetched. This mirrors
    /// `Storages.isExistentStorage`, which is an exact probe in KRKR
    /// (`TVPGetPlacedPath`): it must not suggest extensions, or `title` would
    /// appear present because `title.ks` exists.
    pub fn catalog_contains(&self, name: &str) -> bool {
        let normalized = normalize_storage_separators(name);
        let lower = normalized.to_ascii_lowercase();
        let Ok(index) = self.inner.catalog_index.read() else {
            return false;
        };
        if index.contains_name(&lower) {
            return true;
        }
        // An explicitly extended bare name may use a unique auto-path
        // basename. Ambiguous basenames remain unresolved; an explicitly
        // qualified path was already checked above. Do not let `startup.ks`
        // make `startup.tjs` appear present merely because both share a stem.
        if normalized.contains('/') || !normalized.contains('.') {
            return false;
        }
        index.unique_basename(&lower).is_some()
    }

    /// Returns whether a deferred publication catalogue can satisfy a *load*
    /// of `name`. KRKR suggests extensions one layer above storage lookup
    /// (`TVPInternalLoadGraphic` walks the registered graphic handlers), so
    /// `PageBreak` must reach `PageBreak.png` and not the `PageBreak.asd`
    /// sidecar that shares its stem. The candidate order is the ordinary
    /// resolver's: the requested name plus its known extensions against the
    /// root and each auto path, then a unique basename for an explicitly
    /// extended name.
    pub fn catalog_contains_for_load(&self, name: &str) -> bool {
        self.catalog_load_path(name).is_some()
    }

    fn catalog_load_path(&self, name: &str) -> Option<String> {
        let normalized = normalize_storage_separators(name);
        let Ok(catalog) = self.inner.catalog_paths.read() else {
            return None;
        };
        let lookup =
            |candidate: &str| catalog_path(candidate).and_then(|key| catalog.get(&key).cloned());
        if let Ok(candidates) = self.storage_candidates(&normalized) {
            for candidate in &candidates {
                if split_archive_candidate(candidate).is_some() {
                    continue;
                }
                if let Some(path) = lookup(candidate) {
                    return Some(path);
                }
            }
        }
        // An explicitly extended bare name may use a unique auto-path
        // basename. Ambiguous basenames remain unresolved; an explicitly
        // qualified path was already checked above. Do not let `startup.ks`
        // make `startup.tjs` appear present merely because both share a stem.
        if normalized.contains('/') || !normalized.contains('.') {
            return None;
        }
        drop(catalog);
        let index = self.inner.catalog_index.read().ok()?;
        index
            .unique_basename(&normalized.to_ascii_lowercase())
            .map(str::to_string)
    }

    /// Returns whether a logical directory exists in the filesystem, XP3
    /// provider, registered storage media, or deferred publication catalogue.
    pub fn is_directory(&self, name: &str) -> bool {
        self.list_directory(name).is_ok()
    }

    /// Lists a media-owned directory through its provider.
    ///
    /// `None` means the name is not media-qualified, no media is registered for
    /// its scheme, or the provider reports that it does not serve a directory
    /// here (`NotFound`/`Unsupported`) — the caller then keeps resolving
    /// through the built-in stack. Any other provider error is returned as-is:
    /// `GetListAt` sits behind the same call as `Open` in the reference, so a
    /// broken media must not look like a missing directory.
    ///
    /// The provider's own order is preserved; it owns its namespace, the way
    /// `steam`'s listing is a `std::set` and `psb`'s follows the document.
    fn media_listing(&self, name: &str) -> io::Result<Option<Vec<String>>> {
        if name.contains('>') {
            return Ok(None);
        }
        let normalized = normalize_storage_separators(name);
        let Some((media_name, media_path)) = split_media_name(&normalized) else {
            return Ok(None);
        };
        let Some(provider) = self.media_provider(media_name) else {
            return Ok(None);
        };
        match provider.list(media_path) {
            Ok(children) => Ok(Some(children)),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::Unsupported
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Lists immediate children using KRKR/fstat semantics: directory names
    /// have a trailing `/`, files do not. The result is sorted and de-duped
    /// case-insensitively across filesystem, XP3 and manifest layers.
    pub fn list_directory(&self, name: &str) -> io::Result<Vec<String>> {
        if let Some(children) = self.media_listing(name)? {
            return Ok(children);
        }
        let (relative, absolute) = self.directory_paths(name)?;
        let prefix = if relative.is_empty() {
            String::new()
        } else {
            format!("{relative}/")
        };
        let prefix_lower = prefix.to_ascii_lowercase();
        let mut children = BTreeMap::<String, String>::new();
        let mut found_directory = false;

        if let Ok(catalog) = self.inner.catalog_paths.read() {
            for path in catalog.values() {
                let normalized = normalize_storage_separators(path);
                let normalized_lower = normalized.to_ascii_lowercase();
                let Some(rest) = normalized
                    .get(prefix.len()..)
                    .filter(|_| normalized_lower.starts_with(&prefix_lower))
                else {
                    continue;
                };
                if rest.is_empty() {
                    continue;
                }
                found_directory = true;
                let (child, is_directory) = match rest.split_once('/') {
                    Some((child, _)) => (child, true),
                    None => (rest, false),
                };
                let display = if is_directory {
                    format!("{child}/")
                } else {
                    child.to_string()
                };
                children
                    .entry(display.to_ascii_lowercase())
                    .or_insert(display);
            }
        }

        for layer in &self.inner.fs_layers {
            let Some(path) = self.layer_directory_path(layer, &relative, absolute.as_deref())?
            else {
                continue;
            };
            if !path.is_dir() {
                continue;
            }
            found_directory = true;
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let file_name = entry.file_name().to_string_lossy().into_owned();
                let display = if entry.path().is_dir() {
                    format!("{file_name}/")
                } else {
                    file_name
                };
                children
                    .entry(display.to_ascii_lowercase())
                    .or_insert(display);
            }
        }

        if let Some(provider) = &self.inner.xp3_provider {
            for path in provider.entry_names() {
                let normalized = normalize_storage_separators(&path);
                let normalized_lower = normalized.to_ascii_lowercase();
                let Some(rest) = normalized
                    .get(prefix.len()..)
                    .filter(|_| normalized_lower.starts_with(&prefix_lower))
                else {
                    continue;
                };
                if rest.is_empty() {
                    continue;
                }
                found_directory = true;
                let (child, is_directory) = match rest.split_once('/') {
                    Some((child, _)) => (child, true),
                    None => (rest, false),
                };
                let display = if is_directory {
                    format!("{child}/")
                } else {
                    child.to_string()
                };
                children
                    .entry(display.to_ascii_lowercase())
                    .or_insert(display);
            }
        }

        if !found_directory {
            return Err(storage_not_found(name));
        }
        Ok(children.into_values().collect())
    }

    fn layer_directory_path(
        &self,
        layer: &ProjectLayer,
        logical_relative: &str,
        absolute: Option<&Path>,
    ) -> io::Result<Option<PathBuf>> {
        if let Some(absolute) = absolute {
            return Ok(absolute
                .starts_with(&layer.root)
                .then(|| absolute.to_path_buf()));
        }
        let Some(project_root) = self.inner.root.as_deref() else {
            return Ok(None);
        };
        let layer_prefix = layer
            .root
            .strip_prefix(project_root)
            .ok()
            .map(path_to_storage_name)
            .unwrap_or_default();
        let relative = if layer_prefix.is_empty() {
            logical_relative.to_string()
        } else if logical_relative.is_empty() {
            return Ok(None);
        } else if logical_relative.eq_ignore_ascii_case(&layer_prefix) {
            String::new()
        } else if let Some(rest) = logical_relative
            .strip_prefix(&format!("{layer_prefix}/"))
            .or_else(|| {
                let prefix_lower = format!("{layer_prefix}/").to_ascii_lowercase();
                logical_relative.get(prefix_lower.len()..).filter(|_| {
                    logical_relative
                        .to_ascii_lowercase()
                        .starts_with(&prefix_lower)
                })
            })
        {
            rest.to_string()
        } else {
            return Ok(None);
        };
        let path = if relative.is_empty() {
            layer.root.clone()
        } else {
            self.resolve_case_insensitive_path(&layer.root, Path::new(&relative))?
                .unwrap_or_else(|| layer.root.join(&relative))
        };
        Ok(Some(path))
    }

    fn directory_paths(&self, name: &str) -> io::Result<(String, Option<PathBuf>)> {
        let normalized = normalize_storage_separators(name);
        let path = Path::new(&normalized);
        if path.is_absolute() {
            let absolute = path.to_path_buf();
            for layer in &self.inner.fs_layers {
                if let Ok(relative) = absolute.strip_prefix(&layer.root) {
                    return Ok((path_to_storage_name(relative), Some(absolute)));
                }
            }
            return Ok((String::new(), Some(absolute)));
        }
        let trimmed = normalized.trim_matches('/');
        let relative = clean_relative_path(trimmed).map_err(tjs_error_to_io)?;
        Ok((path_to_storage_name(&relative), None))
    }

    pub fn placed_path(&self, name: &str) -> Option<PathBuf> {
        match self.resolve_storage(name).ok()? {
            LocatedResource::Fs { path, .. } => Some(path),
            // Media providers report no locally accessible name in the
            // reference either (`GetLocallyAccessibleName` is `""` for
            // psb/lzfs/proxy/steam/zip/var).
            LocatedResource::Xp3 { .. }
            | LocatedResource::Media { .. }
            | LocatedResource::Memory { .. } => None,
        }
    }

    /// Returns the normalized logical storage name selected by the resolver.
    /// Unlike [`Self::placed_path`], this also works for XP3, media and
    /// memory-backed resources, matching KRKR's `TVPGetPlacedPath` contract
    /// (which returns a logical `archive.xp3>entry` name rather than an OS path
    /// for archives, and the `media://domain/path` spelling for a media).
    pub fn resolved_storage_name(&self, name: &str) -> Option<String> {
        Some(self.resolve_storage(name).ok()?.storage_name().to_string())
    }

    pub fn read_data(&self, name: &str) -> Result<StorageData> {
        self.read_data_for_kind(name, StorageLoadKind::Generic)
    }

    /// Reads the bytes a graphic load resolves to. `TVPInternalLoadGraphic`
    /// suggests only the extensions with a registered graphic handler
    /// (`visual/GraphicsLoaderIntf.cpp:1478-1506`), so an image load of
    /// `PageBreak` must reach `PageBreak.png` and never the same-stem
    /// `PageBreak.asd` sidecar.
    pub fn read_image_storage(&self, name: &str) -> Result<ResourceData> {
        self.read_data_for_kind(name, StorageLoadKind::Image)
            .map(|storage| storage.data)
    }

    fn read_data_for_kind(&self, name: &str, kind: StorageLoadKind) -> Result<StorageData> {
        let located = self
            .resolve_storage_io_for_kind(name, kind)
            .map_err(io_error)?;
        let storage_name = located.storage_name().to_string();
        let encoding_hint = located.encoding_hint();
        let external_source = located.memory_source_path().map(str::to_owned);
        let data = match self.load_located_data(&located).map_err(io_error) {
            Ok(data) => data,
            Err(error) => {
                if let Some(path) = external_source.as_deref() {
                    self.finish_external_read(path);
                }
                return Err(error);
            }
        };
        if let Some(path) = external_source {
            self.finish_external_read(&path);
        }
        Ok(StorageData {
            storage_name,
            data,
            encoding_hint,
        })
    }

    pub fn read_binary_storage(&self, name: &str) -> Result<ResourceData> {
        self.read_data(name).map(|storage| storage.data)
    }

    pub fn read_binary_vec(&self, name: &str) -> Result<Vec<u8>> {
        let data = self.read_binary_storage(name)?;
        data.as_bytes()
            .map(|bytes| bytes.into_owned())
            .map_err(io_error)
    }

    pub fn read_text_storage(&self, name: &str, configured_encoding: &str) -> Result<String> {
        let storage = self.read_data(name)?;
        let bytes = storage.data.as_bytes().map_err(io_error)?;
        decode_text_storage(name, &bytes, storage.encoding_hint, configured_encoding)
    }

    pub fn write_text_storage(&self, name: &str, mode: &str, text: &str) -> Result<()> {
        let bytes = encode_tjs_text_stream(text, mode)?;
        self.write_binary_storage(name, mode, &bytes)
    }

    pub fn write_binary_storage(&self, name: &str, mode: &str, bytes: &[u8]) -> Result<()> {
        // A registered media owns writes into its scheme, exactly as the
        // reference dispatches through `_TVPCreateStream`'s write branch
        // (`StorageIntf.cpp:1236-1244`): the provider is asked before the
        // memory overlay and the filesystem, with no existence probe. Any
        // successful write invalidates the write-side caches the way the
        // built-in path does, mirroring the reference's cache clear after a
        // write (`:1282-1289`).
        if let Some((provider, media_path)) = self.media_write_target(name) {
            provider.write(&media_path, mode, bytes).map_err(io_error)?;
            self.invalidate_write_caches();
            return Ok(());
        }
        let Some(root) = self.inner.root.as_ref() else {
            let key = memory_write_key(name)?;
            let mut output = bytes.to_vec();
            if let Some(offset) = storage_mode_offset(mode) {
                let mut current = self.read_binary_vec(&key).map_err(|_| {
                    TjsError::runtime(format!("cannot update missing storage: {name}"))
                })?;
                let offset = usize::try_from(offset).map_err(|_| {
                    TjsError::runtime(format!("storage offset is too large: {offset}"))
                })?;
                let end = offset
                    .checked_add(bytes.len())
                    .ok_or_else(|| TjsError::runtime("storage write is too large"))?;
                if current.len() < end {
                    current.resize(end, 0);
                }
                current[offset..end].copy_from_slice(bytes);
                output = current;
            }
            let data: Arc<[u8]> = Arc::from(output);
            if let Ok(mut files) = self.inner.memory_files.write() {
                files.insert(key.clone(), Arc::clone(&data));
            }
            if let Ok(mut writes) = self.inner.memory_writes.lock() {
                writes.insert(key, data);
            }
            self.invalidate_write_caches();
            return Ok(());
        };
        let path = storage_write_path(root, name)?;
        // Release cached raw file views before writing so Windows does not reject
        // overwriting a file that is still held by a read-only mmap in `raw_cache`.
        self.clear_raw_cache();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let result = if let Some(offset) = storage_mode_offset(mode) {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .truncate(false)
                .open(&path)
                .map_err(io_error)?;
            file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
            file.write_all(bytes).map_err(io_error)
        } else {
            fs::write(&path, bytes).map_err(io_error)
        };
        if result.is_ok() {
            self.invalidate_write_caches();
        }
        result
    }

    /// Creates the directory `name` the way the reference's `fstat` plugin
    /// does: the name is resolved through the same root mapping the write path
    /// uses (`TVPNormalizeStorageName` + `TVPGetLocalName`,
    /// `StorageIntf.cpp:549`, `:579-584`) and only the final component is
    /// created — `CreateDirectory(dir, NULL)`
    /// (`krkr2 src/plugins/win32/fstat/Main.cpp:586`) never builds parents and
    /// fails with `ERROR_ALREADY_EXISTS` when the directory is already there,
    /// so this is `fs::create_dir`, never `create_dir_all`.
    ///
    /// A media-qualified name has no local path in the reference either:
    /// `TVPGetLocalName` throws `TVPCannotGetLocalName` when the media reports
    /// no locally accessible name (`StorageIntf.cpp:581-584`), so one is
    /// rejected here instead of becoming a folder named after its scheme.
    #[allow(clippy::result_large_err)] // the crate-wide `TjsError` size lint
    pub fn create_directory(&self, name: &str) -> Result<()> {
        if split_media_name(name).is_some() {
            return Err(TjsError::runtime(format!(
                "storage path must be a local name: {name}"
            )));
        }
        let Some(root) = self.inner.root.as_ref() else {
            return Err(TjsError::runtime(format!(
                "cannot create a directory without a project root: {name}"
            )));
        };
        let path = storage_write_path(root, name)?;
        fs::create_dir(&path).map_err(io_error)?;
        self.invalidate_write_caches();
        Ok(())
    }

    /// The filter slots this storage's mounted archives consult
    /// (`TVPSetXP3ArchiveExtractionFilter`). Installing into the returned
    /// registry reaches every later stream creation and read, whether or not
    /// the archive was opened before the install.
    pub fn xp3_filter_registry(&self) -> Arc<Xp3FilterRegistry> {
        Arc::clone(&self.inner.xp3_filter_registry)
    }

    pub fn open_storage(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
        match self.resolve_storage_io(name)? {
            LocatedResource::Fs { path, .. } => {
                File::open(path).map(|file| Box::new(file) as Box<dyn ResourceStream>)
            }
            LocatedResource::Xp3 {
                archive,
                entry_name,
                ..
            } => {
                let provider = self.inner.xp3_provider.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "XP3 provider is not configured")
                })?;
                match archive.as_deref() {
                    Some(archive) => provider.open_in(archive, &entry_name),
                    None => provider.open(&entry_name),
                }
            }
            LocatedResource::Media {
                provider,
                media_path,
                ..
            } => provider.open(&media_path),
            LocatedResource::Memory {
                source_path, data, ..
            } => {
                let stream = Box::new(Cursor::new(data.to_vec())) as Box<dyn ResourceStream>;
                self.finish_external_read(&source_path);
                Ok(stream)
            }
        }
    }

    pub fn storage_byte_len(&self, name: &str) -> io::Result<Option<u64>> {
        match self.resolve_storage_io(name)? {
            LocatedResource::Fs { byte_len, .. } | LocatedResource::Xp3 { byte_len, .. } => {
                Ok(Some(byte_len))
            }
            LocatedResource::Memory { data, .. } => Ok(Some(data.len() as u64)),
            // A media decides its own length and may not know it without
            // opening the entry.
            LocatedResource::Media {
                provider,
                media_path,
                ..
            } => provider.byte_len(&media_path),
        }
    }

    pub(crate) fn storage_candidates(&self, name: &str) -> Result<Vec<String>> {
        self.storage_candidates_for_kind(name, StorageLoadKind::Generic)
    }

    /// Candidate spellings for a lookup of `kind`. An image lookup suggests
    /// only the registered graphic-handler extensions.
    pub(crate) fn storage_candidates_for_kind(
        &self,
        name: &str,
        kind: StorageLoadKind,
    ) -> Result<Vec<String>> {
        storage_candidates_with_auto_paths(
            name,
            &self.auto_paths(),
            &self.media_auto_path_table(),
            kind,
        )
    }

    fn resolve_storage(&self, name: &str) -> Result<LocatedResource> {
        self.resolve_storage_io(name).map_err(io_error)
    }

    fn resolve_storage_io(&self, name: &str) -> io::Result<LocatedResource> {
        self.resolve_storage_io_for_kind(name, StorageLoadKind::Generic)
    }

    fn resolve_storage_io_for_kind(
        &self,
        name: &str,
        kind: StorageLoadKind,
    ) -> io::Result<LocatedResource> {
        // A media is live storage, so a miss must not be remembered the way a
        // filesystem miss is: a Steam cloud file can appear, and a script
        // re-probing `steam://` has to see it.
        let media_qualified = media_qualified_name(name);

        // A registered storage media is consulted before everything else, the
        // way `TVPIsExistentStorageNoSearchNoNormalize` dispatches on the media
        // name before the file media ever sees it (`StorageIntf.cpp:799-830`).
        // An unregistered scheme simply falls through to the built-in stack,
        // which is what names like `psb://` did before media registration
        // existed.
        if let Some(storage) = self.media_location(name) {
            return Ok(storage);
        }

        if let Some(storage) = self.find_absolute_storage(name)? {
            return Ok(storage);
        }

        if let Ok(cache) = self.lookup_cache_for(kind).lock()
            && let Some(storage) = cache.get(name).cloned()
        {
            // A lookup-cache hit must still refresh the external-memory LRU;
            // otherwise repeatedly reading a hot fetched asset can be evicted
            // by an unrelated insertion.
            if let Some(LocatedResource::Memory { source_path, .. }) = storage.as_ref() {
                self.touch_external_memory(source_path);
            }
            return storage.ok_or_else(|| storage_not_found(name));
        }

        let groups = storage_candidate_groups(
            name,
            &self.auto_paths(),
            &self.media_auto_path_table(),
            kind,
        )
        .map_err(tjs_error_to_io)?;
        // Candidates are ordered the way the reference searches: the requested
        // name itself (the project folder) first, then one candidate per auto
        // path in *reverse* declaration order, so the last `Storages.addAutoPath`
        // wins. Each requested spelling is resolved completely before the walk
        // moves on to the next one: the filesystem, the one mount an
        // `archive.xp3>` candidate pins, and -- only when none of that
        // spelling's declared candidates matched -- the mount-wide scan for
        // archives the game never declared as an auto path
        // (`Xp3ResourceProvider::get_entry` walks the mounts in reverse, so the
        // later mount wins for a duplicate member).
        //
        // The group boundary keeps a declared auto path from being pre-empted
        // by that fallback. Running the mount-wide scan only once *every*
        // candidate was tried let the declared `patch3.xp3>PageBreak.asd`
        // beat the `system/PageBreak.png` that the earlier `system/` auto path
        // addresses; deferring the scan to the end of each spelling keeps the
        // qualified spelling the reference's auto-path table yields for a
        // sidecar (`pagebreak.asd` resolves through `patch3.xp3>`), while an
        // earlier spelling's mount hit still beats a later spelling, which is
        // how the graphic loader probes suggested names: a full storage lookup
        // per `name + extension`, first hit wins (`TVPInternalLoadGraphic`,
        // `visual/GraphicsLoaderIntf.cpp:1478-1506`).
        //
        // Resolving all filesystem candidates in a separate pass first would
        // also let a folder auto path declared *before* a patch archive shadow
        // the archive's copy; the reference keeps one auto-path table entry per
        // basename and the latest declaration replaces it
        // (`tTJSHashTable::Add`, `krkrz/src/core/tjs2/tjsHashSearch.h`),
        // whether that declaration names a folder or an archive.
        for candidates in &groups {
            for candidate in candidates {
                // A media-backed auto-path candidate is a media name
                // (`TVPGetPlacedPath` returns `proxy://./krmovie.dll`, and
                // `_TVPCreateStream` then opens it through the media,
                // `StorageIntf.cpp:1189`, `:1279-1289`).
                if let Some(storage) = self.media_location(candidate) {
                    self.cache_lookup(kind, name, Some(storage.clone()));
                    return Ok(storage);
                }
                if let Some((archive, member)) = split_archive_candidate(candidate) {
                    if let Some(provider) = &self.inner.xp3_provider
                        && let Some(entry) = provider.get_entry_in(archive, member)
                    {
                        let storage = LocatedResource::Xp3 {
                            storage_name: candidate.clone(),
                            archive: Some(archive.to_string()),
                            entry_name: entry.name.clone(),
                            byte_len: entry.original_size,
                        };
                        self.cache_lookup(kind, name, Some(storage.clone()));
                        return Ok(storage);
                    }
                    continue;
                }
                let relative = clean_relative_path(candidate).map_err(tjs_error_to_io)?;
                if let Some(storage) = self.find_fs_candidate(candidate, &relative)? {
                    self.cache_lookup(kind, name, Some(storage.clone()));
                    return Ok(storage);
                }
            }

            // The name-only scan is the engine's fallback for archives the game
            // never declared as an auto path. It runs after the declared
            // candidates of the same spelling, so a qualified candidate of that
            // spelling still wins over a bare-name mount hit (the reference's
            // auto-path table reports the archive-qualified placed path), and
            // before the next spelling's candidates, so an image load's earlier
            // suggestion wins over a later one.
            if let Some(provider) = &self.inner.xp3_provider {
                for candidate in candidates {
                    if split_archive_candidate(candidate).is_some() {
                        continue;
                    }
                    if let Some(entry) = provider.get_entry(candidate) {
                        let storage = LocatedResource::Xp3 {
                            storage_name: candidate.clone(),
                            archive: None,
                            entry_name: entry.name.clone(),
                            byte_len: entry.original_size,
                        };
                        self.cache_lookup(kind, name, Some(storage.clone()));
                        return Ok(storage);
                    }
                }
            }
        }

        for candidate in groups.iter().flatten() {
            if split_archive_candidate(candidate).is_some() {
                continue;
            }
            let normalized = normalize_storage_separators(candidate);
            let Ok(memory_files) = self.inner.memory_files.read() else {
                continue;
            };
            if let Some(data) = memory_files.get(&normalized) {
                self.touch_external_memory(&normalized);
                let storage = LocatedResource::Memory {
                    storage_name: candidate.clone(),
                    source_path: normalized.clone(),
                    encoding_hint: infer_encoding_from_path(Path::new(candidate)),
                    data: Arc::clone(data),
                };
                self.cache_lookup(kind, name, Some(storage.clone()));
                return Ok(storage);
            }
            if let Some((stored_path, data)) = memory_files
                .iter()
                .find(|(stored, _)| stored.eq_ignore_ascii_case(&normalized))
            {
                self.touch_external_memory(stored_path);
                let storage = LocatedResource::Memory {
                    storage_name: candidate.clone(),
                    source_path: stored_path.clone(),
                    encoding_hint: infer_encoding_from_path(Path::new(stored_path)),
                    data: Arc::clone(data),
                };
                self.cache_lookup(kind, name, Some(storage.clone()));
                return Ok(storage);
            }
        }

        // XP3 entries (and deferred manifest entries) are indexed by their
        // full logical names, but KRKR's auto-path table also exposes a
        // unique basename.  Resolve only an explicitly named candidate here:
        // extension selection remains the caller's responsibility.
        for candidate in groups.iter().flatten() {
            if split_archive_candidate(candidate).is_some() {
                continue;
            }
            let Some(alias) = self.catalog_alias(candidate) else {
                continue;
            };
            if let Some(storage) = self.find_catalog_resource(candidate, &alias)? {
                self.cache_lookup(kind, name, Some(storage.clone()));
                return Ok(storage);
            }
        }

        // Remote Web manifests expose unambiguous basename aliases for KRKR's
        // auto-path lookup. Mirror that behavior in the memory overlay so a
        // preloaded `main/Config.tjs` also satisfies `Config.tjs` without a
        // duplicate network request.
        for candidate in groups.iter().flatten() {
            if split_archive_candidate(candidate).is_some() {
                continue;
            }
            let normalized = normalize_storage_separators(candidate);
            if normalized.contains('/') {
                continue;
            }
            let Ok(memory_files) = self.inner.memory_files.read() else {
                continue;
            };
            let mut matched: Option<(&String, &Arc<[u8]>)> = None;
            let mut ambiguous = false;
            for (stored_path, data) in memory_files.iter() {
                if stored_path
                    .rsplit('/')
                    .next()
                    .is_some_and(|basename| basename.eq_ignore_ascii_case(&normalized))
                {
                    if matched.is_some() {
                        ambiguous = true;
                        break;
                    }
                    matched = Some((stored_path, data));
                }
            }
            if !ambiguous && let Some((stored_path, data)) = matched {
                self.touch_external_memory(stored_path);
                let storage = LocatedResource::Memory {
                    storage_name: candidate.clone(),
                    source_path: stored_path.clone(),
                    encoding_hint: infer_encoding_from_path(Path::new(stored_path)),
                    data: Arc::clone(data),
                };
                self.cache_lookup(kind, name, Some(storage.clone()));
                return Ok(storage);
            }
        }

        if !media_qualified {
            self.cache_lookup(kind, name, None);
        }
        Err(storage_not_found(name))
    }

    /// The catalogued name behind an explicitly extended bare name, when
    /// exactly one entry claims that basename. Ambiguous basenames stay
    /// unresolved, the rule [`Self::catalog_contains`] documents.
    fn catalog_alias(&self, name: &str) -> Option<String> {
        let normalized = normalize_storage_separators(name);
        if normalized.contains('/') || !normalized.contains('.') {
            return None;
        }
        self.inner
            .catalog_index
            .read()
            .ok()?
            .unique_basename(&normalized.to_ascii_lowercase())
            .map(str::to_string)
    }

    fn find_catalog_resource(
        &self,
        storage_name: &str,
        catalog_path: &str,
    ) -> io::Result<Option<LocatedResource>> {
        let relative = clean_relative_path(catalog_path).map_err(tjs_error_to_io)?;
        if let Some(storage) = self.find_fs_candidate(storage_name, &relative)? {
            return Ok(Some(storage));
        }
        if let Some(provider) = &self.inner.xp3_provider
            && let Some(entry) = provider.get_entry(catalog_path)
        {
            return Ok(Some(LocatedResource::Xp3 {
                storage_name: storage_name.to_string(),
                archive: None,
                entry_name: entry.name.clone(),
                byte_len: entry.original_size,
            }));
        }
        let Ok(memory_files) = self.inner.memory_files.read() else {
            return Ok(None);
        };
        let Some((stored_path, data)) = memory_files
            .iter()
            .find(|(stored, _)| stored.eq_ignore_ascii_case(catalog_path))
        else {
            return Ok(None);
        };
        self.touch_external_memory(stored_path);
        Ok(Some(LocatedResource::Memory {
            storage_name: storage_name.to_string(),
            source_path: stored_path.clone(),
            encoding_hint: infer_encoding_from_path(Path::new(catalog_path)),
            data: Arc::clone(data),
        }))
    }

    fn find_fs_candidate(
        &self,
        storage_name: &str,
        relative: &Path,
    ) -> io::Result<Option<LocatedResource>> {
        for layer in self.inner.fs_layers.iter().rev() {
            let path = layer.root.join(relative);
            if path.is_file() {
                return Ok(Some(self.located_fs_storage(
                    storage_name.to_string(),
                    path,
                    layer.encoding_hint,
                )?));
            }
            if let Some(path) = self.resolve_case_insensitive_path(&layer.root, relative)?
                && path.is_file()
            {
                return Ok(Some(self.located_fs_storage(
                    storage_name.to_string(),
                    path,
                    layer.encoding_hint,
                )?));
            }
        }
        Ok(None)
    }

    fn resolve_case_insensitive_path(
        &self,
        root: &Path,
        relative: &Path,
    ) -> io::Result<Option<PathBuf>> {
        let mut current = root.to_path_buf();
        for component in relative.components() {
            let Component::Normal(part) = component else {
                return Ok(None);
            };
            let exact = current.join(part);
            if exact.exists() {
                current = exact;
                continue;
            }

            let Some(part) = part.to_str() else {
                return Ok(None);
            };
            if !current.is_dir() {
                return Ok(None);
            }
            let Some(path) = self.case_insensitive_dir_entry(&current, part)? else {
                return Ok(None);
            };
            current = path;
        }
        Ok(Some(current))
    }

    fn case_insensitive_dir_entry(&self, dir: &Path, name: &str) -> io::Result<Option<PathBuf>> {
        let key = name.to_ascii_lowercase();
        if let Ok(cache) = self.inner.case_insensitive_dir_cache.lock()
            && let Some(entries) = cache.get(dir)
        {
            return Ok(entries.get(&key).cloned());
        }

        let mut entries = HashMap::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            entries
                .entry(entry.file_name().to_string_lossy().to_ascii_lowercase())
                .or_insert_with(|| entry.path());
        }
        let result = entries.get(&key).cloned();
        if let Ok(mut cache) = self.inner.case_insensitive_dir_cache.lock() {
            cache.insert(dir.to_path_buf(), entries);
        }
        Ok(result)
    }

    fn located_fs_storage(
        &self,
        storage_name: String,
        path: PathBuf,
        encoding_hint: Option<&'static Encoding>,
    ) -> io::Result<LocatedResource> {
        let byte_len = path.metadata()?.len();
        Ok(LocatedResource::Fs {
            storage_name,
            path,
            encoding_hint,
            byte_len,
        })
    }

    fn find_absolute_storage(&self, name: &str) -> io::Result<Option<LocatedResource>> {
        let path = Path::new(name);
        if !path.is_absolute() || !is_safe_absolute_storage_path(path) {
            return Ok(None);
        }
        let path = path.to_path_buf();
        if !path.is_file() {
            return Ok(None);
        }
        let encoding_hint = self
            .inner
            .fs_layers
            .iter()
            .find(|layer| path.starts_with(&layer.root))
            .and_then(|layer| layer.encoding_hint)
            .or_else(|| infer_encoding_from_path(&path));
        self.located_fs_storage(name.to_string(), path, encoding_hint)
            .map(Some)
    }

    fn load_located_data(&self, located: &LocatedResource) -> io::Result<ResourceData> {
        // A media is live storage (Steam cloud, a container mounted by a
        // plugin, a value the script owns) and caches on its own terms — the
        // `psb` document cache is the reference example. `cache_source` answers
        // `None` for one, so this resolver never remembers its bytes.
        let key = located.cache_source().map(|source| RawCacheKey {
            revision: self.revision(),
            filter_generation: self.inner.xp3_filter_registry.generation(),
            source,
        });
        if let Some(key) = &key
            && let Ok(mut cache) = self.inner.raw_cache.lock()
            && let Some(data) = cache.get(key)
        {
            return Ok(data);
        }

        let data = match located {
            LocatedResource::Fs { path, byte_len, .. } => load_fs_resource_data(path, *byte_len)?,
            LocatedResource::Xp3 {
                archive,
                entry_name,
                ..
            } => {
                let provider = self.inner.xp3_provider.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "XP3 provider is not configured")
                })?;
                let mut stream = match archive.as_deref() {
                    Some(archive) => provider.open_in(archive, entry_name)?,
                    None => provider.open(entry_name)?,
                };
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes)?;
                ResourceData::from_vec(bytes)
            }
            LocatedResource::Media {
                provider,
                media_path,
                ..
            } => provider.read(media_path)?,
            LocatedResource::Memory { data, .. } => ResourceData::from_vec(data.to_vec()),
        };

        if let Some(key) = key
            && let Ok(mut cache) = self.inner.raw_cache.lock()
        {
            cache.insert(key, data.clone());
        }
        Ok(data)
    }

    fn lookup_cache_for(
        &self,
        kind: StorageLoadKind,
    ) -> &Mutex<HashMap<String, Option<LocatedResource>>> {
        match kind {
            StorageLoadKind::Generic => &self.inner.lookup_cache,
            StorageLoadKind::Image => &self.inner.image_lookup_cache,
        }
    }

    fn cache_lookup(&self, kind: StorageLoadKind, name: &str, storage: Option<LocatedResource>) {
        if let Ok(mut cache) = self.lookup_cache_for(kind).lock() {
            cache.insert(name.to_string(), storage);
        }
    }

    pub fn auto_paths(&self) -> Vec<String> {
        self.inner
            .auto_paths
            .read()
            .map(|paths| paths.clone())
            .unwrap_or_default()
    }

    /// Rebuilds [`Self::inner`]'s catalogue index from the catalogue the
    /// caller has just written. This is one of the index's three update sites
    /// — `add_catalog_paths` and `insert_memory_with_policy` extend it
    /// instead of rebuilding — and all three run while the catalogue's write
    /// lock is held, so the index never moves except with the map.
    fn rebuild_catalog_index(&self, catalog: &BTreeMap<String, String>) {
        if let Ok(mut index) = self.inner.catalog_index.write() {
            *index = CatalogIndex::from_catalog(catalog);
        }
    }

    fn invalidate_caches(&self) {
        self.inner.graphic_revision.fetch_add(1, Ordering::Relaxed);
        self.invalidate_write_caches();
    }

    /// Drops the caches that a plain storage write invalidates: the name
    /// lookup, the case-insensitive directory listings and the raw byte
    /// views. Decoded graphics survive, matching KRKR, where only
    /// `System.clearGraphicCache` and compact events clear them.
    fn invalidate_write_caches(&self) {
        self.inner.revision.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut cache) = self.inner.lookup_cache.lock() {
            cache.clear();
        }
        if let Ok(mut cache) = self.inner.image_lookup_cache.lock() {
            cache.clear();
        }
        if let Ok(mut cache) = self.inner.case_insensitive_dir_cache.lock() {
            cache.clear();
        }
        self.clear_raw_cache();
    }

    fn clear_raw_cache(&self) {
        if let Ok(mut cache) = self.inner.raw_cache.lock() {
            cache.clear();
        }
    }
}

impl StoragePort for PackageMount {
    fn open(&self, path: &str) -> io::Result<Box<dyn ResourceStream>> {
        self.0.open(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.0.exists(path)
    }

    fn data(&self, path: &str) -> io::Result<ResourceData> {
        self.0.data(path)
    }

    fn byte_len(&self, path: &str) -> io::Result<Option<u64>> {
        self.0.byte_len(path)
    }

    fn revision(&self) -> u64 {
        self.0.revision()
    }

    fn graphic_revision(&self) -> u64 {
        self.0.graphic_revision()
    }
}

impl StoragePort for ProjectStorage {
    fn open(&self, path: &str) -> io::Result<Box<dyn ResourceStream>> {
        self.open_storage(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.storage_exists(path)
    }

    fn data(&self, path: &str) -> io::Result<ResourceData> {
        self.read_binary_storage(path).map_err(tjs_error_to_io)
    }

    fn byte_len(&self, path: &str) -> io::Result<Option<u64>> {
        self.storage_byte_len(path)
    }

    fn revision(&self) -> u64 {
        ProjectStorage::revision(self)
    }

    fn graphic_revision(&self) -> u64 {
        ProjectStorage::graphic_revision(self)
    }
}

impl krkr_core::ProjectStoragePort for ProjectStorage {
    fn storage_exists(&self, name: &str) -> bool {
        ProjectStorage::storage_exists(self, name)
    }

    fn storage_exists_exact(&self, name: &str) -> bool {
        ProjectStorage::storage_exists_exact(self, name)
    }

    fn is_directory(&self, name: &str) -> bool {
        ProjectStorage::is_directory(self, name)
    }

    fn list_directory(&self, name: &str) -> io::Result<Vec<String>> {
        ProjectStorage::list_directory(self, name)
    }

    fn placed_path(&self, name: &str) -> Option<String> {
        ProjectStorage::placed_path(self, name).map(|path| path.display().to_string())
    }

    fn resolved_storage_name(&self, name: &str) -> Option<String> {
        ProjectStorage::resolved_storage_name(self, name)
    }

    fn read_binary_storage(&self, name: &str) -> io::Result<ResourceData> {
        ProjectStorage::read_binary_storage(self, name).map_err(tjs_error_to_io)
    }

    fn read_image_storage(&self, name: &str) -> io::Result<ResourceData> {
        ProjectStorage::read_image_storage(self, name).map_err(tjs_error_to_io)
    }

    fn read_text_storage(&self, name: &str, configured_encoding: &str) -> io::Result<String> {
        ProjectStorage::read_text_storage(self, name, configured_encoding).map_err(tjs_error_to_io)
    }

    fn read_text_storage_mode(
        &self,
        name: &str,
        mode: &str,
        configured_encoding: &str,
    ) -> io::Result<String> {
        if let Some(offset) = storage_mode_offset(mode) {
            let bytes = ProjectStorage::read_binary_vec(self, name).map_err(tjs_error_to_io)?;
            let offset = usize::try_from(offset).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "storage offset is too large")
            })?;
            let bytes = bytes.get(offset..).unwrap_or_default();
            decode_text_storage(name, bytes, None, configured_encoding).map_err(tjs_error_to_io)
        } else {
            ProjectStorage::read_text_storage(self, name, configured_encoding)
                .map_err(tjs_error_to_io)
        }
    }

    fn write_text_storage(&self, name: &str, mode: &str, text: &str) -> io::Result<()> {
        ProjectStorage::write_text_storage(self, name, mode, text).map_err(tjs_error_to_io)
    }

    fn write_binary_storage(&self, name: &str, mode: &str, bytes: &[u8]) -> io::Result<()> {
        ProjectStorage::write_binary_storage(self, name, mode, bytes).map_err(tjs_error_to_io)
    }

    fn create_directory(&self, name: &str) -> io::Result<()> {
        ProjectStorage::create_directory(self, name).map_err(tjs_error_to_io)
    }

    fn add_auto_path(&self, path: &str) {
        ProjectStorage::add_auto_path(self, path);
    }

    fn remove_auto_path(&self, path: &str) -> bool {
        ProjectStorage::remove_auto_path(self, path)
    }

    fn clear_archive_cache(&self) -> io::Result<()> {
        ProjectStorage::clear_archive_cache(self).map_err(tjs_error_to_io)
    }

    fn catalog_contains(&self, name: &str) -> bool {
        ProjectStorage::catalog_contains(self, name)
    }

    fn catalog_contains_for_load(&self, name: &str) -> bool {
        ProjectStorage::catalog_contains_for_load(self, name)
    }

    fn set_catalog_paths(&self, paths: &[String]) {
        ProjectStorage::set_catalog_paths(self, paths.iter().cloned());
    }

    fn insert_memory(&self, path: &str, bytes: Vec<u8>) {
        ProjectStorage::insert_memory(self, path.to_string(), bytes);
    }

    fn insert_external_memory(&self, path: &str, bytes: Vec<u8>) {
        ProjectStorage::insert_external_memory(self, path.to_string(), bytes);
    }

    fn drain_memory_writes(&self) -> Vec<(String, Vec<u8>)> {
        ProjectStorage::drain_memory_writes(self)
    }

    fn xp3_filter_registry(&self) -> Option<Arc<Xp3FilterRegistry>> {
        // A storage without archives has no read path to filter; answering
        // `None` tells a plugin that instead of accepting an install nothing
        // will ever consult.
        self.inner
            .xp3_provider
            .is_some()
            .then(|| ProjectStorage::xp3_filter_registry(self))
    }

    fn register_storage_media(&self, media: Arc<dyn StorageMediaProvider>) -> io::Result<()> {
        ProjectStorage::register_media(self, media)
    }

    fn unregister_storage_media(&self, media_name: &str) -> bool {
        ProjectStorage::unregister_media(self, media_name)
    }

    fn storage_media_names(&self) -> Vec<String> {
        ProjectStorage::media_names(self)
    }
}

impl LocatedResource {
    fn storage_name(&self) -> &str {
        match self {
            Self::Fs { storage_name, .. }
            | Self::Xp3 { storage_name, .. }
            | Self::Media { storage_name, .. }
            | Self::Memory { storage_name, .. } => storage_name,
        }
    }

    fn encoding_hint(&self) -> Option<&'static Encoding> {
        match self {
            Self::Fs { encoding_hint, .. } => *encoding_hint,
            Self::Xp3 { .. } => None,
            Self::Media { encoding_hint, .. } => *encoding_hint,
            Self::Memory { encoding_hint, .. } => *encoding_hint,
        }
    }

    fn cache_source(&self) -> Option<RawCacheSource> {
        Some(match self {
            Self::Fs { path, .. } => RawCacheSource::Fs(path.clone()),
            Self::Xp3 {
                archive,
                entry_name,
                ..
            } => RawCacheSource::Xp3(match archive {
                Some(archive) => format!("{archive}>{entry_name}"),
                None => entry_name.clone(),
            }),
            Self::Memory { storage_name, .. } => RawCacheSource::Memory(storage_name.clone()),
            // Media bytes are live and never cached here.
            Self::Media { .. } => return None,
        })
    }

    fn memory_source_path(&self) -> Option<&str> {
        match self {
            Self::Memory { source_path, .. } => Some(source_path),
            Self::Fs { .. } | Self::Xp3 { .. } | Self::Media { .. } => None,
        }
    }
}

impl RawDataCache {
    fn new(capacity_bytes: usize, max_entry_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            bytes: 0,
            capacity_bytes,
            max_entry_bytes,
        }
    }

    fn get(&mut self, key: &RawCacheKey) -> Option<ResourceData> {
        let data = self.entries.get(key)?.data.clone();
        self.touch(key.clone());
        Some(data)
    }

    fn insert(&mut self, key: RawCacheKey, data: ResourceData) {
        let bytes = data.byte_len().min(usize::MAX as u64) as usize;
        if bytes > self.max_entry_bytes || bytes > self.capacity_bytes {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries
            .insert(key.clone(), RawDataCacheEntry { data, bytes });
        self.touch(key);
        self.evict_to_capacity();
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.bytes = 0;
    }

    fn touch(&mut self, key: RawCacheKey) {
        self.lru.retain(|item| item != &key);
        self.lru.push_back(key);
    }

    fn evict_to_capacity(&mut self) {
        while self.bytes > self.capacity_bytes {
            let Some(key) = self.lru.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

impl ResourceDataSource for MmapResourceData {
    fn byte_len(&self) -> u64 {
        self.mmap.len() as u64
    }

    fn as_bytes(&self) -> io::Result<Cow<'_, [u8]>> {
        Ok(Cow::Borrowed(self.mmap.as_ref()))
    }

    fn open_stream(&self) -> io::Result<Box<dyn ResourceStream>> {
        Ok(Box::new(MmapResourceStream {
            mmap: Arc::clone(&self.mmap),
            position: 0,
        }))
    }
}

impl Read for MmapResourceStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.position >= self.mmap.len() as u64 {
            return Ok(0);
        }
        let start = self.position as usize;
        let end = (start + buffer.len()).min(self.mmap.len());
        let len = end - start;
        buffer[..len].copy_from_slice(&self.mmap[start..end]);
        self.position = self.position.saturating_add(len as u64);
        Ok(len)
    }
}

impl Seek for MmapResourceStream {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let len = self.mmap.len() as i128;
        let next = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => len + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if next < 0 || next > len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "resource seek target is outside the mapped file",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

fn load_fs_resource_data(path: &Path, byte_len: u64) -> io::Result<ResourceData> {
    if byte_len == 0 {
        return Ok(ResourceData::from_bytes(Arc::<[u8]>::from([])));
    }
    let file = File::open(path)?;
    // SAFETY: The map is read-only and owns no mutable alias to the file. If an
    // external process mutates the file concurrently, the OS-defined mmap view is
    // still contained behind immutable bytes and cache invalidation is handled by
    // ProjectStorage revision changes for writes made through the engine.
    let mmap = unsafe { MmapOptions::new().map(&file)? };
    Ok(ResourceData::new(Arc::new(MmapResourceData {
        mmap: Arc::new(mmap),
    })))
}

pub fn project_layers(root: &Path) -> Vec<ProjectLayer> {
    let mut layers = Vec::new();
    push_project_layer(
        &mut layers,
        root.to_path_buf(),
        infer_encoding_from_layer_name(root),
    );
    for name in ["data", "sys", "patch", "patch2", "patch3", "special"] {
        let layer = root.join(name);
        if layer.is_dir() {
            push_project_layer(
                &mut layers,
                layer,
                infer_encoding_from_layer_name(Path::new(name)),
            );
        }
    }
    layers
}

fn push_project_layer(
    layers: &mut Vec<ProjectLayer>,
    root: PathBuf,
    encoding_hint: Option<&'static Encoding>,
) {
    if layers.iter().any(|layer| layer.root == root) {
        return;
    }
    layers.push(ProjectLayer {
        root,
        encoding_hint,
    });
}

fn infer_encoding_from_layer_name(path: &Path) -> Option<&'static Encoding> {
    match path.file_name().and_then(|name| name.to_str()) {
        Some("patch") | Some("patch2") | Some("patch3") | Some("special") => Some(GBK),
        Some("data") | Some("sys") => Some(SHIFT_JIS),
        _ => None,
    }
}

fn infer_encoding_from_path(path: &Path) -> Option<&'static Encoding> {
    path.components().find_map(|component| match component {
        Component::Normal(part) => infer_encoding_from_layer_name(Path::new(part)),
        _ => None,
    })
}

fn storage_candidates_with_auto_paths(
    name: &str,
    auto_paths: &[String],
    media_table: &MediaAutoPathTable,
    kind: StorageLoadKind,
) -> Result<Vec<String>> {
    let groups = storage_candidate_groups(name, auto_paths, media_table, kind)?;
    Ok(groups.into_iter().flatten().collect())
}

/// Candidate spellings for one lookup, grouped by the requested spelling: the
/// name itself first, then one candidate per auto path in *reverse* declaration
/// order, so the last `Storages.addAutoPath` wins. The resolver resolves a
/// whole group before moving to the next spelling, which is the unit the
/// reference resolves: `TVPGetPlacedPath` finishes the current-folder check and
/// the auto-path table for one name before anything else
/// (`StorageIntf.cpp:1153-1197`), and the graphic loader probes one suggested
/// name at a time (`visual/GraphicsLoaderIntf.cpp:1478-1506`).
fn storage_candidate_groups(
    name: &str,
    auto_paths: &[String],
    media_table: &MediaAutoPathTable,
    kind: StorageLoadKind,
) -> Result<Vec<Vec<String>>> {
    let names = storage_lookup_names(name, kind)?;
    let mut groups = Vec::with_capacity(names.len());
    for name in names {
        let clean = clean_relative_path(&name)?;
        let mut candidates = Vec::with_capacity(auto_paths.len() + 1);
        push_unique_storage_candidate(&mut candidates, &clean);
        for auto_path in auto_paths.iter().rev() {
            for candidate in auto_path_candidates(auto_path, &clean, media_table) {
                push_unique_storage_name(&mut candidates, candidate);
            }
        }
        groups.push(candidates);
    }
    Ok(groups)
}

fn exact_storage_candidates_with_auto_paths(
    name: &str,
    auto_paths: &[String],
    media_table: &MediaAutoPathTable,
) -> Result<Vec<String>> {
    let normalized = normalize_storage_separators(name);
    let clean = clean_relative_path(&normalized)?;
    let mut candidates = Vec::with_capacity(auto_paths.len() + 1);
    push_unique_storage_candidate(&mut candidates, &clean);
    for auto_path in auto_paths.iter().rev() {
        for candidate in auto_path_candidates(auto_path, &clean, media_table) {
            push_unique_storage_name(&mut candidates, candidate);
        }
    }
    Ok(candidates)
}

/// The candidates one auto path contributes for `clean`. A media-backed auto
/// path contributes the name its media placed (`TVPGetPlacedPath` returns
/// `auto path + storagename`, `StorageIntf.cpp:1189`); every other auto path
/// contributes the folder/archive join the engine has always resolved.
fn auto_path_candidates(
    auto_path: &str,
    clean: &Path,
    media_table: &MediaAutoPathTable,
) -> Vec<String> {
    if media_table.media_paths.contains(auto_path) {
        // The table only places names this auto path listed: the reference
        // rebuilds it by listing the media (`TVPRebuildAutoPathTable`,
        // `StorageIntf.cpp:1035-1144`), so a name the listing does not carry
        // has no entry and misses, even when the media itself would serve it.
        return media_table
            .placed_name(auto_path, clean)
            .into_iter()
            .collect();
    }
    let Some(inner) = normalize_auto_path(auto_path) else {
        return Vec::new();
    };
    let Ok(auto_relative) = clean_relative_path(&inner) else {
        return Vec::new();
    };
    let joined = path_to_storage_name(&auto_relative.join(clean));
    // `archive.xp3>` auto paths name one archive. KRKR resolves the member
    // through that archive alone, so keep the qualifier on the candidate
    // instead of degrading it into a name-only lookup across every mount.
    match auto_path_archive(auto_path) {
        Some(archive) => vec![format!("{archive}>{joined}")],
        None => vec![joined],
    }
}

/// Returns the archive file name of an `.../archive.xp3>prefix` auto path.
fn auto_path_archive(auto_path: &str) -> Option<String> {
    let path = normalize_storage_separators(auto_path);
    let (outer, _) = path.split_once('>')?;
    let name = outer.rsplit('/').next()?;
    name.rsplit_once('.')
        .filter(|(_, extension)| extension.eq_ignore_ascii_case("xp3"))
        .map(|_| name.to_string())
}

/// Splits a candidate produced from an archive-scoped auto path.
fn split_archive_candidate(candidate: &str) -> Option<(&str, &str)> {
    candidate.split_once('>')
}

/// Whether `name` is media-qualified (`media://…`). The in-archive branch owns
/// names with `>` (`StorageIntf.cpp:804-827`), so they are not media names.
fn media_qualified_name(name: &str) -> bool {
    !name.contains('>') && split_media_name(&normalize_storage_separators(name)).is_some()
}

fn push_unique_storage_candidate(candidates: &mut Vec<String>, path: &Path) {
    push_unique_storage_name(candidates, path_to_storage_name(path));
}

fn push_unique_storage_name(candidates: &mut Vec<String>, candidate: String) {
    if !candidates.iter().any(|item| item == &candidate) {
        candidates.push(candidate);
    }
}

fn path_to_storage_name(path: &Path) -> String {
    normalize_storage_separators(&path.to_string_lossy())
}

/// The extension set a lookup may suggest, threaded from the loader that asked
/// for the bytes.
///
/// KRKR suggests extensions at the loader layer, not inside `TVPGetPlacedPath`
/// (`krkrz/base/StorageIntf.cpp:1153-1197` probes the name as given and then
/// the auto-path table): the graphic loader refuses a name whose extension no
/// handler serves and, for an extensionless name, tries `name + extension` for
/// each registered graphic handler and loads the first that exists
/// (`TVPInternalLoadGraphic`, `krkrz/visual/GraphicsLoaderIntf.cpp:1452`,
/// suggestion loop `:1478-1506`; `GraphicsLoaderIntf.cpp:2307-2336` in the
/// stock krkr2 tree). A plain storage read instead completes with the engine's
/// known storage extensions. Keeping the kind on the candidate list is what
/// stops an image load of `PageBreak` from reaching the same-stem
/// `PageBreak.asd` sidecar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum StorageLoadKind {
    /// `Storages.open`, script and struct reads, and every non-graphic load.
    Generic,
    /// A graphic load (`Layer.loadImages`, `Bitmap.load`, KAG image tags).
    Image,
}

/// Extensions of the graphic handlers this engine registers, in the order the
/// engine's completion list already pinned (TLG first). Only these may be
/// suggested to a graphic load. The engine registers handlers for TLG
/// (5/6), PNG, JPEG, BMP and WebP; `dib`/`jif`/`jxr`, which stock KRKR serves
/// through the same table, have no engine handler and are therefore not
/// suggested here.
const GRAPHIC_STORAGE_EXTENSIONS: [&str; 6] = ["tlg", "png", "jpg", "jpeg", "bmp", "webp"];

/// The engine's extension completion for a non-graphic load. The graphic
/// formats lead, then the names a script may ask for. The order is the
/// reference-neutral engine order and decides what a bare stem resolves to, so
/// it stays explicit.
const STORAGE_EXTENSIONS: [&str; 14] = [
    "tlg", "png", "jpg", "jpeg", "bmp", "webp", "ks", "tjs", "asd", "ogg", "wav", "tcw", "mpg",
    "mpeg",
];

/// The engine's extension completion for a name that carries no known
/// extension. The reference has no completion at this layer:
/// `TVPGetPlacedPath` (`krkrz/base/StorageIntf.cpp:1153-1197`) probes the name
/// as given and then the auto-path table, and the completion a game relies on
/// is the game's own -- KAGEX appends the extension itself and probes the
/// exact name (`system/Utils.tjs:373 getExistFileNameAutoExtFill`, fed by
/// `_imageFileExtList`, `system/KAGEnvironment.tjs:60300`, and
/// `Layer.SupportedExtensions`, `system/Utils.tjs:2409`), while a graphic load
/// suggests the registered handler's extensions (`TVPInternalLoadGraphic`,
/// `krkrz/visual/GraphicsLoaderIntf.cpp:1478-1506`). These lists are the
/// engine-side equivalent for the load path, so they stay explicit and
/// ordered: the order decides what a bare stem resolves to.
///
/// KAGEX image-source extensions (`stand`, `sinfo`, `event`, `stage`, `emf`)
/// are deliberately absent. The game spells those names out before probing
/// (`Storages.isExistentStorage(name + extension)`), so `foo` resolving to
/// `foo.stand` here would answer a call the reference answers with `""` and
/// flip the branch (`Storages.getPlacedPath(stem)` then
/// `getPlacedPath(stem + ".stand")`) the framework probes with.
///
/// An image load suggests only [`GRAPHIC_STORAGE_EXTENSIONS`]: `PageBreak` must
/// reach `PageBreak.png` and never the `PageBreak.asd` sidecar.
fn storage_lookup_names(name: &str, kind: StorageLoadKind) -> Result<Vec<String>> {
    let name = normalize_storage_separators(name);
    clean_relative_path(&name)?;
    let path = Path::new(&name);
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| is_known_storage_extension(extension, kind))
    {
        return Ok(vec![name]);
    }

    let mut names = Vec::with_capacity(STORAGE_EXTENSIONS.len() + 1);
    names.push(name.clone());
    for extension in storage_extensions(kind) {
        names.push(format!("{name}.{extension}"));
    }
    Ok(names)
}

/// The extensions a lookup of `kind` may suggest. The graphic set is a prefix
/// of the generic one, so a name with a non-graphic extension completes like an
/// extensionless one for an image load and can never pick up a non-graphic
/// sidecar; the reference's graphic loader refuses such a name outright
/// (`GraphicsLoaderIntf.cpp:1507-1509` throws `TVPUnknownGraphicFormat`) and
/// only ever completes with handler extensions.
fn storage_extensions(kind: StorageLoadKind) -> &'static [&'static str] {
    match kind {
        StorageLoadKind::Generic => &STORAGE_EXTENSIONS,
        StorageLoadKind::Image => &GRAPHIC_STORAGE_EXTENSIONS,
    }
}

fn is_known_storage_extension(extension: &str, kind: StorageLoadKind) -> bool {
    let extension = extension.to_ascii_lowercase();
    storage_extensions(kind)
        .iter()
        .any(|known| *known == extension)
}

pub(crate) fn normalize_auto_path(path: &str) -> Option<String> {
    let path = normalize_storage_separators(path);
    if let Some((_, inner_path)) = path.split_once('>') {
        return Some(inner_path.trim_start_matches('/').to_string());
    }
    if path.is_empty() {
        return Some(String::new());
    }
    let path = Path::new(&path);
    if path.is_absolute() {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .filter(|name| !name.ends_with(".xp3"))
    } else if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xp3"))
    {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
    } else {
        Some(path.to_string_lossy().into_owned())
    }
}

pub fn decode_text_storage(
    _name: &str,
    bytes: &[u8],
    encoding_hint: Option<&'static Encoding>,
    configured_encoding: &str,
) -> Result<String> {
    // KRKR strips an UTF-8 BOM before handing text to the configured
    // decoder. Treat it as an encoding declaration rather than exposing
    // U+FEFF to scripts or scenario parsers.
    let bytes = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        &bytes[3..]
    } else {
        bytes
    };
    if let Some(text) = decode_tjs_text_stream(bytes)? {
        return Ok(text);
    }

    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
        return Ok(text);
    }

    let mut encodings = Vec::new();
    if let Some(encoding) = encoding_hint {
        encodings.push(encoding);
    }
    if let Some(encoding) = Encoding::for_label(configured_encoding.as_bytes()) {
        encodings.push(encoding);
    }
    encodings.push(SHIFT_JIS);
    encodings.push(GBK);
    encodings.push(UTF_8);

    for encoding in encodings {
        let (text, _, had_errors) = encoding.decode(bytes);
        if !had_errors {
            return Ok(text.into_owned());
        }
    }

    let encoding = encoding_hint.unwrap_or_else(|| {
        Encoding::for_label(configured_encoding.as_bytes()).unwrap_or(SHIFT_JIS)
    });
    let (text, _, _) = encoding.decode(bytes);
    Ok(text.into_owned())
}

pub(crate) fn decode_tjs_text_stream(bytes: &[u8]) -> Result<Option<String>> {
    if bytes.starts_with(&[0xff, 0xfe]) {
        return decode_utf16le(&bytes[2..]).map(Some);
    }
    if !bytes.starts_with(&[0xfe, 0xfe]) || bytes.len() < 5 {
        return Ok(None);
    }
    let mode = bytes[2];
    if bytes[3] != 0xff || bytes[4] != 0xfe {
        return Ok(None);
    }
    match mode {
        0 => {
            let mut units = utf16le_units(&bytes[5..]);
            for unit in &mut units {
                if *unit >= 0x20 {
                    *unit ^= ((*unit & 0x00fe) << 8) ^ 1;
                }
            }
            String::from_utf16(&units)
                .map(Some)
                .map_err(|error| TjsError::runtime(format!("invalid UTF-16 text stream: {error}")))
        }
        1 => {
            let mut units = utf16le_units(&bytes[5..]);
            for unit in &mut units {
                *unit = swap_adjacent_bits(*unit);
            }
            String::from_utf16(&units)
                .map(Some)
                .map_err(|error| TjsError::runtime(format!("invalid UTF-16 text stream: {error}")))
        }
        2 => {
            if bytes.len() < 21 {
                return Err(TjsError::runtime("compressed text stream is truncated"));
            }
            let compressed_len = u64::from_le_bytes(
                bytes[5..13]
                    .try_into()
                    .expect("slice length checked for compressed length"),
            ) as usize;
            let uncompressed_len = u64::from_le_bytes(
                bytes[13..21]
                    .try_into()
                    .expect("slice length checked for uncompressed length"),
            ) as usize;
            let compressed = bytes
                .get(21..21 + compressed_len)
                .ok_or_else(|| TjsError::runtime("compressed text stream is truncated"))?;
            let mut decoder = ZlibDecoder::new(compressed);
            let mut decoded = Vec::with_capacity(uncompressed_len);
            decoder.read_to_end(&mut decoded).map_err(io_error)?;
            if decoded.len() != uncompressed_len {
                return Err(TjsError::runtime("compressed text stream length mismatch"));
            }
            decode_utf16le(&decoded).map(Some)
        }
        _ => Err(TjsError::runtime(format!(
            "unsupported text stream mode {mode}"
        ))),
    }
}

pub(crate) fn encode_tjs_text_stream(text: &str, mode: &str) -> Result<Vec<u8>> {
    let mut payload = utf16le_bytes(text);
    if mode.contains('z') {
        let level = mode
            .split('z')
            .nth(1)
            .and_then(|rest| rest.chars().next())
            .and_then(|ch| ch.to_digit(10))
            .unwrap_or(6);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
        encoder.write_all(&payload).map_err(io_error)?;
        let compressed = encoder.finish().map_err(io_error)?;
        let mut bytes = vec![0xfe, 0xfe, 2, 0xff, 0xfe];
        bytes.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&compressed);
        return Ok(bytes);
    }
    if mode.contains('c') {
        for chunk in payload.chunks_exact_mut(2) {
            let unit = u16::from_le_bytes([chunk[0], chunk[1]]);
            chunk.copy_from_slice(&swap_adjacent_bits(unit).to_le_bytes());
        }
        let mut bytes = vec![0xfe, 0xfe, 1, 0xff, 0xfe];
        bytes.extend_from_slice(&payload);
        return Ok(bytes);
    }
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

fn decode_utf16le(bytes: &[u8]) -> Result<String> {
    String::from_utf16(&utf16le_units(bytes))
        .map_err(|error| TjsError::runtime(format!("invalid UTF-16 text stream: {error}")))
}

fn utf16le_units(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

fn utf16le_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>()
}

fn swap_adjacent_bits(value: u16) -> u16 {
    ((value & 0xaaaa) >> 1) | ((value & 0x5555) << 1)
}

pub fn normalize_storage_separators(path: &str) -> String {
    path.replace('\\', "/")
}

/// Normalizes a logical KRKR storage name for APIs such as
/// `Storages.getFullPath`. This is deliberately independent of the host OS:
/// separators are `/`, duplicate and `.` segments collapse, and `..` may
/// remove a prior segment but cannot escape the logical project root. XP3's
/// `>` delimiter is preserved while its in-archive path receives the same
/// normalization and KRKR's case-insensitive spelling. A trailing `/` is
/// part of the name: the reference's compression loop only deletes a
/// delimiter that is followed by another one (`StorageIntf.cpp:409-413`), so
/// `"dir/"` normalizes to `"dir/"` and `"dir//"` to `"dir/"` — but a `/`
/// immediately in front of `>` is a duplicated delimiter there and
/// disappears (`"dir/>"` is `"dir>"`).
///
/// A media-qualified name keeps the `://` the reference treats as structure
/// (`media://domain/path`, `StorageIntf.cpp:299-354`, reassembled at `:462`):
/// `getFullPath("psb://container.psb/inner/")` stays `psb://container.psb/inner/`
/// instead of folding to `psb:/container.psb/inner/`, so the result still
/// reaches the provider through `split_media_name`.
pub fn normalize_storage_name(path: &str) -> Result<String> {
    let path = normalize_storage_separators(path);
    let (outer, inner) = path
        .split_once('>')
        .map_or((path.as_str(), None), |(outer, inner)| (outer, Some(inner)));
    // The reference splits `media://domain/path` before it compacts anything
    // (`StorageIntf.cpp:299-354`), lower-cases the media name (`:366-374`) and
    // reassembles `media + "://" + domain + path` (`:462`), so the two slashes
    // are structure rather than a path delimiter and never collapse. Only the
    // path part is compacted; the domain is re-emitted as written, the way the
    // reference leaves it to the media's own `NormalizeDomainName` (this
    // engine does not case-fold outer names either).
    let mut outer = match split_media_prefix(outer) {
        Some((media, domain, path)) => {
            let path = normalize_logical_path(path, false)?;
            format!("{}://{domain}{path}", media.to_ascii_lowercase())
        }
        None => normalize_logical_path(outer, false)?,
    };
    let Some(inner) = inner else {
        return Ok(outer);
    };
    // The reference appends `>` + the in-archive name and runs one compression
    // loop over the combined string (`StorageIntf.cpp:392-398`), where `>` is
    // a delimiter like `/` (`:405`). A trailing delimiter on the outer path is
    // therefore deleted by the duplicated-delimiter rule (`:409-413`); it only
    // survives when it ends the whole name. The two slashes of a media prefix
    // are structure rather than path delimiters, so an empty name space keeps
    // them (`"psb://>"`).
    if outer.ends_with('/') && !outer.ends_with("://") {
        outer.pop();
    }
    let inner = normalize_logical_path(inner, true)?;
    Ok(format!("{outer}>{inner}"))
}

/// Splits the reference's `media://domain/path` form (`StorageIntf.cpp:299-354`)
/// into the media name, the domain and the path (which keeps its leading `/`).
///
/// Only the explicit `://` spelling counts: it is the one
/// [`split_media_name`] accepts, and the reference's `media:/path` and
/// `media:path` spellings have never reached a provider in this engine. The
/// reference fills an omitted domain from the media's current domain (`"."`)
/// and prepends its current path to a relative one; this engine has no
/// current media/domain/path state and re-emits what the name carries. A
/// `media://domain` name with no path is `TVPInvalidPathName` there
/// (`:341-343`); this engine hands such a name to the registered media, which
/// owns its own namespace validation (`crate::media`), so the spelling is kept
/// instead of rejected.
fn split_media_prefix(name: &str) -> Option<(&str, &str, &str)> {
    let (media, name_space) = name.split_once("://")?;
    if !is_valid_media_name(media) {
        return None;
    }
    // The domain runs to the next `/`; a name with no further delimiter names
    // the media itself, which keeps the whole `name_space` as its domain.
    let domain_len = name_space.find('/').unwrap_or(name_space.len());
    Some((media, &name_space[..domain_len], &name_space[domain_len..]))
}

fn normalize_logical_path(path: &str, lower_case: bool) -> Result<String> {
    let absolute = path.starts_with('/');
    // TVPNormalizeStorageName's compression loop (`StorageIntf.cpp:400-453`)
    // collapses duplicated delimiters but keeps a trailing one: the only
    // deletion is of a delimiter that is itself followed by a delimiter
    // (`:409-413`). `getFullPath("dir/")` therefore stays `"dir/"`, while
    // `"dir//"` collapses to `"dir/"`.
    let trailing_delimiter = path.ends_with('/');
    let mut parts = Vec::new();
    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            if parts.pop().is_none() {
                return Err(TjsError::runtime(format!(
                    "storage path must stay inside project root: {path}"
                )));
            }
            continue;
        }
        parts.push(if lower_case {
            component.to_ascii_lowercase()
        } else {
            component.to_string()
        });
    }
    let mut result = parts.join("/");
    if result.is_empty() {
        // The logical root keeps the reference's single leading delimiter.
        if absolute {
            result.push('/');
        }
    } else {
        if absolute {
            result.insert(0, '/');
        }
        if trailing_delimiter && !result.ends_with('/') {
            result.push('/');
        }
    }
    Ok(result)
}

fn canonical_memory_path(path: &str) -> String {
    let normalized = normalize_storage_separators(path);
    clean_relative_path(&normalized)
        .map(|path| path_to_storage_name(&path))
        .unwrap_or(normalized)
}

fn catalog_path(path: &str) -> Option<String> {
    let normalized = normalize_storage_separators(path);
    if normalized.ends_with('/') {
        return None;
    }
    let trimmed = normalized.trim_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let relative = clean_relative_path(trimmed).ok()?;
    let value = path_to_storage_name(&relative);
    (!value.is_empty()).then_some(value.to_ascii_lowercase())
}

fn is_safe_absolute_storage_path(path: &Path) -> bool {
    path.components()
        .all(|component| !matches!(component, Component::ParentDir))
}

fn memory_write_key(name: &str) -> Result<String> {
    if name.contains('>') {
        return Err(TjsError::runtime(format!(
            "cannot write to archive-qualified storage: {name}"
        )));
    }
    let normalized = normalize_storage_separators(name);
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err(TjsError::runtime(format!(
            "memory storage path must be relative: {name}"
        )));
    }
    Ok(normalize_storage_separators(
        clean_relative_path(&normalized)?
            .to_str()
            .unwrap_or_default(),
    ))
}

pub(crate) fn storage_write_path(root: &Path, name: &str) -> Result<PathBuf> {
    if name.contains('>') {
        return Err(TjsError::runtime(format!(
            "cannot write to archive-qualified storage: {name}"
        )));
    }
    let normalized = normalize_storage_separators(name);
    let path = Path::new(&normalized);
    if path.is_absolute() {
        if is_safe_absolute_storage_path(path) {
            return Ok(path.to_path_buf());
        }
        return Err(TjsError::runtime(format!(
            "storage path must be safe: {}",
            path.display()
        )));
    }
    Ok(root.join(clean_relative_path(&normalized)?))
}

pub fn storage_mode_offset(mode: &str) -> Option<u64> {
    let offset = mode
        .split('o')
        .nth(1)?
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    (!offset.is_empty()).then(|| offset.parse().ok()).flatten()
}

/// Opens every archive of `root` with `filters` as their shared filter
/// registry (`Xp3ResourceProvider::open_archives_with_options`); the archives
/// keep reading the registry's slots per stream creation and per read, which
/// is what makes a filter installed after this call effective.
pub(crate) fn open_project_archives(
    root: &Path,
    filters: Arc<Xp3FilterRegistry>,
) -> Result<Option<Xp3ResourceProvider>> {
    let archives = project_archive_paths(root);
    if archives.is_empty() {
        return Ok(None);
    }
    Xp3ResourceProvider::open_archives_with_options(
        archives,
        Xp3OpenOptions::default().with_filter_registry(filters),
    )
    .map(Some)
    .map_err(|error| TjsError::runtime(format!("failed to open XP3 archives: {error}")))
}

pub(crate) fn project_archive_paths(root: &Path) -> Vec<PathBuf> {
    let mut archives = xp3_files_in_directory(&root.join("sys"));
    archives.extend(xp3_files_in_directory(root));
    archives
}

fn xp3_files_in_directory(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut archives = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("xp3"))
        })
        .collect::<Vec<_>>();
    archives.sort();
    archives
}

pub(crate) fn clean_relative_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(TjsError::runtime(format!(
            "storage path must be relative: {}",
            path.display()
        )));
    }

    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(TjsError::runtime(format!(
                    "storage path must stay inside project root: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(clean)
}

pub fn io_error(error: io::Error) -> TjsError {
    TjsError::runtime(error.to_string())
}

pub(crate) fn tjs_error_to_io(error: TjsError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
}

fn storage_not_found(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("storage `{name}` not found"),
    )
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use krkr_core::StoragePort;

    use super::*;

    #[test]
    fn normalizes_logical_storage_names_like_krkr() {
        assert_eq!(
            normalize_storage_name(r"foo\\bar/./baz/../qux").unwrap(),
            "foo/bar/qux"
        );
        assert_eq!(
            normalize_storage_name(r"archive.xp3>FOO\\BAR/../Baz").unwrap(),
            "archive.xp3>foo/baz"
        );
        assert!(normalize_storage_name("../escape").is_err());
    }

    /// Ground truth: the compression loop of `TVPNormalizeStorageName`
    /// (`krkrz/src/core/base/StorageIntf.cpp:400-453`, run over the path after
    /// the media/domain split and `tTVPFileMedia::NormalizePathName`,
    /// `win32/StorageImpl.cpp:81-92`). Its only delimiter deletion targets a
    /// delimiter followed by another one (`:409-413`), so a trailing `/`
    /// survives; duplicated interior delimiters collapse; a `..` that lands on
    /// a delimiter keeps that delimiter. The trailing-slash rows were also
    /// reproduced by running that loop verbatim in a C++ harness.
    #[test]
    fn normalization_keeps_the_reference_trailing_delimiter() {
        // A trailing delimiter survives, and a run of them collapses to one.
        assert_eq!(normalize_storage_name("dir/").unwrap(), "dir/");
        assert_eq!(normalize_storage_name("dir").unwrap(), "dir");
        assert_eq!(normalize_storage_name("dir//").unwrap(), "dir/");
        assert_eq!(normalize_storage_name("dir///").unwrap(), "dir/");
        assert_eq!(normalize_storage_name("a/b//").unwrap(), "a/b/");
        assert_eq!(normalize_storage_name("./dir/").unwrap(), "dir/");
        assert_eq!(normalize_storage_name("dir\\").unwrap(), "dir/");
        assert_eq!(normalize_storage_name("dir/./").unwrap(), "dir/");
        // The reference keeps the delimiter a `..` collapses onto, so the
        // parent of the removed segment keeps its trailing slash.
        assert_eq!(normalize_storage_name("a/b/../").unwrap(), "a/");
        assert_eq!(normalize_storage_name("a/b/../c/").unwrap(), "a/c/");
        assert_eq!(normalize_storage_name("/").unwrap(), "/");
        assert_eq!(normalize_storage_name("//").unwrap(), "/");
        // The empty name stays empty (the reference returns it early,
        // `StorageIntf.cpp:262`). A name whose segments all collapse keeps
        // this engine's existing empty spelling of the root instead of
        // inventing the absolute `"/"`; the trailing delimiter is only part
        // of a name that still has a segment to hang it on.
        assert_eq!(normalize_storage_name("").unwrap(), "");
        assert_eq!(normalize_storage_name("./").unwrap(), "");
        // XP3's `>` delimiter: the in-archive part keeps its trailing slash
        // exactly like the outer path (its `NormalizeInArchiveStorageName`,
        // `StorageIntf.cpp:601-642`, also collapses duplicated slashes but
        // keeps the last one).
        assert_eq!(
            normalize_storage_name("archive.xp3>DIR/").unwrap(),
            "archive.xp3>dir/"
        );
        assert_eq!(
            normalize_storage_name("archive.xp3>DIR//").unwrap(),
            "archive.xp3>dir/"
        );
        assert_eq!(
            normalize_storage_name("archive.xp3>").unwrap(),
            "archive.xp3>"
        );
        // `>` is just another delimiter inside the reference's single
        // compression pass over `path + ">" + in-archive name`
        // (`StorageIntf.cpp:392-398`, `:405`), so an outer trailing `/` in
        // front of it is a duplicated delimiter and disappears. Ground truth
        // from the loop run verbatim: "dir/>"→"dir>", "a//>"→"a>",
        // "a/>b/c/"→"a>b/c/", "/>x"→">x".
        assert_eq!(normalize_storage_name("dir/>").unwrap(), "dir>");
        assert_eq!(normalize_storage_name("a//>").unwrap(), "a>");
        assert_eq!(normalize_storage_name("a/>b/c/").unwrap(), "a>b/c/");
        assert_eq!(normalize_storage_name("/>x").unwrap(), ">x");
        // Media-qualified names keep their trailing delimiter too; the media
        // prefix itself is `normalization_keeps_the_media_prefix`'s subject.
        assert_eq!(
            normalize_storage_name("psb://container.psb/inner/").unwrap(),
            "psb://container.psb/inner/"
        );
    }

    /// Ground truth: the media/domain split of `NormalizeStorageName`
    /// (`StorageIntf.cpp:299-354`) and its `media + "://" + domain + path`
    /// reassembly (`:462`). The two slashes are structure, not a path
    /// delimiter, so a media-qualified name keeps them and the normalized
    /// result still dispatches through [`split_media_name`].
    #[test]
    fn normalization_keeps_the_media_prefix() {
        assert_eq!(
            normalize_storage_name("psb://container.psb/inner/").unwrap(),
            "psb://container.psb/inner/"
        );
        assert_eq!(
            normalize_storage_name("psb://container.psb/inner").unwrap(),
            "psb://container.psb/inner"
        );
        // A media-root name: the domain is everything up to the next `/`.
        assert_eq!(
            normalize_storage_name("psb://container.psb/").unwrap(),
            "psb://container.psb/"
        );
        // The reference lower-cases the media name (`:366-374`) and leaves the
        // domain alone; only the path is compacted.
        assert_eq!(
            normalize_storage_name("PSB://container.psb//inner/").unwrap(),
            "psb://container.psb/inner/"
        );
        assert_eq!(
            normalize_storage_name("psb://container.psb/./inner/../").unwrap(),
            "psb://container.psb/"
        );
        // Separators are unified before the media split (`:271-277`).
        assert_eq!(
            normalize_storage_name(r"psb:\\container.psb\inner").unwrap(),
            "psb://container.psb/inner"
        );
        // An explicit `.` domain survives, the way the finding's reference
        // sample `TVPNormalizeStorageName("file://./x")` keeps it.
        assert_eq!(normalize_storage_name("file://./x").unwrap(), "file://./x");
        // `media://name` (a domain with no path) is `TVPInvalidPathName` in the
        // reference (`:341-343`); this engine hands such a name to the
        // registered media, which owns its namespace validation
        // (`crate::media`), so the spelling survives for that hand-off.
        assert_eq!(
            normalize_storage_name("psb://container.psb").unwrap(),
            "psb://container.psb"
        );
        // A media path may not escape its namespace, exactly like a plain one.
        assert!(normalize_storage_name("psb://container.psb/../x").is_err());
        // The archive delimiter is split off first, so the media prefix sits
        // on the outer half and the in-archive half keeps its own rules.
        assert_eq!(
            normalize_storage_name("psb://container.psb/arc.xp3>INNER/").unwrap(),
            "psb://container.psb/arc.xp3>inner/"
        );
        // The `/` in front of `>` is a duplicated delimiter (`:392-398`,
        // `:409-413`) — but the media prefix's `//` is not a path delimiter at
        // all, so an empty name space keeps it.
        assert_eq!(
            normalize_storage_name("psb://container.psb/>inner").unwrap(),
            "psb://container.psb>inner"
        );
        assert_eq!(
            normalize_storage_name("psb://>inner").unwrap(),
            "psb://>inner"
        );
        // Only the explicit `://` spelling is a media name here: it is the one
        // `split_media_name` accepts, so the reference's `media:/path` and
        // `media:path` spellings keep the plain-path compaction.
        assert_eq!(
            normalize_storage_name("psb:/container.psb").unwrap(),
            "psb:/container.psb"
        );
        assert_eq!(normalize_storage_name("psb2://c/x").unwrap(), "psb2:/c/x");
        assert_eq!(normalize_storage_name("://c/x").unwrap(), ":/c/x");
        // The provider dispatch the round trip needs: `getFullPath` output fed
        // back into a `Storages` call still splits into media and namespace.
        let full_path = normalize_storage_name("psb://container.psb/inner/").unwrap();
        assert_eq!(
            split_media_name(&full_path),
            Some(("psb", "container.psb/inner/"))
        );
    }

    #[test]
    fn decode_text_storage_strips_utf8_bom() {
        let bytes = [0xef, 0xbb, 0xbf, b'a', b'b', b'c'];
        assert_eq!(
            decode_text_storage("text.tjs", &bytes, None, "UTF-8").expect("decode"),
            "abc"
        );
    }

    #[test]
    fn remove_auto_path_normalizes_case_and_separators() {
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        storage.add_auto_path(r"Sound\");
        assert!(storage.remove_auto_path("sound/"));
        assert!(storage.auto_paths().is_empty());
    }

    #[test]
    fn storage_candidates_apply_auto_paths_to_xp3_lookups() {
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        storage.add_auto_path("bgimage/");
        storage.add_auto_path("/tmp/game/sys/bgimage.xp3>");

        let candidates = storage.storage_candidates("白").expect("candidates");

        assert!(candidates.iter().any(|candidate| candidate == "白.jpg"));
        assert!(candidates.iter().any(|candidate| candidate == "白.tlg"));
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate == "bgimage/白.jpg")
        );
    }

    /// A media-backed auto path whose scheme has no registered provider keeps
    /// the folder join: the reference would throw `TVPUnsupportedMediaName` out
    /// of its listing (`StorageIntf.cpp:211-222`), and this engine's
    /// unregistered schemes keep resolving through the built-in stack (the
    /// divergence `crate::media` records). Once a provider owns the scheme the
    /// candidate is the placed media name — `crate::media`'s
    /// `media_auto_path_places_a_plain_name_on_the_listing_media` pins that
    /// half.
    #[test]
    fn media_auto_path_candidates_without_a_provider_keep_the_folder_join() {
        let storage = ProjectStorage::new(None, Vec::new(), None, Vec::new());
        storage.add_auto_path("psb://container.psb/");

        let candidates = storage.storage_candidates("inner").expect("candidates");

        assert!(candidates.iter().any(|candidate| candidate == "inner"));
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate == "psb:/container.psb/inner")
        );
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate.starts_with("psb://"))
        );
    }

    #[test]
    fn normalize_auto_path_uses_archive_inner_prefix() {
        assert_eq!(
            normalize_auto_path("/tmp/game/sys/bgimage.xp3>"),
            Some(String::new())
        );
        assert_eq!(
            normalize_auto_path("/tmp/game/sys/bgimage.xp3>patch/"),
            Some("patch/".to_string())
        );
        assert_eq!(
            normalize_auto_path("/tmp/game/bgimage/"),
            Some("bgimage".to_string())
        );
    }

    #[test]
    fn project_archive_paths_include_sys_archives_before_root_archives() {
        let root = temp_root("archives");
        fs::create_dir_all(root.join("sys")).expect("create sys");
        fs::write(root.join("data.xp3"), []).expect("write data archive");
        fs::write(root.join("patch.xp3"), []).expect("write patch archive");
        fs::write(root.join("sys/bgimage.xp3"), []).expect("write bg archive");
        fs::write(root.join("sys/fgimage.XP3"), []).expect("write fg archive");
        fs::write(root.join("sys/readme.txt"), []).expect("write readme");

        let paths = project_archive_paths(&root)
            .into_iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .expect("strip root")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                "sys/bgimage.xp3",
                "sys/fgimage.XP3",
                "data.xp3",
                "patch.xp3"
            ]
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Minimal XP3 container writer for resolver fixtures: magic, one raw
    /// segment per entry, then a raw (uncompressed) index block. Mirrors the
    /// layout `krkr-xp3/src/parse.rs` reads, in the same shape as that crate's
    /// own `build_archive` test fixture.
    fn build_xp3_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        fn push_chunk(output: &mut Vec<u8>, name: &[u8; 4], body: &[u8]) {
            output.extend_from_slice(name);
            output.extend_from_slice(&u64::try_from(body.len()).expect("chunk fits").to_le_bytes());
            output.extend_from_slice(body);
        }

        let mut archive = Vec::new();
        archive.extend_from_slice(&krkr_xp3::XP3_MAGIC);
        let index_pointer_offset = archive.len();
        archive.extend_from_slice(&0u64.to_le_bytes());

        let mut built = Vec::new();
        for (name, data) in entries {
            let offset = archive.len() as u64;
            archive.extend_from_slice(data);
            built.push((*name, offset, data.len() as u64));
        }
        let index_offset = archive.len() as u64;

        let mut index = Vec::new();
        for (name, offset, size) in &built {
            let mut file = Vec::new();
            let mut info = Vec::new();
            info.extend_from_slice(&0u32.to_le_bytes());
            info.extend_from_slice(&size.to_le_bytes());
            info.extend_from_slice(&size.to_le_bytes());
            let units = name.encode_utf16().collect::<Vec<_>>();
            info.extend_from_slice(
                &u16::try_from(units.len())
                    .expect("entry name fits")
                    .to_le_bytes(),
            );
            for unit in units {
                info.extend_from_slice(&unit.to_le_bytes());
            }
            push_chunk(&mut file, b"info", &info);

            let mut segm = Vec::new();
            segm.extend_from_slice(&0u32.to_le_bytes());
            segm.extend_from_slice(&offset.to_le_bytes());
            segm.extend_from_slice(&size.to_le_bytes());
            segm.extend_from_slice(&size.to_le_bytes());
            push_chunk(&mut file, b"segm", &segm);

            push_chunk(&mut file, b"adlr", &0u32.to_le_bytes());
            push_chunk(&mut index, b"File", &file);
        }

        archive.push(0); // raw index block
        archive.extend_from_slice(&(index.len() as u64).to_le_bytes());
        archive.extend_from_slice(&index);
        archive[index_pointer_offset..index_pointer_offset + 8]
            .copy_from_slice(&index_offset.to_le_bytes());
        archive
    }

    /// Two archives that both hold `dup.bin`, plus one member only each. The
    /// mount list is sorted (`a.xp3` before `z.xp3`), so a name-only lookup
    /// falls back to `z.xp3` and any other outcome comes from auto paths.
    fn project_with_two_archives(prefix: &str) -> PathBuf {
        let root = temp_root(prefix);
        fs::create_dir_all(&root).expect("create root");
        fs::write(
            root.join("a.xp3"),
            build_xp3_archive(&[("dup.bin", b"from-a"), ("a-only.bin", b"a-only")]),
        )
        .expect("write a.xp3");
        fs::write(
            root.join("z.xp3"),
            build_xp3_archive(&[("dup.bin", b"from-z"), ("z-only.bin", b"z-only")]),
        )
        .expect("write z.xp3");
        root
    }

    /// Ground truth, krkrz `TVPGetPlacedPath` (`StorageIntf.cpp:1164-1195`):
    /// the current folder is probed first, then the auto path table by
    /// basename. `TVPAddAutoPath` appends to `TVPAutoPathList`
    /// (`StorageIntf.cpp:999-1015`) and `TVPRebuildAutoPathTable` fills
    /// `TVPAutoPathTable` in that order (`:1035-1141`); `tTJSHashTable::Add`
    /// replaces a duplicate key's value (`src/core/tjs2/tjsHashSearch.h`), so
    /// **the last declaration wins**. KAG3's `Initialize.tjs` documents and
    /// relies on it ("later specified is used with higher priority"), adding
    /// `patch.xp3>` after every shipped archive. GINKA's `Initialize.tjs`
    /// does the same, and its boot chain declares `GINKA.xp3>` even later.
    #[test]
    fn auto_path_declaration_order_decides_between_mounts() {
        let root = project_with_two_archives("declared-order");

        // Declaring `z.xp3>` first and `a.xp3>` second: the later `a.xp3>`
        // wins even though the mount list would pick `z.xp3` for a plain name.
        let storage = ProjectStorage::for_root(&root).expect("storage");
        storage.add_auto_path("z.xp3>");
        storage.add_auto_path("a.xp3>");
        assert_eq!(
            storage.read_binary_vec("dup.bin").expect("declared copy"),
            b"from-a".as_slice()
        );
        assert_eq!(
            storage.resolved_storage_name("dup.bin").as_deref(),
            Some("a.xp3>dup.bin")
        );

        // Reversed declaration flips the served copy.
        let reversed = ProjectStorage::for_root(&root).expect("storage");
        reversed.add_auto_path("a.xp3>");
        reversed.add_auto_path("z.xp3>");
        assert_eq!(
            reversed
                .read_binary_vec("dup.bin")
                .expect("later declaration"),
            b"from-z".as_slice()
        );
        assert_eq!(
            reversed.resolved_storage_name("dup.bin").as_deref(),
            Some("z.xp3>dup.bin")
        );

        // Dropping the later declaration restores the other archive's copy;
        // this is the GINKA `title.pbd` probe in miniature.
        reversed.remove_auto_path("z.xp3>");
        assert_eq!(
            reversed.read_binary_vec("dup.bin").expect("restored copy"),
            b"from-a".as_slice()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// A folder auto path and an archive auto path are entries in the same
    /// ordered list, so the later declaration wins regardless of which backend
    /// serves the member. The filesystem-first pass was letting a folder
    /// declared *before* a patch archive shadow the archive's copy, which the
    /// reference's single auto-path table cannot do.
    #[test]
    fn later_declared_archive_beats_an_earlier_folder_auto_path() {
        let root = project_with_two_archives("folder-before-archive");
        fs::create_dir_all(root.join("overlay")).expect("create overlay dir");
        fs::write(root.join("overlay/dup.bin"), b"from-folder").expect("write overlay copy");

        let storage = ProjectStorage::for_root(&root).expect("storage");
        storage.add_auto_path("overlay/");
        storage.add_auto_path("z.xp3>");
        assert_eq!(
            storage.read_binary_vec("dup.bin").expect("archive copy"),
            b"from-z".as_slice()
        );

        // The reverse declaration keeps the loose folder copy in front.
        let folder_last = ProjectStorage::for_root(&root).expect("storage");
        folder_last.add_auto_path("z.xp3>");
        folder_last.add_auto_path("overlay/");
        assert_eq!(
            folder_last.read_binary_vec("dup.bin").expect("folder copy"),
            b"from-folder".as_slice()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// An `archive.xp3>member` name addresses exactly one mount
    /// (`TVPIsExistentStorageNoSearchNoNormalize`, `StorageIntf.cpp:799-830`),
    /// so no auto path can shadow it.
    #[test]
    fn explicit_archive_member_address_stays_pinned() {
        let root = project_with_two_archives("explicit-pin");
        let storage = ProjectStorage::for_root(&root).expect("storage");
        storage.add_auto_path("z.xp3>");
        storage.add_auto_path("a.xp3>");

        assert_eq!(
            storage
                .read_binary_vec("z.xp3>dup.bin")
                .expect("pinned z.xp3"),
            b"from-z".as_slice()
        );
        assert_eq!(
            storage
                .read_binary_vec("a.xp3>dup.bin")
                .expect("pinned a.xp3"),
            b"from-a".as_slice()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Without a declaration the engine still serves archive members; the
    /// mount-wide scan resolves a duplicate through the later mount
    /// (`Xp3ResourceProvider::get_entry` scans the mount list in reverse).
    #[test]
    fn duplicate_member_without_auto_paths_serves_the_later_mount() {
        let root = project_with_two_archives("mount-order");
        let storage = ProjectStorage::for_root(&root).expect("storage");

        assert_eq!(
            storage.read_binary_vec("dup.bin").expect("later mount"),
            b"from-z".as_slice()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Members that only one archive holds keep resolving from it whatever the
    /// auto path order is.
    #[test]
    fn non_duplicate_members_resolve_from_their_only_archive() {
        let root = project_with_two_archives("unique-members");
        let storage = ProjectStorage::for_root(&root).expect("storage");
        storage.add_auto_path("z.xp3>");
        storage.add_auto_path("a.xp3>");

        assert_eq!(
            storage.read_binary_vec("a-only.bin").expect("a member"),
            b"a-only".as_slice()
        );
        assert_eq!(
            storage.read_binary_vec("z-only.bin").expect("z member"),
            b"z-only".as_slice()
        );
        assert_eq!(
            storage.resolved_storage_name("z-only.bin").as_deref(),
            Some("z.xp3>z-only.bin")
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// 纸上的魔法使's page-break bug. `PageBreak` is a plain storage name: the
    /// base `data.xp3` ships `system/PageBreak.png` (a real image) while the
    /// higher-priority `patch3.xp3` carries a root `PageBreak.asd` sidecar, and
    /// `patch3.xp3>` is declared last, so it heads the candidate list.
    ///
    /// The candidate walk is name-major and resolves every spelling before the
    /// next: `patch3.xp3>PageBreak` (the archive pin), `system/PageBreak`
    /// (through the mount-wide scan), `PageBreak.tlg`, ... and it reaches
    /// `system/PageBreak.png` -- found by the mount-wide scan because
    /// `data.xp3` itself is not an auto path -- before the `.asd` spelling's
    /// candidates. Before the fold, the qualified `patch3.xp3>PageBreak.asd`
    /// was resolved in a pass that ran before *any* mount-wide scan, so the
    /// image load got the sidecar and the PNG decoder failed with "The image
    /// format could not be determined".
    #[test]
    fn pagebreak_reaches_the_png_and_not_the_asd_sidecar() {
        let root = temp_root("pagebreak");
        fs::create_dir_all(&root).expect("create root");
        fs::write(
            root.join("data.xp3"),
            build_xp3_archive(&[
                ("system/PageBreak.png", b"png-bytes"),
                ("system/PageBreak.asd", b"base-asd"),
            ]),
        )
        .expect("write data.xp3");
        fs::write(
            root.join("patch3.xp3"),
            build_xp3_archive(&[
                ("PageBreak.asd", b"patch-asd"),
                ("LineBreak.asd", b"line-asd"),
            ]),
        )
        .expect("write patch3.xp3");

        let storage = ProjectStorage::for_root(&root).expect("storage");
        storage.add_auto_path("system/");
        storage.add_auto_path("patch3.xp3>");

        // The image load's suggestions are graphic-only
        // (`TVPInternalLoadGraphic`), and the `.png` suggestion resolves
        // through the `system/` auto path instead of patch3's sidecar.
        assert_eq!(
            storage.resolved_storage_name("PageBreak").as_deref(),
            Some("system/PageBreak.png")
        );
        assert_eq!(
            storage.read_binary_vec("PageBreak").expect("plain read"),
            b"png-bytes".as_slice()
        );
        assert_eq!(
            storage
                .read_image_storage("PageBreak")
                .expect("graphic load")
                .as_bytes()
                .expect("bytes")
                .into_owned(),
            b"png-bytes".as_slice()
        );

        // The sidecar keeps the qualified spelling the reference's auto-path
        // table reports (`TVPGetPlacedPath`, `StorageIntf.cpp:1160-1195`): the
        // table maps a basename to the prefix of the auto path that carries it,
        // and the latest declaration replaces an earlier one.
        assert_eq!(
            storage.resolved_storage_name("PageBreak.asd").as_deref(),
            Some("patch3.xp3>PageBreak.asd")
        );
        assert_eq!(
            storage.read_binary_vec("PageBreak.asd").expect("sidecar"),
            b"patch-asd".as_slice()
        );
        assert_eq!(
            storage.resolved_storage_name("LineBreak.asd").as_deref(),
            Some("patch3.xp3>LineBreak.asd")
        );

        // An explicitly spelled member of the base archive reaches the same
        // resource through the mount-wide scan.
        assert_eq!(
            storage.resolved_storage_name("PageBreak.png").as_deref(),
            Some("system/PageBreak.png")
        );
        assert_eq!(
            storage
                .resolved_storage_name("system/PageBreak.png")
                .as_deref(),
            Some("system/PageBreak.png")
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The fold's deliberate reordering across spellings: an earlier suggested
    /// spelling's mount hit beats a later spelling's filesystem hit. The
    /// graphic loader probes one suggested name at a time with a full storage
    /// lookup and takes the first that exists (`TVPInternalLoadGraphic`,
    /// `visual/GraphicsLoaderIntf.cpp:1478-1506`), so `hero.png` inside an
    /// archive wins over a loose `hero.jpg` even though every filesystem
    /// candidate used to be resolved in a pass that ran before any XP3 name
    /// scan.
    #[test]
    fn completion_resolves_one_spelling_before_the_next() {
        let root = temp_root("spelling-order");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("hero.jpg"), b"jpg-on-disk").expect("write loose file");
        fs::write(
            root.join("data.xp3"),
            build_xp3_archive(&[("hero.png", b"png-in-archive")]),
        )
        .expect("write data.xp3");
        let storage = ProjectStorage::for_root(&root).expect("storage");
        storage.add_auto_path("bgimage/");

        assert_eq!(
            storage.read_binary_vec("hero").expect("image load"),
            b"png-in-archive".as_slice()
        );
        assert_eq!(
            storage.resolved_storage_name("hero").as_deref(),
            Some("hero.png")
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The XP3 filter seam end to end at the storage level: a host installs an
    /// extraction filter *after* the project (and therefore its archives) was
    /// built, and the bytes a later read returns go through it — including
    /// when the same entry was already read (and cached) before the install.
    ///
    /// Reference timing: the extraction callback is read per chunk of every
    /// read (`XP3Archive.cpp:1047`), so nothing about the mount's age can
    /// freeze it; `xp3filter.dll` installs it from a post-registration step,
    /// after the game's archives are open.
    #[test]
    fn an_extraction_filter_installed_after_the_mount_filters_later_reads() {
        let root = temp_root("xp3-filter-install");
        fs::create_dir_all(&root).expect("create root");
        fs::write(
            root.join("data.xp3"),
            build_xp3_archive(&[("secret/data.bin", b"cipher-me")]),
        )
        .expect("write data.xp3");

        let storage = ProjectStorage::for_root(&root).expect("storage");
        // Read once before any filter is installed: the raw cache now holds
        // the unfiltered bytes for this entry.
        assert_eq!(
            storage
                .read_binary_vec("secret/data.bin")
                .expect("plain read"),
            b"cipher-me".as_slice()
        );

        let registry = storage.xp3_filter_registry();
        registry.set_extraction_filter(Some(std::sync::Arc::new(
            |info: krkr_core::Xp3ExtractionFilterInfo<'_>,
             _ctx: &mut krkr_core::Xp3FilterContext| {
                assert_eq!(info.file_name, "secret/data.bin");
                for byte in info.buffer.iter_mut() {
                    *byte ^= 0xff;
                }
            },
        )));

        let expected: Vec<u8> = b"cipher-me".iter().map(|byte| byte ^ 0xff).collect();
        assert_eq!(
            storage
                .read_binary_vec("secret/data.bin")
                .expect("filtered read"),
            expected.as_slice(),
            "the cached pre-filter bytes must not be served after the install"
        );

        // The port hands the same registry to a plugin; a storage with no
        // archives answers `None` instead.
        let port: &dyn krkr_core::ProjectStoragePort = &storage;
        assert!(port.xp3_filter_registry().is_some());

        let memory = ProjectStorage::from_memory([("a.txt", b"a".to_vec())]);
        let port: &dyn krkr_core::ProjectStoragePort = &memory;
        assert!(port.xp3_filter_registry().is_none());

        registry.set_extraction_filter(None);
        assert_eq!(
            storage
                .read_binary_vec("secret/data.bin")
                .expect("plain read again"),
            b"cipher-me".as_slice()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn filesystem_storage_lookup_falls_back_to_case_insensitive_match() {
        let root = temp_root("case");
        fs::create_dir_all(root.join("patch")).expect("create patch");
        fs::write(root.join("patch/sc_title_bt_Gallery.png"), b"gallery")
            .expect("write mixed-case resource");

        let storage = ProjectStorage::for_root(&root).expect("storage");

        assert_eq!(
            storage
                .read_binary_vec("sc_title_bt_GALLERY.png")
                .expect("read mixed-case resource"),
            b"gallery"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn exact_storage_lookup_accepts_safe_absolute_project_files() {
        let root = temp_root("absolute");
        fs::create_dir_all(&root).expect("create root");
        let archive = root.join("patch_append1.xp3");
        fs::write(&archive, b"archive").expect("write archive");
        let storage =
            ProjectStorage::new(Some(root.clone()), project_layers(&root), None, Vec::new());

        assert!(
            storage.storage_exists_exact(
                archive
                    .to_str()
                    .expect("temporary path must be valid UTF-8")
            )
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn resource_provider_reads_mmap_backed_data_and_streams_files() {
        let root = temp_root("data");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("hello.bin"), b"hello").expect("write fixture");
        let storage = ProjectStorage::for_root(&root).expect("storage");

        let data = storage.data("hello.bin").expect("load data");
        assert_eq!(data.as_bytes().expect("bytes").as_ref(), b"hello");

        let mut stream = data.open_stream().expect("open data stream");
        let mut text = String::new();
        stream.read_to_string(&mut text).expect("read stream");
        assert_eq!(text, "hello");

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn memory_storage_serves_case_insensitive_candidates() {
        let storage = ProjectStorage::from_memory([("startup.ks", b"WEB".to_vec())]);
        assert!(storage.storage_exists("STARTUP.KS"));
        assert_eq!(
            storage.read_binary_vec("startup.ks").expect("memory data"),
            b"WEB"
        );
    }

    #[test]
    fn memory_catalog_lists_deferred_files_and_directories() {
        let storage = ProjectStorage::from_memory_with_catalog(
            [("startup.tjs", b"WEB".to_vec())],
            [
                "append/vol1/start.ks",
                "append/vol1/voice.ogg",
                "main/title.ks",
            ],
        );
        assert!(storage.is_directory("append/"));
        assert!(storage.is_directory("APPEND/VOL1/"));
        assert_eq!(
            storage.list_directory("append/").expect("append directory"),
            vec!["vol1/".to_string()]
        );
        assert_eq!(
            storage
                .list_directory("append/vol1/")
                .expect("volume directory"),
            vec!["start.ks".to_string(), "voice.ogg".to_string()]
        );
        assert_eq!(
            storage.list_directory("/").expect("root directory"),
            vec![
                "append/".to_string(),
                "main/".to_string(),
                "startup.tjs".to_string()
            ]
        );
    }

    #[test]
    fn catalog_resolves_unique_explicit_basename_without_stem_guessing() {
        let storage = ProjectStorage::from_memory_with_catalog(
            [("fgimage/portrait.txt", b"portrait".to_vec())],
            ["fgimage/portrait.txt", "fgimage/other.bin"],
        );

        assert!(storage.storage_exists_exact("portrait.txt"));
        assert_eq!(
            storage
                .read_binary_vec("portrait.txt")
                .expect("catalog alias"),
            b"portrait"
        );
        assert!(!storage.storage_exists_exact("portrait"));

        let ambiguous = ProjectStorage::from_memory_with_catalog(
            [
                ("fgimage/portrait.txt", b"one".to_vec()),
                ("bgimage/portrait.txt", b"two".to_vec()),
            ],
            ["fgimage/portrait.txt", "bgimage/portrait.txt"],
        );
        assert!(!ambiguous.storage_exists_exact("portrait.txt"));
        assert!(ambiguous.read_binary_vec("portrait.txt").is_err());
    }

    /// The catalogue index answers exactly like the scan it replaced: full
    /// names compare case-insensitively, a qualified name never falls back to
    /// the basename rule, and an explicitly extended bare name resolves only
    /// while exactly one entry claims that basename.
    #[test]
    fn catalog_index_answers_the_documented_matching_rules() {
        let storage = ProjectStorage::from_memory_with_catalog(
            Vec::<(String, Vec<u8>)>::new(),
            ["Append/Vol1/Only.KS", "append/vol1/other.bin"],
        );

        assert!(storage.catalog_contains("append/vol1/only.ks"));
        assert!(storage.catalog_contains("APPEND/VOL1/ONLY.KS"));
        assert!(storage.catalog_contains("append\\vol1\\only.ks"));
        assert!(storage.catalog_contains("Only.KS"));
        assert!(storage.catalog_contains("only.KS"));
        assert!(storage.storage_exists_exact("only.ks"));

        assert!(!storage.catalog_contains("append/vol1/only.tjs"));
        assert!(!storage.catalog_contains("only"));
        assert!(!storage.catalog_contains("only.tjs"));
        assert!(!storage.catalog_contains("missing/only.ks"));

        storage.add_catalog_paths(["append/vol2/ONLY.ks"]);
        assert!(!storage.catalog_contains("only.ks"));
        assert!(!storage.storage_exists_exact("only.ks"));
        assert!(storage.catalog_contains("append/vol1/only.ks"));
        assert!(storage.catalog_contains("append/vol2/only.ks"));
    }

    /// Every catalogue write rebuilds the index: adding an entry makes a bare
    /// name ambiguous at once, replacing the catalogue can make it unique
    /// again, and a memory insert announces its file the same way.
    #[test]
    fn catalog_index_follows_catalogue_writes() {
        let storage = ProjectStorage::from_memory_with_catalog(
            Vec::<(String, Vec<u8>)>::new(),
            ["append/vol1/only.ks"],
        );
        assert!(storage.catalog_contains("only.ks"));
        assert!(storage.catalog_contains_for_load("only.ks"));

        storage.add_catalog_paths(["append/vol2/only.ks"]);
        assert!(!storage.catalog_contains("only.ks"));
        assert!(!storage.catalog_contains_for_load("only.ks"));

        storage.set_catalog_paths(["append/vol2/only.ks"]);
        assert!(storage.catalog_contains("only.ks"));
        assert!(storage.catalog_contains_for_load("ONLY.KS"));
        assert!(storage.catalog_contains("append/vol2/only.ks"));
        assert!(!storage.catalog_contains("append/vol1/only.ks"));

        storage.set_catalog_paths(Vec::<String>::new());
        assert!(!storage.catalog_contains("only.ks"));
        assert!(!storage.catalog_contains("append/vol2/only.ks"));

        storage.insert_memory("append/vol3/only.ks", b"bytes".to_vec());
        assert!(storage.catalog_contains("only.ks"));
        assert!(storage.catalog_contains("APPEND/VOL3/ONLY.KS"));
    }

    /// The index is an optimisation of the catalogue scan, not a redefinition
    /// of it: across a deterministic corpus of catalogues and probes, the
    /// indexed answers equal the brute-force scan they replaced — including
    /// spelled-out case variants, mixed separators and duplicate basenames.
    #[test]
    fn catalog_index_agrees_with_the_reference_scan() {
        fn scan_contains(catalog: &BTreeMap<String, String>, name: &str) -> bool {
            let normalized = normalize_storage_separators(name);
            if catalog
                .values()
                .any(|path| path.eq_ignore_ascii_case(&normalized))
            {
                return true;
            }
            if normalized.contains('/') || !normalized.contains('.') {
                return false;
            }
            let mut matches = catalog.values().filter(|path| {
                path.rsplit('/')
                    .next()
                    .is_some_and(|file| file.eq_ignore_ascii_case(&normalized))
            });
            matches.next().is_some() && matches.next().is_none()
        }

        fn scan_unique_basename(catalog: &BTreeMap<String, String>, name: &str) -> Option<String> {
            let normalized = normalize_storage_separators(name);
            if normalized.contains('/') || !normalized.contains('.') {
                return None;
            }
            let mut matched = None;
            for path in catalog.values() {
                if path
                    .rsplit('/')
                    .next()
                    .is_some_and(|file| file.eq_ignore_ascii_case(&normalized))
                {
                    if matched.is_some() {
                        return None;
                    }
                    matched = Some(path.clone());
                }
            }
            matched
        }

        let segments = ["data", "Data", "sys", "bgimage", "fgimage", "vol1", "Vol2"];
        let stems = ["only", "Only", "config", "title"];
        let suffix = ["ks", "KS", "tjs", "png", "noext"];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut random = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };

        for case in 0..64 {
            let mut catalog = BTreeMap::new();
            for _ in 0..random() % 8 + 1 {
                let name = format!(
                    "{}/{}_{}.{}",
                    segments[random() % segments.len()],
                    stems[random() % stems.len()],
                    random() % 3,
                    suffix[random() % suffix.len()]
                );
                if let Some(key) = catalog_path(&name) {
                    catalog.entry(key).or_insert(name);
                }
            }
            let storage = ProjectStorage::from_memory_with_catalog(
                Vec::<(String, Vec<u8>)>::new(),
                catalog.values().cloned(),
            );
            assert_eq!(
                *storage.inner.catalog_paths.read().expect("catalogue"),
                catalog,
                "case {case} catalogued a different set of names"
            );

            for _ in 0..16 {
                let dir = segments[random() % segments.len()];
                let stem = stems[random() % stems.len()];
                let extension = suffix[random() % suffix.len()];
                let probes = [
                    format!("{dir}/{stem}_{}.{extension}", random() % 3),
                    format!("{stem}_{}.{extension}", random() % 3),
                    stem.to_string(),
                    format!("{stem}.{extension}"),
                    format!("{dir}/{stem}.{extension}").replace('/', "\\"),
                    format!("{dir}//{stem}_{}.{extension}", random() % 3),
                ];
                for probe in probes {
                    assert_eq!(
                        storage.catalog_contains(&probe),
                        scan_contains(&catalog, &probe),
                        "case {case} probe {probe:?}"
                    );
                    assert_eq!(
                        storage.catalog_alias(&probe),
                        scan_unique_basename(&catalog, &probe),
                        "case {case} probe {probe:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn catalog_resolves_extensionless_name_for_loads_only() {
        // A translation overlay publishes `backlog_base.png` at the package
        // root while the base archive keeps `image/backlog_base.png`. An
        // extensionless *load* must see the deferred resource so the Web host
        // can fetch it, while the exact probe stays exact like
        // `Storages.isExistentStorage`.
        let storage = ProjectStorage::from_memory_with_catalog(
            [("image/backlog_base.png", b"image".to_vec())],
            ["backlog_base.png", "image/backlog_base.png"],
        );
        assert!(!storage.catalog_contains("backlog_base"));
        assert!(storage.catalog_contains_for_load("backlog_base"));
        assert!(storage.storage_exists("backlog_base"));

        let nested_only = ProjectStorage::from_memory_with_catalog(
            [("image/title.png", b"title".to_vec())],
            ["image/title.png"],
        );
        assert!(!nested_only.catalog_contains_for_load("title"));
        nested_only.add_auto_path("image/");
        assert!(nested_only.catalog_contains_for_load("title"));
    }

    #[test]
    fn resolved_storage_name_follows_the_normal_resolver() {
        let catalog = ProjectStorage::from_memory_with_catalog(
            [("fgimage/portrait.txt", b"portrait".to_vec())],
            ["fgimage/portrait.txt", "fgimage/other.bin"],
        );
        assert_eq!(
            catalog.resolved_storage_name("portrait.txt").as_deref(),
            Some("portrait.txt")
        );
        assert!(catalog.resolved_storage_name("portrait").is_none());

        let memory = ProjectStorage::from_memory([("main/Config.tjs", b"cfg".to_vec())]);
        assert_eq!(
            memory.resolved_storage_name("Config.tjs").as_deref(),
            Some("Config.tjs")
        );
        assert_eq!(
            memory
                .read_binary_vec("Config.tjs")
                .expect("unique basename"),
            b"cfg"
        );

        let root = temp_root("placed");
        fs::create_dir_all(root.join("bgimage")).expect("create dir");
        fs::write(root.join("bgimage/sky.jpg"), b"sky").expect("write image");
        let fs_storage = ProjectStorage::new(
            Some(root.clone()),
            project_layers(&root),
            None,
            vec!["bgimage/".to_string()],
        );
        assert_eq!(
            fs_storage.resolved_storage_name("sky").as_deref(),
            Some("bgimage/sky.jpg")
        );
        assert!(fs_storage.read_binary_vec("sky").is_ok());
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The completion rule every load path (`Storages.open`, `read_data`) and
    /// `Storages.getPlacedPath` applies when the name carries no known
    /// extension. KRKR's own `TVPGetPlacedPath` does not complete names at all
    /// (`krkrz/base/StorageIntf.cpp:1153-1197` probes the name as given and
    /// then the auto-path table), so this list is the engine-side equivalent
    /// of the graphic loader's extension suggestion
    /// (`TVPInternalLoadGraphic`, `krkrz/visual/GraphicsLoaderIntf.cpp:1480-1503`,
    /// which walks the registered graphic handlers). The order decides what a
    /// bare stem resolves to, so the list is pinned here as a fixture instead
    /// of drifting with the next format that gets added.
    #[test]
    fn completion_offers_the_known_extensions_in_order() {
        let storage = ProjectStorage::from_memory(std::iter::empty::<(&str, Vec<u8>)>());
        // The layer directories are auto paths of their own, so the raw
        // candidate list interleaves them; the completion rule itself is the
        // sequence of directory-free spellings.
        let stems: Vec<String> = storage
            .storage_candidates("hero")
            .expect("candidates")
            .into_iter()
            .filter(|candidate| !candidate.contains('/'))
            .collect();
        assert_eq!(
            stems,
            [
                "hero",
                "hero.tlg",
                "hero.png",
                "hero.jpg",
                "hero.jpeg",
                "hero.bmp",
                "hero.webp",
                "hero.ks",
                "hero.tjs",
                "hero.asd",
                "hero.ogg",
                "hero.wav",
                "hero.tcw",
                "hero.mpg",
                "hero.mpeg",
            ]
            .map(str::to_string)
        );

        // With an auto path, each spelling is followed by its auto-path
        // candidate: the reference resolves one name against every declared
        // path before trying the next name. Auto paths are searched in
        // reverse declaration order, so the freshly added one comes first.
        storage.add_auto_path("fgimage/");
        let candidates = storage.storage_candidates("hero").expect("candidates");
        assert_eq!(
            candidates.into_iter().take(2).collect::<Vec<_>>(),
            ["hero", "fgimage/hero"].map(str::to_string)
        );
    }

    /// Two candidates on disk: the first extension in the list wins, and a
    /// name that already carries a known extension is never re-extended.
    #[test]
    fn completion_picks_the_first_file_and_leaves_extended_names_alone() {
        let storage = ProjectStorage::from_memory([
            ("hero.png", b"png".to_vec()),
            ("hero.tlg", b"tlg".to_vec()),
        ]);
        assert_eq!(
            storage.resolved_storage_name("hero").as_deref(),
            Some("hero.tlg")
        );
        assert_eq!(
            storage
                .read_binary_vec("hero")
                .expect("first candidate wins"),
            b"tlg"
        );

        // `hero.png` names one file: the known extension stops completion, so
        // the `.tlg` sibling is not offered even though it exists.
        let candidates = storage.storage_candidates("hero.png").expect("candidates");
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate.ends_with(".tlg")),
            "{candidates:?}"
        );
        assert_eq!(
            candidates
                .into_iter()
                .filter(|candidate| !candidate.contains('/'))
                .collect::<Vec<_>>(),
            ["hero.png".to_string()]
        );
        assert_eq!(
            storage.resolved_storage_name("hero.png").as_deref(),
            Some("hero.png")
        );
        assert!(storage.read_binary_vec("hero.png").is_ok());
    }

    /// An image load suggests only the extensions with a registered graphic
    /// handler -- `TVPInternalLoadGraphic` walks the graphic handler table and
    /// takes the first `name + extension` that exists
    /// (`visual/GraphicsLoaderIntf.cpp:1478-1506`), and the candidate list here
    /// is that rule's engine-side equivalent. Non-image loads keep the full
    /// storage extension set, because their completion answers a script's
    /// plain-name probe rather than a decoder's.
    #[test]
    fn image_load_completion_only_suggests_graphic_extensions() {
        let image = ProjectStorage::from_memory([("hero.png", b"png".to_vec())]);
        assert_eq!(storage_image_bytes(&image, "hero"), b"png".as_slice());

        // The image suggestion list is the graphic prefix of the storage list,
        // so a stem that only has a non-graphic same-stem file must miss for a
        // graphic load while the plain read still resolves it.
        let sidecar = ProjectStorage::from_memory([("hero.asd", b"sidecar".to_vec())]);
        assert_eq!(
            sidecar.read_binary_vec("hero").expect("plain read"),
            b"sidecar".as_slice()
        );
        assert!(sidecar.resolved_storage_name("hero").as_deref() == Some("hero.asd"));
        assert!(
            sidecar.read_image_storage("hero").is_err(),
            "an image load must not complete `hero` with `hero.asd`"
        );

        // An explicit non-graphic extension is treated like no extension for a
        // graphic load (the graphic set is the known set), so the exact name is
        // still probed first: `PageBreak.asd` requested as an image resolves to
        // the sidecar and fails in the decoder, the way the reference rejects
        // the name with `TVPUnknownGraphicFormat`.
        assert_eq!(
            storage_image_bytes(&sidecar, "hero.asd"),
            b"sidecar".as_slice()
        );

        // The completion itself, spelling for spelling.
        let image_candidates = image
            .storage_candidates_for_kind("hero2", StorageLoadKind::Image)
            .expect("image candidates");
        assert_eq!(
            image_candidates
                .into_iter()
                .filter(|candidate| !candidate.contains('/'))
                .collect::<Vec<_>>(),
            [
                "hero2",
                "hero2.tlg",
                "hero2.png",
                "hero2.jpg",
                "hero2.jpeg",
                "hero2.bmp",
                "hero2.webp"
            ]
            .map(str::to_string)
        );
        assert_eq!(
            image
                .storage_candidates("hero2")
                .expect("generic candidates")
                .into_iter()
                .filter(|candidate| !candidate.contains('/'))
                .collect::<Vec<_>>(),
            [
                "hero2",
                "hero2.tlg",
                "hero2.png",
                "hero2.jpg",
                "hero2.jpeg",
                "hero2.bmp",
                "hero2.webp",
                "hero2.ks",
                "hero2.tjs",
                "hero2.asd",
                "hero2.ogg",
                "hero2.wav",
                "hero2.tcw",
                "hero2.mpg",
                "hero2.mpeg"
            ]
            .map(str::to_string)
        );

        // An explicitly graphic name is never re-extended for either kind.
        let explicit = image
            .storage_candidates_for_kind("hero.png", StorageLoadKind::Image)
            .expect("image candidates");
        assert_eq!(
            explicit
                .into_iter()
                .filter(|candidate| !candidate.contains('/'))
                .collect::<Vec<_>>(),
            ["hero.png".to_string()]
        );
    }

    /// `ProjectStorage::read_image_storage` bytes for a fixture.
    fn storage_image_bytes(storage: &ProjectStorage, name: &str) -> Vec<u8> {
        storage
            .read_image_storage(name)
            .expect("image load")
            .as_bytes()
            .expect("image bytes")
            .into_owned()
    }

    /// The pathological spellings a script can pass. They must stay
    /// deterministic: an empty or dot-terminated name is probed as-is first
    /// and only then extended, an unknown extension is treated like no
    /// extension at all, and case is honoured by the case-insensitive layer
    /// lookup without changing the returned spelling.
    #[test]
    fn completion_handles_pathological_names() {
        let storage = ProjectStorage::from_memory([("hero.tlg", b"tlg".to_vec())]);

        // Empty name: the reference returns "" early (`StorageIntf.cpp:262`
        // for normalization) and `TVPGetPlacedPath` reports nothing.
        assert!(storage.resolved_storage_name("").is_none());
        let empty: Vec<String> = storage
            .storage_candidates("")
            .expect("candidates")
            .into_iter()
            .filter(|candidate| !candidate.contains('/'))
            .collect();
        assert_eq!(empty[..2], ["", ".tlg"].map(str::to_string));

        // A trailing dot is not a known extension, so the stem is used
        // verbatim (`hero.`) and completion appends to the whole name.
        let dotted: Vec<String> = storage
            .storage_candidates("hero.")
            .expect("candidates")
            .into_iter()
            .filter(|candidate| !candidate.contains('/'))
            .collect();
        assert_eq!(dotted[..2], ["hero.", "hero..tlg"].map(str::to_string));
        assert!(storage.resolved_storage_name("hero.").is_none());

        // An unknown extension completes like a bare stem: `hero.xyz` first,
        // then `hero.xyz.tlg`, so a script's made-up suffix can still reach a
        // real file only when that exact name exists.
        let unknown: Vec<String> = storage
            .storage_candidates("hero.xyz")
            .expect("candidates")
            .into_iter()
            .filter(|candidate| !candidate.contains('/'))
            .collect();
        assert_eq!(
            unknown[..2],
            ["hero.xyz", "hero.xyz.tlg"].map(str::to_string)
        );
        assert!(storage.resolved_storage_name("hero.xyz").is_none());

        // Case: completion is suffix-based, and the lookup below it is
        // case-insensitive, so an upper-case spelling resolves and reports
        // the spelling the script asked for (the reference's placed path is
        // likewise the normalized request, not the on-disk case).
        assert_eq!(
            storage.resolved_storage_name("HERO.TLG").as_deref(),
            Some("HERO.TLG")
        );
        assert_eq!(
            storage.resolved_storage_name("HERO").as_deref(),
            Some("HERO.tlg")
        );
        assert_eq!(storage.read_binary_vec("HERO.TLG").expect("case"), b"tlg");
    }

    /// KAGEX resolves stand definitions itself: `system/Utils.tjs:373`
    /// `getExistFileNameAutoExtFill` appends the caller's extension list and
    /// probes `Storages.isExistentStorage`, and `system/AffineSource.tjs:667`
    /// `findAffineSource` then classifies the source by the extension it sees
    /// (`.stand`/`.sinfo`/`.event` -> `AffineSourceStand`,
    /// `system/AffineSourceStand.tjs:1554-1556`). The engine therefore has to
    /// resolve the exact `foo.stand` name through the auto path, and must not
    /// answer a bare stem with its `.stand` sibling: the game's own probe
    /// order (`getPlacedPath(stem)` then `getPlacedPath(stem + ".stand")`)
    /// and `findAffineSource`'s fallback depend on the miss, and the
    /// reference's `TVPGetPlacedPath` reports nothing for the stem either.
    #[test]
    fn kagex_stand_and_sinfo_names_resolve_exactly_and_a_bare_stem_stays_missing() {
        let storage = ProjectStorage::from_memory([
            ("fgimage/アカリ.stand", b"stand definition".to_vec()),
            ("fgimage/info/アカリＡ.sinfo", b"#face info".to_vec()),
        ]);
        storage.add_auto_path("fgimage/");
        storage.add_auto_path("fgimage/info/");

        assert_eq!(
            storage.resolved_storage_name("アカリ.stand").as_deref(),
            Some("fgimage/アカリ.stand")
        );
        assert_eq!(
            storage
                .read_binary_vec("アカリ.stand")
                .expect("the stand definition loads"),
            b"stand definition"
        );
        assert!(storage.storage_exists_exact("アカリＡ.sinfo"));
        assert_eq!(
            storage
                .read_binary_vec("アカリＡ.sinfo")
                .expect("the face info loads"),
            b"#face info"
        );

        assert!(storage.resolved_storage_name("アカリ").is_none());
        assert!(!storage.storage_exists_exact("アカリ"));
        assert!(storage.read_binary_vec("アカリ").is_err());
    }

    #[test]
    fn catalogue_changes_invalidate_resolver_revision() {
        let storage = ProjectStorage::from_memory([("startup.tjs", b"WEB".to_vec())]);
        let before = storage.revision();
        storage.add_catalog_paths(["deferred.ks"]);
        assert!(storage.revision() > before);
        let before_clear = storage.revision();
        storage.clear_archive_cache().expect("clear cache");
        assert!(storage.revision() > before_clear);
    }

    /// KRKR only drops decoded graphics on `System.clearGraphicCache`, a
    /// compact event or an out-of-memory retry; writing a save file leaves
    /// the graphic cache untouched. The name lookup still has to notice the
    /// new file, so `revision` moves while `graphic_revision` stays put.
    #[test]
    fn storage_writes_keep_the_graphic_revision_but_move_the_lookup_revision() {
        let storage = ProjectStorage::from_memory([("startup.tjs", b"WEB".to_vec())]);
        let lookup = storage.revision();
        let graphic = storage.graphic_revision();

        storage
            .write_binary_storage("savedata/slot0.ksd", "w", b"save")
            .expect("write save");

        assert!(storage.revision() > lookup);
        assert_eq!(storage.graphic_revision(), graphic);

        storage.add_auto_path("bgimage/");
        assert!(storage.graphic_revision() > graphic);
    }

    #[test]
    fn storage_writes_normalize_backslashes_and_reject_archive_members() {
        let storage = ProjectStorage::from_memory(std::iter::empty::<(&str, Vec<u8>)>());
        storage
            .write_binary_storage("saved\\state.bin", "w", b"ok")
            .expect("normalized memory write");
        assert_eq!(
            storage
                .read_binary_vec("saved/state.bin")
                .expect("read normalized"),
            b"ok"
        );
        assert!(
            storage
                .write_binary_storage("..\\outside.bin", "w", b"bad")
                .is_err()
        );
        assert!(
            storage
                .write_binary_storage("data.xp3>member.bin", "w", b"bad")
                .is_err()
        );
    }

    #[test]
    fn memory_storage_accepts_writes_and_exposes_a_journal() {
        let storage = ProjectStorage::from_memory([("savedata/state.bin", b"old".to_vec())]);
        storage
            .write_binary_storage("savedata/state.bin", "o1", b"X")
            .expect("write memory data");
        assert_eq!(
            storage
                .read_binary_vec("savedata/state.bin")
                .expect("read memory data"),
            b"oXd".as_slice()
        );
        let writes = storage.drain_memory_writes();
        assert_eq!(
            writes,
            vec![("savedata/state.bin".to_string(), b"oXd".to_vec())]
        );
        assert!(storage.drain_memory_writes().is_empty());
    }

    #[test]
    fn external_memory_cache_evicts_oldest_entry_and_tracks_bytes() {
        let mut cache = ExternalMemoryCache::with_capacity(5);
        cache.touch("first.bin", 3);
        cache.touch("second.bin", 2);
        assert_eq!(cache.bytes, 5);
        cache.touch("third.bin", 2);
        assert_eq!(cache.bytes, 7);
        assert_eq!(
            cache.pop_lru_if_over_capacity().as_deref(),
            Some("first.bin")
        );
        assert_eq!(cache.bytes, 4);
        assert!(!cache.entries.contains_key("first.bin"));
        assert_eq!(cache.lru, vec!["second.bin", "third.bin"]);

        // Reads move a resident resource to the MRU end before the next
        // eviction, preserving useful assets under a bounded cache.
        cache.lru.retain(|path| path != "second.bin");
        cache.lru.push_back("second.bin".to_string());
        cache.touch("fourth.bin", 3);
        assert_eq!(
            cache.pop_lru_if_over_capacity().as_deref(),
            Some("third.bin")
        );
        assert_eq!(cache.bytes, 5);
        assert!(cache.entries.contains_key("second.bin"));
        assert!(cache.entries.contains_key("fourth.bin"));

        // An individual resource larger than the budget is retained only
        // until its first read, then released without blocking the retry.
        let mut large = ExternalMemoryCache::with_capacity(5);
        large.touch("movie.bin", 8);
        assert!(large.pop_lru_if_over_capacity().is_none());
        assert!(large.finish_read("MOVIE.BIN"));
        assert_eq!(large.bytes, 0);
        assert!(large.entries.is_empty());
    }

    fn temp_root(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "Kirakira-engine-storage-{prefix}-{}-{nanos}",
            std::process::id()
        ))
    }
}
