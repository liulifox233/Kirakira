//! k2compat.dll compatibility layer (`docs/plugins/k2compat.md`).
//!
//! The shipped DLL provides the two native pieces the krkrz `k2compat/*.tjs`
//! scripts need:
//!
//! - **`Window.TouchMouse`** — the `WindowTouchMouse` class object bound onto
//!   the `Window` class object, with the `enabled` flag and five
//!   `onTouchMouse*` callbacks (`Down`, `Move`, `Up`, `Click`, `DblClick`).
//!   The DLL binds a *class*: games construct it (`new Window.TouchMouse(kag)`)
//!   and hand the instance to their own hook lists, so the shim binds a native
//!   constructor whose instances carry the same members.
//! - **`ModelessOwnerWindow`** — the class behind K2 modeless dialogs, built on
//!   a hidden native owner window (`USER32`). The dossier could not recover its
//!   member list.
//!
//! Kirakira delivers touch and mouse input as the ordinary KRKR pointer
//! callbacks (`Window.onMouseDown/Move/Up`, `Layer.onMouse*`) — see
//! `EngineEvent::TouchInput` in krkr-engine — and no engine code dispatches
//! `onTouchMouse*`, so a game that drives its touch input through these
//! callbacks does not receive the engine's input that way. The five callbacks
//! are not inert, though: they carry the *reference's* arities and forwarding
//! (`k2compat.dll` `0x10004390`-`0x100048d0`, each `SimpleBinder` member
//! rejecting short calls), so a script — or a hook list, as GINKA's
//! `MouseGestureBase` hands its instance to one — that calls one lands on the
//! Window's same-kind `on*` event with the arguments converted to integers.
//! The Window is `Window.mainWindow` (the object the engine delivers its own
//! pointer events to); which window the DLL resolves internally is the one gap
//! this port cannot pin from the binary, as is the reference's delivery — its
//! handler path posts through the Window's draw device, so the reference's
//! `on*` may run on a later turn where the port's runs inline.
//! `ModelessOwnerWindow` is an empty class object, enough for the dossier's
//! `typeof`-only probe; no member is claimed because none was recovered.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Window.TouchMouse and ModelessOwnerWindow",
    notes: "TouchMouse is the WindowTouchMouse class object the DLL binds: `new Window.TouchMouse(owner)` yields an instance carrying `enabled` and the five onTouchMouse* callbacks (the class object carries them too, so a script can read `Window.TouchMouse.enabled` without constructing one). The five callbacks carry the reference's arities (onTouchMouseDown/Up >=4, onTouchMouseMove >=3, onTouchMouseClick/DblClick >=2; ncbind/SimpleBinder reject short calls) and the reference's forwarding: each calls the Window's same-kind `on*` event (onMouseDown/Up/Move, onClick, onDoubleClick) with the arguments converted to integers. The port forwards to `Window.mainWindow`, the object the engine delivers its own pointer events to; which window the DLL resolves internally could not be pinned from the binary. The engine itself never dispatches onTouchMouse* (Kirakira routes touch/mouse input to the KRKR pointer callbacks Window/Layer onMouseDown/Move/Up, and a plugin module cannot hook that dispatch). ModelessOwnerWindow is an empty class object: the dossier could not recover its member list, so none is claimed.",
    install: |engine| engine.register_plugin(K2CompatPlugin),
};
pub struct K2CompatPlugin;

impl KrkrPlugin for K2CompatPlugin {
    fn name(&self) -> &str {
        "k2compat.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_window_touch_mouse(runtime);
        install_modeless_owner_window(runtime);
        Ok(())
    }
}

