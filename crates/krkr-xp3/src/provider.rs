use std::{fs::File, io, path::Path, sync::Arc};

use krkr_core::{ResourceStream, StoragePort, Xp3FilterRegistry};

use crate::{Result, Xp3Archive, Xp3Entry, Xp3Error, Xp3OpenOptions, normalize_entry_name};

#[derive(Clone)]
pub struct Xp3ResourceProvider {
    archives: Arc<[Xp3Archive<File>]>,
    /// Every mount's identity, parallel to `archives`. KRKR's auto-path table
    /// stores `archive.xp3>` entries that address one specific archive, so the
    /// provider has to keep enough of each mount to tell them apart
    /// ([`MountName`], [`Self::archive_index`]).
    mounts: Arc<[MountName]>,
    /// The filter registry this provider's archives read. The provider owns it
    /// so "the registry the archives consult" and "the registry the host hands
    /// a plugin" can never be two different objects
    /// (`ProjectStoragePort::xp3_filter_registry`).
    filter_registry: Arc<Xp3FilterRegistry>,
}

/// What one mount is called by an `archive.xp3>` qualifier: the three
/// spellings [`Xp3ResourceProvider::archive_index`] compares against.
#[derive(Clone, Debug, Default)]
struct MountName {
    /// Lower-cased file name (`data.xp3`) — the fallback identity of a mount
    /// whose directory the provider does not know.
    file_name: String,
    /// Lower-cased, `/`-separated path as handed to the constructor, for an
    /// absolute qualifier.
    path: String,
    /// Lower-cased path below the base the provider was opened under
    /// (`sys/data.xp3`) — the identity a relative qualifier names. `None` when
    /// the mount was given without a base (a relative path is its own logical
    /// name, an absolute one outside the base has none).
    logical: Option<String>,
}

impl MountName {
    fn new(path: &Path, base: Option<&Path>) -> Self {
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file_name = normalize_mount_path(&file_name);
        // The path below the base when there is one, else the path as given —
        // a relative mount names itself (`open_archives(["sys/x.xp3"])`), an
        // absolute one outside the base has no logical name at all.
        let logical = base
            .and_then(|base| path.strip_prefix(base).ok())
            .map(|relative| normalize_mount_path(&relative.to_string_lossy()))
            .or_else(|| {
                path.is_relative()
                    .then(|| normalize_mount_path(&path.to_string_lossy()))
            });
        Self {
            file_name,
            path: normalize_mount_path(&path.to_string_lossy()),
            logical,
        }
    }
}

/// The comparison spelling of a mount path or qualifier: `\` folds to `/`,
/// `.` and empty segments disappear so `./x.xp3` and `x.xp3` are one name, the
/// rest is ASCII-lowercased, and a leading `/` stays. The reference's file
/// media lower-cases every path it is handed
/// (`tTVPFileMedia::NormalizePathName`, `base/win32/StorageImpl.cpp:81-92`)
/// and its storage-name compression drops `.` segments and duplicated
/// delimiters (`StorageIntf.cpp:400-453`), so both sides of the comparison are
/// spelled the way the reference would have spelled them.
fn normalize_mount_path(path: &str) -> String {
    let folded = path.replace('\\', "/");
    let absolute = folded.starts_with('/');
    let mut normalized = String::with_capacity(folded.len());
    for part in folded.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(&part.to_ascii_lowercase());
    }
    if absolute {
        normalized.insert(0, '/');
    }
    normalized
}

/// Drops a `file://` scheme from a qualifier: the reference's media manager
/// hands the media only the text after `media://` (`GetDomainAndPath`,
/// `StorageIntf.cpp:164-168`, used by `Open`/`CheckExistentStorage`, `:498`,
/// `:506`), so `addAutoPath("file://./x.xp3>")` names the same archive as
/// `"x.xp3>"` for the file media that owns this engine's built-in stack. Any
/// other scheme (or a drive-letter `C:` spelling) is left alone and simply
/// matches no mount.
fn strip_file_media(archive: &str) -> String {
    const SCHEME: &str = "file://";
    let folded = archive.replace('\\', "/");
    if folded
        .get(..SCHEME.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(SCHEME))
    {
        return folded[SCHEME.len()..].to_string();
    }
    folded
}

impl Xp3ResourceProvider {
    pub fn open_archive(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_archives([path])
    }

