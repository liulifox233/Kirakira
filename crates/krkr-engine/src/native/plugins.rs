use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{install_static_object, native_void, required_arg_string};

pub(crate) fn install_plugins(runtime: &mut Runtime<KrkrHost>) {
    let plugins = install_static_object(runtime, "Plugins");
    // `tTJSNC_Plugins` declares an empty `finalize` with
    // `TJS_DECL_EMPTY_FINALIZE_METHOD` (`PluginIntf.cpp:27`); scripts reach it
    // as `Plugins.finalize(...)` while tearing a session down.
    runtime.register_object_native(plugins, "finalize", native_void);
    // `link`/`unlink` declare `if(numparams < 1) return TJS_E_BADPARAMCOUNT;`
    // in `TVPCreateNativeClass_Plugins` (`base/win32/PluginImpl.cpp:957`,
    // `:970`), so the floor sits at the registration site.
    runtime.register_object_native_with_arg_count(
        plugins,
        "link",
        NativeArgCount::AtLeast(1),
        plugins_link,
    );
    runtime.register_object_native_with_arg_count(
        plugins,
        "unlink",
        NativeArgCount::AtLeast(1),
        plugins_unlink,
    );
    runtime.register_object_native(plugins, "getList", plugins_get_list);
    // KAG3/KAGEX games guard every optional plugin with the *global*
    // `CanLoadPlugin`; a game that carries its own definition replaces this
    // one at boot, so the engine answer only reaches projects whose scripts
    // never got one.
    runtime.register_global_native("CanLoadPlugin", can_load_plugin);
}

fn plugins_link(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Plugins.link")?;
    // KRKR only publishes a plugin's classes when its module is loaded here,
    // and `TVPLoadPlugin` returns early once the same module is loaded. Mirror
    // that: install on the first explicit link so a boot script that shadowed
    // a class name (a kirikiroid2 `patch.tjs` does) does not keep the stub.
    if let Some(plugin) = runtime.host_mut().plugin_to_install(&name) {
        plugin.register(runtime)?;
    }
    runtime.host_mut().link_plugin(&name);
    Ok(Variant::Void)
}

fn plugins_unlink(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Plugins.unlink")?;
    // The reference unregisters a plugin's media as its module goes away
    // (`V2Unlink`, `varfile/Main.cpp:451-477`), so the plugin's `unregister`
    // runs before its name leaves the registry.
    if let Some(plugin) = runtime.host().plugin_to_unregister(&name) {
        plugin.unregister(runtime)?;
    }
    Ok(Variant::Integer(i64::from(
        runtime.host_mut().unlink_plugin(&name),
    )))
}