/// The five callbacks, with the reference's `ArgsCount` and the `Window` event
/// each one forwards to.
///
/// The arities are `cmpl` thresholds in `k2compat.dll`'s SimpleBinder members,
/// cross-checked against the ncbind rule `numparams < ArgsCount` →
/// `TJS_E_BADPARAMCOUNT`: `onTouchMouseDown` ≥4 (`0x100044d0`, `cmpl $0x4` at
/// `0x100044f4`), `onTouchMouseUp` ≥4 (`0x10004640`, `0x10004664`),
/// `onTouchMouseMove` ≥3 (`0x10004390`, `0x100043b4`), `onTouchMouseClick` ≥2
/// (`0x100047b0`, `0x100047d3`), `onTouchMouseDblClick` ≥2 (`0x100048d0`,
/// `0x100048f3`).
///
/// Each body resolves the Window's same-kind handler and calls it with the
/// arguments converted to integers, in order: `onMouseDown`/`onMouseUp` get all
/// four (`0x10004563-0x10004621` pushes args[3], args[2], args[1], args[0] into
/// the call), `onMouseMove` three, `onClick`/`onDoubleClick` two. The class
/// table that installs them is built in `FUN_10004cc0`.
const TOUCH_MOUSE_EVENTS: &[(&str, usize, &str)] = &[
    ("onTouchMouseDown", 4, "onMouseDown"),
    ("onTouchMouseUp", 4, "onMouseUp"),
    ("onTouchMouseMove", 3, "onMouseMove"),
    ("onTouchMouseClick", 2, "onClick"),
    ("onTouchMouseDblClick", 2, "onDoubleClick"),
];

/// Members the dossier recovered for `ModelessOwnerWindow`: none. The empty
/// list is deliberate — the shim claims nothing the survey could not confirm.
const MODELESS_OWNER_WINDOW_MEMBERS: &[&str] = &[];

fn install_window_touch_mouse(runtime: &mut Runtime<KrkrHost>) {
    let Some(window) = runtime.global_member("Window").object_handle() else {
        return;
    };
    // A `TouchMouse` a script already published wins; never shadow it.
    if runtime
        .object_member(window, "TouchMouse")
        .object_handle()
        .is_some()
    {
        return;
    }
    // The DLL's binder binds the *class object* of `WindowTouchMouse` under
    // `Window.TouchMouse` (`docs/plugins/k2compat.md`), and games construct it:
    // GINKA's `sysscn/gesture.tjs` runs `new Window.TouchMouse(kag)` and hands
    // the result to its hook list. A plain object is not callable, so that
    // `new` fails with `TJS_E_INVALIDTYPE` and the boot stops before the first
    // scenario. The class object carries the members too, because scripts read
    // `Window.TouchMouse.enabled` without constructing anything.
    let touch_mouse = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "WindowTouchMouse");
            install_touch_mouse_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(touch_mouse, "WindowTouchMouse");
    install_touch_mouse_members(runtime, touch_mouse);
    runtime.set_object_member(window, "TouchMouse", Variant::Object(touch_mouse));
}

