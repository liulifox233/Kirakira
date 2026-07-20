use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{install_static_object, native_void};

pub(crate) fn install_debug(runtime: &mut Runtime<KrkrHost>) {
    let debug = install_static_object(runtime, "Debug");
    runtime.register_object_native(debug, "message", debug_message);
    runtime.register_object_native(debug, "notice", debug_message);
    runtime.register_object_native(debug, "startLogToFile", native_void);
    runtime.register_object_native(debug, "logAsError", debug_message);
    runtime.register_object_native(debug, "addLoggingHandler", native_void);
    runtime.register_object_native(debug, "removeLoggingHandler", native_void);
    runtime.register_object_native(debug, "getLastLog", debug_get_last_log);
    runtime.set_object_member(debug, "logLocation", Variant::String(String::new()));
    runtime.set_object_member(debug, "logToFileOnError", Variant::Integer(0));
    runtime.set_object_member(debug, "clearLogFileOnError", Variant::Integer(0));
}

fn debug_message(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let message = args
        .iter()
        .map(Variant::to_tjs_string)
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    runtime.host_mut().log(&message);
    Ok(Variant::Void)
}

fn debug_get_last_log(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        runtime.host().logs().last().cloned().unwrap_or_default(),
    ))
}
