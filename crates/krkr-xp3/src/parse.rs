use std::io::{Read, Seek, SeekFrom};

use flate2::read::ZlibDecoder;

use crate::{
    Result, XP3_MAGIC, Xp3Entry, Xp3Error, Xp3Segment, Xp3SegmentEncoding,
    util::{
        checked_add, ensure_offset, ensure_range, le_u16, le_u32, le_u64, normalize_entry_name,
        read_u8, read_u32_at, read_u64, read_u64_at, usize_from_u64,
    },
};

pub(crate) const XP3_INDEX_ENCODE_RAW: u8 = 0;
pub(crate) const XP3_INDEX_ENCODE_ZLIB: u8 = 1;
pub(crate) const XP3_INDEX_CONTINUE: u8 = 0x80;

const XP3_INDEX_ENCODE_METHOD_MASK: u8 = 0x07;
const XP3_SEGM_ENCODE_METHOD_MASK: u32 = 0x07;
const XP3_SEGM_ENCODE_RAW: u32 = 0;
const XP3_SEGM_ENCODE_ZLIB: u32 = 1;

const CHUNK_HEADER_LEN: usize = 12;
const SEGMENT_RECORD_LEN: usize = 28;
const EXE_SCAN_CHUNK_LEN: usize = 256 * 1024;
const MAX_CONTINUOUS_INDEX_BLOCKS: usize = 4096;

pub(crate) fn find_xp3_base_offset<R>(reader: &mut R, file_len: u64) -> Result<u64>
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

pub(crate) fn read_entries<R>(
    reader: &mut R,
    base_offset: u64,
    file_len: u64,
) -> Result<Vec<Xp3Entry>>
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

pub(crate) fn parse_index(
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