// `result_large_err` is the crate-wide `TjsError` size lint every native
// handler closure carries (`http_request.rs`/`sqlite3.rs` allow it too); the
// new forwarding closures must not add instances of it.
#[allow(clippy::result_large_err)]
fn install_touch_mouse_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.set_object_member(handle, "__enabled", Variant::Integer(0));
    runtime.register_object_native_property(
        handle,
        "enabled",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let enabled = this_obj
                .map(|object| runtime.bound_this(object).unwrap_or(object))
                .and_then(|object| match runtime.object_member(object, "__enabled") {
                    Variant::Integer(value) => Some(value),
                    _ => None,
                })
                .unwrap_or(0);
            Ok(Variant::Integer(enabled))
        },
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            if let Some(object) =
                this_obj.map(|object| runtime.bound_this(object).unwrap_or(object))
            {
                runtime.set_object_member(
                    object,
                    "__enabled",
                    Variant::Integer(i64::from(value.is_truthy())),
                );
            }
            Ok(())
        },
    );
    for (event, arg_count, target) in TOUCH_MOUSE_EVENTS {
        let (event, target) = (*event, *target);
        runtime.register_object_native_with_arg_count(
            handle,
            event,
            NativeArgCount::AtLeast(*arg_count),
            move |runtime: &mut Runtime<KrkrHost>,
                  _this_obj: Option<ObjectHandle>,
                  args: Vec<Variant>| {
                let Some(window) = main_window(runtime) else {
                    return Ok(Variant::Void);
                };
                // Through the dispatch path: the handler may be a script
                // function (a raw member) or a native property.
                let handler = runtime.resolve_object_member(window, target)?;
                if matches!(handler, Variant::Void) {
                    return Ok(Variant::Void);
                }
                // The DLL hands the event on with every argument converted to
                // an integer (`tTJSVariant::operator tjs_int`), in order.
                let forwarded = args
                    .iter()
                    .map(Variant::to_integer)
                    .collect::<Result<Vec<i64>>>()?
                    .into_iter()
                    .map(Variant::Integer)
                    .collect();
                runtime.call_function(handler, forwarded)?;
                Ok(Variant::Void)
            },
        );
    }
}

/// The Window the touch-mouse callbacks forward to: the engine's main window
/// (`Window.mainWindow`, the object its own pointer events are delivered to).
///
/// The DLL resolves the target itself (`FUN_100040e0` reads the Window handler
/// through the class store and posts the event to it); a script-side probe of
/// which window that is would need the real engine, so the port pins it to the
/// process's main window and reports the gap.
fn main_window(runtime: &mut Runtime<KrkrHost>) -> Option<ObjectHandle> {
    let class = runtime.global_member("Window").object_handle()?;
    runtime
        .resolve_object_member(class, "mainWindow")
        .ok()?
        .object_handle()
}

