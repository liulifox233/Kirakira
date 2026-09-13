//! `menu.dll` — the shipped native-menu plugin, surfaced on the engine's
//! `MenuItem` object.
//!
//! # Reference
//!
//! PARQUET ships `plugin/menu.dll` (139 264 bytes, PE32, built 2014-07-12,
//! exports `V2Link` @`0x10003570` and `V2Unlink` @`0x10003b50`). No source of
//! it is on this machine — the krkrz plugin tree the M24 census cites is not
//! checked out here, only the platform-neutral Win32 port
//! (`Kirikiroid2/src/core/visual/win32/MenuItemImpl.cpp`, GBK) and the TVP
//! core (`Kirikiroid2/src/core/visual/MenuItemIntf.cpp`). The surface below
//! is therefore recovered from the shipped binary itself with `objdump`; all
//! addresses are virtual addresses at the image's `0x10000000` base.
//!
//! The binary registers one native class and one class property:
//!
//! * `tTJSNC_MenuItem` — `TJSCreateNativeClassForPlugin("MenuItem", 0x100074d0)`
//!   @`0x100076a7` with the class-name string @`0x1001a3e0`; class id from
//!   `TJSRegisterNativeClass("MenuItem")` @`0x100058b1`; `TJSNativeClassSetClassID`
//!   @`0x10007714`. The native instance (`0x50` bytes, vtable @`0x1001a798`) is
//!   created by `0x100074d0`; its `Construct` @`0x100064c0` reads
//!   `param[0]` as the action owner (`ActionOwner`, instance `+0x38/+0x3c`,
//!   `Owner` = the TJS object at `+0x34`) and, when `param[1]` is an object
//!   with a readable `HWND` member, attaches the item to that window
//!   (`+0x2c`), creating the native menu bar and registering a message
//!   receiver; otherwise `param[1]` is the caption (`+0x24`).
//!   `numparams < 1` answers `TJS_E_BADPARAMCOUNT` (`0x100064f6`).
//! * members, registered through `TJSNativeClassRegisterNCM` with the class
//!   name @`0x1001a3f4`, `nitMethod` = 1 and `nitProperty` = 2
//!   (`tjsNative.h:31-35`), `TJS_STATICMEMBER` = 0x10000:
//!   | member | kind | registration | callback(s) |
//!   |---|---|---|---|
//!   | `finalize` | method | `0x1000789b` | `0x10004980` (empty, `TJS_DECL_EMPTY_FINALIZE_METHOD`) |
//!   | `MenuItem` (ctor) | method | `0x1000790c` | `0x10008bf0` via `TJSCreateNativeClassConstructor` (`0x100058d0`) |
//!   | `add` | method | `0x100079f8` | `0x10008c70` |
//!   | `insert` | method | `0x10007ad7` | `0x10008d00` |
//!   | `remove` | method | `0x10007bb7` | `0x10008d90` |
//!   | `popup` | method | `0x10007c97` | `0x10008e20` |
//!   | `onClick` (event) | method | `0x10007d77` | `0x10008ee0` |
//!   | `fireClick` | method | `0x10007e57` | `0x10008fd0` |
//!   | `caption` | property | `0x10007f37` | get `0x10009030`, set `0x100090f0` |
//!   | `checked` | property | `0x1000801c` | get `0x100091d0`, set `0x10009250` |
//!   | `enabled` | property | `0x10008101` | get `0x100092d0`, set `0x10009350` |
//!   | `group` | property | `0x100081e6` | get `0x100093f0`, set `0x10009460` |
//!   | `radio` | property | `0x100082cb` | get `0x100094d0`, set `0x10009550` |
//!   | `shortcut` | property | `0x100083b0` | get `0x100095d0`, set `0x10009680` |
//!   | `visible` | property | `0x10008495` | get `0x10009720`, set `0x100097b0` |
//!   | `parent` | property (ro) | `0x1000857a` | get `0x10009820` |
//!   | `children` | property (ro) | `0x1000865f` | get `0x100098f0` |
//!   | `root` | property (ro) | `0x10008744` | get `0x10009990` |
//!   | `window` | property (ro) | `0x10008829` | get `0x10009a50` |
//!   | `index` | property | `0x1000890e` | get `0x10009b20`, set `0x10009b90` |
//!   | `HMENU` | property (ro) | `0x100089f3` | get `0x10009c00` |
//!   | `textToKeycode` | static property (ro) | `0x10008adb` | get `0x10009c80` |
//!   | `keycodeToText` | static property (ro) | `0x10008bc3` | get `0x10009cf0` |
//!
//!   The shared read-only setter is `0x10002ca0` (`TJS_E_ACCESSDENYED`).
//!   Arity is checked in the callbacks: `add` ≥1 and `remove` ≥1
//!   (`0x10008ca8`, `0x10008de2`), `insert` ≥2 (`0x10008d47`), `popup` ≥3
//!   (`0x10008e70`), ctor ≥1; `fireClick`/`onClick` check nothing
//!   (`0x10008fd0`, `0x10008f55`).
//! * `Window.menu` — a `WindowMenuProperty` instance (vtable @`0x1001a704`,
//!   RTTI `.?AVWindowMenuProperty@@`) built in `V2Link` @`0x10003601` and
//!   installed with `PropSet(0x200 /* TJS_MEMBERENSURE */, L"menu", …)` on the
//!   global `Window` object @`0x100037ec`/`0x1000380f`. Its getter caches one
//!   `MenuItem` per window `HWND`; the setter answers `TJS_E_ACCESSDENYED`.
//! * `MenuItem.onClick` (`0x10008ee0`) forwards to the action owner:
//!   `actionOwner.action(event)` where `event` is `TVPCreateEventObject("onClick",
//!   objthis, objthis)` (the `"action"` string @`0x1001a594`).
//!   `MenuItem::OnClick` (`0x10006db0`) gates on `CanDeliverEvents`
//!   (`0x10006f60`: the item and every ancestor `Enabled`), walks up to the
//!   root looking for its `Window` (`+0x2c`) and posts `"onClick"` to the
//!   owner (`TVPPostEvent`, @`0x10006e38`). The native click path posts
//!   `"fireClick"` the same way (`0x10006e90`).
//! * `textToKeycode` / `keycodeToText` are process-wide objects built by
//!   `CreateShortCutKeyCodeTable` (`0x10003080-0x1000356c`): a `Dictionary`
//!   (lowercased name → virtual key) and an `Array` (virtual key → name)
//!   filled from `MapVirtualKeyW` + `GetKeyNameTextW` over keys 8…255 (with
//!   the `"Num "` numpad fixup) and then four compatibility names applied
//!   with `force = false` (`SetShortCutKeyCode`, `0x10002cb0`): `"BkSp"` → 8
//!   (`0x100033d1`), `"Del"` → 0x2E (`0x1000344a`), `"PgUp"` → 0x21
//!   (`0x100034c3`), `"PgDn"` → 0x22 (`0x1000353c`).
//! * error texts come from the plugin's string table (RT_STRING block 6):
//!   `0x65` "Too many MenuItem. Cannot create MenuItem." (thrown `0x1000a3ec`
//!   when the native menu item cannot be built), `0x66` "Specify Window class
//!   object." (`TVPSpecifyWindow`, thrown from `Construct` `0x10006570`,
//!   `0x100066cd`, `0x100066f4`), `0x67` "Specify MenuItem class object."
//!   (`TVPSpecifyMenuItem`, thrown from `CastFromVariant` `0x10006a7f`,
//!   `0x10006aaa`, `0x10006ac7`), `0x68` "Internal error occurred. : at %1
//!   line %2" (`TVPInternalError`), `0x69` "The specified menu item is not a
//!   child of this menu item." (`TVPNotChildMenuItem`, thrown from `Remove`
//!   `0x1000706f`).
//!
//! # What the engine already provides
//!
//! `krkr-engine` ships its own `MenuItem` object (`native/classes.rs`
//! `MENU_ITEM_CLASS`, `:10412`; installed @`globals/mod.rs:38`): ctor
//! (`owner`, `caption`), methods `add/insert/remove/clear/click/onClick/popup`,
//! properties `owner/caption/shortcut/checked/enabled/visible/radio/group/children`
//! and the `index` native property, plus one `MenuItem` materialized as
//! `Window.menu` at window construction (`:558`, `alloc_menu_item_object`
//! `:880`). Those members are installed per instance, so a plugin can only
//! *add* — it cannot replace what the engine already publishes.
//!
//! # What this module installs
//!
//! The reference members the engine lacks, with the recovered semantics:
//!
//! * `fireClick()` — the reference's gate (`CanDeliverEvents` over the
//!   script-visible `enabled` chain, then a `window` on the root) and then the
//!   `onClick` delivery (`MenuItem::OnClick`, `0x10006db0`): a script override
//!   (a closure) is called, and otherwise the reference's default —
//!   `actionOwner.action(event)` with the `TVPCreateEventObject("onClick", …)`
//!   event (`0x10008ee0`) — is performed, since the engine's own `onClick` is
//!   a no-op. The reference *posts* the event; this port calls it
//!   synchronously and routes an escaping exception through the engine's event
//!   boundary (`Runtime::process_unhandled_exception`), because a plugin
//!   cannot reach the engine's event queue.
//! * `root` — the parent-chain walk of `GetRootMenuItem` (`0x10006af0` /
//!   `0x10009990`) over the engine's `parent` member; the item itself for a
//!   root. Self-bound, the reference's `tTJSVariant(dsp, dsp)` and the shape
//!   `new MenuItem(…)` values carry; the engine's own `parent`/`owner`/
//!   `children` members are stored *unbound* (see the gap list), so
//!   `item.root === item.parent` is false in this engine.
//! * `window` (`0x10009a50`) — the item's **own** `Window` field (`+0x2c`,
//!   `0x10009aa6 mov 0x2c(%eax),%eax`), no walk, and `AddChild`
//!   (`0x10006f90`) writes only `Parent` (`+0x40`): a descendant's `window`
//!   is void, exactly like the reference. The field is set only by the object
//!   `TVPCreateMenuItemObject(window)` built; in this engine that object is
//!   the window's `menu` member (owner = the window), which is the one shape
//!   [`attached_window`] recognises. The engine materializes `window.menu`
//!   without a class link (gap below), so in practice only that object would
//!   ever answer — and it cannot be read from script until the engine links
//!   it; `fireClick` still reaches it through its parent-chain gate.
//! * `HMENU` — the reference type of an item with no native menu object
//!   (`GetMenuItemHandleForPlugin` returns `NULL`, also the platform-less
//!   port's constant result, `Kirikiroid2 …/MenuItemImpl.cpp:199-203`): 0.
//! * `textToKeycode` / `keycodeToText` — the two shared tables. The Win32
//!   enumeration is unavailable here, so the table is a built-in one
//!   ([`WINDOWS_KEY_NAMES`], the names Windows reports on a standard US
//!   layout, inferred) plus the four binary-verified compatibility entries;
//!   the objects are shared and read-only exactly like the reference's.
//! * `finalize()` — the reference's empty finalize method.
//!
//! # Engine gaps this module cannot close
//!
//! Recorded in [`META`]; each is reachable only outside
//! `crates/krkr-plugins/src/menu.rs`:
//!
//! * `popup` (`0x10008e20` → `TrackPopupMenuEx`) needs a native menu; the
//!   engine's own `popup` stays a no-op (`classes.rs:952`).
//! * the Win32 message receiver (`registerMessageReceiver`,
//!   `0x1001a360`/`0x1000670d`), accelerator keys
//!   (`TVPRegisterAcceleratorKey`) and the menu bar (`CreateMenu`/`SetMenu`,
//!   `0x100065c0-0x100065f8`) need an HWND and a message loop. The engine's
//!   `MenuItem` properties store values instead (no `SetMenuItemInfoW`
//!   update), so `checked` has no radio-group unchecking and `shortcut` is
//!   not normalised through `TextToShortCut`/`ShortCutToText`.
//! * the engine's own `onClick` member is a no-op installed per instance, so
//!   a direct `item.onClick()` does not forward to the action owner;
//!   `fireClick` performs the reference's default itself (above).
//! * the engine's `MenuItem` members `parent`/`owner`/`children` are stored as
//!   plain objects while the reference returns `tTJSVariant(dsp, dsp)`
//!   (self-bound), so `===` between them and a self-bound item is false.
//! * `Window.menu` is materialized by the engine *without* a class link
//!   (`alloc_menu_item_object` never calls `set_object_super_class`), so the
//!   class-level members below are invisible on `window.menu` itself; every
//!   `new MenuItem(…)` instance resolves them. The engine should link that
//!   object to the `MenuItem` class.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "MenuItem.fireClick/root/window/HMENU/finalize + textToKeycode/keycodeToText",
    notes: "Surface recovered from the shipped menu.dll (no source on this machine; \
            objdump RE, registration sites cited in the module docs). Real: fireClick \
            (reference gate + synchronous onClick delivery and the reference's \
            actionOwner.action(event) default, through the event boundary), \
            root/window/HMENU getters, the empty finalize, and the shared \
            textToKeycode/keycodeToText tables (four compat names binary-verified; the \
            rest of the table is the standard Windows key-name set because this engine \
            has no MapVirtualKey/GetKeyNameText). The engine's own MenuItem already \
            covers ctor/add/insert/remove/click/onClick/popup, caption/checked/enabled/\
            group/radio/shortcut/visible/parent/children/index and Window.menu; a plugin \
            cannot replace instance members. Missing (Win32): popup, accelerator keys, \
            the message receiver / menu-bar plumbing, radio-group unchecking and \
            shortcut text normalisation. window.menu itself is materialized by the \
            engine without a class link, so these class members are not visible on it.",
    install: |engine| engine.register_plugin(MenuPlugin),
};

