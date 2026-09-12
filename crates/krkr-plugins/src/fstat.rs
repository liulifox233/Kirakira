//! `fstat.dll` — file statistics and file operations on `Storages`.
//!
//! Reference: krkrz `src/plugins/win32/fstat/Main.cpp` (28 members) plus the
//! krkr2 trunk revision, which adds `createDirectoryNoNormalize`, and the
//! `TemporaryFiles` class and `FILE_ATTRIBUTE_*` globals that ship with both.
//! PARQUET links `fstat.dll` by name even though the code lives inside
//! `PackinOne.dll`, so this module installs the surface under that DLL's own
//! name.
//!
//! Where the reference reaches for Win32, this module goes through the engine
//! storage layer: a name that resolves to a real file or directory
//! ([`KrkrHost::placed_path`]) is served by `std::fs`, while an archive or
//! memory member is served by the host's read/write/listing calls. Names that
//! exist only inside an XP3 take the path the reference takes there — `fstat`
//! answers with `size` alone and the mutators answer 0 instead of touching the
//! archive. The members the engine has no capability for (directory creation,
//! the Windows shell picker) report failure honestly instead of pretending to
//! succeed.
//!
//! Deliberate divergences from the reference, all driven by the engine's
//! storage model:
//!
//! - `dirlist`/`dirlistEx` also list archive and memory members (the reference
//!   can only see the local filesystem) and do not synthesize the Win32
//!   `./`/`../` entries.
//! - Attributes and timestamps map onto what POSIX exposes: the read-only bit
//!   is settable, the creation time (`ctime`) is not, and `getFileAttributes`
//!   on a missing path answers `0xFFFFFFFF` like the reference does.
//! - Directory creation and `selectDirectory` need storage-layer and shell
//!   support the engine does not have; they return the reference's failure
//!   value (0) and log once.
//! - `isExistentDirectory` takes a directory name with or without the trailing
//!   `/` the reference demands (`Main.cpp:759-762`): this engine's
//!   `getFullPath` drops the delimiter the reference's keeps, so scripts that
//!   probe `getFullPath` results would otherwise be rejected for asking about
//!   a directory that is plainly there.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Storages.fstat/dirlist/dirlistEx/copyFile/deleteFile/moveFile/truncateFile/\
              exportFile/getTime/setTime/getFileAttributes/getMD5HashString/getTemporaryName, \
              TemporaryFiles class",
    notes: "Stat, digest, listing, copy/export and timestamp members work on storage names; \
            directory creation and selectDirectory report failure because the storage layer has \
            no directory-creation or shell-dialog capability. A directory with no entries is \
            invisible to the engine's file-based storage resolution, and only the read-only \
            attribute plus mtime/atime are settable on this platform.",
    install: |engine| engine.register_plugin(FstatPlugin),
};

pub struct FstatPlugin;

impl KrkrPlugin for FstatPlugin {
    fn name(&self) -> &str {
        "fstat.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_fstat(runtime);
        runtime.host_mut().log(
            "fstat.dll registered: Storages stat/dir/copy/md5 members and the TemporaryFiles class",
        );
        Ok(())
    }
}

/// The seven attribute constants the reference sets on the global object
/// (`Main.cpp:15-22`).
const FILE_ATTRIBUTE_READONLY: i64 = 0x0000_0001;
const FILE_ATTRIBUTE_HIDDEN: i64 = 0x0000_0002;
const FILE_ATTRIBUTE_SYSTEM: i64 = 0x0000_0004;
const FILE_ATTRIBUTE_DIRECTORY: i64 = 0x0000_0010;
const FILE_ATTRIBUTE_ARCHIVE: i64 = 0x0000_0020;
const FILE_ATTRIBUTE_NORMAL: i64 = 0x0000_0080;
const FILE_ATTRIBUTE_TEMPORARY: i64 = 0x0000_0100;

/// `<attributes>` the reference's `setFileAttributes`/`resetFileAttributes`
/// accept: `NORMAL|READONLY|HIDDEN|ARCHIVE|SYSTEM|TEMPORARY` (`Main.cpp:612`).
const ATTRIBUTE_MASK: i64 = FILE_ATTRIBUTE_READONLY
    | FILE_ATTRIBUTE_HIDDEN
    | FILE_ATTRIBUTE_SYSTEM
    | FILE_ATTRIBUTE_ARCHIVE
    | FILE_ATTRIBUTE_NORMAL
    | FILE_ATTRIBUTE_TEMPORARY;

/// FILETIME counts 100 ns ticks from 1601-01-01; Unix time starts
/// 11644473600 s later (`Main.cpp:56`, the literal `0x19DB1DED53E8000`).
const FILETIME_EPOCH_OFFSET: i64 = 116_444_736_000_000_000;

/// Every member the plugin attaches to `Storages`, in reference registration
/// order (`Main.cpp:988-1016` plus trunk's `createDirectoryNoNormalize`).
/// Shared with the module's surface test.
#[cfg(test)]
const STORAGES_MEMBERS: &[&str] = &[
    "clearStorageCaches",
    "fstat",
    "getTime",
    "setTime",
    "getLastModifiedFileTime",
    "setLastModifiedFileTime",
    "exportFile",
    "deleteFile",
    "truncateFile",
    "moveFile",
    "dirlist",
    "dirlistEx",
    "removeDirectory",
    "createDirectory",
    "createDirectoryNoNormalize",
    "changeDirectory",
    "setFileAttributes",
    "resetFileAttributes",
    "getFileAttributes",
    "selectDirectory",
    "isExistentDirectory",
    "copyFile",
    "copyFileNoNormalize",
    "isExistentStorageNoSearchNoNormalize",
    "getDisplayName",
    "getMD5HashString",
    "searchPath",
    "getTemporaryName",
];

