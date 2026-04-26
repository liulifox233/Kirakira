use std::{
    collections::HashMap,
    error::Error,
    fmt,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};

use flate2::read::ZlibDecoder;

pub const XP3_MAGIC: [u8; 11] = [
    0x58, 0x50, 0x33, 0x0d, 0x0a, 0x20, 0x0a, 0x1a, 0x8b, 0x67, 0x01,
];

const XP3_INDEX_ENCODE_METHOD_MASK: u8 = 0x07;
const XP3_INDEX_ENCODE_RAW: u8 = 0;
const XP3_INDEX_ENCODE_ZLIB: u8 = 1;
const XP3_INDEX_CONTINUE: u8 = 0x80;

const XP3_SEGM_ENCODE_METHOD_MASK: u32 = 0x07;
const XP3_SEGM_ENCODE_RAW: u32 = 0;
const XP3_SEGM_ENCODE_ZLIB: u32 = 1;

const CHUNK_HEADER_LEN: usize = 12;
const SEGMENT_RECORD_LEN: usize = 28;
const EXE_SCAN_CHUNK_LEN: usize = 256 * 1024;
const MAX_CONTINUOUS_INDEX_BLOCKS: usize = 4096;

pub type Result<T> = std::result::Result<T, Xp3Error>;

#[derive(Debug)]
pub enum Xp3Error {
    Io(io::Error),
    InvalidArchive(&'static str),
    Unsupported(&'static str),
    InvalidPath(String),
    NotFound(String),
}

impl fmt::Display for Xp3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::InvalidArchive(message) => write!(f, "invalid XP3 archive: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported XP3 feature: {message}"),
            Self::InvalidPath(path) => write!(f, "invalid XP3 entry path: {path:?}"),
            Self::NotFound(path) => write!(f, "XP3 entry not found: {path}"),
        }
    }
}

impl Error for Xp3Error {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Xp3Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub trait Xp3ExtractionFilter: Send + Sync {
    fn apply(&self, uncompressed_offset: u64, buffer: &mut [u8], file_hash: u32);
}

impl<F> Xp3ExtractionFilter for F
where
    F: Fn(u64, &mut [u8], u32) + Send + Sync,
{
    fn apply(&self, uncompressed_offset: u64, buffer: &mut [u8], file_hash: u32) {
        self(uncompressed_offset, buffer, file_hash);
    }
}

#[derive(Clone, Default)]
pub struct Xp3OpenOptions {
    extraction_filter: Option<Arc<dyn Xp3ExtractionFilter>>,
}

impl Xp3OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_extraction_filter<F>(mut self, filter: F) -> Self
    where
        F: Xp3ExtractionFilter + 'static,
    {
        self.extraction_filter = Some(Arc::new(filter));
        self
    }
}