pub struct MenuPlugin;

impl KrkrPlugin for MenuPlugin {
    fn name(&self) -> &str {
        "menu.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_menu_surface(runtime);
        Ok(())
    }
}

/// The class every recovered member belongs to.
const MENU_ITEM_CLASS: &str = "MenuItem";

/// Backing members for the shared keycode tables (kept on the class so the
/// table objects stay reachable and a re-registration reuses them).
const TEXT_TO_KEYCODE_BACKING: &str = "__menuTextToKeycode";
const KEYCODE_TO_TEXT_BACKING: &str = "__menuKeycodeToText";

/// Installs the reference members the engine has no counterpart for. Members
/// the class already publishes (the engine's own, or a script's) are left
/// alone, the way the reference loads before any script and overwrites with
/// `TJS_MEMBERENSURE`.
fn install_menu_surface(runtime: &mut Runtime<KrkrHost>) {
    let Variant::Object(class) = runtime.global_member(MENU_ITEM_CLASS) else {
        runtime
            .host_mut()
            .log("menu.dll: this engine has no MenuItem class; the menu surface is not installed");
        return;
    };

    if take_slot(runtime, class, "fireClick") {
        runtime.register_object_native(class, "fireClick", menu_item_fire_click);
    }
    if take_slot(runtime, class, "finalize") {
        runtime.register_object_native(class, "finalize", menu_item_finalize);
    }
    install_read_only_property(runtime, class, "root", menu_item_root);
    install_read_only_property(runtime, class, "window", menu_item_window);
    install_read_only_property(runtime, class, "HMENU", menu_item_hmenu);
    install_keycode_tables(runtime, class);
}

