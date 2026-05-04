pub(crate) mod classes;
mod clipboard;
mod debug;
mod kag;
mod plugins;
mod scripts;
mod storages;
mod system;

pub(crate) use classes::{
    ASYNC_TRIGGER_CLASS, BASIC_DRAW_DEVICE_CLASS, BITMAP_CLASS, BITMAP_LAYER_TREE_OWNER_CLASS,
    FONT_CLASS, IMAGE_FUNCTION_CLASS, LAYER_CLASS, MENU_ITEM_CLASS, PHASE_VOCODER_CLASS,
    RECT_CLASS, TIMER_CLASS, VIDEO_OVERLAY_CLASS, WAVE_SOUND_BUFFER_CLASS, WINDOW_CLASS,
    install_native_class,
};
pub(crate) use clipboard::install_clipboard;
pub(crate) use debug::install_debug;
pub(crate) use kag::{create_kag_parser_object, install_kag_parser, refresh_kag_parser_object};
pub(crate) use plugins::install_plugins;
pub(crate) use scripts::install_scripts;
pub(crate) use storages::install_storages;
pub(crate) use system::install_system;

use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

fn install_static_object(runtime: &mut Runtime<KrkrHost>, name: &str) -> ObjectHandle {
    let handle = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(handle, name);
    runtime.set_global_member(name, Variant::Object(handle));
    handle
}

fn register_stub_method(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &'static str,
    method: &'static str,
) {
    runtime.register_object_native(
        handle,
        method,
        move |runtime: &mut Runtime<KrkrHost>,
              _this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            runtime.host_mut().log(&format!(
                "{class_name}.{method} is registered as a runtime stub"
            ));
            Ok(Variant::Void)
        },
    );
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn first_arg_or_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(args.into_iter().next().unwrap_or_default())
}

fn arg_string(args: &[Variant], index: usize) -> Result<Option<String>> {
    args.get(index)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()
}

fn required_arg_string(args: &[Variant], index: usize, method: &str) -> Result<String> {
    arg_string(args, index)?
        .ok_or_else(|| TjsError::runtime(format!("{method} requires argument {index}")))
}
