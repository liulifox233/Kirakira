use krkr_kag::{
    Attribute, AttributeValue, CallFrame, DebugLevel, KagError, KagHost, KagParser, LabelEvent,
    ParserSnapshot, ScenarioLoadEvent, ScriptEvent, Tag,
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
    vm::Vm,
};

use crate::{
    host::KrkrHost,
    script::{execute_expression_on_runtime, execute_script_on_runtime},
};

use super::{arg_string, required_arg_string};

pub(crate) fn install_kag_parser(runtime: &mut Runtime<KrkrHost>) {
    let handle = runtime.register_global_native("KAGParser", kag_parser_constructor);
    runtime.add_object_class_info(handle, "KAGParser");
    install_kag_parser_methods(runtime, handle);
}

fn kag_parser_constructor(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = this_obj
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(handle, "KAGParser");
    install_kag_parser_methods(runtime, handle);
    runtime
        .host_mut()
        .insert_kag_parser(handle, KagParser::new());
    refresh_kag_parser_members(runtime, handle)?;
    Ok(Variant::Object(handle))
}

fn install_kag_parser_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_vm_native(handle, "loadScenario", kag_load_scenario);
    runtime.register_object_vm_native(handle, "goToLabel", kag_go_to_label);
    runtime.register_object_vm_native(handle, "callLabel", kag_call_label);
    runtime.register_object_vm_native(handle, "getNextTag", kag_get_next_tag);
    runtime.register_object_vm_native(handle, "assign", kag_assign);
    runtime.register_object_vm_native(handle, "clear", kag_clear);
    runtime.register_object_vm_native(handle, "store", kag_store);
    runtime.register_object_vm_native(handle, "restore", kag_restore);
    runtime.register_object_vm_native(handle, "clearCallStack", kag_clear_call_stack);
    runtime.register_object_vm_native(handle, "interrupt", kag_interrupt);
    runtime.register_object_vm_native(handle, "resetInterrupt", kag_reset_interrupt);
    runtime.register_object_vm_native(handle, "popMacroArgs", kag_pop_macro_args);
}

fn kag_load_scenario(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let storage = required_arg_string(&args, 0, "KAGParser.loadScenario")?;
    with_parser(
        vm,
        this_obj,
        "KAGParser.loadScenario",
        |parser, vm, owner| {
            let mut host = TjsKagHost::new(vm, owner);
            parser
                .load_scenario_with(storage, &mut host)
                .map_err(kag_to_tjs)?;
            Ok(Variant::Void)
        },
    )
}

fn kag_go_to_label(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let label = required_arg_string(&args, 0, "KAGParser.goToLabel")?;
    with_parser(vm, this_obj, "KAGParser.goToLabel", |parser, _, _| {
        parser.go_to_label(&label).map_err(kag_to_tjs)?;
        Ok(Variant::Void)
    })
}

fn kag_call_label(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let label = required_arg_string(&args, 0, "KAGParser.callLabel")?;
    with_parser(vm, this_obj, "KAGParser.callLabel", |parser, _, _| {
        parser.call_label(&label).map_err(kag_to_tjs)?;
        Ok(Variant::Void)
    })
}

fn kag_get_next_tag(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.getNextTag", |parser, vm, owner| {
        let mut host = TjsKagHost::new(vm, owner);
        let Some(tag) = parser.next_tag_with(&mut host).map_err(kag_to_tjs)? else {
            return Ok(Variant::Void);
        };
        Ok(Variant::Object(tag_to_dictionary(
            host.vm.runtime_mut(),
            &tag,
        )?))
    })
}

fn kag_assign(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(Variant::Object(source_handle)) = args.first().cloned() else {
        return Err(TjsError::runtime("KAGParser.assign requires a KAGParser"));
    };
    let source = vm
        .runtime()
        .host()
        .kag_parser(source_handle)
        .cloned()
        .ok_or_else(|| TjsError::runtime("KAGParser.assign requires a KAGParser"))?;
    with_parser(vm, this_obj, "KAGParser.assign", |parser, _, _| {
        parser.assign(&source);
        Ok(Variant::Void)
    })
}

