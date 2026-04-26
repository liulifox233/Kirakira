use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};

use flate2::read::ZlibDecoder;

use crate::{
    Xp3Entry, Xp3ExtractionFilter, Xp3Segment, Xp3SegmentEncoding,
    cache::{SegmentCache, SegmentCacheKey},
    util::{checked_add_io, ensure_range_io, usize_from_u64_io},
};

pub struct Xp3EntryStream<R> {
    reader: Arc<Mutex<R>>,
    segments: Vec<Xp3Segment>,
    file_size: u64,
    file_hash: u32,
    position: u64,
    entry_index: usize,
    file_len: u64,
    extraction_filter: Option<Arc<dyn Xp3ExtractionFilter>>,
    segment_cache: Arc<SegmentCache>,
    active_segment: Option<ActiveDecodedSegment>,
}

struct ActiveDecodedSegment {
    key: SegmentCacheKey,
    data: Arc<[u8]>,
}

impl<R> Xp3EntryStream<R> {
    pub(crate) fn new(
        reader: Arc<Mutex<R>>,
        segment_cache: Arc<SegmentCache>,
        extraction_filter: Option<Arc<dyn Xp3ExtractionFilter>>,
        entry_index: usize,
        entry: Xp3Entry,
        file_len: u64,
    ) -> Self {
        Self {
            reader,
            segments: entry.segments,
            file_size: entry.original_size,
            file_hash: entry.file_hash,
            position: 0,
            entry_index,
            file_len,
            extraction_filter,
            segment_cache,
            active_segment: None,
        }
    }
}

impl<R> Xp3EntryStream<R>
where
    R: Read + Seek + Send,
{
    fn find_segment(&self, position: u64) -> io::Result<usize> {
        if self.segments.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "XP3 entry has no segments",
            ));
        }

        let mut low = 0;
        let mut high = self.segments.len();
        while low < high {
            let mid = low + (high - low) / 2;
            if self.segments[mid].uncompressed_offset <= position {
                low = mid + 1;
            } else {
                high = mid;
            }
        }

        let index = low.checked_sub(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "XP3 entry has a segment gap")
        })?;
        let segment = &self.segments[index];
        let end = checked_add_io(
            segment.uncompressed_offset,
            segment.uncompressed_size,
            "XP3 segment offset overflow",
        )?;
        if position >= end {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "XP3 entry has a segment gap",
            ));
        }
        Ok(index)
    }

    fn read_raw_segment(
        &self,
        segment: &Xp3Segment,
        offset_in_segment: u64,
        output: &mut [u8],
    ) -> io::Result<()> {
        let file_offset = checked_add_io(
            segment.archive_offset,
            offset_in_segment,
            "XP3 raw segment offset overflow",
        )?;
        let read_len = u64::try_from(output.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "read buffer is too large"))?;
        ensure_range_io(
            file_offset,
            read_len,
            self.file_len,
            "XP3 raw segment exceeds archive length",
        )?;

        let mut reader = self
            .reader
            .lock()
            .map_err(|_| io::Error::other("XP3 reader lock poisoned"))?;
        reader.seek(SeekFrom::Start(file_offset))?;
        reader.read_exact(output)
    }

    fn decompressed_segment(
        &mut self,
        segment_index: usize,
        segment: &Xp3Segment,
    ) -> io::Result<Arc<[u8]>> {
        let key = SegmentCacheKey {
            entry_index: self.entry_index,
            segment_index,
        };
        if let Some(active) = &self.active_segment
            && active.key == key
        {
            return Ok(Arc::clone(&active.data));
        }

        if let Some(data) = self.segment_cache.get(key)? {
            self.active_segment = Some(ActiveDecodedSegment {
                key,
                data: Arc::clone(&data),
            });
            return Ok(data);
        }

        let compressed_len = usize_from_u64_io(segment.archived_size, "XP3 segment is too large")?;
        let expected_len =
            usize_from_u64_io(segment.uncompressed_size, "XP3 segment is too large")?;
        ensure_range_io(
            segment.archive_offset,
            segment.archived_size,
            self.file_len,
            "XP3 compressed segment exceeds archive length",
        )?;

        let mut compressed = vec![0; compressed_len];
        {
            let mut reader = self
                .reader
                .lock()
                .map_err(|_| io::Error::other("XP3 reader lock poisoned"))?;
            reader.seek(SeekFrom::Start(segment.archive_offset))?;
            reader.read_exact(&mut compressed)?;
        }

        let mut decoder = ZlibDecoder::new(&compressed[..]);
        let mut decoded = Vec::with_capacity(expected_len);
        decoder.read_to_end(&mut decoded)?;
        if decoded.len() != expected_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "XP3 compressed segment decompressed to an unexpected size",
            ));
        }

        let decoded = Arc::<[u8]>::from(decoded.into_boxed_slice());
        self.segment_cache.insert(key, Arc::clone(&decoded))?;
        self.active_segment = Some(ActiveDecodedSegment {
            key,
            data: Arc::clone(&decoded),
        });
        Ok(decoded)
    }
}

impl<R> Read for Xp3EntryStream<R>
where
    R: Read + Seek + Send,
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.position >= self.file_size {
            return Ok(0);
        }

        let request_len = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        let target_len = request_len.min(self.file_size - self.position);
        let mut written = 0usize;

        while u64::try_from(written).unwrap_or(u64::MAX) < target_len {
            let segment_index = self.find_segment(self.position)?;
            let segment = self.segments[segment_index].clone();
            let offset_in_segment = self.position - segment.uncompressed_offset;
            let segment_remaining = segment.uncompressed_size - offset_in_segment;
            let output_remaining = target_len - u64::try_from(written).unwrap_or(u64::MAX);
            let chunk_len_u64 = segment_remaining.min(output_remaining);
            let chunk_len = usize_from_u64_io(chunk_len_u64, "XP3 read chunk is too large")?;
            let output = &mut buffer[written..written + chunk_len];

            match segment.encoding {
                Xp3SegmentEncoding::Raw => {
                    self.read_raw_segment(&segment, offset_in_segment, output)?;
                }
                Xp3SegmentEncoding::Zlib => {
                    let decoded = self.decompressed_segment(segment_index, &segment)?;
                    let start =
                        usize_from_u64_io(offset_in_segment, "XP3 segment offset is too large")?;
                    let end = start.checked_add(chunk_len).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "XP3 segment slice overflow")
                    })?;
                    output.copy_from_slice(decoded.get(start..end).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "XP3 segment slice is out of bounds",
                        )
                    })?);
                }
            }

            if let Some(filter) = &self.extraction_filter {
                filter.apply(self.position, output, self.file_hash);
            }

            self.position =
                checked_add_io(self.position, chunk_len_u64, "XP3 stream position overflow")?;
            written += chunk_len;
        }

        Ok(written)
    }
}

impl<R> Seek for Xp3EntryStream<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let new_position = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => i128::from(self.file_size) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };

        if new_position < 0 || new_position > i128::from(self.file_size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "XP3 seek target is outside the entry",
            ));
        }

        self.position = u64::try_from(new_position).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "XP3 seek target is too large")
        })?;
        Ok(self.position)
    }
}
