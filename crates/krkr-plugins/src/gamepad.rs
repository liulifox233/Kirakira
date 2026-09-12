//! gamepad.dll compatibility shim (`docs/plugins/gamepad.md`).
//!
//! The shipped DLL polls DirectInput/XInput and exposes two TJS classes:
//! `Gamepad` (the manager — it is a class object scripts reach directly, so
//! `Gamepad.count` and `Gamepad.getController(0)` both work) and `GamepadPort`
//! (one pad's state, a flat member list recovered from the native instance
//! binder).
//!
//! Kirakira has no pad backend, so the shim registers the exact recovered
//! member surface with no device attached: `Gamepad.count` reports 0 and every
//! port reads empty state — zero counters, triggers, sticks and `keyState`,
//! empty `name`/`type`. The member spellings are part of the contract,
//! including the misspelled `degital*Count` group (the typo is how the
//! reference spells it, so scripts written against it must keep working).
//! `getController` answers with a zeroed port for any index instead of a miss:
//! the dossier's compat requirement is that `Gamepad.getController(0)` is
//! readable even with no pad connected.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Gamepad / GamepadPort (DirectInput/XInput surface)",
    notes: "Surface-compatible with no pad backend: Gamepad.count is 0 and every port reads empty state (0 counters/triggers/sticks/keyState, empty name/type); keyState's bit layout was not recovered by the dossier, so the shim reads 0. leftVibration/rightVibration store the requested value and drive no hardware; update()/initialize()/finalize() are real callable methods.",
    install: |engine| engine.register_plugin(GamepadPlugin),
};

pub struct GamepadPlugin;

impl KrkrPlugin for GamepadPlugin {
    fn name(&self) -> &str {
        "gamepad.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_gamepad(runtime);
        Ok(())
    }
}

/// `Gamepad` members, besides the `count` member.
const MANAGER_METHODS: &[&str] = &["getController", "initialize", "finalize"];

/// `GamepadPort` members that read as an integer zero with no pad connected:
/// `keyState` (its bit layout is unrecovered — see the module's `META`), both
/// triggers, all four sticks, the analog and digital direction counters, and
/// the button counters. The `degital*` spellings are the reference's own.
const PORT_ZERO_MEMBERS: &[&str] = &[
    "keyState",
    "leftTrigger",
    "rightTrigger",
    "leftThumbStickX",
    "leftThumbStickY",
    "rightThumbStickX",
    "rightThumbStickY",
    "analogLeftUpCount",
    "analogLeftDownCount",
    "analogLeftLeftCount",
    "analogLeftRightCount",
    "analogRightUpCount",
    "analogRightDownCount",
    "analogRightLeftCount",
    "analogRightRightCount",
    "degitalUpCount",
    "degitalDownCount",
    "degitalLeftCount",
    "degitalRightCount",
    "buttonStartCount",
    "buttonBackCount",
    "buttonLeftThumbCount",
    "buttonRightThumbCount",
    "buttonLeftShoulderCount",
    "buttonRightShoulderCount",
    "buttonLeftTriggerCount",
    "buttonRightTriggerCount",
    "buttonACount",
    "buttonBCount",
    "buttonXCount",
    "buttonYCount",
];

/// Port identification strings; both are empty with no pad connected (the
/// dossier did not recover the string formats the getters build).
const PORT_STRING_MEMBERS: &[&str] = &["name", "type"];

/// Rumble requests. The reference maps these onto the pad's vibration motors;
/// with no backend the shim keeps the requested value (default 0) and reads it
/// back, so a game that re-reads what it set is not surprised, while no
/// hardware ever moves.
const PORT_VIBRATION_MEMBERS: &[&str] = &["leftVibration", "rightVibration"];

