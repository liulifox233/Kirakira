use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, VecDeque},
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
use krkr_core::{ResourceData, ResourceDataSource, ResourceStream, StoragePort};
use krkr_tjs2::{Result, TjsError};
use krkr_xp3::Xp3ResourceProvider;
use memmap2::{Mmap, MmapOptions};

const RAW_CACHE_CAPACITY_BYTES: usize = 64 * 1024 * 1024;
const RAW_CACHE_MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;

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
    case_insensitive_dir_cache: Mutex<HashMap<PathBuf, HashMap<String, PathBuf>>>,
    raw_cache: Mutex<RawDataCache>,
    xp3_provider: Option<Xp3ResourceProvider>,
    memory_files: RwLock<BTreeMap<String, Arc<[u8]>>>,
    /// Logical files known to a publication even when their bytes have not
    /// been fetched yet.  Browser packages populate this from manifest.json;
    /// native views also keep memory writes here so directory enumeration has
    /// one backend-neutral source of truth.
    catalog_paths: RwLock<BTreeMap<String, String>>,
    /// Files written through a memory-backed storage view since the last
    /// drain. Browser hosts persist this journal in their own origin storage;
    /// native filesystem-backed views never use it.
    memory_writes: Mutex<BTreeMap<String, Arc<[u8]>>>,
    auto_paths: RwLock<Vec<String>>,
    revision: AtomicU64,
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectLayer {
    root: PathBuf,
    encoding_hint: Option<&'static Encoding>,
}

#[derive(Clone)]
pub struct StorageData {
    pub storage_name: String,
    pub data: ResourceData,
    pub encoding_hint: Option<&'static Encoding>,
}

