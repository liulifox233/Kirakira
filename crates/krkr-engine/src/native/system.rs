use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use crate::host::KrkrHost;

use super::{install_static_object, register_stub_method};

static UUID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(crate) fn install_system(runtime: &mut Runtime<KrkrHost>) {
    let system = install_static_object(runtime, "System");
    for method in [
        // Implemented below with a portable message table.
        // addFont is implemented below; keep the remaining legacy methods as
        // observable stubs for compatibility diagnostics.
        "system",
        "readRegValue",
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
    runtime.register_object_native(system, "toActualColor", system_to_actual_color);
    runtime.register_object_native(system, "assignMessage", system_assign_message);
    runtime.register_object_native(system, "doCompact", system_do_compact);
    runtime.register_object_native(system, "setArgument", system_set_argument);
    runtime.register_object_native(system, "createUUID", system_create_uuid);
    runtime.register_object_native(system, "getArgument", system_get_argument);
    runtime.register_object_native(system, "addFont", system_add_font);

    for (name, value) in [
        ("versionString", Variant::String("Kirakira".to_string())),
        ("platformName", Variant::String("Kirakira".to_string())),
        ("osName", Variant::String(std::env::consts::OS.to_string())),
        (
            "exePath",
            Variant::String(runtime.host().system_paths().exe_path.clone()),
        ),
        // `TVPNormalizeStorageName(ParamStr(0))`: the full path of the running
        // program, not just its directory.  Restart helpers pass it straight to
        // `Storages.getLocalName`, so leaving it undefined turns a reboot into a
        // "requires a storage name" throw.
        (
            "exeName",
            Variant::String(
                std::env::current_exe()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| runtime.host().system_paths().exe_path.clone()),
            ),
        ),
        (
            "dataPath",
            Variant::String(runtime.host().system_paths().data_path.clone()),
        ),
        (
            "personalPath",
            Variant::String(runtime.host().system_paths().personal_path.clone()),
        ),
        (
            "appDataPath",
            Variant::String(runtime.host().system_paths().app_data_path.clone()),
        ),
        // KRKR invokes this callback at the outer script/event boundary when
        // an exception escaped the VM.  Startup.tjs may replace the default
        // void value with the project's handler.
        ("exceptionHandler", Variant::Void),
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
    runtime.register_object_native_property(
        system,
        "eventDisabled",
        system_event_disabled_get,
        system_event_disabled_set,
    );
    let version_info = runtime.alloc_ordinary_object();
    runtime.set_object_member(system, "versionInformation", Variant::Object(version_info));
}

fn system_event_disabled_get(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    Ok(Variant::Integer(i64::from(
        runtime.host().scheduler().event_disabled(),
    )))
}

fn system_event_disabled_set(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    value: Variant,
) -> Result<()> {
    runtime
        .host_mut()
        .scheduler_mut()
        .set_event_disabled(value.is_truthy());
    Ok(())
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

fn system_add_continuous_handler(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handler = args
        .first()
        .ok_or_else(|| TjsError::runtime("System.addContinuousHandler requires a handler"))?;
    if !matches!(handler, Variant::Object(_) | Variant::Closure(_)) {
        return Err(TjsError::runtime(
            "System.addContinuousHandler requires an object closure",
        ));
    }
    runtime.host_mut().add_continuous_handler(handler.clone());
    Ok(Variant::Void)
}

fn system_remove_continuous_handler(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handler = args
        .first()
        .ok_or_else(|| TjsError::runtime("System.removeContinuousHandler requires a handler"))?;
    if !matches!(handler, Variant::Object(_) | Variant::Closure(_)) {
        return Err(TjsError::runtime(
            "System.removeContinuousHandler requires an object closure",
        ));
    }
    let removed = runtime.host_mut().remove_continuous_handler(handler);
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
    Ok(Variant::Integer(runtime.host().tick_count_millis()))
}

fn system_to_actual_color(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let color = args
        .first()
        .ok_or_else(|| TjsError::runtime("System.toActualColor requires a color"))?
        .to_integer()? as u32;
    Ok(Variant::Integer(super::classes::to_actual_color(
        color as i64,
    )))
}

fn system_assign_message(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let id = args
        .first()
        .ok_or_else(|| TjsError::runtime("System.assignMessage requires an id"))?
        .to_tjs_string()?;
    let message = args
        .get(1)
        .ok_or_else(|| TjsError::runtime("System.assignMessage requires a message"))?
        .to_tjs_string()?;
    Ok(Variant::Integer(i64::from(
        runtime.host_mut().assign_system_message(&id, &message),
    )))
}

fn system_do_compact(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    runtime.host().clear_archive_cache()?;
    Ok(Variant::Void)
}

fn system_set_argument(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .ok_or_else(|| TjsError::runtime("System.setArgument requires a name"))?
        .to_tjs_string()?;
    let value = args
        .get(1)
        .ok_or_else(|| TjsError::runtime("System.setArgument requires a value"))?
        .to_tjs_string()?;
    runtime.host_mut().set_command_argument(&name, &value);
    Ok(Variant::Void)
}

fn system_create_uuid(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let ticks = runtime.host_mut().now_millis() as u64;
    #[cfg(target_arch = "wasm32")]
    let nanos = ticks;
    #[cfg(not(target_arch = "wasm32"))]
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(ticks);
    let mut state = nanos ^ ticks.rotate_left(17) ^ UUID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_exact_mut(8) {
        state ^= state << 7;
        state ^= state >> 9;
        state ^= state << 8;
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Variant::String(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )))
}

fn system_get_argument(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .ok_or_else(|| TjsError::runtime("System.getArgument requires a name"))?
        .to_tjs_string()?;
    Ok(runtime
        .host()
        .command_argument(&name)
        .map(Variant::String)
        .unwrap_or(Variant::Void))
}

fn system_add_font(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_else(|| "web-font".to_string());
    let bytes = match args.get(1) {
        Some(Variant::Octet(bytes)) => bytes.clone(),
        Some(value) => value.to_tjs_string()?.into_bytes(),
        None => Vec::new(),
    };
    if bytes.is_empty() {
        return Ok(Variant::Integer(0));
    }
    runtime
        .host_mut()
        .font_system_mut()
        .load_font_data(name, bytes)
        .map(|_| Variant::Integer(1))
        .map_err(TjsError::runtime)
}