fn install_fstat(runtime: &mut Runtime<KrkrHost>) {
    let storages = match runtime.global_member("Storages") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "Storages");
            runtime.set_global_member("Storages", Variant::Object(handle));
            handle
        }
    };
    for (name, handler) in [
        (
            "clearStorageCaches",
            storages_clear_storage_caches
                as fn(
                    &mut Runtime<KrkrHost>,
                    Option<ObjectHandle>,
                    Vec<Variant>,
                ) -> Result<Variant>,
        ),
        ("fstat", storages_fstat),
        ("getTime", storages_get_time),
        ("setTime", storages_set_time),
        (
            "getLastModifiedFileTime",
            storages_get_last_modified_file_time,
        ),
        (
            "setLastModifiedFileTime",
            storages_set_last_modified_file_time,
        ),
        ("exportFile", storages_export_file),
        ("deleteFile", storages_delete_file),
        ("truncateFile", storages_truncate_file),
        ("moveFile", storages_move_file),
        ("dirlist", storages_dirlist),
        ("dirlistEx", storages_dirlist_ex),
        ("removeDirectory", storages_remove_directory),
        ("createDirectory", storages_create_directory),
        ("createDirectoryNoNormalize", storages_create_directory),
        ("changeDirectory", storages_change_directory),
        ("setFileAttributes", storages_set_file_attributes),
        ("resetFileAttributes", storages_reset_file_attributes),
        ("getFileAttributes", storages_get_file_attributes),
        ("selectDirectory", storages_select_directory),
        ("isExistentDirectory", storages_is_existent_directory),
        ("copyFile", storages_copy_file),
        ("copyFileNoNormalize", storages_copy_file),
        (
            "isExistentStorageNoSearchNoNormalize",
            storages_is_existent_storage_no_search_no_normalize,
        ),
        ("getDisplayName", storages_get_display_name),
        ("getMD5HashString", storages_get_md5_hash_string),
        ("searchPath", storages_search_path),
        ("getTemporaryName", storages_get_temporary_name),
    ] {
        runtime.register_object_native(storages, name, handler);
    }
    runtime.register_object_native_property(
        storages,
        "currentPath",
        storages_current_path_get,
        storages_current_path_set,
    );

    for (name, value) in [
        ("FILE_ATTRIBUTE_READONLY", FILE_ATTRIBUTE_READONLY),
        ("FILE_ATTRIBUTE_HIDDEN", FILE_ATTRIBUTE_HIDDEN),
        ("FILE_ATTRIBUTE_SYSTEM", FILE_ATTRIBUTE_SYSTEM),
        ("FILE_ATTRIBUTE_DIRECTORY", FILE_ATTRIBUTE_DIRECTORY),
        ("FILE_ATTRIBUTE_ARCHIVE", FILE_ATTRIBUTE_ARCHIVE),
        ("FILE_ATTRIBUTE_NORMAL", FILE_ATTRIBUTE_NORMAL),
        ("FILE_ATTRIBUTE_TEMPORARY", FILE_ATTRIBUTE_TEMPORARY),
    ] {
        runtime.set_global_member(name, Variant::Integer(value));
    }

    install_temporary_files(runtime);
}

// ---------------------------------------------------------------------------
// Shared helpers

/// Reads a required string argument. The reference checks the argument count
/// before it converts anything, so a missing argument is
/// `TJS_E_BADPARAMCOUNT` (`Invalid argument count`).
fn required_string(args: &[Variant], index: usize, _what: &str) -> Result<String> {
    match args.get(index) {
        Some(value) => value.to_tjs_string(),
        None => Err(TjsError::bad_param_count()),
    }
}

fn arg_string(args: &[Variant], index: usize) -> Result<Option<String>> {
    args.get(index).map(Variant::to_tjs_string).transpose()
}

fn arg_integer(args: &[Variant], index: usize, _what: &str) -> Result<i64> {
    match args.get(index) {
        Some(value) => value.to_integer(),
        None => Err(TjsError::bad_param_count()),
    }
}

/// The reference's `cannot open : <name>` (`Main.cpp:206`).
fn cannot_open(name: &str) -> TjsError {
    TjsError::runtime(format!("cannot open : {name}"))
}

/// The reference's directory-name check (`Main.cpp:452`).
fn require_trailing_slash(name: &str) -> Result<()> {
    if name.ends_with('/') {
        Ok(())
    } else {
        Err(TjsError::runtime(
            "'/' must be specified at the end of given directory name.",
        ))
    }
}

fn log_once(runtime: &mut Runtime<KrkrHost>, flag: &AtomicBool, message: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        runtime.host_mut().log(message);
    }
}

/// True when the storage name exists as a file or a directory.
fn storage_present(runtime: &Runtime<KrkrHost>, name: &str) -> bool {
    runtime.host().storage_exists(name) || runtime.host().storage_is_directory(name)
}

/// The native path behind a storage name, when the engine has one.
///
/// Storage resolution is file-based, so a directory name has no `placed_path`
/// of its own; a directory's path is derived from one of its entries, which is
/// the only real path the engine can hand back for it. An empty directory
/// therefore has no observable metadata.
fn native_path(runtime: &mut Runtime<KrkrHost>, name: &str) -> Option<PathBuf> {
    if let Some(path) = runtime.host().placed_path(name) {
        return Some(path);
    }
    if !name.ends_with('/') && !runtime.host().storage_is_directory(name) {
        return None;
    }
    let entries = runtime.host().storage_dirlist(name).ok()?;
    entries.iter().find_map(|entry| {
        runtime
            .host()
            .placed_path(&format!("{name}{entry}"))
            .and_then(|path| path.parent().map(Path::to_path_buf))
    })
}

fn native_metadata(runtime: &mut Runtime<KrkrHost>, name: &str) -> Option<std::fs::Metadata> {
    std::fs::metadata(native_path(runtime, name)?).ok()
}

