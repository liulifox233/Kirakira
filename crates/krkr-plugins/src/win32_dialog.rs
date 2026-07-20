//! No-op stub of the KRKR plugin `win32dialog.dll` (wtnbgo/win32dialog).
//!
//! Surface-compatible only: every class, method, property and constant the
//! real plugin registers is present, but no Win32 dialog is ever created and
//! all queries return zero/empty values. `open()` reports the modeless result
//! (0) immediately, `messageBox` pretends the default button was pressed and
//! `chooseColor` reports cancellation, so scripts that merely probe the API
//! keep running. Real dialog interaction is out of scope for the stub.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

pub struct Win32DialogPlugin;

impl KrkrPlugin for Win32DialogPlugin {
    fn name(&self) -> &str {
        "win32dialog.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_win32dialog(runtime);
        Ok(())
    }
}

fn install_win32dialog(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "WIN32Dialog");
            install_dialog_members(runtime, instance);
            runtime.set_object_member(instance, "owner", args.first().cloned().unwrap_or_default());
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "WIN32Dialog");
    install_dialog_members(runtime, class);
    install_dialog_statics(runtime, class);
    install_subclasses(runtime, class);
    for (name, value) in INT_CONSTANTS {
        runtime.set_object_member(class, *name, Variant::Integer(*value));
    }
    for (name, value) in STR_CONSTANTS {
        runtime.set_object_member(class, *name, Variant::String((*value).to_owned()));
    }
    runtime.set_global_member("WIN32Dialog", Variant::Object(class));
}

fn bound_instance(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    class_name: &'static str,
) -> ObjectHandle {
    let instance = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(instance, class_name);
    instance
}

// -------------------------------------------------------------
// WIN32Dialog instance members

fn install_dialog_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for name in [
        "loadResource",
        "makeTemplate",
        "close",
        "show",
        "setItemInt",
        "setItemText",
        "setItemEnabled",
        "setItemFocus",
        "setItemPos",
        "setItemSize",
        "setPos",
        "setSize",
        "setActive",
        "bringToFront",
        "setScrollInfo",
        "setMessageResult",
        "closeProgress",
        // event default stubs
        "onInit",
        "onCommand",
        "onNotify",
        "onHScroll",
        "onVScroll",
        "onSize",
    ] {
        runtime.register_object_native(handle, name, native_void);
    }
    for name in [
        "open",
        "getItem",
        "getItemID",
        "getItemLong",
        "setItemLong",
        "getItemInt",
        "getItemEnabled",
        "getItemLeft",
        "getItemTop",
        "getItemWidth",
        "getItemHeight",
        "setItemBitmap",
        "isExistentItem",
        "lockItemUpdate",
        "unlockItemUpdate",
        "sendItemMessage",
        "invalidateRect",
        "invalidateAll",
        "insertTab",
        "deleteTab",
        "deleteAllTab",
        "getCurSel",
        "setCurSel",
        "selectTab",
        "openProgress",
        "propSheetMessage",
    ] {
        runtime.register_object_native(handle, name, zero);
    }
    runtime.register_object_native(handle, "getItemClassName", empty_string);
    runtime.register_object_native(handle, "getItemText", empty_string);
    runtime.register_object_native(handle, "getBaseUnits", base_units);
    for name in ["mapRect", "getWindowRect", "getClientRect"] {
        runtime.register_object_native(handle, name, empty_rect);
    }
    runtime.register_object_native(handle, "getScrollInfo", scroll_info);

    // read/write data members
    runtime.set_object_member(handle, "modeless", Variant::Integer(0));
    runtime.set_object_member(handle, "icon", Variant::Void);
    runtime.set_object_member(handle, "progressValue", Variant::Real(0.0));
    runtime.set_object_member(handle, "progressCanceled", Variant::Integer(0));

    // Geometry/state properties. The reference registers these read-only, but
    // TJS2 lets subclass instances shadow native properties with dynamic
    // members (GINKA's LogWindowPad assigns `this.height` etc.), so they are
    // read/write here: writes land in a plain member, reads return it or 0.
    for name in [
        "left",
        "top",
        "width",
        "height",
        "HWND",
        "isValid",
        "propsheet",
        "progress",
    ] {
        register_shadowed_zero_property(runtime, handle, name);
    }
}

/// Read/write native property emulating TJS2 dynamic-member shadowing: the
/// setter overwrites the property object on the instance with the assigned
/// plain value (later reads bypass the property entirely); the getter returns
/// Integer 0 while the member still holds the property object itself.
fn register_shadowed_zero_property(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
) {
    runtime.register_object_native_property(
        handle,
        name,
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
            {
                let value = runtime.object_member(this, name);
                if !runtime.variant_is_property(&value) && !matches!(value, Variant::Void) {
                    return Ok(value);
                }
            }
            Ok(Variant::Integer(0))
        },
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
            {
                runtime.set_object_member(this, name, value);
            }
            Ok(())
        },
    );
}

// -------------------------------------------------------------
// WIN32Dialog static methods

fn install_dialog_statics(runtime: &mut Runtime<KrkrHost>, class: ObjectHandle) {
    runtime.register_object_native(class, "messageBox", message_box);
    runtime.register_object_native(class, "chooseColor", native_void);
    runtime.register_object_native(class, "initCommonControls", native_void);
    runtime.register_object_native(class, "initCommonControlsEx", one);
    runtime.register_object_native(class, "getOctetAddress", zero);
    runtime.register_object_native(class, "getStringAddress", zero);
    runtime.register_object_native(class, "openPropertySheet", zero);
}

// Pretend the user pressed the default button of the message box.
fn message_box(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let kind = args
        .get(3)
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(0);
    let buttons: &[i64] = match kind & 0x0F {
        1 => &[1, 2],      // MB_OKCANCEL: IDOK, IDCANCEL
        2 => &[3, 4, 5],   // MB_ABORTRETRYIGNORE: IDABORT, IDRETRY, IDIGNORE
        3 => &[6, 7, 2],   // MB_YESNOCANCEL: IDYES, IDNO, IDCANCEL
        4 => &[6, 7],      // MB_YESNO: IDYES, IDNO
        5 => &[4, 2],      // MB_RETRYCANCEL: IDRETRY, IDCANCEL
        6 => &[2, 10, 11], // MB_CANCELTRYCONTINUE: IDCANCEL, IDTRYAGAIN, IDCONTINUE
        _ => &[1],         // MB_OK: IDOK
    };
    let default = ((kind >> 8) & 0x03) as usize;
    let id = buttons.get(default).copied().unwrap_or(buttons[0]);
    Ok(Variant::Integer(id))
}

// -------------------------------------------------------------
// Subclasses

fn install_subclasses(runtime: &mut Runtime<KrkrHost>, class: ObjectHandle) {
    let header = subclass_constructor(runtime, "Header", install_header_members);
    let items = subclass_constructor(runtime, "Items", install_items_members);
    let bitmap = bitmap_constructor(runtime);
    let solid_brush = solid_brush_constructor(runtime);
    let draw_item = subclass_constructor(runtime, "DrawItem", install_draw_item_members);
    let notify = subclass_constructor(runtime, "Notify", install_notify_members);
    let blob = blob_constructor(runtime);
    for (name, handle) in [
        ("Header", header),
        ("Items", items),
        ("Bitmap", bitmap),
        ("SolidBrush", solid_brush),
        ("DrawItem", draw_item),
        ("Notify", notify),
        ("Blob", blob),
    ] {
        runtime.set_object_member(class, name, Variant::Object(handle));
    }
}

fn subclass_constructor(
    runtime: &mut Runtime<KrkrHost>,
    class_name: &'static str,
    install: fn(&mut Runtime<KrkrHost>, ObjectHandle),
) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, class_name);
            install(runtime, instance);
            runtime.register_object_native(instance, "finalize", native_void);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, class_name);
    install(runtime, handle);
    runtime.register_object_native(handle, "finalize", native_void);
    handle
}

fn install_header_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "store", native_void);
    runtime.set_object_member(handle, "dlgItems", Variant::Integer(0));
}

fn install_items_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "store", native_void);
}

fn bitmap_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "Bitmap");
            runtime.set_object_member(instance, "layer", args.first().cloned().unwrap_or_default());
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Bitmap");
    handle
}

fn solid_brush_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "SolidBrush");
            runtime.set_object_member(instance, "rgb", args.first().cloned().unwrap_or_default());
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "SolidBrush");
    handle
}

fn install_draw_item_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "draw", native_void);
    for name in [
        "ctrlType",
        "ctrlID",
        "itemID",
        "itemAction",
        "itemState",
        "itemData",
        "hwndItem",
    ] {
        runtime.register_object_native_property(handle, name, prop_zero, prop_read_only);
    }
    runtime.register_object_native_property(handle, "itemRect", prop_item_rect, prop_read_only);
}

fn install_notify_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    for name in ["hwndFrom", "idFrom", "code"] {
        runtime.register_object_native_property(handle, name, prop_zero, prop_read_only);
    }
    for name in ["getByte", "getWord", "getDWord"] {
        runtime.register_object_native(handle, name, zero);
    }
}

