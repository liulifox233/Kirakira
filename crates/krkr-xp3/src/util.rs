use std::{
    error::Error,
    fmt,
    io::{self, Read, Seek, SeekFrom},
};

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

pub(crate) fn read_u8<R>(reader: &mut R) -> io::Result<u8>
where
    R: Read,
{
    let mut buffer = [0; 1];
    reader.read_exact(&mut buffer)?;
    Ok(buffer[0])
}

pub(crate) fn read_u64<R>(reader: &mut R) -> io::Result<u64>
where
    R: Read,
{
    let mut buffer = [0; 8];
    reader.read_exact(&mut buffer)?;
    Ok(u64::from_le_bytes(buffer))
}

pub(crate) fn read_u32_at<R>(reader: &mut R, offset: u64) -> io::Result<u32>
where
    R: Read + Seek,
{
    let mut buffer = [0; 4];
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(&mut buffer)?;
    Ok(u32::from_le_bytes(buffer))
}

pub(crate) fn read_u64_at<R>(reader: &mut R, offset: u64) -> io::Result<u64>
where
    R: Read + Seek,
{
    let mut buffer = [0; 8];
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(&mut buffer)?;
    Ok(u64::from_le_bytes(buffer))
}

pub(crate) fn le_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or(Xp3Error::InvalidArchive(
            "XP3 little-endian u16 is truncated",
        ))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

pub(crate) fn le_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or(Xp3Error::InvalidArchive(
            "XP3 little-endian u32 is truncated",
        ))?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(crate) fn le_u64(data: &[u8], offset: usize) -> Result<u64> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or(Xp3Error::InvalidArchive(
            "XP3 little-endian u64 is truncated",
        ))?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

pub(crate) fn checked_add(left: u64, right: u64, message: &'static str) -> Result<u64> {
    left.checked_add(right)
        .ok_or(Xp3Error::InvalidArchive(message))
}

pub(crate) fn checked_add_io(left: u64, right: u64, message: &'static str) -> io::Result<u64> {
    left.checked_add(right)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, message))
}

pub(crate) fn ensure_offset(offset: u64, file_len: u64, message: &'static str) -> Result<()> {
    if offset >= file_len {
        return Err(Xp3Error::InvalidArchive(message));
    }
    Ok(())
}

pub(crate) fn ensure_range(
    offset: u64,
    len: u64,
    file_len: u64,
    message: &'static str,
) -> Result<()> {
    let end = checked_add(offset, len, message)?;
    if end > file_len {
        return Err(Xp3Error::InvalidArchive(message));
    }
    Ok(())
}

pub(crate) fn ensure_range_io(
    offset: u64,
    len: u64,
    file_len: u64,
    message: &'static str,
) -> io::Result<()> {
    let end = checked_add_io(offset, len, message)?;
    if end > file_len {
        return Err(io::Error::new(io::ErrorKind::InvalidData, message));
    }
    Ok(())
}

pub(crate) fn usize_from_u64(value: u64, message: &'static str) -> Result<usize> {
    usize::try_from(value).map_err(|_| Xp3Error::InvalidArchive(message))
}

pub(crate) fn usize_from_u64_io(value: u64, message: &'static str) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, message))
}
