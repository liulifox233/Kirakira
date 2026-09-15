//! `minizip.dll` — the `zip` storage media and the `Zip` / `Unzip` archive
//! classes.
//!
//! Reference: krkr2's `src/plugins/win32/minizip/` (`main.cpp`, `storage.cpp`,
//! `ioapi.cpp`, `narrow.h`), built over the zlib contrib minizip under
//! `src/plugins/win32/zlib/contrib/minizip/`. `wtnbgo/minizip` (the upstream the
//! catalog entry points at) is the same plugin re-based on minizip-ng; its
//! surface deltas are listed at the bottom of this comment.
//!
//! # What the reference exposes
//!
//! * Class `Zip` (`main.cpp:526-531`): `new Zip()`, `open(filename,
//!   overwrite=0)`, `close()`, `add(srcfile, destfile, deflateLevel=void,
//!   password=void) → bool`.
//! * Class `Unzip` (`main.cpp:533-539`): `new Unzip()`, `open(filename)`,
//!   `close()`, `list() → Array`, `extract(srcname, destfile, password=void) →
//!   bool`.
//! * `Storages.mountZip(name, zipfile)` / `Storages.unmountZip(name)`
//!   (`storage.cpp:568-622`): mount a zip file as the `zip://<name>/…`
//!   namespace, read-only.
//! * A read-only storage media named `zip` (`storage.cpp:18, 433-435`),
//!   registered by `initZipStorage()` from the pre-registration callback
//!   (`main.cpp:547-551`, `storage.cpp:624-627`) and dropped on unregistration
//!   (`storage.cpp:629-631`).
//!
//! # Decisions the reference makes, and where this port lands
//!
//! * `Zip.open` overwrite modes (`main.cpp:186-204`): `0` throws
//!   `"<filename> exists."` when the name resolves, `1` creates/truncates, `2`
//!   appends when the file exists and falls back to create when it does not.
//!   Append is the minizip `APPEND_STATUS_ADDINZIP` mode; this port keeps the
//!   old archive's data and entries and writes a new central directory over
//!   them, so readers find the last EOCD exactly as they do with minizip.
//! * `Zip.add` stores each entry under its UTF-8 name with flag bit 11 set
//!   (`main.cpp:303-309` passes `NarrowString(destname, true)` and `FLAG_UTF8`,
//!   defined at `main.cpp:37-38`), deflates unless the level is `0`, and
//!   encrypts with the PKWARE ZipCrypto the password selects (`main.cpp:242-245,
//!   290-309` and the bundled `crypt.c`).
//! * `Unzip.open` decides the whole archive's name encoding from the **first**
//!   entry's UTF-8 flag (`main.cpp:379-384`); `list` decodes names through
//!   `storeFilename` (`main.cpp:334-350`) and reports `filename`,
//!   `uncompressed_size`, `compressed_size`, `crypted`, `deflated`,
//!   `deflateLevel` (`(flag & 0x6) / 2`) and `crc`, plus `date` when the DOS
//!   timestamp is non-zero (`main.cpp:425-448`).
//! * Entry lookup is the minizip `unzLocateFile(..., CASESENSITIVITY)` with
//!   `CASESENSITIVITY = 0` (`main.cpp:34, 491`), which on a non-unix host is
//!   the *case-insensitive* default (`zlib/contrib/minizip/unzip.c:96-100,
//!   383-413`); comparison folds ASCII only (`unzip.c:350-368`). Names are
//!   capped at `UNZ_MAXFILENAMEINZIP` (256) by the reference — this port does
//!   not impose that cap.
//! * The media is read-only: `Open` accepts only `TJS_BS_READ` and otherwise
//!   throws `"%1:cannot open zipfile"` (`storage.cpp:460-478`); a name without
//!   a `/` throws `"invalid path:%1"` (`storage.cpp:542-557`);
//!   `GetLocallyAccessibleName` is empty (`storage.cpp:492-494`); the lister
//!   table is built at mount time from entry names, one map from `"/" + dir`
//!   to the names directly inside it (`storage.cpp:158-169, 198-210`).
//! * `Unzip`/`Zip` read and write through the storage system, never the local
//!   filesystem: the minizip file functions are `TVPCreateIStream` wrappers
//!   (`ioapi.cpp:6-23`), so a zip may itself live in an XP3 archive or another
//!   media. This port resolves through [`ProjectStoragePort`] the same way and
//!   reads the whole container into an in-memory index (see below).
//!
//! # Deviations, all forced by the engine's seams
//!
//! * **Whole-buffer writes.** The engine's write seam is
//!   `ProjectStoragePort::write_binary_storage(name, mode, bytes)`
//!   (`krkr-core/src/lib.rs:242`); there is no writable stream, so this port
//!   keeps the archive image in memory and (re)writes the storage name after
//!   every successful mutation and on `close`. `Zip.open` creates the (empty)
//!   file immediately like `zipOpen2_64` does, and every `add` rewrites the
//!   file with the complete image. A game that never calls `close` leaves an
//!   unfinalized archive on disk, exactly as the reference does.
//! * **Entry data is materialized on open.** Reading through the media decodes
//!   the requested entry into memory and serves a cursor over it, where the
//!   reference streams from the archive. `Seek` on the resulting stream is
//!   therefore a memory cursor (`UnzipStream::Seek` in the reference also only
//!   supports rewinding, `storage.cpp:275-296`).
//! * **Name decoding for flag-less entries.** Flag bit 11 alone decides the
//!   encoding (`storeFilename`, `main.cpp:335-350`): set means UTF-8, clear
//!   means the host's ANSI code page, because the flag-less branch's
//!   `ttstr = const char*` assignment reaches `TJS_mbstowcs` —
//!   `MultiByteToWideChar(CP_ACP, …)` (`tjsConfig.cpp:208-243`), CP932 on the
//!   Japanese Windows hosts the reference ships to — and queries are converted
//!   back through the same code page (`NarrowString(srcname, utf8)`,
//!   `main.cpp:491`, `narrow.h:22-28`). This port decodes a flag-less name with
//!   the same WHATWG table the engine reads project text with
//!   (`encoding_rs::SHIFT_JIS`, `krkr-assets/src/storage.rs:2600`), and keeps
//!   its previous fallback — valid UTF-8 as-is, otherwise CP437 — only for
//!   bytes that code page cannot represent at all, where the reference throws
//!   instead (`TJSNarrowToWideConversionError`, `tjsVariantString.h:110-112`).
//!   The flag is honoured per entry, where the reference decides the whole
//!   archive's encoding from the *first* entry (`main.cpp:379-384`,
//!   `storage.cpp:60-61`) — a mixed archive decodes more of its names here,
//!   never fewer. Names are read from the central directory only and entry
//!   comments are never decoded, both as in the reference.
//! * **Passwords are UTF-8.** `NarrowString(password)` uses the ANSI code page
//!   (`narrow.h:22-28`); identical for ASCII passwords.
//! * **DOS timestamps are interpreted as UTC.** DOS dates are local time and
//!   the reference converts them through `FileTimeToLocalFileTime`
//!   (`main.cpp:434-447`); the engine exposes no local-time offset to plugins.
//! * **Deflate output is not byte-identical to zlib's.** Both directions are
//!   implemented in this file (RFC 1951): inflate handles stored, fixed and
//!   dynamic blocks; deflate emits fixed-Huffman LZ77 at the requested effort.
//!   The bytes differ; the format and the decoded contents do not.
//! * **`extract` reports a CRC mismatch as failure.** The reference writes
//!   whatever `unzReadCurrentFile` produced and ignores the CRC error
//!   `unzCloseCurrentFile` returns (`main.cpp:494-509`); this port fails the
//!   call instead of writing corrupt output.
//! * **Zip64 in, not out.** Sizes/offsets beyond 32 bits are read through the
//!   Zip64 extra field and EOCD64; the writer refuses an archive that would
//!   need them (multi-gigabyte images cannot exist in the in-memory design
//!   anyway). Multi-volume archives are refused like the reference
//!   (`readme.txt:10-13`).
//!
//! Upstream `wtnbgo/minizip` deltas, for the record: `Zip` gains
//! `CompressionMethodStore/Deflate/Bzip2/Lzma` constants and `add` two more
//! optional arguments (`compressionMethod`, `ignoreDate`), `Unzip.open` gains a
//! `force_utf8` second argument, and `Unzip.list`'s `date` comes from a
//! `time_t`. This port follows the krkr2 contract the catalog entry and the
//! dossier name as the reference; the upstream additions are not installed so a
//! krkr2 script cannot observe a superset surface.

use std::{
    collections::BTreeMap,
    io::{self, Cursor, Read, Seek, SeekFrom},
    sync::{Arc, Mutex, OnceLock},
};

use encoding_rs::SHIFT_JIS;
use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::{self, ResourceStream, StorageMediaProvider},
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "`Zip`/`Unzip` archive classes and the read-only `zip` storage media",
    notes: "Real: `new Zip()`/`new Unzip()` with the reference's open/close/add/list/extract \
            surface, `Storages.mountZip`/`unmountZip`, and a `zip://<domain>/<entry>` media \
            registered through the plugin storage seam (`TVPRegisterStorageMedia`). Reads decode \
            stored/deflated entries (hand-rolled RFC 1951 decompressor) and verify the CRC; the \
            writer stores or fixed-Huffman-deflates, computes CRCs, encrypts with PKWARE \
            ZipCrypto and appends by rewriting the central directory. Deviations: whole-buffer \
            storage writes instead of a writable IStream, entry data materialized in memory, \
            flag-less entry names decoded through CP932 (the reference's ANSI code page on its \
            Japanese hosts; UTF-8-as-is or CP437 only for bytes CP932 cannot represent, where the \
            reference throws), DOS dates treated as UTC, CRC-mismatched extraction reports failure.",
    install: |engine| engine.register_plugin(MinizipPlugin::new()),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what
/// `Plugins.link("minizip.dll")` resolves through [`crate::catalog`].
pub(crate) const NAME: &str = "minizip.dll";

/// The media name `ZipStorage::GetName` assigns (`storage.cpp:18, 433-435`).
pub(crate) const MEDIA_NAME: &str = "zip";

/// The banner `PreRegistCallback` logs through `TVPAddImportantLog`
/// (`main.cpp:12-16, 547-551`).
const BANNER: &str = "----- MiniZip Copyright START -----\n\
MiniZip/MiniUnz 1.01b, demo of zLib + Zip package written by Gilles Vollant\n\
more info at http://www.winimage.com/zLibDll/minizip.html\n\
----- MiniZip Copyright END -----";

/// `don't open zipfile` (`main.cpp:234, 407, 478`).
const NOT_OPEN: &str = "don't open zipfile";

/// `%1:cannot open zipfile` (`storage.cpp:476`), with `%1` filled by the name
/// the media was handed.
const MEDIA_OPEN_ERROR: &str = "cannot open zipfile";

// ---------------------------------------------------------------------------
// Plugin lifecycle
// ---------------------------------------------------------------------------

pub struct MinizipPlugin {
    /// The media instance. `register` runs at boot and again on `Plugins.link`,
    /// and the storage registry accepts a second registration only when it is
    /// the *same* `Arc` (the `lzfs` precedent,
    /// `crates/krkr-engine/src/plugin_api/storage.rs:26-40`).
    media: OnceLock<Arc<ZipMedia>>,
    /// Whether this instance has registered before. A new engine gives a new
    /// plugin instance and therefore a fresh instance pool (the `motion_player`
    /// precedent for handle-keyed thread-local state); the *same* instance
    /// registering again must not clear live state.
    registered: std::sync::atomic::AtomicBool,
}

impl MinizipPlugin {
    pub fn new() -> Self {
        Self {
            media: OnceLock::new(),
            registered: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl Default for MinizipPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KrkrPlugin for MinizipPlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let first = !self
            .registered
            .swap(true, std::sync::atomic::Ordering::SeqCst);
        if first {
            // A fresh engine hands out TJS object handles from zero again; the
            // instance pools live in this crate, so they have to be cleared
            // before a new engine installs the classes.
            clear_instance_pools();
        }

        let media = Arc::clone(self.media.get_or_init(|| Arc::new(ZipMedia::new())));
        let registered = plugin_api::storage::register_storage_media(
            runtime,
            Arc::clone(&media) as Arc<dyn StorageMediaProvider>,
        );
        match registered {
            Ok(()) => {
                if first {
                    runtime.host_mut().log(&format!(
                        "minizip: {BANNER}\nmedia `{MEDIA_NAME}` registered — \
                         `{MEDIA_NAME}://<domain>/<entry>` names resolve against the zips \
                         mounted with `Storages.mountZip`, read-only"
                    ));
                }
            }
            // A host without project storage has no media registry and no way
            // to address `zip://` at all; the classes still work against
            // whatever storage the host does have (`lzfs` takes the same path).
            Err(error) if runtime.host().project_storage().is_err() => {
                runtime.host_mut().log(&format!(
                    "WARN minizip: media `{MEDIA_NAME}` not registered: {error}"
                ));
            }
            Err(error) => return Err(error),
        }

        install_minizip(runtime, &media);
        Ok(())
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // `doneZipStorage` unregisters the media when the module goes away
        // (`storage.cpp:579-585, 629-631`); the reference's `ZipStorage` object
        // dies with it, so every mount goes too.
        plugin_api::storage::unregister_storage_media(runtime, MEDIA_NAME);
        if let Some(media) = self.media.get() {
            media.clear_mounts();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Per-instance state
// ---------------------------------------------------------------------------

/// One open `Zip` writer.
struct ZipInstance {
    /// The storage name the archive is written back to.
    storage_name: String,
    writer: ZipWriter,
}

/// One open `Unzip` reader.
struct UnzipInstance {
    archive: Arc<ReadArchive>,
}

thread_local! {
    static ZIP_INSTANCES: std::cell::RefCell<BTreeMap<ObjectHandle, ZipInstance>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
    static UNZIP_INSTANCES: std::cell::RefCell<BTreeMap<ObjectHandle, UnzipInstance>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

fn clear_instance_pools() {
    ZIP_INSTANCES.with(|map| map.borrow_mut().clear());
    UNZIP_INSTANCES.with(|map| map.borrow_mut().clear());
}

/// The object a native method acts on: the bound `this`, or the object itself,
/// never the global object. `None` means the method was called without an
/// instance (the reference would crash on a null `self`; a clean error is
/// better).
fn instance_handle(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    let handle = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))?;
    (handle != runtime.global_handle()).then_some(handle)
}

fn bound_instance(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    class_name: &'static str,
) -> ObjectHandle {
    let instance = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(instance, class_name);
    instance
}

// ---------------------------------------------------------------------------
// TJS surface installation
// ---------------------------------------------------------------------------

fn install_minizip(runtime: &mut Runtime<KrkrHost>, media: &Arc<ZipMedia>) {
    install_zip_class(runtime);
    install_unzip_class(runtime);
    install_storages_members(runtime, media);
}

fn install_zip_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "Zip");
            install_zip_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "Zip");
    install_zip_members(runtime, class);
    runtime.set_global_member("Zip", Variant::Object(class));
}

fn install_unzip_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "Unzip");
            install_unzip_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "Unzip");
    install_unzip_members(runtime, class);
    runtime.set_global_member("Unzip", Variant::Object(class));
}