fn blob_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let size = args
                .first()
                .and_then(|value| value.to_integer().ok())
                .unwrap_or(0);
            new_blob(runtime, this_obj, size)
        },
    );
    runtime.add_object_class_info(handle, "Blob");
    install_blob_members(runtime, handle);
    runtime.register_object_native(handle, "ReferPointer", blob_refer_pointer);
    handle
}

fn new_blob(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    size: i64,
) -> Result<Variant> {
    let instance = bound_instance(runtime, this_obj, "Blob");
    install_blob_members(runtime, instance);
    let size = usize::try_from(size).unwrap_or(0);
    runtime.set_object_member(instance, "data", Variant::Octet(vec![0; size]));
    Ok(Variant::Object(instance))
}

// `Blob.ReferPointer(ptr)` is a static factory; always build a fresh object
// instead of reusing the class handle it was called through.
fn blob_refer_pointer(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    new_blob(runtime, None, 0)
}

fn install_blob_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native_property(handle, "pointer", prop_zero, prop_read_only);
    for name in ["getByte", "getWord", "getDWord", "getDWordLong"] {
        runtime.register_object_native(handle, name, zero);
    }
    runtime.register_object_native(handle, "getText", empty_string);
    for name in ["setByte", "setWord", "setDWord", "setDWordLong", "setText"] {
        runtime.register_object_native(handle, name, native_void);
    }
}

// -------------------------------------------------------------
// Shared return values

fn int_dict(runtime: &mut Runtime<KrkrHost>, entries: &[(&str, i64)]) -> Variant {
    let dict = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(dict, "Dictionary");
    for (name, value) in entries {
        runtime.set_object_member(dict, *name, Variant::Integer(*value));
    }
    Variant::Object(dict)
}

fn base_units(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(int_dict(runtime, &[("h", 8), ("v", 16)]))
}

fn empty_rect(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(int_dict(
        runtime,
        &[("left", 0), ("top", 0), ("right", 0), ("bottom", 0)],
    ))
}

fn scroll_info(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(int_dict(
        runtime,
        &[
            ("pos", 0),
            ("min", 0),
            ("max", 0),
            ("page", 0),
            ("trackpos", 0),
        ],
    ))
}

fn prop_item_rect(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    Ok(int_dict(runtime, &[("x", 0), ("y", 0), ("w", 0), ("h", 0)]))
}

fn prop_zero(_runtime: &mut Runtime<KrkrHost>, _this_obj: Option<ObjectHandle>) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn prop_read_only(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _value: Variant,
) -> Result<()> {
    Err(TjsError::runtime("WIN32Dialog: property is read-only"))
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

fn empty_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(String::new()))
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

// -------------------------------------------------------------
// Constants (transcribed from the ENUM/U_ENUM block of the reference
// main.cpp; values are the Win32 SDK definitions as positive 64-bit
// integers, i.e. unsigned 32-bit zero-extension for 0x8xxxxxxx and
// (0U-N) style values).

