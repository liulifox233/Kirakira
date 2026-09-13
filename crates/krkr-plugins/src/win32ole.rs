//! `win32ole.dll` — the OLE automation bridge: the script-facing object model
//! and its failure semantics are ported member by member, the COM/IDispatch
//! transport is Windows-only and is reported the way the reference itself
//! reports a failed connection.
//!
//! # Reference
//!
//! Unlike `menu.dll`, this plugin has official source. The copy followed here
//! is the krkr2 trunk tree, `kirikiri2/src/plugins/win32/win32ole/`:
//!
//! * `main.cpp` (1 252 lines) — the two NCB classes, their factories, the
//!   event sink and the `CArchive` URL protocol's registration hooks;
//! * `IDispatchWrapper.cpp` / `.hpp` (734 / 227 lines) — the VARIANT ↔
//!   tTJSVariant marshalling and the TJS-side call into `IDispatch::Invoke`;
//! * `CArchive.cpp` (221 lines) — an `IInternetProtocol`/`IClassFactory` that
//!   serves XP3 members to hosted controls; it registers no TJS surface;
//! * `manual.tjs` (140 lines) — the author's pseudo-manual. Its entries are
//!   checked against the code below, and two of them are not code: the
//!   `ActiveX` default argument list (the factory implements different
//!   defaults, `main.cpp:995-1004`) and the `onCreate`/`onDestroy` events
//!   (their calls are commented out in this revision, `:936-937`, `:944-945`)
//!   — the same manual-vs-code gap the `win32dialog` port records for
//!   `onKeyDown`/`onKeyUp`.
//!
//! Kirikiroid2 has no portable copy (`Kirikiroid2/src/plugins/` holds
//! `win32dialog`, `dirlist`, … but no `win32ole`), and the krkrz tree's
//! `win32ole/` is the same implementation, so the surface is stated from the
//! trunk source alone. All sources are Shift-JIS.
//!
//! # The surface, exactly as the reference registers it
//!
//! Two globals, each a native class object registered under its own name
//! (`global->PropSet(..., _className, ...)`, `ncbind.hpp:1902-1924`, removed
//! again by the same auto-register on unlink, `:1936-1940`):
//!
//! * `WIN32OLE` — factory `WIN32OLE(objthis, progIdOrCLSID)` (`main.cpp:225-233`
//!   over `:193-215`), registered as the member named after the class
//!   (`ncbind.hpp:1777-1784`; in TJS that is what `super.WIN32OLE(name)` and
//!   `_X.WIN32OLE(name)` reach, `classes.rs:67-95`), and the members `invoke`,
//!   `set`, `get`, `missing`, `addEvent`, `getConstant` (`:644-652`). The
//!   instance's `missing` is also installed as its missing-member hook
//!   (`ClassInstanceInfo(TJS_CII_SET_MISSING, 0, "missing")`, `:213-214`).
//! * `ActiveX` — factory `ActiveX(objthis, numparams, param)` (`:1055-1064`
//!   over `:995-1029`) with its own copies of those six (`:1169-1176`),
//!   `setExternalUI`, `setPos`, `setSize` (`:1177-1179`), the read-only
//!   property `isValidWindow` (`:1180`) and `visible`, `left`, `top`, `width`,
//!   `height` (`:1181-1185`). Every instance is also a `WIN32OLE` surface
//!   holder because its constructor chains into `WIN32OLE(objthis, NULL)`
//!   (`:995`), which installs the hook and the six members; the manual's
//!   `class ActiveX extends WIN32OLE` (`manual.tjs:59`) documents that shared
//!   surface, not a TJS class relationship — NCB registers two independent
//!   classes and neither is a superclass of the other.
//!
//! Both classes additionally carry the empty `finalize` ncbind adds to every
//! class it registers (`ncbind.hpp:1875-1877`, the comment reads
//! 空のfinalizeメソッドを追加).
//!
//! Argument contracts, all of them the reference's:
//!
//! * `WIN32OLE`/`ActiveX` need one argument (`main.cpp:226-228`, `:1057-1059`).
//! * `ActiveX`'s window argument: when `numparams >= 2` and the second
//!   argument is a `tvtObject`, the object must be a `Window` instance or the
//!   constructor throws `must set window object` (`main.cpp:1007-1024`). The
//!   four geometry arguments are read **only** when `numparams >= 6`
//!   (`:999-1004`), so a partial list is ignored entirely and the C++ defaults
//!   `left = 0, top = 0, width = -1, height = -1` (`:995`) stand — not the
//!   `width=100, height=100` the manual declares (`manual.tjs:71`).
//! * `addEvent` needs a name (`main.cpp:517-519`) and converts its receiver
//!   with `AsObject` (`:526`); `missing` needs three arguments (`:299`);
//!   `getConstant` converts a given target with `AsObject` (`:621`);
//!   `setPos`/`setSize` need two each, because ncbind's `Method` wrapper
//!   rejects a short call (`ncbind.hpp:1186`).
//! * Any member reached without a native instance — `WIN32OLE.invoke(...)` on
//!   the class object, and an object whose factory never ran — answers
//!   `TJS_E_NATIVECLASSCRASH`, not an instance answer (`ncbind.hpp:1547` for
//!   `RawCallback`, `:1133` for `Method`, `:1486` for a property); the factory
//!   that removes that state is the subject of the next section.
//!
//! # The split this port is built on
//!
//! **(a) platform-independent script plumbing** — the class objects and their
//! member tables, the argument checks and conversions above, the
//! missing-member hook and its protocol, the `ActiveX` geometry state, and
//! every error and failure answer. All of that is implemented and tested here.
//!
//! **(b) Windows-only transport** — `OleInitialize`/`AtlAxWinInit`
//! (`main.cpp:1199-1215`), `CLSIDFromProgID`/`CLSIDFromString` and
//! `CoCreateInstance` for `WIN32OLE` (`:196-208`), the ATL `CAxWindow` host
//! with `CreateControl`/`QueryControl` for `ActiveX` (`:893-938`), `AtlAdvise`
//! event sinks (`:494-511`), the `ITypeInfo`/`ITypeLib` walk that harvests
//! constants (`:545-611`) and the `CArchive` URL protocol. None of it exists
//! on any target this crate builds for, and no crate in the dependency graph
//! provides it, so the reference's `IDispatch *pDispatch` (`main.cpp:151`) is
//! always absent.
//!
//! # What an absent dispatch means, member by member
//!
//! * `new WIN32OLE(name)` and `new ActiveX(name, …)` **succeed** and return an
//!   object, exactly as the reference's constructors do for an identifier they
//!   cannot resolve: they log and leave `pDispatch` NULL (`main.cpp:196-211`).
//!   The log names the step that would have failed — `bad CLSID <name>` when
//!   neither `CLSIDFromProgID` nor `CLSIDFromString` can own the text
//!   (`:210`), `CoCreateInstance failed <name>` for text that is a CLSID,
//!   which `CLSIDFromString` does parse (`:199`, `:207`). `ActiveX` reports
//!   `CreateControl failed <progId>` (`:932`): hosting the control is the step
//!   that cannot run. The plugin's transport report is logged once at
//!   registration, in the place `PreRegistCallback` logs its failed
//!   `OleInitialize` (`:1202-1206`).
//! * `invoke`, `set`, `get` — **always** `TJS_E_FAIL` (`Unknown failure :
//!   FFFFFFFF`), whatever the argument count or type: the reference tests
//!   `pDispatch` before it looks at its arguments (`main.cpp:251`, `:275`,
//!   `:291`), so on a cleared object the `numparams == 0` and non-string-name
//!   checks below those guards are unreachable and this port does not run them
//!   either.
//! * **Every member needs the factory call that created the object.** NCB
//!   keeps its C++ object behind a native instance that only the factory
//!   registers (`SetAdaptorWithNativeInstance`, `ncbind.hpp:186-192`); the
//!   class-body call the `extends` clause makes copies the class's members
//!   without one (`tTJSNativeClass::FuncCall`, `tjsNative.cpp:302-321`, whose
//!   `CreateNativeInstance` answers NULL, `tjsNative.h:204`), so a
//!   `class X extends WIN32OLE` instance that never calls
//!   `super.WIN32OLE(name)` answers `TJS_E_NATIVECLASSCRASH` from every
//!   member — the same NULL instance lookup as a class-object call
//!   (`ncbind.hpp:1049-1052`, `:1547`). `super.WIN32OLE(name)` runs the
//!   factory on the instance and attaches it (`main.cpp:225-233`).
//! * `missing(set, name, value)` — the instance's missing-member hook
//!   (`main.cpp:213-214`). It answers 0/false in both directions because the
//!   guarded `invoke` fails, i.e. it *declines* (`ret` is assigned, `:322-324`
//!   — `tjsObject.cpp:477-521` treats a false answer exactly like "no missing
//!   hook"). A declined hook leaves the engine's ordinary member semantics in
//!   place: reading an absent member raises `Member "x" does not exist`
//!   (the reference falls through to `TJS_E_MEMBERNOTFOUND`,
//!   `tjsObject.cpp:1337-1345`), and writing one creates the member on the
//!   instance, which is what `tTJSCustomObject::PropSet` does after a declined
//!   hook when the store carries `TJS_MEMBERENSURE` (`tjsObject.cpp:1495-1509`).
//! * `addEvent(name[, receiver])` — validates like the reference (a name is
//!   required, the receiver must be an object) and then reports the
//!   registration failure: `findIID`'s `GetTypeInfo`/`GetContainingTypeLib`
//!   walk and `AtlAdvise` (`main.cpp:426-511`) need a live dispatch, so the
//!   call logs `イベント[<name>]の登録に失敗しました` (`:534-536`) and answers
//!   void — `TJS_S_OK` in the reference, which never turns a failed
//!   registration into an exception.
//! * `getConstant([target])` — a no-op answering void, because its whole body
//!   is guarded by `if (pDispatch)` (`main.cpp:596-611`); a non-object target
//!   still raises the reference's object-conversion error (`:621`).
//! * `ActiveX.setExternalUI`, `setPos`, `setSize`, `left`, `top`, `width`,
//!   `height` — **usable**: the four geometry values are plain int fields in
//!   the reference (`main.cpp:865-868`, `:1096-1148`) and only `_setPos`'s
//!   `SetWindowPos` (`:954-958`) has nothing to move; `setExternalUI` returns
//!   void whether or not a window exists (`:1070-1077`).
//! * `ActiveX.visible` and `isValidWindow` — always 0 here, because the
//!   getters read the window handle (`m_hWnd && IsWindowVisible()`,
//!   `main.cpp:1088-1094`) and this engine hosts no ATL window: the port keeps
//!   the reference's own window-less answer. A `visible` write is accepted and
//!   dropped (the reference's `setVisible` is a no-op without a window,
//!   `:1079-1086`), an `isValidWindow` write is refused with
//!   `TJS_E_ACCESSDENYED` (`NCB_PROPERTY_RO`, `:1180`).
//!
//! # Deliberate deviations
//!
//! * `new ActiveX(name, null)` takes the standalone-window branch. The
//!   reference reads the null object's `AsObjectNoAddRef` — a NULL
//!   `iTJSDispatch2` (`tjsVariant.h:681-686`) — and calls `IsInstanceOf` on it
//!   (`main.cpp:1009-1010`), which is undefined behaviour; this port treats
//!   the null object as "no window argument", the branch the code takes for an
//!   omitted argument (`:1027`) and the default the manual documents.
//! * `addEvent` on a cleared object reports the failure the surrounding code
//!   describes instead of crashing: the reference's `findIID` calls
//!   `pDispatch->GetTypeInfo` with no NULL check (`main.cpp:434`), so its
//!   actual behaviour there is a null-dereference, while `_addEventMethod` is
//!   written to log a failed registration and answer `TJS_S_OK`
//!   (`:534-537`).
//! * The `Window` message receiver the `ActiveX` constructor registers
//!   (`setReceiver`, `main.cpp:876-886`, replying to `TVP_WM_ATTACH`/`DETACH`
//!   through `messageHandler`, `:1151-1167`) is not registered: it hands a
//!   native function pointer and a C++ `this` through TJS as integers, and
//!   this engine has no message channel into a hosted control. Nothing
//!   observable is lost while no window can be created at all.
//! * Per-object state lives in the instance's own property objects instead of
//!   a C++ native instance — the same substitution the sibling `windowEx` port
//!   records, because a TJS object here has no native instance to carry it. The
//!   marker that stands in for "the factory attached a native instance" is the
//!   instance member `__win32ole_instance`, and `super.WIN32OLE()` without an
//!   identifier is the subclass initializer rather than the factory's
//!   `TJS_E_BADPARAMCOUNT` (`main.cpp:226-228`): one call cannot be both, and
//!   the engine resolves every native class the same way (`classes.rs:106-117`).