fn install_zip_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // `RawCallback("open", …)` checks `numparams < 1` and `add` checks `< 2`
    // in the handler (`main.cpp:181, 232`); `close` is an argument-less method
    // (`main.cpp:529`).
    runtime.register_object_native_with_arg_count(
        handle,
        "open",
        NativeArgCount::AtLeast(1),
        zip_open,
    );
    runtime.register_object_native(handle, "close", zip_close);
    runtime.register_object_native_with_arg_count(
        handle,
        "add",
        NativeArgCount::AtLeast(2),
        zip_add,
    );
}

fn install_unzip_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // `Unzip::open(const tjs_char*)` and `extract` needs two arguments
    // (`main.cpp:373, 476`); `list`/`close` take none.
    runtime.register_object_native_with_arg_count(
        handle,
        "open",
        NativeArgCount::AtLeast(1),
        unzip_open,
    );
    runtime.register_object_native(handle, "close", unzip_close);
    runtime.register_object_native(handle, "list", unzip_list);
    runtime.register_object_native_with_arg_count(
        handle,
        "extract",
        NativeArgCount::AtLeast(2),
        unzip_extract,
    );
}

/// `NCB_ATTACH_CLASS(StoragesZip, Storages)` (`storage.cpp:568-622`): the two
/// mount members on the global `Storages` object.
fn install_storages_members(runtime: &mut Runtime<KrkrHost>, media: &Arc<ZipMedia>) {
    let storages = match runtime.global_member("Storages") {
        Variant::Object(handle) => handle,
        _ => return,
    };
    let mount_media = Arc::clone(media);
    runtime.register_object_native_with_arg_count(
        storages,
        "mountZip",
        NativeArgCount::AtLeast(2),
        move |runtime: &mut Runtime<KrkrHost>, _this: Option<ObjectHandle>, args: Vec<Variant>| {
            storages_mount_zip(runtime, &mount_media, args)
        },
    );
    let unmount_media = Arc::clone(media);
    runtime.register_object_native_with_arg_count(
        storages,
        "unmountZip",
        NativeArgCount::AtLeast(1),
        move |_runtime: &mut Runtime<KrkrHost>, _this: Option<ObjectHandle>, args: Vec<Variant>| {
            let domain = args
                .first()
                .ok_or_else(TjsError::bad_param_count)?
                .to_tjs_string()?;
            Ok(Variant::Integer(i64::from(unmount_media.unmount(&domain))))
        },
    );
}

// ---------------------------------------------------------------------------
// `Zip` natives (`main.cpp:176-328`)
// ---------------------------------------------------------------------------

/// `Zip.open(filename, overwrite=0)`: `1` overwrites, `2` appends when the
/// archive exists (`main.cpp:176-207`).
fn zip_open(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let filename = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let mut overwrite = match args.get(1) {
        Some(value) => value.to_integer()?,
        None => 0,
    };

    let storage = runtime
        .host()
        .project_storage()
        .map_err(|error| TjsError::runtime(error.to_string()))?;
    let exists = storage.storage_exists(&filename);
    if overwrite == 2 {
        // "append, but create when it is not there" (`main.cpp:186-190`).
        if !exists {
            overwrite = 1;
        }
    } else if overwrite == 0 && exists {
        return Err(TjsError::runtime(format!("{filename} exists.")));
    }

    let writer = if overwrite == 2 {
        let data = storage
            .read_binary_storage(&filename)
            .map_err(|_| TjsError::runtime(format!("{filename} can't open.")))?;
        let bytes = data
            .as_bytes()
            .map_err(|_| TjsError::runtime(format!("{filename} can't open.")))?
            .into_owned();
        ZipWriter::from_image(bytes)
            .map_err(|_| TjsError::runtime(format!("{filename} can't open.")))?
    } else {
        ZipWriter::new()
    };

    // `zipOpen2_64` creates/truncates the output immediately (`main.cpp:200`),
    // so the storage name exists while the archive is being built.
    storage
        .write_binary_storage(&filename, "", writer.image())
        .map_err(|_| TjsError::runtime(format!("{filename} can't open.")))?;

    ZIP_INSTANCES.with(|map| {
        map.borrow_mut().insert(
            instance,
            ZipInstance {
                storage_name: filename,
                writer,
            },
        );
    });
    Ok(Variant::Void)
}

/// `Zip.close()`: finalize the central directory, write the archive back and
/// forget the state (`main.cpp:212-217`).
fn zip_close(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(instance) = instance_handle(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let state = ZIP_INSTANCES.with(|map| map.borrow_mut().remove(&instance));
    let Some(mut state) = state else {
        return Ok(Variant::Void);
    };
    state
        .writer
        .finish()
        .map_err(|error| TjsError::runtime(error.to_string()))?;
    let storage = runtime
        .host()
        .project_storage()
        .map_err(|error| TjsError::runtime(error.to_string()))?;
    storage
        .write_binary_storage(&state.storage_name, "", state.writer.image())
        .map_err(|_| TjsError::runtime(format!("{} can't open.", state.storage_name)))?;
    Ok(Variant::Void)
}

/// `Zip.add(srcfile, destfile, deflateLevel=void, password=void) → bool`
/// (`main.cpp:227-328`).
fn zip_add(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    if args.len() < 2 {
        return Err(TjsError::bad_param_count());
    }
    let srcname = args[0].to_tjs_string()?;
    let destname = args[1].to_tjs_string()?;
    // `int compressLevel = numparams > 2 ? (int)*param[2] : Z_DEFAULT_COMPRESSION`
    // (`main.cpp:239`): an explicit `void` casts to 0, which stores the entry
    // uncompressed, while an absent argument is zlib's default level.
    let level = match args.get(2) {
        Some(value) => value.to_integer()?,
        None => Z_DEFAULT_COMPRESSION,
    };
    // `param[3]->Type() == tvtString` (`main.cpp:242`): a non-string password
    // is ignored, not an error.
    let password = match args.get(3) {
        Some(Variant::String(text)) => Some(text.clone()),
        _ => None,
    };
    // The reference checks `self->zf` before anything else (`main.cpp:233-235`):
    // an unopened archive answers `don't open zipfile` even for a source that
    // does not exist.
    if !ZIP_INSTANCES.with(|map| map.borrow().contains_key(&instance)) {
        return Err(TjsError::runtime(NOT_OPEN));
    }

    let storage = runtime
        .host()
        .project_storage()
        .map_err(|error| TjsError::runtime(error.to_string()))?;
    if !storage.storage_exists(&srcname) {
        return Err(TjsError::runtime(format!("{srcname} not exists.")));
    }
    let Ok(data) = storage.read_binary_storage(&srcname) else {
        // The reference leaves `ret` uninitialized when `TVPCreateIStream`
        // fails (`main.cpp:285-321`); "the entry was not added" is the only
        // meaningful answer a script can use.
        return Ok(Variant::Integer(0));
    };
    let data = data
        .as_bytes()
        .map_err(|error| TjsError::runtime(error.to_string()))?;

    // The DOS timestamp comes from the source's local file when it has one and
    // from the clock otherwise (`main.cpp:254-281`).
    let dos = match storage.placed_path(&srcname) {
        Some(path) => std::fs::metadata(&path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(dos_from_system_time)
            .unwrap_or_else(|| dos_from_system_time(std::time::SystemTime::now())),
        None => dos_from_system_time(std::time::SystemTime::now()),
    };

    let added: Result<Option<String>> = ZIP_INSTANCES.with(|map| {
        let mut borrowed = map.borrow_mut();
        let Some(state) = borrowed.get_mut(&instance) else {
            return Err(TjsError::runtime(NOT_OPEN));
        };
        state
            .writer
            .add_entry(&destname, &data, level, password.as_deref(), dos)
            .map_err(|error| TjsError::runtime(error.to_string()))?;
        state
            .writer
            .finish()
            .map_err(|error| TjsError::runtime(error.to_string()))?;
        let storage_name = state.storage_name.clone();
        let written = runtime
            .host()
            .project_storage()
            .map_err(|error| TjsError::runtime(error.to_string()))?
            .write_binary_storage(&storage_name, "", state.writer.image());
        match written {
            Ok(()) => Ok(None),
            Err(error) => Ok(Some(format!(
                "WARN minizip: cannot write `{storage_name}`: {error}"
            ))),
        }
    });
    match added? {
        None => Ok(Variant::Integer(1)),
        Some(warning) => {
            runtime.host_mut().log(&warning);
            Ok(Variant::Integer(0))
        }
    }
}

// ---------------------------------------------------------------------------
// `Unzip` natives (`main.cpp:373-522`)
// ---------------------------------------------------------------------------

/// `Unzip.open(filename)` (`main.cpp:373-385`).
fn unzip_open(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let filename = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let stream = runtime
        .host()
        .project_storage()
        .map_err(|error| TjsError::runtime(error.to_string()))?
        .open(&filename)
        .map_err(|_| TjsError::runtime(format!("{filename} can't open.")))?;
    let archive = ReadArchive::new(stream)
        .map_err(|_| TjsError::runtime(format!("{filename} can't open.")))?;
    UNZIP_INSTANCES.with(|map| {
        map.borrow_mut().insert(
            instance,
            UnzipInstance {
                archive: Arc::new(archive),
            },
        );
    });
    Ok(Variant::Void)
}

/// `Unzip.close()` (`main.cpp:390-395`).
fn unzip_close(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(instance) = this_obj {
        UNZIP_INSTANCES.with(|map| map.borrow_mut().remove(&instance));
    }
    Ok(Variant::Void)
}

/// `Unzip.list()` (`main.cpp:401-464`): an Array of Dictionaries in the
/// reference's member order.
fn unzip_list(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let archive = UNZIP_INSTANCES.with(|map| {
        map.borrow()
            .get(&instance)
            .map(|state| Arc::clone(&state.archive))
    });
    let Some(archive) = archive else {
        return Err(TjsError::runtime(NOT_OPEN));
    };

    let mut items = Vec::with_capacity(archive.entries.len());
    for entry in &archive.entries {
        let object = runtime.alloc_dictionary_object();
        runtime.set_object_member(object, "filename", Variant::String(entry.name.clone()));
        runtime.set_object_member(
            object,
            "uncompressed_size",
            Variant::Integer(entry.uncompressed_size as i64),
        );
        runtime.set_object_member(
            object,
            "compressed_size",
            Variant::Integer(entry.compressed_size as i64),
        );
        runtime.set_object_member(
            object,
            "crypted",
            Variant::Integer(i64::from(entry.is_encrypted())),
        );
        runtime.set_object_member(
            object,
            "deflated",
            Variant::Integer(i64::from(entry.is_deflated())),
        );
        runtime.set_object_member(
            object,
            "deflateLevel",
            Variant::Integer(entry.deflate_level_hint()),
        );
        runtime.set_object_member(object, "crc", Variant::Integer(i64::from(entry.crc)));
        // `setDateProp` skips the member entirely for a zero FILETIME
        // (`main.cpp:126-144`).
        if let Some(millis) = unix_millis_from_dos(entry.dos_date, entry.dos_time) {
            let date = runtime
                .call_function(
                    runtime.global_member("Date"),
                    vec![Variant::Integer(millis)],
                )
                .unwrap_or(Variant::Void);
            runtime.set_object_member(object, "date", date);
        }
        items.push(Variant::Object(object));
    }
    Ok(Variant::Object(runtime.alloc_array_object(items)))
}

/// `Unzip.extract(srcname, destfile, password=void) → bool`
/// (`main.cpp:472-522`).
fn unzip_extract(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    if args.len() < 2 {
        return Err(TjsError::bad_param_count());
    }
    let srcname = args[0].to_tjs_string()?;
    let destname = args[1].to_tjs_string()?;
    let password = match args.get(2) {
        Some(Variant::String(text)) => Some(text.clone()),
        _ => None,
    };

    let archive = UNZIP_INSTANCES.with(|map| {
        map.borrow()
            .get(&instance)
            .map(|state| Arc::clone(&state.archive))
    });
    let Some(archive) = archive else {
        return Err(TjsError::runtime(NOT_OPEN));
    };

    // `unzLocateFile` and `unzOpenCurrentFilePassword` failures both answer
    // `false` (`main.cpp:491-512`); only a destination that cannot be opened
    // throws.
    let Some(entry) = locate_entry(&archive.entries, &srcname).cloned() else {
        return Ok(Variant::Integer(0));
    };
    let Ok(data) = archive.read_entry(&entry, password.as_deref()) else {
        return Ok(Variant::Integer(0));
    };
    runtime
        .host_mut()
        .write_binary_storage(&destname, "", &data)
        .map_err(|_| TjsError::runtime(format!("{destname} can't open.")))?;
    Ok(Variant::Integer(1))
}

// ---------------------------------------------------------------------------
// `Storages.mountZip` / `unmountZip`
// ---------------------------------------------------------------------------

/// `StoragesZip::mountZip(name, zipfile)` (`storage.cpp:594-599`): open the
/// container through the ordinary storage search and index it.
fn storages_mount_zip(
    runtime: &mut Runtime<KrkrHost>,
    media: &Arc<ZipMedia>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let domain = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let zipfile = args
        .get(1)
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let stream = runtime
        .host()
        .project_storage()
        .map_err(|error| TjsError::runtime(error.to_string()))?
        .open(&zipfile);
    let mounted = match stream {
        Ok(stream) => media.mount(&domain, stream),
        Err(_) => false,
    };
    Ok(Variant::Integer(i64::from(mounted)))
}

// ---------------------------------------------------------------------------
// The `zip` storage media (`storage.cpp:393-562`)
// ---------------------------------------------------------------------------

/// The media table entry: one mounted archive plus the directory listing the
/// reference builds once at mount (`UnzipBase::init` → `entryName`,
/// `storage.cpp:53-76, 198-210`).
struct MountedZip {
    archive: Arc<ReadArchive>,
    listing: BTreeMap<String, Vec<String>>,
}

/// The `zip` media: `zip://<domain>/<entry>` for every domain mounted with
/// `Storages.mountZip`.
struct ZipMedia {
    mounted: Mutex<BTreeMap<String, MountedZip>>,
}

impl ZipMedia {
    fn new() -> Self {
        Self {
            mounted: Mutex::new(BTreeMap::new()),
        }
    }

    fn let_mounted<R>(&self, run: impl FnOnce(&BTreeMap<String, MountedZip>) -> R) -> R {
        let guard = self
            .mounted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        run(&guard)
    }

    /// `ZipStorage::mount` (`storage.cpp:505-517`): replacing an existing
    /// domain is implicit (`unmount` runs first), and a container that does not
    /// index answers `false` instead of installing a broken mount.
    fn mount(&self, domain: &str, stream: Box<dyn ResourceStream>) -> bool {
        let Ok(archive) = ReadArchive::new(stream) else {
            return false;
        };
        let listing = directory_table(&archive.entries);
        let mut guard = self
            .mounted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.insert(
            domain.to_string(),
            MountedZip {
                archive: Arc::new(archive),
                listing,
            },
        );
        true
    }

    /// `ZipStorage::unmount` (`storage.cpp:524-532`): `true` only when a mount
    /// was removed.
    fn unmount(&self, domain: &str) -> bool {
        let mut guard = self
            .mounted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(domain).is_some()
    }

    /// Drops every mount. `doneZipStorage` releases the whole `ZipStorage`
    /// object when the module unloads (`storage.cpp:579-585`), which takes its
    /// `unzipTable` with it.
    fn clear_mounts(&self) {
        self.mounted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    fn domain_archive(&self, domain: &str) -> Option<Arc<ReadArchive>> {
        self.let_mounted(|mounted| mounted.get(domain).map(|zip| Arc::clone(&zip.archive)))
    }
}

/// The reference splits the media name space at the first `/`: `domain/path`
/// (`storage.cpp:542-557`); a name without one is `invalid path:%1`.
fn split_domain(name: &str) -> Option<(&str, &str)> {
    name.split_once('/')
}

impl StorageMediaProvider for ZipMedia {
    fn media_name(&self) -> &str {
        MEDIA_NAME
    }

    fn exists(&self, name: &str) -> bool {
        // The reference's `CheckExistentStorage` throws for a name without a
        // `/` (`storage.cpp:542-557`), but an existence probe must not report
        // an error through this trait (`krkr-core/src/media.rs:69-73`), so the
        // name is a plain miss.
        let Some((domain, path)) = split_domain(name) else {
            return false;
        };
        let Some(archive) = self.domain_archive(domain) else {
            return false;
        };
        locate_entry(&archive.entries, path).is_some()
    }

    fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
        let Some((domain, path)) = split_domain(name) else {
            return Err(media_error(format!("invalid path:{name}")));
        };
        let archive = self
            .domain_archive(domain)
            .ok_or_else(|| media_error(format!("{name}:{MEDIA_OPEN_ERROR}")))?;
        let entry = locate_entry(&archive.entries, path)
            .cloned()
            .ok_or_else(|| media_error(format!("{name}:{MEDIA_OPEN_ERROR}")))?;
        // The media has no password parameter — a `zip://` read of an
        // encrypted entry cannot succeed, exactly like the reference's
        // `unzOpenCurrentFile` without a password.
        let data = archive
            .read_entry(&entry, None)
            .map_err(|_| media_error(format!("{name}:{MEDIA_OPEN_ERROR}")))?;
        Ok(Box::new(Cursor::new(data)))
    }

    fn list(&self, name: &str) -> io::Result<Vec<String>> {
        let Some((domain, path)) = split_domain(name) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("invalid path:{name}"),
            ));
        };
        let Some(zip) = self.let_mounted(|mounted| {
            mounted
                .get(domain)
                .map(|zip| zip.listing.get(&format!("/{path}")).cloned())
        }) else {
            return Ok(Vec::new());
        };
        Ok(zip.unwrap_or_default())
    }

    /// `Open` serves `TJS_BS_READ` only and throws `"%1:cannot open zipfile"`
    /// otherwise (`storage.cpp:460-478`); the engine's write branch lands here.
    fn write(&self, name: &str, _mode: &str, _bytes: &[u8]) -> io::Result<()> {
        Err(media_error(format!("{name}:{MEDIA_OPEN_ERROR}")))
    }
}

