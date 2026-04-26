use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    marker::PhantomData,
    path::Path,
    sync::Arc,
};

use crate::{Result, SegmentCacheConfig, Xp3ExtractionFilter, Xp3OpenOptions};
use crate::{
    Xp3Entry, Xp3EntryStream, Xp3Error,
    cache::SegmentCache,
    parse::{find_xp3_base_offset, read_entries},
    source::{ArchiveSourceHandle, FileArchiveSource, SeekArchiveSource},
    util::normalize_entry_name,
};

pub struct Xp3Archive<R> {
    reader: ArchiveSourceHandle<R>,
    entries: Vec<Xp3Entry>,
    by_name: HashMap<String, usize>,
    base_offset: u64,
    file_len: u64,
    extraction_filter: Option<Arc<dyn Xp3ExtractionFilter>>,
    segment_cache: Arc<SegmentCache>,
    reader_type: PhantomData<fn() -> R>,
}

impl<R> Xp3Archive<R>
where
    R: Read + Seek + Send,
{
    pub fn open(reader: R) -> Result<Self> {
        Self::open_with_options(reader, Xp3OpenOptions::default())
    }

    pub fn open_with_options(reader: R, options: Xp3OpenOptions) -> Result<Self> {
        Self::open_with_source(reader, options, |reader| {
            ArchiveSourceHandle::Seek(Arc::new(SeekArchiveSource::new(reader)))
        })
    }

    fn open_with_source(
        mut reader: R,
        options: Xp3OpenOptions,
        make_source: impl FnOnce(R) -> ArchiveSourceHandle<R>,
    ) -> Result<Self> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;

        let base_offset = find_xp3_base_offset(&mut reader, file_len)?;
        let mut entries = read_entries(&mut reader, base_offset, file_len)?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));

        let mut by_name = HashMap::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            by_name.entry(entry.name.clone()).or_insert(index);
        }

        Ok(Self {
            reader: make_source(reader),
            entries,
            by_name,
            base_offset,
            file_len,
            extraction_filter: options.extraction_filter,
            segment_cache: Arc::new(SegmentCache::new(options.segment_cache)),
            reader_type: PhantomData,
        })
    }

    pub fn entries(&self) -> &[Xp3Entry] {
        &self.entries
    }

    pub fn get_entry(&self, name: &str) -> Option<&Xp3Entry> {
        let name = normalize_entry_name(name).ok()?;
        self.by_name
            .get(&name)
            .and_then(|index| self.entries.get(*index))
    }

    pub fn get_entry_by_index(&self, index: usize) -> Option<&Xp3Entry> {
        self.entries.get(index)
    }

    pub fn open_by_name(&self, name: &str) -> Result<Option<Xp3EntryStream<R>>> {
        let name = normalize_entry_name(name)?;
        let Some(index) = self.by_name.get(&name).copied() else {
            return Ok(None);
        };
        self.open_by_index(index).map(Some)
    }

    pub fn open_by_index(&self, index: usize) -> Result<Xp3EntryStream<R>> {
        let entry = self
            .entries
            .get(index)
            .cloned()
            .ok_or_else(|| Xp3Error::NotFound(index.to_string()))?;

        Ok(Xp3EntryStream::new(
            self.reader.clone(),
            Arc::clone(&self.segment_cache),
            self.extraction_filter.clone(),
            index,
            entry,
            self.file_len,
        ))
    }

    pub fn base_offset(&self) -> u64 {
        self.base_offset
    }

    pub fn segment_cache_config(&self) -> SegmentCacheConfig {
        self.segment_cache.config()
    }

    pub fn clear_segment_cache(&self) -> Result<()> {
        self.segment_cache.clear()?;
        Ok(())
    }
}

impl Xp3Archive<File> {
    pub fn open_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_file_with_options(path, Xp3OpenOptions::default())
    }

    pub fn open_file_with_options(path: impl AsRef<Path>, options: Xp3OpenOptions) -> Result<Self> {
        let file = File::open(path)?;
        Self::open_with_source(file, options, |file| {
            ArchiveSourceHandle::File(Arc::new(FileArchiveSource::new(file)))
        })
    }
}

impl<R> Clone for Xp3Archive<R> {
    fn clone(&self) -> Self {
        Self {
            reader: self.reader.clone(),
            entries: self.entries.clone(),
            by_name: self.by_name.clone(),
            base_offset: self.base_offset,
            file_len: self.file_len,
            extraction_filter: self.extraction_filter.clone(),
            segment_cache: Arc::clone(&self.segment_cache),
            reader_type: PhantomData,
        }
    }
}
