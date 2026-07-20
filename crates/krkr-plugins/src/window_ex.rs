//! No-op stub of wtnbgo's windowEx.dll (windowEx/main.cpp).
//!
//! Attaches the windowEx member surface onto the engine's `Window`, `MenuItem`,
//! `Pad`, `Debug.console`, `System`, and `Scripts` objects so scripts that
//! probe or call these members keep running. Every member is a surface-
//! compatible placeholder: no Win32 window manipulation is performed, and
//! query-style members return conservative defaults.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

pub struct WindowExPlugin;

impl KrkrPlugin for WindowExPlugin {
    fn name(&self) -> &str {
        "windowEx.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_window_ex(runtime);
        install_menu_item_ex(runtime);
        install_pad_ex(runtime);
        install_console_ex(runtime);
        install_system_ex(runtime);
        install_scripts_ex(runtime);
        Ok(())
    }
}

// WindowEx — NCB_ATTACH_CLASS_WITH_HOOK(WindowEx, Window)
fn install_window_ex(runtime: &mut Runtime<KrkrHost>) {
    let Variant::Object(window) = runtime.global_member("Window") else {
        return;
    };

    // HT* hit-test constants.
    for (name, value) in [
        ("nchtError", 65534),       // HTERROR & 0xFFFF
        ("nchtTransparent", 65535), // HTTRANSPARENT & 0xFFFF
        ("nchtNoWhere", 0),
        ("nchtClient", 1),
        ("nchtCaption", 2),
        ("nchtSysMenu", 3),
        ("nchtSize", 4),
        ("nchtGrowBox", 4),
        ("nchtMenu", 5),
        ("nchtHScroll", 6),
        ("nchtVScroll", 7),
        ("nchtMinButton", 8),
        ("nchtReduce", 8),
        ("nchtMaxButton", 9),
        ("nchtZoom", 9),
        ("nchtLeft", 10),
        ("nchtRight", 11),
        ("nchtTop", 12),
        ("nchtTopLeft", 13),
        ("nchtTopRight", 14),
        ("nchtBottom", 15),
        ("nchtBottomLeft", 16),
        ("nchtBottomRight", 17),
        ("nchtBorder", 18),
    ] {
        runtime.set_object_member(window, name, Variant::Integer(value));
    }

    // Read/write properties. In the reference these are backed by Win32 window
    // state; here they are plain members with the same defaults (maximizeBox /
    // minimizeBox default to 1 because a fresh window allows both).
    for (name, value) in [
        ("maximizeBox", 1),
        ("minimizeBox", 1),
        ("maximized", 0),
        ("minimized", 0),
        ("disableResize", 0),
        ("disableMove", 0),
        ("enableNCMouseEvent", 0),
    ] {
        if matches!(runtime.object_member(window, name), Variant::Void) {
            runtime.set_object_member(window, name, Variant::Integer(value));
        }
    }
    if matches!(runtime.object_member(window, "exSystemMenu"), Variant::Void) {
        runtime.set_object_member(window, "exSystemMenu", Variant::Void);
    }

    // Void methods.
    for method in [
        "minimize",
        "maximize",
        "showRestore",
        "focusMenuByKey",
        "resetWindowIcon",
        "setWindowIcon",
        "setOverlayBitmap",
        "resetExSystemMenu",
        "bringTo",
        "sendToBack",
        "registerDeviceChange",
        // Called early by games to enable the extended event dispatch; events
        // are simply never delivered by the stub.
        "registerExEvent",
    ] {
        register_unless_closure(runtime, window, method, native_void);
    }

    // Methods with return values.
    register_unless_closure(runtime, window, "setWindowCornerPreference", one);
    register_unless_closure(runtime, window, "setClientRect", one);
    register_unless_closure(runtime, window, "getWindowRect", empty_rect_dict);
    register_unless_closure(runtime, window, "getClientRect", empty_rect_dict);
    register_unless_closure(runtime, window, "getNormalRect", empty_rect_dict);
    register_unless_closure(runtime, window, "ncHitTest", nc_hit_test);
    register_unless_closure(runtime, window, "setMessageHook", zero);
    register_unless_closure(runtime, window, "registerHotKey", zero);
    register_unless_closure(runtime, window, "acquireImeControl", one);
    register_unless_closure(runtime, window, "resetImeContext", one);
    // The reference keeps notification names in Window._Notifications, filled by
    // registerExEvent dispatch. The stub never dispatches, so that member is not
    // implemented and the queries report "no notifications".
    register_unless_closure(runtime, window, "getNotificationNum", minus_one);
    register_unless_closure(runtime, window, "getNotificationName", empty_string);
}