fn kag_clear(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.clear", |parser, _, _| {
        parser.clear();
        Ok(Variant::Void)
    })
}

fn kag_store(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.store", |parser, vm, _| {
        let runtime = vm.runtime_mut();
        let snapshot = parser.store();
        let id = runtime.host_mut().store_kag_snapshot(snapshot);
        let object = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(object, "Dictionary");
        runtime.set_object_member(object, "__kagSnapshotId", Variant::Integer(id));
        runtime.set_object_member(
            object,
            "curStorage",
            Variant::String(parser.cur_storage().unwrap_or_default().to_string()),
        );
        runtime.set_object_member(
            object,
            "curLabel",
            Variant::String(parser.cur_label().unwrap_or_default().to_string()),
        );
        runtime.set_object_member(
            object,
            "curLine",
            Variant::Integer(parser.cur_line().unwrap_or(0) as i64),
        );
        runtime.set_object_member(
            object,
            "curPos",
            Variant::Integer(parser.cur_pos().unwrap_or(0) as i64),
        );
        Ok(Variant::Object(object))
    })
}

fn kag_restore(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(Variant::Object(snapshot_object)) = args.first().cloned() else {
        return Err(TjsError::runtime(
            "KAGParser.restore requires a snapshot object",
        ));
    };
    let snapshot = snapshot_from_object(vm.runtime(), snapshot_object)?;
    with_parser(vm, this_obj, "KAGParser.restore", |parser, _, _| {
        parser.restore(snapshot).map_err(kag_to_tjs)?;
        Ok(Variant::Void)
    })
}

fn kag_clear_call_stack(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.clearCallStack", |parser, _, _| {
        parser.clear_call_stack();
        Ok(Variant::Void)
    })
}

fn kag_interrupt(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.interrupt", |parser, _, _| {
        parser.interrupt();
        Ok(Variant::Void)
    })
}

fn kag_reset_interrupt(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.resetInterrupt", |parser, _, _| {
        parser.reset_interrupt();
        Ok(Variant::Void)
    })
}

fn kag_pop_macro_args(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.popMacroArgs", |parser, _, _| {
        parser.pop_macro_args().map_err(kag_to_tjs)?;
        Ok(Variant::Void)
    })
}

fn with_parser<F>(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    method: &str,
    f: F,
) -> Result<Variant>
where
    F: FnOnce(&mut KagParser, &mut Vm<KrkrHost>, ObjectHandle) -> Result<Variant>,
{
    let handle = require_kag_this(this_obj, method)?;
    let mut parser = vm
        .runtime_mut()
        .host_mut()
        .take_kag_parser(handle)
        .ok_or_else(|| TjsError::runtime(format!("{method} requires a KAGParser")))?;

    let result = (|| {
        sync_parser_from_members(vm, handle, &mut parser)?;
        vm.runtime_mut()
            .host_mut()
            .insert_kag_parser(handle, parser.clone());
        let result = f(&mut parser, vm, handle);
        refresh_kag_parser_members_from_parser(vm.runtime_mut(), handle, &parser)?;
        result
    })();

    vm.runtime_mut()
        .host_mut()
        .insert_kag_parser(handle, parser);
    result
}

fn require_kag_this(this_obj: Option<ObjectHandle>, method: &str) -> Result<ObjectHandle> {
    this_obj.ok_or_else(|| TjsError::runtime(format!("{method} requires a KAGParser instance")))
}

fn sync_parser_from_members(
    vm: &mut Vm<KrkrHost>,
    handle: ObjectHandle,
    parser: &mut KagParser,
) -> Result<()> {
    let runtime = vm.runtime_mut();
    parser.set_ignore_cr(runtime.object_member(handle, "ignoreCR").is_truthy());
    parser.set_process_special_tags(
        runtime
            .object_member(handle, "processSpecialTags")
            .is_truthy(),
    );
    parser.set_debug_level(debug_level_from_variant(
        &runtime.object_member(handle, "debugLevel"),
    )?);

    if let Some(storage) = arg_string(&[runtime.object_member(handle, "curStorage")], 0)?
        && !storage.is_empty()
        && parser.cur_storage() != Some(storage.as_str())
    {
        let mut host = TjsKagHost::new(vm, handle);
        parser
            .load_scenario_with(storage, &mut host)
            .map_err(kag_to_tjs)?;
    }

    Ok(())
}

