use std::{
    fs::{self, File},
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::{Compression, write::ZlibEncoder};
use krkr_core::StoragePort;

use crate::{
    SegmentCacheConfig, XP3_MAGIC, Xp3Archive, Xp3ContentFilterAction, Xp3Entry, Xp3Error,
    Xp3ExtractionFilterInfo, Xp3FilterContext, Xp3FilterRegistry, Xp3OpenOptions,
    Xp3ResourceProvider, normalize_entry_name,
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

/// The provider probe used to normalize once and then let every archive
/// normalize again for its exact map and once more for the lowercased map.
/// `Xp3Archive::get_entry`/`get_entry_ascii_case_insensitive` still have
/// exactly those per-call semantics, so separately-opened handles of the same
/// archives are the pre-change reference the hoisted provider must reproduce.
fn per_archive_probe_entry<'a>(
    archives: &'a [Xp3Archive<File>],
    path: &str,
) -> Option<&'a Xp3Entry> {
    let normalized = normalize_entry_name(path).ok()?;
    for archive in archives.iter().rev() {
        if let Some(entry) = archive.get_entry(&normalized) {
            return Some(entry);
        }
        if let Some(entry) = archive.get_entry_ascii_case_insensitive(&normalized) {
            return Some(entry);
        }
    }
    None
}

/// The pre-change `StoragePort::open` over a reference archive list: the
/// entry name the per-archive walk selects and the bytes it serves.
fn per_archive_probe_open(archives: &[Xp3Archive<File>], path: &str) -> Option<(String, Vec<u8>)> {
    let normalized = normalize_entry_name(path).ok()?;
    for archive in archives.iter().rev() {
        let entry_name = if archive.get_entry(&normalized).is_some() {
            normalized.clone()
        } else if let Some(entry) = archive.get_entry_ascii_case_insensitive(&normalized) {
            entry.name.clone()
        } else {
            continue;
        };
        let mut stream = archive.open_by_name(&entry_name).ok().flatten()?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).ok()?;
        return Some((entry_name, bytes));
    }
    None
}

/// Mirrors `Xp3ResourceProvider::archive_index`, which the probe hoist does
/// not touch: the reference walk has to visit the mount the provider resolved.
///
/// A qualifier is a storage path, not a file name — `TVPRebuildAutoPathTable`
/// reads the archive at the spelling before `>` (`StorageIntf.cpp:1055-1066`)
/// and `_TVPCreateStream` opens exactly that file (`:1249-1260`) — so an
/// absolute qualifier matches a mount's path, a relative path with a directory
/// matches a mount's path below `base`, and a bare name matches the mount
/// directly below `base` before falling back to a file-name match.
fn reference_archive_index(base: &Path, paths: &[&PathBuf], archive: &str) -> Option<usize> {
    fn normalize(path: &str) -> String {
        let folded = path.replace('\\', "/");
        let absolute = folded.starts_with('/');
        let body = folded
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<_>>()
            .join("/")
            .to_ascii_lowercase();
        if absolute { format!("/{body}") } else { body }
    }

    let query = normalize(archive.strip_prefix("file://").unwrap_or(archive));
    if query.is_empty() {
        return None;
    }
    let mounts = paths
        .iter()
        .map(|path| {
            let logical = if path.is_relative() {
                Some(normalize(&path.to_string_lossy()))
            } else {
                path.strip_prefix(base)
                    .ok()
                    .map(|relative| normalize(&relative.to_string_lossy()))
            };
            (logical, normalize(&path.to_string_lossy()))
        })
        .collect::<Vec<_>>();
    if query.starts_with('/') {
        return mounts.iter().rposition(|(_, path)| path == &query);
    }
    if query.contains('/') {
        return mounts
            .iter()
            .rposition(|(logical, _)| logical.as_deref() == Some(query.as_str()));
    }
    mounts
        .iter()
        .rposition(|(logical, _)| logical.as_deref() == Some(query.as_str()))
        .or_else(|| {
            mounts
                .iter()
                .rposition(|(_, path)| path.rsplit('/').next().is_some_and(|name| name == query))
        })
}

/// The pre-change lookup inside one named archive: normalize once and let the
/// archive normalize again for each of its two maps.
fn per_archive_probe_entry_in<'a>(
    archives: &'a [Xp3Archive<File>],
    index: usize,
    path: &str,
) -> Option<&'a Xp3Entry> {
    let normalized = normalize_entry_name(path).ok()?;
    let archive = &archives[index];
    archive
        .get_entry(&normalized)
        .or_else(|| archive.get_entry_ascii_case_insensitive(&normalized))
}