// MenuItemEx — NCB_ATTACH_CLASS_WITH_HOOK(MenuItemEx, MenuItem) + popupEx.
fn install_menu_item_ex(runtime: &mut Runtime<KrkrHost>) {
    // Mirrors PreRegistCallback: KRKRZ has no MenuItem until menu.dll loads, so
    // a dummy object is registered to carry the extensions.
    let menu_item = match runtime.global_member("MenuItem") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "MenuItem");
            runtime.set_global_member("MenuItem", Variant::Object(handle));
            runtime.host_mut().log(
                "windowEx.dll: MenuItem global is missing; installed a dummy object (loading menu.dll later would shadow these extensions)",
            );
            handle
        }
    };

    // HBMMENU_* bitmap constants.
    for (name, value) in [
        ("biSystem", 1),
        ("biRestore", 2),
        ("biMinimize", 3),
        ("biClose", 5),
        ("biCloseDisabled", 6),
        ("biMinimizeDisabled", 7),
        ("biPopupClose", 8),
        ("biPopupRestore", 9),
        ("biPopupMaximize", 10),
        ("biPopupMinimize", 11),
    ] {
        runtime.set_object_member(menu_item, name, Variant::Integer(value));
    }

    for property in ["rightJustify", "bmpItem", "bmpChecked", "bmpUnchecked"] {
        if matches!(runtime.object_member(menu_item, property), Variant::Void) {
            runtime.set_object_member(menu_item, property, Variant::Integer(0));
        }
    }

    // popupEx(flags, x, y, window, rect, menulist) — the reference returns the
    // selected MenuItem object, or void when the menu is cancelled. The stub
    // always reports a cancellation.
    register_unless_closure(runtime, menu_item, "popupEx", native_void);
}

// PadEx — NCB_ATTACH_CLASS_WITH_HOOK(PadEx, Pad)
fn install_pad_ex(runtime: &mut Runtime<KrkrHost>) {
    // Mirrors PreRegistCallback's dummy Pad for KRKRZ.
    let pad = match runtime.global_member("Pad") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "Pad");
            runtime.set_global_member("Pad", Variant::Object(handle));
            handle
        }
    };
    register_unless_closure(runtime, pad, "registerExEvent", native_void);
}

// ConsoleEx — NCB_ATTACH_FUNCTION_WITHTAG(*, Debug_console, Debug.console, ...)
fn install_console_ex(runtime: &mut Runtime<KrkrHost>) {
    let debug = match runtime.global_member("Debug") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "Debug");
            runtime.set_global_member("Debug", Variant::Object(handle));
            handle
        }
    };
    let console = match runtime.object_member(debug, "console") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "Console");
            runtime.set_object_member(debug, "console", Variant::Object(handle));
            handle
        }
    };

    register_unless_closure(runtime, console, "restoreMaximize", zero);
    register_unless_closure(runtime, console, "maximize", zero);
    register_unless_closure(runtime, console, "getRect", native_void);
    register_unless_closure(runtime, console, "setPos", native_void);
    register_unless_closure(runtime, console, "getPlacement", native_void);
    register_unless_closure(runtime, console, "setPlacement", zero);
    register_unless_closure(runtime, console, "bringAfter", native_void);
}

// System — NCB_ATTACH_FUNCTION(*, System, ...)
fn install_system_ex(runtime: &mut Runtime<KrkrHost>) {
    let Variant::Object(system) = runtime.global_member("System") else {
        return;
    };

    register_unless_closure(runtime, system, "getDisplayMonitors", display_monitors);
    register_unless_closure(runtime, system, "getMonitorInfo", native_void);
    register_unless_closure(runtime, system, "getCursorPos", cursor_pos);
    register_unless_closure(runtime, system, "setCursorPos", one);
    register_unless_closure(runtime, system, "setClipCursor", native_void);
    // The reference throws on unknown metric names; the stub returns 0 for
    // everything and never throws.
    register_unless_closure(runtime, system, "getSystemMetrics", zero);
    register_unless_closure(runtime, system, "readEnvValue", native_void);
    register_unless_closure(runtime, system, "expandEnvString", expand_env_string);
    register_unless_closure(runtime, system, "setApplicationIcon", native_void);
    register_unless_closure(runtime, system, "setIconicPreview", zero);
    register_unless_closure(runtime, system, "getDoubleClickTime", double_click_time);
    register_unless_closure(runtime, system, "setDpiAwareness", zero);
    register_unless_closure(runtime, system, "findWindowEx", zero);
    register_unless_closure(runtime, system, "loadCursor", zero);
    register_unless_closure(runtime, system, "classLongPtr", zero);
    register_unless_closure(runtime, system, "mapVirtualKey", zero);
    register_unless_closure(runtime, system, "breathe", native_void);
    register_unless_closure(runtime, system, "isBreathing", zero);
    register_unless_closure(runtime, system, "clearGraphicCache", native_void);
    register_unless_closure(runtime, system, "getAboutString", about_string);
    register_unless_closure(runtime, system, "getCPUType", zero);
}

