//! The XP3 filter facility (`TVPSetXP3ArchiveExtractionFilter`,
//! `TVPSetXP3ArchiveContentFilter`).
//!
//! Part of [`crate::plugin_api`]: a plugin crate's production dependencies are
//! `krkr-engine` and `krkr-tjs2` only, so the filter vocabulary — the two
//! filter traits, the extraction info struct, the per-stream context and the
//! registry that holds them — is re-exported here from `krkr-core` (where it
//! sits next to `StorageMediaProvider` for the same reason), and this module is
//! the plugin-facing way to install into the running project's registry.
//!
//! `xp3filter.dll` is the reference consumer. On post-registration it reads
//! `xp3filter.tjs` and sets both core callbacks
//! (`Kirikiroid2/src/plugins/xp3filter.cpp:438-458`), and that works even
//! though the game's archives already exist because the engine reads the
//! callbacks *late*: the content filter when an entry stream is created
//! (`XP3Archive.cpp:585-595`) and the extraction filter on every read chunk
//! (`:1047-1053`). The same holds here — the registry a plugin installs into
//! is the one the mounted archives consult, so an install from a plugin's
//! `register` (which always runs after the project storage was built) reaches
//! every later read, and even a stream that was already read once picks the
//! filter up on its next read.
//!
//! ```ignore
//! use krkr_engine::plugin_api::xp3;
//!
//! // In `KrkrPlugin::register`, i.e. after the project's archives exist.
//! xp3::set_extraction_filter(runtime, Some(Arc::new(
//!     |info: xp3::Xp3ExtractionFilterInfo<'_>, _ctx: &mut xp3::Xp3FilterContext| {
//!         for byte in info.buffer.iter_mut() {
//!             *byte ^= 0x5a;
//!         }
//!     },
//! )))?;
//! ```
//!
//! **Errors and lifecycle.** A host whose storage has no archives (a browser
//! or script-only host) answers `None` from [`filter_registry`] and both
//! setters fail with `io::ErrorKind::NotFound`, so a plugin can log "nothing
//! to filter here" instead of installing into a void. `None` passed as the
//! filter clears the slot, the way `TVPSetXP3FilterScript("")` clears both
//! (`xp3filter.cpp:429-430`). The engine keeps no handle on an installed
//! filter beyond the registry, so dropping the last `Arc` a plugin holds is
//! not how a filter is uninstalled — clear the slot.
//!
//! **Thread contract.** An extraction filter runs on the read path, which may
//! be a resource worker rather than the script thread, so
//! [`Xp3ExtractionFilter: Send + Sync`](Xp3ExtractionFilter) is a hard
//! requirement; [`Xp3FilterContext`] is `Send` for the same reason and is the
//! place a content filter hands state to the extraction calls of the same
//! stream. A bridge that wants to call back into TJS must therefore do it the
//! way K2 does — a private per-thread script engine behind a `Send` wrapper
//! (`xp3filter.cpp:212-220`, `:295-354`) — and never hand a `Runtime` handle
//! across that boundary.

use std::{io, sync::Arc};

use krkr_tjs2::runtime::Runtime;

use crate::KrkrHost;

pub use krkr_core::{
    Xp3ContentFilter, Xp3ContentFilterAction, Xp3ExtractionFilter, Xp3ExtractionFilterInfo,
    Xp3FilterContext, Xp3FilterRegistry,
};

/// The filter registry the running project's archives consult, or `None` when
/// its storage has no archives (a host that only serves memory or deferred
/// resources). The handle is the same object the archives hold, so anything a
/// plugin installs through it is live for the next stream creation and the
/// next read.
pub fn filter_registry(runtime: &Runtime<KrkrHost>) -> Option<Arc<Xp3FilterRegistry>> {
    runtime.host().project_storage().ok()?.xp3_filter_registry()
}

/// `TVPSetXP3ArchiveExtractionFilter` (`XP3Archive.cpp:31-34`): install the
/// callback that sees every decompressed chunk of every archive read, or clear
/// it with `None`.
pub fn set_extraction_filter(
    runtime: &Runtime<KrkrHost>,
    filter: Option<Arc<dyn Xp3ExtractionFilter>>,
) -> io::Result<()> {
    filter_registry(runtime)
        .ok_or_else(no_archives)?
        .set_extraction_filter(filter);
    Ok(())
}

/// `TVPSetXP3ArchiveContentFilter` (`XP3Archive.cpp:38-41`): install the
/// callback that decides, per entry, whether the whole file is fetched into
/// memory, or clear it with `None`.
pub fn set_content_filter(
    runtime: &Runtime<KrkrHost>,
    filter: Option<Arc<dyn Xp3ContentFilter>>,
) -> io::Result<()> {
    filter_registry(runtime)
        .ok_or_else(no_archives)?
        .set_content_filter(filter);
    Ok(())
}