/// The reference's message shape: `%1:cannot open zipfile` with `%1` the name
/// the media was handed (`storage.cpp:476`).
fn media_error(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, message)
}

// ---------------------------------------------------------------------------
// ZIP format core
// ---------------------------------------------------------------------------
//
// One in-file implementation of the parts of the ZIP format the reference uses
// through zlib's contrib minizip. Nothing here is shared with `krkr-xp3`'s
// flate2 dependency: this crate's production dependencies are the engine and
// the TJS runtime only, and the batch rules do not allow new ones.

const LOCAL_HEADER_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_HEADER_SIGNATURE: u32 = 0x0201_4b50;
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const ZIP64_EOCD_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_EXTRA_ID: u16 = 0x0001;

/// `FLAG_UTF8 (1<<11)` (`main.cpp:37-38`); bit 0 is minizip's encryption flag.
const FLAG_ENCRYPTED: u16 = 1 << 0;
const FLAG_UTF8: u16 = 1 << 11;

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATED: u16 = 8;

/// `Z_DEFAULT_COMPRESSION` (`zlib.h`), the level `Zip.add` uses when the
/// argument is absent (`main.cpp:239`).
const Z_DEFAULT_COMPRESSION: i64 = -1;

/// The largest EOCD search window: the 22-byte record plus a 64 KiB comment.
const MAX_EOCD_SEARCH: usize = 65_557;

/// An upper bound for one entry's uncompressed size. A wider declared size is
/// refused before any allocation, so a crafted archive cannot ask the engine
/// for an unbounded buffer.
const MAX_ENTRY_SIZE: u64 = u32::MAX as u64;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// One central-directory record, as `list`/`open` need it.
#[derive(Clone, Debug)]
struct Entry {
    /// The name bytes exactly as stored.
    name_raw: Vec<u8>,
    /// The name as a script sees it (`storeFilename`, `main.cpp:334-350`).
    name: String,
    flags: u16,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    local_header_offset: u64,
}

impl Entry {
    fn is_encrypted(&self) -> bool {
        self.flags & FLAG_ENCRYPTED != 0
    }

    fn is_deflated(&self) -> bool {
        self.method == METHOD_DEFLATED
    }

    /// `(file_info.flag & 0x6) / 2` (`main.cpp:430`).
    fn deflate_level_hint(&self) -> i64 {
        i64::from((self.flags & 0x6) >> 1)
    }
}

/// A parsed archive index over a seekable container.
struct ReadArchive {
    stream: Mutex<Box<dyn ResourceStream>>,
    entries: Vec<Entry>,
}

impl ReadArchive {
    fn new(stream: Box<dyn ResourceStream>) -> io::Result<Self> {
        let mut stream = stream;
        let entries = read_archive_index(&mut stream)?;
        Ok(Self {
            stream: Mutex::new(stream),
            entries,
        })
    }

    fn read_entry(&self, entry: &Entry, password: Option<&str>) -> io::Result<Vec<u8>> {
        let mut stream = self
            .stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        decode_entry(&mut **stream, entry, password)
    }
}

/// Reads the end-of-central-directory record and the central directory,
/// returning every entry in file order.
fn read_archive_index<R: Read + Seek>(reader: &mut R) -> io::Result<Vec<Entry>> {
    let length = reader.seek(SeekFrom::End(0))?;
    if length < 22 {
        return Err(invalid("not a zip archive"));
    }
    let eocd = find_eocd(reader, length)?;

    if eocd.central_directory_offset > length
        || eocd.central_directory_size > length
        || eocd
            .central_directory_offset
            .saturating_add(eocd.central_directory_size)
            > length
    {
        return Err(invalid("central directory is out of bounds"));
    }
    let mut directory = vec![0u8; eocd.central_directory_size as usize];
    reader.seek(SeekFrom::Start(eocd.central_directory_offset))?;
    reader.read_exact(&mut directory)?;
    parse_central_directory(&directory)
}

struct EndOfCentralDirectory {
    central_directory_offset: u64,
    central_directory_size: u64,
}

/// Finds the last end-of-central-directory record, following the Zip64
/// locator/record when the EOCD's 32-bit fields are saturated.
fn find_eocd<R: Read + Seek>(reader: &mut R, length: u64) -> io::Result<EndOfCentralDirectory> {
    let window = length.min(MAX_EOCD_SEARCH as u64) as usize;
    let window_start = length - window as u64;
    let mut tail = vec![0u8; window];
    reader.seek(SeekFrom::Start(window_start))?;
    reader.read_exact(&mut tail)?;

    let mut found = None;
    for index in (0..=tail.len() - 22).rev() {
        if u32::from_le_bytes(tail[index..index + 4].try_into().expect("4 bytes")) != EOCD_SIGNATURE
        {
            continue;
        }
        // A signature inside a comment must not win: the comment length has to
        // account for exactly the bytes behind the record.
        let comment_length =
            u16::from_le_bytes(tail[index + 20..index + 22].try_into().expect("2 bytes")) as usize;
        if index + 22 + comment_length == tail.len() {
            found = Some(index);
            break;
        }
    }
    let index = found.ok_or_else(|| invalid("end of central directory not found"))?;
    let record = &tail[index..];

    let disk = u16::from_le_bytes(record[4..6].try_into().expect("2 bytes"));
    let directory_disk = u16::from_le_bytes(record[6..8].try_into().expect("2 bytes"));
    if disk != 0 || directory_disk != 0 {
        return Err(invalid("multi-volume zip archives are not supported"));
    }
    let mut entries = u64::from(u16::from_le_bytes(
        record[10..12].try_into().expect("2 bytes"),
    ));
    let mut directory_size = u64::from(u32::from_le_bytes(
        record[12..16].try_into().expect("4 bytes"),
    ));
    let mut directory_offset = u64::from(u32::from_le_bytes(
        record[16..20].try_into().expect("4 bytes"),
    ));

    let saturated =
        entries == 0xFFFF || directory_size == 0xFFFF_FFFF || directory_offset == 0xFFFF_FFFF;
    if saturated {
        // The Zip64 EOCD locator sits directly before the EOCD record.
        let eocd_offset = window_start + index as u64;
        if eocd_offset >= 20
            && let Some((size, offset, count)) = read_zip64_eocd(reader, eocd_offset - 20)?
        {
            directory_size = size;
            directory_offset = offset;
            entries = count;
        }
    }
    let _ = entries;
    Ok(EndOfCentralDirectory {
        central_directory_offset: directory_offset,
        central_directory_size: directory_size,
    })
}

/// Reads the Zip64 EOCD record through its locator; `None` when the locator is
/// absent or unusable, in which case the 32-bit fields stay as read.
fn read_zip64_eocd<R: Read + Seek>(
    reader: &mut R,
    locator_offset: u64,
) -> io::Result<Option<(u64, u64, u64)>> {
    reader.seek(SeekFrom::Start(locator_offset))?;
    let mut locator = [0u8; 20];
    if reader.read_exact(&mut locator).is_err() {
        return Ok(None);
    }
    if u32::from_le_bytes(locator[0..4].try_into().expect("4 bytes"))
        != ZIP64_EOCD_LOCATOR_SIGNATURE
    {
        return Ok(None);
    }
    let record_offset = u64::from_le_bytes(locator[8..16].try_into().expect("8 bytes"));
    reader.seek(SeekFrom::Start(record_offset))?;
    let mut record = [0u8; 56];
    reader.read_exact(&mut record)?;
    if u32::from_le_bytes(record[0..4].try_into().expect("4 bytes")) != ZIP64_EOCD_SIGNATURE {
        return Err(invalid("Zip64 end of central directory not found"));
    }
    let entries = u64::from_le_bytes(record[32..40].try_into().expect("8 bytes"));
    let directory_size = u64::from_le_bytes(record[40..48].try_into().expect("8 bytes"));
    let directory_offset = u64::from_le_bytes(record[48..56].try_into().expect("8 bytes"));
    Ok(Some((directory_size, directory_offset, entries)))
}

fn parse_central_directory(directory: &[u8]) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut position = 0usize;
    while position + 46 <= directory.len() {
        let record = &directory[position..];
        if u32::from_le_bytes(record[0..4].try_into().expect("4 bytes")) != CENTRAL_HEADER_SIGNATURE
        {
            return Err(invalid("central directory record signature mismatch"));
        }
        let flags = u16::from_le_bytes(record[8..10].try_into().expect("2 bytes"));
        let method = u16::from_le_bytes(record[10..12].try_into().expect("2 bytes"));
        let dos_time = u16::from_le_bytes(record[12..14].try_into().expect("2 bytes"));
        let dos_date = u16::from_le_bytes(record[14..16].try_into().expect("2 bytes"));
        let crc = u32::from_le_bytes(record[16..20].try_into().expect("4 bytes"));
        let mut compressed_size = u64::from(u32::from_le_bytes(
            record[20..24].try_into().expect("4 bytes"),
        ));
        let mut uncompressed_size = u64::from(u32::from_le_bytes(
            record[24..28].try_into().expect("4 bytes"),
        ));
        let name_length = u16::from_le_bytes(record[28..30].try_into().expect("2 bytes")) as usize;
        let extra_length = u16::from_le_bytes(record[30..32].try_into().expect("2 bytes")) as usize;
        let comment_length =
            u16::from_le_bytes(record[32..34].try_into().expect("2 bytes")) as usize;
        let mut local_header_offset = u64::from(u32::from_le_bytes(
            record[42..46].try_into().expect("4 bytes"),
        ));
        let record_length = 46 + name_length + extra_length + comment_length;
        if position + record_length > directory.len() {
            return Err(invalid("central directory record is truncated"));
        }
        let name_raw = directory[position + 46..position + 46 + name_length].to_vec();
        let extra =
            &directory[position + 46 + name_length..position + 46 + name_length + extra_length];

        // `0xFFFFFFFF` fields are superseded by the Zip64 extra field, in the
        // order the specification fixes: uncompressed, compressed, offset.
        if (uncompressed_size == u64::from(u32::MAX)
            || compressed_size == u64::from(u32::MAX)
            || local_header_offset == u64::from(u32::MAX))
            && let Some(values) = zip64_extra_values(extra)
        {
            let mut values = values.into_iter();
            if uncompressed_size == u64::from(u32::MAX) {
                uncompressed_size = values
                    .next()
                    .ok_or_else(|| invalid("Zip64 extra field is too short"))?;
            }
            if compressed_size == u64::from(u32::MAX) {
                compressed_size = values
                    .next()
                    .ok_or_else(|| invalid("Zip64 extra field is too short"))?;
            }
            if local_header_offset == u64::from(u32::MAX) {
                local_header_offset = values
                    .next()
                    .ok_or_else(|| invalid("Zip64 extra field is too short"))?;
            }
        }

        let name = decode_name(&name_raw, flags & FLAG_UTF8 != 0);
        entries.push(Entry {
            name_raw,
            name,
            flags,
            method,
            dos_time,
            dos_date,
            crc,
            compressed_size,
            uncompressed_size,
            local_header_offset,
        });
        position += record_length;
    }
    Ok(entries)
}

/// The 64-bit values of a Zip64 extra field (`0x0001`), in order.
fn zip64_extra_values(extra: &[u8]) -> Option<Vec<u64>> {
    let mut position = 0usize;
    while position + 4 <= extra.len() {
        let id = u16::from_le_bytes(extra[position..position + 2].try_into().expect("2 bytes"));
        let size = u16::from_le_bytes(
            extra[position + 2..position + 4]
                .try_into()
                .expect("2 bytes"),
        ) as usize;
        let body = extra.get(position + 4..position + 4 + size)?;
        if id == ZIP64_EXTRA_ID {
            let mut values = Vec::new();
            let mut offset = 0usize;
            while offset + 8 <= body.len() {
                values.push(u64::from_le_bytes(
                    body[offset..offset + 8].try_into().expect("8 bytes"),
                ));
                offset += 8;
            }
            return Some(values);
        }
        position += 4 + size;
    }
    None
}