use std::sync::{Arc, Mutex, MutexGuard};

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{Closure, NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "WIN32OLE / ActiveX OLE automation classes",
    notes: "Surface and script plumbing of win32ole/main.cpp ported member by member: the two \
            class objects, the members (WIN32OLE: invoke/set/get/missing/addEvent/getConstant; \
            ActiveX adds setExternalUI/setPos/setSize and the isValidWindow/visible/left/top/\
            width/height properties, plus ncbind's auto finalize), the missing-member hook, the \
            argument checks and conversions that run before the transport (factory counts, \
            addEvent/missing counts, the ActiveX Window argument, the six-argument geometry \
            rule), the ActiveX geometry state, and every error answer. The classes' constructor \
            members (`WIN32OLE`/`ActiveX`, NCB's class-name NCM) run the factory on the instance, \
            so `super.WIN32OLE(name)` attaches the OLE instance while members reached without one \
            answer TJS_E_NATIVECLASSCRASH, like the reference's missing native instance. The \
            COM/IDispatch transport is Windows-only and absent on every target this crate builds \
            for, so every object is created in the state the reference leaves when \
            CLSIDFromProgID/CoCreateInstance fail: invoke/set/get always report TJS_E_FAIL, \
            addEvent logs the reference's registration failure, getConstant is a no-op, and no \
            member can reach a real OLE object.",
    install: |engine| engine.register_plugin(Win32OlePlugin),
};

pub struct Win32OlePlugin;

