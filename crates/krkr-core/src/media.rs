//! Storage media providers (`iTVPStorageMedia`).
//!
//! KRKR plugins extend the storage namespace by registering a *media* — a
//! handler for one URI scheme — on the project storage
//! ([`ProjectStoragePort::register_storage_media`](crate::ProjectStoragePort::register_storage_media)).
//! `psbfile.dll` registers `psb`, `lzfs.dll` registers `lzfs`,
//! `yuzuex.dll`/`packinone.dll` register `proxy`, `krkrsteam.dll` registers
//! `steam`, `minizip` registers `zip` and `varfile` registers `var`. A game
//! then reads `psb://container.psb/entry`, `lzfs://./data/file.bin`,
//! `steam://./save.dat` and so on through the ordinary
//! `TVPCreateStream`/`Storages.*` entry points.
//!
//! The reference lines behind the name parsing live with the engine divergences
//! in `krkr-assets`' `media` module, which re-exports everything here.

use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::Weak,
};

use crate::{ProjectStoragePort, ResourceData, ResourceStream};

/// The media name the reference has already registered for the built-in
/// filesystem resolver (`StorageIntf.cpp:200-205`).
pub const FILE_MEDIA_NAME: &str = "file";

/// Whether `name` can be registered as a media name. The reference recognizes
/// a media name as the run of ASCII letters before `:`
/// (`StorageIntf.cpp:299-318`), so anything else can never be addressed by a
/// storage name.
pub fn is_valid_media_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphabetic())
}

/// Splits `media://domain/path` into the media name and the name space the
/// media owns (`domain/path`). Returns `None` for names that are not
/// media-qualified, so an ordinary relative path always stays on the built-in
/// resolver.
///
/// A bare `media://` is *not* a media name here: the reference rejects it as
/// `TVPInvalidPathName` (`StorageIntf.cpp:341-343`), and inventing a new error
/// for a name the pre-registry engine accepted as an ordinary path would be a
/// regression.
pub fn split_media_name(name: &str) -> Option<(&str, &str)> {
    let (media_name, media_path) = name.split_once("://")?;
    if !is_valid_media_name(media_name) || media_path.is_empty() {
        return None;
    }
    Some((media_name, media_path))
}

/// A registered storage media, mirroring the operations the engine uses from
/// `iTVPStorageMedia` (`StorageIntf.h:107-143`).
///
/// Implementations are shared between threads: the engine resolves storage
/// from the script thread, from resource workers and from media threads, so
/// `Send + Sync` is required and interior mutability must be the provider's
/// own concern (the reference uses critical sections the same way).
///
/// `name` is always the media's own name space — everything after
/// `media://`, with `\` already unified to `/`. A provider that mounts a
/// container the way `psb`/`zip`/`var` do splits its container or domain at the
/// first `/`; `lzfs`/`steam` treat the whole string as one path.
pub trait StorageMediaProvider: Send + Sync {
    /// The media name, e.g. `psb`, `lzfs`, `proxy`, `steam`
    /// (`iTVPStorageMedia::GetName`, `StorageIntf.h:113`).
    fn media_name(&self) -> &str;

    /// `iTVPStorageMedia::CheckExistentStorage` (`StorageIntf.h:128`): whether
    /// this media serves `name`. A `false` return is a *miss* — the resolver
    /// then tries the built-in stack — so a provider must not report an error
    /// this way.
    fn exists(&self, name: &str) -> bool;

    /// Opens `name` as a stream (`iTVPStorageMedia::Open`,
    /// `StorageIntf.h:131`). The error is the media's own failure and is
    /// surfaced verbatim, the way the reference propagates
    /// `TVPThrowExceptionMessage` out of `Open` (for example
    /// `cannot open steamfile:%1`, krkr2 `plugins/win32/steam/Storages.cpp:394`).
    fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>>;

    /// Reads `name` whole. The default streams it through [`Self::open`] the
    /// way `TVPCreateStream` plus a read-to-end does.
    fn read(&self, name: &str) -> io::Result<ResourceData> {
        let mut stream = self.open(name)?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        Ok(ResourceData::from_vec(bytes))
    }

    /// Length of `name` when the media can answer it cheaply. The default
    /// opens the entry and seeks to its end.
    fn byte_len(&self, name: &str) -> io::Result<Option<u64>> {
        let mut stream = self.open(name)?;
        let current = stream.stream_position().ok();
        let len = stream.seek(SeekFrom::End(0)).ok();
        if let Some(position) = current {
            let _ = stream.seek(SeekFrom::Start(position));
        }
        Ok(len)
    }