/// Decodes an entry name (`storeFilename`, `main.cpp:335-350`).
///
/// Flag bit 11 decides the encoding, and only that: a flagged name is UTF-8
/// (`MultiByteToWideChar(CP_UTF8, 0, …)`, `main.cpp:337-346`), and a flag-less
/// name is the host's ANSI code page — the `ttstr = const char*` assignment at
/// `main.cpp:348` reaches `SetString(const tjs_nchar*)`
/// (`tjsVariantString.h:108-122`) and therefore `TJS_mbstowcs`
/// (`tjsConfig.h:99-100`), which on Windows is
/// `MultiByteToWideChar(CP_ACP, MB_PRECOMPOSED|MB_ERR_INVALID_CHARS, …)`
/// (`tjsConfig.cpp:208-243`). CP932 is that code page on the Japanese Windows
/// hosts the reference ships to, and the engine already reads project text
/// through the same WHATWG table (`krkr-assets/src/storage.rs:2600`), so a
/// stored name and the script string asking for it land on the same code
/// points. Queries travel the other way through the same code page
/// (`NarrowString(srcname, utf8)`, `main.cpp:491`; `narrow.h:22-28`), which is
/// why resolving one against `locate_entry`'s decoded name is equivalent to the
/// reference's byte comparison.
///
/// Bytes the code page cannot represent leave the reference with no name at
/// all: `TJS_mbstowcs` returns `-1` and `SetString` throws
/// `TJSNarrowToWideConversionError` (`tjsVariantString.h:110-112`). This port
/// keeps the previous fallback there — valid UTF-8 as-is, otherwise CP437 —
/// rather than failing the archive.
fn decode_name(raw: &[u8], utf8: bool) -> String {
    if utf8 {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let (text, _, had_errors) = SHIFT_JIS.decode(raw);
    if !had_errors {
        return text.into_owned();
    }
    match std::str::from_utf8(raw) {
        Ok(text) => text.to_owned(),
        Err(_) => cp437(raw),
    }
}

/// The ZIP specification's default for un-flagged names, kept only for bytes
/// the reference's code page cannot decode at all (see [`decode_name`]).
fn cp437(raw: &[u8]) -> String {
    const HIGH: [char; 128] = [
        'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ',
        'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú',
        'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡',
        '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟',
        '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘',
        '┌', '█', '▄', '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ',
        '∞', 'φ', 'ε', '∩', '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²',
        '■', '\u{00a0}',
    ];
    raw.iter()
        .map(|&byte| {
            if byte < 0x80 {
                byte as char
            } else {
                HIGH[(byte - 0x80) as usize]
            }
        })
        .collect()
}

/// The reference's `unzLocateFile(..., CASESENSITIVITY)` with the Windows
/// default: ASCII-case-insensitive, first match in file order
/// (`unzip.c:96-100, 350-368, 383-413`). Both the stored bytes and the decoded
/// name are compared so a name written by either convention matches.
fn locate_entry<'a>(entries: &'a [Entry], query: &str) -> Option<&'a Entry> {
    entries.iter().find(|entry| {
        ascii_case_insensitive_eq(query.as_bytes(), &entry.name_raw)
            || ascii_case_insensitive_eq(query.as_bytes(), entry.name.as_bytes())
    })
}

fn ascii_case_insensitive_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(&left, &right)| left.eq_ignore_ascii_case(&right))
}

/// The reference's per-directory listing table (`UnzipBase::entryName`,
/// `storage.cpp:198-210`): `"/" + everything through the last slash` → the
/// name behind it. A directory entry (`"dir/"`) therefore contributes an empty
/// name to its own list and is not listed under its parent — that is what the
/// DLL does, empty string included.
fn directory_table(entries: &[Entry]) -> BTreeMap<String, Vec<String>> {
    let mut table: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in entries {
        let name = &entry.name;
        let (key, value) = match name.rfind('/') {
            Some(index) => (
                format!("/{}", &name[..index + 1]),
                name[index + 1..].to_owned(),
            ),
            None => ("/".to_owned(), name.clone()),
        };
        table.entry(key).or_default().push(value);
    }
    table
}

/// Reads and decodes one entry's data, verifying the size and the CRC.
fn decode_entry<R: Read + Seek + ?Sized>(
    reader: &mut R,
    entry: &Entry,
    password: Option<&str>,
) -> io::Result<Vec<u8>> {
    if entry.uncompressed_size > MAX_ENTRY_SIZE {
        return Err(invalid("entry is too large"));
    }
    let data_offset = local_data_offset(reader, entry)?;
    // The compressed size comes from the central directory, so a crafted
    // archive could claim anything; the allocation is bounded by the actual
    // container length first.
    let file_length = reader.seek(SeekFrom::End(0))?;
    let data_end = data_offset
        .checked_add(entry.compressed_size)
        .ok_or_else(|| invalid("entry data offset overflow"))?;
    if data_end > file_length {
        return Err(invalid("entry data is out of bounds"));
    }
    let mut compressed = vec![0u8; entry.compressed_size as usize];
    reader.seek(SeekFrom::Start(data_offset))?;
    reader.read_exact(&mut compressed)?;

    let compressed = if entry.is_encrypted() {
        let password = password.ok_or_else(|| invalid("password required"))?;
        decrypt_zipcrypto(compressed, password.as_bytes(), entry)?
    } else {
        compressed
    };

    let data = match entry.method {
        METHOD_STORED => compressed,
        METHOD_DEFLATED => inflate(&compressed, entry.uncompressed_size as usize)?,
        method => {
            return Err(invalid(&format!("unsupported compression method {method}")));
        }
    };

    if data.len() as u64 != entry.uncompressed_size {
        return Err(invalid("entry size mismatch"));
    }
    // Bit 3 entries carry their CRC in the central directory, which is where
    // this reader takes it from, so the check always has a value.
    if entry.crc != 0 && crc32(&data) != entry.crc {
        return Err(invalid("entry CRC mismatch"));
    }
    Ok(data)
}

/// The offset of an entry's payload: past the local header and its name/extra
/// fields. Entry sizes come from the central directory because a streaming
/// writer (bit 3) leaves zeros in the local header (minizip patches the local
/// header in place; this port writes the final values up front).
fn local_data_offset<R: Read + Seek + ?Sized>(reader: &mut R, entry: &Entry) -> io::Result<u64> {
    reader.seek(SeekFrom::Start(entry.local_header_offset))?;
    let mut header = [0u8; 30];
    reader.read_exact(&mut header)?;
    if u32::from_le_bytes(header[0..4].try_into().expect("4 bytes")) != LOCAL_HEADER_SIGNATURE {
        return Err(invalid("local file header signature mismatch"));
    }
    let name_length = u16::from_le_bytes(header[26..28].try_into().expect("2 bytes")) as u64;
    let extra_length = u16::from_le_bytes(header[28..30].try_into().expect("2 bytes")) as u64;
    entry
        .local_header_offset
        .checked_add(30 + name_length + extra_length)
        .ok_or_else(|| invalid("local header offset overflow"))
}

// ---------------------------------------------------------------------------
// ZipCrypto (PKWARE traditional encryption, minizip's `crypt.c`)
// ---------------------------------------------------------------------------

struct ZipCrypto {
    key0: u32,
    key1: u32,
    key2: u32,
}

impl ZipCrypto {
    fn new(password: &[u8]) -> Self {
        let mut cipher = Self {
            key0: 0x1234_5678,
            key1: 0x2345_6789,
            key2: 0x3456_7890,
        };
        for &byte in password {
            cipher.update(byte);
        }
        cipher
    }

    fn update(&mut self, byte: u8) {
        self.key0 = crc32_update(self.key0, byte);
        self.key1 = self
            .key1
            .wrapping_add(self.key0 & 0xff)
            .wrapping_mul(134_775_813)
            .wrapping_add(1);
        self.key2 = crc32_update(self.key2, (self.key1 >> 24) as u8);
    }

    fn stream_byte(&self) -> u8 {
        let temp = (self.key2 | 2) & 0xffff;
        (temp.wrapping_mul(temp ^ 1) >> 8) as u8
    }

    fn decrypt(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            *byte ^= self.stream_byte();
            self.update(*byte);
        }
    }

    fn encrypt(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            let plain = *byte;
            *byte = plain ^ self.stream_byte();
            self.update(plain);
        }
    }
}

/// Strips the 12-byte encryption header and decrypts the payload, checking the
/// password-check byte the way `unzOpenCurrentFile3` does (`crypt.c`): the
/// high byte of the CRC (or of the DOS time, for bit 3 entries).
fn decrypt_zipcrypto(mut data: Vec<u8>, password: &[u8], entry: &Entry) -> io::Result<Vec<u8>> {
    if data.len() < 12 {
        return Err(invalid("encrypted entry is too short"));
    }
    let mut cipher = ZipCrypto::new(password);
    let mut header = data[..12].to_vec();
    cipher.decrypt(&mut header);
    let expected = if entry.flags & 0x8 != 0 {
        (entry.dos_time >> 8) as u8
    } else {
        (entry.crc >> 24) as u8
    };
    if header[11] != expected {
        return Err(invalid("incorrect password"));
    }
    let mut payload = data.split_off(12);
    cipher.decrypt(&mut payload);
    Ok(payload)
}

/// Encrypts `plain` under PKWARE ZipCrypto: a 12-byte header whose last byte
/// is the password-check byte, then the encrypted payload.
fn encrypt_zipcrypto(plain: &[u8], password: &[u8], check: u8, seed: u64) -> Vec<u8> {
    let mut cipher = ZipCrypto::new(password);
    let mut header = [0u8; 12];
    let mut state = seed | 1;
    for byte in header.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = (state >> 24) as u8;
    }
    header[11] = check;
    cipher.encrypt(&mut header);

    let mut payload = plain.to_vec();
    cipher.encrypt(&mut payload);

    let mut out = Vec::with_capacity(12 + payload.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&payload);
    out
}

// ---------------------------------------------------------------------------
// CRC-32 (the ZIP polynomial, also the ZipCrypto key schedule)
// ---------------------------------------------------------------------------

const CRC_TABLE: [u32; 256] = build_crc_table();

const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0usize;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

fn crc32_update(crc: u32, byte: u8) -> u32 {
    (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize]
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = crc32_update(crc, byte);
    }
    !crc
}

// ---------------------------------------------------------------------------
// DOS timestamps (`zip64local_TmzDateToDosDate` and `zip64local_DosDateToTmuDate`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct DosDateTime {
    time: u16,
    date: u16,
}

fn dos_from_system_time(time: std::time::SystemTime) -> DosDateTime {
    dos_from_unix_millis(
        time.duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0),
    )
}

fn dos_from_unix_millis(millis: i64) -> DosDateTime {
    let seconds = millis.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let year = year.clamp(1980, 2107);
    let date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
    let time = (((seconds_of_day / 3600) as u16) << 11)
        | ((((seconds_of_day / 60) % 60) as u16) << 5)
        | ((seconds_of_day % 60) / 2) as u16;
    DosDateTime { time, date }
}

/// The unix-millisecond value of a DOS timestamp, or `None` when there is no
/// date at all — the reference skips the `date` member for a zero FILETIME
/// (`main.cpp:126-144`). DOS timestamps are local time in the format; the
/// engine exposes no local offset, so they are read as UTC (module docs).
fn unix_millis_from_dos(date: u16, time: u16) -> Option<i64> {
    if date == 0 {
        return None;
    }
    let year = 1980 + i64::from(date >> 9);
    let month = i64::from((date >> 5) & 0xf);
    let day = i64::from(date & 0x1f);
    if month == 0 || day == 0 {
        return None;
    }
    let hour = i64::from(time >> 11);
    let minute = i64::from((time >> 5) & 0x3f);
    let second = i64::from(time & 0x1f) * 2;
    let days = days_from_civil(year, month as u32, day as u32);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000)
}

/// Days-to-civil-date conversion (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The inverse (`days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = (year - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// The whole archive image, built in memory because the engine's write seam is
/// whole-buffer (`ProjectStoragePort::write_binary_storage`) while the
/// reference streams to an IStream.
struct ZipWriter {
    image: Vec<u8>,
    records: Vec<CentralRecord>,
    /// End of the local-entry region (before any central directory this writer
    /// appended), so `finish` can rewrite the directory instead of stacking
    /// dead copies.
    data_end: usize,
    finalized: bool,
}

#[derive(Clone)]
struct CentralRecord {
    name_raw: Vec<u8>,
    flags: u16,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    local_offset: u64,
}

impl ZipWriter {
    fn new() -> Self {
        Self {
            image: Vec::new(),
            records: Vec::new(),
            data_end: 0,
            finalized: false,
        }
    }

    /// Appends to an existing archive (`Zip.open(filename, 2)`): the old bytes
    /// are kept as they are and the combined central directory is written
    /// behind them. An existing-but-empty file behaves like a fresh archive —
    /// minizip explicitly allows appending to an empty archive (`zip.c`'s
    /// `LoadCentralDirectoryRecord`, "disable to allow appending to empty ZIP
    /// archive"); a non-empty file that is not a zip fails the open.
    fn from_image(image: Vec<u8>) -> io::Result<Self> {
        if image.is_empty() {
            return Ok(Self::new());
        }
        let mut cursor = Cursor::new(image.as_slice());
        let entries = read_archive_index(&mut cursor)?;
        let records = entries.into_iter().map(CentralRecord::from_entry).collect();
        let data_end = image.len();
        Ok(Self {
            image,
            records,
            data_end,
            finalized: false,
        })
    }

    fn image(&self) -> &[u8] {
        &self.image
    }

    /// Writes one local entry (`zipWriteInFileInZip` + `zipCloseFileInZip`,
    /// `main.cpp:303-315`).
    fn add_entry(
        &mut self,
        name: &str,
        data: &[u8],
        level: i64,
        password: Option<&str>,
        dos: DosDateTime,
    ) -> io::Result<()> {
        let name_raw = name.as_bytes().to_vec();
        if name_raw.len() > u16::MAX as usize {
            return Err(invalid("entry name is too long"));
        }
        if data.len() as u64 > u64::from(u32::MAX) {
            return Err(invalid("entry is too large for this writer"));
        }

        let stored = level == 0;
        let method = if stored {
            METHOD_STORED
        } else {
            METHOD_DEFLATED
        };
        let mut flags = FLAG_UTF8;
        // `zipOpenNewFileInZip4_64` sets the compression-level hint bits from
        // the level it is handed (`zip.c:1114-1122`), and `list` reads them
        // back as `deflateLevel` (`main.cpp:430`).
        if level == 8 || level == 9 {
            flags |= 0x2;
        } else if level == 2 {
            flags |= 0x4;
        } else if level == 1 {
            flags |= 0x6;
        }
        if password.is_some() {
            flags |= FLAG_ENCRYPTED;
        }

        let crc = crc32(data);
        let mut payload = if stored {
            data.to_vec()
        } else {
            deflate(data, level)
        };
        if let Some(password) = password {
            payload = encrypt_zipcrypto(
                &payload,
                password.as_bytes(),
                (crc >> 24) as u8,
                cipher_seed(),
            );
        }
        if payload.len() as u64 > u64::from(u32::MAX) {
            return Err(invalid("entry is too large for this writer"));
        }
        if self.image.len() as u64 > u64::from(u32::MAX) {
            return Err(invalid("archive is too large for this writer"));
        }
        if self.records.len() >= u16::MAX as usize {
            return Err(invalid("too many entries for this writer"));
        }

        let local_offset = self.image.len() as u64;
        let version_needed: u16 = if stored { 10 } else { 20 };
        write_u32(&mut self.image, LOCAL_HEADER_SIGNATURE);
        write_u16(&mut self.image, version_needed);
        write_u16(&mut self.image, flags);
        write_u16(&mut self.image, method);
        write_u16(&mut self.image, dos.time);
        write_u16(&mut self.image, dos.date);
        write_u32(&mut self.image, crc);
        write_u32(&mut self.image, payload.len() as u32);
        write_u32(&mut self.image, data.len() as u32);
        write_u16(&mut self.image, name_raw.len() as u16);
        write_u16(&mut self.image, 0);
        self.image.extend_from_slice(&name_raw);
        self.image.extend_from_slice(&payload);

        self.records.push(CentralRecord {
            name_raw,
            flags,
            method,
            dos_time: dos.time,
            dos_date: dos.date,
            crc,
            compressed_size: payload.len() as u64,
            uncompressed_size: data.len() as u64,
            local_offset,
        });
        self.data_end = self.image.len();
        self.finalized = false;
        Ok(())
    }