#[derive(Clone, Debug)]
enum LocatedResource {
    Fs {
        storage_name: String,
        path: PathBuf,
        encoding_hint: Option<&'static Encoding>,
        byte_len: u64,
    },
    Xp3 {
        storage_name: String,
        entry_name: String,
        byte_len: u64,
    },
    Memory {
        storage_name: String,
        data: Arc<[u8]>,
        encoding_hint: Option<&'static Encoding>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RawCacheKey {
    revision: u64,
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

struct MmapResourceData {
    mmap: Arc<Mmap>,
}

struct MmapResourceStream {
    mmap: Arc<Mmap>,
    position: u64,
}

impl ProjectStorage {
    pub fn package_mount(&self) -> PackageMount {
        PackageMount(self.clone())
    }
    pub fn for_root(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let fs_layers = project_layers(&root);
        let xp3_provider = open_project_archives(&root)?;
        Ok(Self::new(Some(root), fs_layers, xp3_provider, Vec::new()))
    }

    pub(crate) fn new(
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
        let catalog_paths = memory_files
            .keys()
            .filter_map(|path| catalog_path(path).map(|key| (key, path.clone())))
            .collect();
        Self {
            inner: Arc::new(ProjectStorageInner {
                root,
                fs_layers,
                lookup_cache: Mutex::new(HashMap::new()),
                case_insensitive_dir_cache: Mutex::new(HashMap::new()),
                raw_cache: Mutex::new(RawDataCache::new(
                    RAW_CACHE_CAPACITY_BYTES,
                    RAW_CACHE_MAX_ENTRY_BYTES,
                )),
                xp3_provider,
                memory_files: RwLock::new(memory_files),
                catalog_paths: RwLock::new(catalog_paths),
                memory_writes: Mutex::new(BTreeMap::new()),
                auto_paths: RwLock::new(auto_paths),
                revision: AtomicU64::new(1),
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
        }
    }

    /// Adds logical files to the catalogue. Existing entries retain their
    /// first-seen spelling so directory listings remain deterministic.
    pub fn add_catalog_paths<I, S>(&self, paths: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        if let Ok(mut catalog) = self.inner.catalog_paths.write() {
            for path in paths {
                let path = path.into();
                if let Some(key) = catalog_path(&path) {
                    catalog
                        .entry(key)
                        .or_insert_with(|| normalize_storage_separators(&path));
                }
            }
        }
    }

    /// Adds a file to a memory-backed storage view. Web resource schedulers
    /// use this after an asynchronous fetch completes; native storage views
    /// simply reject the mutation because they have no memory overlay.
    pub fn insert_memory(&self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        let path = normalize_storage_separators(&path.into());
        let catalog_entry = catalog_path(&path);
        if let Ok(mut files) = self.inner.memory_files.write() {
            files.insert(path.clone(), Arc::from(bytes.into()));
            self.invalidate_caches();
        }
        if let Some(path) = catalog_entry
            && let Ok(mut catalog) = self.inner.catalog_paths.write()
        {
            catalog.entry(path.clone()).or_insert(path);
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
            self.invalidate_caches();
        }
    }

    pub fn remove_auto_path(&self, path: &str) -> bool {
        let Ok(mut auto_paths) = self.inner.auto_paths.write() else {
            return false;
        };
        let before = auto_paths.len();
        auto_paths.retain(|item| item != path);
        let removed = before != auto_paths.len();
        if removed {
            self.invalidate_caches();
        }
        removed
    }

    pub fn clear_archive_cache(&self) -> Result<()> {
        if let Some(provider) = &self.inner.xp3_provider {
            provider
                .clear_segment_cache()
                .map_err(|error| TjsError::runtime(error.to_string()))?;
        }
        self.clear_raw_cache();
        Ok(())
    }

    pub fn storage_exists(&self, name: &str) -> bool {
        self.resolve_storage(name).is_ok()
    }

    /// Returns whether a logical directory exists in the filesystem, XP3
    /// provider, or deferred publication catalogue.
    pub fn is_directory(&self, name: &str) -> bool {
        self.list_directory(name).is_ok()
    }

    /// Lists immediate children using KRKR/fstat semantics: directory names
    /// have a trailing `/`, files do not. The result is sorted and de-duped
    /// case-insensitively across filesystem, XP3 and manifest layers.
    pub fn list_directory(&self, name: &str) -> io::Result<Vec<String>> {
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
            LocatedResource::Xp3 { .. } | LocatedResource::Memory { .. } => None,
        }
    }

    pub fn read_data(&self, name: &str) -> Result<StorageData> {
        let located = self.resolve_storage(name)?;
        let storage_name = located.storage_name().to_string();
        let encoding_hint = located.encoding_hint();
        let data = self.load_located_data(&located).map_err(io_error)?;
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
        let Some(root) = self.inner.root.as_ref() else {
            let key = memory_write_key(name)?;
            let mut output = bytes.to_vec();
            if let Some(offset) = storage_mode_offset(mode) {
                let mut current = self.read_binary_vec(&key).unwrap_or_default();
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
            self.invalidate_caches();
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
                .create(true)
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
            self.invalidate_caches();
        }
        result
    }

    pub fn open_storage(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
        match self.resolve_storage_io(name)? {
            LocatedResource::Fs { path, .. } => {
                File::open(path).map(|file| Box::new(file) as Box<dyn ResourceStream>)
            }
            LocatedResource::Xp3 { entry_name, .. } => {
                let provider = self.inner.xp3_provider.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "XP3 provider is not configured")
                })?;
                provider.open(&entry_name)
            }
            LocatedResource::Memory { data, .. } => Ok(Box::new(Cursor::new(data.to_vec()))),
        }
    }

    pub fn storage_byte_len(&self, name: &str) -> io::Result<Option<u64>> {
        self.resolve_storage_io(name)
            .map(|located| Some(located.byte_len()))
    }

    pub(crate) fn storage_candidates(&self, name: &str) -> Result<Vec<String>> {
        storage_candidates_with_auto_paths(name, &self.auto_paths())
    }

    fn resolve_storage(&self, name: &str) -> Result<LocatedResource> {
        self.resolve_storage_io(name).map_err(io_error)
    }

    fn resolve_storage_io(&self, name: &str) -> io::Result<LocatedResource> {
        if let Some(storage) = self.find_absolute_storage(name)? {
            return Ok(storage);
        }

        if let Ok(cache) = self.inner.lookup_cache.lock()
            && let Some(storage) = cache.get(name).cloned()
        {
            return storage.ok_or_else(|| storage_not_found(name));
        }

        let candidates = self.storage_candidates(name).map_err(tjs_error_to_io)?;
        for candidate in &candidates {
            let relative = clean_relative_path(candidate).map_err(tjs_error_to_io)?;
            if let Some(storage) = self.find_fs_candidate(candidate, &relative)? {
                self.cache_lookup(name, Some(storage.clone()));
                return Ok(storage);
            }
        }

        if let Some(provider) = &self.inner.xp3_provider {
            for candidate in &candidates {
                if let Some(entry) = provider.get_entry(candidate) {
                    let storage = LocatedResource::Xp3 {
                        storage_name: candidate.clone(),
                        entry_name: entry.name.clone(),
                        byte_len: entry.original_size,
                    };
                    self.cache_lookup(name, Some(storage.clone()));
                    return Ok(storage);
                }
            }
        }

        for candidate in &candidates {
            let normalized = normalize_storage_separators(candidate);
            let Ok(memory_files) = self.inner.memory_files.read() else {
                continue;
            };
            if let Some(data) = memory_files.get(&normalized) {
                let storage = LocatedResource::Memory {
                    storage_name: candidate.clone(),
                    encoding_hint: infer_encoding_from_path(Path::new(candidate)),
                    data: Arc::clone(data),
                };
                self.cache_lookup(name, Some(storage.clone()));
                return Ok(storage);
            }
            if let Some((stored_path, data)) = memory_files
                .iter()
                .find(|(stored, _)| stored.eq_ignore_ascii_case(&normalized))
            {
                let storage = LocatedResource::Memory {
                    storage_name: candidate.clone(),
                    encoding_hint: infer_encoding_from_path(Path::new(stored_path)),
                    data: Arc::clone(data),
                };
                self.cache_lookup(name, Some(storage.clone()));
                return Ok(storage);
            }
        }

        // Remote Web manifests expose unambiguous basename aliases for KRKR's
        // auto-path lookup. Mirror that behavior in the memory overlay so a
        // preloaded `main/Config.tjs` also satisfies `Config.tjs` without a
        // duplicate network request.
        for candidate in &candidates {
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
                let storage = LocatedResource::Memory {
                    storage_name: candidate.clone(),
                    encoding_hint: infer_encoding_from_path(Path::new(stored_path)),
                    data: Arc::clone(data),
                };
                self.cache_lookup(name, Some(storage.clone()));
                return Ok(storage);
            }
        }

        self.cache_lookup(name, None);
        Err(storage_not_found(name))
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
        let key = RawCacheKey {
            revision: self.revision(),
            source: located.cache_source(),
        };
        if let Ok(mut cache) = self.inner.raw_cache.lock()
            && let Some(data) = cache.get(&key)
        {
            return Ok(data);
        }

        let data = match located {
            LocatedResource::Fs { path, byte_len, .. } => load_fs_resource_data(path, *byte_len)?,
            LocatedResource::Xp3 { entry_name, .. } => {
                let provider = self.inner.xp3_provider.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "XP3 provider is not configured")
                })?;
                let mut stream = provider.open(entry_name)?;
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes)?;
                ResourceData::from_vec(bytes)
            }
            LocatedResource::Memory { data, .. } => ResourceData::from_vec(data.to_vec()),
        };

        if let Ok(mut cache) = self.inner.raw_cache.lock() {
            cache.insert(key, data.clone());
        }
        Ok(data)
    }

    fn cache_lookup(&self, name: &str, storage: Option<LocatedResource>) {
        if let Ok(mut cache) = self.inner.lookup_cache.lock() {
            cache.insert(name.to_string(), storage);
        }
    }

    fn auto_paths(&self) -> Vec<String> {
        self.inner
            .auto_paths
            .read()
            .map(|paths| paths.clone())
            .unwrap_or_default()
    }

    fn invalidate_caches(&self) {
        self.inner.revision.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut cache) = self.inner.lookup_cache.lock() {
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
}