fn install_modeless_owner_window(runtime: &mut Runtime<KrkrHost>) {
    if runtime
        .global_member("ModelessOwnerWindow")
        .object_handle()
        .is_some()
    {
        return;
    }
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "ModelessOwnerWindow");
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "ModelessOwnerWindow");
    for member in MODELESS_OWNER_WINDOW_MEMBERS {
        runtime.register_object_native(class, *member, native_void);
    }
    runtime.set_global_member("ModelessOwnerWindow", Variant::Object(class));
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

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use krkr_core::{ButtonState, EngineEvent, FrameInput, Point, PointerButton, Size};
    use krkr_engine::{EngineConfig, EngineInput, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::{K2CompatPlugin, TOUCH_MOUSE_EVENTS};

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(K2CompatPlugin).expect("plugin");
        engine
    }

    #[test]
    fn window_touch_mouse_carries_the_dossier_members() {
        let engine = engine();
        let runtime = engine.tjs_runtime();
        let window = runtime
            .global_member("Window")
            .object_handle()
            .expect("Window class object");
        let touch_mouse = runtime
            .object_member(window, "TouchMouse")
            .object_handle()
            .expect("Window.TouchMouse");
        assert!(
            runtime
                .object_class_infos(touch_mouse)
                .iter()
                .any(|info| info == "WindowTouchMouse"),
            "TouchMouse does not carry the WindowTouchMouse class name"
        );

        assert!(
            !matches!(runtime.object_member(touch_mouse, "enabled"), Variant::Void),
            "Window.TouchMouse.enabled is missing"
        );
        for (event, _, _) in TOUCH_MOUSE_EVENTS {
            assert!(
                !matches!(runtime.object_member(touch_mouse, event), Variant::Void),
                "Window.TouchMouse.{event} is missing"
            );
        }
    }

    /// The DLL's binder binds the `WindowTouchMouse` *class object* under
    /// `Window.TouchMouse`, and games construct it: GINKA's
    /// `sysscn/gesture.tjs` runs `new Window.TouchMouse(kag)` inside
    /// `MouseGestureBase` and hands the instance to its hook list. A bare
    /// ordinary object is not callable, so that `new` raises
    /// `TJS_E_INVALIDTYPE` and the boot stops before the first scenario (the
    /// `1c1c754` regression).
    #[test]
    fn the_touch_mouse_is_the_constructible_class_object() {
        let mut engine = engine();
        let value = engine
            .execute_script(
                "probe.tjs",
                r#"
                var instance = new Window.TouchMouse(0);
                var declared = 0;
                if (typeof instance.onTouchMouseDown != "undefined") declared++;
                if (typeof instance.onTouchMouseMove != "undefined") declared++;
                if (typeof instance.onTouchMouseUp != "undefined") declared++;
                if (typeof instance.onTouchMouseClick != "undefined") declared++;
                if (typeof instance.onTouchMouseDblClick != "undefined") declared++;
                var before = instance.enabled;
                instance.enabled = true;
                var callable = typeof instance.onTouchMouseDown(0, 0, 0, 0) == "void";
                return typeof Window.TouchMouse + "|" + typeof instance + "|" +
                    before + "|" + instance.enabled + "|" + declared + "|" + callable;
                "#,
            )
            .expect("construct Window.TouchMouse");
        assert_eq!(value, Variant::String("Object|Object|0|1|5|1".to_string()));
    }

    #[test]
    fn modeless_owner_window_is_an_empty_class_object() {
        let engine = engine();
        let runtime = engine.tjs_runtime();
        let class = runtime
            .global_member("ModelessOwnerWindow")
            .object_handle()
            .expect("ModelessOwnerWindow class object");
        // The dossier recovered no member list, so the shim claims none.
        assert!(
            runtime.object_members(class).is_empty(),
            "ModelessOwnerWindow claims members the survey did not recover: {:?}",
            runtime.object_members(class)
        );
        assert!(
            runtime
                .object_class_infos(class)
                .iter()
                .any(|info| info == "ModelessOwnerWindow"),
            "the class object does not carry its own class name"
        );
    }

    #[test]
    fn modeless_owner_window_instances_exist_for_typeof_probes() {
        let mut engine = engine();
        let value = engine
            .execute_script(
                "probe.tjs",
                r#"
                var probe = typeof global.ModelessOwnerWindow;
                var instance = new ModelessOwnerWindow();
                return probe + "|" + typeof instance + "|" +
                    typeof instance.some_unknown_member;
                "#,
            )
            .expect("probe modeless owner window");
        assert_eq!(
            value,
            Variant::String("Object|Object|undefined".to_string())
        );
    }

    #[test]
    fn enabled_defaults_off_and_round_trips() {
        let mut engine = engine();
        let value = engine
            .execute_script(
                "probe.tjs",
                r#"
                var before = Window.TouchMouse.enabled;
                Window.TouchMouse.enabled = true;
                var after = Window.TouchMouse.enabled;
                Window.TouchMouse.enabled = false;
                return before + "|" + after + "|" + Window.TouchMouse.enabled;
                "#,
            )
            .expect("probe enabled flag");
        assert_eq!(value, Variant::String("0|1|0".to_string()));
    }

    /// Touch/mouse input reaches the KRKR pointer callbacks and never the
    /// `onTouchMouse*` handlers: our input model has no touch-mouse event
    /// source, and a plugin module cannot hook the engine's dispatch.
    #[test]
    fn touch_input_drives_the_pointer_callbacks_not_the_touch_mouse_events() {
        let mut engine = engine();
        engine
            .execute_script(
                "setup.tjs",
                r#"
                global.pointerHits = 0;
                global.touchHits = 0;
                var window = new Window();
                window.onMouseDown = function(x, y, button, shift) { global.pointerHits++; };
                window.onMouseMove = function(x, y, shift) { global.pointerHits++; };
                window.onMouseUp = function(x, y, button, shift) { global.pointerHits++; };
                Window.TouchMouse.onTouchMouseDown = function() { global.touchHits++; };
                Window.TouchMouse.onTouchMouseMove = function() { global.touchHits++; };
                Window.TouchMouse.onTouchMouseUp = function() { global.touchHits++; };
                Window.TouchMouse.onTouchMouseClick = function() { global.touchHits++; };
                Window.TouchMouse.onTouchMouseDblClick = function() { global.touchHits++; };
                "#,
            )
            .expect("setup handlers");

        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::TouchInput {
                            id: 1,
                            position: Point::new(10.0, 10.0),
                            phase: krkr_core::TouchPhase::Started,
                        },
                        EngineEvent::TouchInput {
                            id: 1,
                            position: Point::new(12.0, 12.0),
                            phase: krkr_core::TouchPhase::Moved,
                        },
                        EngineEvent::TouchInput {
                            id: 1,
                            position: Point::new(12.0, 12.0),
                            phase: krkr_core::TouchPhase::Ended,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("touch update");

        assert_eq!(
            engine
                .execute_expression("setup.tjs", "pointerHits")
                .expect("pointer hits"),
            Variant::Integer(3),
            "touch input must still reach the KRKR pointer callbacks"
        );
        assert_eq!(
            engine
                .execute_expression("setup.tjs", "touchHits")
                .expect("touch hits"),
            Variant::Integer(0),
            "the touch-mouse handlers must stay inert"
        );
    }

    /// The reference's arities: each member rejects one-argument-short calls
    /// with `TJS_E_BADPARAMCOUNT` (ncbind's `numparams < ArgsCount`, the
    /// `cmpl` thresholds in `k2compat.dll`) and accepts the reference count.
    #[test]
    fn the_touch_mouse_arities_match_the_reference() {
        let mut engine = engine();
        engine
            .execute_script(
                "setup.tjs",
                "global.accepted = 0; global.rejected = 0;\
                 function probe(f) {\
                     try { f(); global.accepted++; } catch (e) { global.rejected++; }\
                 }",
            )
            .expect("setup probe");
        for (call, short) in [
            (
                "probe(function() { Window.TouchMouse.onTouchMouseDown(1, 2, 3, 4); });",
                "probe(function() { Window.TouchMouse.onTouchMouseDown(1, 2, 3); });",
            ),
            (
                "probe(function() { Window.TouchMouse.onTouchMouseUp(1, 2, 3, 4); });",
                "probe(function() { Window.TouchMouse.onTouchMouseUp(1, 2, 3); });",
            ),
            (
                "probe(function() { Window.TouchMouse.onTouchMouseMove(1, 2, 3); });",
                "probe(function() { Window.TouchMouse.onTouchMouseMove(1, 2); });",
            ),
            (
                "probe(function() { Window.TouchMouse.onTouchMouseClick(1, 2); });",
                "probe(function() { Window.TouchMouse.onTouchMouseClick(1); });",
            ),
            (
                "probe(function() { Window.TouchMouse.onTouchMouseDblClick(1, 2); });",
                "probe(function() { Window.TouchMouse.onTouchMouseDblClick(1); });",
            ),
        ] {
            let accepted_before = integer(&mut engine, "accepted");
            let rejected_before = integer(&mut engine, "rejected");
            engine
                .execute_script("call.tjs", call)
                .expect("minimum call");
            assert_eq!(
                integer(&mut engine, "accepted"),
                accepted_before + 1,
                "{call} is the reference minimum and must be accepted"
            );
            engine
                .execute_script("call.tjs", short)
                .expect("short call");
            assert_eq!(
                integer(&mut engine, "rejected"),
                rejected_before + 1,
                "{short} is one argument short of the reference floor"
            );
        }
    }

    /// The recovered forwarding: each callback calls the Window's same-kind
    /// `on*` event with its arguments converted to integers, in order
    /// (`onTouchMouseDown` → `onMouseDown(1, 2, 3, 4)` and so on).
    #[test]
    fn the_touch_mouse_callbacks_forward_to_the_windows_same_kind_event() {
        let mut engine = engine();
        let value = engine
            .execute_script(
                "probe.tjs",
                r#"
                global.log = "";
                var window = new Window();
                window.onMouseDown = function(x, y, button, shift) {
                    global.log += "down(" + x + "," + y + "," + button + "," + shift + ");";
                };
                window.onMouseUp = function(x, y, button, shift) {
                    global.log += "up(" + x + "," + y + "," + button + "," + shift + ");";
                };
                window.onMouseMove = function(x, y, shift) {
                    global.log += "move(" + x + "," + y + "," + shift + ");";
                };
                window.onClick = function(x, y) { global.log += "click(" + x + "," + y + ");"; };
                window.onDoubleClick = function(x, y) { global.log += "dbl(" + x + "," + y + ");"; };
                var touch = new Window.TouchMouse(0);
                touch.onTouchMouseDown(1, 2, 3, 4);
                touch.onTouchMouseUp(5, 6, 7, 8);
                touch.onTouchMouseMove(9, 10, 11);
                touch.onTouchMouseClick(12, 13);
                touch.onTouchMouseDblClick(14, 15);
                return log;
                "#,
            )
            .expect("forward the touch events");
        assert_eq!(
            value,
            Variant::String(
                "down(1,2,3,4);up(5,6,7,8);move(9,10,11);click(12,13);dbl(14,15);".to_string()
            )
        );
    }

    /// Without a Window the callbacks are callable no-ops — the same early-out
    /// the DLL's handler resolution has when it finds no window handler.
    #[test]
    fn the_touch_mouse_callbacks_without_a_window_are_callable_no_ops() {
        let mut engine = engine();
        let value = engine
            .execute_script(
                "probe.tjs",
                r#"
                return typeof Window.TouchMouse.onTouchMouseDown(1, 2, 3, 4) + "|" +
                    typeof Window.TouchMouse.onTouchMouseDblClick(1, 2);
                "#,
            )
            .expect("call the declared events");
        assert_eq!(value, Variant::String("void|void".to_string()));
    }

    fn integer(engine: &mut KrkrEngine, expression: &str) -> i64 {
        engine
            .execute_expression("read.tjs", expression)
            .unwrap_or_else(|error| panic!("{expression}: {error}"))
            .to_integer()
            .expect("integer")
    }

    #[test]
    fn plain_pointer_input_also_leaves_the_touch_mouse_events_alone() {
        let mut engine = engine();
        engine
            .execute_script(
                "setup.tjs",
                r#"
                global.pointerHits = 0;
                global.touchHits = 0;
                var window = new Window();
                window.onMouseDown = function(x, y, button, shift) { global.pointerHits++; };
                window.onMouseUp = function(x, y, button, shift) { global.pointerHits++; };
                Window.TouchMouse.onTouchMouseDown = function() { global.touchHits++; };
                Window.TouchMouse.onTouchMouseUp = function() { global.touchHits++; };
                Window.TouchMouse.onTouchMouseClick = function() { global.touchHits++; };
                "#,
            )
            .expect("setup handlers");
        engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 0.0),
                    vec![
                        EngineEvent::CursorMoved {
                            position: Point::new(4.0, 4.0),
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Pressed,
                        },
                        EngineEvent::PointerInput {
                            button: PointerButton::Primary,
                            state: ButtonState::Released,
                        },
                    ],
                ),
                Duration::ZERO,
            )
            .expect("mouse update");
        assert_eq!(
            engine
                .execute_expression("setup.tjs", "pointerHits")
                .expect("pointer hits"),
            Variant::Integer(2)
        );
        assert_eq!(
            engine
                .execute_expression("setup.tjs", "touchHits")
                .expect("touch hits"),
            Variant::Integer(0)
        );
    }
}