fn refresh_kag_parser_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) -> Result<()> {
    let parser = runtime
        .host()
        .kag_parser(handle)
        .cloned()
        .ok_or_else(|| TjsError::runtime("KAGParser instance is not registered"))?;
    refresh_kag_parser_members_from_parser(runtime, handle, &parser)
}

fn refresh_kag_parser_members_from_parser(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    parser: &KagParser,
) -> Result<()> {
    runtime.set_object_member(
        handle,
        "curLine",
        Variant::Integer(parser.cur_line().unwrap_or(0) as i64),
    );
    runtime.set_object_member(
        handle,
        "curPos",
        Variant::Integer(parser.cur_pos().unwrap_or(0) as i64),
    );
    runtime.set_object_member(
        handle,
        "curLineStr",
        Variant::String(parser.cur_line_str().unwrap_or_default().to_string()),
    );
    runtime.set_object_member(
        handle,
        "processSpecialTags",
        Variant::Integer(i64::from(parser.process_special_tags())),
    );
    runtime.set_object_member(
        handle,
        "ignoreCR",
        Variant::Integer(i64::from(parser.ignore_cr())),
    );
    runtime.set_object_member(
        handle,
        "debugLevel",
        Variant::Integer(debug_level_to_integer(parser.debug_level())),
    );
    runtime.set_object_member(
        handle,
        "callStackDepth",
        Variant::Integer(parser.call_stack_depth() as i64),
    );
    runtime.set_object_member(
        handle,
        "curStorage",
        Variant::String(parser.cur_storage().unwrap_or_default().to_string()),
    );
    runtime.set_object_member(
        handle,
        "curLabel",
        Variant::String(parser.cur_label().unwrap_or_default().to_string()),
    );

    let macros = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(macros, "Dictionary");
    for (name, source) in parser.macro_definitions() {
        runtime.set_object_member(macros, name, Variant::String(source.to_string()));
    }
    runtime.set_object_member(handle, "macros", Variant::Object(macros));

    let params = attributes_to_dictionary(runtime, parser.macro_params())?;
    runtime.set_object_member(handle, "macroParams", Variant::Object(params));
    runtime.set_object_member(handle, "mp", Variant::Object(params));
    Ok(())
}

struct TjsKagHost<'a, 'bc, 'rt> {
    vm: &'a mut Vm<'bc, 'rt, KrkrHost>,
    owner: ObjectHandle,
}

impl<'a, 'bc, 'rt> TjsKagHost<'a, 'bc, 'rt> {
    fn new(vm: &'a mut Vm<'bc, 'rt, KrkrHost>, owner: ObjectHandle) -> Self {
        Self { vm, owner }
    }

    fn call_event(&mut self, name: &str, args: Vec<Variant>) -> krkr_kag::Result<Option<Variant>> {
        if matches!(
            self.vm.runtime().object_member(self.owner, name),
            Variant::Void
        ) {
            return Ok(None);
        }
        self.vm
            .call_object_method(self.owner, name, args)
            .map(Some)
            .map_err(kag_host_error)
    }

    fn call_process_event(&mut self, name: &str, tag: &Tag) -> krkr_kag::Result<bool> {
        let tag = tag_to_dictionary(self.vm.runtime_mut(), tag).map_err(kag_host_error)?;
        Ok(self
            .call_event(name, vec![Variant::Object(tag)])?
            .map(|value| value.is_truthy())
            .unwrap_or(true))
    }
}

