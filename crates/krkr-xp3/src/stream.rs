use std::{
    io::{self, Read, Seek, SeekFrom},
    marker::PhantomData,
    sync::Arc,
};

use flate2::read::ZlibDecoder;
use krkr_core::{
    Xp3ContentFilterAction, Xp3ExtractionFilterInfo, Xp3FilterContext, Xp3FilterRegistry,
};

use crate::{
    Xp3Entry, Xp3Segment, Xp3SegmentEncoding,
    cache::{SegmentCache, SegmentCacheKey},
    source::ArchiveSourceHandle,
    util::{checked_add_io, ensure_range_io, usize_from_u64_io},
};

pub struct Xp3EntryStream<R> {
    reader: ArchiveSourceHandle<R>,
    segments: Vec<Xp3Segment>,
    file_size: u64,
    file_hash: u32,
    entry_name: String,
    position: u64,
    entry_index: usize,
    file_len: u64,
    /// The filter slots, read live on every chunk — the reference consults
    /// `TVPXP3ArchiveExtractionFilter` inside `Read` (`XP3Archive.cpp:1047`),
    /// never once per stream.
    filters: Arc<Xp3FilterRegistry>,
    /// The stream's `FilterContext` (`XP3Archive.cpp:586`, `:1054`): the
    /// content filter seeds it when the stream is created, every extraction
    /// call of this stream sees it.
    filter_context: Xp3FilterContext,
    /// Set when the content filter answered
    /// [`Xp3ContentFilterAction::FetchFull`]: the whole entry was read into
    /// memory at creation, through the extraction filter as it stood then,
    /// and reads are served from those bytes (`XP3Archive.cpp:585-595`).
    full_data: Option<Arc<[u8]>>,
    segment_cache: Arc<SegmentCache>,
    active_segment: Option<ActiveDecodedSegment>,
    reader_type: PhantomData<fn() -> R>,
}

struct ActiveDecodedSegment {
    key: SegmentCacheKey,
    data: Arc<[u8]>,
}

impl<R> Xp3EntryStream<R> {
    pub(crate) fn new(
        reader: ArchiveSourceHandle<R>,
        segment_cache: Arc<SegmentCache>,
        filters: Arc<Xp3FilterRegistry>,
        entry_index: usize,
        entry: Xp3Entry,
        file_len: u64,
    ) -> Self {
        Self {
            reader,
            segments: entry.segments,
            file_size: entry.original_size,
            file_hash: entry.file_hash,
            entry_name: entry.name,
            position: 0,
            entry_index,
            file_len,
            filters,
            filter_context: Xp3FilterContext::new(),
            full_data: None,
            segment_cache,
            active_segment: None,
            reader_type: PhantomData,
        }
    }

    /// Creates a stream the way the reference's `CreateStreamByIndex` does
    /// (`XP3Archive.cpp:576-604`): the content filter is asked about the entry
    /// first — with this stream's fresh `FilterContext` — and when it answers
    /// `FetchFull` the entry is read whole into memory and the caller reads
    /// those bytes.
    pub(crate) fn open(
        reader: ArchiveSourceHandle<R>,
        segment_cache: Arc<SegmentCache>,
        filters: Arc<Xp3FilterRegistry>,
        entry_index: usize,
        entry: Xp3Entry,
        file_len: u64,
        archive_name: &str,
    ) -> io::Result<Self>
    where
        R: Read + Seek + Send,
    {
        let mut stream = Self::new(reader, segment_cache, filters, entry_index, entry, file_len);
        if let Some(filter) = stream.filters.content_filter() {
            let action = filter.apply(
                &stream.entry_name,
                archive_name,
                stream.file_size,
                &mut stream.filter_context,
            );
            if action == Xp3ContentFilterAction::FetchFull {
                stream.fetch_full_entry()?;
            }
        }
        Ok(stream)
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

        self.reader.read_exact_at(file_offset, output)
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
        self.reader
            .read_exact_at(segment.archive_offset, &mut compressed)?;

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

impl<R> Xp3EntryStream<R>
where
    R: Read + Seek + Send,
{
    /// The archive read path of `tTVPXP3ArchiveStream::Read`
    /// (`XP3Archive.cpp:1040-1067`): walk the segments from the current
    /// position and hand every chunk the extraction filter as it stands
    /// *now*, with the stream's context.
    fn read_archive_chunk(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
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

            if let Some(filter) = self.filters.extraction_filter() {
                filter.apply(
                    Xp3ExtractionFilterInfo {
                        offset: self.position,
                        buffer: output,
                        file_hash: self.file_hash,
                        file_name: &self.entry_name,
                    },
                    &mut self.filter_context,
                );
            }

            self.position =
                checked_add_io(self.position, chunk_len_u64, "XP3 stream position overflow")?;
            written += chunk_len;
        }

        Ok(written)
    }

    /// `XP3_CONTENT_FILTER_FETCH_FULLDATA` (`XP3Archive.cpp:589-593`): read the
    /// whole entry through the archive path — so the extraction filter sees
    /// every chunk, with this stream's context, exactly as it does when the
    /// reference reads into `tTVPMemoryStream` — and serve reads from it.
    fn fetch_full_entry(&mut self) -> io::Result<()> {
        let len = usize_from_u64_io(self.file_size, "XP3 entry is too large to fetch whole")?;
        let mut data = vec![0u8; len];
        let mut filled = 0usize;
        while filled < len {
            let read = self.read_archive_chunk(&mut data[filled..])?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        if filled != len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "XP3 entry ended before its declared size",
            ));
        }
        self.full_data = Some(Arc::from(data.into_boxed_slice()));
        self.position = 0;
        Ok(())
    }
}

impl<R> Read for Xp3EntryStream<R>
where
    R: Read + Seek + Send,
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(data) = &self.full_data {
            let position = usize::try_from(self.position).unwrap_or(usize::MAX);
            let remaining = data.get(position..).unwrap_or_default();
            let len = remaining.len().min(buffer.len());
            buffer[..len].copy_from_slice(&remaining[..len]);
            self.position += u64::try_from(len).unwrap_or(0);
            return Ok(len);
        }
        self.read_archive_chunk(buffer)
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
