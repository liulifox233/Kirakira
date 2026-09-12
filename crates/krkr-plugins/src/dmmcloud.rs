//! dmmcloud.dll compatibility shim (`docs/plugins/dmmcloud.md`).
//!
//! The shipped DLL is a thin message bridge into the DMM GAME PLAYER browser
//! shell: every `DMMCloud` method builds a URL or command and hands it to
//! `libUbiCustomEvent.dll` (`FUbiSendCustomEvent`), the launcher event bridge
//! that ships with the store build. Off a DMM build that backend does not
//! exist, and the original DLL cannot even load without the import — so this
//! shim registers the recovered `DMMCloud` class without the dependency and
//! answers `false` from every IPC method, the honest result for a launcher
//! that is not there. Nothing is stored, no window, tab or store page opens,
//! and the game's own fallback path is what runs.
//!
//! The recovered signatures (`bool(const wchar_t*, int)`, `bool(int,int)`,
//! `bool(int)`, `bool()`) fix each method's minimum argument count: a call
//! with fewer arguments fails with `TJS_E_BADPARAMCOUNT` before the handler,
//! the way the ncbind declarations reject it. Extra arguments are tolerated.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "DMMCloud (DMM GAME PLAYER launcher bridge)",
    notes: "Compat surface off a DMM build: the five IPC methods are registered with the recovered argument counts and return false (no launcher bridge here, so nothing opens or is stored), dcgp_game holds the product-id string and finalize is a no-op. Calls with fewer arguments than a recovered signature requires fail with TJS_E_BADPARAMCOUNT; extra arguments are tolerated. Unlike the original, this module has no libUbiCustomEvent.dll dependency.",
    install: |engine| engine.register_plugin(DmmCloudPlugin),
};

pub struct DmmCloudPlugin;

impl KrkrPlugin for DmmCloudPlugin {
    fn name(&self) -> &str {
        "dmmcloud.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_dmm_cloud(runtime);
        Ok(())
    }
}

/// The IPC methods with the minimum argument count their recovered ncbind
/// signature requires (`docs/plugins/dmmcloud.md`).
const METHODS: &[(&str, usize)] = &[
    ("open_store", 1),
    ("launch_ime", 1),
    ("close_ime", 0),
    ("new_window", 1),
    ("new_tab", 1),
];

/// The launcher's product identifier, recovered from the DLL as the literal
/// `dcgp_game` (DMM GAME PLAYER).
const DCGP_GAME: &str = "dcgp_game";

fn install_dmm_cloud(runtime: &mut Runtime<KrkrHost>) {
    // A class the scripts already installed wins; never shadow it.
    if runtime.global_member("DMMCloud").object_handle().is_some() {
        return;
    }
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "DMMCloud");
            install_dmm_cloud_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "DMMCloud");
    install_dmm_cloud_members(runtime, class);
    runtime.set_global_member("DMMCloud", Variant::Object(class));
}

fn install_dmm_cloud_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    for (name, min_args) in METHODS {
        runtime.register_object_native_with_arg_count(
            handle,
            *name,
            NativeArgCount::AtLeast(*min_args),
            launcher_unavailable,
        );
    }
    runtime.register_object_native(handle, "finalize", native_void);
    runtime.set_object_member(handle, "dcgp_game", Variant::String(DCGP_GAME.to_string()));
}

/// Every IPC method's answer with no launcher bridge: `false`, the result the
/// ncbind signatures declare (`bool ...`). No argument is read — there is
/// nothing to send it to.
fn launcher_unavailable(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
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

    use super::DmmCloudPlugin;

    /// The dossier's member table (`docs/plugins/dmmcloud.md`), spelled out
    /// independently of the install constants so that a dropped member fails.
    const MEMBERS: &[&str] = &[
        "open_store",
        "launch_ime",
        "close_ime",
        "new_window",
        "new_tab",
        "dcgp_game",
        "finalize",
    ];

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(DmmCloudPlugin).expect("plugin");
        engine
    }

    fn run(engine: &mut KrkrEngine, script: &str) -> Variant {
        engine
            .execute_script("probe.tjs", script)
            .expect("probe script")
    }

    #[test]
    fn the_class_and_its_instances_carry_the_dossier_members() {
        let engine = engine();
        let runtime = engine.tjs_runtime();
        let class = runtime
            .global_member("DMMCloud")
            .object_handle()
            .expect("DMMCloud class object");
        for name in MEMBERS {
            assert!(
                !matches!(runtime.object_member(class, name), Variant::Void),
                "DMMCloud.{name} is missing"
            );
        }
    }

    #[test]
    fn every_ipc_method_answers_false_without_the_launcher() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            return DMMCloud.open_store("dcgp_game") + "|" + DMMCloud.launch_ime(0) + "|" +
                DMMCloud.close_ime() + "|" + DMMCloud.new_window(1) + "|" +
                DMMCloud.new_tab(1) + "|" + DMMCloud.new_window(1, 2) + "|" +
                DMMCloud.launch_ime(0, 1) + "|" + DMMCloud.dcgp_game;
            "#,
        );
        assert_eq!(
            value,
            Variant::String("0|0|0|0|0|0|0|dcgp_game".to_string())
        );
    }

    #[test]
    fn the_methods_also_work_on_instances_and_finalize_is_a_no_op() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var cloud = new DMMCloud();
            var opened = cloud.open_store("dcgp_game");
            var finalized = cloud.finalize();
            return opened + "|" + typeof finalized;
            "#,
        );
        assert_eq!(value, Variant::String("0|void".to_string()));
    }

    #[test]
    fn calls_missing_a_signature_argument_fail_with_badparamcount() {
        for call in [
            "DMMCloud.open_store()",
            "DMMCloud.launch_ime()",
            "DMMCloud.new_window()",
            "DMMCloud.new_tab()",
        ] {
            let mut engine = engine();
            let error = engine
                .execute_script("probe.tjs", &format!("return {call};"))
                .expect_err("a missing argument must be rejected");
            assert_eq!(error.kind, TjsErrorKind::BadParamCount, "{call}");
        }
    }

    /// The recovered signatures are minimum counts: extra arguments are
    /// tolerated (the shim has nothing to send them to) and `close_ime` takes
    /// none at all.
    #[test]
    fn extra_arguments_are_tolerated() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            return DMMCloud.open_store("dcgp_game", 1, 2) + "|" + DMMCloud.close_ime(1);
            "#,
        );
        assert_eq!(value, Variant::String("0|0".to_string()));
    }
}