    pub fn open_archives<I, P>(paths: I) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        Self::open_archives_with_options(paths, Xp3OpenOptions::default())
    }

    pub fn open_archives_with_options<I, P>(paths: I, options: Xp3OpenOptions) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        Self::open_archives_below_with_options(None, paths, options)
    }

    /// Opens archives that live below `base`, so that a declared
    /// `dir/archive.xp3>` qualifier can name the mount at
    /// `<base>/dir/archive.xp3` — the mount the qualifier path addresses.
    ///
    /// A project's archives are found by scanning `<root>/sys` and `<root>`
    /// (`project_archive_paths`), so the base is what turns the mount's
    /// filesystem path back into the logical name the game declared. Without
    /// it, a relative qualifier has no mount identity to match
    /// ([`Self::archive_index`]).
    pub fn open_archives_below<I, P>(base: impl AsRef<Path>, paths: I) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        Self::open_archives_below_with_options(
            Some(base.as_ref()),
            paths,
            Xp3OpenOptions::default(),
        )
    }

    fn open_archives_below_with_options<I, P>(
        base: Option<&Path>,
        paths: I,
        options: Xp3OpenOptions,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        // One registry for every archive of this provider — the caller's when
        // the options carried one, else a fresh one. The archives read it per
        // entry-stream creation and per read, so they must share the handle
        // the host exposes; giving each archive its own would make an install
        // through that handle reach nothing.
        let filter_registry = options.filter_registry.clone().unwrap_or_default();
        let mut archives = Vec::new();
        let mut mounts = Vec::new();
        for path in paths {
            let path = path.as_ref();
            archives.push(Xp3Archive::open_file_with_options(
                path,
                options
                    .clone()
                    .with_filter_registry(Arc::clone(&filter_registry)),
            )?);
            mounts.push(MountName::new(path, base));
        }
        Ok(Self {
            archives: archives.into(),
            mounts: mounts.into(),
            filter_registry,
        })
    }

    /// Wraps archives that were opened elsewhere. They keep the registry they
    /// were opened with; the provider reports the *first* archive's, which is
    /// exact for the normal case — a set opened together from one
    /// [`Xp3OpenOptions`] shares one registry — and is documented here because
    /// a heterogeneous set would leave the other archives reading a registry
    /// no host can reach.
    pub fn from_archives(archives: Vec<Xp3Archive<File>>) -> Self {
        let filter_registry = archives
            .first()
            .map(Xp3Archive::filter_registry)
            .unwrap_or_default();
        // These archives were opened elsewhere, so nothing here knows where
        // they came from; an empty identity matches no qualifier, which is what
        // a name-only mount can honestly answer.
        let mounts = vec![MountName::default(); archives.len()];
        Self {
            archives: archives.into(),
            mounts: mounts.into(),
            filter_registry,
        }
    }

    /// The filter registry every archive of this provider reads — the object a
    /// plugin installs into (`TVPSetXP3ArchiveExtractionFilter`).
    pub fn filter_registry(&self) -> Arc<Xp3FilterRegistry> {
        Arc::clone(&self.filter_registry)
    }

    pub fn archive_count(&self) -> usize {
        self.archives.len()
    }

    pub fn entry_count(&self) -> usize {
        self.archives
            .iter()
            .map(|archive| archive.entries().len())
            .sum()
    }

    /// Returns logical member names from all mounted archives. Publication
    /// hosts use this to build a virtual directory index without opening any
    /// compressed payloads.
    pub fn entry_names(&self) -> Vec<String> {
        let mut names = self
            .archives
            .iter()
            .flat_map(|archive| archive.entries().iter().map(|entry| entry.name.clone()))
            .collect::<Vec<_>>();
        names.sort();
        names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        names
    }

    pub fn is_empty(&self) -> bool {
        self.archives.is_empty()
    }

    pub fn get_entry(&self, path: &str) -> Option<&Xp3Entry> {
        let probe = NormalizedProbe::new(path).ok()?;
        for archive in self.archives.iter().rev() {
            if let Some(entry) = archive.get_entry_normalized(&probe.name) {
                return Some(entry);
            }
            if let Some(entry) =
                archive.get_entry_normalized_ascii_case_insensitive(&probe.ascii_lowercase)
            {
                return Some(entry);
            }
        }
        None
    }

    /// Resolves a member inside one named archive, ignoring every other mount.
    /// `archive` is a storage path (KRKR builds auto paths from
    /// `System.exePath` plus the declared name), and the mount it names is
    /// selected by that path — see [`Self::archive_index`].
    pub fn get_entry_in(&self, archive: &str, path: &str) -> Option<&Xp3Entry> {
        let index = self.archive_index(archive)?;
        let probe = NormalizedProbe::new(path).ok()?;
        let archive = &self.archives[index];
        archive
            .get_entry_normalized(&probe.name)
            .or_else(|| archive.get_entry_normalized_ascii_case_insensitive(&probe.ascii_lowercase))
    }

    pub fn open_in(&self, archive: &str, path: &str) -> io::Result<Box<dyn ResourceStream>> {
        let entry_name = self
            .get_entry_in(archive, path)
            .map(|entry| entry.name.clone())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_string()))?;
        let index = self
            .archive_index(archive)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, archive.to_string()))?;
        let stream = self.archives[index]
            .open_by_name(&entry_name)
            .map_err(xp3_error_to_io)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, entry_name.clone()))?;
        Ok(Box::new(stream))
    }

    /// The mount an `archive.xp3>` qualifier names.
    ///
    /// The reference resolves the qualifier as a *storage path*, never as a
    /// file name. `TVPRebuildAutoPathTable` splits an auto path at `>` and
    /// reads the archive at the spelling before it (`StorageIntf.cpp:1055-1105`,
    /// `arcname` at `:1060`, `TVPArchiveCache.Get(arcname)` at `:1066`);
    /// `TVPGetPlacedPath` returns that same spelling with the requested
    /// storage name appended (`:1189`), and `_TVPCreateStream` splits the
    /// placed path at `>` and opens the archive the leading part names
    /// (`:1249-1260`). `TVPArchiveCache::Get` then probes and opens exactly
    /// that file — there is no walk over other archives, and no comparison
    /// that ignores a directory (`:757-780`, `TVPIsExistentStorageNoSearch`,
    /// `:799-830`). The reference's file media resolves a relative spelling
    /// against the current directory, which the engine sets to the project
    /// root, so:
    ///
    /// * a path with a directory names one mount: `sys/data.xp3` is the mount
    ///   below the provider's base, `/…/sys/data.xp3` an absolute mount path,
    ///   and a directory that names no mount resolves nothing — a same-named
    ///   file elsewhere is a different archive, not a fallback;
    /// * a bare name addresses the file in the current directory, i.e. the
    ///   mount sitting directly below the base, and only then falls back to the
    ///   file-name walk for mounts the provider was given without a base.
    ///
    /// The file-name fallback is what a caller that mounted archives by path
    /// alone can honestly answer (`get_entry_in("patch.xp3", …)`), and it is
    /// the last match in the list — the order `get_entry` walks in reverse.
    fn archive_index(&self, archive: &str) -> Option<usize> {
        let query = normalize_mount_path(&strip_file_media(archive));
        if query.is_empty() {
            return None;
        }
        if query.starts_with('/') {
            return self.mounts.iter().rposition(|mount| mount.path == query);
        }
        if query.contains('/') {
            return self
                .mounts
                .iter()
                .rposition(|mount| mount.logical.as_deref() == Some(query.as_str()));
        }
        self.mounts
            .iter()
            .rposition(|mount| mount.logical.as_deref() == Some(query.as_str()))
            .or_else(|| {
                self.mounts
                    .iter()
                    .rposition(|mount| mount.file_name == query)
            })
    }

    pub fn clear_segment_cache(&self) -> Result<()> {
        for archive in self.archives.iter() {
            archive.clear_segment_cache()?;
        }
        Ok(())
    }
}