fn install_gamepad(runtime: &mut Runtime<KrkrHost>) {
    // A class object the scripts already installed wins; never shadow it.
    if runtime.global_member("Gamepad").object_handle().is_some()
        || runtime
            .global_member("GamepadPort")
            .object_handle()
            .is_some()
    {
        return;
    }

    let port_class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "GamepadPort");
            install_port_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(port_class, "GamepadPort");
    install_port_members(runtime, port_class);
    runtime.set_global_member("GamepadPort", Variant::Object(port_class));

    let manager_class = runtime.alloc_native_constructor(
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "Gamepad");
            install_manager_members(runtime, instance, port_class);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(manager_class, "Gamepad");
    install_manager_members(runtime, manager_class, port_class);
    runtime.set_global_member("Gamepad", Variant::Object(manager_class));
}

fn install_manager_members(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    port_class: ObjectHandle,
) {
    // No pad backend: the reference reports zero devices and the manager keeps
    // reporting them until a pad is plugged in.
    runtime.set_object_member(handle, "count", Variant::Integer(0));
    for method in MANAGER_METHODS {
        match *method {
            "getController" => {
                runtime.register_object_native_with_arg_count(
                    handle,
                    "getController",
                    NativeArgCount::AtLeast(1),
                    move |runtime: &mut Runtime<KrkrHost>,
                          _this_obj: Option<ObjectHandle>,
                          _args: Vec<Variant>| {
                        Ok(Variant::Object(new_port(runtime, port_class)))
                    },
                );
            }
            // The dossier recovered these only as methods (return types
            // unknown); the shim initializes and finalizes an empty subsystem,
            // so both answer void.
            "initialize" | "finalize" => {
                runtime.register_object_native(handle, *method, native_void);
            }
            _ => unreachable!("no other manager method is declared"),
        }
    }
}

fn install_port_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    for name in PORT_ZERO_MEMBERS {
        runtime.set_object_member(handle, *name, Variant::Integer(0));
    }
    for name in PORT_STRING_MEMBERS {
        runtime.set_object_member(handle, *name, Variant::String(String::new()));
    }
    for name in PORT_VIBRATION_MEMBERS {
        runtime.set_object_member(handle, *name, Variant::Integer(0));
    }
    runtime.register_object_native(handle, "update", port_update);
}

/// One zeroed port, the answer `getController(index)` gives while no pad is
/// connected (the reference's out-of-range answer was not recovered; a
/// readable empty port keeps the dossier's compat expression
/// `Gamepad.getController(0).buttonACount` working).
fn new_port(runtime: &mut Runtime<KrkrHost>, port_class: ObjectHandle) -> ObjectHandle {
    let port = bound_instance(runtime, None, "GamepadPort");
    install_port_members(runtime, port);
    runtime.set_object_super_class(port, port_class);
    port
}