fn no_archives() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "the project storage has no XP3 archives to filter",
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use krkr_assets::ProjectStorage;

    use crate::{EngineConfig, KrkrEngine, KrkrPlugin, SystemPaths};

    use super::*;

    /// A plugin whose whole `V2Link` is the reference's install step: the
    /// extraction filter below is what `xp3filter.dll` sets on
    /// post-registration (`xp3filter.cpp:438-458`).
    struct FilterPlugin;

    impl KrkrPlugin for FilterPlugin {
        fn name(&self) -> &str {
            "xp3filter.dll"
        }

        fn register(&self, runtime: &mut Runtime<KrkrHost>) -> krkr_tjs2::Result<()> {
            set_extraction_filter(
                runtime,
                Some(Arc::new(
                    |info: Xp3ExtractionFilterInfo<'_>, _ctx: &mut Xp3FilterContext| {
                        for byte in info.buffer.iter_mut() {
                            *byte ^= 0xff;
                        }
                    },
                )),
            )
            .expect("the project storage has archives");
            Ok(())
        }
    }

    fn temp_root(prefix: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-xp3-filter-{prefix}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    /// Minimal XP3 container writer for this module's fixture: magic, one raw
    /// segment per entry, a raw index block — the same layout the fixtures in
    /// `krkr-assets/src/storage.rs` and `krkr-xp3/src/tests.rs` write, and the
    /// one `krkr-xp3/src/parse.rs` reads.
    fn build_xp3_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        fn push_chunk(output: &mut Vec<u8>, name: &[u8; 4], body: &[u8]) {
            output.extend_from_slice(name);
            output.extend_from_slice(&u64::try_from(body.len()).expect("chunk fits").to_le_bytes());
            output.extend_from_slice(body);
        }

        let mut archive = Vec::new();
        // `krkr_xp3::XP3_MAGIC` (`XP3\r\n \n\x1a\x8b\x67\x01`); the engine
        // crate does not depend on `krkr-xp3` directly, so the fixture spells
        // it out rather than reaching through the assets crate.
        archive.extend_from_slice(&[
            0x58, 0x50, 0x33, 0x0d, 0x0a, 0x20, 0x0a, 0x1a, 0x8b, 0x67, 0x01,
        ]);
        let index_pointer_offset = archive.len();
        archive.extend_from_slice(&0u64.to_le_bytes());

        let mut built = Vec::new();
        for (name, data) in entries {
            let offset = archive.len() as u64;
            archive.extend_from_slice(data);
            built.push((*name, offset, data.len() as u64));
        }
        let index_offset = archive.len() as u64;

        let mut index = Vec::new();
        for (name, offset, size) in &built {
            let mut file = Vec::new();
            let mut info = Vec::new();
            info.extend_from_slice(&0u32.to_le_bytes());
            info.extend_from_slice(&size.to_le_bytes());
            info.extend_from_slice(&size.to_le_bytes());
            let units = name.encode_utf16().collect::<Vec<_>>();
            info.extend_from_slice(
                &u16::try_from(units.len())
                    .expect("entry name fits")
                    .to_le_bytes(),
            );
            for unit in units {
                info.extend_from_slice(&unit.to_le_bytes());
            }
            push_chunk(&mut file, b"info", &info);

            let mut segm = Vec::new();
            segm.extend_from_slice(&0u32.to_le_bytes());
            segm.extend_from_slice(&offset.to_le_bytes());
            segm.extend_from_slice(&size.to_le_bytes());
            segm.extend_from_slice(&size.to_le_bytes());
            push_chunk(&mut file, b"segm", &segm);

            push_chunk(&mut file, b"adlr", &0u32.to_le_bytes());
            push_chunk(&mut index, b"File", &file);
        }

        archive.push(0);
        archive.extend_from_slice(&(index.len() as u64).to_le_bytes());
        archive.extend_from_slice(&index);
        archive[index_pointer_offset..index_pointer_offset + 8]
            .copy_from_slice(&index_offset.to_le_bytes());
        archive
    }

    fn engine_with_archive(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("project storage");
        KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine")
    }

    fn read_entry(engine: &KrkrEngine, name: &str) -> Vec<u8> {
        engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage")
            .read_binary_storage(name)
            .unwrap_or_else(|error| panic!("read `{name}`: {error}"))
            .as_bytes()
            .expect("fixture bytes")
            .into_owned()
    }

    /// The seam end to end, from a plugin: `register` installs an extraction
    /// filter (after the storage and its archives already exist — the timing
    /// the audit flagged), and the bytes the engine reads change. The first
    /// read happens *before* the plugin registers and populates the storage's
    /// raw-byte cache, so this also pins that a filter install is not masked by
    /// it.
    #[test]
    fn a_plugin_installed_filter_changes_what_the_engine_reads() {
        let root = temp_root("install");
        fs::write(
            root.join("data.xp3"),
            build_xp3_archive(&[("secret/data.bin", b"plaintext")]),
        )
        .expect("write data.xp3");

        let mut engine = engine_with_archive(&root);
        assert_eq!(
            read_entry(&engine, "secret/data.bin"),
            b"plaintext".as_slice()
        );

        engine
            .register_plugin(FilterPlugin)
            .expect("register the filter plugin");

        let expected: Vec<u8> = b"plaintext".iter().map(|byte| byte ^ 0xff).collect();
        assert_eq!(
            read_entry(&engine, "secret/data.bin"),
            expected,
            "a filter installed after the archives opened must reach the read"
        );

        // `None` clears the slot, the way `TVPSetXP3FilterScript("")` does.
        let runtime = engine.tjs_runtime();
        set_extraction_filter(runtime, None).expect("clear the filter");
        assert_eq!(
            read_entry(&engine, "secret/data.bin"),
            b"plaintext".as_slice()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// A content filter is reachable the same way, and its per-stream context
    /// reaches the extraction calls of that stream.
    #[test]
    fn a_plugin_installed_content_filter_fetches_the_whole_entry() {
        let root = temp_root("content");
        fs::write(
            root.join("data.xp3"),
            build_xp3_archive(&[("whole.bin", b"abcdefgh")]),
        )
        .expect("write data.xp3");

        let engine = engine_with_archive(&root);
        let runtime = engine.tjs_runtime();

        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let asked = Arc::clone(&asked);
            set_content_filter(
                runtime,
                Some(Arc::new(
                    move |file: &str, archive: &str, size: u64, ctx: &mut Xp3FilterContext| {
                        asked.lock().expect("content log").push((
                            file.to_string(),
                            archive.to_string(),
                            size,
                        ));
                        ctx.set(2u32);
                        Xp3ContentFilterAction::FetchFull
                    },
                )),
            )
            .expect("install the content filter");
        }
        set_extraction_filter(
            runtime,
            Some(Arc::new(
                |info: Xp3ExtractionFilterInfo<'_>, ctx: &mut Xp3FilterContext| {
                    let shift = *ctx.get::<u32>().expect("the content filter seeded it");
                    for byte in info.buffer.iter_mut() {
                        *byte += u8::try_from(shift).expect("shift fits");
                    }
                },
            )),
        )
        .expect("install the extraction filter");

        assert_eq!(read_entry(&engine, "whole.bin"), b"cdefghij");
        assert_eq!(
            *asked.lock().expect("content log"),
            vec![("whole.bin".to_string(), "data.xp3".to_string(), 8)]
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// A host whose storage has no archives answers `None` and a named
    /// failure, so the plugin logs rather than installing into nothing.
    #[test]
    fn a_host_without_archives_refuses_the_install() {
        let storage = ProjectStorage::from_memory([("a.txt", b"a".to_vec())]);
        let engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        let runtime = engine.tjs_runtime();

        assert!(filter_registry(runtime).is_none());
        let error = set_extraction_filter(runtime, None).expect_err("no archives");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let error = set_content_filter(runtime, None).expect_err("no archives");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    /// The registry handed out is the one the archives hold, so a filter can
    /// be installed through it and cleared again — the property that makes the
    /// post-registration install work.
    #[test]
    fn the_registry_is_shared_with_the_mounted_archives() {
        let root = temp_root("shared");
        fs::write(
            root.join("data.xp3"),
            build_xp3_archive(&[("a.bin", b"bytes")]),
        )
        .expect("write data.xp3");

        let engine = engine_with_archive(&root);
        let runtime = engine.tjs_runtime();
        let registry = filter_registry(runtime).expect("archives are mounted");
        assert!(registry.extraction_filter().is_none());
        set_extraction_filter(
            runtime,
            Some(Arc::new(
                |info: Xp3ExtractionFilterInfo<'_>, _ctx: &mut Xp3FilterContext| {
                    for byte in info.buffer.iter_mut() {
                        *byte ^= 0xff;
                    }
                },
            )),
        )
        .expect("install");
        assert!(registry.extraction_filter().is_some());
        let expected: Vec<u8> = b"bytes".iter().map(|byte| byte ^ 0xff).collect();
        assert_eq!(read_entry(&engine, "a.bin"), expected);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