    /// Writes the central directory and the EOCD (`zipClose`). Idempotent: a
    /// second call without a new entry does nothing.
    fn finish(&mut self) -> io::Result<()> {
        if self.finalized {
            return Ok(());
        }
        if self.records.len() > u16::MAX as usize {
            return Err(invalid("too many entries for this writer"));
        }
        self.image.truncate(self.data_end);
        let directory_offset = self.image.len() as u64;
        for record in &self.records {
            write_u32(&mut self.image, CENTRAL_HEADER_SIGNATURE);
            write_u16(&mut self.image, 20); // version made by
            write_u16(
                &mut self.image,
                if record.method == METHOD_STORED {
                    10
                } else {
                    20
                },
            );
            write_u16(&mut self.image, record.flags);
            write_u16(&mut self.image, record.method);
            write_u16(&mut self.image, record.dos_time);
            write_u16(&mut self.image, record.dos_date);
            write_u32(&mut self.image, record.crc);
            write_u32(&mut self.image, record.compressed_size as u32);
            write_u32(&mut self.image, record.uncompressed_size as u32);
            write_u16(&mut self.image, record.name_raw.len() as u16);
            write_u16(&mut self.image, 0); // extra
            write_u16(&mut self.image, 0); // comment
            write_u16(&mut self.image, 0); // disk
            write_u16(&mut self.image, 0); // internal attrs
            write_u32(&mut self.image, 0); // external attrs
            write_u32(&mut self.image, record.local_offset as u32);
            self.image.extend_from_slice(&record.name_raw);
        }
        let directory_size = self.image.len() as u64 - directory_offset;
        write_u32(&mut self.image, EOCD_SIGNATURE);
        write_u16(&mut self.image, 0);
        write_u16(&mut self.image, 0);
        write_u16(&mut self.image, self.records.len() as u16);
        write_u16(&mut self.image, self.records.len() as u16);
        write_u32(&mut self.image, directory_size as u32);
        write_u32(&mut self.image, directory_offset as u32);
        write_u16(&mut self.image, 0);
        self.finalized = true;
        Ok(())
    }
}

impl CentralRecord {
    fn from_entry(entry: Entry) -> Self {
        Self {
            name_raw: entry.name_raw,
            flags: entry.flags,
            method: entry.method,
            dos_time: entry.dos_time,
            dos_date: entry.dos_date,
            crc: entry.crc,
            compressed_size: entry.compressed_size,
            uncompressed_size: entry.uncompressed_size,
            local_offset: entry.local_header_offset,
        }
    }
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// A non-constant byte source for the ZipCrypto header, seeded from the clock
/// (the reference leaves it to `rand()`).
fn cipher_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        ^ std::process::id() as u64
}

// ---------------------------------------------------------------------------
// Deflate (RFC 1951)
// ---------------------------------------------------------------------------

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

// ---------------------------------------------------------------------------
// Inflate (decoder)
// ---------------------------------------------------------------------------

struct BitReader<'a> {
    data: &'a [u8],
    byte: usize,
    bit: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte: 0,
            bit: 0,
        }
    }

    fn bit(&mut self) -> io::Result<u32> {
        let byte = *self
            .data
            .get(self.byte)
            .ok_or_else(|| invalid("unexpected end of deflate stream"))?;
        let value = u32::from((byte >> self.bit) & 1);
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.byte += 1;
        }
        Ok(value)
    }

    fn bits(&mut self, count: u32) -> io::Result<u32> {
        let mut value = 0u32;
        for index in 0..count {
            value |= self.bit()? << index;
        }
        Ok(value)
    }

    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.byte += 1;
        }
    }

    fn u8(&mut self) -> io::Result<u8> {
        let byte = *self
            .data
            .get(self.byte)
            .ok_or_else(|| invalid("unexpected end of deflate stream"))?;
        self.byte += 1;
        Ok(byte)
    }

    fn u16_le(&mut self) -> io::Result<u16> {
        let low = u16::from(self.u8()?);
        let high = u16::from(self.u8()?);
        Ok(low | (high << 8))
    }
}

/// A canonical Huffman decoding table, built and walked like zlib's `puff.c`.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn from_lengths(lengths: &[u8]) -> io::Result<Self> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            if length > 15 {
                return Err(invalid("Huffman code length is out of range"));
            }
            counts[length as usize] += 1;
        }
        // An incomplete code is legal (a distance table may be empty); an
        // over-subscribed one is not.
        let mut left = 1i32;
        for &count in counts.iter().skip(1) {
            left <<= 1;
            left -= i32::from(count);
            if left < 0 {
                return Err(invalid("over-subscribed Huffman code"));
            }
        }
        let mut offsets = [0u16; 16];
        let mut total = 0u16;
        for length in 1..16 {
            offsets[length] = total;
            total += counts[length];
        }
        let mut symbols = vec![0u16; total as usize];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                symbols[offsets[length as usize] as usize] = symbol as u16;
                offsets[length as usize] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> io::Result<u16> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for length in 1..=15 {
            code |= reader.bit()? as i32;
            let count = i32::from(self.counts[length]);
            if code < first + count {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(invalid("invalid Huffman code"))
    }
}

fn fixed_tables() -> (Huffman, Huffman) {
    let mut literal_lengths = [0u8; 288];
    for (symbol, length) in literal_lengths.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let distance_lengths = [5u8; 32];
    (
        Huffman::from_lengths(&literal_lengths).expect("fixed literal table"),
        Huffman::from_lengths(&distance_lengths).expect("fixed distance table"),
    )
}

fn dynamic_tables(reader: &mut BitReader<'_>) -> io::Result<(Huffman, Huffman)> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let literal_count = reader.bits(5)? as usize + 257;
    let distance_count = reader.bits(5)? as usize + 1;
    let code_length_count = reader.bits(4)? as usize + 4;
    let mut code_lengths = [0u8; 19];
    for &symbol in ORDER.iter().take(code_length_count) {
        code_lengths[symbol] = reader.bits(3)? as u8;
    }
    let code_length_table = Huffman::from_lengths(&code_lengths)?;

    let mut lengths = vec![0u8; literal_count + distance_count];
    let mut index = 0usize;
    while index < lengths.len() {
        let symbol = code_length_table.decode(reader)?;
        match symbol {
            0..=15 => {
                lengths[index] = symbol as u8;
                index += 1;
            }
            16 => {
                let previous = *lengths
                    .get(index.wrapping_sub(1))
                    .filter(|_| index > 0)
                    .ok_or_else(|| invalid("code length repeat without a previous length"))?;
                let repeat = 3 + reader.bits(2)? as usize;
                if index + repeat > lengths.len() {
                    return Err(invalid("code length repeat overflows the table"));
                }
                lengths[index..index + repeat].fill(previous);
                index += repeat;
            }
            17 => {
                let repeat = 3 + reader.bits(3)? as usize;
                if index + repeat > lengths.len() {
                    return Err(invalid("code length repeat overflows the table"));
                }
                index += repeat;
            }
            18 => {
                let repeat = 11 + reader.bits(7)? as usize;
                if index + repeat > lengths.len() {
                    return Err(invalid("code length repeat overflows the table"));
                }
                index += repeat;
            }
            _ => return Err(invalid("invalid code length symbol")),
        }
    }
    let literal = Huffman::from_lengths(&lengths[..literal_count])?;
    let distance = Huffman::from_lengths(&lengths[literal_count..])?;
    Ok((literal, distance))
}

fn compressed_block(
    reader: &mut BitReader<'_>,
    output: &mut Vec<u8>,
    literal_table: &Huffman,
    distance_table: &Huffman,
    limit: usize,
) -> io::Result<()> {
    loop {
        let symbol = literal_table.decode(reader)?;
        match symbol {
            0..=255 => output.push(symbol as u8),
            256 => return Ok(()),
            257..=285 => {
                let index = usize::from(symbol - 257);
                let length = usize::from(LENGTH_BASE[index])
                    + reader.bits(u32::from(LENGTH_EXTRA[index]))? as usize;
                let distance_symbol = distance_table.decode(reader)?;
                if distance_symbol > 29 {
                    return Err(invalid("invalid deflate distance code"));
                }
                let distance = usize::from(DISTANCE_BASE[distance_symbol as usize])
                    + reader.bits(u32::from(DISTANCE_EXTRA[distance_symbol as usize]))? as usize;
                if distance == 0 || distance > output.len() {
                    return Err(invalid("deflate back-reference out of range"));
                }
                let start = output.len() - distance;
                for step in 0..length {
                    let byte = output[start + step];
                    output.push(byte);
                }
            }
            _ => return Err(invalid("invalid deflate literal/length code")),
        }
        // The declared size is the cap, checked per symbol: a crafted stream
        // must not be able to grow the buffer past it before the block ends.
        if output.len() > limit {
            return Err(invalid("deflate stream expands past the declared size"));
        }
    }
}

fn stored_block(reader: &mut BitReader<'_>, output: &mut Vec<u8>, limit: usize) -> io::Result<()> {
    reader.align();
    let length = reader.u16_le()?;
    let complement = reader.u16_le()?;
    if length != !complement {
        return Err(invalid("stored block length mismatch"));
    }
    if output.len() + usize::from(length) > limit {
        return Err(invalid("deflate stream expands past the declared size"));
    }
    for _ in 0..length {
        output.push(reader.u8()?);
    }
    Ok(())
}

/// Decodes a raw DEFLATE stream (`-MAX_WBITS`, the window the reference hands
/// `zipWriteInFileInZip`, `main.cpp:307`) that must expand to `expected`
/// bytes.
fn inflate(input: &[u8], expected: usize) -> io::Result<Vec<u8>> {
    let mut reader = BitReader::new(input);
    let mut output = Vec::with_capacity(expected.min(1 << 16));
    loop {
        let final_block = reader.bit()? != 0;
        match reader.bits(2)? {
            0 => stored_block(&mut reader, &mut output, expected)?,
            1 => {
                let (literal, distance) = fixed_tables();
                compressed_block(&mut reader, &mut output, &literal, &distance, expected)?;
            }
            2 => {
                let (literal, distance) = dynamic_tables(&mut reader)?;
                compressed_block(&mut reader, &mut output, &literal, &distance, expected)?;
            }
            _ => return Err(invalid("invalid deflate block type")),
        }
        if output.len() > expected {
            return Err(invalid("deflate stream expands past the declared size"));
        }
        if final_block {
            break;
        }
    }
    Ok(output)
}

// ---------------------------------------------------------------------------
// Deflate (encoder): fixed-Huffman LZ77
// ---------------------------------------------------------------------------

struct BitWriter {
    out: Vec<u8>,
    accumulator: u32,
    bits: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            accumulator: 0,
            bits: 0,
        }
    }

    /// Writes one bit (and the low bits of `value`) LSB-first, the bit order
    /// RFC 1951 fixes for the stream.
    fn write_bit(&mut self, bit: u32) {
        self.accumulator |= (bit & 1) << self.bits;
        self.bits += 1;
        if self.bits == 8 {
            self.out.push(self.accumulator as u8);
            self.accumulator = 0;
            self.bits = 0;
        }
    }

    fn write_bits(&mut self, value: u32, count: u32) {
        for index in 0..count {
            self.write_bit((value >> index) & 1);
        }
    }

    /// Writes a Huffman code MSB-first, the order the specification uses for
    /// code bits.
    fn write_code(&mut self, code: u32, count: u32) {
        for index in (0..count).rev() {
            self.write_bit((code >> index) & 1);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push(self.accumulator as u8);
        }
        self.out
    }
}

fn write_fixed_symbol(writer: &mut BitWriter, symbol: u16) {
    match symbol {
        0..=143 => writer.write_code(0x30 + u32::from(symbol), 8),
        144..=255 => writer.write_code(0x190 + u32::from(symbol - 144), 9),
        256..=279 => writer.write_code(u32::from(symbol - 256), 7),
        _ => writer.write_code(0xC0 + u32::from(symbol - 280), 8),
    }
}

fn write_match(writer: &mut BitWriter, length: usize, distance: usize) {
    // Longest-match table lookup: the largest base not greater than the value.
    let length_index = LENGTH_BASE
        .iter()
        .rposition(|&base| usize::from(base) <= length)
        .expect("length has a code");
    write_fixed_symbol(writer, 257 + length_index as u16);
    writer.write_bits(
        (length - usize::from(LENGTH_BASE[length_index])) as u32,
        u32::from(LENGTH_EXTRA[length_index]),
    );
    let distance_index = DISTANCE_BASE
        .iter()
        .rposition(|&base| usize::from(base) <= distance)
        .expect("distance has a code");
    // Fixed distance codes are their symbol as a 5-bit code.
    writer.write_code(distance_index as u32, 5);
    writer.write_bits(
        (distance - usize::from(DISTANCE_BASE[distance_index])) as u32,
        u32::from(DISTANCE_EXTRA[distance_index]),
    );
}

/// Level-derived effort, mirroring zlib's configuration table's shape.
fn deflate_effort(level: i64) -> (usize, usize) {
    match level {
        1 => (4, 8),
        2 => (8, 16),
        3 => (16, 32),
        4 => (32, 64),
        5 => (64, 128),
        6 => (128, 258),
        7 => (256, 258),
        8 => (512, 258),
        _ => (1024, 258),
    }
}

fn deflate_level(level: i64) -> i64 {
    if (0..=9).contains(&level) { level } else { 6 }
}

const DEFLATE_WINDOW: usize = 32_768;
const DEFLATE_MIN_MATCH: usize = 3;
const DEFLATE_MAX_MATCH: usize = 258;
const DEFLATE_HASH_BITS: usize = 15;
const DEFLATE_HASH_SIZE: usize = 1 << DEFLATE_HASH_BITS;
const DEFLATE_NIL: u32 = u32::MAX;

fn deflate_hash(data: &[u8], position: usize) -> usize {
    let a = u32::from(data[position]);
    let b = u32::from(data[position + 1]);
    let c = u32::from(data[position + 2]);
    (((a << 10) ^ (b << 5) ^ c) as usize) & (DEFLATE_HASH_SIZE - 1)
}