impl KagHost for TjsKagHost<'_, '_, '_> {
    fn load_scenario(&mut self, storage: &str) -> krkr_kag::Result<String> {
        self.vm
            .runtime()
            .host()
            .read_text_storage(storage)
            .map_err(kag_host_error)
    }

    fn on_scenario_load(
        &mut self,
        event: ScenarioLoadEvent<'_>,
    ) -> krkr_kag::Result<Option<String>> {
        self.call_event(
            "onScenarioLoad",
            vec![Variant::String(event.storage.to_string())],
        )?
        .and_then(|value| match value {
            Variant::String(source) => Some(Ok(source)),
            _ => None,
        })
        .transpose()
    }

    fn on_scenario_loaded(&mut self, event: ScenarioLoadEvent<'_>) -> krkr_kag::Result<()> {
        self.call_event(
            "onScenarioLoaded",
            vec![Variant::String(event.storage.to_string())],
        )?;
        Ok(())
    }

    fn eval_bool(&mut self, expression: &str) -> krkr_kag::Result<bool> {
        Ok(eval_expression(self.vm.runtime_mut(), expression)?.is_truthy())
    }

    fn eval_string(&mut self, expression: &str) -> krkr_kag::Result<String> {
        eval_expression(self.vm.runtime_mut(), expression)?
            .to_tjs_string()
            .map_err(kag_host_error)
    }

    fn on_label(&mut self, event: LabelEvent<'_>) -> krkr_kag::Result<()> {
        self.call_event(
            "onLabel",
            vec![
                Variant::String(event.label.name.clone()),
                Variant::String(event.label.page_name.clone().unwrap_or_default()),
            ],
        )?;
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
            .is_none()
        {
            execute_script_on_runtime(self.vm.runtime_mut(), event.storage, event.script)
                .map(|_| ())
                .map_err(kag_host_error)?;
        }
        Ok(())
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

    fn on_after_return(&mut self, _frame: &CallFrame) -> krkr_kag::Result<()> {
        self.call_event("onAfterReturn", Vec::new())?;
        Ok(())
    }
}

fn tag_to_dictionary(runtime: &mut Runtime<KrkrHost>, tag: &Tag) -> Result<ObjectHandle> {
    let object = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(object, "Dictionary");
    runtime.set_object_member(object, "tagname", Variant::String(tag.tagname.clone()));
    for attribute in &tag.attributes {
        if let Attribute::Named { name, value } = attribute {
            runtime.set_object_member(object, name, attribute_value_to_variant(value)?);
        }
    }
    Ok(object)
}

fn attributes_to_dictionary(
    runtime: &mut Runtime<KrkrHost>,
    attributes: &[Attribute],
) -> Result<ObjectHandle> {
    let object = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(object, "Dictionary");
    for attribute in attributes {
        if let Attribute::Named { name, value } = attribute {
            runtime.set_object_member(object, name, attribute_value_to_variant(value)?);
        }
    }
    Ok(object)
}

fn attribute_value_to_variant(value: &AttributeValue) -> Result<Variant> {
    Ok(raw_attribute_value_to_variant(value.raw()))
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

fn snapshot_from_object(
    runtime: &Runtime<KrkrHost>,
    snapshot_object: ObjectHandle,
) -> Result<ParserSnapshot> {
    let id = runtime
        .object_member(snapshot_object, "__kagSnapshotId")
        .to_integer()?;
    runtime
        .host()
        .kag_snapshot(id)
        .cloned()
        .ok_or_else(|| TjsError::runtime("KAGParser snapshot is not available"))
}

fn debug_level_from_variant(value: &Variant) -> Result<DebugLevel> {
    Ok(match value.to_integer()? {
        0 => DebugLevel::None,
        2 => DebugLevel::Verbose,
        _ => DebugLevel::Simple,
    })
}

fn debug_level_to_integer(level: DebugLevel) -> i64 {
    match level {
        DebugLevel::None => 0,
        DebugLevel::Simple => 1,
        DebugLevel::Verbose => 2,
    }
}

fn kag_to_tjs(error: krkr_kag::KagError) -> TjsError {
    TjsError::runtime(error.to_string())
}

fn eval_expression(runtime: &mut Runtime<KrkrHost>, expression: &str) -> krkr_kag::Result<Variant> {
    execute_expression_on_runtime(runtime, expression, expression).map_err(kag_host_error)
}

fn kag_host_error(error: impl std::fmt::Display) -> KagError {
    KagError::host(error.to_string())
}
