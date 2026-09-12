//! KAGParserEx.dll: what the DLL adds, and where each delta lives.
//!
//! The DLL (`krkrz/src/plugins/win32/kagparserex/`) is a copy of the core KAG
//! parser plus three extensions; `V2Link` swaps the engine's global
//! `KAGParser` class object for the plugin's, `V2Unlink` puts the original
//! back (`Main.cpp:19-62`).
//!
//! 1. **`taglist`** — every tag dictionary `getNextTag` returns carries an
//!    insertion-ordered array of its member names, tag name included
//!    (`KAGParser.cpp:1333-1341`, filled by `ArgValue::add` `:1308-1326` with
//!    the tag name added at `:1803-1806`). This is the part games consume, and
//!    **the engine already emits it** for every KAG tag dictionary:
//!    `tag_to_dictionary` in `crates/krkr-engine/src/kag.rs` attaches
//!    `taglist` with `tagname` first and the attributes in scenario order.
//!    Nothing in this module installs it — `taglist` is covered by
//!    `taglist_orders_the_tag_name_and_every_attribute` below.
//! 2. **`paramMacros`** — a read-only per-instance Dictionary built in the
//!    ctor (`:395-406`), denied to script assignment (`:2820-2833`), copied by
//!    `assign` (`:446-451`) and round-tripped by `store`/`restore`
//!    (`:537, 773-780`). It changes parsing: `EntryParam` (`:1551-1633`) looks
//!    the lowercased attribute name up in the dictionary, and when the value
//!    is an object it walks it in pairs, strips a leading `&`/`%` per value to
//!    mark it entity/macro-arg, recurses (a macro entry may name another
//!    macro), adds every pair to the result dictionary and drops the
//!    macro-named attribute itself. The `pmacro`/`erasepmacro` special tags
//!    (`:1848-1851, 2323-2336`) are the scenario-side way to fill it —
//!    `erasepmacro` raises `TVPUnknownMacroName` for an unknown name — and
//!    with `processSpecialTags = false` they are ordinary tags.
//! 3. **`multiLineTagEnabled`** — read-write, default false (`:402,
//!    2915-2933`). While set, an attribute loop that reaches a line-final `\`
//!    splices the next line into the current one (`:2345-2373`), requiring
//!    that line to start with `;` (a missing `;`, or running past the last
//!    line, raises `TVPKAGSyntaxError`), drops both the `\` and the `;`, then
//!    resumes after the consumed lines and logs `ignore after multi-line tag`
//!    (`:1869`, inside the `TVP_KAG_STEP_NEXT` macro `:1864-1884`) at
//!    `debugLevel >= tkdlSimple` (`:1867`).
//!
//! The two parsing behaviours are engine capabilities, not plugin ones. The
//! parser that decides them is `krkr_kag::KagParser`
//! (`crates/krkr-kag/src/parser.rs`), which `crates/krkr-plugins/Cargo.toml`
//! does not depend on and which exposes no attribute or line hook — its only
//! seam is the `KagHost` callback trait — and the engine's `KAGParser` class
//! (`crates/krkr-engine/src/native/kag.rs`) reads exactly five members back
//! into the parser: `ignoreCR`, `processSpecialTags`, `debugLevel`, `macros`
//! and `curStorage` (`sync_parser_from_members`). No member a plugin can
//! install is consulted while a tag is parsed, and `install_kag_parser_methods`
//! installs the engine's own methods on every parser instance, so a
//! class-level wrapper is shadowed for `new KAGParser()` and for a TJS subclass
//! instance alike — measured, not assumed, by
//! `a_class_level_override_cannot_reach_parser_instances` below. Registering
//! `paramMacros` and `multiLineTagEnabled` here would therefore hand scripts a
//! surface whose writes change no parse result — a game could set
//! `multiLineTagEnabled = true` and still get the parse error below — so the
//! two members, the two special tags and the message texts stay prerequisites:
//!
//! - attribute expansion, the multi-line splice and the `pmacro`/`erasepmacro`
//!   tag kinds belong in `crates/krkr-kag/src/parser.rs`;
//! - the two properties, their `store`/`restore` fields and the member sync
//!   belong in `crates/krkr-engine/src/native/kag.rs`;
//! - the texts are the core KAG message map's
//!   (`krkrz/src/plugins/win32/KAGParser/KAGParser.cpp:30-41`), which the Ex
//!   resolves at throw time (`KAGParserEx.hpp:14`), while `kag_to_tjs`
//!   (`crates/krkr-engine/src/native/kag.rs`) reports parse errors in English
//!   today.
//!
//! A TJS class compiled at registration time and installed as the global
//! `KAGParser` is the one route left open to this module, and it was rejected:
//! it would have to reimplement the attribute grammar — expansion order,
//! `cond`, `&`/`%` evaluation — in script, it would break the moment a game
//! defines its own `getNextTag` or `onScenarioLoad`, and the engine's own KAG
//! session would stay outside it either way, because that session calls
//! `KagParser::next_tag_with` directly instead of going through the class.
//!
//! `kagparser_ex_members_are_absent_rather_than_inert` and
//! `multi_line_tags_and_kag_message_texts_await_the_engine_support` pin the
//! current, pre-implementation behaviour so the gap is reported instead of
//! faked; they are the tests to replace when the parser work lands.
//!
//! One deliberate difference from the krkrz Ex tree: `[emb]`'s optional
//! `escape` attribute (`krkr2/.../kagparserex/KAGParser.cpp:1659, 2092-2101`)
//! is implemented by our engine (`krkr-kag`'s `handle_special_tag`, `"emb"`)
//! even though krkrz's Ex dropped it.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "KAGParser tag dictionaries expose taglist",
    notes: "The engine emits the ordered taglist for every KAG tag; paramMacros/pmacro/erasepmacro, \
            multiLineTagEnabled and the TVPKAG* message texts are parser capabilities (krkr-kag + \
            native/kag.rs), so they are reported as prerequisites instead of being stubbed here.",
    install: |engine| engine.register_plugin(KagParserExPlugin),
};