impl LocatedResource {
    fn storage_name(&self) -> &str {
        match self {
            Self::Fs { storage_name, .. }
            | Self::Xp3 { storage_name, .. }
            | Self::Memory { storage_name, .. } => storage_name,
        }
    }

    fn encoding_hint(&self) -> Option<&'static Encoding> {
        match self {
            Self::Fs { encoding_hint, .. } => *encoding_hint,
            Self::Xp3 { .. } => None,
            Self::Memory { encoding_hint, .. } => *encoding_hint,
        }
    }

    fn byte_len(&self) -> u64 {
        match self {
            Self::Fs { byte_len, .. } | Self::Xp3 { byte_len, .. } => *byte_len,
            Self::Memory { data, .. } => data.len() as u64,
        }
    }

    fn cache_source(&self) -> RawCacheSource {
        match self {
            Self::Fs { path, .. } => RawCacheSource::Fs(path.clone()),
            Self::Xp3 { entry_name, .. } => RawCacheSource::Xp3(entry_name.clone()),
            Self::Memory { storage_name, .. } => RawCacheSource::Memory(storage_name.clone()),
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

pub(crate) fn project_layers(root: &Path) -> Vec<ProjectLayer> {
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

fn storage_candidates_with_auto_paths(name: &str, auto_paths: &[String]) -> Result<Vec<String>> {
    let names = storage_lookup_names(name)?;
    let mut candidates = Vec::with_capacity(names.len() * (auto_paths.len() + 1));
    for name in names {
        let clean = clean_relative_path(&name)?;
        push_unique_storage_candidate(&mut candidates, &clean);
        for auto_path in auto_paths.iter().rev() {
            for candidate in auto_path_candidates(auto_path, &clean) {
                push_unique_storage_candidate(&mut candidates, &candidate);
            }
        }
    }
    Ok(candidates)
}

fn auto_path_candidates(auto_path: &str, clean: &Path) -> Vec<PathBuf> {
    let Some(auto_path) = normalize_auto_path(auto_path) else {
        return Vec::new();
    };
    let Ok(auto_relative) = clean_relative_path(&auto_path) else {
        return Vec::new();
    };
    vec![auto_relative.join(clean)]
}

fn push_unique_storage_candidate(candidates: &mut Vec<String>, path: &Path) {
    let candidate = path_to_storage_name(path);
    if !candidates.iter().any(|item| item == &candidate) {
        candidates.push(candidate);
    }
}

fn path_to_storage_name(path: &Path) -> String {
    normalize_storage_separators(&path.to_string_lossy())
}

fn storage_lookup_names(name: &str) -> Result<Vec<String>> {
    let name = normalize_storage_separators(name);
    clean_relative_path(&name)?;
    let path = Path::new(&name);
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(is_known_storage_extension)
    {
        return Ok(vec![name]);
    }

    let mut names = Vec::with_capacity(12);
    names.push(name.clone());
    for extension in [
        "tlg", "png", "jpg", "jpeg", "bmp", "webp", "ks", "tjs", "asd", "ogg", "wav", "tcw", "mpg",
        "mpeg",
    ] {
        names.push(format!("{name}.{extension}"));
    }
    Ok(names)
}

fn is_known_storage_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "bmp"
            | "webp"
            | "ks"
            | "tjs"
            | "asd"
            | "ogg"
            | "wav"
            | "tcw"
            | "mpg"
            | "mpeg"
    )
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

pub(crate) fn decode_text_storage(
    _name: &str,
    bytes: &[u8],
    encoding_hint: Option<&'static Encoding>,
    configured_encoding: &str,
) -> Result<String> {
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

pub(crate) fn normalize_storage_separators(path: &str) -> String {
    path.replace('\\', "/")
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
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(TjsError::runtime(format!(
            "memory storage path must be relative: {name}"
        )));
    }
    Ok(normalize_storage_separators(
        clean_relative_path(name)?.to_str().unwrap_or_default(),
    ))
}

pub(crate) fn storage_write_path(root: &Path, name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        if is_safe_absolute_storage_path(path) {
            return Ok(path.to_path_buf());
        }
        return Err(TjsError::runtime(format!(
            "storage path must be safe: {}",
            path.display()
        )));
    }
    Ok(root.join(clean_relative_path(name)?))
}

pub(crate) fn storage_mode_offset(mode: &str) -> Option<u64> {
    let offset = mode
        .split('o')
        .nth(1)?
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    (!offset.is_empty()).then(|| offset.parse().ok()).flatten()
}

pub(crate) fn open_project_archives(root: &Path) -> Result<Option<Xp3ResourceProvider>> {
    let archives = project_archive_paths(root);
    if archives.is_empty() {
        return Ok(None);
    }
    Xp3ResourceProvider::open_archives(archives)
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

pub(crate) fn io_error(error: io::Error) -> TjsError {
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