/// Reads all bytes a probe resolves to, failing the test on a miss.
fn read_provider_entry(provider: &Xp3ResourceProvider, path: &str) -> Vec<u8> {
    let mut stream = provider.open(path).expect("open provider entry");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read provider entry");
    bytes
}

/// The probe hoist is an optimisation, not a rule change: across case
/// variants, separator spellings, extensionless and extended bare names and
/// odd or invalid paths, `get_entry`, `get_entry_in` and `open` answer
/// exactly like the pre-change walk that re-normalized inside every archive.
#[test]
fn hoisted_probe_lookups_agree_with_per_archive_normalization() {
    let root = temp_root("probe-normalization");
    fs::create_dir_all(&root).expect("create temp dir");
    let data_path = root.join("data.xp3");
    let patch_path = root.join("patch.xp3");
    let extra_path = root.join("extra.xp3");
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
                    segments: vec![FixtureSegment::raw(b"base-only")],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "CASE.txt",
                    segments: vec![FixtureSegment::raw(b"case-data")],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "dup.bin",
                    segments: vec![FixtureSegment::raw(b"dup-data")],
                    hash: 0,
                    time: None,
                },
                // A case-collapsed pair inside one mount: the lowercased map
                // keeps the first entry in name order, so a mixed-case probe
                // reaches `THING.txt` while the exact spelling reaches
                // `thing.txt`.
                FixtureEntry {
                    name: "THING.txt",
                    segments: vec![FixtureSegment::raw(b"upper-thing")],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "thing.txt",
                    segments: vec![FixtureSegment::raw(b"lower-thing")],
                    hash: 0,
                    time: None,
                },
                // `to_ascii_lowercase` folds ASCII only.
                FixtureEntry {
                    name: "ÄÖÜ.ks",
                    segments: vec![FixtureSegment::raw(b"umlaut")],
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
                    segments: vec![FixtureSegment::raw(b"patch-only")],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "dup.bin",
                    segments: vec![FixtureSegment::raw(b"dup-patch")],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "MixedCase.TXT",
                    segments: vec![FixtureSegment::raw(b"mixed")],
                    hash: 0,
                    time: None,
                },
            ],
            BuildOptions::default(),
        ),
    )
    .expect("write patch archive");
    fs::write(
        &extra_path,
        build_archive(
            &[
                FixtureEntry {
                    name: "only-extra.ks",
                    segments: vec![FixtureSegment::raw(b"extra")],
                    hash: 0,
                    time: None,
                },
                // The newest mount holds the upper-case spelling of a name the
                // older mounts also carry: its case-insensitive hit must beat
                // their exact hits, because the walk resolves one mount
                // completely before it moves to the next.
                FixtureEntry {
                    name: "DUP.BIN",
                    segments: vec![FixtureSegment::raw(b"dup-extra")],
                    hash: 0,
                    time: None,
                },
            ],
            BuildOptions::default(),
        ),
    )
    .expect("write extra archive");

    // Opened below the fixture root so a qualifier's own spelling is what picks
    // a mount (`sys/extra.xp3` below this root names no mount).
    let provider =
        Xp3ResourceProvider::open_archives_below(&root, [&data_path, &patch_path, &extra_path])
            .expect("open provider");
    let reference = [&data_path, &patch_path, &extra_path].map(|path| {
        Xp3Archive::open_file(path)
            .unwrap_or_else(|error| panic!("open reference {path:?}: {error}"))
    });
    let mounts = [&data_path, &patch_path, &extra_path];

    // Name shapes the two resolvers can receive: stored spellings, ASCII case
    // variants (including one the ASCII-only fold cannot reach), an
    // intra-mount exact-vs-lowercase collision, both separator spellings,
    // extensionless and extended bare names, trailing delimiters, and the
    // empty and escaping spellings normalization rejects.
    const PROBES: &[&str] = &[
        "scenario/start.ks",
        "scenario/base_only.ks",
        "scenario/patch_only.ks",
        "only-extra.ks",
        "CASE.txt",
        "MixedCase.TXT",
        "SCENARIO/START.KS",
        "Scenario/Start.Ks",
        "case.txt",
        "CASE.TXT",
        "mixedcase.txt",
        "MIXEDCASE.TXT",
        "ÄÖÜ.ks",
        "äöü.ks",
        "ÄÖÜ.KS",
        "thing.txt",
        "THING.txt",
        "Thing.txt",
        "dup.bin",
        "DUP.BIN",
        "Dup.Bin",
        r"scenario\start.ks",
        r"SCENARIO\START.KS",
        r"scenario\.\start.ks",
        r".\scenario\start.ks",
        "scenario//start.ks",
        "start",
        "start.ks",
        "START",
        "case",
        "dup",
        "scenario/",
        ".",
        "./",
        "",
        "scenario/../start.ks",
        "/absolute.ks",
        "../outside.ks",
        r"..\outside.ks",
        r"\scenario\start.ks",
    ];

    for probe in PROBES {
        assert_eq!(
            provider.get_entry(probe).cloned(),
            per_archive_probe_entry(&reference, probe).cloned(),
            "get_entry({probe:?})"
        );
        let opened = provider.open(probe).ok().map(|mut stream| {
            let mut bytes = Vec::new();
            stream
                .read_to_end(&mut bytes)
                .expect("read provider stream");
            bytes
        });
        assert_eq!(
            opened,
            per_archive_probe_open(&reference, probe).map(|(_, bytes)| bytes),
            "open({probe:?})"
        );
    }

    for archive_name in [
        "data.xp3",
        "patch.xp3",
        "PATCH.XP3",
        "extra.xp3",
        r".\extra.xp3",
        r"sys\extra.xp3",
        "sys/extra.xp3",
        "missing.xp3",
    ] {
        for probe in PROBES {
            let reference_entry = reference_archive_index(&root, &mounts, archive_name)
                .and_then(|index| per_archive_probe_entry_in(&reference, index, probe).cloned());
            assert_eq!(
                provider.get_entry_in(archive_name, probe).cloned(),
                reference_entry,
                "get_entry_in({archive_name:?}, {probe:?})"
            );
        }
    }

    // A qualifier that carries a directory names the mount at that path, so a
    // directory this fixture does not mount resolves nothing — `sys/extra.xp3`
    // is not the `extra.xp3` the provider holds (`TVPRebuildAutoPathTable`
    // reads the archive at the spelling before `>`, `StorageIntf.cpp:1060`,
    // and `_TVPCreateStream` opens exactly that file, `:1249-1260`).
    assert!(
        provider
            .get_entry_in(r"sys\extra.xp3", "only-extra.ks")
            .is_none()
    );
    assert!(
        provider
            .get_entry_in("sys/extra.xp3", "only-extra.ks")
            .is_none()
    );
    assert!(
        provider
            .get_entry_in("/elsewhere/extra.xp3", "only-extra.ks")
            .is_none()
    );
    assert!(provider.open_in("sys/extra.xp3", "only-extra.ks").is_err());
    assert_eq!(
        provider
            .get_entry_in("extra.xp3", "only-extra.ks")
            .map(|entry| entry.name.as_str()),
        Some("only-extra.ks")
    );

    // Concrete answers the differential helpers share too little code with to
    // prove on their own. The newest mount is searched first; inside one mount
    // the exact map is consulted before the lowercased one.
    assert_eq!(
        read_provider_entry(&provider, r"SCENARIO\START.KS"),
        b"patch"
    );
    assert_eq!(
        read_provider_entry(&provider, "scenario/base_only.ks"),
        b"base-only"
    );
    assert_eq!(read_provider_entry(&provider, "only-extra.ks"), b"extra");
    assert_eq!(read_provider_entry(&provider, "dup.bin"), b"dup-extra");
    assert_eq!(read_provider_entry(&provider, "MixedCase.TXT"), b"mixed");
    assert_eq!(read_provider_entry(&provider, "mixedcase.txt"), b"mixed");
    assert_eq!(read_provider_entry(&provider, "thing.txt"), b"lower-thing");
    assert_eq!(read_provider_entry(&provider, "Thing.txt"), b"upper-thing");
    assert_eq!(read_provider_entry(&provider, "ÄÖÜ.KS"), b"umlaut");
    assert!(provider.open("äöü.ks").is_err());
    assert!(provider.get_entry("start").is_none());
    assert!(provider.get_entry("../outside.ks").is_none());

    let mut pinned = provider
        .open_in("patch.xp3", r"SCENARIO\START.KS")
        .expect("open pinned member");
    let mut pinned_bytes = Vec::new();
    pinned
        .read_to_end(&mut pinned_bytes)
        .expect("read pinned member");
    assert_eq!(pinned_bytes, b"patch");

    fs::remove_dir_all(root).expect("remove temp dir");
}

