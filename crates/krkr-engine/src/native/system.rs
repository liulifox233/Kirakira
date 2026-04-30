use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{first_arg_or_void, install_static_object, register_stub_method};

pub(crate) fn install_system(runtime: &mut Runtime<KrkrHost>) {
    let system = install_static_object(runtime, "System");
    for method in [
        "terminate",
        "exit",
        "addContinuousHandler",
        "removeContinuousHandler",
        "clearGraphicCache",
        "touchImages",
        "assignMessage",
        "doCompact",
        "inform",
        "getKeyState",
        "shellExecute",
        "system",
        "readRegValue",
        "setArgument",
        "createAppLock",
        "dumpHeap",
        "nullpo",
        "showVersion",
    ] {
        register_stub_method(runtime, system, "System", method);
    }
    runtime.register_object_native(system, "getTickCount", system_get_tick_count);
    runtime.register_object_native(system, "toActualColor", first_arg_or_void);
    runtime.register_object_native(system, "createUUID", system_create_uuid);
    runtime.register_object_native(system, "getArgument", system_get_argument);

    for (name, value) in [
        ("versionString", Variant::String("krkr-ruri".to_string())),
        ("platformName", Variant::String("krkr-ruri".to_string())),
        ("osName", Variant::String(std::env::consts::OS.to_string())),
        ("exePath", Variant::String(exe_path(runtime))),
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
        ("title", Variant::String("krkr-ruri".to_string())),
        ("screenWidth", Variant::Integer(0)),
        ("screenHeight", Variant::Integer(0)),
        ("desktopLeft", Variant::Integer(0)),
        ("desktopTop", Variant::Integer(0)),
        ("desktopWidth", Variant::Integer(0)),
        ("desktopHeight", Variant::Integer(0)),
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

fn temp_path() -> String {
    format!("{}/", std::env::temp_dir().display())
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
