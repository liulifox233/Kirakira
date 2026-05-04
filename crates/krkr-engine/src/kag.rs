use krkr_kag::{
    Attribute, AttributeValue, CallFrame, KagError, KagHost, KagParser, LabelEvent,
    ScenarioLoadEvent, ScriptEvent, Tag,
};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::{
    host::KrkrHost,
    native::refresh_kag_parser_object,
    script::{execute_expression_on_runtime, execute_script_on_runtime},
};

pub(crate) struct EngineKagHost<'a> {
    runtime: &'a mut Runtime<KrkrHost>,
    owner: ObjectHandle,
}

impl<'a> EngineKagHost<'a> {
    pub(crate) fn for_owner(runtime: &'a mut Runtime<KrkrHost>, owner: ObjectHandle) -> Self {
        Self { runtime, owner }
    }

    fn call_event(&mut self, name: &str, args: Vec<Variant>) -> krkr_kag::Result<Option<Variant>> {
        if matches!(self.runtime.object_member(self.owner, name), Variant::Void) {
            return Ok(None);
        }
        self.runtime
            .call_object_method(self.owner, name, args)
            .map(Some)
            .map_err(kag_host_error)
    }

    fn call_process_event(&mut self, name: &str, tag: &Tag) -> krkr_kag::Result<bool> {
        let tag = tag_to_dictionary(self.runtime, tag).map_err(kag_host_error)?;
        Ok(self
            .call_event(name, vec![Variant::Object(tag)])?
            .map(|value| value.is_truthy())
            .unwrap_or(true))
    }
}

impl KagHost for EngineKagHost<'_> {
    fn sync_parser_state(&mut self, parser: &KagParser) -> krkr_kag::Result<()> {
        self.runtime
            .host_mut()
            .insert_kag_parser(self.owner, parser.clone());
        refresh_kag_parser_object(self.runtime, self.owner, parser).map_err(kag_host_error)
    }

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
        if let Some(value) = self.call_event(
            "onScenarioLoad",
            vec![Variant::String(event.storage.to_string())],
        )? {
            return Ok(match value {
                Variant::String(source) => Some(source),
                _ => None,
            });
        }
        self.runtime
            .host_mut()
            .log(&format!("KAG loading scenario `{}`", event.storage));
        Ok(None)
    }

    fn on_scenario_loaded(&mut self, event: ScenarioLoadEvent<'_>) -> krkr_kag::Result<()> {
        if self
            .call_event(
                "onScenarioLoaded",
                vec![Variant::String(event.storage.to_string())],
            )?
            .is_some()
        {
            return Ok(());
        }
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
        let page = event
            .label
            .page_name
            .clone()
            .map(Variant::String)
            .unwrap_or(Variant::Void);
        if self
            .call_event(
                "onLabel",
                vec![Variant::String(event.label.name.clone()), page],
            )?
            .is_some()
        {
            return Ok(());
        }
        self.runtime.host_mut().log(&format!(
            "KAG label `{}` in `{}`",
            event.label.name, event.storage
        ));
        Ok(())
    }

    fn on_script(&mut self, event: ScriptEvent<'_>) -> krkr_kag::Result<()> {
        if self
            .call_event(
                "onScript",
                vec![
                    Variant::String(event.script.to_string()),
                    Variant::String(event.storage.to_string()),
                    Variant::Integer(event.span.start as i64),
                ],
            )?
            .is_some()
        {
            return Ok(());
        }
        execute_script_on_runtime(self.runtime, event.storage, event.script)
            .map(|_| ())
            .map_err(kag_host_error)
    }

    fn on_jump(
        &mut self,
        tag: &Tag,
        _storage: Option<&str>,
        _target: Option<&str>,
    ) -> krkr_kag::Result<bool> {
        self.call_process_event("onJump", tag)
    }

    fn on_call(
        &mut self,
        tag: &Tag,
        _storage: Option<&str>,
        _target: Option<&str>,
    ) -> krkr_kag::Result<bool> {
        self.call_process_event("onCall", tag)
    }

    fn on_return(
        &mut self,
        tag: &Tag,
        _storage: Option<&str>,
        _target: Option<&str>,
    ) -> krkr_kag::Result<bool> {
        self.call_process_event("onReturn", tag)
    }

    fn on_call_stack_depth(&mut self, depth: usize) -> krkr_kag::Result<()> {
        self.runtime.set_object_member(
            self.owner,
            "callStackDepth",
            Variant::Integer(depth as i64),
        );
        Ok(())
    }

    fn on_after_return(&mut self, _frame: &CallFrame) -> krkr_kag::Result<()> {
        self.call_event("onAfterReturn", Vec::new())?;
        Ok(())
    }
}

fn eval_expression(runtime: &mut Runtime<KrkrHost>, expression: &str) -> krkr_kag::Result<Variant> {
    execute_expression_on_runtime(runtime, expression, expression).map_err(kag_host_error)
}

fn kag_host_error(error: impl std::fmt::Display) -> KagError {
    KagError::host(error.to_string())
}

pub(crate) fn tag_to_dictionary(
    runtime: &mut Runtime<KrkrHost>,
    tag: &Tag,
) -> Result<ObjectHandle> {
    let object = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(object, "Dictionary");
    runtime.set_object_member(object, "tagname", Variant::String(tag.tagname.clone()));
    fill_attributes_dictionary(runtime, object, &tag.attributes);
    Ok(object)
}

pub(crate) fn attributes_to_dictionary(
    runtime: &mut Runtime<KrkrHost>,
    attributes: &[Attribute],
) -> Result<ObjectHandle> {
    let object = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(object, "Dictionary");
    fill_attributes_dictionary(runtime, object, attributes);
    Ok(object)
}

fn fill_attributes_dictionary(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    attributes: &[Attribute],
) {
    for attribute in attributes {
        if let Attribute::Named { name, value } = attribute {
            runtime.set_object_member(object, name, attribute_value_to_variant(value));
        }
    }
}

fn attribute_value_to_variant(value: &AttributeValue) -> Variant {
    raw_attribute_value_to_variant(value.raw())
}

fn raw_attribute_value_to_variant(value: &str) -> Variant {
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") {
        Variant::Integer(1)
    } else if value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no") {
        Variant::Integer(0)
    } else {
        Variant::String(value.to_string())
    }
}