/// Builds a TJS `Dictionary` the way the reference's `TJSCreateDictionaryObject`
/// does, so scripts see a dictionary instance rather than an opaque object.
fn new_dictionary(runtime: &mut Runtime<KrkrHost>) -> Option<ObjectHandle> {
    runtime
        .call_function(runtime.global_member("Dictionary"), Vec::new())
        .ok()?
        .object_handle()
}

fn set_member(runtime: &mut Runtime<KrkrHost>, object: ObjectHandle, name: &str, value: Variant) {
    runtime.set_object_member(object, name, value);
}

/// `Date` for a millisecond timestamp, or `void` when there is no time to
/// report (the reference leaves the member void for a zero FILETIME,
/// `Main.cpp:57-68`).
fn date_variant(runtime: &mut Runtime<KrkrHost>, millis: Option<i64>) -> Variant {
    let Some(millis) = millis else {
        return Variant::Void;
    };
    runtime
        .call_function(
            runtime.global_member("Date"),
            vec![Variant::Integer(millis)],
        )
        .unwrap_or(Variant::Void)
}

/// The three timestamps the reference stores, in its insertion order
/// (`Main.cpp:195-198`).
fn set_time_members(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    metadata: &std::fs::Metadata,
) {
    for (member, value) in [
        ("mtime", metadata.modified().ok()),
        ("ctime", metadata.created().ok()),
        ("atime", metadata.accessed().ok()),
    ] {
        let millis = value.and_then(system_time_millis);
        let date = date_variant(runtime, millis);
        set_member(runtime, object, member, date);
    }
}

fn system_time_millis(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as i64)
}

fn filetime_to_millis(filetime: i64) -> i64 {
    (filetime - FILETIME_EPOCH_OFFSET).div_euclid(10_000)
}

fn millis_to_filetime(millis: i64) -> i64 {
    millis
        .saturating_mul(10_000)
        .saturating_add(FILETIME_EPOCH_OFFSET)
}

fn filetime_to_system_time(filetime: i64) -> SystemTime {
    let millis = filetime_to_millis(filetime).max(0) as u64;
    UNIX_EPOCH + Duration::from_millis(millis)
}

fn system_time_to_filetime(time: SystemTime) -> Option<i64> {
    system_time_millis(time).map(millis_to_filetime)
}

/// Reads a `Date` member out of a stat dictionary and converts it back to the
/// millisecond timestamp the runtime's `Date` stores.
fn date_member_millis(
    runtime: &mut Runtime<KrkrHost>,
    times: ObjectHandle,
    member: &str,
) -> Option<i64> {
    let value = runtime.object_member(times, member);
    let handle = value.object_handle()?;
    match runtime.resolve_object_member(handle, "getTime") {
        Ok(Variant::Closure(_) | Variant::Object(_)) => {}
        Ok(_) => return None,
        Err(_) => return None,
    }
    match runtime.call_object_method(handle, "getTime", Vec::new()) {
        Ok(Variant::Integer(millis)) => Some(millis),
        Ok(Variant::Real(millis)) => Some(millis as i64),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Stat members

fn storages_clear_storage_caches(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let _ = runtime.host().clear_archive_cache();
    Ok(Variant::Void)
}

fn storages_fstat(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.fstat")?;
    if !storage_present(runtime, &name) {
        return Err(cannot_open(&name));
    }
    let Some(info) = new_dictionary(runtime) else {
        return Ok(Variant::Void);
    };
    match native_metadata(runtime, &name) {
        // A real file carries size and the three timestamps; a directory
        // carries the timestamps without a size (`Main.cpp:143-147`).
        Some(metadata) => {
            if !metadata.is_dir() {
                set_member(
                    runtime,
                    info,
                    "size",
                    Variant::Integer(metadata.len() as i64),
                );
            }
            set_time_members(runtime, info, &metadata);
        }
        // Archive and memory members answer size only (`Main.cpp:227-246`).
        None => match runtime.host().read_binary_storage(&name) {
            Ok(bytes) => set_member(runtime, info, "size", Variant::Integer(bytes.len() as i64)),
            Err(_) => return Err(cannot_open(&name)),
        },
    }
    Ok(Variant::Object(info))
}

fn storages_get_time(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.getTime")?;
    let Some(metadata) = native_metadata(runtime, &name) else {
        // `_getTime` answers `cannot open : <raw argument>` for every name it
        // cannot stat, archive members included (`Main.cpp:185-208`).
        return Err(cannot_open(&name));
    };
    let Some(info) = new_dictionary(runtime) else {
        return Ok(Variant::Void);
    };
    set_time_members(runtime, info, &metadata);
    Ok(Variant::Object(info))
}

fn storages_set_time(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.setTime")?;
    let Some(times) = args.get(1).and_then(Variant::object_handle) else {
        return Err(TjsError::runtime(
            "Storages.setTime requires a dictionary of times",
        ));
    };
    let Some(path) = native_path(runtime, &name) else {
        return Ok(Variant::Integer(0));
    };
    let modified = date_member_millis(runtime, times, "mtime");
    let accessed = date_member_millis(runtime, times, "atime");
    // POSIX has no settable creation time; the reference sets it through
    // `SetFileTime`'s creation slot, which this engine cannot reach.
    let _ = date_member_millis(runtime, times, "ctime");
    if modified.is_none() && accessed.is_none() {
        return Ok(Variant::Integer(0));
    }
    let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) else {
        return Ok(Variant::Integer(0));
    };
    let mut file_times = std::fs::FileTimes::new();
    if let Some(millis) = modified {
        file_times = file_times.set_modified(filetime_to_system_time(millis_to_filetime(millis)));
    }
    if let Some(millis) = accessed {
        file_times = file_times.set_accessed(filetime_to_system_time(millis_to_filetime(millis)));
    }
    Ok(Variant::Integer(i64::from(
        file.set_times(file_times).is_ok(),
    )))
}

fn storages_get_last_modified_file_time(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.getLastModifiedFileTime")?;
    let filetime = native_metadata(runtime, &name)
        .and_then(|metadata| metadata.modified().ok())
        .and_then(system_time_to_filetime)
        .unwrap_or(0);
    Ok(Variant::Integer(filetime))
}

fn storages_set_last_modified_file_time(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.setLastModifiedFileTime")?;
    let filetime = arg_integer(&args, 1, "Storages.setLastModifiedFileTime")?;
    let Some(path) = native_path(runtime, &name) else {
        return Ok(Variant::Integer(0));
    };
    let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) else {
        return Ok(Variant::Integer(0));
    };
    let times = std::fs::FileTimes::new().set_modified(filetime_to_system_time(filetime));
    Ok(Variant::Integer(i64::from(file.set_times(times).is_ok())))
}

