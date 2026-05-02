use std::{
    fs,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::{Compression, write::ZlibEncoder};
use krkr_core::ResourceProvider;

use crate::{
    SegmentCacheConfig, XP3_MAGIC, Xp3Archive, Xp3Error, Xp3OpenOptions, Xp3ResourceProvider,
    normalize_entry_name,
    parse::{XP3_INDEX_CONTINUE, XP3_INDEX_ENCODE_RAW, XP3_INDEX_ENCODE_ZLIB, parse_index},
};

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
fn opens_archive_from_borrowed_reader() {
    let archive_bytes = build_archive(
        &[FixtureEntry {
            name: "borrowed.ks",
            segments: vec![FixtureSegment::raw(b"borrowed")],
            hash: 0,
            time: None,
        }],
        BuildOptions::default(),
    );
    let borrowed_bytes = archive_bytes.as_slice();
    let archive = Xp3Archive::open(Cursor::new(borrowed_bytes)).expect("open borrowed archive");

    let mut stream = archive
        .open_by_name("borrowed.ks")
        .expect("open entry")
        .expect("entry exists");
    let mut contents = String::new();
    stream
        .read_to_string(&mut contents)
        .expect("read borrowed entry");

    assert_eq!(contents, "borrowed");
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
fn applies_configured_cache_limits_and_allows_clearing() {
    let archive = Xp3Archive::open_with_options(
        Cursor::new(build_archive(
            &[FixtureEntry {
                name: "data.bin",
                segments: vec![FixtureSegment::zlib(b"abcdef")],
                hash: 0,
                time: None,
            }],
            BuildOptions::default(),
        )),
        Xp3OpenOptions::new().with_segment_cache_config(SegmentCacheConfig::new(4, 4)),
    )
    .expect("open fixture");

    assert_eq!(
        archive.segment_cache_config(),
        SegmentCacheConfig::new(4, 4)
    );

    let mut stream = archive
        .open_by_name("data.bin")
        .expect("open entry")
        .expect("entry exists");
    let mut contents = Vec::new();
    stream.read_to_end(&mut contents).expect("read");
    assert_eq!(contents, b"abcdef");
    archive.clear_segment_cache().expect("clear cache");
}

#[test]
fn stream_keeps_active_compressed_segment_when_global_cache_skips_it() {
    let read_calls = Arc::new(AtomicUsize::new(0));
    let archive_data = build_archive(
        &[FixtureEntry {
            name: "large.bin",
            segments: vec![FixtureSegment::zlib(b"abcdefghijkl")],
            hash: 0,
            time: None,
        }],
        BuildOptions::default(),
    );
    let reader = CountingReader {
        inner: Cursor::new(archive_data),
        read_calls: Arc::clone(&read_calls),
    };
    let archive = Xp3Archive::open_with_options(
        reader,
        Xp3OpenOptions::new().with_segment_cache_config(SegmentCacheConfig::disabled()),
    )
    .expect("open fixture");
    read_calls.store(0, Ordering::Relaxed);

    let mut stream = archive
        .open_by_name("large.bin")
        .expect("open entry")
        .expect("entry exists");
    let mut contents = Vec::new();
    let mut chunk = [0; 2];
    loop {
        let read = stream.read(&mut chunk).expect("read chunk");
        if read == 0 {
            break;
        }
        contents.extend_from_slice(&chunk[..read]);
    }

    assert_eq!(contents, b"abcdefghijkl");
    assert_eq!(read_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn file_archive_supports_concurrent_independent_streams() {
    let root = temp_root("file-archive-concurrent");
    fs::create_dir_all(&root).expect("create temp dir");
    let archive_path = root.join("data.xp3");
    fs::write(
        &archive_path,
        build_archive(
            &[
                FixtureEntry {
                    name: "first.bin",
                    segments: vec![
                        FixtureSegment::raw(b"first"),
                        FixtureSegment::zlib(b"-data"),
                    ],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "second.bin",
                    segments: vec![
                        FixtureSegment::zlib(b"second"),
                        FixtureSegment::raw(b"-data"),
                    ],
                    hash: 0,
                    time: None,
                },
            ],
            BuildOptions::default(),
        ),
    )
    .expect("write archive");
    let archive = Xp3Archive::open_file(&archive_path).expect("open file archive");
    let cloned_archive = archive.clone();
    assert_eq!(cloned_archive.entries().len(), 2);
    let archive = Arc::new(cloned_archive);

    let handles = ["first.bin", "second.bin"].map(|name| {
        let archive = Arc::clone(&archive);
        thread::spawn(move || {
            let mut stream = archive
                .open_by_name(name)
                .expect("open stream")
                .expect("entry exists");
            let mut contents = Vec::new();
            stream.read_to_end(&mut contents).expect("read entry");
            contents
        })
    });

    let [first, second] = handles.map(|handle| handle.join().expect("reader thread should finish"));
    assert_eq!(first, b"first-data");
    assert_eq!(second, b"second-data");

    fs::remove_dir_all(root).expect("remove temp dir");
}

#[test]
fn xp3_resource_provider_reads_entries_with_patch_priority() {
    let root = temp_root("provider");
    fs::create_dir_all(&root).expect("create temp dir");
    let data_path = root.join("data.xp3");
    let patch_path = root.join("patch.xp3");
    fs::write(
        &data_path,
        build_archive(
            &[
                FixtureEntry {
                    name: "scenario/start.ks",
                    segments: vec![FixtureSegment::raw(b"base")],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "scenario/base_only.ks",
                    segments: vec![FixtureSegment::zlib(b"base-only")],
                    hash: 0,
                    time: None,
                },
            ],
            BuildOptions::default(),
        ),
    )
    .expect("write data archive");
    fs::write(
        &patch_path,
        build_archive(
            &[
                FixtureEntry {
                    name: "scenario/start.ks",
                    segments: vec![FixtureSegment::raw(b"patch")],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "scenario/patch_only.ks",
                    segments: vec![FixtureSegment::zlib(b"patch-only")],
                    hash: 0,
                    time: None,
                },
            ],
            BuildOptions::default(),
        ),
    )
    .expect("write patch archive");
    let provider =
        Xp3ResourceProvider::open_archives([&data_path, &patch_path]).expect("open provider");

    assert_eq!(provider.archive_count(), 2);
    assert_eq!(provider.entry_count(), 4);
    assert!(provider.exists("scenario\\start.ks"));
    assert!(provider.exists("SCENARIO\\START.KS"));
    assert!(provider.exists("scenario/base_only.ks"));
    assert!(provider.exists("scenario/patch_only.ks"));
    assert!(!provider.exists("../outside.ks"));
    assert!(!provider.exists("scenario/missing.ks"));

    let mut contents = String::new();
    provider
        .open("scenario/start.ks")
        .expect("open patched entry")
        .read_to_string(&mut contents)
        .expect("read patched entry");
    assert_eq!(contents, "patch");

    contents.clear();
    provider
        .open("SCENARIO/START.KS")
        .expect("open patched entry with different case")
        .read_to_string(&mut contents)
        .expect("read patched entry with different case");
    assert_eq!(contents, "patch");

    contents.clear();
    provider
        .open("scenario/base_only.ks")
        .expect("open base entry")
        .read_to_string(&mut contents)
        .expect("read base entry");
    assert_eq!(contents, "base-only");

    let error = match provider.open("scenario/missing.ks") {
        Ok(_) => panic!("missing entry should fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), io::ErrorKind::NotFound);

    fs::remove_dir_all(root).expect("remove temp dir");
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

struct CountingReader {
    inner: Cursor<Vec<u8>>,
    read_calls: Arc<AtomicUsize>,
}

impl Read for CountingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read > 0 {
            self.read_calls.fetch_add(1, Ordering::Relaxed);
        }
        Ok(read)
    }
}

impl Seek for CountingReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
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

fn write_index_block(archive: &mut Vec<u8>, index_data: &[u8], compressed: bool, continuous: bool) {
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

fn temp_root(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "Kirakira-xp3-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}