/// A fixed-Huffman LZ77 encoder. The output is valid DEFLATE (decodable by
/// zlib, minizip, Python's `zipfile`, every ZIP reader); the bytes are simply
/// not zlib's, which the module docs record.
fn deflate(data: &[u8], level: i64) -> Vec<u8> {
    let (max_chain, good_match) = deflate_effort(deflate_level(level));
    let mut writer = BitWriter::new();
    writer.write_bit(1); // final block
    writer.write_bits(1, 2); // fixed Huffman tables

    if data.len() >= DEFLATE_MIN_MATCH {
        let mut head = vec![DEFLATE_NIL; DEFLATE_HASH_SIZE];
        let mut prev = vec![DEFLATE_NIL; DEFLATE_WINDOW];
        let mut position = 0usize;
        while position < data.len() {
            let mut best_length = 0usize;
            let mut best_distance = 0usize;
            if position + DEFLATE_MIN_MATCH <= data.len() {
                let hash = deflate_hash(data, position);
                let limit = position.saturating_sub(DEFLATE_WINDOW - 1);
                let mut candidate = head[hash];
                let mut chain = 0usize;
                while candidate != DEFLATE_NIL && (candidate as usize) >= limit && chain < max_chain
                {
                    let start = candidate as usize;
                    let max_length = (data.len() - position).min(DEFLATE_MAX_MATCH);
                    let mut length = 0usize;
                    while length < max_length && data[start + length] == data[position + length] {
                        length += 1;
                    }
                    if length > best_length {
                        best_length = length;
                        best_distance = position - start;
                        if length >= good_match {
                            break;
                        }
                    }
                    candidate = prev[start % DEFLATE_WINDOW];
                    chain += 1;
                }
                // Insert this position before it advances.
                prev[position % DEFLATE_WINDOW] = head[hash];
                head[hash] = position as u32;
            }

            if best_length >= DEFLATE_MIN_MATCH {
                write_match(&mut writer, best_length, best_distance);
                // Index the positions the match covered, so later matches can
                // reach into it (zlib inserts every position too).
                for index in position + 1..position + best_length {
                    if index + DEFLATE_MIN_MATCH <= data.len() {
                        let hash = deflate_hash(data, index);
                        prev[index % DEFLATE_WINDOW] = head[hash];
                        head[hash] = index as u32;
                    }
                }
                position += best_length;
            } else {
                write_fixed_symbol(&mut writer, u16::from(data[position]));
                position += 1;
            }
        }
    } else {
        for &byte in data {
            write_fixed_symbol(&mut writer, u16::from(byte));
        }
    }
    write_fixed_symbol(&mut writer, 256); // end of block
    writer.finish()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::SystemTime,
    };

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths};
    use krkr_tjs2::TjsErrorKind;

    use super::*;

    /// Decodes the hex fixtures below.
    fn hex(value: &str) -> Vec<u8> {
        assert_eq!(value.len() % 2, 0, "hex fixture has an even length");
        (0..value.len())
            .step_by(2)
            .map(|index| {
                u8::from_str_radix(&value[index..index + 2], 16).expect("hex fixture byte")
            })
            .collect()
    }

    /// A zip built by Python's `zipfile` (the generator is recorded in the
    /// module's development notes): one deflated text entry, one stored binary
    /// entry, one UTF-8-named deflated entry and one larger deflated entry.
    const FIXTURE_ZIP_HEX: &str = "504b030414000000080083182258c2b284bf2c000000750000000a000000726561646d652e747874f348cdc9c957482bcacf55485428a82cc9c8cfd34d2acdcc2951a8ca2c5048cbac28292d4ad5e3f2a0aa3200504b0304140000000000831822588cce0e1040000000400000000f000000646174612f73746f7265642e62696e000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f504b03041400000808008318225834357cfd150000001300000009000000636166c3a92e7478742bcdcb4cce4f4955c84bcc4d5548cd2b29aae40200504b0304140000000800831822582f7b5ccd410000000c0700000c000000646174612f6269672e7478742bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb8179a38a47158f2a1e553caa7854f170529c98990300504b0102140314000000080083182258c2b284bf2c000000750000000a0000000000000000000000800100000000726561646d652e747874504b01021403140000000000831822588cce0e1040000000400000000f0000000000000000000000800154000000646174612f73746f7265642e62696e504b010214031400000808008318225834357cfd15000000130000000900000000000000000000008001c1000000636166c3a92e747874504b01021403140000000800831822582f7b5ccd410000000c0700000c00000000000000000000008001fd000000646174612f6269672e747874504b05060000000004000400e6000000680100000000";

    /// A python-built ZipCrypto archive (password `secret`), constructed from
    /// the documented PKWARE algorithm — an independent implementation of the
    /// encryption this port has to decrypt.
    const FIXTURE_ENCRYPTED_ZIP_HEX: &str = "504b030414000108080006042258b25188503d0000005a0000000a0000007365637265742e747874d957341e5af57cffeecf6d951fcb8edc9b6ca169db673329606aa175044b49b2c41493885be2ca7af1ce103093fed6c5c1183a7cd4be2d7478073ad51d504b0102140014000108080006042258b25188503d0000005a0000000a00000000000000000000000000000000007365637265742e747874504b0506000000000100010038000000650000000000";

    /// A python-built archive whose entry carries general-purpose flag bit 3
    /// and a data descriptor; the sizes in the local header are zero and the
    /// central directory holds the truth.
    const FIXTURE_DESCRIPTOR_ZIP_HEX: &str = "504b0304140008080800060422580000000000000000000000000c00000073747265616d65642e7478744b492d4e2eca2c28c92f5248cd2b29aa542848acccc94f4cd151282fca2c2949cd53282e294a4dcccdcc4bd72d2ea9cc49d5e30200504b07087cf6c06d3500000033000000504b01021400140008080800060422587cf6c06d35000000330000000c000000000000000000000000000000000073747265616d65642e747874504b050600000000010001003a0000006f0000000000";

    /// The name both entries of [`FIXTURE_CP932_ZIP_HEX`] carry, and its CP932
    /// bytes (`正` `90 b3`, `し` `82 b5`, `い` `82 a2`, `名` `96 bc`, `前` `91 4f`).
    const CP932_NAME: &str = "正しい名前.txt";
    const CP932_NAME_BYTES: [u8; 14] = [
        0x90, 0xb3, 0x82, 0xb5, 0x82, 0xa2, 0x96, 0xbc, 0x91, 0x4f, 0x2e, 0x74, 0x78, 0x74,
    ];

    /// A hand-built archive: two stored entries, no extra fields, no data
    /// descriptors. Entry 0 is named `正しい名前.txt` in CP932 bytes with
    /// general-purpose flag bit 11 **clear** — the shape a legacy Japanese
    /// writer produces — and holds `cp932 named entry\n`; entry 1 is named
    /// `日本語.txt` in UTF-8 bytes with bit 11 set and holds
    /// `utf8 named entry\n`. The name bytes and payloads are quoted here, so
    /// the image regenerates from them alone. One archive therefore pins both
    /// decode rules at once.
    const FIXTURE_CP932_ZIP_HEX: &str = "504b030414000000000022581883fc826e2c12000000120000000e00000090b382b582a296bc914f2e7478746370393332206e616d656420656e7472790a504b0304140000080000225818833371643311000000110000000d000000e697a5e69cace8aa9e2e74787475746638206e616d656420656e7472790a504b0102140014000000000022581883fc826e2c12000000120000000e000000000000000000000000000000000090b382b582a296bc914f2e747874504b01021400140000080000225818833371643311000000110000000d000000000000000000000000003e000000e697a5e69cace8aa9e2e747874504b05060000000002000200770000007a0000000000";

    /// zlib's raw-DEFLATE encoding of `deflate_test_text()` at level 6 with
    /// `Z_FIXED` (one fixed-Huffman block).
    const DEFLATE_FIXED_HEX: &str = "cbce2c4acc066285aacc0285b4cc8a92d2a2548594cabcc4dccc64858cd2b4b4dcc43c85a49cfce46c85e4fcb2d4a2c4f4543d0503432363135333730b4b85c4a4e494d4b4f48cccacec9cdcbcfc82c2a2e292d2b2f28aca2a05472767175737770f4f2f6f1f5f3fff80c0a0e090d0b0f088c8286b85a2d482d4c41285928c548592c4cc1c85e27c85dcc492e48cd46285c402a054919e42f6a8cb465d36eab251978dba6cd465a32e1b75d9a8cb465d36eab251978dba6cd465a32e1b75d9a8cb465d36eab251978dba6cd465a32ea392cb00";

    /// zlib's raw-DEFLATE encoding of `deflate_test_text()` at level 6 with the
    /// default strategy (one dynamic-Huffman block).
    const DEFLATE_DYNAMIC_HEX: &str = "edce3712c2301445d1adfc1530e43054e49c33ddb72cdb424ec83269f5b008ca57dcea36472bc3fa177d544a9e7ad9dc4872df31474a50907b5ec4313961223489e4210dfbb240c552b952add51bcd16b1235ce9f981bae9308a93f46e329b3f9eaff7873add5e7f301c8d27d3d97cb15cad37dbddfe703c9d2fd73619994ab66403499655485942115b11c88c38fd2d53200d19649041061964904106196490410619649041061964904106196490fd49f605";

    /// A 1500-byte deterministic binary payload and zlib's level-6 raw
    /// encoding of it (a dynamic block dominated by distance codes).
    const BINARY_DEFLATE_HEX: &str = "2d94073b9001184555a8345054282a14152a0d14158a0a45854a034585a24251a1d24051a1a85054a8345054282a14152a0d94068a0a4585c6eb7bef3f38cf3dcfb922aa73368467567553b7d8742ceb5df73156de27eed4f41abbc837eede67c9098b7724147eedafb36ce7b9e2e68153edf79c7fd2aa307d75d0a5a7bf958cd71e4c7df167b8a9dbe1eb155d46ccf588bcf9566cd4bccdd1391f7a6a2ed87232afaecf789b6da7ee37484f5aea7fe6c13759bd15bb931e7d1f64e0b8ef62e9cfc1339c43ae3c6f1f3a735d58faab7f2ab3d71f210435f38d476f1182a5d7f1db1f09c127f6eea7be84105ff0a5df6442286a1a30652521b4c84f5bb59f10148dd61cb84a0826ae87aebd2604f7881b6f4409212afb7d0f0d42c8aded3dce9a10eaa5262ef1230419dde5bb120941df61ef851242700abefcac8d1042d35efe5526848ccaae23cd08a15a7cf47c4f4290d05ab8358610b46db79fce2704bb80b30f1b092130f9f10f39424829fb35c49010ca3b86cd72210491ce290941989210842909419892108429094198921084290941989210842909419892108429094198921084290941989210842909419892108429d9a69a39dbb4f4629b3eb16c33be806d1635b1cd1679b6a968c4364d5cd9a67b04db8cca669bb9b56cb35e8a6dcae8b24d7d07b6e914cc3643d3d8664625dbac16679b125a6c53db966dda05b0cdc064b69952c636cb3bd8a6882adb54b7609b56de6cd3378e6d2614b2cde266b6d9aac036958cd9a6a91bdbf488649bd1396c33af8e6d3648b34d593db669e0c8369d43d866583adbccac1245181a08c31a61f8218c4484518230da108632c23043189e08230661e4238c46842187300c11860bc20847185908a3066148220c1d84618f308210462ac2a840186208431361d8200c7f849184304a11463bc2504118e608c30b61c4228c0284d18430e4118611c27045181108231b61d4220c2984a18b301c104630c24843189508431c6168210c5b8411803092114619c2e84018b83935dc9c256ece0737178f9b2bc2cdb5e0e614717326b83977dc5c146e2e1737578f9b93c1cde9e3e69c7073a1b8b90cdc5c356e4e0237a78d9bb3c3cd05e2e6527073e5b83911dc9c3a6ece0a37e78b9b4bc0cd1577dafc0f";

    fn deflate_test_text() -> Vec<u8> {
        "kirakira zip fixture dynamic huffman block coverage. 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ; repeat the tail so matches appear. "
            .repeat(40)
            .into_bytes()
    }

    fn binary_test_payload() -> Vec<u8> {
        (0..1500u32)
            .map(|index| ((index * 37 + (index >> 3)) & 0xff) as u8)
            .collect()
    }

    /// A stored (BTYPE=00) DEFLATE block, the third block shape the decoder
    /// has to accept.
    fn stored_stream(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x01];
        let length = data.len() as u16;
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    // -----------------------------------------------------------------
    // Core unit tests
    // -----------------------------------------------------------------

    #[test]
    fn crc32_matches_published_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"kirakira"), 0x98A8_5292);
    }

    /// The three DEFLATE block shapes all decode to the same text: a stored
    /// block built here, zlib's fixed-Huffman output and zlib's
    /// dynamic-Huffman output.
    #[test]
    fn inflate_decodes_stored_fixed_and_dynamic_blocks() {
        let text = deflate_test_text();
        for (label, stream) in [
            ("stored", stored_stream(&text)),
            ("fixed", hex(DEFLATE_FIXED_HEX)),
            ("dynamic", hex(DEFLATE_DYNAMIC_HEX)),
        ] {
            let decoded = inflate(&stream, text.len())
                .unwrap_or_else(|error| panic!("{label} stream: {error}"));
            assert_eq!(decoded, text, "{label} stream decoded differently");
        }
        let binary = binary_test_payload();
        assert_eq!(
            inflate(&hex(BINARY_DEFLATE_HEX), binary.len()).expect("binary stream"),
            binary
        );
    }

    #[test]
    fn inflate_rejects_malformed_streams() {
        assert!(inflate(&[], 0).is_err());
        // A stored block whose LEN/NLEN pair does not complement.
        assert!(inflate(&[0x01, 0x05, 0x00, 0x00, 0x00, b'a', b'b'], 2).is_err());
        // A stream that stops mid-literal.
        assert!(inflate(&hex(DEFLATE_DYNAMIC_HEX)[..3], 4096).is_err());
    }

    /// The deflate encoder round-trips through the decoder for the payload
    /// shapes a game can hand `Zip.add`: empty, tiny, repetitive and
    /// incompressible.
    #[test]
    fn deflate_round_trips_through_inflate() {
        let payloads: [Vec<u8>; 6] = [
            Vec::new(),
            vec![0xAB],
            b"ab".to_vec(),
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec(),
            deflate_test_text(),
            (0..4096u32)
                .map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8)
                .collect(),
        ];
        for level in [-1, 0, 1, 2, 6, 9] {
            for payload in &payloads {
                if level == 0 {
                    continue; // level 0 is method STORED; the writer handles it
                }
                let compressed = deflate(payload, level);
                assert_eq!(
                    inflate(&compressed, payload.len()).expect("deflate round trip"),
                    *payload,
                    "level {level}, {} bytes",
                    payload.len()
                );
            }
        }
        // Repetitive input must actually compress, or the encoder is not
        // emitting matches.
        let text = deflate_test_text();
        assert!(deflate(&text, 6).len() < text.len() / 4);
    }

    #[test]
    fn dos_datetime_round_trips_through_unix_millis() {
        for (year, month, day, hour, minute, second) in [
            (1980, 1, 1, 0, 0, 0),
            (2024, 1, 2, 3, 4, 6),
            (2026, 9, 13, 23, 59, 58),
        ] {
            let millis = days_from_civil(year, month, day) * 86_400 * 1000
                + hour * 3_600_000
                + minute * 60_000
                + second * 1000;
            let dos = dos_from_unix_millis(millis);
            assert_eq!(
                unix_millis_from_dos(dos.date, dos.time),
                Some(millis),
                "{year}-{month}-{day} {hour}:{minute}:{second}"
            );
        }
        assert_eq!(unix_millis_from_dos(0, 0), None);
    }

    /// `storeFilename` decides a name's encoding from general-purpose flag bit
    /// 11 alone (`main.cpp:335-350`): set means UTF-8, clear means the host
    /// ANSI code page — the `ttstr = const char*` assignment reaches
    /// `SetString(const tjs_nchar*)` → `TJS_mbstowcs` (`tjsVariantString.h:108-122`,
    /// `tjsConfig.h:99-100`), which on Windows is
    /// `MultiByteToWideChar(CP_ACP, MB_PRECOMPOSED|MB_ERR_INVALID_CHARS, …)`
    /// (`tjsConfig.cpp:208-243`). CP932 is that code page on the Japanese
    /// Windows hosts the reference ships to, and the engine already reads
    /// project text through the same table (`krkr-assets/src/storage.rs:2600`).
    /// CP437 is not part of that rule; it survives here only for bytes the ANSI
    /// code page cannot represent, where the reference throws instead
    /// (`TJSThrowNarrowToWideConversionError`, `tjsVariantString.cpp:34-37`).
    #[test]
    fn decode_name_follows_the_reference_ansi_rule() {
        assert_eq!(decode_name(b"plain.txt", false), "plain.txt");
        // Flag bit 11 set: UTF-8 bytes, unchanged.
        assert_eq!(decode_name("café.txt".as_bytes(), true), "café.txt");
        assert_eq!(decode_name(CP932_NAME.as_bytes(), true), CP932_NAME);
        // Flag bit 11 clear: the bytes are the ANSI code page.
        assert_eq!(decode_name(&CP932_NAME_BYTES, false), CP932_NAME);
        // 0x82 0xA0 is "あ" in CP932; CP437 would render "éá".
        assert_eq!(decode_name(&[0x82, 0xA0], false), "あ");
        // The reference throws on bytes the ANSI code page cannot represent;
        // this port keeps its previous fallback instead of failing the whole
        // archive, so a UTF-8-encoded Japanese name without the flag still
        // decodes (`日本語` is not a valid CP932 byte sequence).
        assert_eq!(decode_name("日本語.txt".as_bytes(), false), "日本語.txt");
        // Neither encoding fits 0x82 followed by a space: CP437's "é " it is.
        assert_eq!(decode_name(&[0x82, 0x20], false), "é ");
    }

    /// A bit-11-clear name is CP932, so the script string the game would ask
    /// for resolves and the CP437 mojibake the same bytes used to decode to no
    /// longer answers. The bit-11 entry in the same archive keeps resolving as
    /// UTF-8.
    #[test]
    fn flagless_cp932_names_resolve_where_their_mojibake_does_not() {
        let entries = read_archive_index(&mut Cursor::new(hex(FIXTURE_CP932_ZIP_HEX)))
            .expect("fixture index");
        assert_eq!(entries[0].flags & FLAG_UTF8, 0);
        assert_eq!(entries[1].flags & FLAG_UTF8, FLAG_UTF8);
        assert_eq!(entries[0].name_raw, CP932_NAME_BYTES);
        assert_eq!(entries[1].name_raw, "日本語.txt".as_bytes());
        assert_eq!(entries[0].name, CP932_NAME);
        assert_eq!(entries[1].name, "日本語.txt");
        assert_eq!(
            locate_entry(&entries, CP932_NAME).map(|entry| entry.name.as_str()),
            Some(CP932_NAME)
        );
        assert_eq!(
            locate_entry(&entries, "日本語.txt").map(|entry| entry.name.as_str()),
            Some("日本語.txt")
        );
        // What CP437 made of the flag-less bytes — the spelling the old decode
        // reported, and the one a lookup must now miss.
        assert_eq!(cp437(&CP932_NAME_BYTES), "É│é╡éóû╝æO.txt");
        assert_ne!(decode_name(&CP932_NAME_BYTES, false), "É│é╡éóû╝æO.txt");
        assert!(locate_entry(&entries, "É│é╡éóû╝æO.txt").is_none());
    }

    /// The listing table has the reference's shape: files land under
    /// `"/" + dir/`, and a directory entry contributes an empty name to its
    /// own list (`storage.cpp:198-210`).
    #[test]
    fn directory_table_follows_the_reference_shape() {
        let entries =
            read_archive_index(&mut Cursor::new(hex(FIXTURE_ZIP_HEX))).expect("fixture index");
        let table = directory_table(&entries);
        assert_eq!(
            table.get("/").cloned().unwrap_or_default(),
            vec!["readme.txt".to_string(), "café.txt".to_string()]
        );
        assert_eq!(
            table.get("/data/").cloned().unwrap_or_default(),
            vec!["stored.bin".to_string(), "big.txt".to_string()]
        );
        // An explicit directory entry is filed under its own path with an
        // empty name, exactly like the C++ `entryName`.
        let mut with_directory = entries.clone();
        with_directory.push(Entry {
            name_raw: b"data/".to_vec(),
            name: "data/".to_string(),
            flags: FLAG_UTF8,
            method: METHOD_STORED,
            dos_time: 0,
            dos_date: 0,
            crc: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            local_header_offset: 0,
        });
        let table = directory_table(&with_directory);
        assert_eq!(
            table.get("/data/").cloned().unwrap_or_default(),
            vec![
                "stored.bin".to_string(),
                "big.txt".to_string(),
                String::new()
            ]
        );
    }

    #[test]
    fn entry_lookup_is_ascii_case_insensitive() {
        let entries =
            read_archive_index(&mut Cursor::new(hex(FIXTURE_ZIP_HEX))).expect("fixture index");
        assert_eq!(
            locate_entry(&entries, "README.TXT").map(|entry| entry.name.as_str()),
            Some("readme.txt")
        );
        assert_eq!(
            locate_entry(&entries, "café.TXT").map(|entry| entry.name.as_str()),
            Some("café.txt")
        );
        assert!(locate_entry(&entries, "missing.txt").is_none());
    }

    /// ZipCrypto round-trips through this port's own cipher, and the check
    /// byte rejects a wrong password.
    #[test]
    fn zipcrypto_round_trips_and_rejects_a_wrong_password() {
        let plain = b"payload for the zipcrypto unit test";
        let encrypted = encrypt_zipcrypto(plain, b"hunter2", 0x42, 7);
        let entry = Entry {
            name_raw: b"x".to_vec(),
            name: "x".to_string(),
            flags: FLAG_ENCRYPTED | FLAG_UTF8,
            method: METHOD_STORED,
            dos_time: 0,
            dos_date: 0,
            crc: 0x42 << 24,
            compressed_size: encrypted.len() as u64,
            uncompressed_size: plain.len() as u64,
            local_header_offset: 0,
        };
        assert_eq!(
            decrypt_zipcrypto(encrypted.clone(), b"hunter2", &entry).expect("decrypt"),
            plain
        );
        assert!(decrypt_zipcrypto(encrypted, b"wrong", &entry).is_err());
    }

    /// The writer's output parses back through the reader with the same
    /// entries, contents, methods and flags — including the appended mode,
    /// where the old entries survive.
    #[test]
    fn writer_output_round_trips_through_the_reader() {
        let mut writer = ZipWriter::new();
        writer
            .add_entry(
                "a.txt",
                b"deflated entry",
                Z_DEFAULT_COMPRESSION,
                None,
                DosDateTime {
                    time: 0x0406,
                    date: ((2024 - 1980) << 9) | (1 << 5) | 2,
                },
            )
            .expect("add deflated");
        writer
            .add_entry(
                "dir/stored.bin",
                &binary_test_payload(),
                0,
                None,
                DosDateTime { time: 0, date: 0 },
            )
            .expect("add stored");
        writer
            .add_entry(
                "secret.bin",
                b"encrypted entry",
                6,
                Some("pw"),
                DosDateTime {
                    time: 0x0406,
                    date: ((2024 - 1980) << 9) | (1 << 5) | 2,
                },
            )
            .expect("add encrypted");
        writer.finish().expect("finish");
        let image = writer.image().to_vec();

        let archive = ReadArchive::new(Box::new(Cursor::new(image.clone()))).expect("index");
        assert_eq!(archive.entries.len(), 3);
        let listed: Vec<(String, bool, bool)> = archive
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.name.clone(),
                    entry.is_deflated(),
                    entry.is_encrypted(),
                )
            })
            .collect();
        assert_eq!(
            listed,
            vec![
                ("a.txt".to_string(), true, false),
                ("dir/stored.bin".to_string(), false, false),
                ("secret.bin".to_string(), true, true),
            ]
        );
        let deflated = locate_entry(&archive.entries, "a.txt").expect("a.txt");
        assert_eq!(
            archive.read_entry(deflated, None).expect("read a.txt"),
            b"deflated entry"
        );
        let stored = locate_entry(&archive.entries, "dir/stored.bin").expect("stored");
        assert_eq!(
            archive.read_entry(stored, None).expect("read stored"),
            binary_test_payload()
        );
        let secret = locate_entry(&archive.entries, "secret.bin").expect("secret");
        assert!(archive.read_entry(secret, None).is_err());
        assert_eq!(
            archive
                .read_entry(secret, Some("pw"))
                .expect("decrypt secret"),
            b"encrypted entry"
        );

        // Appending keeps the old entries and adds the new one.
        let mut appended = ZipWriter::from_image(image).expect("append");
        appended
            .add_entry(
                "later.txt",
                b"added later",
                Z_DEFAULT_COMPRESSION,
                None,
                DosDateTime { time: 0, date: 0 },
            )
            .expect("append entry");
        appended.finish().expect("finish append");
        let archive =
            ReadArchive::new(Box::new(Cursor::new(appended.image().to_vec()))).expect("index");
        assert_eq!(
            archive
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a.txt", "dir/stored.bin", "secret.bin", "later.txt"]
        );
        assert_eq!(
            archive
                .read_entry(
                    locate_entry(&archive.entries, "later.txt").expect("later"),
                    None
                )
                .expect("read later"),
            b"added later"
        );
    }

    /// The level hint bits `zipOpenNewFileInZip4_64` sets (`zip.c:1114-1122`)
    /// come back out of `list` as `deflateLevel`.
    #[test]
    fn writer_records_the_reference_level_hint_bits() {
        for (level, expected) in [(-1, 0), (1, 3), (2, 2), (6, 0), (9, 1)] {
            let mut writer = ZipWriter::new();
            writer
                .add_entry(
                    "x.bin",
                    b"data for the level hint",
                    level,
                    None,
                    DosDateTime { time: 0, date: 0 },
                )
                .expect("add");
            writer.finish().expect("finish");
            let archive =
                ReadArchive::new(Box::new(Cursor::new(writer.image().to_vec()))).expect("index");
            assert_eq!(
                archive.entries[0].deflate_level_hint(),
                expected,
                "level {level}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Engine-level tests
    // -----------------------------------------------------------------

    fn temp_root(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-minizip-{prefix}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    fn plugin_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("project storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine");
        engine
            .register_plugin(MinizipPlugin::new())
            .expect("register the minizip plugin");
        engine
    }

    fn write_file(root: &Path, name: &str, bytes: &[u8]) {
        if let Some(parent) = root.join(name).parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(root.join(name), bytes).expect("write fixture");
    }

    fn storage_read(engine: &KrkrEngine, name: &str) -> Vec<u8> {
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");
        storage
            .read_binary_storage(name)
            .unwrap_or_else(|error| panic!("read `{name}`: {error}"))
            .as_bytes()
            .expect("bytes")
            .into_owned()
    }

    fn evaluate(engine: &mut KrkrEngine, script: &str) -> Result<Variant> {
        engine.execute_expression("inline.tjs", script)
    }

    /// The python-built fixture opens through `Unzip`, lists the reference's
    /// dictionary members and extracts byte-identical contents.
    #[test]
    fn a_python_built_zip_lists_and_extracts() {
        let root = temp_root("fixture");
        write_file(&root, "fixture.zip", &hex(FIXTURE_ZIP_HEX));
        let mut engine = plugin_engine(&root);

        let listed = evaluate(
            &mut engine,
            "(function() {\n\
                 var z = new Unzip();\n\
                 z.open(\"fixture.zip\");\n\
                 var l = z.list();\n\
                 var first = l[0];\n\
                 var out = first.filename + \":\" + first.uncompressed_size + \":\" + \n\
                     first.compressed_size + \":\" + first.crypted + \":\" + first.deflated + \":\" + \n\
                     first.deflateLevel + \":\" + (typeof first.date);\n\
                 out += \"|\" + l[1].filename + \":\" + l[1].deflated;\n\
                 out += \"|\" + l[2].filename;\n\
                 out += \"|\" + l.length;\n\
                 z.close();\n\
                 return out;\n\
             })()",
        )
        .expect("list the fixture");
        assert_eq!(
            listed,
            Variant::String(
                "readme.txt:117:44:0:1:0:Object|data/stored.bin:0|café.txt|4".to_string()
            )
        );

        let extracted = evaluate(
            &mut engine,
            "(function() {\n\
                 var z = new Unzip();\n\
                 z.open(\"fixture.zip\");\n\
                 var ok = z.extract(\"data/big.txt\", \"out/big.txt\");\n\
                 z.close();\n\
                 return ok;\n\
             })()",
        )
        .expect("extract the fixture");
        assert_eq!(extracted, Variant::Integer(1));
        assert_eq!(
            storage_read(&engine, "out/big.txt"),
            ("the quick brown fox jumps over the lazy dog. ".repeat(40) + "tail").into_bytes()
        );
    }

    /// `Unzip.extract` with the right password returns the plaintext and a
    /// wrong password fails the call without writing anything.
    #[test]
    fn extract_of_the_encrypted_fixture_checks_the_password() {
        let root = temp_root("encrypted");
        write_file(&root, "secret.zip", &hex(FIXTURE_ENCRYPTED_ZIP_HEX));
        let mut engine = plugin_engine(&root);

        let wrong = evaluate(
            &mut engine,
            "(function() { var z = new Unzip(); z.open(\"secret.zip\"); \n\
                 var ok = z.extract(\"secret.txt\", \"wrong.txt\", \"nope\");\n\
                 z.close(); return ok; })()",
        )
        .expect("wrong password");
        assert_eq!(wrong, Variant::Integer(0));
        assert!(
            !root.join("wrong.txt").exists(),
            "a failed extract must not write the destination"
        );

        let right = evaluate(
            &mut engine,
            "(function() { var z = new Unzip(); z.open(\"secret.zip\"); \n\
                 var ok = z.extract(\"secret.txt\", \"right.txt\", \"secret\");\n\
                 z.close(); return ok; })()",
        )
        .expect("right password");
        assert_eq!(right, Variant::Integer(1));
        assert_eq!(
            storage_read(&engine, "right.txt"),
            b"top secret payload for the encrypted fixture\n".repeat(2)
        );
    }

    /// An entry written with a data descriptor (flag bit 3, local sizes zero)
    /// reads through the central-directory values.
    #[test]
    fn a_data_descriptor_entry_reads() {
        let root = temp_root("descriptor");
        write_file(&root, "streamed.zip", &hex(FIXTURE_DESCRIPTOR_ZIP_HEX));
        let mut engine = plugin_engine(&root);

        let extracted = evaluate(
            &mut engine,
            "(function() { var z = new Unzip(); z.open(\"streamed.zip\"); \n\
                 var ok = z.extract(\"streamed.txt\", \"streamed.out\");\n\
                 z.close(); return ok; })()",
        )
        .expect("extract descriptor entry");
        assert_eq!(extracted, Variant::Integer(1));
        assert_eq!(
            storage_read(&engine, "streamed.out"),
            b"descriptor entry payload, written streaming-style.\n"
        );
    }

    /// `Storages.mountZip`/`unmountZip` drive the `zip://` media: reads,
    /// existence probes and directory listings all come from the mounted
    /// archive.
    #[test]
    fn the_zip_media_serves_mounted_archives() {
        let root = temp_root("media");
        write_file(&root, "fixture.zip", &hex(FIXTURE_ZIP_HEX));
        let mut engine = plugin_engine(&root);

        let mounted =
            evaluate(&mut engine, "Storages.mountZip(\"data\", \"fixture.zip\")").expect("mount");
        assert_eq!(mounted, Variant::Integer(1));

        assert_eq!(
            storage_read(&engine, "zip://data/readme.txt"),
            b"Hello from a python-built zip fixture.\n".repeat(3)
        );
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");
        assert!(storage.storage_exists("zip://data/README.TXT"));
        assert!(!storage.storage_exists("zip://data/missing.txt"));
        assert!(!storage.storage_exists("zip://other/readme.txt"));
        assert_eq!(
            storage
                .list_directory("zip://data/")
                .expect("list the archive root"),
            vec!["readme.txt".to_string(), "café.txt".to_string()]
        );
        assert_eq!(
            storage
                .list_directory("zip://data/data/")
                .expect("list a subdirectory"),
            vec!["stored.bin".to_string(), "big.txt".to_string()]
        );

        // A second mount of the same domain replaces the first, and unmount
        // reports whether a mount was dropped (`storage.cpp:505-532`).
        assert_eq!(
            evaluate(&mut engine, "Storages.mountZip(\"data\", \"fixture.zip\")")
                .expect("re-mount"),
            Variant::Integer(1)
        );
        assert_eq!(
            evaluate(&mut engine, "Storages.unmountZip(\"data\")").expect("unmount"),
            Variant::Integer(1)
        );
        assert_eq!(
            evaluate(&mut engine, "Storages.unmountZip(\"data\")").expect("unmount again"),
            Variant::Integer(0)
        );
    }

    /// The script path over a legacy archive: `Unzip.list` reports the CP932
    /// name, `extract` resolves the script string the game would pass, and the
    /// `zip://` media serves it. Before the ANSI decode this was the reported
    /// failure — the entry existed, but nothing could name it.
    #[test]
    fn a_legacy_cp932_archive_is_reachable_by_its_real_names() {
        let root = temp_root("cp932");
        write_file(&root, "legacy.zip", &hex(FIXTURE_CP932_ZIP_HEX));
        let mut engine = plugin_engine(&root);

        let listed = evaluate(
            &mut engine,
            "(function() {\n\
                 var z = new Unzip();\n\
                 z.open(\"legacy.zip\");\n\
                 var l = z.list();\n\
                 var out = l[0].filename + \"|\" + l[1].filename + \"|\" + l.length;\n\
                 z.close();\n\
                 return out;\n\
             })()",
        )
        .expect("list the legacy archive");
        assert_eq!(
            listed,
            Variant::String("正しい名前.txt|日本語.txt|2".to_string())
        );

        let extracted = evaluate(
            &mut engine,
            "(function() {\n\
                 var z = new Unzip();\n\
                 z.open(\"legacy.zip\");\n\
                 var ok = z.extract(\"正しい名前.txt\", \"out/name.txt\");\n\
                 z.close();\n\
                 return ok;\n\
             })()",
        )
        .expect("extract by the CP932 name");
        assert_eq!(extracted, Variant::Integer(1));
        assert_eq!(
            storage_read(&engine, "out/name.txt"),
            b"cp932 named entry\n"
        );

        // The mojibake spelling the flag-less bytes used to decode to must not
        // resolve, or the fix would have traded one name for another.
        let mojibake = evaluate(
            &mut engine,
            "(function() {\n\
                 var z = new Unzip();\n\
                 z.open(\"legacy.zip\");\n\
                 var ok = z.extract(\"É│é╡éóû╝æO.txt\", \"out/mojibake.txt\");\n\
                 z.close();\n\
                 return ok;\n\
             })()",
        )
        .expect("extract by the mojibake name");
        assert_eq!(mojibake, Variant::Integer(0));

        evaluate(&mut engine, "Storages.mountZip(\"legacy\", \"legacy.zip\")").expect("mount");
        assert_eq!(
            storage_read(&engine, "zip://legacy/正しい名前.txt"),
            b"cp932 named entry\n"
        );
        assert_eq!(
            storage_read(&engine, "zip://legacy/日本語.txt"),
            b"utf8 named entry\n"
        );
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");
        assert!(storage.storage_exists("zip://legacy/正しい名前.txt"));
        assert!(!storage.storage_exists("zip://legacy/É│é╡éóû╝æO.txt"));
        assert_eq!(
            storage
                .list_directory("zip://legacy/")
                .expect("list the legacy root"),
            vec!["正しい名前.txt".to_string(), "日本語.txt".to_string()]
        );
    }

    /// The media's failure text is the reference's (`storage.cpp:476,
    /// 542-557`): a mounted domain with an unreadable entry answers
    /// `%1:cannot open zipfile`, and a name without `/` is `invalid path:%1`.
    #[test]
    fn the_media_uses_the_reference_error_messages() {
        let root = temp_root("messages");
        write_file(&root, "fixture.zip", &hex(FIXTURE_ZIP_HEX));
        write_file(&root, "secret.zip", &hex(FIXTURE_ENCRYPTED_ZIP_HEX));
        let mut engine = plugin_engine(&root);
        evaluate(&mut engine, "Storages.mountZip(\"sec\", \"secret.zip\")").expect("mount");

        // An encrypted entry exists but cannot be opened without a password.
        let error = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage")
            .read_binary_storage("zip://sec/secret.txt")
            .map(|_| ())
            .expect_err("encrypted entry");
        assert!(
            error.to_string().contains("cannot open zipfile"),
            "unexpected message: {error}"
        );

        let media = ZipMedia::new();
        let error = media
            .open("nodomain")
            .map(|_| ())
            .expect_err("a name without a slash");
        assert_eq!(error.to_string(), "invalid path:nodomain");
        let error = media
            .open("d/missing.txt")
            .map(|_| ())
            .expect_err("a missing mount");
        assert_eq!(error.to_string(), "d/missing.txt:cannot open zipfile");
    }

    /// `Zip.add` writes the reference archive shape: UTF-8 names, deflated or
    /// stored as the level says, ZipCrypto with a password, and the file is
    /// readable again through `Unzip` and through the media.
    #[test]
    fn zip_add_writes_a_readable_archive() {
        let root = temp_root("add");
        write_file(&root, "src/text.txt", &deflate_test_text());
        write_file(&root, "src/binary.bin", &binary_test_payload());
        let mut engine = plugin_engine(&root);

        let written = evaluate(
            &mut engine,
            "(function() {\n\
                 var z = new Zip();\n\
                 z.open(\"out.zip\", 1);\n\
                 var a = z.add(\"src/text.txt\", \"text.txt\", 6);\n\
                 var b = z.add(\"src/binary.bin\", \"dir/binary.bin\", 0);\n\
                 var c = z.add(\"src/text.txt\", \"secret.txt\", void, \"pw\");\n\
                 z.close();\n\
                 return a && b && c;\n\
             })()",
        )
        .expect("write the archive");
        assert_eq!(written, Variant::Integer(1));

        // The archive parses through this port's reader with the expected
        // entries, methods and flags.
        let image = fs::read(root.join("out.zip")).expect("read out.zip");
        let archive =
            ReadArchive::new(Box::new(Cursor::new(image))).expect("parse written archive");
        assert_eq!(archive.entries.len(), 3);
        assert!(archive.entries[0].is_deflated() && !archive.entries[0].is_encrypted());
        assert!(!archive.entries[1].is_deflated());
        assert!(archive.entries[2].is_encrypted());
        assert!(
            archive
                .entries
                .iter()
                .all(|entry| entry.flags & FLAG_UTF8 != 0)
        );

        // ... and through the script surface, including the encrypted entry.
        let listed = evaluate(
            &mut engine,
            "(function() { var z = new Unzip(); z.open(\"out.zip\"); \n\
                 var l = z.list(); \n\
                 var ok = z.extract(\"secret.txt\", \"round-trip.txt\", \"pw\");\n\
                 z.close();\n\
                 return l.length + \":\" + l[1].deflated + \":\" + l[2].crypted + \":\" + ok; })()",
        )
        .expect("read the archive back");
        assert_eq!(listed, Variant::String("3:0:1:1".to_string()));
        assert_eq!(storage_read(&engine, "round-trip.txt"), deflate_test_text());

        // The media serves what the writer produced, too.
        evaluate(&mut engine, "Storages.mountZip(\"out\", \"out.zip\")").expect("mount");
        assert_eq!(
            storage_read(&engine, "zip://out/dir/binary.bin"),
            binary_test_payload()
        );
    }

    /// The three overwrite modes: `0` refuses an existing archive, `1`
    /// replaces it, `2` appends to it (and creates it when it does not exist).
    #[test]
    fn zip_open_honours_the_overwrite_modes() {
        let root = temp_root("overwrite");
        write_file(&root, "src/one.txt", b"one");
        write_file(&root, "src/two.txt", b"two");
        let mut engine = plugin_engine(&root);

        // Mode 0 on a missing name creates it and writes an empty archive.
        evaluate(
            &mut engine,
            "(function() { var z = new Zip(); z.open(\"one.zip\", 0); z.close(); })()",
        )
        .expect("create one.zip");
        assert!(root.join("one.zip").exists(), "open must create the file");

        let error = evaluate(
            &mut engine,
            "(function() { var z = new Zip(); z.open(\"one.zip\", 0); z.close(); })()",
        )
        .expect_err("mode 0 must refuse an existing archive");
        assert!(
            error.to_string().contains("one.zip exists."),
            "unexpected message: {error}"
        );

        // Mode 2 on a missing name falls back to create (`main.cpp:186-190`).
        let appended = evaluate(
            &mut engine,
            "(function() {\n\
                 var z = new Zip(); z.open(\"two.zip\", 2);\n\
                 z.add(\"src/one.txt\", \"one.txt\", 6); z.close();\n\
                 var a = new Zip(); a.open(\"two.zip\", 2);\n\
                 var ok = a.add(\"src/two.txt\", \"two.txt\", 6); a.close();\n\
                 var u = new Unzip(); u.open(\"two.zip\");\n\
                 var names = \"\";\n\
                 var l = u.list();\n\
                 for (var i = 0; i < l.length; i++) names += l[i].filename + \",\";\n\
                 u.close();\n\
                 return ok + \":\" + names;\n\
             })()",
        )
        .expect("append");
        assert_eq!(appended, Variant::String("1:one.txt,two.txt,".to_string()));
        assert!(!storage_read(&engine, "two.zip").is_empty());

        let error = evaluate(
            &mut engine,
            "(function() { var z = new Zip(); z.open(\"src/one.txt\", 2); })()",
        )
        .expect_err("appending to a non-zip must fail");
        assert!(
            error.to_string().contains("src/one.txt can't open."),
            "unexpected message: {error}"
        );
    }

    /// `Zip.add`'s failure surface: a missing source throws
    /// `"<src> not exists."`, an unopened archive throws `"don't open
    /// zipfile"`, and the same message covers `Unzip.list`/`extract`
    /// (`main.cpp:234, 248-252, 407, 478`).
    #[test]
    fn the_class_error_surface_matches_the_reference() {
        let root = temp_root("errors");
        let mut engine = plugin_engine(&root);

        let error = evaluate(
            &mut engine,
            "(function() { var z = new Zip(); z.open(\"e.zip\", 1);\n\
                 return z.add(\"missing.txt\", \"x\", 6); })()",
        )
        .expect_err("missing source");
        assert!(
            error.to_string().contains("missing.txt not exists."),
            "unexpected message: {error}"
        );

        let error = evaluate(
            &mut engine,
            "(function() { var z = new Zip(); z.add(\"a\", \"b\"); })()",
        )
        .expect_err("unopened zip");
        assert!(
            error.to_string().contains("don't open zipfile"),
            "unexpected message: {error}"
        );

        let error = evaluate(
            &mut engine,
            "(function() { var u = new Unzip(); return u.list(); })()",
        )
        .expect_err("unopened unzip");
        assert!(
            error.to_string().contains("don't open zipfile"),
            "unexpected message: {error}"
        );

        let error = evaluate(
            &mut engine,
            "(function() { var u = new Unzip(); u.open(\"nope.zip\"); })()",
        )
        .expect_err("missing archive");
        assert!(
            error.to_string().contains("nope.zip can't open."),
            "unexpected message: {error}"
        );

        // A missing entry is a `false` answer, not an error (`main.cpp:491-521`).
        write_file(&root, "fixture.zip", &hex(FIXTURE_ZIP_HEX));
        let missing = evaluate(
            &mut engine,
            "(function() { var u = new Unzip(); u.open(\"fixture.zip\");\n\
                 var ok = u.extract(\"missing\", \"out\"); u.close(); return ok; })()",
        )
        .expect("a missing entry must answer false");
        assert_eq!(missing, Variant::Integer(0));
    }

    /// The member and argument surface follows the decoded reference tables:
    /// `Zip` (open/close/add), `Unzip` (open/close/list/extract) and the two
    /// `Storages` members (`main.cpp:526-539`, `storage.cpp:619-622`).
    #[test]
    fn the_surface_matches_the_reference_table() {
        let root = temp_root("surface");
        let mut engine = plugin_engine(&root);

        for (member, class) in [
            ("open", "Zip"),
            ("close", "Zip"),
            ("add", "Zip"),
            ("open", "Unzip"),
            ("close", "Unzip"),
            ("list", "Unzip"),
            ("extract", "Unzip"),
        ] {
            let value = evaluate(
                &mut engine,
                &format!(
                    "(function() {{ var instance = new {class}(); \
                     return instance.{member} instanceof \"Function\"; }})()"
                ),
            )
            .unwrap_or_else(|error| panic!("{class}.{member}: {error}"));
            assert_eq!(
                value.to_integer().expect("instanceof is numeric"),
                1,
                "{class}.{member} is not installed"
            );
        }
        for member in ["mountZip", "unmountZip"] {
            let value = evaluate(
                &mut engine,
                &format!("(Storages.{member} instanceof \"Function\")"),
            )
            .unwrap_or_else(|error| panic!("Storages.{member}: {error}"));
            assert_eq!(value.to_integer().expect("instanceof is numeric"), 1);
        }

        // Argument-count checks are the reference's `TJS_E_BADPARAMCOUNT`
        // (`main.cpp:181, 232, 476`). `new` binds to the whole call expression
        // in the official grammar (`tjs.y:656`: `"new" func_call_expr`), so a
        // plain call is written through a statement, not `new X().m()`.
        for script in [
            "(function() { var z = new Zip(); z.open(); })()",
            "(function() { var z = new Zip(); z.add(\"only-one\"); })()",
            "(function() { var u = new Unzip(); u.open(); })()",
            "(function() { var u = new Unzip(); u.extract(\"only-one\"); })()",
            "Storages.mountZip(\"only-one\")",
            "Storages.unmountZip()",
        ] {
            let error = evaluate(&mut engine, script)
                .map(|_| ())
                .expect_err(&format!("`{script}` must fail"));
            assert_eq!(
                error.kind,
                TjsErrorKind::BadParamCount,
                "`{script}`: {error}"
            );
        }
    }

    /// `Plugins.link` installs the module a second time (the engine calls
    /// `register` again) without a duplicate-media failure, and
    /// `Plugins.unlink` drops the media (the reference's `doneZipStorage`).
    #[test]
    fn linking_again_keeps_one_media_and_unlinking_drops_it() {
        let root = temp_root("link");
        write_file(&root, "fixture.zip", &hex(FIXTURE_ZIP_HEX));
        let mut engine = plugin_engine(&root);
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );

        evaluate(&mut engine, "Plugins.link(\"minizip.dll\")").expect("link again");
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );

        evaluate(&mut engine, "Plugins.unlink(\"minizip.dll\")").expect("unlink");
        assert!(engine.host().storage_media_names().is_empty());

        evaluate(&mut engine, "Plugins.link(\"minizip.dll\")").expect("link back");
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );
    }

    /// A host without project storage installs the plugin without a media and
    /// says so, instead of failing the boot (`lzfs` takes the same path).
    #[test]
    fn a_host_without_storage_stays_installable() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(MinizipPlugin::new())
            .expect("install into a script-only host");
        assert!(engine.host().storage_media_names().is_empty());
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("minizip") && line.contains("not registered")),
            "the failed registration must be visible in the log"
        );
        // The classes still install, so a script can work against whatever
        // storage the host provides.
        let value = engine
            .execute_expression("inline.tjs", "typeof Zip")
            .expect("Zip class");
        assert_ne!(value, Variant::String("undefined".to_string()));
    }
}
