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
    runtime.register_object_native(storages, "setTextEncoding", storages_set_text_encoding);
    runtime.register_object_native(storages, "getFullPath", storages_get_full_path);
    runtime.register_object_native(storages, "getPlacedPath", storages_get_placed_path);
    runtime.register_object_native(storages, "isExistentStorage", storages_exists);
    runtime.register_object_native(storages, "isExistentDirectory", storages_is_directory);
    runtime.register_object_native(storages, "dirlist", storages_dirlist);
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

fn storages_set_text_encoding(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(encoding) = arg_string(&args, 0)? {
        runtime.host_mut().set_text_encoding(encoding.clone());
        if let Variant::Object(scripts) = runtime.global_member("Scripts") {
            runtime.set_object_member(scripts, "textEncoding", Variant::String(encoding));
        }
    }
    Ok(Variant::Void)
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
    let exists =
        arg_string(&args, 0)?.is_some_and(|name| runtime.host().storage_exists_exact(&name));
    Ok(Variant::Integer(i64::from(exists)))
}

fn storages_is_directory(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let exists =
        arg_string(&args, 0)?.is_some_and(|name| runtime.host().storage_is_directory(&name));
    Ok(Variant::Integer(i64::from(exists)))
}

fn storages_dirlist(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(name) = arg_string(&args, 0)? else {
        return Err(krkr_tjs2::TjsError::runtime(
            "Storages.dirlist requires a directory",
        ));
    };
    let entries = runtime.host().storage_dirlist(&name)?;
    let values = entries.into_iter().map(Variant::String).collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

fn storages_extract_ext(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        arg_string(&args, 0)?
            .map(|value| extract_storage_ext(&value))
            .unwrap_or_default(),
    ))
}

fn storages_extract_name(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        arg_string(&args, 0)?
            .map(|value| extract_storage_name(&value))
            .unwrap_or_default(),
    ))
}

fn storages_extract_path(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        arg_string(&args, 0)?
            .map(|value| extract_storage_path(&value))
            .unwrap_or_default(),
    ))
}

fn storages_chop_ext(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        arg_string(&args, 0)?
            .map(|value| chop_storage_ext(&value))
            .unwrap_or_default(),
    ))
}

fn storages_clear_archive_cache(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    runtime.host().clear_archive_cache()?;
    Ok(Variant::Void)
}

fn storage_delimiter_index(name: &str) -> Option<usize> {
    name.rfind(['/', '\\', '>'])
}

fn storage_file_start(name: &str) -> usize {
    storage_delimiter_index(name)
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn storage_extension_index(name: &str) -> Option<usize> {
    let start = storage_file_start(name);
    name[start..].rfind('.').map(|index| start + index)
}

fn extract_storage_ext(name: &str) -> String {
    storage_extension_index(name)
        .map(|index| name[index..].to_string())
        .unwrap_or_default()
}

fn extract_storage_name(name: &str) -> String {
    name[storage_file_start(name)..].to_string()
}

fn extract_storage_path(name: &str) -> String {
    storage_delimiter_index(name)
        .map(|index| name[..=index].to_string())
        .unwrap_or_default()
}

fn chop_storage_ext(name: &str) -> String {
    storage_extension_index(name)
        .map(|index| name[..index].to_string())
        .unwrap_or_else(|| name.to_string())
}
