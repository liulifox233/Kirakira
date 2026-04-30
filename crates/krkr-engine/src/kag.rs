use krkr_kag::{KagError, KagHost, LabelEvent, ScenarioLoadEvent, ScriptEvent};
use krkr_tjs2::runtime::{Runtime, Variant};

use crate::{
    host::KrkrHost,
    script::{execute_expression_on_runtime, execute_script_on_runtime},
};

pub(crate) struct EngineKagHost<'a> {
    runtime: &'a mut Runtime<KrkrHost>,
}

impl<'a> EngineKagHost<'a> {
    pub(crate) fn new(runtime: &'a mut Runtime<KrkrHost>) -> Self {
        Self { runtime }
    }
}

impl KagHost for EngineKagHost<'_> {
    fn load_scenario(&mut self, storage: &str) -> krkr_kag::Result<String> {
        self.runtime
            .host()
            .read_text_storage(storage)
            .map_err(kag_host_error)
    }

    fn on_scenario_load(
        &mut self,
        event: ScenarioLoadEvent<'_>,
    ) -> krkr_kag::Result<Option<String>> {
        self.runtime
            .host_mut()
            .log(&format!("KAG loading scenario `{}`", event.storage));
        Ok(None)
    }

    fn on_scenario_loaded(&mut self, event: ScenarioLoadEvent<'_>) -> krkr_kag::Result<()> {
        self.runtime
            .host_mut()
            .log(&format!("KAG loaded scenario `{}`", event.storage));
        Ok(())
    }

    fn eval_bool(&mut self, expression: &str) -> krkr_kag::Result<bool> {
        Ok(eval_expression(self.runtime, expression)?.is_truthy())
    }

    fn eval_string(&mut self, expression: &str) -> krkr_kag::Result<String> {
        eval_expression(self.runtime, expression)?
            .to_tjs_string()
            .map_err(kag_host_error)
    }

    fn on_label(&mut self, event: LabelEvent<'_>) -> krkr_kag::Result<()> {
        self.runtime.host_mut().log(&format!(
            "KAG label `{}` in `{}`",
            event.label.name, event.storage
        ));
        Ok(())
    }

    fn on_script(&mut self, event: ScriptEvent<'_>) -> krkr_kag::Result<()> {
        execute_script_on_runtime(self.runtime, event.storage, event.script)
            .map(|_| ())
            .map_err(kag_host_error)
    }
}

fn eval_expression(runtime: &mut Runtime<KrkrHost>, expression: &str) -> krkr_kag::Result<Variant> {
    execute_expression_on_runtime(runtime, expression, expression).map_err(kag_host_error)
}

fn kag_host_error(error: impl std::fmt::Display) -> KagError {
    KagError::host(error.to_string())
}
