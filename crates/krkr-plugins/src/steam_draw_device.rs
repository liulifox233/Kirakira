//! SteamDrawDevice.dll compatibility shim (`docs/plugins/SteamDrawDevice.md`).
//!
//! The DLL is wamsoft's "DualDrawDevice": one Direct3D9 draw-device
//! implementation registered under two class names — `DualDrawDevice_2` (the
//! KRKR2 "K2" interface flavour) and `DualDrawDevice_Z` (the KRKRZ "KZ"
//! flavour) — through SimpleBinder, so each class carries `interface`,
//! `recreate` and SimpleBinder's own `finalize` slot. Scripts instantiate one
//! and hand it to the engine: `Window.drawDevice = new DualDrawDevice_Z();`.
//!
//! Kirakira renders through wgpu and has no script-selectable draw device, so
//! the classes are a compat surface: constructing them works, `interface`
//! reports the object's engine identity, and `recreate`/`finalize` are
//! callable no-ops. The reference's `interface` is a native property whose
//! getter hands out the raw `iTVPDrawDevice*` as an integer
//! (`reinterpret_cast<tjs_int64>`, `drawdevice/Main.cpp:127-135`); Kirakira
//! has no such object, so the shim answers the object handle — the same
//! stand-in `Window.layerTreeOwnerInterface` uses for a raw interface handle —
//! and the engine never consults it. `recreate` re-creates the device's back
//! buffer after a display-mode change; the wgpu surface reconfigures itself,
//! so the call has nothing to do. The DLL's `-smoothzoom` command-line switch
//! has no counterpart and no effect: our scaling is always filtered, which is
//! what the switch turns on in the reference.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "DualDrawDevice_2 / DualDrawDevice_Z (Direct3D9 draw-device classes)",
    notes: "Compat surface: both class names construct and carry interface/recreate/finalize. \
            interface is the object's engine identity (its object handle, the Window.\
            layerTreeOwnerInterface convention), not a real iTVPDrawDevice*; recreate (back-buffer \
            recreation after a display-mode change) and finalize are callable no-ops because wgpu \
            owns presentation. Window.drawDevice = new DualDrawDevice_Z() assigns the member and \
            the engine keeps rendering through wgpu, which is the reference's observable result \
            (pixels on screen). The DLL's -smoothzoom switch has no counterpart; our scaling is \
            always filtered, which is what the switch turns on.",
    install: |engine| engine.register_plugin(SteamDrawDevicePlugin),
};

pub struct SteamDrawDevicePlugin;

impl KrkrPlugin for SteamDrawDevicePlugin {
    fn name(&self) -> &str {
        "SteamDrawDevice.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_steam_draw_device(runtime);
        Ok(())
    }
}

/// The two class names the DLL exports (the dossier's table); scripts pick one
/// by their engine's interface generation, both carry the same members.
const CLASS_NAMES: &[&str] = &["DualDrawDevice_2", "DualDrawDevice_Z"];

fn install_steam_draw_device(runtime: &mut Runtime<KrkrHost>) {
    for class_name in CLASS_NAMES {
        // A class the scripts already installed wins; never shadow it.
        if runtime.global_member(class_name).object_handle().is_some() {
            continue;
        }
        let name = *class_name;
        let class = runtime.alloc_native_constructor(
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  _args: Vec<Variant>| {
                let instance = bound_instance(runtime, this_obj, name);
                install_draw_device_members(runtime, instance);
                Ok(Variant::Object(instance))
            },
        );
        runtime.add_object_class_info(class, name);
        install_draw_device_members(runtime, class);
        runtime.set_global_member(name, Variant::Object(class));
    }
}

fn install_draw_device_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // The reference's getter hands out the raw device pointer and its setter
    // is denied (`TJS_DENY_NATIVE_PROP_SETTER`, `drawdevice/Main.cpp:127-135`);
    // a script write would plant a value nothing reads, so the shim keeps the
    // reference's error shape.
    runtime.register_object_native_property_with_access(
        handle,
        "interface",
        NativePropertyAccess::ReadOnly,
        interface_identity,
        |_runtime: &mut Runtime<KrkrHost>, _this_obj: Option<ObjectHandle>, _value: Variant| Ok(()),
    );
    runtime.register_object_native(handle, "recreate", native_void);
    runtime.register_object_native(handle, "finalize", native_void);
}

