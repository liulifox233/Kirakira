//! `yuzuex.dll` — the `ProxyStorageMap` global dictionary (proxyfs repackaged).
//!
//! The shipped binary *is* proxyfs: its export table names `proxyfs.dll`
//! (`docs/plugins/yuzuex.md:19`), and its registration function builds exactly
//! two things (`yuzuex.md:29-33`): a TJS dictionary published as the global
//! `ProxyStorageMap` with `TVPRegisterGlobalObject`, and a storage media for
//! the `proxy` domain registered with `TVPRegisterStorageMedia`, whose open
//! failure reads `cannot open proxyfile:%1` (`yuzuex.md:22`). That is also the
//! whole script-visible surface: scripts fill the dictionary, the media
//! consults it, and there are no TJS classes (`yuzuex.md:38-39`).
//!
//! The dictionary is implemented here for real. `ProxyStorageMap` is a TJS
//! `Dictionary` instance published at registration and empty until a script
//! fills it, which is exactly what `TJSCreateDictionaryObject` hands the
//! reference — and an ordinary `Dictionary` instance in this runtime already
//! has the reference's miss-reads-as-void dispatch
//! (`tjsDictionary.cpp:720-731`, `vm/dispatch.rs`), so a script probing an
//! unmapped name sees the same void.
//!
//! The media is *not* implemented, and a plugin cannot implement it from
//! here: a module only ever receives [`krkr_engine::KrkrHost`], whose
//! `project_storage()` is an immutable `&dyn ProjectStoragePort`, while the
//! media registry (`TVPRegisterStorageMedia`'s port,
//! `krkr_assets::ProjectStorage::register_media`, described in
//! `krkr-assets/src/media.rs`) lives on a concrete type this crate's
//! production dependencies do not even name. A mapped path therefore resolves
//! unchanged, and [`YuzuExPlugin::register`] reports that in the engine log
//! instead of pretending the redirection works. This is the dossier's
//! compat-only fallback (`yuzuex.md:56-58`), which the dossier accepts only
//! while nothing depends on the redirection; the mapping's match semantics
//! (exact entry vs prefix replacement, `yuzuex.md:62-63`) are an open question
//! there, so none is applied or claimed here.
//!
//! The three option-descriptor categories this DLL also embeds
//! (`yuzuex.md:34-36`) are deliberately not registered from this module: they
//! duplicate the categories `kagexopt.dll` carries, and the engine merges
//! duplicate categories, so a second copy would have to stay idempotent for no
//! gain (`docs/plugins/kagexopt.md:112`).

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "ProxyStorageMap global dictionary (the proxyfs `proxy` storage media it feeds)",
    notes: "The global dictionary is real: `ProxyStorageMap` is published as a TJS Dictionary instance, empty until a script fills it, with the reference's miss-reads-as-void behaviour. The media behind it is absent — a plugin has no media-registration hook (KrkrHost::project_storage() is an immutable `&dyn ProjectStoragePort`; the registry lives on krkr-assets' concrete ProjectStorage), so a mapped path resolves unchanged and register() says so in the engine log. No mapping semantics are claimed: the dossier leaves exact-vs-prefix open. The option categories this DLL duplicates from kagexopt.dll are left to that plugin rather than registered twice.",
    install: |engine| engine.register_plugin(YuzuExPlugin),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what a game's
/// `Plugins.link("yuzuex.dll")` resolves through [`crate::catalog`]. The
/// catalog also carries the DLL's own export-table name, `proxyfs.dll`, as an
/// alias (`yuzuex.md:19`).
pub(crate) const NAME: &str = "yuzuex.dll";

/// The global the reference's registration publishes with
/// `TVPRegisterGlobalObject` (`yuzuex.md:29-30`).
pub(crate) const GLOBAL_NAME: &str = "ProxyStorageMap";

/// The storage media the reference registers with `TVPRegisterStorageMedia`
/// (`yuzuex.md:32`), the name space proxyfs owns.
pub(crate) const MEDIA_NAME: &str = "proxy";

/// What that media raises when a name it is handed cannot be opened
/// (`yuzuex.md:22`, `:33`). `%1` is the message system's placeholder for that
/// name — the same `cannot open <media>file:%1` form the `steam` media carries
/// (`krkr-assets/src/media.rs:125`). Nothing raises it yet (see the module
/// docs); the engine-side hook that registers this media has to keep the
/// reference's wording.
pub(crate) const OPEN_ERROR: &str = "cannot open proxyfile:%1";

pub struct YuzuExPlugin;

impl KrkrPlugin for YuzuExPlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // `ProxyStorageMap = TJSCreateDictionaryObject()` plus
        // `TVPRegisterGlobalObject` (`yuzuex.md:29-30`). The object stays
        // empty: the reference registers no TJS class and the scripts own
        // every entry.
        let map = runtime.alloc_dictionary_object();
        runtime.set_global_member(GLOBAL_NAME, Variant::Object(map));
        for line in registration_log() {
            runtime.host_mut().log(&line);
        }
        Ok(())
    }
}