/// Reads the bytes one named mount serves for a member, `None` when either the
/// mount or the member is not there.
fn read_pinned_entry(
    provider: &Xp3ResourceProvider,
    archive: &str,
    member: &str,
) -> Option<Vec<u8>> {
    let mut stream = provider.open_in(archive, member).ok()?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// Two mounted archives that share a file name in different directories. The
/// reference resolves an `archive.xp3>` qualifier as the storage path it
/// spells — `TVPRebuildAutoPathTable` reads the archive at the part before `>`
/// (`StorageIntf.cpp:1055-1066`) and `_TVPCreateStream` opens exactly that file
/// (`:1249-1260`) — so `sys/x.xp3` is the archive below `sys/`, never the root
/// file that happens to share its name.
#[test]
fn archive_qualifier_names_the_mount_its_path_spells() {
    let root = temp_root("qualifier");
    fs::create_dir_all(root.join("sys")).expect("create sys dir");
    let sys_path = root.join("sys/x.xp3");
    let root_path = root.join("x.xp3");
    fs::write(
        &sys_path,
        build_archive(
            &[
                FixtureEntry {
                    name: "dup.bin",
                    segments: vec![FixtureSegment::raw(b"from-sys")],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "sys_only.bin",
                    segments: vec![FixtureSegment::raw(b"sys-only")],
                    hash: 0,
                    time: None,
                },
            ],
            BuildOptions::default(),
        ),
    )
    .expect("write sys archive");
    fs::write(
        &root_path,
        build_archive(
            &[
                FixtureEntry {
                    name: "dup.bin",
                    segments: vec![FixtureSegment::raw(b"from-root")],
                    hash: 0,
                    time: None,
                },
                FixtureEntry {
                    name: "root_only.bin",
                    segments: vec![FixtureSegment::raw(b"root-only")],
                    hash: 0,
                    time: None,
                },
            ],
            BuildOptions::default(),
        ),
    )
    .expect("write root archive");

    // The mount list is the project's: `sys/*.xp3` first, then the root's, so
    // a name-only walk would end at the root archive.
    let provider = Xp3ResourceProvider::open_archives_below(&root, [&sys_path, &root_path])
        .expect("open provider");
    let sys_bytes = Some(b"from-sys".to_vec());
    let root_bytes = Some(b"from-root".to_vec());

    // The declared directory decides, in every spelling of the same path.
    assert_eq!(
        read_pinned_entry(&provider, "sys/x.xp3", "dup.bin"),
        sys_bytes
    );
    assert_eq!(
        read_pinned_entry(&provider, r"sys\x.xp3", "dup.bin"),
        sys_bytes
    );
    assert_eq!(
        read_pinned_entry(&provider, "SYS/X.XP3", "dup.bin"),
        sys_bytes
    );
    assert_eq!(
        read_pinned_entry(&provider, &sys_path.to_string_lossy(), "dup.bin"),
        sys_bytes
    );
    assert_eq!(
        read_pinned_entry(&provider, "sys/x.xp3", "sys_only.bin"),
        Some(b"sys-only".to_vec())
    );

    // A bare name is the file in the root directory, which is the later mount.
    assert_eq!(read_pinned_entry(&provider, "x.xp3", "dup.bin"), root_bytes);
    assert_eq!(
        read_pinned_entry(&provider, "x.xp3", "root_only.bin"),
        Some(b"root-only".to_vec())
    );
    assert_eq!(read_pinned_entry(&provider, "x.xp3", "sys_only.bin"), None);

    // A directory no mount lives in resolves nothing: the archive named does
    // not exist, and a same-named file elsewhere is a different archive.
    assert_eq!(read_pinned_entry(&provider, "other/x.xp3", "dup.bin"), None);
    assert_eq!(
        read_pinned_entry(&provider, "/elsewhere/x.xp3", "dup.bin"),
        None
    );

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

/// The reference reads `TVPXP3ArchiveExtractionFilter` inside its read loop
/// (`XP3Archive.cpp:1047`), not once per stream, so a filter installed after
/// the archive was opened — and after the entry was already read once — still
/// changes what the next read returns.
#[test]
fn extraction_filter_installed_after_open_applies_to_later_reads() {
    let registry = Arc::new(Xp3FilterRegistry::new());
    let archive = Xp3Archive::open_with_options(
        Cursor::new(build_archive(
            &[FixtureEntry {
                name: "secret.txt",
                segments: vec![FixtureSegment::raw(b"plaintext")],
                hash: 0x99,
                time: None,
            }],
            BuildOptions::default(),
        )),
        Xp3OpenOptions::default().with_filter_registry(Arc::clone(&registry)),
    )
    .expect("open fixture");

    let read_entry = |archive: &Xp3Archive<Cursor<Vec<u8>>>| {
        let mut stream = archive
            .open_by_name("secret.txt")
            .expect("open entry")
            .expect("entry exists");
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).expect("read entry");
        bytes
    };

    // Read once with no filter installed: the bytes are the stored ones.
    assert_eq!(read_entry(&archive), b"plaintext");

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let seen = Arc::clone(&seen);
        registry.set_extraction_filter(Some(Arc::new(
            move |info: Xp3ExtractionFilterInfo<'_>, _ctx: &mut Xp3FilterContext| {
                seen.lock().expect("filter log").push((
                    info.offset,
                    info.file_hash,
                    info.file_name.to_string(),
                ));
                for byte in info.buffer.iter_mut() {
                    *byte ^= 0xff;
                }
            },
        )));
    }

    let expected: Vec<u8> = b"plaintext".iter().map(|byte| byte ^ 0xff).collect();
    assert_eq!(
        read_entry(&archive),
        expected,
        "a filter installed after the archive opened must reach the read path"
    );
    // The extraction info carries the chunk's uncompressed offset, the entry's
    // index hash and its name (`XP3Archive.cpp:1047-1048`).
    assert_eq!(
        *seen.lock().expect("filter log"),
        vec![(0, 0x99, "secret.txt".to_string())]
    );

    // `TVPSetXP3FilterScript("")` clears the slots (`xp3filter.cpp:429-430`).
    registry.set_extraction_filter(None);
    assert_eq!(read_entry(&archive), b"plaintext");
}

