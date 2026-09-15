use std::{fs::File, io, path::Path, sync::Arc};

use krkr_core::{ResourceStream, StoragePort, Xp3FilterRegistry};

use crate::{Result, Xp3Archive, Xp3Entry, Xp3Error, Xp3OpenOptions, normalize_entry_name};

#[derive(Clone)]
pub struct Xp3ResourceProvider {
    archives: Arc<[Xp3Archive<File>]>,
    /// Lower-case archive file names (`data.xp3`), parallel to `archives`.
    /// KRKR's auto-path table stores `archive.xp3>` entries that address one
    /// specific archive, so the provider has to keep that identity around.
    archive_names: Arc<[String]>,
    /// The filter registry this provider's archives read. The provider owns it
    /// so "the registry the archives consult" and "the registry the host hands
    /// a plugin" can never be two different objects
    /// (`ProjectStoragePort::xp3_filter_registry`).
    filter_registry: Arc<Xp3FilterRegistry>,
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
        // One registry for every archive of this provider — the caller's when
        // the options carried one, else a fresh one. The archives read it per
        // entry-stream creation and per read, so they must share the handle
        // the host exposes; giving each archive its own would make an install
        // through that handle reach nothing.
        let filter_registry = options.filter_registry.clone().unwrap_or_default();
        let mut archives = Vec::new();
        let mut names = Vec::new();
        for path in paths {
            let path = path.as_ref();
            archives.push(Xp3Archive::open_file_with_options(
                path,
                options
                    .clone()
                    .with_filter_registry(Arc::clone(&filter_registry)),
            )?);
            names.push(
                path.file_name()
                    .map(|name| name.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default(),
            );
        }
        Ok(Self {
            archives: archives.into(),
            archive_names: names.into(),
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
        let archive_names = vec![String::new(); archives.len()];
        Self {
            archives: archives.into(),
            archive_names: archive_names.into(),
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
    /// `archive` may carry a directory prefix (KRKR builds auto paths from
    /// `System.arcPath`), so only the file name is compared.
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

    fn archive_index(&self, archive: &str) -> Option<usize> {
        let normalized = archive.replace('\\', "/");
        let wanted = normalized.rsplit('/').next()?.to_ascii_lowercase();
        if wanted.is_empty() {
            return None;
        }
        self.archive_names
            .iter()
            .rposition(|name| name.as_str() == wanted)
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
