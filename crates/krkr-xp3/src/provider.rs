use std::{fs::File, io, path::Path, sync::Arc};

use krkr_core::{ResourceStream, StoragePort};

use crate::{Result, Xp3Archive, Xp3Entry, Xp3Error, Xp3OpenOptions, normalize_entry_name};

#[derive(Clone)]
pub struct Xp3ResourceProvider {
    archives: Arc<[Xp3Archive<File>]>,
    /// Lower-case archive file names (`data.xp3`), parallel to `archives`.
    /// KRKR's auto-path table stores `archive.xp3>` entries that address one
    /// specific archive, so the provider has to keep that identity around.
    archive_names: Arc<[String]>,
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
        let mut archives = Vec::new();
        let mut names = Vec::new();
        for path in paths {
            let path = path.as_ref();
            archives.push(Xp3Archive::open_file_with_options(path, options.clone())?);
            names.push(
                path.file_name()
                    .map(|name| name.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default(),
            );
        }
        Ok(Self {
            archives: archives.into(),
            archive_names: names.into(),
        })
    }

    pub fn from_archives(archives: Vec<Xp3Archive<File>>) -> Self {
        let archive_names = vec![String::new(); archives.len()];
        Self {
            archives: archives.into(),
            archive_names: archive_names.into(),
        }
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
        let normalized = normalize_entry_name(path).ok()?;
        for archive in self.archives.iter().rev() {
            if let Some(entry) = archive.get_entry(&normalized) {
                return Some(entry);
            }
            if let Some(entry) = archive.get_entry_ascii_case_insensitive(&normalized) {
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
        let normalized = normalize_entry_name(path).ok()?;
        let archive = &self.archives[index];
        archive
            .get_entry(&normalized)
            .or_else(|| archive.get_entry_ascii_case_insensitive(&normalized))
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
        let normalized = normalize_entry_name(path).map_err(xp3_error_to_io)?;
        for archive in self.archives.iter().rev() {
            let entry_name = if archive.get_entry(&normalized).is_some() {
                normalized.clone()
            } else if let Some(entry) = archive.get_entry_ascii_case_insensitive(&normalized) {
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

        Err(io::Error::new(io::ErrorKind::NotFound, normalized))
    }

    fn exists(&self, path: &str) -> bool {
        self.get_entry(path).is_some()
    }

    fn byte_len(&self, path: &str) -> io::Result<Option<u64>> {
        Ok(self.get_entry(path).map(|entry| entry.original_size))
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