impl KrkrPlugin for Win32OlePlugin {
    fn name(&self) -> &str {
        "win32ole.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // `PreRegistCallback` runs `OleInitialize` and `AtlAxWinInit`
        // (`main.cpp:1199-1215`) and logs `OLE 初期化失敗` when the first fails
        // (`:1205`). Both belong to the transport half this platform does not
        // have, so the report is made once, here, and the classes install
        // anyway — the reference installs its classes even after that failure.
        runtime.host_mut().log(
            "win32ole.dll: COM/ATL transport unavailable on this platform (no OleInitialize, \
             CLSIDFromProgID/CoCreateInstance or ATL window host); WIN32OLE/ActiveX objects are \
             created the way the reference leaves them when the connection fails, with no \
             dispatch",
        );
        install_classes(runtime);
        Ok(())
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // ncbind's auto-register deletes each class it registered from the
        // script global when the plugin is unloaded (`ncbind.hpp:1936-1940`).
        for name in [WIN32OLE_CLASS, ACTIVEX_CLASS] {
            runtime.delete_object_member(runtime.global_handle(), name);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The reference's own failure codes
// ---------------------------------------------------------------------------

/// `TJS_E_FAIL` (`tjsErrorDefs.h:39`, `-1`), the code every guarded dispatch
/// member returns when `pDispatch` is NULL (`main.cpp:251`, `:259`, `:291`).
/// `TJSThrowFrom_tjs_error` has no named case for it (`tjsError.cpp:265-271`),
/// so a script sees the generic text the sibling `windowEx` port already
/// carries for `setMessageHook`.
fn dispatch_unavailable() -> TjsError {
    TjsError::runtime("Unknown failure : FFFFFFFF")
}

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

const WIN32OLE_CLASS: &str = "WIN32OLE";
const ACTIVEX_CLASS: &str = "ActiveX";

/// The marker a factory call leaves on the object it initialized — this port's
/// stand-in for the native instance NCB registers
/// (`SetAdaptorWithNativeInstance`, `ncbind.hpp:186-192`). A class-body call
/// copies the class members without it (`tTJSNativeClass::FuncCall`,
/// `tjsNative.cpp:302-321`), and NCB then answers `TJS_E_NATIVECLASSCRASH`
/// from every member whose native instance lookup fails
/// (`ncbind.hpp:1049-1052`, `:1547`).
const OLE_INSTANCE_MARKER: &str = "__win32ole_instance";

/// The members `NCB_REGISTER_CLASS(WIN32OLE)` registers (`main.cpp:644-652`),
/// minus `finalize`: ncbind adds that one as a native class *method*
/// (`ncbind.hpp:1875-1877`, whose callback is empty), so it answers void on
/// every receiver instead of going through the instance lookup.
const OLE_METHODS: &[&str] = &["invoke", "set", "get", "missing", "addEvent", "getConstant"];

/// `ActiveX`'s own additions to that list (`main.cpp:1177-1179`).
const ACTIVEX_METHODS: &[&str] = &["setExternalUI", "setPos", "setSize"];

/// `ActiveX`'s properties (`main.cpp:1180-1185`).
const ACTIVEX_PROPERTIES: &[(&str, NativePropertyAccess)] = &[
    ("isValidWindow", NativePropertyAccess::ReadOnly),
    ("visible", NativePropertyAccess::ReadWrite),
    ("left", NativePropertyAccess::ReadWrite),
    ("top", NativePropertyAccess::ReadWrite),
    ("width", NativePropertyAccess::ReadWrite),
    ("height", NativePropertyAccess::ReadWrite),
];

fn install_classes(runtime: &mut Runtime<KrkrHost>) {
    // The class's argument count is the factory's own `numparams < 1` check
    // (`main.cpp:226-228`, `:1057-1059`), and it is made inside the handler
    // rather than declared here: the same object also serves the class-body
    // call that initializes a subclass instance, which passes no arguments.
    // The engine's own native classes resolve that the same way
    // (`classes.rs:106-117`).
    let win32ole = runtime.alloc_native_constructor(construct_win32ole);
    runtime.add_object_class_info(win32ole, WIN32OLE_CLASS);
    install_class_members(runtime, win32ole, false);
    install_class_name_constructor(runtime, win32ole, WIN32OLE_CLASS);
    runtime.set_global_member(WIN32OLE_CLASS, Variant::Object(win32ole));

    let activex = runtime.alloc_native_constructor(construct_activex);
    runtime.add_object_class_info(activex, ACTIVEX_CLASS);
    install_class_members(runtime, activex, true);
    install_class_name_constructor(runtime, activex, ACTIVEX_CLASS);
    runtime.set_global_member(ACTIVEX_CLASS, Variant::Object(activex));
}

/// NCB registers each class's factory as a member named after the class
/// (`TJSNativeClassRegisterNCM(_classobj, _className, ...)`,
/// `ncbind.hpp:1777-1784`, registered under that name by `RegistItem`
/// (`:1883-1894`)), and krkrz's native classes publish their
/// constructors the same way (`RegisterNCM("Layer", ...)`, `tjsNative.h:233`).
/// That member is what `super.WIN32OLE(name)` calls — TJS2 resolves the call's
/// objthis as `clo.ObjThis ? clo.ObjThis : ra[-1]` (`tjsInterCodeExec.cpp:2015`)
/// — and the engine's native classes install it with the same shape
/// (`classes.rs:83-95`).
fn install_class_name_constructor(
    runtime: &mut Runtime<KrkrHost>,
    class: ObjectHandle,
    name: &'static str,
) {
    runtime.set_object_member(class, name, Variant::Closure(Closure::new(class, None)));
}

/// The class object carries the reference's member table because NCB registers
/// the members on it (`main.cpp:644-652`, `:1169-1186`), and every one of them
/// answers `TJS_E_NATIVECLASSCRASH` there: NCB looks the calling object's
/// native instance up before it runs a callback and reports exactly that when
/// there is none (`ncbind.hpp:1547`, `:1133`, `:1486`). `activex` selects the
/// `ActiveX` additions.
fn install_class_members(runtime: &mut Runtime<KrkrHost>, class: ObjectHandle, activex: bool) {
    // ncbind's empty `finalize` is a native class method, not a callback with
    // an instance lookup, so it answers void on the class object too
    // (`ncbind.hpp:1875-1877`).
    runtime.register_object_native(class, "finalize", returns_void);
    for name in OLE_METHODS {
        runtime.register_object_native(class, *name, no_native_instance);
    }
    if !activex {
        return;
    }
    for name in ACTIVEX_METHODS {
        runtime.register_object_native(class, *name, no_native_instance);
    }
    for (name, access) in ACTIVEX_PROPERTIES {
        runtime.register_object_native_property_with_access(
            class,
            *name,
            *access,
            no_native_instance_property,
            no_native_instance_property_set,
        );
    }
}

/// A class-object call: `GetNativeInstance` finds nothing, which NCB answers
/// with `TJS_E_NATIVECLASSCRASH` (`ncbind.hpp:1547`).
fn no_native_instance(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Err(TjsError::native_class_crash())
}

fn no_native_instance_property(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    Err(TjsError::native_class_crash())
}

/// The setter of the class-object properties. On a read-only property NCB's
/// dummy setter answers first (`ncbind.hpp:1485`), so a class-object write to
/// `isValidWindow` is an access denial rather than a crash — the engine's own
/// access policy enforces that before this runs.
fn no_native_instance_property_set(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _value: Variant,
) -> Result<()> {
    Err(TjsError::native_class_crash())
}

/// ncbind's automatically added `finalize` (`ncbind.hpp:1875-1877`), an empty
/// class method that answers void on every receiver.
fn returns_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

/// `setExternalUI` (`main.cpp:1070-1077`): it creates the IE-only UI handler
/// and attaches it when a window exists, so without one its void answer is all
/// that is left.
fn activex_set_external_ui(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    require_ole_instance(runtime, this_obj)?;
    Ok(Variant::Void)
}

/// The answers a `Window`-less object gives: `m_hWnd` is NULL, so
/// `isValidWindow` and `visible` read false (`main.cpp:1088-1094`).
fn property_zero(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    require_ole_instance(runtime, this_obj)?;
    Ok(Variant::Integer(0))
}

/// `setVisible` (`main.cpp:1079-1086`) with no window to show: the argument is
/// converted (NCB converts it before the setter runs) and nothing is stored,
/// because the getter reads the window rather than a field.
fn visible_setter(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    value: Variant,
) -> Result<()> {
    require_ole_instance(runtime, this_obj)?;
    integer_argument(&value)?;
    Ok(())
}

/// The setter behind a read-only property (`main.cpp:1180`): the engine's
/// access policy refuses a script write before it runs, and it stores nothing.
fn read_only_setter(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _value: Variant,
) -> Result<()> {
    require_ole_instance(runtime, this_obj)?;
    Ok(())
}

/// An argument the declaration above has already counted. The lookup keeps a
/// caller that reached the handler without one from panicking in a native.
fn argument(args: &[Variant], index: usize) -> Result<&Variant> {
    args.get(index).ok_or_else(TjsError::bad_param_count)
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// `WIN32OLE::factory` (`main.cpp:225-233`) over `WIN32OLE::WIN32OLE`
/// (`:193-215`), and the class-body call that initializes a subclass instance
/// (`tTJSNativeClass::FuncCall`, `tjsNative.cpp:302-321`).
///
/// The two are one closure here because an engine-level native class is one
/// object, the way it is for the engine's own classes (`classes.rs:42-65`): a
/// call with an instance bound initializes that instance, a call without one
/// is the factory. Only the factory runs the constructor body, so only it
/// reports the connection failure and attaches the OLE instance
/// (`super.WIN32OLE(name)` reaches the same factory through the class-name
/// member, while the `extends` call carries no identifier).
fn construct_win32ole(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `SysAllocString(progIdOrCLSID)` (`main.cpp:197`) is the conversion; an
    // argument that cannot convert to a string raises instead.
    let identifier = match args.first() {
        Some(value) => Some(value.to_tjs_string()?),
        None => None,
    };
    let target = construction_target(runtime, this_obj);
    // `WIN32OLE::factory`'s `numparams < 1` (`main.cpp:226-228`), checked where
    // the reference checks it: the factory runs only for the `new` form (and
    // for `super.WIN32OLE(...)`, which always carries an identifier here).
    if target.is_none() && identifier.is_none() {
        return Err(TjsError::bad_param_count());
    }

    let instance = target.unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(instance, WIN32OLE_CLASS);
    install_ole_members(runtime, instance);
    install_class_name_constructor_on(runtime, instance, WIN32OLE_CLASS, instance);
    if let Some(identifier) = identifier {
        log_connection_failure(runtime, &identifier);
        attach_ole_instance(runtime, instance);
    }
    Ok(Variant::Object(instance))
}

/// `ActiveX::factory` (`main.cpp:1055-1064`) over `ActiveX::ActiveX`
/// (`:995-1029`), plus the subclass-initialization call, which
/// `WIN32OLE(objthis, NULL)` (`:995`) makes it a member of as well.
///
/// The reference's order is kept exactly: the identifier, then the geometry
/// **only** when six arguments are present (`:999-1004`), then the window
/// argument (`:1006-1028`). That order is observable — a conversion that fails
/// in the geometry list raises before the window check runs.
fn construct_activex(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let identifier = match args.first() {
        Some(value) => Some(value.to_tjs_string()?),
        None => None,
    };
    let target = construction_target(runtime, this_obj);
    if target.is_none() && identifier.is_none() {
        return Err(TjsError::bad_param_count());
    }

    // `left(0), top(0), width(-1), height(-1)` (`main.cpp:995`); the manual's
    // documented `width=100, height=100` is not the factory's default.
    let mut geometry = Geometry {
        left: 0,
        top: 0,
        width: -1,
        height: -1,
    };
    if args.len() >= 6 {
        geometry.left = integer_argument(&args[2])?;
        geometry.top = integer_argument(&args[3])?;
        geometry.width = integer_argument(&args[4])?;
        geometry.height = integer_argument(&args[5])?;
    }

    // `param[1]->Type() == tvtObject` (`main.cpp:1007`): the second argument
    // describes a parent window or is ignored. A `Window` instance is accepted
    // (`:1010`); any other object throws (`:1023`). The null object is the one
    // divergence — see the module docs.
    if let Some(handle) = args.get(1).and_then(Variant::object_handle)
        && (!object_is_instance_of(runtime, handle, "Window")
            || runtime.global_member("Window").object_handle() == Some(handle))
    {
        return Err(TjsError::runtime("must set window object"));
    }

    let instance = target.unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(instance, ACTIVEX_CLASS);
    install_ole_members(runtime, instance);
    install_activex_members(runtime, instance, Arc::new(Mutex::new(geometry)));
    install_class_name_constructor_on(runtime, instance, ACTIVEX_CLASS, instance);
    if let Some(identifier) = identifier {
        // `CreateControl` (`main.cpp:917`) needs the ATL host that does not
        // exist here; the reference logs this message when the control cannot
        // be created (`:932`) and the object stays without a dispatch.
        runtime
            .host_mut()
            .log(&format!("win32ole.dll: CreateControl failed {identifier}"));
        attach_ole_instance(runtime, instance);
    }
    Ok(Variant::Object(instance))
}

/// The object a construction call initializes: the instance the call carries
/// (subclass initialization, where TJS2 binds the caller's `this`), or `None`
/// for the `new` form, which allocates. A call made on the global object is
/// the engine's own convention for "no bound instance" (`classes.rs:119-122`).
fn construction_target(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
}

/// `WIN32OLE::WIN32OLE`'s tail (`main.cpp:213-214`), run only by the factory
/// (including `ActiveX`'s chained `WIN32OLE(objthis, NULL)`, `:995`).
///
/// The marker is this port's stand-in for the native instance NCB registers
/// (`SetAdaptorWithNativeInstance`, `ncbind.hpp:186-192`): the class-body call
/// copies the class members without one, and NCB answers
/// `TJS_E_NATIVECLASSCRASH` for every member on an object whose native
/// instance lookup fails (`ncbind.hpp:1049-1052`).
fn attach_ole_instance(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle) {
    runtime.set_object_member(instance, OLE_INSTANCE_MARKER, Variant::Integer(1));
    runtime.set_object_call_missing(instance, "missing");
}

/// The OLE instance behind a member call, or `None` when there is none: the
/// class object, an object that only ran the `extends` call, and every other
/// receiver. NCB looks its C++ instance up the same way, and reports
/// `TJS_E_NATIVECLASSCRASH` when the lookup finds nothing
/// (`ncbind.hpp:1049-1052`, `:1547`).
fn ole_instance(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    let handle = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())?;
    (!matches!(
        runtime.object_member(handle, OLE_INSTANCE_MARKER),
        Variant::Void
    ))
    .then_some(handle)
}

/// [`ole_instance`] as the members use it.
fn require_ole_instance(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<ObjectHandle> {
    ole_instance(runtime, this_obj).ok_or_else(TjsError::native_class_crash)
}

/// [`install_class_name_constructor`] for an instance, which receives the
/// bound copy of the class-name member NCB's member copy installs
/// (`tjsNative.cpp:295`, `classes.rs:130-135`).
fn install_class_name_constructor_on(
    runtime: &mut Runtime<KrkrHost>,
    instance: ObjectHandle,
    name: &'static str,
    bound_to: ObjectHandle,
) {
    let Some(class) = runtime.global_member(name).object_handle() else {
        return;
    };
    runtime.set_object_member(
        instance,
        name,
        Variant::Closure(Closure::new(class, Some(bound_to))),
    );
}

/// `CLSIDFromProgID` and `CLSIDFromString` (`main.cpp:198-201`) identify the
/// class, `CoCreateInstance` (`:205-208`) instantiates it. Neither can run
/// here, so the construction reports the message of the step that would have
/// failed for this identifier: text that `CLSIDFromString` parses is past both
/// lookups and fails at instantiation, everything else fails at the lookup.
fn log_connection_failure(runtime: &mut Runtime<KrkrHost>, identifier: &str) {
    let message = if is_clsid_text(identifier) {
        format!("win32ole.dll: CoCreateInstance failed {identifier}")
    } else {
        format!("win32ole.dll: bad CLSID {identifier}")
    };
    runtime.host_mut().log(&message);
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` or the same text without braces —
/// the two forms `CLSIDFromString` accepts.
fn is_clsid_text(text: &str) -> bool {
    let body = match text.strip_prefix('{') {
        Some(rest) => match rest.strip_suffix('}') {
            Some(inner) => inner,
            None => return false,
        },
        None => text,
    };
    const GROUP_WIDTHS: [usize; 5] = [8, 4, 4, 4, 12];
    let groups: Vec<&str> = body.split('-').collect();
    groups.len() == GROUP_WIDTHS.len()
        && groups.iter().zip(GROUP_WIDTHS).all(|(group, width)| {
            group.len() == width && group.chars().all(|digit| digit.is_ascii_hexdigit())
        })
}

// ---------------------------------------------------------------------------
// Instance members
// ---------------------------------------------------------------------------

/// The members `NCB_REGISTER_CLASS(WIN32OLE)` registers (`main.cpp:644-652`)
/// plus ncbind's `finalize` (`ncbind.hpp:1875-1877`).
fn install_ole_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", returns_void);
    runtime.register_object_native(handle, "invoke", dispatch_member);
    runtime.register_object_native(handle, "set", dispatch_member);
    runtime.register_object_native(handle, "get", dispatch_member);
    runtime.register_object_native(handle, "missing", ole_missing);
    runtime.register_object_native(handle, "addEvent", ole_add_event);
    runtime.register_object_native(handle, "getConstant", ole_get_constant);
}

/// `WIN32OLE::invokeMethod` / `setMethod` / `getMethod` (`main.cpp:329-339`)
/// of a cleared object: the three differ only in the dispatch flags they would
/// pass (`DISPATCH_PROPERTYGET|DISPATCH_METHOD`, `DISPATCH_PROPERTYPUT`,
/// `DISPATCH_PROPERTYGET`), and all three test `pDispatch` before those
/// arguments (`:251`, `:275`, `:291`).
fn dispatch_member(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    require_ole_instance(runtime, this_obj)?;
    Err(dispatch_unavailable())
}

/// `WIN32OLE::missing` (`main.cpp:297-326`), installed as the instance's
/// missing-member hook and callable by a script like any member.
fn ole_missing(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    require_ole_instance(runtime, this_obj)?;
    // `numparams < 3` (`main.cpp:299`).
    if args.len() < 3 {
        return Err(TjsError::bad_param_count());
    }
    // `set`: `invoke(DISPATCH_PROPERTYPUT, membername, NULL, 1, &param[2])`
    // (`main.cpp:302-304`). `get`: `invoke(DISPATCH_PROPERTYGET|DISPATCH_METHOD,
    // membername, &result, 0, NULL)` (`:306-309`), whose `TJS_E_BADPARAMCOUNT`
    // branch would build a method wrapper for a live property-less dispatch
    // (`:310-313`) — unreachable without one. Both fail on a cleared object,
    // so `ret` stays false and the handler answers false (`:322-324`).
    Ok(Variant::Integer(0))
}

/// `WIN32OLE::_addEventMethod` (`main.cpp:516-538`).
fn ole_add_event(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    require_ole_instance(runtime, this_obj)?;
    // `numparams < 1` (`main.cpp:517-519`).
    let Some(name) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    let name = name.to_tjs_string()?;
    if let Some(receiver) = args.get(1) {
        // `param[1]->AsObject()` (`main.cpp:526`); the null object answers a
        // NULL receiver (`tjsVariant.h:681-686`), which the reference reads as
        // "no receiver to send to" (`:527`).
        as_object(receiver)?;
    }
    // `findIID` (`main.cpp:426-489`) needs `GetTypeInfo`/`GetContainingTypeLib`
    // from a live dispatch and `AtlAdvise` (`:501`) needs the connection
    // point, so the registration always fails here. The reference logs this
    // exact line (`:535`) and still answers `TJS_S_OK` (`:537`).
    runtime.host_mut().log(&format!(
        "win32ole.dll: イベント[{name}]の登録に失敗しました"
    ));
    Ok(Variant::Void)
}

/// `WIN32OLE::_getConstantMethod` (`main.cpp:616-630`).
fn ole_get_constant(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    require_ole_instance(runtime, this_obj)?;
    if let Some(target) = args.first() {
        // `param[0]->AsObject()` (`main.cpp:621`).
        as_object(target)?;
    }
    // `getConstant` is guarded by `if (pDispatch)` (`main.cpp:597`): with no
    // dispatch there is no type library to enumerate, so the call neither
    // stores nor raises, and answers `TJS_S_OK` — void (`:629`).
    Ok(Variant::Void)
}

/// The `ActiveX` constructor's own state and members (`main.cpp:995-1029`,
/// `:1177-1185`).
fn install_activex_members(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    geometry: Arc<Mutex<Geometry>>,
) {
    // `setExternalUI` (`main.cpp:1070-1077`) creates the IE-only UI handler
    // and attaches it when a window exists; without one it is the void no-op
    // the handler's return type already makes it.
    runtime.register_object_native(handle, "setExternalUI", activex_set_external_ui);

    let pos_state = Arc::clone(&geometry);
    runtime.register_object_native_with_arg_count(
        handle,
        "setPos",
        // `NCB_METHOD(setPos)` (`main.cpp:1178`) with two parameters
        // (`ncbind.hpp:1186`).
        NativeArgCount::AtLeast(2),
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            require_ole_instance(runtime, this_obj)?;
            let left = integer_argument(argument(&args, 0)?)?;
            let top = integer_argument(argument(&args, 1)?)?;
            let mut geometry = lock_geometry(&pos_state)?;
            geometry.left = left;
            geometry.top = top;
            Ok(Variant::Void)
        },
    );

    let size_state = Arc::clone(&geometry);
    runtime.register_object_native_with_arg_count(
        handle,
        "setSize",
        NativeArgCount::AtLeast(2),
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            require_ole_instance(runtime, this_obj)?;
            let width = integer_argument(argument(&args, 0)?)?;
            let height = integer_argument(argument(&args, 1)?)?;
            let mut geometry = lock_geometry(&size_state)?;
            geometry.width = width;
            geometry.height = height;
            Ok(Variant::Void)
        },
    );

    // `NCB_PROPERTY_RO(isValidWindow, isValidWindow)` (`main.cpp:1180`):
    // `m_hWnd != 0` (`:1088-1090`), and no ATL window can be hosted here.
    runtime.register_object_native_property_with_access(
        handle,
        "isValidWindow",
        NativePropertyAccess::ReadOnly,
        property_zero,
        read_only_setter,
    );

    // `NCB_PROPERTY(visible, getVisible, setVisible)` (`main.cpp:1181`):
    // `m_hWnd && IsWindowVisible()` (`:1092-1094`), false without a window;
    // `setVisible` (`:1079-1086`) stores nothing and is a no-op there. The
    // argument is still converted, because NCB converts it before the setter
    // runs.
    runtime.register_object_native_property(handle, "visible", property_zero, visible_setter);

    // `NCB_PROPERTY(left, getLeft, setLeft)` and its three siblings
    // (`main.cpp:1182-1185`) over `main.cpp:865-868`.
    register_geometry_property(
        runtime,
        handle,
        "left",
        Arc::clone(&geometry),
        |state| state.left,
        |state, value| state.left = value,
    );
    register_geometry_property(
        runtime,
        handle,
        "top",
        Arc::clone(&geometry),
        |state| state.top,
        |state, value| state.top = value,
    );
    register_geometry_property(
        runtime,
        handle,
        "width",
        Arc::clone(&geometry),
        |state| state.width,
        |state, value| state.width = value,
    );
    register_geometry_property(
        runtime,
        handle,
        "height",
        Arc::clone(&geometry),
        |state| state.height,
        |state, value| state.height = value,
    );
}

/// The four geometry values the reference keeps as `int` members of the native
/// `ActiveX` instance (`main.cpp:865-868`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Geometry {
    left: i64,
    top: i64,
    width: i64,
    height: i64,
}

fn register_geometry_property(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
    state: Arc<Mutex<Geometry>>,
    read: fn(&Geometry) -> i64,
    write: fn(&mut Geometry, i64),
) {
    let getter_state = Arc::clone(&state);
    let setter_state = Arc::clone(&state);
    runtime.register_object_native_property(
        handle,
        name,
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            require_ole_instance(runtime, this_obj)?;
            let geometry = lock_geometry(&getter_state)?;
            Ok(Variant::Integer(read(&geometry)))
        },
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            require_ole_instance(runtime, this_obj)?;
            let value = integer_argument(&value)?;
            let mut geometry = lock_geometry(&setter_state)?;
            write(&mut geometry, value);
            Ok(())
        },
    );
}