fn plugins_get_list(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let values = runtime
        .host()
        .linked_plugins()
        .map(|name| Variant::String(name.to_string()))
        .collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

/// `CanLoadPlugin(name)` — the KAG plugin probe.
///
/// The reference ships it as a *script* function with the KAG templates
/// (`kirikiri2/branches/kag3ex3/template/system/Initialize.tjs:237-242`, the
/// kag3ex2 copy at `:236-241`), and the games built on those templates carry
/// their own copy — PARQUET's `data.xp3 > system/Initialize.tjs` decompiles to
/// the same body. krkrZ has no occurrence of the name at all, so a project
/// that calls it must either define it or find the engine's definition; the
/// engine provides this one so a KRKRZ-era boot does not die on
/// `MemberNotFound` where a KAGEX game merely checks an optional module.
///
/// The reference body is a pure file probe:
///
/// ```text
/// var exepath = System.exePath, exist = Storages.isExistentStorage;
/// if (exist(exepath+name) || exist(exepath+"plugin/"+name) || exist(exepath+"system/"+name)) return true;
/// var placed = Storages.getPlacedPath(name);
/// return placed != "" && exist(placed);
/// ```
///
/// It never consults the loaded-plugin list, and it appends no extension:
/// `CanLoadPlugin("motionplayer")` probes a file named `motionplayer`, not
/// `motionplayer.dll`. A module whose file is present but cannot load still
/// answers true — the load failure belongs to `Plugins.link` (the reference's
/// `TVPLoadPlugin` throws there and the game catches it), not to the probe.
///
/// This engine ships its modules as Rust registrations instead of DLL files,
/// so a name the host registered (case-insensitively, the way `Plugins.link`
/// matches it) answers true as well; the storage probe below asks the
/// reference question for everything else. The catalog's alias spellings are
/// covered only where the game ships a file under that spelling — the engine
/// registry knows a module's canonical name, not the catalog's alias table.
fn can_load_plugin(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // The reference concatenates the argument into a path (`exepath + name`),
    // so a number probes its decimal spelling. A void/null argument names no
    // file at all.
    let name = args
        .first()
        .filter(|value| !matches!(value, Variant::Void | Variant::Null))
        .and_then(|value| value.to_tjs_string().ok())
        .unwrap_or_default();
    if name.is_empty() {
        return Ok(Variant::Integer(0));
    }
    let can_load = plugin_registered(runtime, &name) || plugin_file_present(runtime, &name);
    Ok(Variant::Integer(i64::from(can_load)))
}

/// True when the host registered a module under this spelling — the engine's
/// counterpart of the reference's "the DLL file is there".
///
/// `plugin_to_unregister` is the registry's side-effect-free, case-insensitive
/// lookup; `plugin_to_install` cannot be used here because it marks the name
/// as script-linked, which would make a later `Plugins.link` skip the install.
fn plugin_registered(runtime: &Runtime<KrkrHost>, name: &str) -> bool {
    runtime.host().plugin_to_unregister(name).is_some()
}

/// The reference's storage probe: the exact name beside the executable, under
/// `plugin/`, under `system/`, or wherever storage search places it.
fn plugin_file_present(runtime: &mut Runtime<KrkrHost>, name: &str) -> bool {
    // `Storages.isExistentStorage` refreshes the script-fed storage tables
    // first, so the probe sees what a game's own `CanLoadPlugin` would see.
    crate::plugin_api::storage::refresh_storage_tables(runtime);
    let exe_path = runtime.host().system_paths().exe_path.clone();
    for candidate in [
        format!("{exe_path}{name}"),
        format!("{exe_path}plugin/{name}"),
        format!("{exe_path}system/{name}"),
    ] {
        if runtime.host().storage_exists_exact(&candidate) {
            return true;
        }
    }
    match runtime.host().placed_storage_name(name) {
        Some(placed) => runtime.host().storage_exists_exact(&placed),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineConfig, KrkrEngine, KrkrPlugin, SystemPaths};
    use krkr_assets::ProjectStorage;
    use krkr_core::ProjectStoragePort;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A module this build "ships": registered with the host the way a real
    /// plugin is, so the registry probe has a name to answer for.
    struct ShippedModule;

    impl KrkrPlugin for ShippedModule {
        fn name(&self) -> &str {
            "shipped.dll"
        }

        fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
            Ok(())
        }
    }

    fn temp_root(prefix: &str) -> PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "Kirakira-native-plugins-{prefix}-{}-{nanos}-{id}",
            std::process::id()
        ))
    }

    /// `KrkrEngine::for_project` is private to `engine`; this is the same
    /// construction (project storage plus a trailing-slash `System.exePath`)
    /// for a module that only sits next to it.
    fn project_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("project storage");
        KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage) as Arc<dyn ProjectStoragePort>),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine")
    }

    fn probe(engine: &mut KrkrEngine, expression: &str) -> i64 {
        match engine
            .execute_expression("can-load-plugin-probe.tjs", expression)
            .expect("expression")
        {
            Variant::Integer(value) => value,
            other => panic!("{expression} answered {other:?}"),
        }
    }

    /// A plugin this build ships answers true through the TJS global under
    /// every spelling of its registered name — `Plugins.link` matches names
    /// case-insensitively, and the probe has to agree with it.
    #[test]
    fn a_shipped_module_answers_true_for_every_spelling_of_its_name() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ShippedModule).expect("register");

        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"shipped.dll\")"), 1);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"Shipped.DLL\")"), 1);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"SHIPPED.dll\")"), 1);
    }

    /// The reference probe is a file question, so a module with no Rust
    /// implementation behind it still answers true when the game ships the
    /// file — the module's absence surfaces from `Plugins.link`, not here.
    /// All three reference locations are covered.
    #[test]
    fn a_file_that_has_no_registered_module_answers_true() {
        let root = temp_root("files");
        fs::create_dir_all(root.join("plugin")).expect("plugin dir");
        fs::create_dir_all(root.join("system")).expect("system dir");
        fs::write(root.join("toplevel.dll"), b"").expect("toplevel file");
        fs::write(root.join("plugin/handmade.dll"), b"").expect("plugin file");
        fs::write(root.join("system/kag-only.dll"), b"").expect("system file");

        let mut engine = project_engine(&root);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"toplevel.dll\")"), 1);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"handmade.dll\")"), 1);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"kag-only.dll\")"), 1);

        fs::remove_dir_all(&root).expect("cleanup");
    }

    /// `Storages.getPlacedPath` is the reference's fourth question: a file a
    /// mounted auto path places under the requested name answers true.
    #[test]
    fn a_file_found_through_a_placed_path_answers_true() {
        let root = temp_root("placed");
        fs::create_dir_all(root.join("extra")).expect("extra dir");
        fs::write(root.join("extra/placed.dll"), b"").expect("placed file");

        let mut engine = project_engine(&root);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"placed.dll\")"), 0);
        engine
            .execute_script(
                "can-load-plugin-probe.tjs",
                &format!("Storages.addAutoPath(\"{}/extra/\");", root.display()),
            )
            .expect("auto path");
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"placed.dll\")"), 1);

        fs::remove_dir_all(&root).expect("cleanup");
    }

    /// The reference appends no extension: `CanLoadPlugin("one")` probes a
    /// file named `one`, so a `one.dll` on disk is not found under the stem.
    #[test]
    fn the_probe_never_appends_an_extension() {
        let root = temp_root("extensionless");
        fs::create_dir_all(root.join("plugin")).expect("plugin dir");
        fs::write(root.join("plugin/one.dll"), b"").expect("plugin file");

        let mut engine = project_engine(&root);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"one.dll\")"), 1);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"one\")"), 0);

        fs::remove_dir_all(&root).expect("cleanup");
    }

    /// Nonsense input answers false: an unknown name, an empty name, a
    /// missing argument and a name that no file matches.
    #[test]
    fn nonsense_input_answers_false() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"nonsense.dll\")"), 0);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"\")"), 0);
        assert_eq!(probe(&mut engine, "CanLoadPlugin()"), 0);
        assert_eq!(probe(&mut engine, "CanLoadPlugin(12345)"), 0);
    }

    /// The KAG templates define `CanLoadPlugin` themselves
    /// (`Initialize.tjs:237`), and a project's definition has to keep winning:
    /// the engine global is a fallback, never an override.  PARQUET is the
    /// proof — it carries the script body and its answer is the one in force.
    #[test]
    fn a_project_definition_replaces_the_engine_probe() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "project-initialize.tjs",
                "function CanLoadPlugin(name) { return 7; }",
            )
            .expect("project definition");

        assert_eq!(probe(&mut engine, "CanLoadPlugin(\"motionplayer.dll\")"), 7);
    }

    /// M175.  `Plugins.link`/`unlink` declare
    /// `if(numparams < 1) return TJS_E_BADPARAMCOUNT;`
    /// (`base/win32/PluginImpl.cpp:957`, `:970`), so a short call reports
    /// `TJS_E_BADPARAMCOUNT` (-1004) before the handler runs, while the
    /// reference arity keeps working for a module this build actually ships.
    #[test]
    fn plugins_method_floors_reject_short_calls() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ShippedModule).expect("register");
        let value = engine
            .execute_script(
                "plugins_floors.tjs",
                r#"
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var rejected = [
                    message(function() { Plugins.link(); }),
                    message(function() { Plugins.unlink(); })
                ].join("|");
                function check(body) {
                    try { body(); } catch (e) {
                        if (e.message === "Invalid argument count") { return "bad"; }
                    }
                    return "ok";
                }
                return rejected + "@"
                    + check(function() { Plugins.link("shipped.dll"); }) + ":"
                    + check(function() { Plugins.unlink("shipped.dll"); });
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("Invalid argument count|Invalid argument count@ok:ok".to_string())
        );
        // The identity from Rust: the dispatch check answers
        // `TJS_E_BADPARAMCOUNT` (-1004) before the handler.
        let error = engine
            .execute_expression("plugins_floors.tjs", "Plugins.link()")
            .expect_err("a short link call must fail");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        assert_eq!(error.tjs_error_code(), Some(-1004));
        assert_eq!(error.message, "Invalid argument count");
    }
}