impl fmt::Debug for Xp3OpenOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Xp3OpenOptions")
            .field("extraction_filter", &self.extraction_filter.is_some())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Xp3SegmentEncoding {
    Raw,
    Zlib,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Xp3Segment {
    pub encoding: Xp3SegmentEncoding,
    pub archive_offset: u64,
    pub uncompressed_offset: u64,
    pub uncompressed_size: u64,
    pub archived_size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Xp3Entry {
    pub name: String,
    pub flags: u32,
    pub original_size: u64,
    pub archived_size: u64,
    pub file_hash: u32,
    pub modified_time: Option<u64>,
    pub segments: Vec<Xp3Segment>,
}

pub struct Xp3Archive<R> {
    reader: Arc<Mutex<R>>,
    entries: Vec<Xp3Entry>,
    by_name: HashMap<String, usize>,
    base_offset: u64,
    file_len: u64,
    extraction_filter: Option<Arc<dyn Xp3ExtractionFilter>>,
    segment_cache: Arc<SegmentCache>,
}

impl<R> Xp3Archive<R>
where
    R: Read + Seek + Send,
{
    pub fn open(reader: R) -> Result<Self> {
        Self::open_with_options(reader, Xp3OpenOptions::default())
    }

    pub fn open_with_options(mut reader: R, options: Xp3OpenOptions) -> Result<Self> {
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
            reader: Arc::new(Mutex::new(reader)),
            entries,
            by_name,
            base_offset,
            file_len,
            extraction_filter: options.extraction_filter,
            segment_cache: Arc::new(SegmentCache::default()),
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

        Ok(Xp3EntryStream {
            reader: Arc::clone(&self.reader),
            segments: entry.segments,
            file_size: entry.original_size,
            file_hash: entry.file_hash,
            position: 0,
            entry_index: index,
            file_len: self.file_len,
            extraction_filter: self.extraction_filter.clone(),
            segment_cache: Arc::clone(&self.segment_cache),
        })
    }

    pub fn base_offset(&self) -> u64 {
        self.base_offset
    }
}

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
        &self,
        segment_index: usize,
        segment: &Xp3Segment,
    ) -> io::Result<Arc<[u8]>> {
        let key = SegmentCacheKey {
            entry_index: self.entry_index,
            segment_index,
        };
        if let Some(data) = self.segment_cache.get(key)? {
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SegmentCacheKey {
    entry_index: usize,
    segment_index: usize,
}

#[derive(Default)]
struct SegmentCache {
    segments: Mutex<HashMap<SegmentCacheKey, Arc<[u8]>>>,
}

impl SegmentCache {
    fn get(&self, key: SegmentCacheKey) -> io::Result<Option<Arc<[u8]>>> {
        let segments = self
            .segments
            .lock()
            .map_err(|_| io::Error::other("XP3 segment cache lock poisoned"))?;
        Ok(segments.get(&key).cloned())
    }

    fn insert(&self, key: SegmentCacheKey, data: Arc<[u8]>) -> io::Result<()> {
        let mut segments = self
            .segments
            .lock()
            .map_err(|_| io::Error::other("XP3 segment cache lock poisoned"))?;
        segments.entry(key).or_insert(data);
        Ok(())
    }
}

pub fn normalize_entry_name(path: &str) -> Result<String> {
    let normalized_separators = path.replace('\\', "/");
    if normalized_separators.starts_with('/') {
        return Err(Xp3Error::InvalidPath(path.to_owned()));
    }

    let mut parts = Vec::new();
    for part in normalized_separators.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(Xp3Error::InvalidPath(path.to_owned())),
            part => parts.push(part),
        }
    }

    if parts.is_empty() {
        return Err(Xp3Error::InvalidPath(path.to_owned()));
    }

    Ok(parts.join("/"))
}

fn find_xp3_base_offset<R>(reader: &mut R, file_len: u64) -> Result<u64>
where
    R: Read + Seek,
{
    if file_len < XP3_MAGIC.len() as u64 {
        return Err(Xp3Error::InvalidArchive("file is too short"));
    }

    let mut header = [0; XP3_MAGIC.len()];
    reader.seek(SeekFrom::Start(0))?;
    reader.read_exact(&mut header)?;

    if header == XP3_MAGIC {
        return Ok(0);
    }

    if header[0] != b'M' || header[1] != b'Z' {
        return Err(Xp3Error::InvalidArchive("XP3 magic was not found"));
    }

    let mut scan_offset = 16u64;
    let mut buffer = vec![0; EXE_SCAN_CHUNK_LEN];
    while scan_offset < file_len {
        let read_len = (file_len - scan_offset).min(EXE_SCAN_CHUNK_LEN as u64) as usize;
        reader.seek(SeekFrom::Start(scan_offset))?;
        reader.read_exact(&mut buffer[..read_len])?;

        let mut position = 0usize;
        while position + XP3_MAGIC.len() <= read_len {
            if buffer[position..position + XP3_MAGIC.len()] == XP3_MAGIC {
                return Ok(scan_offset + position as u64);
            }
            position += 16;
        }

        scan_offset = checked_add(
            scan_offset,
            EXE_SCAN_CHUNK_LEN as u64,
            "EXE-bound XP3 scan offset overflow",
        )?;
    }

    Err(Xp3Error::InvalidArchive(
        "EXE-bound XP3 magic was not found",
    ))
}

fn read_entries<R>(reader: &mut R, base_offset: u64, file_len: u64) -> Result<Vec<Xp3Entry>>
where
    R: Read + Seek,
{
    let mut index_offset = initial_index_offset(reader, base_offset, file_len)?;
    let mut entries = Vec::new();

    for _ in 0..MAX_CONTINUOUS_INDEX_BLOCKS {
        let (index_flag, index_data) = read_index_block(reader, index_offset, file_len)?;
        parse_index(&index_data, base_offset, file_len, &mut entries)?;

        if index_flag & XP3_INDEX_CONTINUE == 0 {
            return Ok(entries);
        }

        let next_relative = read_u64(reader)?;
        index_offset = checked_add(base_offset, next_relative, "XP3 continued index overflow")?;
        ensure_offset(
            index_offset,
            file_len,
            "continued XP3 index offset is out of bounds",
        )?;
    }

    Err(Xp3Error::InvalidArchive("too many continuous XP3 indices"))
}

fn initial_index_offset<R>(reader: &mut R, base_offset: u64, file_len: u64) -> Result<u64>
where
    R: Read + Seek,
{
    let pointer_offset = checked_add(
        base_offset,
        XP3_MAGIC.len() as u64,
        "XP3 header offset overflow",
    )?;
    ensure_range(
        pointer_offset,
        8,
        file_len,
        "XP3 initial index pointer is out of bounds",
    )?;
    let initial_relative = read_u64_at(reader, pointer_offset)?;
    let initial_offset = checked_add(base_offset, initial_relative, "XP3 index offset overflow")?;
    ensure_offset(
        initial_offset,
        file_len,
        "XP3 index offset is out of bounds",
    )?;

    if checked_add(initial_offset, 4, "XP3 current header marker overflow")? <= file_len {
        let marker = read_u32_at(reader, initial_offset)?;
        if marker == 0x80 {
            let current_pointer_offset = checked_add(
                initial_offset,
                9,
                "XP3 current header pointer offset overflow",
            )?;
            ensure_range(
                current_pointer_offset,
                8,
                file_len,
                "XP3 current header index pointer is out of bounds",
            )?;
            let current_relative = read_u64_at(reader, current_pointer_offset)?;
            let current_offset = checked_add(
                base_offset,
                current_relative,
                "XP3 current index offset overflow",
            )?;
            ensure_offset(
                current_offset,
                file_len,
                "XP3 current index offset is out of bounds",
            )?;
            return Ok(current_offset);
        }
    }

    Ok(initial_offset)
}

fn read_index_block<R>(reader: &mut R, index_offset: u64, file_len: u64) -> Result<(u8, Vec<u8>)>
where
    R: Read + Seek,
{
    ensure_offset(index_offset, file_len, "XP3 index offset is out of bounds")?;
    reader.seek(SeekFrom::Start(index_offset))?;

    let index_flag = read_u8(reader)?;
    match index_flag & XP3_INDEX_ENCODE_METHOD_MASK {
        XP3_INDEX_ENCODE_RAW => {
            let index_size = read_u64(reader)?;
            let payload_offset = reader.stream_position()?;
            ensure_range(
                payload_offset,
                index_size,
                file_len,
                "XP3 raw index exceeds archive length",
            )?;
            let mut data = vec![0; usize_from_u64(index_size, "XP3 index is too large")?];
            reader.read_exact(&mut data)?;
            Ok((index_flag, data))
        }
        XP3_INDEX_ENCODE_ZLIB => {
            let compressed_size = read_u64(reader)?;
            let index_size = read_u64(reader)?;
            let payload_offset = reader.stream_position()?;
            ensure_range(
                payload_offset,
                compressed_size,
                file_len,
                "XP3 compressed index exceeds archive length",
            )?;
            let mut compressed =
                vec![0; usize_from_u64(compressed_size, "XP3 compressed index is too large")?];
            reader.read_exact(&mut compressed)?;

            let expected_len = usize_from_u64(index_size, "XP3 index is too large")?;
            let mut decoder = ZlibDecoder::new(&compressed[..]);
            let mut data = Vec::with_capacity(expected_len);
            decoder.read_to_end(&mut data)?;
            if data.len() != expected_len {
                return Err(Xp3Error::InvalidArchive(
                    "XP3 index decompressed to an unexpected size",
                ));
            }

            Ok((index_flag, data))
        }
        _ => Err(Xp3Error::Unsupported("unknown XP3 index encoding")),
    }
}

fn parse_index(
    index_data: &[u8],
    base_offset: u64,
    file_len: u64,
    entries: &mut Vec<Xp3Entry>,
) -> Result<()> {
    let mut cursor = 0usize;
    while cursor < index_data.len() {
        let chunk = read_chunk(index_data, cursor)?;
        if chunk.name == *b"File" {
            entries.push(parse_file_chunk(chunk.body, base_offset, file_len)?);
        }
        cursor = chunk.end;
    }
    Ok(())
}

fn parse_file_chunk(data: &[u8], base_offset: u64, file_len: u64) -> Result<Xp3Entry> {
    let mut info = None;
    let mut segments = None;
    let mut file_hash = 0;
    let mut modified_time = None;

    let mut cursor = 0usize;
    while cursor < data.len() {
        let chunk = read_chunk(data, cursor)?;
        match &chunk.name {
            b"info" => info = Some(parse_info_chunk(chunk.body)?),
            b"segm" => segments = Some(parse_segment_chunk(chunk.body, base_offset, file_len)?),
            b"adlr" => file_hash = parse_adlr_chunk(chunk.body)?,
            b"time" => modified_time = Some(parse_time_chunk(chunk.body)?),
            _ => {}
        }
        cursor = chunk.end;
    }

    let info = info.ok_or(Xp3Error::InvalidArchive("XP3 File chunk is missing info"))?;
    let segments = segments.ok_or(Xp3Error::InvalidArchive("XP3 File chunk is missing segm"))?;
    let segment_size = segments.iter().try_fold(0u64, |size, segment| {
        checked_add(size, segment.uncompressed_size, "XP3 segment size overflow")
    })?;
    if segment_size != info.original_size {
        return Err(Xp3Error::InvalidArchive(
            "XP3 segment sizes do not match info size",
        ));
    }

    Ok(Xp3Entry {
        name: info.name,
        flags: info.flags,
        original_size: info.original_size,
        archived_size: info.archived_size,
        file_hash,
        modified_time,
        segments,
    })
}

struct ParsedInfo {
    name: String,
    flags: u32,
    original_size: u64,
    archived_size: u64,
}

fn parse_info_chunk(data: &[u8]) -> Result<ParsedInfo> {
    if data.len() < 22 {
        return Err(Xp3Error::InvalidArchive("XP3 info chunk is too short"));
    }

    let flags = le_u32(data, 0)?;
    let original_size = le_u64(data, 4)?;
    let archived_size = le_u64(data, 12)?;
    let name_len = usize::from(le_u16(data, 20)?);
    let name_bytes = name_len
        .checked_mul(2)
        .ok_or(Xp3Error::InvalidArchive("XP3 entry name is too large"))?;
    let name_start = 22usize;
    let name_end = name_start
        .checked_add(name_bytes)
        .ok_or(Xp3Error::InvalidArchive("XP3 entry name overflow"))?;
    if name_end > data.len() {
        return Err(Xp3Error::InvalidArchive(
            "XP3 info chunk entry name exceeds chunk size",
        ));
    }

    let mut units = Vec::with_capacity(name_len);
    for bytes in data[name_start..name_end].chunks_exact(2) {
        units.push(u16::from_le_bytes([bytes[0], bytes[1]]));
    }
    let name = String::from_utf16(&units)
        .map_err(|_| Xp3Error::InvalidArchive("XP3 entry name is not valid UTF-16"))?;
    let name = normalize_entry_name(&name)?;

    Ok(ParsedInfo {
        name,
        flags,
        original_size,
        archived_size,
    })
}

fn parse_segment_chunk(data: &[u8], base_offset: u64, file_len: u64) -> Result<Vec<Xp3Segment>> {
    if !data.len().is_multiple_of(SEGMENT_RECORD_LEN) {
        return Err(Xp3Error::InvalidArchive(
            "XP3 segm chunk has a partial segment record",
        ));
    }

    let mut offset_in_entry = 0u64;
    let mut segments = Vec::with_capacity(data.len() / SEGMENT_RECORD_LEN);
    for record in data.chunks_exact(SEGMENT_RECORD_LEN) {
        let flags = le_u32(record, 0)?;
        let encoding = match flags & XP3_SEGM_ENCODE_METHOD_MASK {
            XP3_SEGM_ENCODE_RAW => Xp3SegmentEncoding::Raw,
            XP3_SEGM_ENCODE_ZLIB => Xp3SegmentEncoding::Zlib,
            _ => return Err(Xp3Error::Unsupported("unknown XP3 segment encoding")),
        };

        let relative_offset = le_u64(record, 4)?;
        let archive_offset = checked_add(
            base_offset,
            relative_offset,
            "XP3 segment archive offset overflow",
        )?;
        let uncompressed_size = le_u64(record, 12)?;
        let archived_size = le_u64(record, 20)?;
        ensure_range(
            archive_offset,
            archived_size,
            file_len,
            "XP3 segment exceeds archive length",
        )?;
        if encoding == Xp3SegmentEncoding::Raw && archived_size < uncompressed_size {
            return Err(Xp3Error::InvalidArchive(
                "XP3 raw segment is smaller than its uncompressed size",
            ));
        }

        segments.push(Xp3Segment {
            encoding,
            archive_offset,
            uncompressed_offset: offset_in_entry,
            uncompressed_size,
            archived_size,
        });
        offset_in_entry = checked_add(
            offset_in_entry,
            uncompressed_size,
            "XP3 entry segment offset overflow",
        )?;
    }

    Ok(segments)
}

fn parse_adlr_chunk(data: &[u8]) -> Result<u32> {
    if data.len() != 4 {
        return Err(Xp3Error::InvalidArchive("XP3 adlr chunk size is invalid"));
    }
    le_u32(data, 0)
}

fn parse_time_chunk(data: &[u8]) -> Result<u64> {
    if data.len() != 8 {
        return Err(Xp3Error::InvalidArchive("XP3 time chunk size is invalid"));
    }
    le_u64(data, 0)
}

#[derive(Clone, Copy)]
struct Chunk<'a> {
    name: [u8; 4],
    body: &'a [u8],
    end: usize,
}