/// `update()` is a real method in the reference — scripts call it every frame
/// to pump device state into the port. The shim pumps the empty device state,
/// so the port's zeroed members stay current and a script that wrote one of
/// them cannot keep a stale value. Rumble requests are device *output* and are
/// left alone.
fn port_update(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) {
        for name in PORT_ZERO_MEMBERS {
            runtime.set_object_member(this, *name, Variant::Integer(0));
        }
        for name in PORT_STRING_MEMBERS {
            runtime.set_object_member(this, *name, Variant::String(String::new()));
        }
    }
    Ok(Variant::Void)
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
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::{TjsErrorKind, runtime::Variant};

    use super::GamepadPlugin;

    /// The dossier's member tables (`docs/plugins/gamepad.md`), spelled out
    /// here independently of the install constants so that a dropped or
    /// renamed member fails this test.
    const MANAGER_MEMBERS: &[&str] = &["count", "getController", "initialize", "finalize"];
    const PORT_MEMBERS: &[&str] = &[
        "name",
        "type",
        "update",
        "keyState",
        "leftTrigger",
        "rightTrigger",
        "leftVibration",
        "rightVibration",
        "leftThumbStickX",
        "leftThumbStickY",
        "rightThumbStickX",
        "rightThumbStickY",
        "analogLeftUpCount",
        "analogLeftDownCount",
        "analogLeftLeftCount",
        "analogLeftRightCount",
        "analogRightUpCount",
        "analogRightDownCount",
        "analogRightLeftCount",
        "analogRightRightCount",
        "degitalUpCount",
        "degitalDownCount",
        "degitalLeftCount",
        "degitalRightCount",
        "buttonStartCount",
        "buttonBackCount",
        "buttonLeftThumbCount",
        "buttonRightThumbCount",
        "buttonLeftShoulderCount",
        "buttonRightShoulderCount",
        "buttonLeftTriggerCount",
        "buttonRightTriggerCount",
        "buttonACount",
        "buttonBCount",
        "buttonXCount",
        "buttonYCount",
    ];

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(GamepadPlugin).expect("plugin");
        engine
    }

    fn run(engine: &mut KrkrEngine, script: &str) -> Variant {
        engine
            .execute_script("probe.tjs", script)
            .expect("probe script")
    }

    #[test]
    fn both_class_objects_carry_the_dossier_member_lists() {
        let engine = engine();
        let runtime = engine.tjs_runtime();

        let manager = runtime
            .global_member("Gamepad")
            .object_handle()
            .expect("Gamepad class object");
        for name in MANAGER_MEMBERS {
            assert!(
                !matches!(runtime.object_member(manager, name), Variant::Void),
                "Gamepad.{name} is missing"
            );
        }

        let port = runtime
            .global_member("GamepadPort")
            .object_handle()
            .expect("GamepadPort class object");
        for name in PORT_MEMBERS {
            assert!(
                !matches!(runtime.object_member(port, name), Variant::Void),
                "GamepadPort.{name} is missing"
            );
        }

        // The spellings are the contract: the corrected names are not members.
        for name in [
            "digitalUpCount",
            "digitalDownCount",
            "digitalLeftCount",
            "digitalRightCount",
        ] {
            assert!(
                matches!(runtime.object_member(port, name), Variant::Void),
                "GamepadPort.{name} exists although the reference misspells it"
            );
        }
    }

    #[test]
    fn a_machine_with_no_pad_reports_count_zero_and_empty_port_state() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var port = Gamepad.getController(0);
            return (Gamepad.count == 0) + "|" + (port.name == "") + "|" + (port.type == "") + "|" +
                (port.keyState == 0) + "|" + (port.leftTrigger == 0) + "|" +
                (port.rightTrigger == 0) + "|" + (port.leftThumbStickX == 0) + "|" +
                (port.rightThumbStickY == 0) + "|" + (port.analogLeftUpCount == 0) + "|" +
                (port.analogRightRightCount == 0) + "|" + (port.degitalUpCount == 0) + "|" +
                (port.degitalRightCount == 0) + "|" + (port.buttonStartCount == 0) + "|" +
                (port.buttonYCount == 0) + "|" + typeof port.buttonACount;
            "#,
        );
        assert_eq!(
            value,
            Variant::String("1|1|1|1|1|1|1|1|1|1|1|1|1|1|Integer".to_string())
        );
    }

    #[test]
    fn the_manager_methods_and_port_update_are_callable() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var port = Gamepad.getController(0);
            var initialized = Gamepad.initialize();
            var updated = port.update();
            var finalized = Gamepad.finalize();
            return typeof initialized + "|" + typeof updated + "|" + typeof finalized;
            "#,
        );
        assert_eq!(value, Variant::String("void|void|void".to_string()));
    }

    #[test]
    fn update_refreshes_device_state_and_leaves_rumble_requests_alone() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var port = Gamepad.getController(0);
            port.leftVibration = 7;
            port.buttonACount = 3;
            port.name = "faked";
            port.update();
            return port.leftVibration + "|" + port.buttonACount + "|" + (port.name == "");
            "#,
        );
        assert_eq!(value, Variant::String("7|0|1".to_string()));
    }

    #[test]
    fn get_controller_requires_an_index() {
        let mut engine = engine();
        let error = engine
            .execute_script("probe.tjs", "Gamepad.getController();")
            .expect_err("a missing index must be rejected");
        assert_eq!(error.kind, TjsErrorKind::BadParamCount);
    }
}