fn lock_geometry(state: &Mutex<Geometry>) -> Result<MutexGuard<'_, Geometry>> {
    state
        .lock()
        .map_err(|_| TjsError::runtime("win32ole geometry lock poisoned"))
}

// ---------------------------------------------------------------------------
// Variant conversions
// ---------------------------------------------------------------------------

/// `int value = *param[n]` — the C++ conversion the reference performs for its
/// geometry arguments (`main.cpp:1000-1003`) and for the two-argument setters
/// (`:1135-1148`). A value that cannot convert raises the reference's
/// `TJSThrowVariantConvertError` text (`tjsVariant.cpp:142-151`), whose target
/// name for an integer is `int` (`tjsUtils.cpp:36-48`).
fn integer_argument(value: &Variant) -> Result<i64> {
    value
        .to_integer()
        .map_err(|_| TjsError::variant_convert(value, "int"))
}

/// `tTJSVariant::AsObject()` (`tjsVariant.h:668-677`): the object behind an
/// object or closure variant, `None` for the null object — whose
/// `AsObjectNoAddRef` answers NULL (`:681-686`) — and the reference's
/// object-conversion error for everything else.
fn as_object(value: &Variant) -> Result<Option<ObjectHandle>> {
    match value {
        Variant::Null => Ok(None),
        other => match other.object_handle() {
            Some(handle) => Ok(Some(handle)),
            None => Err(TjsError::variant_convert_to_object(value)),
        },
    }
}

