use std::path::PathBuf;

use krkr_tjs2::{
    Result,
    runtime::{Runtime, Variant},
};

use crate::{
    globals::install_tvp_globals,
    host::KrkrHost,
    plugin::KrkrPlugin,
    script::{execute_expression_on_runtime, execute_script_on_runtime},
};

#[derive(Clone, Debug, Default)]
pub struct EngineConfig {
    pub project_root: Option<PathBuf>,
}

pub struct KrkrEngine {
    tjs_runtime: Runtime<KrkrHost>,
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
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("krkr-ruri-engine-{}-{nanos}", std::process::id()))
    }
}