fn read_chunk(data: &[u8], cursor: usize) -> Result<Chunk<'_>> {
    let header_end = cursor
        .checked_add(CHUNK_HEADER_LEN)
        .ok_or(Xp3Error::InvalidArchive("XP3 chunk header overflow"))?;
    if header_end > data.len() {
        return Err(Xp3Error::InvalidArchive("XP3 chunk header is truncated"));
    }

    let name = data[cursor..cursor + 4]
        .try_into()
        .expect("chunk name has a fixed width");
    let size = usize_from_u64(le_u64(data, cursor + 4)?, "XP3 chunk is too large")?;
    let body_start = header_end;
    let body_end = body_start
        .checked_add(size)
        .ok_or(Xp3Error::InvalidArchive("XP3 chunk size overflow"))?;
    if body_end > data.len() {
        return Err(Xp3Error::InvalidArchive("XP3 chunk exceeds parent size"));
    }

    Ok(Chunk {
        name,
        body: &data[body_start..body_end],
        end: body_end,
    })
}

fn read_u8<R>(reader: &mut R) -> io::Result<u8>
where
    R: Read,
{
    let mut buffer = [0; 1];
    reader.read_exact(&mut buffer)?;
    Ok(buffer[0])
}

fn read_u64<R>(reader: &mut R) -> io::Result<u64>
where
    R: Read,
{
    let mut buffer = [0; 8];
    reader.read_exact(&mut buffer)?;
    Ok(u64::from_le_bytes(buffer))
}

