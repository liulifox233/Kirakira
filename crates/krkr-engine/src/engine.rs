use std::path::PathBuf;

use krkr_kag::{KagParser, Tag};
use krkr_tjs2::{
    Result,
    runtime::{Runtime, Variant},
};

use crate::{
    globals::install_tvp_globals,
    host::KrkrHost,
    kag::EngineKagHost,
    plugin::KrkrPlugin,
    script::{execute_expression_on_runtime, execute_script_on_runtime},
};

#[derive(Clone, Debug, Default)]
pub struct EngineConfig {
    pub project_root: Option<PathBuf>,
}

pub struct KrkrEngine {
    tjs_runtime: Runtime<KrkrHost>,
    kag_parser: KagParser,
    plugins: Vec<Box<dyn KrkrPlugin>>,
}

impl KrkrEngine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let host = match config.project_root {
            Some(root) => KrkrHost::for_project(root)?,
            None => KrkrHost::default(),
        };
        let mut tjs_runtime = Runtime::with_host(host);
        install_tvp_globals(&mut tjs_runtime);
        Ok(Self {
            tjs_runtime,
            kag_parser: KagParser::new(),
            plugins: Vec::new(),
        })
    }

    pub fn for_project(root: impl Into<PathBuf>) -> Result<Self> {
        Self::new(EngineConfig {
            project_root: Some(root.into()),
        })
    }

    pub fn tjs_runtime(&self) -> &Runtime<KrkrHost> {
        &self.tjs_runtime
    }

    pub fn tjs_runtime_mut(&mut self) -> &mut Runtime<KrkrHost> {
        &mut self.tjs_runtime
    }

    pub fn host(&self) -> &KrkrHost {
        self.tjs_runtime.host()
    }

    pub fn host_mut(&mut self) -> &mut KrkrHost {
        self.tjs_runtime.host_mut()
    }

    pub fn kag_parser(&self) -> &KagParser {
        &self.kag_parser
    }

    pub fn kag_parser_mut(&mut self) -> &mut KagParser {
        &mut self.kag_parser
    }

    pub fn execute_script(&mut self, source_name: &str, source: &str) -> Result<Variant> {
        execute_script_on_runtime(&mut self.tjs_runtime, source_name, source)
    }

    pub fn execute_expression(&mut self, source_name: &str, source: &str) -> Result<Variant> {
        execute_expression_on_runtime(&mut self.tjs_runtime, source_name, source)
    }

    pub fn execute_storage(&mut self, name: &str) -> Result<Variant> {
        let source = self.tjs_runtime.host().read_text_storage(name)?;
        execute_script_on_runtime(&mut self.tjs_runtime, name, &source)
    }

    pub fn eval_storage(&mut self, name: &str) -> Result<Variant> {
        let source = self.tjs_runtime.host().read_text_storage(name)?;
        execute_expression_on_runtime(&mut self.tjs_runtime, name, &source)
    }

    pub fn execute_startup(&mut self) -> Result<Variant> {
        self.execute_storage("startup.tjs")
    }

    pub fn load_kag_scenario(&mut self, storage: &str) -> krkr_kag::Result<()> {
        let mut host = EngineKagHost::new(&mut self.tjs_runtime);
        self.kag_parser.load_scenario_with(storage, &mut host)
    }

    pub fn next_kag_tag(&mut self) -> krkr_kag::Result<Option<Tag>> {
        let mut host = EngineKagHost::new(&mut self.tjs_runtime);
        self.kag_parser.next_tag_with(&mut host)
    }

    pub fn register_plugin<P>(&mut self, plugin: P) -> Result<()>
    where
        P: KrkrPlugin + 'static,
    {
        plugin.register(&mut self.tjs_runtime)?;
        self.tjs_runtime.host_mut().register_plugin(plugin.name());
        self.plugins.push(Box::new(plugin));
        Ok(())
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use krkr_tjs2::{
        Result,
        runtime::{Runtime, Variant},
    };

    use super::*;
    use crate::{KrkrHost, KrkrPlugin};

    #[test]
    fn installs_core_tjs_and_tvp_globals() {
        let engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        for name in [
            "Array",
            "Dictionary",
            "Date",
            "Math",
            "Exception",
            "RegExp",
            "Debug",
            "System",
            "Storages",
            "Plugins",
            "KAGParser",
            "Scripts",
            "Window",
            "Layer",
            "Bitmap",
            "WaveSoundBuffer",
        ] {
            assert!(
                !matches!(engine.tjs_runtime().global_member(name), Variant::Void),
                "{name} should be registered"
            );
        }
    }

    #[test]
    fn scripts_eval_runs_in_engine() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");

        assert_eq!(
            engine
                .execute_expression("inline.tjs", "Math.abs(-4)")
                .expect("eval"),
            Variant::Real(4.0)
        );
        assert_eq!(
            engine
                .execute_script("inline.tjs", r#"return Scripts.eval("1 + 2");"#)
                .expect("script"),
            Variant::Integer(3)
        );
    }

    #[test]
    fn storage_reads_startup_script_from_project_root() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("startup.tjs"), "return 42;").expect("write startup");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");

        assert_eq!(
            engine.execute_startup().expect("startup"),
            Variant::Integer(42)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn engine_loads_kag_scenario_from_project_storage() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "[emb exp=\"1 + 2\"]").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.load_kag_scenario("first.ks").expect("load scenario");

        let tag = engine
            .next_kag_tag()
            .expect("next tag")
            .expect("embedded text tag");
        assert_eq!(tag.tagname, "ch");
        assert_eq!(tag.literal_attr("text"), Some("3"));
        assert!(engine.next_kag_tag().expect("eof").is_none());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_returns_tag_dictionaries() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A\n").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("first.ks");
                var first = parser.getNextTag();
                var second = parser.getNextTag();
                return first.tagname + ":" + first.text + ":" + second.tagname + ":" + second.eol;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("ch:A:r:true".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_interrupts_before_next_tag() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("first.ks");
                parser.interrupt();
                return parser.getNextTag().tagname;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("interrupt".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_uses_scenario_load_callbacks() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.onScenarioLoad = function(storage) {
                    this.loadedName = storage;
                    return "A";
                };
                parser.onScenarioLoaded = function(storage) {
                    this.loadedDone = storage;
                };
                parser.loadScenario("virtual.ks");
                var tag = parser.getNextTag();
                return tag.text + ":" + parser.loadedName + ":" + parser.loadedDone;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("A:virtual.ks:virtual.ks".to_string())
        );
    }

    #[test]
    fn tjs_kag_parser_fires_label_and_script_callbacks() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "*start|Opening\n[iscript]\nf.value = 7;\n[endscript]\nA",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var f = new Dictionary();
                var parser = new KAGParser();
                parser.onLabel = function(label, page) {
                    this.seenLabel = label;
                    this.seenPage = page;
                };
                parser.onScript = function(script, storage, start) {
                    this.seenScript = script;
                    this.seenScriptStorage = storage;
                    this.seenScriptStart = start;
                    Scripts.exec(script);
                };
                parser.loadScenario("first.ks");
                var tag = parser.getNextTag();
                return parser.seenLabel + ":" + parser.seenPage + ":" +
                    parser.seenScriptStorage + ":" + f.value + ":" + tag.text;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("*start:Opening:first.ks:7:A".to_string())
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_process_callbacks_can_cancel_control_tags() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("first.ks"), "A[jump target=*end]B\n*end\nC").expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.onJump = function(dic) {
                    this.jumpTarget = dic.target;
                    return false;
                };
                parser.loadScenario("first.ks");
                var a = parser.getNextTag();
                var b = parser.getNextTag();
                return a.text + b.text + ":" + parser.jumpTarget;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("AB:*end".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_fires_call_return_callbacks() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "[call target=*sub]X\n*sub\n[return]Y",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.onCall = function(dic) {
                    this.callTarget = dic.target;
                    return true;
                };
                parser.onReturn = function(dic) {
                    this.returned = "yes";
                    return true;
                };
                parser.onAfterReturn = function() {
                    this.afterReturn = "done";
                };
                parser.loadScenario("first.ks");
                var tag = parser.getNextTag();
                return tag.text + ":" + parser.callTarget + ":" +
                    parser.returned + ":" + parser.afterReturn;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("X:*sub:yes:done".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tjs_kag_parser_exposes_pop_macro_args() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("first.ks"),
            "[macro name=m][font face=%face][wait][endmacro][m face=serif]",
        )
        .expect("write scenario");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var parser = new KAGParser();
                parser.loadScenario("first.ks");
                parser.getNextTag();
                var before = parser.macroParams.face;
                parser.popMacroArgs();
                return before + ":" + parser.macroParams.face;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("serif:".to_string()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn plugin_registry_tracks_registered_and_linked_plugins() {
        struct TestPlugin;

        impl KrkrPlugin for TestPlugin {
            fn name(&self) -> &str {
                "test-plugin"
            }

            fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
                runtime.set_global_member("PluginValue", Variant::Integer(9));
                Ok(())
            }
        }

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(TestPlugin).expect("register plugin");

        assert_eq!(engine.plugin_count(), 1);
        assert_eq!(
            engine.tjs_runtime().global_member("PluginValue"),
            Variant::Integer(9)
        );
        assert!(
            engine
                .host()
                .linked_plugins()
                .any(|name| name == "test-plugin")
        );
    }

    fn temp_root() -> PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "krkr-ruri-engine-{}-{nanos}-{id}",
            std::process::id()
        ))
    }
}
