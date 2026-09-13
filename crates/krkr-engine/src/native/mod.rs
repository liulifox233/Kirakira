mod blend;
pub(crate) mod classes;
mod clipboard;
mod debug;
mod kag;
mod plugins;
mod scripts;
mod storages;
mod system;
pub(crate) mod video;

pub(crate) use classes::{
    ASYNC_TRIGGER_CLASS, BASIC_DRAW_DEVICE_CLASS, BITMAP_CLASS, BITMAP_LAYER_TREE_OWNER_CLASS,
    FONT_CLASS, IMAGE_FUNCTION_CLASS, LAYER_CLASS, MENU_ITEM_CLASS, PHASE_VOCODER_CLASS,
    RECT_CLASS, TIMER_CLASS, VIDEO_OVERLAY_CLASS, WAVE_SOUND_BUFFER_CLASS, WINDOW_CLASS,
    install_native_class,
};
pub(crate) use clipboard::install_clipboard;
pub(crate) use debug::install_debug;
pub(crate) use kag::{
    create_kag_parser_object, install_kag_parser, kag_to_tjs, refresh_kag_parser_object,
};
pub(crate) use plugins::install_plugins;
pub(crate) use scripts::install_scripts;
pub(crate) use storages::install_storages;
pub(crate) use system::install_system;
pub(crate) use video::{tick_video_overlays, video_overlay_frame_quads};

use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

/// Publishes one of the engine's static objects (`Debug`, `System`,
/// `Storages`, `Plugins`, `Clipboard`): a plain object that carries only its
/// class-info name, the shape the engine's plugins are written against.
fn install_static_object(runtime: &mut Runtime<KrkrHost>, name: &str) -> ObjectHandle {
    let handle = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(handle, name);
    runtime.set_global_member(name, Variant::Object(handle));
    handle
}

/// Publishes `Scripts` as the native *class* object the reference makes it.
///
/// krkr registers every native class with `val = tTJSVariant(dsp/*, dsp*/)`
/// (`REGISTER_OBJECT(Scripts, TVPCreateNativeClass_Scripts())`,
/// `base/ScriptMgnIntf.cpp:496-510`): the ObjThis operand is commented out, so
/// the class object carries no `this` of its own and `CallFunctionDirect`
/// falls through to `ra[-1]`, the caller's `this`
/// (`tjsInterCodeExec.cpp:2434`).  `Scripts.foreach` needs exactly that:
/// `scriptsEx` runs an unbound callback in the *calling* context
/// (`functhis = funcClosure.ObjThis; if (functhis == 0) functhis = objthis;`,
/// krkr2 `src/plugins/win32/scriptsEx/Main.cpp:491-493`), and KAGEnv hands it
/// literals created inside its own methods — KAGEnvironment's
/// `entryUpdateAll` calls `Scripts_foreach(objects, function(a0,a1){
/// entryUpdate(a1); })` (`system/KAGEnvironment.tjs`), and when the callback
/// cannot reach `entryUpdate` the conductor stops at 少女世界's
/// `[endtrans notrans sync]` (`custom.ks:49`).
///
/// A constructable native function is this VM's model of a native class
/// object, so allocating one reproduces it: the object is still a plain member
/// of the global table, but a member call through it no longer turns the
/// object itself into the callee's receiver (`receiver_supplies_call_this`,
/// `krkr-tjs2`'s `vm/dispatch.rs`).  Constructing one answers void, matching
/// the reference's `TJS_BEGIN_NATIVE_CONSTRUCTOR_DECL_NO_INSTANCE`.
///
/// The other five static objects share the reference's class-object shape but
/// keep today's plain-object one: `window_ex`'s `getSystemMetrics` reads the
/// geometry off its receiver, where the reference resolution goes through the
/// global `System` table (`main.cpp:1919-1974`) — widening the rule to them is
/// a separate change (finding 20260914).
fn install_static_class_object(runtime: &mut Runtime<KrkrHost>, name: &str) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |_runtime: &mut Runtime<KrkrHost>, _this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            Ok(Variant::Void)
        },
    );
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
    register_stub_method_with_arg_count(runtime, handle, class_name, method, 0);
}

/// [`register_stub_method`] with the member's reference `numparams` floor
/// (`0` when the reference declares none): the declaration-level check answers
/// `TJS_E_BADPARAMCOUNT` before the stub body runs, the order the official
/// natives validate in.
fn register_stub_method_with_arg_count(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &'static str,
    method: &'static str,
    min_args: usize,
) {
    let arg_count = if min_args == 0 {
        NativeArgCount::Any
    } else {
        NativeArgCount::AtLeast(min_args)
    };
    runtime.register_object_native_with_arg_count(
        handle,
        method,
        arg_count,
        move |runtime: &mut Runtime<KrkrHost>,
              _this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            let summary = summarize_stub_args(&args);
            runtime
                .host_mut()
                .record_stub_call(class_name, method, &summary);
            Ok(Variant::Void)
        },
    );
}

/// Short human-readable summary of stub call arguments for the first-call
/// warning — storage names are the most useful clue when deciding which
/// stub a game actually depends on.
fn summarize_stub_args(args: &[Variant]) -> String {
    const MAX_ARGS: usize = 3;
    const MAX_STRING_CHARS: usize = 60;
    let mut parts: Vec<String> = args
        .iter()
        .take(MAX_ARGS)
        .map(|arg| match arg {
            Variant::String(text) => {
                if text.chars().count() > MAX_STRING_CHARS {
                    let head: String = text.chars().take(MAX_STRING_CHARS).collect();
                    format!("\"{head}...\"")
                } else {
                    format!("\"{text}\"")
                }
            }
            Variant::Integer(value) => value.to_string(),
            Variant::Real(value) => value.to_string(),
            Variant::Null => "null".to_string(),
            Variant::Void => "void".to_string(),
            Variant::Octet(bytes) => format!("<octet {}B>", bytes.len()),
            Variant::Object(_) => "<object>".to_string(),
            Variant::Closure(_) => "<closure>".to_string(),
            Variant::CodeObject(_) => "<code>".to_string(),
        })
        .collect();
    if args.len() > MAX_ARGS {
        parts.push("...".to_string());
    }
    parts.join(", ")
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