/// `IsInstanceOf(0, NULL, NULL, TJS_W(name), obj)` (`main.cpp:1010`): the class
/// name is looked up on the object and every class above it, the walk the
/// sibling `windowEx` port uses for the same call (`main.cpp:387`, `:908`).
fn object_is_instance_of(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    class_name: &str,
) -> bool {
    let mut current = Some(object);
    while let Some(handle) = current {
        if runtime
            .object_class_infos(handle)
            .iter()
            .any(|info| info == class_name)
        {
            return true;
        }
        current = runtime.object_super_class(handle);
    }
    false
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::{TjsError, TjsErrorKind, runtime::Variant};

    use super::Win32OlePlugin;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(Win32OlePlugin)
            .expect("win32ole plugin");
        engine
    }

    fn eval(engine: &mut KrkrEngine, source: &str) -> Variant {
        engine
            .execute_expression("probe.tjs", source)
            .unwrap_or_else(|error| panic!("{source} failed: {}", error.message))
    }

    fn text(engine: &mut KrkrEngine, source: &str) -> String {
        eval(engine, source)
            .to_tjs_string()
            .unwrap_or_else(|error| panic!("{source} did not produce a string: {}", error.message))
    }

    fn script(engine: &mut KrkrEngine, source: &str) {
        engine
            .execute_script("probe.tjs", source)
            .unwrap_or_else(|error| panic!("{source} failed: {}", error.message));
    }

    fn eval_error(engine: &mut KrkrEngine, source: &str) -> TjsError {
        engine
            .execute_expression("probe.tjs", source)
            .expect_err(&format!("{source} should have failed"))
    }

    fn logs(engine: &KrkrEngine) -> String {
        engine.host().logs().join("\n")
    }

    fn member_names(engine: &mut KrkrEngine, class: &str) -> Vec<String> {
        let handle = engine
            .tjs_runtime()
            .global_member(class)
            .object_handle()
            .unwrap_or_else(|| panic!("{class} is not a class object"));
        let mut names: Vec<String> = engine
            .tjs_runtime()
            .object_members(handle)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    /// Opens one `WIN32OLE` and one `ActiveX` object in the script globals, the
    /// state the member probes share.
    fn fixture(engine: &mut KrkrEngine) {
        script(
            engine,
            "global.ole = new WIN32OLE(\"Shell.Application\");\n\
             global.ax = new ActiveX(\"Shell.Application\");",
        );
    }

    /// The DLL's two class names are installed on the script global under the
    /// names NCB registers them with (`ncbind.hpp:1902-1924`), carry the
    /// reference's member table (`main.cpp:644-652`, `:1169-1186`) plus the
    /// constructor member NCB names after the class (`ncbind.hpp:1777-1784`),
    /// and the catalog resolves the DLL's spellings to this module's entry.
    #[test]
    fn the_class_objects_carry_the_reference_member_table() {
        let mut engine = engine();

        assert_eq!(
            member_names(&mut engine, "WIN32OLE"),
            [
                "WIN32OLE",
                "addEvent",
                "finalize",
                "get",
                "getConstant",
                "invoke",
                "missing",
                "set",
            ]
        );
        assert_eq!(
            member_names(&mut engine, "ActiveX"),
            [
                "ActiveX",
                "addEvent",
                "finalize",
                "get",
                "getConstant",
                "height",
                "invoke",
                "isValidWindow",
                "left",
                "missing",
                "set",
                "setExternalUI",
                "setPos",
                "setSize",
                "top",
                "visible",
                "width",
            ]
        );

        // An instance holds its own copies of the members, so the class-object
        // answers above are the class object's and not an instance's.
        let instance = eval(&mut engine, "new WIN32OLE(\"x\")");
        let handle = instance.object_handle().expect("object");
        assert!(!matches!(
            engine.tjs_runtime().object_member(handle, "invoke"),
            Variant::Void
        ));
        assert!(
            engine
                .tjs_runtime()
                .object_class_infos(handle)
                .iter()
                .any(|info| info == "WIN32OLE")
        );

        // Name resolution: the catalog entry this module implements.
        assert_eq!(
            crate::resolve("WIN32OLE.dll").map(|entry| entry.name),
            Some("win32ole.dll")
        );
        assert_eq!(
            engine.host().linked_plugins().collect::<Vec<_>>(),
            ["win32ole.dll"]
        );
    }

    /// `main.cpp:226-228` and `:1057-1059`: one argument is mandatory, checked
    /// before anything else.
    #[test]
    fn both_constructors_require_an_identifier() {
        let mut engine = engine();

        for source in ["new WIN32OLE()", "new ActiveX()"] {
            let error = eval_error(&mut engine, source);
            assert_eq!(error.kind, TjsErrorKind::BadParamCount, "{source}");
            assert_eq!(error.message, "Invalid argument count", "{source}");
        }
    }

    /// Construction succeeds and leaves the object without a dispatch, the
    /// state `CLSIDFromProgID`/`CLSIDFromString`/`CoCreateInstance` failure
    /// produces (`main.cpp:196-211`). The log names the step that failed.
    #[test]
    fn a_failed_connection_is_reported_with_the_references_own_message() {
        let mut engine = engine();

        assert!(
            eval(&mut engine, "new WIN32OLE(\"Shell.Application\")")
                .object_handle()
                .is_some()
        );
        assert!(logs(&engine).contains("bad CLSID Shell.Application"));

        // A CLSID text is the form `CLSIDFromString` parses (`main.cpp:199`),
        // so the reference gets past both lookups and fails instantiating.
        assert!(
            eval(
                &mut engine,
                "new WIN32OLE(\"{0002DF01-0000-0000-C000-000000000046}\")"
            )
            .object_handle()
            .is_some()
        );
        assert!(
            logs(&engine)
                .contains("CoCreateInstance failed {0002DF01-0000-0000-C000-000000000046}")
        );

        // The braced form is optional for `CLSIDFromString`, a truncated
        // group is not a CLSID.
        assert!(
            eval(
                &mut engine,
                "new WIN32OLE(\"0002DF01-0000-0000-C000-000000000046\")"
            )
            .object_handle()
            .is_some()
        );
        assert!(
            logs(&engine).contains("CoCreateInstance failed 0002DF01-0000-0000-C000-000000000046")
        );
        assert!(
            eval(&mut engine, "new WIN32OLE(\"{0002DF01}\")")
                .object_handle()
                .is_some()
        );
        assert!(logs(&engine).contains("bad CLSID {0002DF01}"));

        // The transport report is made once at registration, where the
        // reference's `PreRegistCallback` reports its failed `OleInitialize`
        // (`main.cpp:1202-1206`).
        assert!(logs(&engine).contains("COM/ATL transport unavailable"));
    }

    /// `main.cpp:251`, `:275`, `:291`: the guarded members test `pDispatch`
    /// before their own argument checks, so a cleared object reports
    /// `TJS_E_FAIL` for every argument list — including the empty one and a
    /// non-string name, which would be `TJS_E_BADPARAMCOUNT` /
    /// `TJS_E_INVALIDPARAM` on a live connection.
    #[test]
    fn the_guarded_members_report_tjs_e_fail_for_any_arguments() {
        let mut engine = engine();
        fixture(&mut engine);

        for source in [
            "ole.invoke(\"Name\")",
            "ole.invoke()",
            "ole.invoke(5)",
            "ole.set(\"x\", 1)",
            "ole.set()",
            "ole.get(\"x\")",
            "ole.get()",
            "ax.invoke(\"Name\")",
            "ax.set(\"x\", 1)",
            "ax.get(\"x\")",
        ] {
            let error = eval_error(&mut engine, source);
            assert_eq!(error.kind, TjsErrorKind::Runtime, "{source}");
            assert_eq!(error.message, "Unknown failure : FFFFFFFF", "{source}");
        }
    }

    /// The hook `main.cpp:213-214` installs is the instance's `missing` member
    /// (`:297-326`) and it declines, because the `invoke` calls it makes are
    /// the guarded ones above. A declined hook keeps the engine's ordinary
    /// member semantics (`tjsObject.cpp:1337-1345`, `:1495-1509`).
    #[test]
    fn the_missing_hook_declines_instead_of_answering() {
        let mut engine = engine();
        fixture(&mut engine);

        let error = eval_error(&mut engine, "ole.answer");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"answer\" does not exist");

        // A store creates the member, exactly like any TJS object.
        assert_eq!(
            text(
                &mut engine,
                "(function() { ole.answer = 42; return \"\" + ole.answer; })()"
            ),
            "42"
        );

        // Called directly it validates its own argument count (`main.cpp:299`)
        // and answers false for a well-formed call (`:322-324`).
        let error = eval_error(&mut engine, "ole.missing()");
        assert_eq!(error.kind, TjsErrorKind::BadParamCount);
        assert_eq!(
            eval(&mut engine, "ole.missing(0, \"answer\", %[value => 0])"),
            Variant::Integer(0)
        );
        assert_eq!(
            eval(&mut engine, "ole.missing(1, \"answer\", %[value => 1])"),
            Variant::Integer(0)
        );
    }

    /// `main.cpp:516-538`: the name is required, the receiver must be an
    /// object, and the registration itself always fails without a dispatch —
    /// which the reference logs and does not turn into an exception.
    #[test]
    fn add_event_validates_its_arguments_and_reports_the_registration_failure() {
        let mut engine = engine();
        fixture(&mut engine);

        let error = eval_error(&mut engine, "ole.addEvent()");
        assert_eq!(error.kind, TjsErrorKind::BadParamCount);
        assert_eq!(error.message, "Invalid argument count");

        let error = eval_error(&mut engine, "ole.addEvent(\"DWebBrowser2Events2\", 5)");
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((int)5 to Object)"
        );

        assert!(matches!(
            eval(&mut engine, "ole.addEvent(\"DWebBrowser2Events2\")"),
            Variant::Void
        ));
        assert!(logs(&engine).contains("イベント[DWebBrowser2Events2]の登録に失敗しました"));

        // The null object is the reference's NULL receiver, and an object
        // receiver is accepted, both ending in the same failed registration.
        assert!(matches!(
            eval(&mut engine, "ole.addEvent(\"X\", null)"),
            Variant::Void
        ));
        assert!(matches!(
            eval(&mut engine, "ole.addEvent(\"X\", %[])"),
            Variant::Void
        ));
        assert_eq!(
            logs(&engine)
                .matches("イベント[X]の登録に失敗しました")
                .count(),
            2
        );
    }

    /// `main.cpp:597-611`, `:616-630`: there is no type library to read
    /// without a dispatch, so the call is a void no-op — after `AsObject` has
    /// converted its target.
    #[test]
    fn get_constant_is_a_no_op_without_a_type_library() {
        let mut engine = engine();
        fixture(&mut engine);

        assert!(matches!(
            eval(&mut engine, "ole.getConstant()"),
            Variant::Void
        ));
        // A target object is the reference's store, and it stays untouched:
        // there is no type library to fill it from (`main.cpp:597-611`).
        script(&mut engine, "global.storeProbe = %[];");
        assert!(matches!(
            eval(&mut engine, "ole.getConstant(storeProbe)"),
            Variant::Void
        ));
        let store = eval(&mut engine, "storeProbe");
        let handle = store.object_handle().expect("object");
        assert_eq!(engine.tjs_runtime().object_members(handle).len(), 0);

        let error = eval_error(&mut engine, "ole.getConstant(5)");
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((int)5 to Object)"
        );
    }

    /// `main.cpp:1007-1024`: an object second argument must be a `Window`.
    #[test]
    fn the_activex_window_argument_must_be_a_window_object() {
        let mut engine = engine();

        let error = eval_error(&mut engine, "new ActiveX(\"Shell.Application\", %[])");
        assert_eq!(error.message, "must set window object");

        // A class object is not a window instance either, the guard the
        // `Window` port keeps for the same call.
        let error = eval_error(&mut engine, "new ActiveX(\"Shell.Application\", Window)");
        assert_eq!(error.message, "must set window object");

        for source in [
            "new ActiveX(\"Shell.Application\", new Window())",
            "new ActiveX(\"Shell.Application\", null)",
            "new ActiveX(\"Shell.Application\", 0)",
        ] {
            assert!(
                eval(&mut engine, source).object_handle().is_some(),
                "{source}"
            );
        }
        // Hosting the control is what cannot run (`main.cpp:917-932`).
        assert!(logs(&engine).contains("CreateControl failed Shell.Application"));
    }

    /// `main.cpp:995-1004`, `:1096-1148`: the four geometry values are plain
    /// fields, and only all six constructor arguments reach them.
    #[test]
    fn activex_geometry_keeps_working_without_a_window() {
        let mut engine = engine();
        fixture(&mut engine);

        assert_eq!(
            text(
                &mut engine,
                "[ax.left, ax.top, ax.width, ax.height].join(\",\")"
            ),
            "0,0,-1,-1"
        );

        script(&mut engine, "ax.setPos(10, 20); ax.setSize(640, 480);");
        assert_eq!(
            text(
                &mut engine,
                "[ax.left, ax.top, ax.width, ax.height].join(\",\")"
            ),
            "10,20,640,480"
        );

        script(&mut engine, "ax.left = 5; ax.height = 8;");
        assert_eq!(text(&mut engine, "ax.left + \":\" + ax.height"), "5:8");

        // TJS's integer conversion, not a strict parse: "0x10" is 16.
        script(&mut engine, "ax.width = \"0x10\";");
        assert_eq!(text(&mut engine, "ax.width"), "16");
        let error = eval_error(&mut engine, "ax.width = %[]");
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((object) to int)"
        );

        // The declared parameter count (`ncbind.hpp:1186`); extra arguments are
        // ignored, so three are accepted.
        let error = eval_error(&mut engine, "ax.setPos(1)");
        assert_eq!(error.kind, TjsErrorKind::BadParamCount);
        assert_eq!(error.message, "Invalid argument count");
        script(&mut engine, "ax.setSize(1, 2, 3);");
        assert_eq!(
            text(&mut engine, "[ax.width, ax.height].join(\",\")"),
            "1,2"
        );

        // Only `numparams >= 6` fills the geometry, so a partial list leaves
        // the C++ defaults in place.
        script(
            &mut engine,
            "global.full = new ActiveX(\"Shell.Application\", null, 1, 2, 3, 4);\n\
             global.partial = new ActiveX(\"Shell.Application\", null, 1, 2, 3, 4, 5);\n\
             global.short = new ActiveX(\"Shell.Application\", null, 1, 2, 3);",
        );
        assert_eq!(
            text(
                &mut engine,
                "[full.left, full.top, full.width, full.height].join(\",\")"
            ),
            "1,2,3,4"
        );
        assert_eq!(
            text(
                &mut engine,
                "[partial.left, partial.top, partial.width, partial.height].join(\",\")"
            ),
            "1,2,3,4"
        );
        assert_eq!(
            text(
                &mut engine,
                "[short.left, short.top, short.width, short.height].join(\",\")"
            ),
            "0,0,-1,-1"
        );

        // The geometry read is not shared state.
        assert_eq!(
            text(&mut engine, "[ax.left, full.left, short.left].join(\",\")"),
            "5,1,0"
        );
    }

    /// `main.cpp:1070-1094`, `:1180-1181`: with no window host the validity
    /// and visibility answers are the reference's own window-less ones.
    #[test]
    fn validity_and_visibility_keep_the_window_less_answers() {
        let mut engine = engine();
        fixture(&mut engine);

        assert_eq!(
            text(&mut engine, "ax.isValidWindow + \":\" + ax.visible"),
            "0:0"
        );

        // `setVisible` is a no-op without a window (`main.cpp:1079-1086`), and
        // the getter keeps reading `m_hWnd && IsWindowVisible()`.
        script(&mut engine, "ax.visible = 1;");
        assert_eq!(text(&mut engine, "ax.visible"), "0");

        // `NCB_PROPERTY_RO(isValidWindow, ...)` (`main.cpp:1180`).
        let error = eval_error(&mut engine, "ax.isValidWindow = 1");
        assert_eq!(error.kind, TjsErrorKind::AccessDenied);
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );

        assert!(matches!(
            eval(&mut engine, "ax.setExternalUI()"),
            Variant::Void
        ));
    }

    /// `ncbind.hpp:1547`, `:1133`, `:1486`: a class-object call has no native
    /// instance behind it, so it answers `TJS_E_NATIVECLASSCRASH` rather than
    /// running an instance member.
    #[test]
    fn class_object_calls_have_no_native_instance() {
        let mut engine = engine();

        for source in [
            "WIN32OLE.invoke(\"x\")",
            "WIN32OLE.set(\"x\", 1)",
            "WIN32OLE.get(\"x\")",
            "WIN32OLE.missing(0, \"x\", 0)",
            "WIN32OLE.addEvent(\"x\")",
            "WIN32OLE.getConstant()",
            "WIN32OLE.invoke()",
            "ActiveX.setPos(1, 2)",
            "ActiveX.setSize(1, 2)",
            "ActiveX.setExternalUI()",
            "ActiveX.left",
            "ActiveX.visible",
            "ActiveX.isValidWindow",
        ] {
            let error = eval_error(&mut engine, source);
            assert_eq!(error.kind, TjsErrorKind::NativeClassCrash, "{source}");
            assert_eq!(error.message, "Invalid object context", "{source}");
        }

        // A read-only class property still refuses the write first: NCB's
        // dummy setter answers before the instance lookup (`ncbind.hpp:1485`).
        assert_eq!(
            eval_error(&mut engine, "ActiveX.isValidWindow = 1").kind,
            TjsErrorKind::AccessDenied
        );
    }

    /// ncbind unregisters the classes it registered when the plugin is
    /// unloaded (`ncbind.hpp:1936-1940`), and `Plugins.link` installs them
    /// again.
    #[test]
    fn unlink_removes_the_globals_and_relink_installs_them_again() {
        let mut engine = engine();

        engine
            .execute_script("unlink.tjs", "Plugins.unlink(\"win32ole.dll\");")
            .expect("unlink");
        for name in ["WIN32OLE", "ActiveX"] {
            let error = eval_error(&mut engine, name);
            assert_eq!(error.kind, TjsErrorKind::MemberNotFound, "{name}");
        }

        engine
            .execute_script("relink.tjs", "Plugins.link(\"win32ole.dll\");")
            .expect("relink");
        assert!(
            eval(&mut engine, "new ActiveX(\"x\")")
                .object_handle()
                .is_some()
        );
        assert!(
            eval(&mut engine, "new WIN32OLE(\"x\")")
                .object_handle()
                .is_some()
        );
    }

    /// A class that extends a native class keeps working: the class-body call
    /// copies the members onto the instance, and `super.WIN32OLE(name)` runs
    /// the factory on it and attaches the OLE instance — the same shape the
    /// engine's own native classes have (`classes.rs:106-135`).
    #[test]
    fn a_script_subclass_receives_the_members_on_its_instance() {
        let mut engine = engine();

        script(
            &mut engine,
            "class MyOle extends WIN32OLE { function MyOle(name) { super.WIN32OLE(name); } }\n\
             global.mine = new MyOle(\"Shell.Application\");",
        );
        assert!(eval(&mut engine, "mine").object_handle().is_some());
        // The factory ran on the instance, so it reported the connection
        // failure for the identifier the subclass passed.
        assert!(logs(&engine).contains("bad CLSID Shell.Application"));
        assert_eq!(
            eval_error(&mut engine, "mine.invoke(\"x\")").message,
            "Unknown failure : FFFFFFFF"
        );
        assert_eq!(
            text(
                &mut engine,
                "(function() { mine.answer = 7; return \"\" + mine.answer; })()"
            ),
            "7"
        );
    }

    /// Until the factory runs, an object has no OLE instance: NCB's native
    /// instance is registered by the factory (`ncbind.hpp:186-192`), and the
    /// class-body call that `class X extends WIN32OLE` makes only copies the
    /// class members (`tTJSNativeClass::FuncCall`, `tjsNative.cpp:302-321`).
    /// Every member then answers `TJS_E_NATIVECLASSCRASH`
    /// (`ncbind.hpp:1049-1052`, `:1547`), the geometry and window properties
    /// included.
    #[test]
    fn a_subclass_without_the_super_call_has_no_ole_instance() {
        let mut engine = engine();

        script(
            &mut engine,
            "class Bare extends WIN32OLE { }\n\
             class BareAx extends ActiveX { }\n\
             global.bare = new Bare(\"Shell.Application\");\n\
             global.bareAx = new BareAx(\"Shell.Application\");",
        );
        // The class-body call copies the members but nothing reports a
        // connection: the factory never ran.
        assert!(!logs(&engine).contains("bad CLSID Shell.Application"));

        for source in [
            "bare.invoke(\"x\")",
            "bare.set(\"x\", 1)",
            "bare.get(\"x\")",
            "bare.missing(0, \"x\", 0)",
            "bare.addEvent(\"x\")",
            "bare.getConstant()",
            "bareAx.setPos(1, 2)",
            "bareAx.setSize(1, 2)",
            "bareAx.setExternalUI()",
            "bareAx.left",
            "bareAx.visible",
            "bareAx.isValidWindow",
        ] {
            let error = eval_error(&mut engine, source);
            assert_eq!(error.kind, TjsErrorKind::NativeClassCrash, "{source}");
            assert_eq!(error.message, "Invalid object context", "{source}");
        }

        // The instance's own members and the class-name member keep working:
        // they are copied members, not instance lookups.
        script(&mut engine, "bare.own = 3; bare.finalize();");
        assert_eq!(text(&mut engine, "bare.own"), "3");

        // Calling the factory on the instance — `super.WIN32OLE(name)`'s shape
        // through the bound class-name member — attaches it from then on.
        script(&mut engine, "bare.WIN32OLE(\"Shell.Application\");");
        assert_eq!(
            eval_error(&mut engine, "bare.invoke(\"x\")").message,
            "Unknown failure : FFFFFFFF"
        );
    }
}
