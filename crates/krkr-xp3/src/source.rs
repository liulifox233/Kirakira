use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    sync::Mutex,
};

pub(crate) trait ArchiveSource: Send + Sync {
    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()>;
}

pub(crate) struct SeekArchiveSource<R> {
    reader: Mutex<R>,
}

impl<R> SeekArchiveSource<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader: Mutex::new(reader),
        }
    }
}

impl<R> ArchiveSource for SeekArchiveSource<R>
where
    R: Read + Seek + Send,
{
    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let mut reader = self
            .reader
            .lock()
            .map_err(|_| io::Error::other("XP3 reader lock poisoned"))?;
        reader.seek(SeekFrom::Start(offset))?;
        reader.read_exact(buffer)
    }
}

#[cfg(any(unix, windows))]
pub(crate) struct FileArchiveSource {
    file: File,
}

#[cfg(any(unix, windows))]
impl FileArchiveSource {
    pub(crate) fn new(file: File) -> Self {
        Self { file }
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) struct FileArchiveSource {
    reader: Mutex<File>,
}

#[cfg(not(any(unix, windows)))]
impl FileArchiveSource {
    pub(crate) fn new(file: File) -> Self {
        Self {
            reader: Mutex::new(file),
        }
    }
}

#[cfg(any(unix, windows))]
impl ArchiveSource for FileArchiveSource {
    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        read_exact_positioned(&self.file, offset, buffer)
    }
}

#[cfg(not(any(unix, windows)))]
impl ArchiveSource for FileArchiveSource {
    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let mut reader = self
            .reader
            .lock()
            .map_err(|_| io::Error::other("XP3 reader lock poisoned"))?;
        reader.seek(SeekFrom::Start(offset))?;
        reader.read_exact(buffer)
    }
}

#[cfg(any(unix, windows))]
fn read_exact_positioned(file: &File, offset: u64, mut buffer: &mut [u8]) -> io::Result<()> {
    let mut read_offset = offset;
    while !buffer.is_empty() {
        let read = match read_positioned(file, read_offset, buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "failed to fill whole buffer",
            ));
        }
        read_offset = read_offset
            .checked_add(u64::try_from(read).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "XP3 read offset is too large")
            })?)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "XP3 read offset overflow")
            })?;
        buffer = &mut buffer[read..];
    }
    Ok(())
}

#[cfg(unix)]
fn read_positioned(file: &File, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;

    file.read_at(buffer, offset)
}

#[cfg(windows)]
fn read_positioned(file: &File, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;

    file.seek_read(buffer, offset)
}
