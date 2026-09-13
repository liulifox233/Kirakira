//! Catalog-level checks that need a real engine: registering the whole
//! catalog has to leave the engine runnable, name resolution has to hold for
//! every external spelling, and a plugin with no implementation has to report
//! itself instead of quietly disappearing.

use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use krkr_engine::{EngineConfig, KrkrEngine, KrkrHost, KrkrPlugin};
use krkr_plugins::{
    CATALOG, GIST_PLUGIN_NAMES, GameProfile, PARQUET_PLUGIN_FILES, canonical_name, missing_plugins,
    register_profile_plugins, register_reference_plugins, resolve,
};
use krkr_tjs2::{Result, runtime::Runtime};

fn test_engine() -> KrkrEngine {
    KrkrEngine::new(EngineConfig::default()).expect("engine")
}

fn registered_plugins(engine: &KrkrEngine) -> BTreeSet<String> {
    engine.host().linked_plugins().map(str::to_string).collect()
}

fn placeholder_reports(engine: &KrkrEngine, name: &str) -> usize {
    let needle = format!("not implemented: {name}");
    engine
        .host()
        .logs()
        .iter()
        .filter(|line| line.contains(&needle))
        .count()
}

#[test]
fn every_gist_and_parquet_name_resolves_to_a_catalog_entry() {
    for name in GIST_PLUGIN_NAMES.iter().chain(PARQUET_PLUGIN_FILES) {
        assert!(
            resolve(name).is_some(),
            "{name} has no catalog entry or alias"
        );
    }
}

#[test]
fn registering_the_full_catalog_installs_every_entry_and_leaves_the_engine_running() {
    let mut engine = test_engine();
    register_reference_plugins(&mut engine).expect("register reference plugins");

    let expected: BTreeSet<String> = CATALOG.iter().map(|entry| entry.name.to_string()).collect();
    assert_eq!(
        registered_plugins(&engine),
        expected,
        "registered plugin names differ from the catalog"
    );
    assert_eq!(engine.plugin_count(), CATALOG.len());

    for entry in missing_plugins() {
        assert!(
            placeholder_reports(&engine, entry.name) > 0,
            "placeholder {} did not report itself",
            entry.name
        );
    }

    // A registered plugin still answers, so the extra entries did not break
    // the modules that existed before the catalog.
    let value = engine
        .execute_expression(
            "smoke.tjs",
            r#"(function() { return "answer=" + Scripts.evalJSON('{"answer": 2}').answer; })()"#,
        )
        .expect("script");
    assert_eq!(value.to_tjs_string().expect("string"), "answer=2");
}

#[test]
fn the_parquet_profile_installs_exactly_the_plugins_parquet_ships() {
    let mut engine = test_engine();
    register_profile_plugins(&mut engine, &GameProfile::parquet())
        .expect("register parquet profile");

    let expected: BTreeSet<String> = PARQUET_PLUGIN_FILES
        .iter()
        .map(|name| canonical_name(name).expect("catalog entry").to_string())
        .collect();
    assert_eq!(registered_plugins(&engine), expected);
    assert!(engine.plugin_count() < CATALOG.len());
}

#[test]
fn a_profile_built_from_aliases_installs_the_canonical_plugins() {
    let mut engine = test_engine();
    register_profile_plugins(
        &mut engine,
        &GameProfile::only(["TextRender.dll", "motionplayer_nod3d.dll", "saveStruct.dll"]),
    )
    .expect("register alias profile");

    assert_eq!(
        registered_plugins(&engine),
        BTreeSet::from([
            "motionplayer.dll".to_string(),
            "savestruct.dll".to_string(),
            "textrender.dll".to_string(),
        ])
    );
}

#[test]
fn a_case_variant_link_reaches_the_registered_plugin() {
    // `Plugins.link` resolves its argument among the registered plugin names
    // case-insensitively and runs `register` on the module again on the first
    // explicit link (`crates/krkr-engine/src/native/plugins.rs`) — the
    // behaviour that cures a class a `patch.tjs` shadowed at boot.
    //
    // The evidence has to be that registration, and no catalog module can
    // supply it: the modules disagree on what a repeated `register` leaves
    // behind. A `Missing` entry such as xp3filter.dll reports itself once per
    // instance by contract (`crates/krkr-plugins/src/xp3_filter.rs` pins it),
    // so a case-variant link adds no log line, and an implemented module
    // re-installs its surface idempotently. Count the `register` calls a
    // probe receives instead, through a plugin registered by the same
    // `KrkrEngine::register_plugin` call the catalog's install functions make.
    struct CountingPlugin {
        calls: Arc<AtomicUsize>,
    }

    impl KrkrPlugin for CountingPlugin {
        fn name(&self) -> &str {
            "CountingProbe.dll"
        }

        fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let mut engine = test_engine();
    engine
        .register_plugin(CountingPlugin {
            calls: Arc::clone(&calls),
        })
        .expect("register the probe");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "boot registration must run"
    );

    engine
        .execute_expression("link.tjs", r#"Plugins.link("COUNTINGPROBE.DLL")"#)
        .expect("link");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "Plugins.link did not reach the registered plugin"
    );

    // A spelling no registered plugin answers re-installs nothing, so the
    // second call above is the case-variant resolution's own doing.
    engine
        .execute_expression("link.tjs", r#"Plugins.link("Nowhere.dll")"#)
        .expect("link an unknown name");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "an unknown name reached a registered plugin"
    );
}