// ---------------------------------------------------------------------------
// File operations

fn storages_export_file(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let source = required_string(&args, 0, "Storages.exportFile")?;
    let destination = required_string(&args, 1, "Storages.exportFile")?;
    let Ok(bytes) = runtime.host().read_binary_storage(&source) else {
        return Err(TjsError::runtime(format!("cannot open readfile: {source}")));
    };
    runtime
        .host_mut()
        .write_binary_storage(&destination, "w", &bytes)
        .map_err(|_| TjsError::runtime(format!("cannot open storefile: {destination}")))?;
    Ok(Variant::Void)
}

fn storages_delete_file(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.deleteFile")?;
    let Some(path) = native_path(runtime, &name) else {
        // An archive member is skipped silently, exactly like the reference
        // (`Main.cpp:355-357`).
        return Ok(Variant::Integer(0));
    };
    let removed = std::fs::remove_file(&path).is_ok();
    if removed {
        let _ = runtime.host().clear_archive_cache();
    }
    Ok(Variant::Integer(i64::from(removed)))
}

fn storages_truncate_file(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.truncateFile")?;
    let size = arg_integer(&args, 1, "Storages.truncateFile")?;
    let Some(path) = native_path(runtime, &name) else {
        return Ok(Variant::Integer(0));
    };
    let truncated = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .and_then(|file| file.set_len(size.max(0) as u64))
        .is_ok();
    Ok(Variant::Integer(i64::from(truncated)))
}

fn storages_move_file(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let from = required_string(&args, 0, "Storages.moveFile")?;
    let to = required_string(&args, 1, "Storages.moveFile")?;
    let (Some(from_path), Some(to_path)) = (native_path(runtime, &from), native_path(runtime, &to))
    else {
        // The reference moves local files only; an archive source or a
        // destination outside the local media cannot be resolved here either.
        return Ok(Variant::Integer(0));
    };
    let moved = std::fs::rename(&from_path, &to_path).is_ok();
    if moved {
        let _ = runtime.host().clear_archive_cache();
    }
    Ok(Variant::Integer(i64::from(moved)))
}

fn storages_copy_file(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let from = required_string(&args, 0, "Storages.copyFile")?;
    let to = required_string(&args, 1, "Storages.copyFile")?;
    let fail_if_exist = args
        .get(2)
        .map(Variant::to_integer)
        .transpose()?
        .is_some_and(|value| value != 0);
    if fail_if_exist && runtime.host().storage_exists(&to) {
        return Ok(Variant::Integer(0));
    }
    let Ok(bytes) = runtime.host().read_binary_storage(&from) else {
        return Ok(Variant::Integer(0));
    };
    let copied = runtime
        .host_mut()
        .write_binary_storage(&to, "w", &bytes)
        .is_ok();
    if copied {
        let _ = runtime.host().clear_archive_cache();
    }
    Ok(Variant::Integer(i64::from(copied)))
}

// ---------------------------------------------------------------------------
// Directory members