/// True when the member slot is still empty, i.e. neither the engine nor a
/// script published a member there.
fn take_slot(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> bool {
    matches!(runtime.object_member(object, name), Variant::Void)
}

/// Registers a read-only native property, the reference's
/// `TJS_DENY_NATIVE_PROP_SETTER`: the script write fails with
/// `TJS_E_ACCESSDENYED` before the accessor runs.
fn install_read_only_property<G>(
    runtime: &mut Runtime<KrkrHost>,
    class: ObjectHandle,
    name: &'static str,
    getter: G,
) where
    G: Fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>) -> Result<Variant> + Send + Sync + 'static,
{
    if !take_slot(runtime, class, name) {
        return;
    }
    runtime.register_object_native_property_with_access(
        class,
        name,
        NativePropertyAccess::ReadOnly,
        getter,
        |_runtime, _this_obj, _value| Ok(()),
    );
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

/// `fireClick()` (`0x10008fd0` → `tTJSNI_MenuItem::OnClick`, `0x10006db0`):
/// deliver the `onClick` event unless the item cannot deliver events
/// (`CanDeliverEvents`, `0x10006f60`, and the window walk at `0x10006dd8`).
fn menu_item_fire_click(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(item) = bound_item(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    if !can_deliver_events(runtime, item) {
        return Ok(Variant::Void);
    }
    // The reference posts `onClick` to the item, whose member then runs: a
    // script override (stored as a closure) or the class's native `onClick`,
    // which forwards to the action owner (`0x10008ee0`). The engine's native
    // `onClick` is a no-op, so the forwarding is performed here. A plugin has
    // no event queue, so the member is called directly and an escaping
    // exception goes through the engine's event boundary instead of out of
    // `fireClick`.
    if matches!(runtime.object_member(item, "onClick"), Variant::Closure(_)) {
        if let Err(error) = runtime.call_object_method(item, "onClick", Vec::new()) {
            runtime.process_unhandled_exception(&error)?;
        }
        return Ok(Variant::Void);
    }
    forward_on_click_to_action_owner(runtime, item)
}

/// The native `onClick`'s own behaviour (`0x10008ee0`): when the item has an
/// action owner carrying `action`, build the `TVPCreateEventObject("onClick",
/// objthis, objthis)` event — `type` plus a self-bound `target`, the shape the
/// engine's own click forwarding uses (`classes.rs:7810-7818`) — and call
/// `actionOwner.action(event)`.
fn forward_on_click_to_action_owner(
    runtime: &mut Runtime<KrkrHost>,
    item: ObjectHandle,
) -> Result<Variant> {
    let Some(owner) = runtime.object_member(item, "owner").object_handle() else {
        return Ok(Variant::Void);
    };
    if matches!(runtime.object_member(owner, "action"), Variant::Void) {
        return Ok(Variant::Void);
    }
    let event = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(event, "Dictionary");
    runtime.set_object_member(event, "target", Variant::self_bound(item));
    runtime.set_object_member(event, "type", Variant::String("onClick".to_string()));
    if let Err(error) =
        runtime.call_object_method(owner, "action", vec![Variant::self_bound(event)])
    {
        runtime.process_unhandled_exception(&error)?;
    }
    Ok(Variant::Void)
}

/// `finalize()` — the reference registers an empty finalize
/// (`TJS_DECL_EMPTY_FINALIZE_METHOD`, `0x10004980`).
fn menu_item_finalize(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// `root` (`0x10009990` → `GetRootMenuItem`, `0x10006af0`): the item's
/// top-most ancestor, the item itself for a root. Self-bound like the
/// reference's `tTJSVariant(dsp, dsp)` return (`0x10009990`), which is also
/// the shape a script's `new MenuItem(…)` carries.
fn menu_item_root(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    let Some(item) = bound_item(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    Ok(Variant::self_bound(root_of(runtime, item)))
}

/// `window` (`0x10009a50`): the item's **own** `Window` field (`+0x2c`,
/// `0x10009aa6 mov 0x2c(%eax),%eax`) — no parent-chain walk, and
/// `AddChild` (`0x10006f90`) writes only `Parent` (`+0x40`). The field is set
/// only by the object `TVPCreateMenuItemObject(window)` built, so a
/// descendant's `window` is void in the reference. The engine equivalent of
/// that object is the window's `menu` member; `attached_window` recognises
/// it per item.
fn menu_item_window(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    let Some(item) = bound_item(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    Ok(match attached_window(runtime, item) {
        // The reference getter hands the window out as `tTJSVariant(dsp, dsp)`
        // (`0x10009a50`), i.e. self-bound; scripts hold `new Window()` results
        // in the same shape, so `item.window === w` holds.
        Some(window) => Variant::self_bound(window),
        None => Variant::Void,
    })
}

/// `HMENU` (`0x10009c00`): the native menu handle, 0 while the item has no
/// native menu object. This engine never builds one, which is also the
/// platform-less port's constant answer (`MenuItemImpl.cpp:199-203`).
fn menu_item_hmenu(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

// ---------------------------------------------------------------------------
// Keycode tables
// ---------------------------------------------------------------------------

/// The `MapVirtualKeyW` + `GetKeyNameTextW` enumeration's result
/// (`0x10003140-0x1000335e`) as a built-in table: this engine has no Win32
/// key-name source, so these are the names Windows reports for a standard US
/// layout. Inferred data — the compatibility names below are the
/// binary-verified part of the tables.
const WINDOWS_KEY_NAMES: &[(i64, &str)] = &[
    (0x08, "Backspace"),
    (0x09, "Tab"),
    (0x0D, "Enter"),
    (0x10, "Shift"),
    (0x11, "Ctrl"),
    (0x12, "Alt"),
    (0x13, "Pause"),
    (0x14, "Caps Lock"),
    (0x1B, "Esc"),
    (0x20, "Space"),
    (0x21, "Page Up"),
    (0x22, "Page Down"),
    (0x23, "End"),
    (0x24, "Home"),
    (0x25, "Left"),
    (0x26, "Up"),
    (0x27, "Right"),
    (0x28, "Down"),
    (0x2C, "Print Screen"),
    (0x2D, "Insert"),
    (0x2E, "Delete"),
    (0x30, "0"),
    (0x31, "1"),
    (0x32, "2"),
    (0x33, "3"),
    (0x34, "4"),
    (0x35, "5"),
    (0x36, "6"),
    (0x37, "7"),
    (0x38, "8"),
    (0x39, "9"),
    (0x41, "A"),
    (0x42, "B"),
    (0x43, "C"),
    (0x44, "D"),
    (0x45, "E"),
    (0x46, "F"),
    (0x47, "G"),
    (0x48, "H"),
    (0x49, "I"),
    (0x4A, "J"),
    (0x4B, "K"),
    (0x4C, "L"),
    (0x4D, "M"),
    (0x4E, "N"),
    (0x4F, "O"),
    (0x50, "P"),
    (0x51, "Q"),
    (0x52, "R"),
    (0x53, "S"),
    (0x54, "T"),
    (0x55, "U"),
    (0x56, "V"),
    (0x57, "W"),
    (0x58, "X"),
    (0x59, "Y"),
    (0x5A, "Z"),
    (0x5B, "Left Windows"),
    (0x5C, "Right Windows"),
    (0x5D, "Applications"),
    (0x60, "Num 0"),
    (0x61, "Num 1"),
    (0x62, "Num 2"),
    (0x63, "Num 3"),
    (0x64, "Num 4"),
    (0x65, "Num 5"),
    (0x66, "Num 6"),
    (0x67, "Num 7"),
    (0x68, "Num 8"),
    (0x69, "Num 9"),
    (0x6A, "Num *"),
    (0x6B, "Num +"),
    (0x6D, "Num -"),
    (0x6E, "Num ."),
    (0x6F, "Num /"),
    (0x70, "F1"),
    (0x71, "F2"),
    (0x72, "F3"),
    (0x73, "F4"),
    (0x74, "F5"),
    (0x75, "F6"),
    (0x76, "F7"),
    (0x77, "F8"),
    (0x78, "F9"),
    (0x79, "F10"),
    (0x7A, "F11"),
    (0x7B, "F12"),
    (0x7C, "F13"),
    (0x7D, "F14"),
    (0x7E, "F15"),
    (0x7F, "F16"),
    (0x80, "F17"),
    (0x81, "F18"),
    (0x82, "F19"),
    (0x83, "F20"),
    (0x84, "F21"),
    (0x85, "F22"),
    (0x86, "F23"),
    (0x87, "F24"),
    (0x90, "Num Lock"),
    (0x91, "Scroll Lock"),
    (0xBA, ";"),
    (0xBB, "="),
    (0xBC, ","),
    (0xBD, "-"),
    (0xBE, "."),
    (0xBF, "/"),
    (0xC0, "`"),
    (0xDB, "["),
    (0xDC, "\\"),
    (0xDD, "]"),
    (0xDE, "'"),
];

/// The KRKR2 compatibility names added after the enumeration with
/// `force = false` (`0x10003370-0x10003550`): the dictionary gains them, an
/// array slot already named by the enumeration keeps that name.
const SHORTCUT_COMPAT_NAMES: &[(&str, i64)] = &[
    ("BkSp", 0x08),
    ("Del", 0x2E),
    ("PgUp", 0x21),
    ("PgDn", 0x22),
];

fn install_keycode_tables(runtime: &mut Runtime<KrkrHost>, class: ObjectHandle) {
    if !take_slot(runtime, class, "textToKeycode") || !take_slot(runtime, class, "keycodeToText") {
        return;
    }

    let (text_to_keycode, keycode_to_text) = build_keycode_tables(runtime);
    runtime.set_object_member(
        class,
        TEXT_TO_KEYCODE_BACKING,
        Variant::Object(text_to_keycode),
    );
    runtime.set_object_member(
        class,
        KEYCODE_TO_TEXT_BACKING,
        Variant::Object(keycode_to_text),
    );

    // The reference getters hand the shared objects out self-bound
    // (`tTJSVariant(dsp, dsp)`, `0x10009c80`/`0x10009cf0`).
    install_read_only_property(
        runtime,
        class,
        "textToKeycode",
        move |runtime, _this_obj| Ok(bound_member(runtime, class, TEXT_TO_KEYCODE_BACKING)),
    );
    install_read_only_property(
        runtime,
        class,
        "keycodeToText",
        move |runtime, _this_obj| Ok(bound_member(runtime, class, KEYCODE_TO_TEXT_BACKING)),
    );
}

/// A stored object member read back self-bound, or void when it is missing.
fn bound_member(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> Variant {
    match runtime.object_member(object, name).object_handle() {
        Some(handle) => Variant::self_bound(handle),
        None => Variant::Void,
    }
}

/// `CreateShortCutKeyCodeTable` (`0x10003080`): the enumeration equivalent
/// first, then the compatibility names with `force = false`, into one shared
/// `Dictionary` (lowercased name → key) and `Array` (key → name).
fn build_keycode_tables(runtime: &mut Runtime<KrkrHost>) -> (ObjectHandle, ObjectHandle) {
    let dictionary = runtime.alloc_dictionary_object();
    let highest = WINDOWS_KEY_NAMES
        .iter()
        .map(|(key, _)| *key)
        .chain(SHORTCUT_COMPAT_NAMES.iter().map(|(_, key)| *key))
        .max()
        .unwrap_or(0);
    let mut names = vec![Variant::Void; highest as usize + 1];

    for (key, name) in WINDOWS_KEY_NAMES {
        set_shortcut_keycode(runtime, dictionary, &mut names, name, *key, true);
    }
    for (name, key) in SHORTCUT_COMPAT_NAMES {
        set_shortcut_keycode(runtime, dictionary, &mut names, name, *key, false);
    }

    let array = runtime.alloc_array_object(names);
    (dictionary, array)
}

/// `SetShortCutKeyCode` (`0x10002cb0`): the dictionary always takes the
/// lowercased text; the array takes it only when the slot is still empty
/// unless `force` is set.
fn set_shortcut_keycode(
    runtime: &mut Runtime<KrkrHost>,
    dictionary: ObjectHandle,
    names: &mut [Variant],
    text: &str,
    key: i64,
    force: bool,
) {
    runtime.set_object_member(dictionary, text.to_lowercase(), Variant::Integer(key));
    let Some(slot) = names.get_mut(key as usize) else {
        return;
    };
    if !force && matches!(slot, Variant::String(_)) {
        return;
    }
    *slot = Variant::String(text.to_string());
}

// ---------------------------------------------------------------------------
// Tree helpers
// ---------------------------------------------------------------------------

/// The item a method or property runs on; `None` when `this` is missing (the
/// reference answers `TJS_E_INVALIDOBJECT` there, which a plugin cannot
/// distinguish from the class object itself).
fn bound_item(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Option<ObjectHandle> {
    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

/// The parent object the engine's tree operations maintain (`add`/`insert`
/// set it, `remove`/`clear` reset it), `None` for a root.
fn parent_of(runtime: &Runtime<KrkrHost>, item: ObjectHandle) -> Option<ObjectHandle> {
    runtime.object_member(item, "parent").object_handle()
}

/// `GetRootMenuItem` (`0x10006af0`): walk `parent` to the top. The depth cap
/// only guards a hand-written cyclic `parent` (a native tree cannot cycle).
fn root_of(runtime: &Runtime<KrkrHost>, item: ObjectHandle) -> ObjectHandle {
    let mut root = item;
    for _ in 0..64 {
        let Some(parent) = parent_of(runtime, root) else {
            break;
        };
        if parent == root {
            break;
        }
        root = parent;
    }
    root
}

/// The item's **own** `Window` field (reference `+0x2c`), which only the
/// object `TVPCreateMenuItemObject(window)` built carries: no walk, exactly
/// like the getter (`0x10009a50`). The engine's `Window.menu` object is that
/// item — its owner is the window and the window's `menu` member is it — so
/// it is the one object that answers the window; every `new MenuItem(…)`,
/// descendants included, stays detached like the reference's.
fn attached_window(runtime: &Runtime<KrkrHost>, item: ObjectHandle) -> Option<ObjectHandle> {
    let owner = runtime.object_member(item, "owner").object_handle()?;
    if !runtime
        .object_class_infos(owner)
        .iter()
        .any(|info| info == "Window")
    {
        return None;
    }
    let menu = runtime.object_member(owner, "menu").object_handle()?;
    (menu == item).then_some(owner)
}

/// `CanDeliverEvents` (`0x10006f60`) and `OnClick`'s window walk
/// (`0x10006dd8`): every ancestor including the item itself must be enabled,
/// and *some* item on the way to the root must carry its own `Window` field
/// (`0x10006db0`: `while (!item->Window) item = item->Parent`). With the
/// per-item [`attached_window`] that is the root's window for a descendant,
/// which is why `fireClick` works on children while `child.window` is void.
fn can_deliver_events(runtime: &Runtime<KrkrHost>, item: ObjectHandle) -> bool {
    let mut current = item;
    for _ in 0..64 {
        if !runtime.object_member(current, "enabled").is_truthy() {
            return false;
        }
        match parent_of(runtime, current) {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }

    let mut current = item;
    for _ in 0..64 {
        if attached_window(runtime, current).is_some() {
            return true;
        }
        match parent_of(runtime, current) {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::{
        TjsErrorKind,
        runtime::{ObjectHandle, Variant},
    };

    use super::{MenuPlugin, install_menu_surface};
    use crate::catalog::PluginStatus;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(MenuPlugin).expect("menu plugin");
        engine
    }

    fn class_object(engine: &mut KrkrEngine, name: &str) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} is not registered"))
    }

    fn script(engine: &mut KrkrEngine, source: &str) -> Variant {
        engine
            .execute_script("probe.tjs", source)
            .unwrap_or_else(|error| panic!("{source} failed: {}", error.message))
    }

    fn script_text(engine: &mut KrkrEngine, source: &str) -> String {
        script(engine, source)
            .to_tjs_string()
            .unwrap_or_else(|error| panic!("{source} did not produce a string: {}", error.message))
    }

    fn eval_error(engine: &mut KrkrEngine, source: &str) -> krkr_tjs2::TjsError {
        engine
            .execute_expression("probe.tjs", source)
            .expect_err(&format!("{source} should have failed"))
    }

    /// Like [`eval_error`] for probes that need statements.
    fn script_error(engine: &mut KrkrEngine, source: &str) -> krkr_tjs2::TjsError {
        engine
            .execute_script("probe.tjs", source)
            .expect_err(&format!("{source} should have failed"))
    }

    /// The members this engine installs on every `MenuItem` instance; the
    /// probe item below has all of them (`parent` comes from `add`).
    const INSTANCE_MEMBERS: &[&str] = &[
        "owner", "caption", "checked", "enabled", "group", "radio", "shortcut", "visible",
        "children", "parent", "index", "add", "insert", "remove", "clear", "click", "onClick",
        "popup",
    ];

    /// The members the reference registers on the class; this module adds the
    /// ones the engine lacks, the rest is the engine's class object.
    const CLASS_MEMBERS: &[&str] = &[
        "finalize",
        "MenuItem",
        "fireClick",
        "root",
        "window",
        "HMENU",
        "textToKeycode",
        "keycodeToText",
    ];

    /// A window, its engine-materialized `menu` root and a probe item added to
    /// it, all stored on the global object so closures can reach them (nested
    /// closures over locals are broken on this base, an engine bug filed
    /// separately).
    fn probe_tree(engine: &mut KrkrEngine) {
        script(
            engine,
            "global.__menuWindow = new Window(); \
             global.__menuRoot = global.__menuWindow.menu; \
             global.__menuItem = new MenuItem(null, \"probe\"); \
             global.__menuRoot.add(global.__menuItem);",
        );
    }

    fn global_object(engine: &mut KrkrEngine, name: &str) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} is not set"))
    }

    #[test]
    fn the_catalogue_resolves_menu_dll_and_reports_a_shim() {
        let entry = crate::catalog::resolve("menu.dll").expect("catalog entry");
        assert_eq!(entry.name, "menu.dll");
        assert_eq!(entry.meta.status, PluginStatus::Shim);
        assert!(!entry.is_placeholder());
        assert!(crate::catalog::is_same_plugin("MENU.DLL", "menu.dll"));
        assert!(crate::GameProfile::parquet().enables("menu.dll"));
    }

    #[test]
    fn the_member_surface_matches_the_reference() {
        let mut engine = engine();
        probe_tree(&mut engine);
        let item = global_object(&mut engine, "__menuItem");
        let class = class_object(&mut engine, "MenuItem");

        // Every instance member the binary and the engine publish.
        for name in INSTANCE_MEMBERS {
            assert!(
                engine.tjs_runtime().has_object_member(item, name),
                "MenuItem.{name} is not on the instance"
            );
        }
        // The class members the binary registers; this module adds the ones
        // the engine has no counterpart for.
        for name in CLASS_MEMBERS {
            assert!(
                engine.tjs_runtime().has_object_member(class, name),
                "MenuItem.{name} is not on the class"
            );
        }

        {
            let runtime = engine.tjs_runtime();
            for name in ["fireClick", "finalize"] {
                let member = runtime.object_member(class, name);
                assert!(
                    member
                        .object_handle()
                        .is_some_and(|handle| runtime.object_is_callable(handle)),
                    "MenuItem.{name} is not callable"
                );
            }
            for name in ["root", "window", "HMENU", "textToKeycode", "keycodeToText"] {
                assert!(
                    runtime.object_member_is_property(class, name),
                    "MenuItem.{name} is not a native property"
                );
            }
            // The table getters return the shared `Dictionary`/`Array` the
            // registration built (`CreateShortCutKeyCodeTable`, `0x10003080`).
            let dictionary = runtime.object_member(class, super::TEXT_TO_KEYCODE_BACKING);
            assert!(
                dictionary
                    .object_handle()
                    .is_some_and(|handle| runtime.is_dictionary_instance(handle)),
                "MenuItem.textToKeycode is not a Dictionary"
            );
            let array = runtime.object_member(class, super::KEYCODE_TO_TEXT_BACKING);
            assert!(
                matches!(array, Variant::Object(_)),
                "MenuItem.keycodeToText is not an Array object"
            );
        }

        // An instance resolves the class members through the class link, and
        // `index` is a native property on it (`manual.tjs:134`).
        assert_eq!(
            script_text(
                &mut engine,
                "return \"\" + global.__menuItem.HMENU \
                    + (global.__menuItem.textToKeycode === MenuItem.textToKeycode) \
                    + (global.__menuItem.root === global.__menuItem.root) \
                    + global.__menuItem.index;"
            ),
            "0110"
        );
    }

    #[test]
    fn fire_click_delivers_on_click_and_obeys_the_reference_gate() {
        let mut engine = engine();
        let source = "\
            global.__clickFired = 0; \
            global.__looseFired = 0; \
            global.__kidFired = 0; \
            var w = new Window(); \
            var root = w.menu; \
            var child = new MenuItem(null, \"child\"); \
            root.add(child); \
            child.onClick = function() { global.__clickFired += 1; }; \
            child.fireClick(); \
            var loose = new MenuItem(null, \"loose\"); \
            loose.onClick = function() { global.__looseFired += 1; }; \
            loose.fireClick(); \
            var parentless = new MenuItem(null, \"parent\"); \
            var kid = new MenuItem(null, \"kid\"); \
            parentless.add(kid); \
            kid.onClick = function() { global.__kidFired += 1; }; \
            kid.fireClick(); \
            child.enabled = 0; \
            child.fireClick(); \
            child.enabled = 1; \
            root.enabled = 0; \
            child.fireClick(); \
            root.enabled = 1; \
            child.fireClick(); \
            return \"\" + global.__clickFired + global.__looseFired + global.__kidFired;";
        // Delivered for the attached child (twice, once per enabled state); a
        // standalone item and a tree whose root has no window deliver nothing
        // (the reference's `OnClick` window walk); a disabled item or ancestor
        // suppresses (`CanDeliverEvents`).
        assert_eq!(script_text(&mut engine, source), "200");

        // A script override that throws must not escape `fireClick` — the
        // reference raises inside the posted event, not at the caller.
        let source = "\
            global.__badDone = 0; \
            var w = new Window(); \
            var root = w.menu; \
            var item = new MenuItem(null, \"bad\"); \
            root.add(item); \
            item.onClick = function() { throw \"boom\"; }; \
            var ok = item.fireClick() === void; \
            global.__badDone = 1; \
            return \"\" + ok + global.__badDone;";
        assert_eq!(script_text(&mut engine, source), "11");

        // The reference's callback ignores the argument count, so extra
        // arguments are accepted (`0x10008fd0` has no `numparams` check).
        let source = "\
            global.__argFired = 0; \
            var w = new Window(); \
            var root = w.menu; \
            var child = new MenuItem(null, \"child\"); \
            root.add(child); \
            child.onClick = function() { global.__argFired += 1; }; \
            child.fireClick(1, 2, 3); \
            return \"\" + global.__argFired;";
        assert_eq!(script_text(&mut engine, source), "1");
    }

    /// The reference's default `onClick` forwards to the action owner
    /// (`0x10008ee0`): `actionOwner.action(event)` with a `TVPCreateEventObject`
    /// event whose `type` is `"onClick"` and whose `target` is the item.
    #[test]
    fn fire_click_forwards_to_the_action_owner_without_a_script_override() {
        let mut engine = engine();
        let source = "\
            global.__actionSeen = \"none\"; \
            global.__owner = %[]; \
            global.__owner.action = function(ev) { \
                global.__actionSeen = ev.type + \":\" + (ev.target === global.__menuItem); \
            }; \
            global.__menuWindow = new Window(); \
            global.__menuRoot = global.__menuWindow.menu; \
            global.__menuItem = new MenuItem(global.__owner, \"item\"); \
            global.__menuRoot.add(global.__menuItem); \
            global.__menuItem.fireClick(); \
            return global.__actionSeen;";
        assert_eq!(script_text(&mut engine, source), "onClick:1");

        // No action owner (a null owner) and no `action` member both stay
        // silent, exactly like the reference's null `ActionOwner`.
        let source = "\
            global.__silent = 0; \
            global.__menuOwnerless = new MenuItem(null, \"ownerless\"); \
            global.__menuRoot.add(global.__menuOwnerless); \
            global.__menuOwnerless.fireClick(); \
            global.__silent = 1; \
            return \"\" + global.__silent;";
        assert_eq!(script_text(&mut engine, source), "1");
    }

    #[test]
    fn root_walks_the_engine_tree_and_window_is_per_item() {
        let mut engine = engine();
        let source = "\
            var w = new Window(); \
            var root = w.menu; \
            var mid = new MenuItem(null, \"mid\"); \
            root.add(mid); \
            var leaf = new MenuItem(null, \"leaf\"); \
            mid.add(leaf); \
            var rooted = \"\" + (leaf.root === mid.root) + (mid.root === mid.root); \
            var windowed = \"\" + (leaf.window === void) + (mid.window === void); \
            var loose = new MenuItem(null, \"loose\"); \
            var detached = \"\" + (loose.root === loose) + (loose.window === void); \
            var lonely = new MenuItem(null, \"lonely\"); \
            var childless = lonely.window === void; \
            return rooted + \"/\" + windowed + \"/\" + detached + \"/\" + childless;";
        // `root` walks `parent` to the same object from every depth; `window`
        // is the item's *own* field (`+0x2c`, `0x10009aa6`, no walk), so a
        // descendant and a standalone item both answer void — exactly the
        // reference, whose `AddChild` (`0x10006f90`) only writes `Parent`
        // (`+0x40`).
        assert_eq!(script_text(&mut engine, source), "11/11/11/1");

        // Only the object `TVPCreateMenuItemObject(window)` built carries the
        // field; here that is the engine's `Window.menu` member. Exercise the
        // per-item rule on it directly — its class-level members are not
        // readable from script while the engine leaves it unlinked (see the
        // module docs and the gap assertion below).
        probe_tree(&mut engine);
        let window = global_object(&mut engine, "__menuWindow");
        let menu = global_object(&mut engine, "__menuRoot");
        let item = global_object(&mut engine, "__menuItem");
        {
            let runtime = engine.tjs_runtime();
            assert_eq!(super::attached_window(runtime, menu), Some(window));
            assert_eq!(super::attached_window(runtime, item), None);
            assert!(super::can_deliver_events(runtime, item));
        }

        // The engine materializes `window.menu` without a class link (see the
        // module docs), so the class-level members this module adds are not
        // readable on the root object itself — only on `new MenuItem(…)`
        // instances. This pins that engine gap; fixing it means updating the
        // docs here.
        assert_eq!(
            script_error(&mut engine, "return global.__menuRoot.root;").kind,
            TjsErrorKind::MemberNotFound
        );
    }

    #[test]
    fn read_only_members_deny_script_writes() {
        let mut engine = engine();
        for source in [
            "MenuItem.root = 1",
            "MenuItem.window = 1",
            "MenuItem.HMENU = 1",
            "MenuItem.textToKeycode = 1",
            "MenuItem.keycodeToText = 1",
        ] {
            assert_eq!(
                eval_error(&mut engine, source).kind,
                TjsErrorKind::AccessDenied,
                "{source}"
            );
        }
        for source in [
            "var item = new MenuItem(null, \"x\"); item.root = 1;",
            "var item = new MenuItem(null, \"x\"); item.window = 1;",
            "var item = new MenuItem(null, \"x\"); item.HMENU = 1;",
        ] {
            assert_eq!(
                script_error(&mut engine, source).kind,
                TjsErrorKind::AccessDenied,
                "{source}"
            );
        }
    }

    #[test]
    fn hmenu_is_the_no_native_menu_value() {
        let mut engine = engine();
        assert_eq!(
            script_text(
                &mut engine,
                "var item = new MenuItem(null, \"x\"); return \"\" + item.HMENU;"
            ),
            "0"
        );
    }

    #[test]
    fn finalize_is_the_reference_no_op() {
        let mut engine = engine();
        assert_eq!(
            script_text(
                &mut engine,
                "var item = new MenuItem(null, \"x\"); \
                 return \"\" + (item.finalize() === void) + (item.fireClick() === void);"
            ),
            "11"
        );
    }

    #[test]
    fn keycode_tables_match_the_binary_surface() {
        let mut engine = engine();
        // The four compatibility names are binary-verified (`0x100033d1`,
        // `0x1000344a`, `0x100034c3`, `0x1000353c`); the rest of the table is
        // the standard Windows name set (inferred).
        let source = "\
            var text = MenuItem.textToKeycode; \
            return \"\" + text[\"bksp\"] + \"/\" + text[\"del\"] + \"/\" + text[\"pgup\"] \
                + \"/\" + text[\"pgdn\"] + \"/\" + text[\"backspace\"] + \"/\" + text[\"f1\"];";
        assert_eq!(script_text(&mut engine, source), "8/46/33/34/8/112");

        let source = "\
            var keys = MenuItem.keycodeToText; \
            return keys[8] + \"/\" + keys[0x22] + \"/\" + keys[0x41] + \"/\" + keys[0x70];";
        assert_eq!(script_text(&mut engine, source), "Backspace/Page Down/A/F1");

        // One shared object per class, exactly like the reference's
        // process-wide statics.
        assert_eq!(
            script_text(
                &mut engine,
                "return \"\" + (MenuItem.textToKeycode === MenuItem.textToKeycode) \
                    + (MenuItem.keycodeToText === MenuItem.keycodeToText);"
            ),
            "11"
        );
    }

    #[test]
    fn installing_twice_keeps_the_tables_and_the_members() {
        // `KrkrPlugin::register` runs at boot and again on the first
        // `Plugins.link`; the second pass must not rebuild or shadow anything.
        let mut engine = engine();
        install_menu_surface(engine.tjs_runtime_mut());
        install_menu_surface(engine.tjs_runtime_mut());
        assert_eq!(
            script_text(
                &mut engine,
                "return \"\" + (MenuItem.textToKeycode === MenuItem.textToKeycode) \
                    + \"/\" + MenuItem.textToKeycode[\"del\"];"
            ),
            "1/46"
        );
    }
}
