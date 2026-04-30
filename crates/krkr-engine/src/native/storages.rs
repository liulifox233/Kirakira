use std::path::{Path, PathBuf};

use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{arg_string, install_static_object, native_void};

pub(crate) fn install_storages(runtime: &mut Runtime<KrkrHost>) {
    let storages = install_static_object(runtime, "Storages");
    runtime.register_object_native(storages, "addAutoPath", storages_add_auto_path);
    runtime.register_object_native(storages, "removeAutoPath", storages_remove_auto_path);
    runtime.register_object_native(storages, "getFullPath", storages_get_full_path);
    runtime.register_object_native(storages, "getPlacedPath", storages_get_placed_path);
    runtime.register_object_native(storages, "isExistentStorage", storages_exists);
    runtime.register_object_native(storages, "extractStorageExt", storages_extract_ext);
    runtime.register_object_native(storages, "extractStorageName", storages_extract_name);
    runtime.register_object_native(storages, "extractStoragePath", storages_extract_path);
    runtime.register_object_native(storages, "chopStorageExt", storages_chop_ext);
    runtime.register_object_native(storages, "clearArchiveCache", storages_clear_archive_cache);
    runtime.register_object_native(storages, "getLocalName", storages_get_placed_path);
    runtime.register_object_native(storages, "selectFile", native_void);
    runtime.register_object_native(storages, "searchCD", native_void);
}

fn storages_add_auto_path(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(value) = args.first() {
        runtime.host_mut().add_auto_path(value.to_tjs_string()?);
    }
    Ok(Variant::Void)
}

fn storages_remove_auto_path(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let removed = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .is_some_and(|path| runtime.host_mut().remove_auto_path(&path));
    Ok(Variant::Integer(i64::from(removed)))
}

fn storages_get_full_path(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(arg_string(&args, 0)?.unwrap_or_default()))
}

fn storages_get_placed_path(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(name) = arg_string(&args, 0)? else {
        return Ok(Variant::String(String::new()));
    };
    let path = runtime
        .host()
        .placed_path(&name)
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    Ok(Variant::String(path))
}

fn storages_exists(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let exists = arg_string(&args, 0)?.is_some_and(|name| runtime.host().storage_exists(&name));
    Ok(Variant::Integer(i64::from(exists)))
}

fn storages_extract_ext(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let ext = arg_string(&args, 0)?
        .and_then(|value| {
            Path::new(&value)
                .extension()
                .map(|ext| ext.to_string_lossy().into())
        })
        .unwrap_or_default();
    Ok(Variant::String(ext))
}

fn storages_extract_name(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = arg_string(&args, 0)?
        .and_then(|value| {
            Path::new(&value)
                .file_name()
                .map(|name| name.to_string_lossy().into())
        })
        .unwrap_or_default();
    Ok(Variant::String(name))
}

fn storages_extract_path(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let path = arg_string(&args, 0)?
        .and_then(|value| {
            Path::new(&value)
                .parent()
                .map(|path| path.to_string_lossy().into())
        })
        .unwrap_or_default();
    Ok(Variant::String(path))
}

fn storages_chop_ext(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let value = arg_string(&args, 0)?.unwrap_or_default();
    let path = Path::new(&value);
    let chopped = path
        .file_stem()
        .map(|stem| {
            path.parent()
                .map(|parent| parent.join(stem))
                .unwrap_or_else(|| PathBuf::from(stem))
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or(value);
    Ok(Variant::String(chopped))
}

fn storages_clear_archive_cache(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    runtime.host().clear_archive_cache()?;
    Ok(Variant::Void)
}