fn storages_dirlist(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let directory = required_string(&args, 0, "Storages.dirlist")?;
    require_trailing_slash(&directory)?;
    let entries = runtime
        .host()
        .storage_dirlist(&directory)
        .map_err(|_| TjsError::runtime("Directory not found."))?;
    let values = entries.into_iter().map(Variant::String).collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

fn storages_dirlist_ex(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let directory = required_string(&args, 0, "Storages.dirlistEx")?;
    require_trailing_slash(&directory)?;
    let entries = runtime
        .host()
        .storage_dirlist(&directory)
        .map_err(|_| TjsError::runtime("Directory not found."))?;
    let values = Vec::with_capacity(entries.len());
    let values = entries.into_iter().fold(values, |mut values, entry| {
        let child = format!("{directory}{entry}");
        let display = entry.trim_end_matches('/').to_string();
        let Some(info) = new_dictionary(runtime) else {
            return values;
        };
        set_member(runtime, info, "name", Variant::String(display));
        match native_metadata(runtime, &child) {
            Some(metadata) => {
                set_member(
                    runtime,
                    info,
                    "size",
                    Variant::Integer(metadata.len() as i64),
                );
                set_member(
                    runtime,
                    info,
                    "attrib",
                    Variant::Integer(file_attributes(&metadata)),
                );
                set_time_members(runtime, info, &metadata);
            }
            None => {
                let size = runtime
                    .host()
                    .read_binary_storage(&child)
                    .map(|bytes| bytes.len() as i64)
                    .unwrap_or(0);
                set_member(runtime, info, "size", Variant::Integer(size));
                set_member(
                    runtime,
                    info,
                    "attrib",
                    Variant::Integer(if entry.ends_with('/') {
                        FILE_ATTRIBUTE_DIRECTORY
                    } else {
                        FILE_ATTRIBUTE_ARCHIVE
                    }),
                );
                for member in ["mtime", "ctime", "atime"] {
                    set_member(runtime, info, member, Variant::Void);
                }
            }
        }
        values.push(Variant::Object(info));
        values
    });
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

fn storages_remove_directory(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let directory = required_string(&args, 0, "Storages.removeDirectory")?;
    require_trailing_slash(&directory)?;
    let Some(path) = native_path(runtime, &directory) else {
        return Ok(Variant::Integer(0));
    };
    let removed = std::fs::remove_dir(&path).is_ok();
    if removed {
        let _ = runtime.host().clear_archive_cache();
    }
    Ok(Variant::Integer(i64::from(removed)))
}

fn storages_create_directory(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    let directory = required_string(&args, 0, "Storages.createDirectory")?;
    if directory.is_empty() {
        return Err(TjsError::bad_param_count());
    }
    require_trailing_slash(&directory)?;
    if runtime.host().storage_is_directory(&directory) {
        return Ok(Variant::Integer(1));
    }
    log_once(
        runtime,
        &LOGGED,
        "fstat.dll: createDirectory needs a mutable-storage API the engine does not expose; \
         returning the reference's failure value",
    );
    Ok(Variant::Integer(0))
}

fn storages_change_directory(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let directory = required_string(&args, 0, "Storages.changeDirectory")?;
    require_trailing_slash(&directory)?;
    let Some(path) = native_path(runtime, &directory) else {
        return Ok(Variant::Integer(0));
    };
    // The reference changes the process working directory and, like it, does
    // not touch the storage layer's own resolution rules (`Main.cpp:584-599`).
    Ok(Variant::Integer(i64::from(
        std::env::set_current_dir(path).is_ok(),
    )))
}

fn storages_is_existent_directory(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // The reference does check for the trailing `/` here as well
    // (`Main.cpp:759-762`), but it never sees a slash-less name on this path:
    // scripts probe the result of `Storages.getFullPath`, whose normalization
    // keeps the delimiter it was handed (`StorageIntf.cpp:549`, `:1391-1402`;
    // `tTVPFileMedia::NormalizePathName` only lower-cases). This engine's
    // `getFullPath` normalizes the delimiter away, and the engine's own
    // `Storages.isExistentDirectory` binding never asked for one, so honoring
    // the reference's check here would only turn a plain existence probe into
    // a boot-stopping throw. GINKA's `addAutoPathRecursive` probes the
    // optional `setup/debug/` exactly that way. Both spellings resolve through
    // `storage_is_directory`, so accept them and report 0/1 as before.
    let directory = required_string(&args, 0, "Storages.isExistentDirectory")?;
    Ok(Variant::Integer(i64::from(
        runtime.host().storage_is_directory(&directory),
    )))
}

fn storages_is_existent_storage_no_search_no_normalize(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.isExistentStorageNoSearchNoNormalize")?;
    Ok(Variant::Integer(i64::from(
        runtime.host().storage_exists_exact(&name),
    )))
}

// ---------------------------------------------------------------------------
// Attributes

/// The attribute bits the engine can observe: directory and read-only.
fn file_attributes(metadata: &std::fs::Metadata) -> i64 {
    let mut attributes = 0;
    if metadata.is_dir() {
        attributes |= FILE_ATTRIBUTE_DIRECTORY;
    }
    if metadata.permissions().readonly() {
        attributes |= FILE_ATTRIBUTE_READONLY;
    }
    if attributes == 0 {
        attributes = FILE_ATTRIBUTE_ARCHIVE | FILE_ATTRIBUTE_NORMAL;
    }
    attributes
}

fn storages_get_file_attributes(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.getFileAttributes")?;
    let attributes = native_metadata(runtime, &name)
        .map(|metadata| file_attributes(&metadata))
        // The reference hands back the raw `0xFFFFFFFF` of a failed Win32
        // call (`Main.cpp:649`).
        .unwrap_or(0xFFFF_FFFF);
    Ok(Variant::Integer(attributes))
}

fn storages_set_file_attributes(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    set_read_only_attribute(runtime, &args, true, "Storages.setFileAttributes")
}

fn storages_reset_file_attributes(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    set_read_only_attribute(runtime, &args, false, "Storages.resetFileAttributes")
}

/// Applies the one attribute POSIX exposes. The reference masks the argument
/// to `NORMAL|READONLY|HIDDEN|ARCHIVE|SYSTEM|TEMPORARY` (`Main.cpp:612`); the
/// hidden/system/archive/temporary bits have no portable equivalent.
fn set_read_only_attribute(
    runtime: &mut Runtime<KrkrHost>,
    args: &[Variant],
    read_only: bool,
    what: &str,
) -> Result<Variant> {
    let name = required_string(args, 0, what)?;
    let attributes = arg_integer(args, 1, what)?;
    let Some(path) = native_path(runtime, &name) else {
        return Ok(Variant::Integer(0));
    };
    if attributes & ATTRIBUTE_MASK & FILE_ATTRIBUTE_READONLY == 0 {
        return Ok(Variant::Integer(1));
    }
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Ok(Variant::Integer(0));
    };
    let mut permissions = metadata.permissions();
    permissions.set_readonly(read_only);
    Ok(Variant::Integer(i64::from(
        std::fs::set_permissions(&path, permissions).is_ok(),
    )))
}

// ---------------------------------------------------------------------------
// Display, digest, search, temporary names

fn storages_get_display_name(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.getDisplayName")?;
    let display = name.trim_end_matches('/');
    // The Windows shell display name is the file's own name; on this engine it
    // is the storage's last path component, which is what the manual documents
    // and what most callers use it for.
    let display = display
        .rsplit(['/', '\\'])
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(display);
    Ok(Variant::String(display.to_string()))
}

fn storages_get_md5_hash_string(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_string(&args, 0, "Storages.getMD5HashString")?;
    let Ok(bytes) = runtime.host().read_binary_storage(&name) else {
        return Err(cannot_open(&name));
    };
    Ok(Variant::String(crate::scripts_ex::md5_hex(&bytes)))
}

fn storages_search_path(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let filename = required_string(&args, 0, "Storages.searchPath")?;
    let search_path = arg_string(&args, 1)?;
    let directories = match search_path.filter(|path| !path.is_empty()) {
        Some(path) => path,
        None => std::env::var("PATH").unwrap_or_default(),
    };
    for directory in directories.split(';').flat_map(|part| part.split(':')) {
        if directory.is_empty() {
            continue;
        }
        let candidate = PathBuf::from(directory).join(&filename);
        if candidate.is_file() {
            // The reference answers a normalized storage name; this engine's
            // storage names have no media prefix, so the local path is what a
            // caller can feed back into the storage layer.
            return Ok(Variant::String(
                candidate.to_string_lossy().replace('\\', "/"),
            ));
        }
    }
    Ok(Variant::Void)
}

fn storages_get_temporary_name(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `TVPGetTemporaryName` builds `<temp>\krkr_<12 hex>_<counter>_<pid>` and
    // never creates the file (`StorageImpl.cpp:270-297`).
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = system_time_millis(SystemTime::now()).unwrap_or(0) as u64
        ^ sequence.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let name = format!(
        "krkr_{:012x}_{}_{}",
        seed & 0xFFFF_FFFF_FFFF,
        sequence,
        std::process::id()
    );
    Ok(Variant::String(format!(
        "{}/{name}",
        std::env::temp_dir().display()
    )))
}

fn storages_select_directory(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if args.is_empty() {
        return Err(TjsError::bad_param_count());
    }
    log_once(
        runtime,
        &LOGGED,
        "fstat.dll: selectDirectory has no host shell dialog; answering 0 (cancelled)",
    );
    Ok(Variant::Integer(0))
}

fn storages_current_path_get(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    let Ok(path) = std::env::current_dir() else {
        return Ok(Variant::String(String::new()));
    };
    let mut path = path.to_string_lossy().replace('\\', "/");
    if !path.ends_with('/') {
        path.push('/');
    }
    Ok(Variant::String(path))
}

fn storages_current_path_set(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    value: Variant,
) -> Result<()> {
    let path = value.to_tjs_string()?;
    let changed = PathBuf::from(&path);
    if std::env::set_current_dir(changed).is_ok() {
        return Ok(());
    }
    runtime.host_mut().log(&format!(
        "fstat.dll: setCurrentPath failed for {}",
        path.trim_end_matches('/')
    ));
    Err(TjsError::runtime(format!("setCurrentPath failed:{}", path)))
}

// ---------------------------------------------------------------------------
// TemporaryFiles

fn install_temporary_files(runtime: &mut Runtime<KrkrHost>) {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "TemporaryFiles");
            install_temporary_files_members(runtime, instance);
            let paths = runtime.alloc_array_object(Vec::new());
            runtime.set_object_member(instance, "__paths", Variant::Object(paths));
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "TemporaryFiles");
    install_temporary_files_members(runtime, handle);
    runtime.set_global_member("TemporaryFiles", Variant::Object(handle));
}