// Scripts — NCB_ATTACH_FUNCTION(setEvalErrorLog, Scripts, ...). Unlike the
// reference, Scripts.eval is NOT overridden; the engine builtin stays in place.
fn install_scripts_ex(runtime: &mut Runtime<KrkrHost>) {
    let Variant::Object(scripts) = runtime.global_member("Scripts") else {
        return;
    };
    if matches!(
        runtime.object_member(scripts, "__windowExEvalErrorLog"),
        Variant::Void
    ) {
        runtime.set_object_member(scripts, "__windowExEvalErrorLog", Variant::Integer(1));
    }
    register_unless_closure(runtime, scripts, "setEvalErrorLog", set_eval_error_log);
}

fn register_unless_closure(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &'static str,
    function: impl krkr_tjs2::runtime::NativeFunction<KrkrHost> + 'static,
) {
    if matches!(runtime.object_member(object, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native(object, name, function);
}

fn truthy(variant: &Variant) -> bool {
    match variant {
        Variant::Void | Variant::Null => false,
        Variant::Integer(value) => *value != 0,
        Variant::Real(value) => *value != 0.0,
        Variant::String(value) => !value.is_empty(),
        Variant::Octet(value) => !value.is_empty(),
        _ => true,
    }
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn one(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(1))
}

fn minus_one(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(-1))
}

fn empty_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(String::new()))
}

fn alloc_rect_dict(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let dict = runtime.alloc_ordinary_object();
    runtime.set_object_member(dict, "x", Variant::Integer(0));
    runtime.set_object_member(dict, "y", Variant::Integer(0));
    runtime.set_object_member(dict, "w", Variant::Integer(0));
    runtime.set_object_member(dict, "h", Variant::Integer(0));
    dict
}

fn empty_rect_dict(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(alloc_rect_dict(runtime)))
}

// Window.ncHitTest reports HTCLIENT (nchtClient) for every point.
fn nc_hit_test(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(1))
}

// System.getDisplayMonitors reports a single zeroed primary monitor.
fn display_monitors(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    runtime.host_mut().log(
        "windowEx.dll: System.getDisplayMonitors is stubbed; reporting one zeroed primary monitor",
    );
    let monitor = runtime.alloc_ordinary_object();
    runtime.set_object_member(monitor, "name", Variant::String("DISPLAY".to_string()));
    runtime.set_object_member(monitor, "primary", Variant::Integer(1));
    let monitor_rect = alloc_rect_dict(runtime);
    runtime.set_object_member(monitor, "monitor", Variant::Object(monitor_rect));
    let work_rect = alloc_rect_dict(runtime);
    runtime.set_object_member(monitor, "work", Variant::Object(work_rect));
    let intersect_rect = alloc_rect_dict(runtime);
    runtime.set_object_member(monitor, "intersect", Variant::Object(intersect_rect));
    Ok(Variant::Object(
        runtime.alloc_array_object(vec![Variant::Object(monitor)]),
    ))
}

fn cursor_pos(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let dict = runtime.alloc_ordinary_object();
    runtime.set_object_member(dict, "x", Variant::Integer(0));
    runtime.set_object_member(dict, "y", Variant::Integer(0));
    Ok(Variant::Object(dict))
}

// System.expandEnvString has no environment to expand; echo the input.
fn expand_env_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(args.into_iter().next().unwrap_or_default())
}

fn double_click_time(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(500))
}

fn about_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        "Kirakira (Kirikiri-compatible emulator)".to_string(),
    ))
}

// Scripts.setEvalErrorLog(enabled) returns the previous flag value.
fn set_eval_error_log(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj else {
        return Ok(Variant::Integer(1));
    };
    let previous = match runtime.object_member(this, "__windowExEvalErrorLog") {
        Variant::Void => Variant::Integer(1),
        value => value,
    };
    let enabled = args.first().map(truthy).unwrap_or(false);
    runtime.set_object_member(
        this,
        "__windowExEvalErrorLog",
        Variant::Integer(i64::from(enabled)),
    );
    Ok(previous)
}