/// `XP3_CONTENT_FILTER_FETCH_FULLDATA` (`XP3Archive.cpp:585-595`): the content
/// filter runs when the stream is created, its `ctx` reaches every extraction
/// call of that stream, and the entry is read whole through the extraction
/// filter into the bytes the caller then reads.
#[test]
fn content_filter_fetches_the_full_entry_through_the_extraction_filter() {
    let registry = Arc::new(Xp3FilterRegistry::new());
    let archive = Xp3Archive::open_with_options(
        Cursor::new(build_archive(
            &[FixtureEntry {
                name: "whole.bin",
                segments: vec![FixtureSegment::raw(b"abcdefgh")],
                hash: 7,
                time: None,
            }],
            BuildOptions::default(),
        )),
        Xp3OpenOptions::default()
            .with_filter_registry(Arc::clone(&registry))
            .with_archive_name("data.xp3"),
    )
    .expect("open fixture");

    let content_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let content_calls = Arc::clone(&content_calls);
        registry.set_content_filter(Some(Arc::new(
            move |file: &str, archive: &str, size: u64, ctx: &mut Xp3FilterContext| {
                content_calls.lock().expect("content log").push((
                    file.to_string(),
                    archive.to_string(),
                    size,
                ));
                ctx.set(2u32);
                Xp3ContentFilterAction::FetchFull
            },
        )));
    }
    registry.set_extraction_filter(Some(Arc::new(
        |info: Xp3ExtractionFilterInfo<'_>, ctx: &mut Xp3FilterContext| {
            let shift = *ctx
                .get::<u32>()
                .expect("the content filter seeded the context");
            for byte in info.buffer.iter_mut() {
                *byte += u8::try_from(shift).expect("shift fits");
            }
        },
    )));

    let mut stream = archive
        .open_by_name("whole.bin")
        .expect("open entry")
        .expect("entry exists");
    let mut first = [0u8; 3];
    stream.read_exact(&mut first).expect("first chunk");
    assert_eq!(&first, b"cde");
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).expect("rest of the entry");
    assert_eq!(rest, b"fghij");

    assert_eq!(
        *content_calls.lock().expect("content log"),
        vec![("whole.bin".to_string(), "data.xp3".to_string(), 8)]
    );
}

/// A content filter that answers `0` (`Decode`) leaves the archive read path
/// alone, and the stream still reads through the *current* extraction filter.
#[test]
fn content_filter_decode_keeps_the_normal_read_path() {
    let registry = Arc::new(Xp3FilterRegistry::new());
    let archive = Xp3Archive::open_with_options(
        Cursor::new(build_archive(
            &[FixtureEntry {
                name: "plain.bin",
                segments: vec![FixtureSegment::zlib(b"decode me")],
                hash: 3,
                time: None,
            }],
            BuildOptions::default(),
        )),
        Xp3OpenOptions::default().with_filter_registry(Arc::clone(&registry)),
    )
    .expect("open fixture");

    registry.set_content_filter(Some(Arc::new(
        |_file: &str, _archive: &str, _size: u64, _ctx: &mut Xp3FilterContext| {
            Xp3ContentFilterAction::Decode
        },
    )));

    let mut stream = archive
        .open_by_name("plain.bin")
        .expect("open entry")
        .expect("entry exists");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read entry");
    assert_eq!(bytes, b"decode me");
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