impl StoragePort for Xp3ResourceProvider {
    fn open(&self, path: &str) -> io::Result<Box<dyn ResourceStream>> {
        let probe = NormalizedProbe::new(path).map_err(xp3_error_to_io)?;
        for archive in self.archives.iter().rev() {
            let entry_name = if archive.get_entry_normalized(&probe.name).is_some() {
                probe.name.clone()
            } else if let Some(entry) =
                archive.get_entry_normalized_ascii_case_insensitive(&probe.ascii_lowercase)
            {
                entry.name.clone()
            } else {
                continue;
            };
            let stream = archive
                .open_by_name(&entry_name)
                .map_err(xp3_error_to_io)?
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, entry_name.clone()))?;
            return Ok(Box::new(stream));
        }

        Err(io::Error::new(io::ErrorKind::NotFound, probe.name))
    }

    fn exists(&self, path: &str) -> bool {
        self.get_entry(path).is_some()
    }

    fn byte_len(&self, path: &str) -> io::Result<Option<u64>> {
        Ok(self.get_entry(path).map(|entry| entry.original_size))
    }
}

/// One lookup path, normalized once for the whole provider call.
///
/// A probe needs two spellings: the normalized name, which is what each
/// archive's exact map keys on, and its ASCII-lowercased form, which is what
/// the case-insensitive map keys on. Building them per archive made an
/// existence probe over seven mounts pay a fresh normalization for every
/// exact and case-insensitive attempt (fifteen normalizations and seven
/// lowercase allocations for a name that is usually absent), so the provider
/// builds both once and hands the archives the normalized-input lookups.
struct NormalizedProbe {
    /// The path after [`normalize_entry_name`].
    name: String,
    /// `name.to_ascii_lowercase()`, the case-insensitive maps' key.
    ascii_lowercase: String,
}

impl NormalizedProbe {
    fn new(path: &str) -> Result<Self> {
        let name = normalize_entry_name(path)?;
        let ascii_lowercase = name.to_ascii_lowercase();
        Ok(Self {
            name,
            ascii_lowercase,
        })
    }
}

fn xp3_error_to_io(error: Xp3Error) -> io::Error {
    match error {
        Xp3Error::Io(error) => error,
        Xp3Error::InvalidPath(path) => io::Error::new(io::ErrorKind::InvalidInput, path),
        Xp3Error::NotFound(path) => io::Error::new(io::ErrorKind::NotFound, path),
        Xp3Error::InvalidArchive(message) | Xp3Error::Unsupported(message) => {
            io::Error::new(io::ErrorKind::InvalidData, message)
        }
    }
}