fn read_u32_at<R>(reader: &mut R, offset: u64) -> io::Result<u32>
where
    R: Read + Seek,
{
    let mut buffer = [0; 4];
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(&mut buffer)?;
    Ok(u32::from_le_bytes(buffer))
}

fn read_u64_at<R>(reader: &mut R, offset: u64) -> io::Result<u64>
where
    R: Read + Seek,
{
    let mut buffer = [0; 8];
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(&mut buffer)?;
    Ok(u64::from_le_bytes(buffer))
}

fn le_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or(Xp3Error::InvalidArchive(
            "XP3 little-endian u16 is truncated",
        ))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn le_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or(Xp3Error::InvalidArchive(
            "XP3 little-endian u32 is truncated",
        ))?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn le_u64(data: &[u8], offset: usize) -> Result<u64> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or(Xp3Error::InvalidArchive(
            "XP3 little-endian u64 is truncated",
        ))?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

fn checked_add(left: u64, right: u64, message: &'static str) -> Result<u64> {
    left.checked_add(right)
        .ok_or(Xp3Error::InvalidArchive(message))
}

fn checked_add_io(left: u64, right: u64, message: &'static str) -> io::Result<u64> {
    left.checked_add(right)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, message))
}

fn ensure_offset(offset: u64, file_len: u64, message: &'static str) -> Result<()> {
    if offset >= file_len {
        return Err(Xp3Error::InvalidArchive(message));
    }
    Ok(())
}