/// What `register` reports: the surface that is real, and the one the engine
/// cannot take from a plugin yet.
fn registration_log() -> [String; 2] {
    [
        format!(
            "yuzuex: global TJS Dictionary `{GLOBAL_NAME}` registered (proxyfs; a script fills it \
             with the virtual-to-real storage names the reference's `{MEDIA_NAME}` media resolves \
             through).",
        ),
        format!(
            "yuzuex: the `{MEDIA_NAME}` storage media is NOT registered — a plugin has no \
             media-registration hook (the registry lives on krkr-assets' ProjectStorage, and \
             KrkrHost::project_storage() is read-only); a mapped path resolves unchanged and \
             `{OPEN_ERROR}` cannot be raised here.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use krkr_engine::{EngineConfig, KrkrEngine};

    use super::*;
    use crate::catalog;

    fn engine_with_yuzuex() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(YuzuExPlugin).expect("register");
        engine
    }

    fn global_names(engine: &KrkrEngine) -> BTreeSet<String> {
        engine
            .tjs_runtime()
            .object_members(engine.tjs_runtime().global_handle())
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// The DLL's whole script surface is the one global (`yuzuex.md:38-39`),
    /// and what it publishes is a TJS `Dictionary` instance — the kind
    /// `TJSCreateDictionaryObject` builds (`yuzuex.md:29`) — not an ordinary
    /// object and not a class, so nothing here claims a surface the reference
    /// does not have.
    #[test]
    fn publishes_exactly_one_new_global_and_it_is_a_dictionary() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let before = global_names(&engine);
        engine.register_plugin(YuzuExPlugin).expect("register");

        assert_eq!(
            global_names(&engine)
                .difference(&before)
                .cloned()
                .collect::<Vec<_>>(),
            vec![GLOBAL_NAME.to_string()],
            "the plugin installs more than the one global the dossier recovers",
        );
        let map = engine
            .tjs_runtime()
            .object_member(engine.tjs_runtime().global_handle(), GLOBAL_NAME);
        let handle = map
            .object_handle()
            .unwrap_or_else(|| panic!("`{GLOBAL_NAME}` is not an object: {map:?}"));
        assert!(
            engine.tjs_runtime().is_dictionary_instance(handle),
            "`{GLOBAL_NAME}` is not a Dictionary instance",
        );
    }

    /// Scripts own the dictionary's entries, and an entry nobody wrote reads
    /// as void rather than throwing — the miss dispatch of a
    /// `tTJSDictionaryObject` (`tjsDictionary.cpp:720-731`). The map also has
    /// to survive into the next script: a game maps its paths once at startup
    /// and reads them from every scenario afterwards, and the media
    /// (`yuzuex.md:32-33`) would consult this same object.
    #[test]
    fn scripts_fill_the_map_and_the_engine_keeps_it() {
        let mut engine = engine_with_yuzuex();
        engine
            .execute_script("startup.tjs", r#"ProxyStorageMap["data/"] = "savedata/";"#)
            .expect("script");
        let value = engine
            .execute_script(
                "scenario.tjs",
                r#"
                return (ProxyStorageMap["data/"] === "savedata/" ? "mapped" : "wrong") + "/" +
                    (ProxyStorageMap["other/"] === void ? "void" : "value");
                "#,
            )
            .expect("script");
        assert_eq!(value.to_tjs_string().expect("string"), "mapped/void");
    }

    /// The plugin registers under the catalog's canonical name, and its log
    /// separates what is real (the dictionary, a `Dictionary` object) from
    /// what is not (the `proxy` media, with the reference's own open-failure
    /// text quoted so the missing hook is unambiguous).
    #[test]
    fn registers_under_the_catalog_name_and_reports_the_media_it_cannot_register() {
        let engine = engine_with_yuzuex();

        assert_eq!(catalog::canonical_name(NAME), Some(NAME));
        assert_eq!(catalog::canonical_name("proxyfs.dll"), Some(NAME));
        assert!(
            engine.host().linked_plugins().any(|linked| linked == NAME),
            "the plugin does not register the name the catalog resolves",
        );

        let logs = engine.host().logs();
        assert!(
            logs.iter()
                .any(|line| line.contains(GLOBAL_NAME) && line.contains("Dictionary")),
            "no line describing the registered global: {logs:?}",
        );
        assert!(
            logs.iter().any(|line| line.contains(MEDIA_NAME)
                && line.contains(OPEN_ERROR)
                && line.contains("NOT registered")),
            "no line reporting the unregistered media: {logs:?}",
        );
    }
}
