use std::{fs::File, io, path::Path, sync::Arc};

use krkr_core::{ResourceProvider, ResourceStream};

use crate::{Result, Xp3Archive, Xp3Entry, Xp3Error, Xp3OpenOptions, normalize_entry_name};

#[derive(Clone)]
pub struct Xp3ResourceProvider {
    archives: Arc<[Xp3Archive<File>]>,
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
        for path in paths {
            archives.push(Xp3Archive::open_file_with_options(
                path.as_ref(),
                options.clone(),
            )?);
        }
        Ok(Self::from_archives(archives))
    }

    pub fn from_archives(archives: Vec<Xp3Archive<File>>) -> Self {
        Self {
            archives: archives.into(),
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

    pub fn clear_segment_cache(&self) -> Result<()> {
        for archive in self.archives.iter() {
            archive.clear_segment_cache()?;
        }
        Ok(())
    }
}

impl ResourceProvider for Xp3ResourceProvider {
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