fn ensure_range(offset: u64, len: u64, file_len: u64, message: &'static str) -> Result<()> {
    let end = checked_add(offset, len, message)?;
    if end > file_len {
        return Err(Xp3Error::InvalidArchive(message));
    }
    Ok(())
}

fn ensure_range_io(offset: u64, len: u64, file_len: u64, message: &'static str) -> io::Result<()> {
    let end = checked_add_io(offset, len, message)?;
    if end > file_len {
        return Err(io::Error::new(io::ErrorKind::InvalidData, message));
    }
    Ok(())
}

fn usize_from_u64(value: u64, message: &'static str) -> Result<usize> {
    usize::try_from(value).map_err(|_| Xp3Error::InvalidArchive(message))
}

fn usize_from_u64_io(value: u64, message: &'static str) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, message))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Seek, Write};

    use flate2::{Compression, write::ZlibEncoder};

    use super::*;

    #[test]
    fn opens_raw_archive_and_normalizes_paths() {
        let archive = Xp3Archive::open(Cursor::new(build_archive(
            &[FixtureEntry {
                name: "scenario\\./start.ks",
                segments: vec![FixtureSegment::raw(b"hello")],
                hash: 0x1234,
                time: Some(42),
            }],
            BuildOptions::default(),
        )))
        .expect("open fixture");

        assert_eq!(archive.entries().len(), 1);
        assert_eq!(archive.entries()[0].name, "scenario/start.ks");
        assert_eq!(archive.entries()[0].file_hash, 0x1234);
        assert_eq!(archive.entries()[0].modified_time, Some(42));
        assert!(archive.get_entry("scenario/start.ks").is_some());
        assert!(archive.get_entry("scenario\\start.ks").is_some());

        let mut stream = archive
            .open_by_name("scenario/start.ks")
            .expect("open entry")
            .expect("entry exists");
        let mut contents = String::new();
        stream
            .read_to_string(&mut contents)
            .expect("read entry contents");
        assert_eq!(contents, "hello");
    }

    #[test]
    fn opens_exe_bound_current_header_archive() {
        let archive = Xp3Archive::open(Cursor::new(build_archive(
            &[FixtureEntry {
                name: "startup.tjs",
                segments: vec![FixtureSegment::raw(b"startup")],
                hash: 7,
                time: None,
            }],
            BuildOptions {
                current_header: true,
                exe_prefix_len: 32,
                ..BuildOptions::default()
            },
        )))
        .expect("open fixture");

        assert_eq!(archive.base_offset(), 32);
        let mut stream = archive
            .open_by_name("startup.tjs")
            .expect("open entry")
            .expect("entry exists");
        let mut contents = Vec::new();
        stream.read_to_end(&mut contents).expect("read entry");
        assert_eq!(contents, b"startup");
    }

    #[test]
    fn reads_multi_segment_stream_and_seek() {
        let archive = Xp3Archive::open(Cursor::new(build_archive(
            &[FixtureEntry {
                name: "data.bin",
                segments: vec![FixtureSegment::raw(b"abc"), FixtureSegment::zlib(b"defgh")],
                hash: 0xbeef,
                time: None,
            }],
            BuildOptions {
                compressed_index: true,
                ..BuildOptions::default()
            },
        )))
        .expect("open fixture");
        let mut stream = archive
            .open_by_name("data.bin")
            .expect("open entry")
            .expect("entry exists");

        let mut contents = Vec::new();
        stream.read_to_end(&mut contents).expect("read all");
        assert_eq!(contents, b"abcdefgh");

        stream.seek(SeekFrom::Start(2)).expect("seek");
        let mut slice = [0; 4];
        stream.read_exact(&mut slice).expect("read slice");
        assert_eq!(&slice, b"cdef");

        stream.seek(SeekFrom::End(-3)).expect("seek from end");
        let mut tail = Vec::new();
        stream.read_to_end(&mut tail).expect("read tail");
        assert_eq!(tail, b"fgh");
    }

    #[test]
    fn reads_continuous_indices() {
        let archive = Xp3Archive::open(Cursor::new(build_archive(
            &[
                FixtureEntry {
                    name: "first.ks",
                    segments: vec![FixtureSegment::raw(b"one")],
                    hash: 1,
                    time: None,
                },
                FixtureEntry {
                    name: "second.ks",
                    segments: vec![FixtureSegment::raw(b"two")],
                    hash: 2,
                    time: None,
                },
            ],
            BuildOptions {
                continuous_after: Some(1),
                ..BuildOptions::default()
            },
        )))
        .expect("open fixture");

        assert_eq!(archive.entries().len(), 2);
        let mut stream = archive
            .open_by_name("second.ks")
            .expect("open entry")
            .expect("entry exists");
        let mut contents = String::new();
        stream.read_to_string(&mut contents).expect("read entry");
        assert_eq!(contents, "two");
    }

    #[test]
    fn rejects_corrupt_chunk_size() {
        let mut index = Vec::new();
        index.extend_from_slice(b"File");
        push_u64(&mut index, 99);

        let mut entries = Vec::new();
        let error = parse_index(&index, 0, 100, &mut entries).expect_err("reject corrupt index");
        assert!(matches!(error, Xp3Error::InvalidArchive(_)));
    }

    #[test]
    fn rejects_parent_paths_during_normalization() {
        assert_eq!(
            normalize_entry_name("./scenario\\start.ks").expect("normalize"),
            "scenario/start.ks"
        );
        assert!(normalize_entry_name("../secret.ks").is_err());
        assert!(normalize_entry_name("/absolute.ks").is_err());
    }

    #[derive(Clone)]
    struct FixtureEntry<'a> {
        name: &'a str,
        segments: Vec<FixtureSegment<'a>>,
        hash: u32,
        time: Option<u64>,
    }

    #[derive(Clone)]
    struct FixtureSegment<'a> {
        data: &'a [u8],
        compressed: bool,
    }

    impl<'a> FixtureSegment<'a> {
        fn raw(data: &'a [u8]) -> Self {
            Self {
                data,
                compressed: false,
            }
        }

        fn zlib(data: &'a [u8]) -> Self {
            Self {
                data,
                compressed: true,
            }
        }
    }

    #[derive(Clone, Copy, Default)]
    struct BuildOptions {
        compressed_index: bool,
        current_header: bool,
        exe_prefix_len: usize,
        continuous_after: Option<usize>,
    }

    struct BuiltEntry<'a> {
        source: &'a FixtureEntry<'a>,
        segments: Vec<BuiltSegment>,
        original_size: u64,
        archived_size: u64,
    }

    struct BuiltSegment {
        relative_offset: u64,
        original_size: u64,
        archived_size: u64,
        compressed: bool,
    }

    fn build_archive(entries: &[FixtureEntry<'_>], options: BuildOptions) -> Vec<u8> {
        let mut archive = Vec::new();
        if options.exe_prefix_len > 0 {
            assert_eq!(options.exe_prefix_len % 16, 0);
            archive.extend_from_slice(b"MZ");
            archive.resize(options.exe_prefix_len, 0);
        }

        let base_offset = archive.len();
        archive.extend_from_slice(&XP3_MAGIC);
        let index_pointer_offset = if options.current_header {
            push_u64(&mut archive, 0x17);
            push_u32(&mut archive, 1);
            archive.push(0x80);
            push_u64(&mut archive, 0);
            let offset = archive.len();
            push_u64(&mut archive, 0);
            offset
        } else {
            let offset = archive.len();
            push_u64(&mut archive, 0);
            offset
        };

        let mut built_entries = Vec::new();
        for entry in entries {
            let mut built_segments = Vec::new();
            let mut original_size = 0u64;
            let mut archived_size = 0u64;

            for segment in &entry.segments {
                let encoded = if segment.compressed {
                    zlib(segment.data)
                } else {
                    segment.data.to_vec()
                };
                let relative_offset =
                    u64::try_from(archive.len() - base_offset).expect("fixture offset fits in u64");
                archive.extend_from_slice(&encoded);

                let segment_original_size =
                    u64::try_from(segment.data.len()).expect("fixture size fits in u64");
                let segment_archived_size =
                    u64::try_from(encoded.len()).expect("fixture size fits in u64");
                original_size += segment_original_size;
                archived_size += segment_archived_size;
                built_segments.push(BuiltSegment {
                    relative_offset,
                    original_size: segment_original_size,
                    archived_size: segment_archived_size,
                    compressed: segment.compressed,
                });
            }

            built_entries.push(BuiltEntry {
                source: entry,
                segments: built_segments,
                original_size,
                archived_size,
            });
        }

        let index_offset = u64::try_from(archive.len() - base_offset).expect("fixture offset fits");
        if let Some(split) = options.continuous_after {
            write_index_block(
                &mut archive,
                &build_index_data(&built_entries[..split]),
                options.compressed_index,
                true,
            );
            let next_index_offset =
                u64::try_from(archive.len() + 8 - base_offset).expect("fixture offset fits");
            push_u64(&mut archive, next_index_offset);
            write_index_block(
                &mut archive,
                &build_index_data(&built_entries[split..]),
                options.compressed_index,
                false,
            );
        } else {
            write_index_block(
                &mut archive,
                &build_index_data(&built_entries),
                options.compressed_index,
                false,
            );
        }
        write_u64_at(&mut archive, index_pointer_offset, index_offset);

        archive
    }

    fn build_index_data(entries: &[BuiltEntry<'_>]) -> Vec<u8> {
        let mut index = Vec::new();
        for entry in entries {
            let mut file = Vec::new();

            let mut info = Vec::new();
            push_u32(&mut info, 0);
            push_u64(&mut info, entry.original_size);
            push_u64(&mut info, entry.archived_size);
            let name: Vec<u16> = entry.source.name.encode_utf16().collect();
            push_u16(
                &mut info,
                u16::try_from(name.len()).expect("fixture name fits"),
            );
            for unit in name {
                push_u16(&mut info, unit);
            }
            push_chunk(&mut file, b"info", &info);
            push_chunk(&mut file, b"unkn", &[1, 2, 3]);

            let mut segm = Vec::new();
            for segment in &entry.segments {
                push_u32(&mut segm, if segment.compressed { 1 } else { 0 });
                push_u64(&mut segm, segment.relative_offset);
                push_u64(&mut segm, segment.original_size);
                push_u64(&mut segm, segment.archived_size);
            }
            push_chunk(&mut file, b"segm", &segm);

            let mut adlr = Vec::new();
            push_u32(&mut adlr, entry.source.hash);
            push_chunk(&mut file, b"adlr", &adlr);

            if let Some(time) = entry.source.time {
                let mut time_chunk = Vec::new();
                push_u64(&mut time_chunk, time);
                push_chunk(&mut file, b"time", &time_chunk);
            }

            push_chunk(&mut index, b"File", &file);
        }
        index
    }

    fn write_index_block(
        archive: &mut Vec<u8>,
        index_data: &[u8],
        compressed: bool,
        continuous: bool,
    ) {
        let mut flag = if compressed {
            XP3_INDEX_ENCODE_ZLIB
        } else {
            XP3_INDEX_ENCODE_RAW
        };
        if continuous {
            flag |= XP3_INDEX_CONTINUE;
        }
        archive.push(flag);

        if compressed {
            let compressed_index = zlib(index_data);
            push_u64(
                archive,
                u64::try_from(compressed_index.len()).expect("fixture index fits"),
            );
            push_u64(
                archive,
                u64::try_from(index_data.len()).expect("fixture index fits"),
            );
            archive.extend_from_slice(&compressed_index);
        } else {
            push_u64(
                archive,
                u64::try_from(index_data.len()).expect("fixture index fits"),
            );
            archive.extend_from_slice(index_data);
        }
    }

    fn push_chunk(output: &mut Vec<u8>, name: &[u8; 4], body: &[u8]) {
        output.extend_from_slice(name);
        push_u64(
            output,
            u64::try_from(body.len()).expect("fixture chunk fits"),
        );
        output.extend_from_slice(body);
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).expect("compress fixture");
        encoder.finish().expect("finish fixture compression")
    }

    fn push_u16(output: &mut Vec<u8>, value: u16) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u64(output: &mut Vec<u8>, value: u64) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn write_u64_at(output: &mut [u8], offset: usize, value: u64) {
        output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