fn install_temporary_files_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", temporary_files_finalize);
    runtime.register_object_native(handle, "entry", temporary_files_entry);
    runtime.register_object_native(handle, "entryFolder", temporary_files_entry_folder);
}

fn temporary_files_this(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
}

fn temporary_files_entry(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    claim_temporary_path(runtime, this_obj, &args, false)
}

fn temporary_files_entry_folder(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    claim_temporary_path(runtime, this_obj, &args, true)
}

/// Claims a path for delete-on-finalize, mirroring `TemporaryFiles::_entry`
/// (`Main.cpp:1044-1057`): the reference opens the file with
/// `FILE_FLAG_DELETE_ON_CLOSE`, so the file disappears when the TJS object is
/// invalidated; this module records the path and deletes it from `finalize`.
///
/// The engine calls `finalize` only for the VM's `invalidate` instruction
/// (`krkr-tjs2/src/vm/dispatch.rs:2039-2090`), not from a garbage collector,
/// so a holder a script simply drops the reference to keeps its files until
/// the object is invalidated explicitly.
fn claim_temporary_path(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: &[Variant],
    folder: bool,
) -> Result<Variant> {
    let name = required_string(args, 0, "TemporaryFiles.entry")?;
    let Some(this) = temporary_files_this(runtime, this_obj) else {
        return Err(TjsError::native_class_crash());
    };
    // `_entry` opens the localized path with `OPEN_EXISTING` and answers 0 when
    // that fails, so a name with no local file simply cannot be claimed.
    let Some(path) = native_path(runtime, &name) else {
        return Ok(Variant::Integer(0));
    };
    let present = if folder {
        path.is_dir()
    } else {
        path.is_file()
    };
    if !present {
        return Ok(Variant::Integer(0));
    }
    let mut paths = match runtime.object_member(this, "__paths") {
        Variant::Object(handle) => Some(handle),
        _ => None,
    };
    if paths.is_none() {
        paths = Some(runtime.alloc_array_object(Vec::new()));
        if let Some(handle) = paths {
            runtime.set_object_member(this, "__paths", Variant::Object(handle));
        }
    }
    if let Some(handle) = paths {
        runtime.array_push(handle, Variant::String(path.to_string_lossy().into_owned()));
    }
    Ok(Variant::Integer(1))
}

