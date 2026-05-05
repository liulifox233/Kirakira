use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{first_arg_or_void, install_static_object, register_stub_method};

pub(crate) fn install_system(runtime: &mut Runtime<KrkrHost>) {
    let system = install_static_object(runtime, "System");
    for method in [
        "assignMessage",
        "doCompact",
        "system",
        "readRegValue",
        "setArgument",
        "dumpHeap",
        "nullpo",
        "showVersion",
    ] {
        register_stub_method(runtime, system, "System", method);
    }
    runtime.register_object_native(system, "terminate", system_exit);
    runtime.register_object_native(system, "exit", system_exit);
    runtime.register_object_native(system, "clearGraphicCache", system_clear_graphic_cache);
    runtime.register_object_native(system, "touchImages", system_touch_images);
    runtime.register_object_native(
        system,
        "addContinuousHandler",
        system_add_continuous_handler,
    );
    runtime.register_object_native(
        system,
        "removeContinuousHandler",
        system_remove_continuous_handler,
    );
    runtime.register_object_native(system, "inform", system_inform);
    runtime.register_object_native(system, "getKeyState", system_get_key_state);
    runtime.register_object_native(system, "shellExecute", system_shell_execute);
    runtime.register_object_native(system, "createAppLock", system_create_app_lock);
    runtime.register_object_native(system, "getTickCount", system_get_tick_count);
    runtime.register_object_native(system, "toActualColor", first_arg_or_void);
    runtime.register_object_native(system, "createUUID", system_create_uuid);
    runtime.register_object_native(system, "getArgument", system_get_argument);
    runtime.register_object_native(system, "addFont", system_add_font);

    for (name, value) in [
        ("versionString", Variant::String("Kirakira".to_string())),
        ("platformName", Variant::String("Kirakira".to_string())),
        ("osName", Variant::String(std::env::consts::OS.to_string())),
        ("exePath", Variant::String(exe_path(runtime))),
        ("dataPath", Variant::String(data_path(runtime))),
        ("personalPath", Variant::String(temp_path())),
        ("appDataPath", Variant::String(temp_path())),
        ("eventDisabled", Variant::Integer(0)),
        ("graphicCacheLimit", Variant::Integer(0)),
        ("exitOnWindowClose", Variant::Integer(1)),
        ("drawThreadNum", Variant::Integer(0)),
        ("processorNum", Variant::Integer(1)),
        ("exeBits", Variant::Integer((usize::BITS) as i64)),
        ("osBits", Variant::Integer((usize::BITS) as i64)),
        ("exitOnNoWindowStartup", Variant::Integer(0)),
        ("title", Variant::String("Kirakira".to_string())),
        ("screenWidth", Variant::Integer(960)),
        ("screenHeight", Variant::Integer(600)),
        ("desktopLeft", Variant::Integer(0)),
        ("desktopTop", Variant::Integer(0)),
        ("desktopWidth", Variant::Integer(960)),
        ("desktopHeight", Variant::Integer(600)),
        ("touchDevice", Variant::Integer(0)),
    ] {
        runtime.set_object_member(system, name, value);
    }
    let version_info = runtime.alloc_ordinary_object();
    runtime.set_object_member(system, "versionInformation", Variant::Object(version_info));
}

fn exe_path(runtime: &Runtime<KrkrHost>) -> String {
    runtime
        .host()
        .project_root()
        .map(|path| format!("{}/", path.display()))
        .unwrap_or_default()
}

fn data_path(runtime: &Runtime<KrkrHost>) -> String {
    runtime
        .host()
        .data_path()
        .map(|path| format!("{}/", path.display()))
        .unwrap_or_else(temp_path)
}

fn temp_path() -> String {
    format!("{}/", std::env::temp_dir().display())
}

fn system_exit(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    runtime.host_mut().request_termination();
    Ok(Variant::Void)
}

fn system_clear_graphic_cache(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    runtime.host_mut().clear_graphic_cache();
    Ok(Variant::Void)
}

fn system_touch_images(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let source = args
        .first()
        .ok_or_else(|| TjsError::runtime("System.touchImages requires a storage array"))?;
    let storages = touch_image_storages(runtime, source)?;
    let limit = args
        .get(1)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0);
    let timeout_ms = args
        .get(2)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0)
        .max(0) as u64;
    runtime
        .host_mut()
        .touch_images(&storages, limit, timeout_ms);
    Ok(Variant::Void)
}

fn touch_image_storages(runtime: &Runtime<KrkrHost>, source: &Variant) -> Result<Vec<String>> {
    if let Variant::Object(array) = source {
        let count = runtime
            .object_member(*array, "count")
            .to_integer()
            .unwrap_or(0)
            .max(0);
        let mut storages = Vec::with_capacity(count as usize);
        for index in 0..count {
            let value = runtime.object_member(*array, &index.to_string());
            if matches!(value, Variant::Void) {
                break;
            }
            storages.push(value.to_tjs_string()?);
        }
        return Ok(storages);
    }

    Ok(vec![source.to_tjs_string()?])
}

fn system_add_font(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(storage) = args.first() else {
        return Err(TjsError::runtime("System.addFont requires a storage name"));
    };
    let storage = storage.to_tjs_string()?;
    let Ok(data) = runtime.host().read_resource_storage(&storage) else {
        return Ok(Variant::Void);
    };
    let bytes = data.to_arc_bytes().map_err(crate::storage::io_error)?;
    let loaded = runtime
        .host_mut()
        .font_system_mut()
        .load_font_data(storage.clone(), bytes.as_ref().to_vec())
        .is_ok();
    if loaded {
        runtime
            .host_mut()
            .log(&format!("System.addFont loaded: {storage}"));
    }
    Ok(Variant::Integer(i64::from(loaded)))
}

fn system_add_continuous_handler(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(handler) = args.first() {
        runtime.host_mut().add_continuous_handler(handler.clone());
    }
    Ok(Variant::Void)
}

fn system_remove_continuous_handler(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let removed = args
        .first()
        .is_some_and(|handler| runtime.host_mut().remove_continuous_handler(handler));
    Ok(Variant::Integer(i64::from(removed)))
}

fn system_inform(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let message = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    runtime.host_mut().log(&format!("System.inform: {message}"));
    Ok(Variant::Void)
}

fn system_get_key_state(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(key) = args.first() else {
        return Ok(Variant::Integer(0));
    };
    let key = key.to_integer()?;
    Ok(Variant::Integer(i64::from(runtime.host().key_state(key))))
}

fn system_shell_execute(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let target = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    runtime
        .host_mut()
        .log(&format!("System.shellExecute ignored: {target}"));
    Ok(Variant::Integer(0))
}

fn system_create_app_lock(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    runtime
        .host_mut()
        .log(&format!("System.createAppLock granted: {name}"));
    Ok(Variant::Integer(1))
}

fn system_get_tick_count(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(runtime.host_mut().now_millis()))
}

fn system_create_uuid(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let ticks = runtime.host_mut().now_millis();
    Ok(Variant::String(format!(
        "00000000-0000-4000-8000-{ticks:012x}"
    )))
}

fn system_get_argument(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}