#[rustfmt::skip]
const INT_CONSTANTS: &[(&str, i64)] = &[
    // stock objects
    ("BLACK_BRUSH", 4),
    ("HOLLOW_BRUSH", 5),
    ("NULL_BRUSH", 5),
    ("WHITE_BRUSH", 0),

    // Window Long index
    ("GWL_STYLE", -16),
    ("GWLP_WNDPROC", -4),
    ("GWLP_HINSTANCE", -6),
    ("GWLP_HWNDPARENT", -8),
    ("GWL_EXSTYLE", -20),
    ("GWLP_USERDATA", -21),
    ("GWL_ID", -12),

    // Window Styles
    ("WS_OVERLAPPED", 0x00000000),
    ("WS_POPUP", 0x80000000),
    ("WS_CHILD", 0x40000000),
    ("WS_MINIMIZE", 0x20000000),
    ("WS_VISIBLE", 0x10000000),
    ("WS_DISABLED", 0x08000000),
    ("WS_CLIPSIBLINGS", 0x04000000),
    ("WS_CLIPCHILDREN", 0x02000000),
    ("WS_MAXIMIZE", 0x01000000),
    ("WS_CAPTION", 0x00C00000),
    ("WS_BORDER", 0x00800000),
    ("WS_DLGFRAME", 0x00400000),
    ("WS_VSCROLL", 0x00200000),
    ("WS_HSCROLL", 0x00100000),
    ("WS_SYSMENU", 0x00080000),
    ("WS_THICKFRAME", 0x00040000),
    ("WS_GROUP", 0x00020000),
    ("WS_TABSTOP", 0x00010000),
    ("WS_MINIMIZEBOX", 0x00020000),
    ("WS_MAXIMIZEBOX", 0x00010000),
    ("WS_TILED", 0x00000000),
    ("WS_ICONIC", 0x20000000),
    ("WS_SIZEBOX", 0x00040000),
    ("WS_TILEDWINDOW", 0x00CF0000),
    ("WS_OVERLAPPEDWINDOW", 0x00CF0000),
    ("WS_POPUPWINDOW", 0x80880000),
    ("WS_CHILDWINDOW", 0x40000000),

    // Extended Window Styles
    ("WS_EX_DLGMODALFRAME", 0x00000001),
    ("WS_EX_NOPARENTNOTIFY", 0x00000004),
    ("WS_EX_TOPMOST", 0x00000008),
    ("WS_EX_ACCEPTFILES", 0x00000010),
    ("WS_EX_TRANSPARENT", 0x00000020),
    ("WS_EX_MDICHILD", 0x00000040),
    ("WS_EX_TOOLWINDOW", 0x00000080),
    ("WS_EX_WINDOWEDGE", 0x00000100),
    ("WS_EX_CLIENTEDGE", 0x00000200),
    ("WS_EX_CONTEXTHELP", 0x00000400),
    ("WS_EX_RIGHT", 0x00001000),
    ("WS_EX_LEFT", 0x00000000),
    ("WS_EX_RTLREADING", 0x00002000),
    ("WS_EX_LTRREADING", 0x00000000),
    ("WS_EX_LEFTSCROLLBAR", 0x00004000),
    ("WS_EX_RIGHTSCROLLBAR", 0x00000000),
    ("WS_EX_CONTROLPARENT", 0x00010000),
    ("WS_EX_STATICEDGE", 0x00020000),
    ("WS_EX_APPWINDOW", 0x00040000),
    ("WS_EX_OVERLAPPEDWINDOW", 0x00000300),
    ("WS_EX_PALETTEWINDOW", 0x00000188),
    ("WS_EX_LAYERED", 0x00080000),
    ("WS_EX_NOINHERITLAYOUT", 0x00100000),
    ("WS_EX_LAYOUTRTL", 0x00400000),
    ("WS_EX_COMPOSITED", 0x02000000),
    ("WS_EX_NOACTIVATE", 0x08000000),

    // Dialog Box Command IDs
    ("IDOK", 1),
    ("IDCANCEL", 2),
    ("IDABORT", 3),
    ("IDRETRY", 4),
    ("IDIGNORE", 5),
    ("IDYES", 6),
    ("IDNO", 7),
    ("IDCLOSE", 8),
    ("IDHELP", 9),
    ("IDTRYAGAIN", 10),
    ("IDCONTINUE", 11),
    ("IDTIMEOUT", 32000),

    // Edit Control Styles
    ("ES_LEFT", 0x0000),
    ("ES_CENTER", 0x0001),
    ("ES_RIGHT", 0x0002),
    ("ES_MULTILINE", 0x0004),
    ("ES_UPPERCASE", 0x0008),
    ("ES_LOWERCASE", 0x0010),
    ("ES_PASSWORD", 0x0020),
    ("ES_AUTOVSCROLL", 0x0040),
    ("ES_AUTOHSCROLL", 0x0080),
    ("ES_NOHIDESEL", 0x0100),
    ("ES_OEMCONVERT", 0x0400),
    ("ES_READONLY", 0x0800),
    ("ES_WANTRETURN", 0x1000),
    ("ES_NUMBER", 0x2000),

    // Edit Control Notification Codes
    ("EN_SETFOCUS", 0x0100),
    ("EN_KILLFOCUS", 0x0200),
    ("EN_CHANGE", 0x0300),
    ("EN_UPDATE", 0x0400),
    ("EN_ERRSPACE", 0x0500),
    ("EN_MAXTEXT", 0x0501),
    ("EN_HSCROLL", 0x0601),
    ("EN_VSCROLL", 0x0602),
    ("EN_ALIGN_LTR_EC", 0x0700),
    ("EN_ALIGN_RTL_EC", 0x0701),

    // Edit control EM_SETMARGIN parameters
    ("EC_LEFTMARGIN", 0x0001),
    ("EC_RIGHTMARGIN", 0x0002),
    ("EC_USEFONTINFO", 0xFFFF),
    // wParam of EM_GET/SETIMESTATUS
    ("EMSIS_COMPOSITIONSTRING", 0x0001),
    // lParam for EMSIS_COMPOSITIONSTRING
    ("EIMES_GETCOMPSTRATONCE", 0x0001),
    ("EIMES_CANCELCOMPSTRINFOCUS", 0x0002),
    ("EIMES_COMPLETECOMPSTRKILLFOCUS", 0x0004),

    // Edit Control Messages
    ("EM_GETSEL", 0x00B0),
    ("EM_SETSEL", 0x00B1),
    ("EM_GETRECT", 0x00B2),
    ("EM_SETRECT", 0x00B3),
    ("EM_SETRECTNP", 0x00B4),
    ("EM_SCROLL", 0x00B5),
    ("EM_LINESCROLL", 0x00B6),
    ("EM_SCROLLCARET", 0x00B7),
    ("EM_GETMODIFY", 0x00B8),
    ("EM_SETMODIFY", 0x00B9),
    ("EM_GETLINECOUNT", 0x00BA),
    ("EM_LINEINDEX", 0x00BB),
    ("EM_SETHANDLE", 0x00BC),
    ("EM_GETHANDLE", 0x00BD),
    ("EM_GETTHUMB", 0x00BE),
    ("EM_LINELENGTH", 0x00C1),
    ("EM_REPLACESEL", 0x00C2),
    ("EM_GETLINE", 0x00C4),
    ("EM_LIMITTEXT", 0x00C5),
    ("EM_CANUNDO", 0x00C6),
    ("EM_UNDO", 0x00C7),
    ("EM_FMTLINES", 0x00C8),
    ("EM_LINEFROMCHAR", 0x00C9),
    ("EM_SETTABSTOPS", 0x00CB),
    ("EM_SETPASSWORDCHAR", 0x00CC),
    ("EM_EMPTYUNDOBUFFER", 0x00CD),
    ("EM_GETFIRSTVISIBLELINE", 0x00CE),
    ("EM_SETREADONLY", 0x00CF),
    ("EM_SETWORDBREAKPROC", 0x00D0),
    ("EM_GETWORDBREAKPROC", 0x00D1),
    ("EM_GETPASSWORDCHAR", 0x00D2),
    ("EM_SETMARGINS", 0x00D3),
    ("EM_GETMARGINS", 0x00D4),
    ("EM_SETLIMITTEXT", 0x00C5),
    ("EM_GETLIMITTEXT", 0x00D5),
    ("EM_POSFROMCHAR", 0x00D6),
    ("EM_CHARFROMPOS", 0x00D7),
    ("EM_SETIMESTATUS", 0x00D8),
    ("EM_GETIMESTATUS", 0x00D9),

    // EDITWORDBREAKPROC code values
    ("WB_LEFT", 0),
    ("WB_RIGHT", 1),
    ("WB_ISDELIMITER", 2),

    // Button Control Styles
    ("BS_PUSHBUTTON", 0x0000),
    ("BS_DEFPUSHBUTTON", 0x0001),
    ("BS_CHECKBOX", 0x0002),
    ("BS_AUTOCHECKBOX", 0x0003),
    ("BS_RADIOBUTTON", 0x0004),
    ("BS_3STATE", 0x0005),
    ("BS_AUTO3STATE", 0x0006),
    ("BS_GROUPBOX", 0x0007),
    ("BS_USERBUTTON", 0x0008),
    ("BS_AUTORADIOBUTTON", 0x0009),
    ("BS_PUSHBOX", 0x000A),
    ("BS_OWNERDRAW", 0x000B),
    ("BS_TYPEMASK", 0x000F),
    ("BS_LEFTTEXT", 0x0020),
    ("BS_TEXT", 0x0000),
    ("BS_ICON", 0x0040),
    ("BS_BITMAP", 0x0080),
    ("BS_LEFT", 0x0100),
    ("BS_RIGHT", 0x0200),
    ("BS_CENTER", 0x0300),
    ("BS_TOP", 0x0400),
    ("BS_BOTTOM", 0x0800),
    ("BS_VCENTER", 0x0C00),
    ("BS_PUSHLIKE", 0x1000),
    ("BS_MULTILINE", 0x2000),
    ("BS_NOTIFY", 0x4000),
    ("BS_FLAT", 0x8000),
    ("BS_RIGHTBUTTON", 0x0020),

    // User Button Notification Codes
    ("BN_CLICKED", 0),
    ("BN_PAINT", 1),
    ("BN_HILITE", 2),
    ("BN_UNHILITE", 3),
    ("BN_DISABLE", 4),
    ("BN_DOUBLECLICKED", 5),
    ("BN_PUSHED", 2),
    ("BN_UNPUSHED", 3),
    ("BN_DBLCLK", 5),
    ("BN_SETFOCUS", 6),
    ("BN_KILLFOCUS", 7),

    // Button Control Messages
    ("BM_GETCHECK", 0x00F0),
    ("BM_SETCHECK", 0x00F1),
    ("BM_GETSTATE", 0x00F2),
    ("BM_SETSTATE", 0x00F3),
    ("BM_SETSTYLE", 0x00F4),
    ("BM_CLICK", 0x00F5),
    ("BM_GETIMAGE", 0x00F6),
    ("BM_SETIMAGE", 0x00F7),
    ("BM_SETDONTCLICK", 0x00F8),
    ("BST_UNCHECKED", 0),
    ("BST_CHECKED", 1),
    ("BST_INDETERMINATE", 2),
    ("BST_PUSHED", 4),
    ("BST_FOCUS", 8),

    // Static Control Constants
    ("SS_LEFT", 0x00),
    ("SS_CENTER", 0x01),
    ("SS_RIGHT", 0x02),
    ("SS_ICON", 0x03),
    ("SS_BLACKRECT", 0x04),
    ("SS_GRAYRECT", 0x05),
    ("SS_WHITERECT", 0x06),
    ("SS_BLACKFRAME", 0x07),
    ("SS_GRAYFRAME", 0x08),
    ("SS_WHITEFRAME", 0x09),
    ("SS_USERITEM", 0x0A),
    ("SS_SIMPLE", 0x0B),
    ("SS_LEFTNOWORDWRAP", 0x0C),
    ("SS_OWNERDRAW", 0x0D),
    ("SS_BITMAP", 0x0E),
    ("SS_ENHMETAFILE", 0x0F),
    ("SS_ETCHEDHORZ", 0x10),
    ("SS_ETCHEDVERT", 0x11),
    ("SS_ETCHEDFRAME", 0x12),
    ("SS_TYPEMASK", 0x1F),
    ("SS_REALSIZECONTROL", 0x40),
    ("SS_NOPREFIX", 0x80),
    ("SS_NOTIFY", 0x0100),
    ("SS_CENTERIMAGE", 0x0200),
    ("SS_RIGHTJUST", 0x0400),
    ("SS_REALSIZEIMAGE", 0x0800),
    ("SS_SUNKEN", 0x1000),
    ("SS_EDITCONTROL", 0x2000),
    ("SS_ENDELLIPSIS", 0x4000),
    ("SS_PATHELLIPSIS", 0x8000),
    ("SS_WORDELLIPSIS", 0xC000),
    ("SS_ELLIPSISMASK", 0xC000),

    // Static Control Messages
    ("STM_SETICON", 0x0170),
    ("STM_GETICON", 0x0171),
    ("STM_SETIMAGE", 0x0172),
    ("STM_GETIMAGE", 0x0173),
    ("STN_CLICKED", 0),
    ("STN_DBLCLK", 1),
    ("STN_ENABLE", 2),
    ("STN_DISABLE", 3),
    ("STM_MSGMAX", 0x0174),

    // Dialog Styles
    ("DS_ABSALIGN", 0x01),
    ("DS_SYSMODAL", 0x02),
    ("DS_LOCALEDIT", 0x20),
    ("DS_SETFONT", 0x40),
    ("DS_MODALFRAME", 0x80),
    ("DS_NOIDLEMSG", 0x0100),
    ("DS_SETFOREGROUND", 0x0200),
    ("DS_3DLOOK", 0x0004),
    ("DS_FIXEDSYS", 0x0008),
    ("DS_NOFAILCREATE", 0x0010),
    ("DS_CONTROL", 0x0400),
    ("DS_CENTER", 0x0800),
    ("DS_CENTERMOUSE", 0x1000),
    ("DS_CONTEXTHELP", 0x2000),
    ("DS_SHELLFONT", 0x0048),
    ("DS_USEPIXELS", 0x8000),

    ("DM_GETDEFID", 0x0400),
    ("DM_SETDEFID", 0x0401),
    ("DM_REPOSITION", 0x0402),
    // Returned in HIWORD() of DM_GETDEFID result if msg is supported
    ("DC_HASDEFID", 0x534B),

    // Dialog Codes
    ("DLGC_WANTARROWS", 0x0001),
    ("DLGC_WANTTAB", 0x0002),
    ("DLGC_WANTALLKEYS", 0x0004),
    ("DLGC_WANTMESSAGE", 0x0004),
    ("DLGC_HASSETSEL", 0x0008),
    ("DLGC_DEFPUSHBUTTON", 0x0010),
    ("DLGC_UNDEFPUSHBUTTON", 0x0020),
    ("DLGC_RADIOBUTTON", 0x0040),
    ("DLGC_WANTCHARS", 0x0080),
    ("DLGC_STATIC", 0x0100),
    ("DLGC_BUTTON", 0x2000),

    // Listbox Return Values
    ("LB_CTLCODE", 0),
    ("LB_OKAY", 0),
    ("LB_ERR", -1),
    ("LB_ERRSPACE", -2),

    // Listbox Notification Codes
    ("LBN_ERRSPACE", -2),
    ("LBN_SELCHANGE", 1),
    ("LBN_DBLCLK", 2),
    ("LBN_SELCANCEL", 3),
    ("LBN_SETFOCUS", 4),
    ("LBN_KILLFOCUS", 5),

    // Listbox Messages
    ("LB_ADDSTRING", 0x0180),
    ("LB_INSERTSTRING", 0x0181),
    ("LB_DELETESTRING", 0x0182),
    ("LB_SELITEMRANGEEX", 0x0183),
    ("LB_RESETCONTENT", 0x0184),
    ("LB_SETSEL", 0x0185),
    ("LB_SETCURSEL", 0x0186),
    ("LB_GETSEL", 0x0187),
    ("LB_GETCURSEL", 0x0188),
    ("LB_GETTEXT", 0x0189),
    ("LB_GETTEXTLEN", 0x018A),
    ("LB_GETCOUNT", 0x018B),
    ("LB_SELECTSTRING", 0x018C),
    ("LB_DIR", 0x018D),
    ("LB_GETTOPINDEX", 0x018E),
    ("LB_FINDSTRING", 0x018F),
    ("LB_GETSELCOUNT", 0x0190),
    ("LB_GETSELITEMS", 0x0191),
    ("LB_SETTABSTOPS", 0x0192),
    ("LB_GETHORIZONTALEXTENT", 0x0193),
    ("LB_SETHORIZONTALEXTENT", 0x0194),
    ("LB_SETCOLUMNWIDTH", 0x0195),
    ("LB_ADDFILE", 0x0196),
    ("LB_SETTOPINDEX", 0x0197),
    ("LB_GETITEMRECT", 0x0198),
    ("LB_GETITEMDATA", 0x0199),
    ("LB_SETITEMDATA", 0x019A),
    ("LB_SELITEMRANGE", 0x019B),
    ("LB_SETANCHORINDEX", 0x019C),
    ("LB_GETANCHORINDEX", 0x019D),
    ("LB_SETCARETINDEX", 0x019E),
    ("LB_GETCARETINDEX", 0x019F),
    ("LB_SETITEMHEIGHT", 0x01A0),
    ("LB_GETITEMHEIGHT", 0x01A1),
    ("LB_FINDSTRINGEXACT", 0x01A2),
    ("LB_SETLOCALE", 0x01A5),
    ("LB_GETLOCALE", 0x01A6),
    ("LB_SETCOUNT", 0x01A7),
    ("LB_INITSTORAGE", 0x01A8),
    ("LB_ITEMFROMPOINT", 0x01A9),
    ("LB_MULTIPLEADDSTRING", 0x01B1),
    ("LB_GETLISTBOXINFO", 0x01B2),
    ("LB_MSGMAX", 0x01B3),

    // Listbox Styles
    ("LBS_NOTIFY", 0x0001),
    ("LBS_SORT", 0x0002),
    ("LBS_NOREDRAW", 0x0004),
    ("LBS_MULTIPLESEL", 0x0008),
    ("LBS_OWNERDRAWFIXED", 0x0010),
    ("LBS_OWNERDRAWVARIABLE", 0x0020),
    ("LBS_HASSTRINGS", 0x0040),
    ("LBS_USETABSTOPS", 0x0080),
    ("LBS_NOINTEGRALHEIGHT", 0x0100),
    ("LBS_MULTICOLUMN", 0x0200),
    ("LBS_WANTKEYBOARDINPUT", 0x0400),
    ("LBS_EXTENDEDSEL", 0x0800),
    ("LBS_DISABLENOSCROLL", 0x1000),
    ("LBS_NODATA", 0x2000),
    ("LBS_NOSEL", 0x4000),
    ("LBS_COMBOBOX", 0x8000),
    ("LBS_STANDARD", 0x00A00003),

    // Combo Box return Values
    ("CB_OKAY", 0),
    ("CB_ERR", -1),
    ("CB_ERRSPACE", -2),

    // Combo Box Notification Codes
    ("CBN_ERRSPACE", -1),
    ("CBN_SELCHANGE", 1),
    ("CBN_DBLCLK", 2),
    ("CBN_SETFOCUS", 3),
    ("CBN_KILLFOCUS", 4),
    ("CBN_EDITCHANGE", 5),
    ("CBN_EDITUPDATE", 6),
    ("CBN_DROPDOWN", 7),
    ("CBN_CLOSEUP", 8),
    ("CBN_SELENDOK", 9),
    ("CBN_SELENDCANCEL", 10),

    // Combo Box styles
    ("CBS_SIMPLE", 0x0001),
    ("CBS_DROPDOWN", 0x0002),
    ("CBS_DROPDOWNLIST", 0x0003),
    ("CBS_OWNERDRAWFIXED", 0x0010),
    ("CBS_OWNERDRAWVARIABLE", 0x0020),
    ("CBS_AUTOHSCROLL", 0x0040),
    ("CBS_OEMCONVERT", 0x0080),
    ("CBS_SORT", 0x0100),
    ("CBS_HASSTRINGS", 0x0200),
    ("CBS_NOINTEGRALHEIGHT", 0x0400),
    ("CBS_DISABLENOSCROLL", 0x0800),
    ("CBS_UPPERCASE", 0x2000),
    ("CBS_LOWERCASE", 0x4000),

    // Combo Box messages
    ("CB_GETEDITSEL", 0x0140),
    ("CB_LIMITTEXT", 0x0141),
    ("CB_SETEDITSEL", 0x0142),
    ("CB_ADDSTRING", 0x0143),
    ("CB_DELETESTRING", 0x0144),
    ("CB_DIR", 0x0145),
    ("CB_GETCOUNT", 0x0146),
    ("CB_GETCURSEL", 0x0147),
    ("CB_GETLBTEXT", 0x0148),
    ("CB_GETLBTEXTLEN", 0x0149),
    ("CB_INSERTSTRING", 0x014A),
    ("CB_RESETCONTENT", 0x014B),
    ("CB_FINDSTRING", 0x014C),
    ("CB_SELECTSTRING", 0x014D),
    ("CB_SETCURSEL", 0x014E),
    ("CB_SHOWDROPDOWN", 0x014F),
    ("CB_GETITEMDATA", 0x0150),
    ("CB_SETITEMDATA", 0x0151),
    ("CB_GETDROPPEDCONTROLRECT", 0x0152),
    ("CB_SETITEMHEIGHT", 0x0153),
    ("CB_GETITEMHEIGHT", 0x0154),
    ("CB_SETEXTENDEDUI", 0x0155),
    ("CB_GETEXTENDEDUI", 0x0156),
    ("CB_GETDROPPEDSTATE", 0x0157),
    ("CB_FINDSTRINGEXACT", 0x0158),
    ("CB_SETLOCALE", 0x0159),
    ("CB_GETLOCALE", 0x015A),
    ("CB_GETTOPINDEX", 0x015B),
    ("CB_SETTOPINDEX", 0x015C),
    ("CB_GETHORIZONTALEXTENT", 0x015D),
    ("CB_SETHORIZONTALEXTENT", 0x015E),
    ("CB_GETDROPPEDWIDTH", 0x015F),
    ("CB_SETDROPPEDWIDTH", 0x0160),
    ("CB_INITSTORAGE", 0x0161),
    ("CB_MULTIPLEADDSTRING", 0x0163),
    ("CB_GETCOMBOBOXINFO", 0x0164),
    ("CB_MSGMAX", 0x0165),

    // Scroll Bar Styles
    ("SBS_HORZ", 0x0000),
    ("SBS_VERT", 0x0001),
    ("SBS_TOPALIGN", 0x0002),
    ("SBS_LEFTALIGN", 0x0002),
    ("SBS_BOTTOMALIGN", 0x0004),
    ("SBS_RIGHTALIGN", 0x0004),
    ("SBS_SIZEBOXTOPLEFTALIGN", 0x0002),
    ("SBS_SIZEBOXBOTTOMRIGHTALIGN", 0x0004),
    ("SBS_SIZEBOX", 0x0008),
    ("SBS_SIZEGRIP", 0x0010),

    // Scroll bar messages
    ("SBM_SETPOS", 0x00E0),
    ("SBM_GETPOS", 0x00E1),
    ("SBM_SETRANGE", 0x00E2),
    ("SBM_SETRANGEREDRAW", 0x00E6),
    ("SBM_GETRANGE", 0x00E3),
    ("SBM_ENABLE_ARROWS", 0x00E4),
    ("SBM_SETSCROLLINFO", 0x00E9),
    ("SBM_GETSCROLLINFO", 0x00EA),
    ("SBM_GETSCROLLBARINFO", 0x00EB),
    ("SIF_RANGE", 0x0001),
    ("SIF_PAGE", 0x0002),
    ("SIF_POS", 0x0004),
    ("SIF_DISABLENOSCROLL", 0x0008),
    ("SIF_TRACKPOS", 0x0010),
    ("SIF_ALL", 0x0017),

    // Scroll bar options
    ("SB_LINEUP", 0),
    ("SB_LINELEFT", 0),
    ("SB_LINEDOWN", 1),
    ("SB_LINERIGHT", 1),
    ("SB_PAGEUP", 2),
    ("SB_PAGELEFT", 2),
    ("SB_PAGEDOWN", 3),
    ("SB_PAGERIGHT", 3),
    ("SB_THUMBPOSITION", 4),
    ("SB_THUMBTRACK", 5),
    ("SB_TOP", 6),
    ("SB_LEFT", 6),
    ("SB_BOTTOM", 7),
    ("SB_RIGHT", 7),
    ("SB_ENDSCROLL", 8),

    // Font Weights
    ("FW_DONTCARE", 0),
    ("FW_THIN", 100),
    ("FW_EXTRALIGHT", 200),
    ("FW_LIGHT", 300),
    ("FW_NORMAL", 400),
    ("FW_MEDIUM", 500),
    ("FW_SEMIBOLD", 600),
    ("FW_BOLD", 700),
    ("FW_EXTRABOLD", 800),
    ("FW_HEAVY", 900),

    // ShowWindow options
    ("SW_HIDE", 0),
    ("SW_SHOWNORMAL", 1),
    ("SW_NORMAL", 1),
    ("SW_SHOWMINIMIZED", 2),
    ("SW_SHOWMAXIMIZED", 3),
    ("SW_MAXIMIZE", 3),
    ("SW_SHOWNOACTIVATE", 4),
    ("SW_SHOW", 5),
    ("SW_MINIMIZE", 6),
    ("SW_SHOWMINNOACTIVE", 7),
    ("SW_SHOWNA", 8),
    ("SW_RESTORE", 9),
    ("SW_SHOWDEFAULT", 10),
    ("SW_FORCEMINIMIZE", 11),

    // Control classes
    ("BUTTON", 0x0080),
    ("EDIT", 0x0081),
    ("STATIC", 0x0082),
    ("LISTBOX", 0x0083),
    ("SCROLLBAR", 0x0084),
    ("COMBOBOX", 0x0085),

    // for MessageBox
    ("MB_ABORTRETRYIGNORE", 0x00000002),
    ("MB_CANCELTRYCONTINUE", 0x00000006),
    ("MB_HELP", 0x00004000),
    ("MB_OK", 0x00000000),
    ("MB_OKCANCEL", 0x00000001),
    ("MB_RETRYCANCEL", 0x00000005),
    ("MB_YESNO", 0x00000004),
    ("MB_YESNOCANCEL", 0x00000003),
    ("MB_ICONEXCLAMATION", 0x00000030),
    ("MB_ICONWARNING", 0x00000030),
    ("MB_ICONINFORMATION", 0x00000040),
    ("MB_ICONASTERISK", 0x00000040),
    ("MB_ICONQUESTION", 0x00000020),
    ("MB_ICONSTOP", 0x00000010),
    ("MB_ICONERROR", 0x00000010),
    ("MB_ICONHAND", 0x00000010),
    ("MB_DEFBUTTON1", 0x00000000),
    ("MB_DEFBUTTON2", 0x00000100),
    ("MB_DEFBUTTON3", 0x00000200),
    ("MB_DEFBUTTON4", 0x00000300),
    ("MB_APPLMODAL", 0x00000000),
    ("MB_SYSTEMMODAL", 0x00001000),
    ("MB_TASKMODAL", 0x00002000),
    ("MB_DEFAULT_DESKTOP_ONLY", 0x00020000),
    ("MB_RIGHT", 0x00080000),
    ("MB_RTLREADING", 0x00100000),
    ("MB_SETFOREGROUND", 0x00010000),
    ("MB_TOPMOST", 0x00040000),
    ("MB_SERVICE_NOTIFICATION", 0x00200000),
    ("MB_SERVICE_NOTIFICATION_NT3X", 0x00040000),

    // plugin-specific: center the box on its owner window
    ("MB_OWNER_CENTER", 0x40000000),

    // InitCommonControlsEx parameters
    ("ICC_LISTVIEW_CLASSES", 0x00000001),
    ("ICC_TREEVIEW_CLASSES", 0x00000002),
    ("ICC_BAR_CLASSES", 0x00000004),
    ("ICC_TAB_CLASSES", 0x00000008),
    ("ICC_UPDOWN_CLASS", 0x00000010),
    ("ICC_PROGRESS_CLASS", 0x00000020),
    ("ICC_HOTKEY_CLASS", 0x00000040),
    ("ICC_ANIMATE_CLASS", 0x00000080),
    ("ICC_WIN95_CLASSES", 0x000000FF),
    ("ICC_DATE_CLASSES", 0x00000100),
    ("ICC_USEREX_CLASSES", 0x00000200),
    ("ICC_COOL_CLASSES", 0x00000400),
    ("ICC_INTERNET_CLASSES", 0x00000800),
    ("ICC_PAGESCROLLER_CLASS", 0x00001000),
    ("ICC_NATIVEFNTCTL_CLASS", 0x00002000),
    ("ICC_STANDARD_CLASSES", 0x00004000),
    ("ICC_LINK_CLASS", 0x00008000),

    // Trackbar Styles
    ("TBS_AUTOTICKS", 0x0001),
    ("TBS_VERT", 0x0002),
    ("TBS_HORZ", 0x0000),
    ("TBS_TOP", 0x0004),
    ("TBS_BOTTOM", 0x0000),
    ("TBS_LEFT", 0x0004),
    ("TBS_RIGHT", 0x0000),
    ("TBS_BOTH", 0x0008),
    ("TBS_NOTICKS", 0x0010),
    ("TBS_ENABLESELRANGE", 0x0020),
    ("TBS_FIXEDLENGTH", 0x0040),
    ("TBS_NOTHUMB", 0x0080),
    ("TBS_TOOLTIPS", 0x0100),
    ("TBS_REVERSED", 0x0200),
    ("TBS_DOWNISLEFT", 0x0400),
    ("TBS_NOTIFYBEFOREMOVE", 0x0800),
    ("TBS_TRANSPARENTBKGND", 0x1000),

    // Trackbar Messages
    ("TBM_GETPOS", 0x0400),
    ("TBM_GETRANGEMIN", 0x0401),
    ("TBM_GETRANGEMAX", 0x0402),
    ("TBM_GETTIC", 0x0403),
    ("TBM_SETTIC", 0x0404),
    ("TBM_SETPOS", 0x0405),
    ("TBM_SETRANGE", 0x0406),
    ("TBM_SETRANGEMIN", 0x0407),
    ("TBM_SETRANGEMAX", 0x0408),
    ("TBM_CLEARTICS", 0x0409),
    ("TBM_SETSEL", 0x040A),
    ("TBM_SETSELSTART", 0x040B),
    ("TBM_SETSELEND", 0x040C),
    ("TBM_GETPTICS", 0x040E),
    ("TBM_GETTICPOS", 0x040F),
    ("TBM_GETNUMTICS", 0x0410),
    ("TBM_GETSELSTART", 0x0411),
    ("TBM_GETSELEND", 0x0412),
    ("TBM_CLEARSEL", 0x0413),
    ("TBM_SETTICFREQ", 0x0414),
    ("TBM_SETPAGESIZE", 0x0415),
    ("TBM_GETPAGESIZE", 0x0416),
    ("TBM_SETLINESIZE", 0x0417),
    ("TBM_GETLINESIZE", 0x0418),
    ("TBM_GETTHUMBRECT", 0x0419),
    ("TBM_GETCHANNELRECT", 0x041A),
    ("TBM_SETTHUMBLENGTH", 0x041B),
    ("TBM_GETTHUMBLENGTH", 0x041C),
    ("TBM_SETTOOLTIPS", 0x041D),
    ("TBM_GETTOOLTIPS", 0x041E),
    ("TBM_SETTIPSIDE", 0x041F),
    // TrackBar Tip Side flags
    ("TBTS_TOP", 0),
    ("TBTS_LEFT", 1),
    ("TBTS_BOTTOM", 2),
    ("TBTS_RIGHT", 3),
    ("TBM_SETBUDDY", 0x0420),
    ("TBM_GETBUDDY", 0x0421),
    ("TBM_SETUNICODEFORMAT", 0x2005),
    ("TBM_GETUNICODEFORMAT", 0x2006),
    ("TB_LINEUP", 0),
    ("TB_LINEDOWN", 1),
    ("TB_PAGEUP", 2),
    ("TB_PAGEDOWN", 3),
    ("TB_THUMBPOSITION", 4),
    ("TB_THUMBTRACK", 5),
    ("TB_TOP", 6),
    ("TB_BOTTOM", 7),
    ("TB_ENDTRACK", 8),
    ("TBCD_TICS", 0x0001),
    ("TBCD_THUMB", 0x0002),
    ("TBCD_CHANNEL", 0x0003),
    ("TRBN_THUMBPOSCHANGING", 0xFFFFFA22), // TRBN_FIRST(0U-1501U)-1

    // Progress
    ("PBS_SMOOTH", 0x01),
    ("PBS_VERTICAL", 0x04),
    ("PBM_SETRANGE", 0x0401),
    ("PBM_SETPOS", 0x0402),
    ("PBM_DELTAPOS", 0x0403),
    ("PBM_SETSTEP", 0x0404),
    ("PBM_STEPIT", 0x0405),
    ("PBM_GETRANGE", 0x0407),
    ("PBM_GETPOS", 0x0408),
    ("PBM_SETBARCOLOR", 0x0409),
    ("PBM_SETBKCOLOR", 0x2001), // CCM_SETBKCOLOR
    ("PBS_MARQUEE", 0x08),
    ("PBM_SETMARQUEE", 0x040A),
    ("PBS_SMOOTHREVERSE", 0x10),
    ("PBM_GETSTEP", 0x040D),
    ("PBM_GETBKCOLOR", 0x040E),
    ("PBM_GETBARCOLOR", 0x040F),
    ("PBM_SETSTATE", 0x0410),
    ("PBM_GETSTATE", 0x0411),
    ("PBST_NORMAL", 0x0001),
    ("PBST_ERROR", 0x0002),
    ("PBST_PAUSED", 0x0003),

    // ListView Styles
    ("LVS_ICON", 0x0000),
    ("LVS_REPORT", 0x0001),
    ("LVS_SMALLICON", 0x0002),
    ("LVS_LIST", 0x0003),
    ("LVS_TYPEMASK", 0x0003),
    ("LVS_SINGLESEL", 0x0004),
    ("LVS_SHOWSELALWAYS", 0x0008),
    ("LVS_SORTASCENDING", 0x0010),
    ("LVS_SORTDESCENDING", 0x0020),
    ("LVS_SHAREIMAGELISTS", 0x0040),
    ("LVS_NOLABELWRAP", 0x0080),
    ("LVS_AUTOARRANGE", 0x0100),
    ("LVS_EDITLABELS", 0x0200),
    ("LVS_OWNERDATA", 0x1000),
    ("LVS_NOSCROLL", 0x2000),
    ("LVS_TYPESTYLEMASK", 0xFC00),
    ("LVS_ALIGNTOP", 0x0000),
    ("LVS_ALIGNLEFT", 0x0800),
    ("LVS_ALIGNMASK", 0x0C00),
    ("LVS_OWNERDRAWFIXED", 0x0400),
    ("LVS_NOCOLUMNHEADER", 0x4000),
    ("LVS_NOSORTHEADER", 0x8000),

    // ListView Extended Styles
    ("LVS_EX_GRIDLINES", 0x00000001),
    ("LVS_EX_SUBITEMIMAGES", 0x00000002),
    ("LVS_EX_CHECKBOXES", 0x00000004),
    ("LVS_EX_TRACKSELECT", 0x00000008),
    ("LVS_EX_HEADERDRAGDROP", 0x00000010),
    ("LVS_EX_FULLROWSELECT", 0x00000020),
    ("LVS_EX_ONECLICKACTIVATE", 0x00000040),
    ("LVS_EX_TWOCLICKACTIVATE", 0x00000080),
    ("LVS_EX_FLATSB", 0x00000100),
    ("LVS_EX_REGIONAL", 0x00000200),
    ("LVS_EX_INFOTIP", 0x00000400),
    ("LVS_EX_UNDERLINEHOT", 0x00000800),
    ("LVS_EX_UNDERLINECOLD", 0x00001000),
    ("LVS_EX_MULTIWORKAREAS", 0x00002000),
    ("LVS_EX_LABELTIP", 0x00004000),
    ("LVS_EX_BORDERSELECT", 0x00008000),
    ("LVS_EX_DOUBLEBUFFER", 0x00010000),
    ("LVS_EX_HIDELABELS", 0x00020000),
    ("LVS_EX_SINGLEROW", 0x00040000),
    ("LVS_EX_SNAPTOGRID", 0x00080000),
    ("LVS_EX_SIMPLESELECT", 0x00100000),
    ("LVS_EX_JUSTIFYCOLUMNS", 0x00200000),
    ("LVS_EX_TRANSPARENTBKGND", 0x00400000),
    ("LVS_EX_TRANSPARENTSHADOWTEXT", 0x00800000),
    ("LVS_EX_AUTOAUTOARRANGE", 0x01000000),
    ("LVS_EX_HEADERINALLVIEWS", 0x02000000),
    ("LVS_EX_AUTOCHECKSELECT", 0x08000000),
    ("LVS_EX_AUTOSIZECOLUMNS", 0x10000000),
    ("LVS_EX_COLUMNSNAPPOINTS", 0x40000000),
    ("LVS_EX_COLUMNOVERFLOW", 0x80000000),

    // ListView Messages (LVM_FIRST = 0x1000)
    ("LVM_FIRST", 0x1000),
    ("LVM_GETBKCOLOR", 0x1000),
    ("LVM_SETBKCOLOR", 0x1001),
    ("LVM_GETIMAGELIST", 0x1002),
    ("LVM_SETIMAGELIST", 0x1003),
    ("LVM_GETITEMCOUNT", 0x1004),
    ("LVM_GETITEMA", 0x1005),
    ("LVM_GETITEMW", 0x104B),
    ("LVM_SETITEMA", 0x1006),
    ("LVM_SETITEMW", 0x104C),
    ("LVM_INSERTITEMA", 0x1007),
    ("LVM_INSERTITEMW", 0x104D),
    ("LVM_DELETEITEM", 0x1008),
    ("LVM_DELETEALLITEMS", 0x1009),
    ("LVM_GETCALLBACKMASK", 0x100A),
    ("LVM_SETCALLBACKMASK", 0x100B),
    ("LVM_GETNEXTITEM", 0x100C),
    ("LVM_FINDITEMA", 0x100D),
    ("LVM_FINDITEMW", 0x1053),
    ("LVM_GETITEMRECT", 0x100E),
    ("LVM_SETITEMPOSITION", 0x100F),
    ("LVM_GETITEMPOSITION", 0x1010),
    ("LVM_GETSTRINGWIDTHA", 0x1011),
    ("LVM_GETSTRINGWIDTHW", 0x1057),
    ("LVM_HITTEST", 0x1012),
    ("LVM_ENSUREVISIBLE", 0x1013),
    ("LVM_SCROLL", 0x1014),
    ("LVM_REDRAWITEMS", 0x1015),
    ("LVM_ARRANGE", 0x1016),
    ("LVM_EDITLABELA", 0x1017),
    ("LVM_EDITLABELW", 0x1076),
    ("LVM_GETEDITCONTROL", 0x1018),
    ("LVM_GETCOLUMNA", 0x1019),
    ("LVM_GETCOLUMNW", 0x105F),
    ("LVM_SETCOLUMNA", 0x101A),
    ("LVM_SETCOLUMNW", 0x1060),
    ("LVM_INSERTCOLUMNA", 0x101B),
    ("LVM_INSERTCOLUMNW", 0x1061),
    ("LVM_DELETECOLUMN", 0x101C),
    ("LVM_GETCOLUMNWIDTH", 0x101D),
    ("LVM_SETCOLUMNWIDTH", 0x101E),
    ("LVM_GETHEADER", 0x101F),
    ("LVM_CREATEDRAGIMAGE", 0x1021),
    ("LVM_GETVIEWRECT", 0x1022),
    ("LVM_GETTEXTCOLOR", 0x1023),
    ("LVM_SETTEXTCOLOR", 0x1024),
    ("LVM_GETTEXTBKCOLOR", 0x1025),
    ("LVM_SETTEXTBKCOLOR", 0x1026),
    ("LVM_GETTOPINDEX", 0x1027),
    ("LVM_GETCOUNTPERPAGE", 0x1028),
    ("LVM_GETORIGIN", 0x1029),
    ("LVM_UPDATE", 0x102A),
    ("LVM_SETITEMSTATE", 0x102B),
    ("LVM_GETITEMSTATE", 0x102C),
    ("LVM_GETITEMTEXTA", 0x102D),
    ("LVM_GETITEMTEXTW", 0x1073),
    ("LVM_SETITEMTEXTA", 0x102E),
    ("LVM_SETITEMTEXTW", 0x1074),
    ("LVM_SETITEMCOUNT", 0x102F),
    ("LVM_SORTITEMS", 0x1030),
    ("LVM_SETITEMPOSITION32", 0x1031),
    ("LVM_GETSELECTEDCOUNT", 0x1032),
    ("LVM_GETITEMSPACING", 0x1033),
    ("LVM_GETISEARCHSTRINGA", 0x1034),
    ("LVM_GETISEARCHSTRINGW", 0x1075),
    ("LVM_SETICONSPACING", 0x1035),
    ("LVM_SETEXTENDEDLISTVIEWSTYLE", 0x1036),
    ("LVM_GETEXTENDEDLISTVIEWSTYLE", 0x1037),
    ("LVM_GETSUBITEMRECT", 0x1038),
    ("LVM_SUBITEMHITTEST", 0x1039),
    ("LVM_SETCOLUMNORDERARRAY", 0x103A),
    ("LVM_GETCOLUMNORDERARRAY", 0x103B),
    ("LVM_SETHOTITEM", 0x103C),
    ("LVM_GETHOTITEM", 0x103D),
    ("LVM_SETHOTCURSOR", 0x103E),
    ("LVM_GETHOTCURSOR", 0x103F),
    ("LVM_APPROXIMATEVIEWRECT", 0x1040),
    ("LVM_SETWORKAREAS", 0x1041),
    ("LVM_GETWORKAREAS", 0x1046),
    ("LVM_GETNUMBEROFWORKAREAS", 0x1049),
    ("LVM_GETSELECTIONMARK", 0x1042),
    ("LVM_SETSELECTIONMARK", 0x1043),
    ("LVM_SETHOVERTIME", 0x1047),
    ("LVM_GETHOVERTIME", 0x1048),
    ("LVM_SETTOOLTIPS", 0x104A),
    ("LVM_GETTOOLTIPS", 0x104E),
    ("LVM_SORTITEMSEX", 0x1051),
    ("LVM_SETBKIMAGEA", 0x1044),
    ("LVM_SETBKIMAGEW", 0x108A),
    ("LVM_GETBKIMAGEA", 0x1045),
    ("LVM_GETBKIMAGEW", 0x108B),
    ("LVM_SETSELECTEDCOLUMN", 0x108C),
    ("LVM_SETVIEW", 0x108E),
    ("LVM_GETVIEW", 0x108F),
    ("LVM_INSERTGROUP", 0x1091),
    ("LVM_SETGROUPINFO", 0x1093),
    ("LVM_GETGROUPINFO", 0x1095),
    ("LVM_REMOVEGROUP", 0x1096),
    ("LVM_MOVEGROUP", 0x1097),
    ("LVM_GETGROUPCOUNT", 0x1098),
    ("LVM_GETGROUPINFOBYINDEX", 0x1099),
    ("LVM_MOVEITEMTOGROUP", 0x109A),
    ("LVM_GETGROUPRECT", 0x1062),
    ("LVM_SETGROUPMETRICS", 0x109B),
    ("LVM_GETGROUPMETRICS", 0x109C),
    ("LVM_ENABLEGROUPVIEW", 0x109D),
    ("LVM_SORTGROUPS", 0x109E),
    ("LVM_INSERTGROUPSORTED", 0x109F),
    ("LVM_REMOVEALLGROUPS", 0x10A0),
    ("LVM_HASGROUP", 0x10A1),
    ("LVM_GETGROUPSTATE", 0x105C),
    ("LVM_GETFOCUSEDGROUP", 0x105D),
    ("LVM_SETTILEVIEWINFO", 0x10A2),
    ("LVM_GETTILEVIEWINFO", 0x10A3),
    ("LVM_SETTILEINFO", 0x10A4),
    ("LVM_GETTILEINFO", 0x10A5),
    ("LVM_SETINSERTMARK", 0x10A6),
    ("LVM_GETINSERTMARK", 0x10A7),
    ("LVM_INSERTMARKHITTEST", 0x10A8),
    ("LVM_GETINSERTMARKRECT", 0x10A9),
    ("LVM_SETINSERTMARKCOLOR", 0x10AA),
    ("LVM_GETINSERTMARKCOLOR", 0x10AB),
    ("LVM_GETSELECTEDCOLUMN", 0x10AE),
    ("LVM_ISGROUPVIEWENABLED", 0x10AF),
    ("LVM_GETOUTLINECOLOR", 0x10B0),
    ("LVM_SETOUTLINECOLOR", 0x10B1),
    ("LVM_CANCELEDITLABEL", 0x10B3),
    ("LVM_MAPINDEXTOID", 0x10B4),
    ("LVM_MAPIDTOINDEX", 0x10B5),
    ("LVM_ISITEMVISIBLE", 0x10B6),
    ("LVM_GETEMPTYTEXT", 0x10CC),
    ("LVM_GETFOOTERRECT", 0x10CD),
    ("LVM_GETFOOTERINFO", 0x10CE),
    ("LVM_GETFOOTERITEMRECT", 0x10CF),
    ("LVM_GETFOOTERITEM", 0x10D0),
    ("LVM_GETITEMINDEXRECT", 0x10D1),
    ("LVM_SETITEMINDEXSTATE", 0x10D2),
    ("LVM_GETNEXTITEMINDEX", 0x10D3),
    ("LVM_SETUNICODEFORMAT", 0x2005), // CCM_SETUNICODEFORMAT
    ("LVM_GETUNICODEFORMAT", 0x2006), // CCM_GETUNICODEFORMAT

    // LVM_GETNEXTITEM options
    ("LVNI_ALL", 0x0000),
    ("LVNI_FOCUSED", 0x0001),
    ("LVNI_SELECTED", 0x0002),
    ("LVNI_CUT", 0x0004),
    ("LVNI_DROPHILITED", 0x0008),
    ("LVNI_STATEMASK", 0x000F),
    ("LVNI_VISIBLEORDER", 0x0010),
    ("LVNI_PREVIOUS", 0x0020),
    ("LVNI_VISIBLEONLY", 0x0040),
    ("LVNI_SAMEGROUPONLY", 0x0080),
    ("LVNI_ABOVE", 0x0100),
    ("LVNI_BELOW", 0x0200),
    ("LVNI_TOLEFT", 0x0400),
    ("LVNI_TORIGHT", 0x0800),
    ("LVNI_DIRECTIONMASK", 0x0F00),

    // ListView Notifications (LVN_FIRST = 0U-100U)
    ("LVN_ITEMCHANGING", 0xFFFFFF9C),
    ("LVN_ITEMCHANGED", 0xFFFFFF9B),
    ("LVN_INSERTITEM", 0xFFFFFF9A),
    ("LVN_DELETEITEM", 0xFFFFFF99),
    ("LVN_DELETEALLITEMS", 0xFFFFFF98),
    ("LVN_BEGINLABELEDITA", 0xFFFFFF97),
    ("LVN_BEGINLABELEDITW", 0xFFFFFF51),
    ("LVN_ENDLABELEDITA", 0xFFFFFF96),
    ("LVN_ENDLABELEDITW", 0xFFFFFF50),
    ("LVN_COLUMNCLICK", 0xFFFFFF94),
    ("LVN_BEGINDRAG", 0xFFFFFF93),
    ("LVN_BEGINRDRAG", 0xFFFFFF91),
    ("LVN_ODCACHEHINT", 0xFFFFFF8F),
    ("LVN_ODFINDITEMA", 0xFFFFFF68),
    ("LVN_ODFINDITEMW", 0xFFFFFF4D),
    ("LVN_ITEMACTIVATE", 0xFFFFFF8E),
    ("LVN_ODSTATECHANGED", 0xFFFFFF8D),
    ("LVN_HOTTRACK", 0xFFFFFF87),
    ("LVN_GETDISPINFOA", 0xFFFFFF6A),
    ("LVN_GETDISPINFOW", 0xFFFFFF4F),
    ("LVN_SETDISPINFOA", 0xFFFFFF69),
    ("LVN_SETDISPINFOW", 0xFFFFFF4E),
    ("LVN_KEYDOWN", 0xFFFFFF65),
    ("LVN_MARQUEEBEGIN", 0xFFFFFF64),
    ("LVN_GETINFOTIPA", 0xFFFFFF63),
    ("LVN_GETINFOTIPW", 0xFFFFFF62),
    ("LVN_INCREMENTALSEARCHA", 0xFFFFFF5E),
    ("LVN_INCREMENTALSEARCHW", 0xFFFFFF5D),
    ("LVN_COLUMNDROPDOWN", 0xFFFFFF5C),
    ("LVN_COLUMNOVERFLOWCLICK", 0xFFFFFF5A),
    ("LVN_BEGINSCROLL", 0xFFFFFF4C),
    ("LVN_ENDSCROLL", 0xFFFFFF4B),
    ("LVN_LINKCLICK", 0xFFFFFF48),
    ("LVN_GETEMPTYMARKUP", 0xFFFFFF45),

    // TabControl Styles
    ("TCS_SCROLLOPPOSITE", 0x0001),
    ("TCS_BOTTOM", 0x0002),
    ("TCS_RIGHT", 0x0002),
    ("TCS_MULTISELECT", 0x0004),
    ("TCS_FLATBUTTONS", 0x0008),
    ("TCS_FORCEICONLEFT", 0x0010),
    ("TCS_FORCELABELLEFT", 0x0020),
    ("TCS_HOTTRACK", 0x0040),
    ("TCS_VERTICAL", 0x0080),
    ("TCS_TABS", 0x0000),
    ("TCS_BUTTONS", 0x0100),
    ("TCS_SINGLELINE", 0x0000),
    ("TCS_MULTILINE", 0x0200),
    ("TCS_RIGHTJUSTIFY", 0x0000),
    ("TCS_FIXEDWIDTH", 0x0400),
    ("TCS_RAGGEDRIGHT", 0x0800),
    ("TCS_FOCUSONBUTTONDOWN", 0x1000),
    ("TCS_OWNERDRAWFIXED", 0x2000),
    ("TCS_TOOLTIPS", 0x4000),
    ("TCS_FOCUSNEVER", 0x8000),
    ("TCS_EX_FLATSEPARATORS", 0x00000001),
    ("TCS_EX_REGISTERDROP", 0x00000002),

    // TabControl Messages (TCM_FIRST = 0x1300)
    ("TCM_GETIMAGELIST", 0x1302),
    ("TCM_SETIMAGELIST", 0x1303),
    ("TCM_GETITEMCOUNT", 0x1304),
    ("TCIF_TEXT", 0x0001),
    ("TCIF_IMAGE", 0x0002),
    ("TCIF_RTLREADING", 0x0004),
    ("TCIF_PARAM", 0x0008),
    ("TCIF_STATE", 0x0010),
    ("TCIS_BUTTONPRESSED", 0x0001),
    ("TCIS_HIGHLIGHTED", 0x0002),
    // WideChar versions
    ("TCM_GETITEM", 0x133C),
    ("TCM_SETITEM", 0x133D),
    ("TCM_INSERTITEM", 0x133E),
    ("TCM_DELETEITEM", 0x1308),
    ("TCM_DELETEALLITEMS", 0x1309),
    ("TCM_GETITEMRECT", 0x130A),
    ("TCM_GETCURSEL", 0x130B),
    ("TCM_SETCURSEL", 0x130C),
    ("TCHT_NOWHERE", 0x0001),
    ("TCHT_ONITEMICON", 0x0002),
    ("TCHT_ONITEMLABEL", 0x0004),
    ("TCHT_ONITEM", 0x0006),
    ("TCM_HITTEST", 0x130D),
    ("TCM_SETITEMEXTRA", 0x130E),
    ("TCM_ADJUSTRECT", 0x1328),
    ("TCM_SETITEMSIZE", 0x1329),
    ("TCM_REMOVEIMAGE", 0x132A),
    ("TCM_SETPADDING", 0x132B),
    ("TCM_GETROWCOUNT", 0x132C),
    ("TCM_GETTOOLTIPS", 0x132D),
    ("TCM_SETTOOLTIPS", 0x132E),
    ("TCM_GETCURFOCUS", 0x132F),
    ("TCM_SETCURFOCUS", 0x1330),
    ("TCM_SETMINTABWIDTH", 0x1331),
    ("TCM_DESELECTALL", 0x1332),
    ("TCM_HIGHLIGHTITEM", 0x1333),
    ("TCM_SETEXTENDEDSTYLE", 0x1334),
    ("TCM_GETEXTENDEDSTYLE", 0x1335),
    ("TCM_SETUNICODEFORMAT", 0x2005),
    ("TCM_GETUNICODEFORMAT", 0x2006),

    // TabControl Notifications (TCN_FIRST = 0U-550U); registered via U_ENUM
    // in the reference so scripts can compare against Notify.code (UINT).
    ("TCN_KEYDOWN", 0xFFFFFDDA),
    ("TCN_SELCHANGE", 0xFFFFFDD9),
    ("TCN_SELCHANGING", 0xFFFFFDD8),
    ("TCN_GETOBJECT", 0xFFFFFDD7),
    ("TCN_FOCUSCHANGE", 0xFFFFFDD6),

    // OwnerDraw
    ("ODT_BUTTON", 4),
    ("ODT_COMBOBOX", 3),
    ("ODT_LISTBOX", 2),
    ("ODT_MENU", 1),
    ("ODT_LISTVIEW", 102),
    ("ODT_STATIC", 5),
    ("ODT_TAB", 101),

    ("ODA_DRAWENTIRE", 0x0001),
    ("ODA_FOCUS", 0x0004),
    ("ODA_SELECT", 0x0002),

    ("ODS_CHECKED", 0x0008),
    ("ODS_DISABLED", 0x0004),
    ("ODS_FOCUS", 0x0010),
    ("ODS_GRAYED", 0x0002),
    ("ODS_SELECTED", 0x0001),
    ("ODS_COMBOBOXEDIT", 0x1000),
    ("ODS_DEFAULT", 0x0020),

    // PropertySheet Notifications (PSN_FIRST = 0U-200U)
    ("PSN_SETACTIVE", 0xFFFFFF38),
    ("PSN_KILLACTIVE", 0xFFFFFF37),
    ("PSN_APPLY", 0xFFFFFF36),
    ("PSN_RESET", 0xFFFFFF35),
    ("PSN_HELP", 0xFFFFFF33),
    ("PSN_WIZBACK", 0xFFFFFF32),
    ("PSN_WIZNEXT", 0xFFFFFF31),
    ("PSN_WIZFINISH", 0xFFFFFF30),
    ("PSN_QUERYCANCEL", 0xFFFFFF2F),
    ("PSN_GETOBJECT", 0xFFFFFF2E),
    ("PSN_TRANSLATEACCELERATOR", 0xFFFFFF2C),
    ("PSN_QUERYINITIALFOCUS", 0xFFFFFF2B),

    ("PSNRET_NOERROR", 0),
    ("PSNRET_INVALID", 1),
    ("PSNRET_INVALID_NOCHANGEPAGE", 2),
    ("PSNRET_MESSAGEHANDLED", 3),

    // PropertySheet Messages (WM_USER based)
    ("PSM_SETCURSEL", 0x0465),
    ("PSM_REMOVEPAGE", 0x0466),
    ("PSM_ADDPAGE", 0x0467),
    ("PSM_CHANGED", 0x0468),
    ("PSM_RESTARTWINDOWS", 0x0469),
    ("PSM_REBOOTSYSTEM", 0x046A),
    ("PSM_CANCELTOCLOSE", 0x046B),
    ("PSM_QUERYSIBLINGS", 0x046C),
    ("PSM_UNCHANGED", 0x046D),
    ("PSM_APPLY", 0x046E),
    ("PSM_SETTITLE", 0x0478), // PSM_SETTITLEW
    ("PSM_SETWIZBUTTONS", 0x0470),
    ("PSM_PRESSBUTTON", 0x0471),
    ("PSM_SETCURSELID", 0x0472),
    ("PSM_SETFINISHTEXT", 0x0479), // PSM_SETFINISHTEXTW
    ("PSM_GETTABCONTROL", 0x0474),
    ("PSM_ISDIALOGMESSAGE", 0x0475),
    ("PSM_GETCURRENTPAGEHWND", 0x0476),
    ("PSM_INSERTPAGE", 0x0477),
    ("PSM_SETHEADERTITLE", 0x047E), // PSM_SETHEADERTITLEW
    ("PSM_SETHEADERSUBTITLE", 0x0480), // PSM_SETHEADERSUBTITLEW
    ("PSM_HWNDTOINDEX", 0x0481),
    ("PSM_INDEXTOHWND", 0x0482),
    ("PSM_PAGETOINDEX", 0x0483),
    ("PSM_INDEXTOPAGE", 0x0484),
    ("PSM_IDTOINDEX", 0x0485),
    ("PSM_INDEXTOID", 0x0486),
    ("PSM_GETRESULT", 0x0487),
    ("PSM_RECALCPAGESIZES", 0x0488),
    ("PSM_SETNEXTTEXT", 0x0489), // PSM_SETNEXTTEXTW
    ("PSM_SHOWWIZBUTTONS", 0x048A),
    ("PSM_ENABLEWIZBUTTONS", 0x048B),
    ("PSM_SETBUTTONTEXT", 0x048C), // PSM_SETBUTTONTEXTW
    ("PSM_SETHEADERBITMAP", 0x048D),
    ("PSM_SETHEADERBITMAPRESOURCE", 0x048E),

    ("PSWIZB_BACK", 0x00000001),
    ("PSWIZB_NEXT", 0x00000002),
    ("PSWIZB_FINISH", 0x00000004),
    ("PSWIZB_DISABLEDFINISH", 0x00000008),
    ("PSWIZBF_ELEVATIONREQUIRED", 0x00000001),
    ("PSWIZB_CANCEL", 0x00000010),
    ("PSWIZB_SHOW", 0),
    ("PSWIZB_RESTORE", 1),
    ("PSWIZF_SETCOLOR", 0xFFFFFFFF), // (UINT)-1

    ("PSBTN_BACK", 0),
    ("PSBTN_NEXT", 1),
    ("PSBTN_FINISH", 2),
    ("PSBTN_OK", 3),
    ("PSBTN_APPLYNOW", 4),
    ("PSBTN_CANCEL", 5),
    ("PSBTN_HELP", 6),
    ("PSBTN_MAX", 6),

    ("ID_PSRESTARTWINDOWS", 0x2),
    ("ID_PSREBOOTSYSTEM", 0x3),

    ("WIZ_CXDLG", 276),
    ("WIZ_CYDLG", 140),
    ("WIZ_CXBMP", 80),
    ("WIZ_BODYX", 92),
    ("WIZ_BODYCX", 184),

    ("PROP_SM_CXDLG", 212),
    ("PROP_SM_CYDLG", 188),
    ("PROP_MED_CXDLG", 227),
    ("PROP_MED_CYDLG", 215),
    ("PROP_LG_CXDLG", 252),
    ("PROP_LG_CYDLG", 218),
];

// Common-control window class names (registered as strings in the reference).
const STR_CONSTANTS: &[(&str, &str)] = &[
    ("HEADER", "SysHeader32"),
    ("LINK", "SysLink"),
    ("LISTVIEW", "SysListView32"),
    ("TREEVIEW", "SysTreeView32"),
    ("TABCONTROL", "SysTabControl32"),
    ("IPADDRESS", "SysIPAddress32"),
    ("PAGESCROLLER", "SysPager"),
    ("ANIMATE", "SysAnimate32"),
    ("MONTHCAL", "SysMonthCal32"),
    ("DATETIMEPICK", "SysDateTimePick32"),
    ("COMBOBOXEX", "ComboBoxEx32"),
    ("NATIVEFONTCTL", "NativeFontCtl"),
    ("TOOLBAR", "ToolbarWindow32"),
    ("REBAR", "ReBarWindow32"),
    ("TOOLTIPS", "tooltips_class32"),
    ("STATUS", "msctls_statusbar32"),
    ("TRACKBAR", "msctls_trackbar32"),
    ("UPDOWN", "msctls_updown32"),
    ("PROGRESS", "msctls_progress32"),
    ("HOTKEY", "msctls_hotkey32"),
];