fn temporary_files_finalize(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = temporary_files_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let Some(paths) = runtime.object_member(this, "__paths").object_handle() else {
        return Ok(Variant::Void);
    };
    let Some(entries) = runtime.array_elements(paths).map(Vec::from) else {
        return Ok(Variant::Void);
    };
    for entry in entries {
        let Variant::String(path) = entry else {
            continue;
        };
        let path = PathBuf::from(path);
        if path.is_dir() {
            let _ = std::fs::remove_dir(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(Variant::Void)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, time::SystemTime};

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths};
    use krkr_tjs2::runtime::Variant;

    use super::*;

    #[test]
    fn every_reference_member_is_installed_on_storages() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(FstatPlugin).expect("plugin");
        for name in STORAGES_MEMBERS {
            let value = engine
                .execute_expression("inline.tjs", &format!("typeof Storages.{name}"))
                .unwrap_or_else(|error| panic!("Storages.{name} probe failed: {error}"));
            assert_ne!(
                value,
                Variant::String("undefined".to_string()),
                "Storages.{name} is not installed"
            );
        }
        assert_ne!(
            engine
                .execute_expression("inline.tjs", "typeof Storages.currentPath")
                .expect("currentPath probe"),
            Variant::String("undefined".to_string())
        );
        assert_ne!(
            engine
                .execute_expression("inline.tjs", "typeof TemporaryFiles")
                .expect("TemporaryFiles probe"),
            Variant::String("undefined".to_string())
        );
        for (name, value) in [
            ("FILE_ATTRIBUTE_READONLY", 0x1),
            ("FILE_ATTRIBUTE_HIDDEN", 0x2),
            ("FILE_ATTRIBUTE_SYSTEM", 0x4),
            ("FILE_ATTRIBUTE_DIRECTORY", 0x10),
            ("FILE_ATTRIBUTE_ARCHIVE", 0x20),
            ("FILE_ATTRIBUTE_NORMAL", 0x80),
            ("FILE_ATTRIBUTE_TEMPORARY", 0x100),
        ] {
            assert_eq!(
                engine
                    .execute_expression("inline.tjs", name)
                    .expect("attribute global"),
                Variant::Integer(value),
                "{name} has the wrong value"
            );
        }
    }

    #[test]
    fn fstat_reports_size_and_dates_for_a_real_file() {
        let root = test_root("fstat-size");
        fs::write(root.join("probe.txt"), b"kirakira").expect("write probe");
        fs::create_dir_all(root.join("folder")).expect("create directory");

        let mut engine = test_engine(&root);
        engine.register_plugin(FstatPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var info = Storages.fstat(\"probe.txt\");\n\
                     var stamp = info.mtime;\n\
                     return info.size + \":\" + (typeof stamp) + \":\" +\n\
                         (stamp.getTime() > 0);\n\
                 })()",
            )
            .expect("fstat a real file");

        assert_eq!(value, Variant::String("8:Object:1".to_string()));

        // A directory answers with the timestamps and no `size`, which is what
        // the reference's `getFileTime` does for directory handles.
        fs::write(root.join("folder/inner.txt"), b"inner").expect("write inner file");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var info = Storages.fstat(\"folder/\");\n\
                     return (typeof info.size) + \":\" + (typeof info.mtime) + \":\" +\n\
                         (info.mtime.getTime() > 0);\n\
                 })()",
            )
            .expect("fstat a directory");

        assert_eq!(value, Variant::String("undefined:Object:1".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn fstat_on_a_memory_member_answers_size_only() {
        // A memory-backed storage has no local file behind it, which is the
        // engine's equivalent of the reference's in-archive branch: the stat
        // carries size and no timestamps.
        let storage = ProjectStorage::from_memory([("member.txt", b"12345".to_vec())]);
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(FstatPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "Storages.fstat(\"member.txt\").size + \":\" + \
                 (typeof Storages.fstat(\"member.txt\").mtime)",
            )
            .expect("fstat a memory member");

        assert_eq!(value, Variant::String("5:undefined".to_string()));
    }

    #[test]
    fn fstat_on_a_missing_name_reports_the_reference_message() {
        let root = test_root("fstat-missing");
        let mut engine = test_engine(&root);
        engine.register_plugin(FstatPlugin).expect("plugin");
        let error = engine
            .execute_expression("inline.tjs", "Storages.fstat(\"absent.txt\")")
            .expect_err("a missing storage must fail");

        assert!(
            error.message.contains("cannot open : absent.txt"),
            "unexpected message: {}",
            error.message
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn fstat_without_arguments_reports_the_argument_count_error() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(FstatPlugin).expect("plugin");
        let error = engine
            .execute_expression("inline.tjs", "Storages.fstat()")
            .expect_err("a missing argument must fail");

        assert_eq!(error.message, "Invalid argument count");
    }

    #[test]
    fn dirlist_requires_a_trailing_slash_and_lists_entries() {
        let root = test_root("fstat-dirlist");
        fs::create_dir_all(root.join("list")).expect("create directory");
        fs::write(root.join("list/alpha.txt"), b"a").expect("write file");
        fs::create_dir_all(root.join("list/beta")).expect("create nested directory");

        let mut engine = test_engine(&root);
        engine.register_plugin(FstatPlugin).expect("plugin");
        let error = engine
            .execute_expression("inline.tjs", "Storages.dirlist(\"list\")")
            .expect_err("a directory without a trailing slash must fail");
        assert_eq!(
            error.message,
            "'/' must be specified at the end of given directory name."
        );

        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var entries = Storages.dirlist(\"list/\");\n\
                     var found = \"\";\n\
                     for (var i = 0; i < entries.count; i++) {\n\
                         if (entries[i] == \"alpha.txt\" || entries[i] == \"beta/\") {\n\
                             found += entries[i] + \",\";\n\
                         }\n\
                     }\n\
                     return found;\n\
                 })()",
            )
            .expect("list a directory");
        assert_eq!(value, Variant::String("alpha.txt,beta/,".to_string()));

        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var entries = Storages.dirlistEx(\"list/\");\n\
                     for (var i = 0; i < entries.count; i++) {\n\
                         if (entries[i].name == \"alpha.txt\") {\n\
                             return entries[i].size + \":\" + (typeof entries[i].mtime) + \":\" +\n\
                                 (entries[i].attrib & 0x10);\n\
                         }\n\
                     }\n\
                     return \"missing\";\n\
                 })()",
            )
            .expect("list with details");
        assert_eq!(value, Variant::String("1:Object:0".to_string()));

        let error = engine
            .execute_expression("inline.tjs", "Storages.dirlist(\"absent/\")")
            .expect_err("a missing directory must fail");
        assert_eq!(error.message, "Directory not found.");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn get_md5_hash_string_matches_a_known_digest() {
        let root = test_root("fstat-md5");
        fs::write(root.join("probe.txt"), b"abc").expect("write probe");

        let mut engine = test_engine(&root);
        engine.register_plugin(FstatPlugin).expect("plugin");
        let value = engine
            .execute_expression("inline.tjs", "Storages.getMD5HashString(\"probe.txt\")")
            .expect("digest a storage");

        assert_eq!(
            value,
            Variant::String("900150983cd24fb0d6963f7d28e17f72".to_string())
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn copy_file_round_trips_the_bytes() {
        let root = test_root("fstat-copy");
        fs::write(root.join("probe.txt"), b"payload").expect("write probe");

        let mut engine = test_engine(&root);
        engine.register_plugin(FstatPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var copied = Storages.copyFile(\"probe.txt\", \"copy.txt\", true);\n\
                     var blocked = Storages.copyFile(\"probe.txt\", \"copy.txt\", true);\n\
                     return copied + \":\" + blocked + \":\" +\n\
                         Storages.fstat(\"copy.txt\").size + \":\" +\n\
                         Storages.getMD5HashString(\"copy.txt\");\n\
                 })()",
            )
            .expect("copy a storage");

        assert_eq!(
            value,
            Variant::String("1:0:7:321c3cf486ed509164edec1e1981fec8".to_string())
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn temporary_name_is_unique_and_creates_no_file() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(FstatPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var first = Storages.getTemporaryName();\n\
                     var second = Storages.getTemporaryName();\n\
                     return first + \"|\" + second;\n\
                 })()",
            )
            .expect("temporary names");

        let Variant::String(names) = &value else {
            panic!("unexpected value: {value:?}");
        };
        let (first, second) = names.split_once('|').expect("two names");
        assert_ne!(first, second, "temporary names must differ");
        assert!(first.contains("krkr_"), "unexpected name: {first}");
        // `TVPGetTemporaryName` never touches the filesystem.
        assert!(
            !std::path::Path::new(first).exists(),
            "getTemporaryName created {first}"
        );
        assert!(!std::path::Path::new(second).exists());
    }

    #[test]
    fn temporary_files_claims_and_releases_files_and_folders() {
        let root = test_root("fstat-temporary");
        fs::write(root.join("probe.txt"), b"temp").expect("write probe");
        fs::create_dir_all(root.join("folder")).expect("create folder");
        fs::write(root.join("folder/inner.txt"), b"inner").expect("write inner");

        let mut engine = test_engine(&root);
        engine.register_plugin(FstatPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var holder = new TemporaryFiles();\n\
                     return holder.entry(\"probe.txt\") + \":\" + holder.entry(\"absent.txt\") + \":\" +\n\
                         holder.entryFolder(\"folder/\") + \":\" + holder.entryFolder(\"probe.txt\");\n\
                 })()",
            )
            .expect("claim temporary paths");

        assert_eq!(value, Variant::String("1:0:1:0".to_string()));
        // Claiming does not delete anything yet, and a holder the script drops
        // without invalidating it leaves its files behind: this engine runs
        // `finalize` for the `invalidate` instruction only, not from a GC.
        assert!(root.join("probe.txt").exists());
        assert!(root.join("folder").exists());

        // `finalize` deletes what the holder claimed; a folder has to be
        // emptied first, exactly as a platform delete-on-close would require.
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var holder = new TemporaryFiles();\n\
                     var claimed = holder.entry(\"probe.txt\") + \":\" + holder.entryFolder(\"folder/\");\n\
                     Storages.deleteFile(\"folder/inner.txt\");\n\
                     holder.finalize();\n\
                     return claimed;\n\
                 })()",
            )
            .expect("finalize the holder");

        assert_eq!(value, Variant::String("1:1".to_string()));
        assert!(
            !root.join("probe.txt").exists(),
            "finalize must delete the claimed file"
        );
        assert!(
            !root.join("folder").exists(),
            "finalize must delete the claimed folder"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn directory_members_require_a_trailing_slash() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(FstatPlugin).expect("plugin");
        for call in [
            "Storages.createDirectory(\"savedata\")",
            "Storages.removeDirectory(\"savedata\")",
            "Storages.changeDirectory(\"savedata\")",
        ] {
            let error = engine
                .execute_expression("inline.tjs", call)
                .expect_err("a directory name without '/' must fail");
            assert_eq!(
                error.message, "'/' must be specified at the end of given directory name.",
                "{call}"
            );
        }
    }

    #[test]
    fn is_existent_directory_accepts_a_slash_less_name() {
        let root = test_root("fstat-is-existent-directory");
        fs::create_dir_all(root.join("folder")).expect("create directory");
        fs::write(root.join("probe.txt"), b"probe").expect("write probe");

        let mut engine = test_engine(&root);
        engine.register_plugin(FstatPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     return Storages.isExistentDirectory(\"folder\") + \":\" +\n\
                         Storages.isExistentDirectory(\"folder/\") + \":\" +\n\
                         Storages.isExistentDirectory(\"probe.txt\") + \":\" +\n\
                         Storages.isExistentDirectory(\"absent\");\n\
                 })()",
            )
            .expect("probe directories");

        // The reference's `getFullPath` keeps the trailing `/` that scripts
        // build their probes from; this engine normalizes it away, so an
        // existing directory must answer 1 either way.
        assert_eq!(value, Variant::String("1:1:0:0".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn set_time_rejects_a_scalar_time_argument() {
        let root = test_root("fstat-set-time");
        fs::write(root.join("probe.txt"), b"stamp").expect("write probe");

        let mut engine = test_engine(&root);
        engine.register_plugin(FstatPlugin).expect("plugin");
        let error = engine
            .execute_expression("inline.tjs", "Storages.setTime(\"probe.txt\", 5)")
            .expect_err("a scalar time argument is not a dictionary");

        assert!(
            error.message.contains("requires a dictionary of times"),
            "unexpected message: {}",
            error.message
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn test_root(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-fstat-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    fn test_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("storage");
        KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine")
    }
}