pub struct KagParserExPlugin;

impl KrkrPlugin for KagParserExPlugin {
    fn name(&self) -> &str {
        "KAGParserEx.dll"
    }

    /// Installs no surface on purpose. The class the DLL replaces is already
    /// engine-native, and every member this plugin could add either duplicates
    /// what the engine gives the class (`taglist`) or would be an inert
    /// placeholder for a parse behaviour the parser does not have yet; see the
    /// module docs for what has to land in the engine instead.
    fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::*;

    fn temp_root() -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "Kirakira-kag-parser-ex-{}-{nanos}-{id}",
            std::process::id()
        ))
    }

    /// An engine over `root` with this plugin registered — the state a game
    /// reaches after `Plugins.link("KAGParserEx.dll")`.
    fn engine_with_project(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine
            .register_plugin(KagParserExPlugin)
            .expect("register plugin");
        engine
    }

    fn write_scenario(root: &Path, name: &str, source: &str) {
        std::fs::create_dir_all(root).expect("create temp root");
        std::fs::write(root.join(name), source).expect("write scenario");
    }

    /// The Ex feature games (PARQUET among them) actually consume: the tag
    /// dictionary carries `taglist`, tag name first, attributes in the order
    /// the scenario wrote them, a valueless attribute reading as `"true"`.
    #[test]
    fn taglist_orders_the_tag_name_and_every_attribute() {
        let root = temp_root();
        write_scenario(&root, "first.ks", "[tag foo=1 bar]");

        let mut engine = engine_with_project(&root);
        let value = engine
            .execute_script(
                "kag-parser-ex-taglist.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("first.ks");
                var tag = parser.getNextTag();
                var names = "";
                for (var i = 0; i < tag.taglist.count; i++) {
                    names += tag.taglist[i];
                    if (i + 1 < tag.taglist.count) names += ",";
                }
                return names + "|" + tag.tagname + "|" + tag.foo + "|" + tag.bar;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("tagname,foo,bar|tag|1|true".to_string())
        );

        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    /// Why the two properties are engine work rather than a class-level
    /// install here, measured instead of assumed: a `getNextTag` a plugin puts
    /// on the global class object answers for no instance, because the engine
    /// installs its own method on every parser it constructs
    /// (`install_kag_parser_methods` checks the instance's own member table
    /// only, `native/kag.rs`) and an instance's own member wins the lookup. A
    /// plain `new KAGParser()` does not even reach the class object — a
    /// class-level data member stays invisible there, while a TJS subclass
    /// instance sees it — so nothing registered from this module could change
    /// what a tag parse produces.
    #[test]
    fn a_class_level_override_cannot_reach_parser_instances() {
        let root = temp_root();
        write_scenario(&root, "first.ks", "[tag a=1]");

        let mut engine = engine_with_project(&root);
        let class = match engine.tjs_runtime().global_member("KAGParser") {
            Variant::Object(class) => class,
            other => panic!("global KAGParser is not an object: {other:?}"),
        };
        engine
            .tjs_runtime_mut()
            .set_object_member(class, "probeMark", Variant::Integer(7));
        engine.tjs_runtime_mut().register_object_native(
            class,
            "getNextTag",
            |_runtime: &mut Runtime<KrkrHost>,
             _this_obj: Option<krkr_tjs2::runtime::ObjectHandle>,
             _args: Vec<Variant>| { Ok(Variant::String("override".to_string())) },
        );

        let value = engine
            .execute_script(
                "kag-parser-ex-shadowing.tjs",
                r#"
                var plain = new KAGParser();
                plain.loadScenario("first.ks");
                var plainTag = plain.getNextTag();
                class Sub extends KAGParser {
                    function Sub() { super.KAGParser(); }
                }
                var sub = new Sub();
                sub.loadScenario("first.ks");
                var subTag = sub.getNextTag();
                return (typeof plain.probeMark) + "/" + (typeof sub.probeMark) + "/" +
                       plainTag.tagname + "/" + subTag.tagname;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("undefined/Integer/tag/tag".to_string())
        );

        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    /// The honest-reporting contract, executable: `paramMacros` and
    /// `multiLineTagEnabled` are absent rather than stubbed, and `pmacro` is
    /// still an ordinary tag, because a surface whose writes change no parse
    /// result would tell a game its parameters are being expanded when they
    /// are not. This test is the one to replace with default/macro-table/
    /// special-tag tests once the parser work lands.
    #[test]
    fn kagparser_ex_members_are_absent_rather_than_inert() {
        let root = temp_root();
        write_scenario(&root, "first.ks", "[pmacro name=p a=1]");

        let mut engine = engine_with_project(&root);
        let value = engine
            .execute_script(
                "kag-parser-ex-absent.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("first.ks");
                var tag = parser.getNextTag();
                return (typeof parser.paramMacros) + "/" +
                       (typeof parser.multiLineTagEnabled) + "/" +
                       tag.tagname + "/" + tag.name + "/" + tag.a;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("undefined/undefined/pmacro/p/1".to_string())
        );

        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    /// What a multi-line tag does today, both spellings. The DLL splices the
    /// continuation into the tag while `multiLineTagEnabled` is set
    /// (`KAGParser.cpp:2345-2373`) and raises `TVPKAGSyntaxError` when the
    /// continuation line does not start with `;`; our parser has neither the
    /// option nor those texts, so the bracketed form ends as an unclosed tag
    /// and the `@` form reads the trailing `\` as an attribute name. Replace
    /// this test with the single- vs multi-line and message-text tests when
    /// the parser grows the option.
    #[test]
    fn multi_line_tags_and_kag_message_texts_await_the_engine_support() {
        let root = temp_root();
        write_scenario(&root, "bracket.ks", "[tag a=1 \\\n;    b=2]");
        write_scenario(&root, "command.ks", "@tag a=1 \\\n;    b=2");

        let mut engine = engine_with_project(&root);
        let error = engine
            .execute_script(
                "kag-parser-ex-bracket.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("bracket.ks");
                parser.getNextTag();
                "#,
            )
            .expect_err("the bracketed continuation is an unclosed tag today");
        let message = error.to_string();
        assert!(
            message.contains("unterminated KAG tag"),
            "unexpected parse error: {message}"
        );

        let value = engine
            .execute_script(
                "kag-parser-ex-command.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("command.ks");
                var tag = parser.getNextTag();
                var names = "";
                for (var i = 0; i < tag.taglist.count; i++) names += tag.taglist[i] + ",";
                return names + "|" + tag.a;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("tagname,a,\\,|1".to_string()));

        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}
