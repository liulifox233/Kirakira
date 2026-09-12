//! k2compat.dll compatibility layer (`docs/plugins/k2compat.md`).
//!
//! The shipped DLL provides the two native pieces the krkrz `k2compat/*.tjs`
//! scripts need:
//!
//! - **`Window.TouchMouse`** — a `WindowTouchMouse`-shaped event source bound
//!   onto the `Window` class object, with the `enabled` flag and five
//!   `onTouchMouse*` callbacks (`Down`, `Move`, `Up`, `Click`, `DblClick`).
//! - **`ModelessOwnerWindow`** — the class behind K2 modeless dialogs, built on
//!   a hidden native owner window (`USER32`). The dossier could not recover its
//!   member list.
//!
//! Kirakira delivers touch and mouse input as the ordinary KRKR pointer
//! callbacks (`Window.onMouseDown/Move/Up`, `Layer.onMouse*`) — see
//! `EngineEvent::TouchInput` in krkr-engine — and no engine code dispatches
//! `onTouchMouse*`. A plugin module installs its surface at registration time
//! and cannot hook that dispatch from here, so `TouchMouse` is declared and
//! inert: `enabled` stores the flag and the five callbacks are callable no-ops.
//! Handlers a script assigns to them are kept but never invoked.
//! `ModelessOwnerWindow` is an empty class object, enough for the dossier's
//! `typeof`-only probe; no member is claimed because none was recovered.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Window.TouchMouse and ModelessOwnerWindow",
    notes: "TouchMouse is declared and inert: `enabled` stores the flag and the five onTouchMouse* callbacks are callable no-ops the engine never invokes (Kirakira routes touch/mouse input to the KRKR pointer callbacks Window/Layer onMouseDown/Move/Up, and a plugin module cannot hook that dispatch). ModelessOwnerWindow is an empty class object: the dossier could not recover its member list, so none is claimed.",
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

/// The callbacks the DLL's class carries (`docs/plugins/k2compat.md`).
const TOUCH_MOUSE_EVENTS: &[&str] = &[
    "onTouchMouseDown",
    "onTouchMouseMove",
    "onTouchMouseUp",
    "onTouchMouseClick",
    "onTouchMouseDblClick",
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
    let touch_mouse = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(touch_mouse, "WindowTouchMouse");
    install_touch_mouse_members(runtime, touch_mouse);
    runtime.set_object_member(window, "TouchMouse", Variant::Object(touch_mouse));
}

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
    for event in TOUCH_MOUSE_EVENTS {
        runtime.register_object_native(handle, *event, native_void);
    }
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
        for event in TOUCH_MOUSE_EVENTS {
            assert!(
                !matches!(runtime.object_member(touch_mouse, event), Variant::Void),
                "Window.TouchMouse.{event} is missing"
            );
        }
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

    #[test]
    fn the_declared_touch_mouse_events_are_callable_no_ops() {
        let mut engine = engine();
        let value = engine
            .execute_script(
                "probe.tjs",
                r#"
                return typeof Window.TouchMouse.onTouchMouseDown(1, 2) + "|" +
                    typeof Window.TouchMouse.onTouchMouseDblClick();
                "#,
            )
            .expect("call the declared events");
        assert_eq!(value, Variant::String("void|void".to_string()));
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