    /// `iTVPStorageMedia::GetListAt` (`StorageIntf.h:136`), in the spelling
    /// `Storages.dirlist`/`getDirList` promise: immediate children only, with a
    /// trailing `/` on directories (`fstat/Main.cpp:469-472`,
    /// `dirlist/Main.cpp:56-64`).
    ///
    /// `ErrorKind::NotFound` or `ErrorKind::Unsupported` mean "this media does
    /// not serve a directory here" — the resolver then falls back to the
    /// built-in stack, exactly like a failed existence probe. Any other error
    /// is a provider failure and is surfaced.
    fn list(&self, name: &str) -> io::Result<Vec<String>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "storage media `{}` does not list `{name}`",
                self.media_name()
            ),
        ))
    }

    /// Writes `name` whole (`_TVPCreateStream`'s write branch,
    /// `StorageIntf.cpp:1236-1244`, which reaches the media's
    /// `Open(name, TJS_BS_WRITE|…)` at `:1279-1289`). `mode` is the engine's
    /// TJS write-mode string; an empty mode is a plain create/replace. The
    /// write bypasses the existence search exactly like the reference, so a
    /// provider that cannot update in place is still asked to.
    ///
    /// Read-only media keep the default, which fails with the error a
    /// read-only `Open` would throw (`minizip/storage.cpp:439-457`).
    fn write(&self, name: &str, mode: &str, bytes: &[u8]) -> io::Result<()> {
        let _ = (name, mode, bytes);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("storage media `{}` is read-only", self.media_name()),
        ))
    }

    /// Gives a wrapping media (`lzfs`, `proxy`) a handle to the engine's
    /// built-in stack, so it can resolve the name it is handed through the
    /// ordinary search path. Called once by the engine before the media is
    /// inserted.
    ///
    /// The handle is [`Weak`] because the registry lives inside the storage: a
    /// strong handle would be a reference cycle. Rules a wrapping media keeps:
    ///
    /// * Use the handle only inside [`Self::exists`]/[`Self::open`]/
    ///   [`Self::write`]; a failed `upgrade()` (the storage is gone) is a miss
    ///   or a provider error, never a panic.
    /// * Never map a name back into your own scheme. Media dispatch is
    ///   scheme-driven, so re-entering yourself loops until the stack
    ///   overflows; map to a scheme-less name (`./data/x`) or another scheme.
    /// * The storage is thread-safe (`ProjectStorage` guards its inner state),
    ///   because a provider may be called from a resource worker.
    fn attach_storage(&self, storage: Weak<dyn ProjectStoragePort>) {
        let _ = storage;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::StoragePort;

    /// A storage that implements only the required port methods, the way a
    /// browser or test host does: every media operation must answer with its
    /// default instead of panicking (design A.5.6).
    struct BarePort;

    impl StoragePort for BarePort {
        fn open(&self, _path: &str) -> io::Result<Box<dyn ResourceStream>> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no storage"))
        }

        fn exists(&self, _path: &str) -> bool {
            false
        }
    }

    impl ProjectStoragePort for BarePort {
        fn is_directory(&self, _name: &str) -> bool {
            false
        }

        fn list_directory(&self, _name: &str) -> io::Result<Vec<String>> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no storage"))
        }

        fn placed_path(&self, _name: &str) -> Option<String> {
            None
        }

        fn read_binary_storage(&self, name: &str) -> io::Result<ResourceData> {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("storage `{name}` not found"),
            ))
        }

        fn read_text_storage(&self, name: &str, _configured_encoding: &str) -> io::Result<String> {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("storage `{name}` not found"),
            ))
        }

        fn write_text_storage(&self, _name: &str, _mode: &str, _text: &str) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no storage"))
        }

        fn write_binary_storage(&self, _name: &str, _mode: &str, _bytes: &[u8]) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no storage"))
        }

        fn add_auto_path(&self, _path: &str) {}

        fn remove_auto_path(&self, _path: &str) -> bool {
            false
        }

        fn clear_archive_cache(&self) -> io::Result<()> {
            Ok(())
        }

        fn catalog_contains(&self, _name: &str) -> bool {
            false
        }

        fn catalog_contains_for_load(&self, _name: &str) -> bool {
            false
        }

        fn set_catalog_paths(&self, _paths: &[String]) {}

        fn insert_memory(&self, _path: &str, _bytes: Vec<u8>) {}

        fn insert_external_memory(&self, _path: &str, _bytes: Vec<u8>) {}

        fn drain_memory_writes(&self) -> Vec<(String, Vec<u8>)> {
            Vec::new()
        }
    }

    struct ReadOnlyMedia {
        attached: Mutex<Option<Weak<dyn ProjectStoragePort>>>,
    }

    impl StorageMediaProvider for ReadOnlyMedia {
        fn media_name(&self) -> &str {
            "psb"
        }

        fn exists(&self, _name: &str) -> bool {
            false
        }

        fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no entry `{name}`"),
            ))
        }

        fn attach_storage(&self, storage: Weak<dyn ProjectStoragePort>) {
            *self.attached.lock().expect("media lock") = Some(storage);
        }
    }

    #[test]
    fn the_port_media_methods_default_to_unsupported() {
        let port = BarePort;
        let media: Arc<dyn StorageMediaProvider> = Arc::new(ReadOnlyMedia {
            attached: Mutex::new(None),
        });
        let error = port
            .register_storage_media(Arc::clone(&media))
            .expect_err("a port without a media registry refuses registration");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(!port.unregister_storage_media("psb"));
        assert!(port.storage_media_names().is_empty());
    }

    #[test]
    fn a_media_keeps_the_weak_storage_handle_it_was_given() {
        let media = ReadOnlyMedia {
            attached: Mutex::new(None),
        };
        assert!(media.attached.lock().expect("media lock").is_none());
        {
            let storage: Arc<dyn ProjectStoragePort> = Arc::new(BarePort);
            media.attach_storage(Arc::downgrade(&storage));
            let handle = media.attached.lock().expect("media lock").clone();
            assert!(handle.expect("weak handle").upgrade().is_some());
        }
        // The registry lives inside the storage, so the handle must not keep it
        // alive: a strong handle here would be a reference cycle.
        let handle = media.attached.lock().expect("media lock").clone();
        assert!(handle.expect("weak handle").upgrade().is_none());
    }
}