/// `interface` — the object's engine identity. The reference's getter returns
/// the raw `iTVPDrawDevice*` as an integer; this engine has no device behind
/// the class, so the handle is the stable, comparable value a script gets
/// instead (and one the engine never reads).
fn interface_identity(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) else {
        return Ok(Variant::Void);
    };
    Ok(Variant::Integer(this.0 as i64))
}

/// `recreate()`/`finalize()` — the reference rebuilds its back buffer on the
/// first and releases its device state on the second; wgpu owns both here.
fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
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

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::{TjsErrorKind, runtime::Variant};

    use super::SteamDrawDevicePlugin;

    /// The dossier's member table (`docs/plugins/SteamDrawDevice.md`), spelled
    /// out here independently of the install code so that a dropped or
    /// renamed member fails this test.
    const MEMBERS: &[&str] = &["interface", "recreate", "finalize"];

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(SteamDrawDevicePlugin)
            .expect("plugin");
        engine
    }

    fn run(engine: &mut KrkrEngine, script: &str) -> Variant {
        engine
            .execute_script("probe.tjs", script)
            .expect("probe script")
    }

    #[test]
    fn both_class_names_carry_the_dossier_members() {
        let engine = engine();
        let runtime = engine.tjs_runtime();
        for class_name in ["DualDrawDevice_2", "DualDrawDevice_Z"] {
            let class = runtime
                .global_member(class_name)
                .object_handle()
                .unwrap_or_else(|| panic!("{class_name} is not registered"));
            for name in MEMBERS {
                assert!(
                    !matches!(runtime.object_member(class, name), Variant::Void),
                    "{class_name}.{name} is missing"
                );
            }
        }
    }

    /// The dossier's usage (`Window.drawDevice = new DualDrawDevice_Z();`)
    /// runs, and the object it leaves behind reads as a draw device.
    #[test]
    fn the_reference_usage_constructs_and_assigns_to_window_draw_device() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var window = new Window();
            var device = new DualDrawDevice_Z();
            window.drawDevice = device;
            return (typeof window.drawDevice == "Object") + "|" +
                (window.drawDevice.interface == device.interface) + "|" +
                typeof device.interface + "|" + typeof device.recreate() + "|" +
                typeof device.finalize();
            "#,
        );
        assert_eq!(value, Variant::String("1|1|Integer|void|void".to_string()));
    }

    /// `interface` stands in for a raw device pointer, so it is a nonzero,
    /// stable, per-object value.
    #[test]
    fn each_instance_reports_a_stable_nonzero_identity() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var first = new DualDrawDevice_2();
            var second = new DualDrawDevice_2();
            return (first.interface != 0) + "|" + (first.interface == first.interface) + "|" +
                (first.interface != second.interface) + "|" +
                (typeof (new DualDrawDevice_Z()).interface == "Integer");
            "#,
        );
        assert_eq!(value, Variant::String("1|1|1|1".to_string()));
    }

    #[test]
    fn interface_denies_script_writes() {
        let mut engine = engine();
        let error = engine
            .execute_script(
                "probe.tjs",
                "var device = new DualDrawDevice_Z(); device.interface = 1;",
            )
            .expect_err("a read-only property denies the write");
        assert_eq!(error.kind, TjsErrorKind::AccessDenied, "{}", error.message);
    }

    /// `recreate` and `finalize` are the reference's only other members and
    /// both are callable whatever the argument list — the shim has nothing to
    /// pass them to.
    #[test]
    fn recreate_and_finalize_tolerate_any_argument_list() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var device = new DualDrawDevice_Z();
            return typeof device.recreate(0, "x") + "|" + typeof device.finalize(1);
            "#,
        );
        assert_eq!(value, Variant::String("void|void".to_string()));
    }
}
