//! Catalog-level checks that need a real engine: registering the whole
//! catalog has to leave the engine runnable, name resolution has to hold for
//! every external spelling, and a plugin with no implementation has to report
//! itself instead of quietly disappearing.

use std::collections::BTreeSet;

use krkr_engine::{EngineConfig, KrkrEngine};
use krkr_plugins::{
    CATALOG, GIST_PLUGIN_NAMES, GameProfile, PARQUET_PLUGIN_FILES, canonical_name, missing_plugins,
    register_profile_plugins, register_reference_plugins, resolve,
};

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
    // A link that reached the registered plugin re-installs it, and the only
    // module-side evidence a link can leave is the "not implemented" report a
    // placeholder emits — so the subject has to be an entry that is still
    // missing. Take whichever one that is instead of pinning a name a later
    // mission implements: k2compat held this spot until M44 implemented it, and
    // windowEx, csvParser, scriptsEx, saveStruct, fstat, wuvorbis and wuopus
    // are being implemented in parallel missions right now.
    let Some(entry) = missing_plugins().next() else {
        // Every catalog entry is implemented, so no module reports itself and
        // this check has no evidence left to read. Name resolution is still
        // covered by the catalog tests above and `crates/krkr-plugins/src`.
        return;
    };

    let mut engine = test_engine();
    register_profile_plugins(&mut engine, &GameProfile::only([entry.name])).expect("register");
    let before = placeholder_reports(&engine, entry.name);

    engine
        .execute_expression(
            "link.tjs",
            &format!(r#"Plugins.link("{}")"#, entry.name.to_ascii_uppercase()),
        )
        .expect("link");

    assert_eq!(
        placeholder_reports(&engine, entry.name),
        before + 1,
        "Plugins.link did not reach the registered plugin"
    );
}
