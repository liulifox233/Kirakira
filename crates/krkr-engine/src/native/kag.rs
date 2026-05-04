use krkr_kag::{
    CallFrame, DebugLevel, KagError, KagHost, KagParser, Krkr2CallFrame, Krkr2ConditionState,
    LabelEvent, ParserSnapshot, ScenarioLoadEvent, ScriptEvent, Tag,
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeFunction, ObjectHandle, Runtime, Variant, VmNativeFunction},
    vm::Vm,
};

use crate::{
    host::KrkrHost,
    kag::{attributes_to_dictionary, tag_to_dictionary},
    script::{execute_expression_on_runtime, execute_script_on_runtime},
};

use super::{arg_string, required_arg_string};

pub(crate) fn install_kag_parser(runtime: &mut Runtime<KrkrHost>) {
    let handle = runtime.register_global_native("KAGParser", kag_parser_constructor);
    runtime.add_object_class_info(handle, "KAGParser");
    install_kag_parser_methods(runtime, handle);
}

pub(crate) fn create_kag_parser_object(runtime: &mut Runtime<KrkrHost>) -> Result<ObjectHandle> {
    let handle = runtime.alloc_ordinary_object();
    initialize_kag_parser_object(runtime, handle, KagParser::new())?;
    Ok(handle)
}

pub(crate) fn refresh_kag_parser_object(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    parser: &KagParser,
) -> Result<()> {
    refresh_kag_parser_members_from_parser(runtime, handle, parser)
}

fn kag_parser_constructor(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = this_obj
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object());
    initialize_kag_parser_object(runtime, handle, KagParser::new())?;
    Ok(Variant::Object(handle))
}

fn initialize_kag_parser_object(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    parser: KagParser,
) -> Result<()> {
    runtime.add_object_class_info(handle, "KAGParser");
    install_kag_parser_methods(runtime, handle);
    runtime.host_mut().insert_kag_parser(handle, parser);
    refresh_kag_parser_members(runtime, handle)?;
    Ok(())
}

fn install_kag_parser_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_kag_native_method_preserving_script(runtime, handle, "finalize", kag_finalize);
    register_kag_vm_native_method_preserving_script(
        runtime,
        handle,
        "loadScenario",
        kag_load_scenario,
    );
    register_kag_vm_native_method_preserving_script(runtime, handle, "goToLabel", kag_go_to_label);
    register_kag_vm_native_method_preserving_script(runtime, handle, "callLabel", kag_call_label);
    register_kag_vm_native_method_preserving_script(
        runtime,
        handle,
        "getNextTag",
        kag_get_next_tag,
    );
    register_kag_vm_native_method_preserving_script(runtime, handle, "assign", kag_assign);
    register_kag_vm_native_method_preserving_script(runtime, handle, "clear", kag_clear);
    register_kag_vm_native_method_preserving_script(runtime, handle, "store", kag_store);
    register_kag_vm_native_method_preserving_script(runtime, handle, "restore", kag_restore);
    register_kag_vm_native_method_preserving_script(
        runtime,
        handle,
        "clearCallStack",
        kag_clear_call_stack,
    );
    register_kag_vm_native_method_preserving_script(runtime, handle, "interrupt", kag_interrupt);
    register_kag_vm_native_method_preserving_script(
        runtime,
        handle,
        "resetInterrupt",
        kag_reset_interrupt,
    );
    register_kag_vm_native_method_preserving_script(
        runtime,
        handle,
        "popMacroArgs",
        kag_pop_macro_args,
    );
}

fn register_kag_native_method_preserving_script<F>(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
    function: F,
) where
    F: NativeFunction<KrkrHost> + 'static,
{
    if !matches!(runtime.object_member(handle, name), Variant::Void) {
        return;
    }
    runtime.register_object_native(handle, name, function);
}

fn register_kag_vm_native_method_preserving_script<F>(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
    function: F,
) where
    F: VmNativeFunction<KrkrHost> + 'static,
{
    if !matches!(runtime.object_member(handle, name), Variant::Void) {
        return;
    }
    runtime.register_object_vm_native(handle, name, function);
}

fn kag_finalize(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
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
        true,
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
    with_parser(vm, this_obj, "KAGParser.goToLabel", true, |parser, _, _| {
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
    with_parser(vm, this_obj, "KAGParser.callLabel", true, |parser, _, _| {
        parser.call_label(&label).map_err(kag_to_tjs)?;
        Ok(Variant::Void)
    })
}

fn kag_get_next_tag(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(
        vm,
        this_obj,
        "KAGParser.getNextTag",
        true,
        |parser, vm, owner| {
            let mut host = TjsKagHost::new(vm, owner);
            let Some(tag) = parser.next_tag_with(&mut host).map_err(kag_to_tjs)? else {
                return Ok(Variant::Void);
            };
            Ok(Variant::Object(tag_to_dictionary(
                host.vm.runtime_mut(),
                &tag,
            )?))
        },
    )
}

fn kag_assign(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(source_handle) = args
        .first()
        .and_then(|value| kag_parser_object_handle(vm.runtime(), value))
    else {
        return Err(TjsError::runtime("KAGParser.assign requires a KAGParser"));
    };
    let source = vm
        .runtime()
        .host()
        .kag_parser(source_handle)
        .cloned()
        .ok_or_else(|| TjsError::runtime("KAGParser.assign requires a KAGParser"))?;
    with_parser(vm, this_obj, "KAGParser.assign", true, |parser, _, _| {
        parser.assign(&source);
        Ok(Variant::Void)
    })
}

fn kag_clear(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.clear", true, |parser, _, _| {
        parser.clear();
        Ok(Variant::Void)
    })
}

fn kag_store(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.store", false, |parser, vm, _| {
        let object = krkr2_parser_snapshot_to_object(vm.runtime_mut(), parser)?;
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
    with_parser(
        vm,
        this_obj,
        "KAGParser.restore",
        true,
        |parser, vm, handle| {
            let snapshot = snapshot_from_object(vm, handle, parser, snapshot_object)?;
            parser.restore(snapshot).map_err(kag_to_tjs)?;
            Ok(Variant::Void)
        },
    )
}

fn kag_clear_call_stack(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(
        vm,
        this_obj,
        "KAGParser.clearCallStack",
        true,
        |parser, _, _| {
            parser.clear_call_stack();
            Ok(Variant::Void)
        },
    )
}

fn kag_interrupt(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(vm, this_obj, "KAGParser.interrupt", true, |parser, _, _| {
        parser.interrupt();
        Ok(Variant::Void)
    })
}

fn kag_reset_interrupt(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(
        vm,
        this_obj,
        "KAGParser.resetInterrupt",
        true,
        |parser, _, _| {
            parser.reset_interrupt();
            Ok(Variant::Void)
        },
    )
}

fn kag_pop_macro_args(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_parser(
        vm,
        this_obj,
        "KAGParser.popMacroArgs",
        true,
        |parser, _, _| {
            parser.pop_macro_args().map_err(kag_to_tjs)?;
            Ok(Variant::Void)
        },
    )
}

fn krkr2_parser_snapshot_to_object(
    runtime: &mut Runtime<KrkrHost>,
    parser: &KagParser,
) -> Result<ObjectHandle> {
    let snapshot = parser.store();
    let object = alloc_dictionary(runtime);

    let macros = alloc_dictionary(runtime);
    for (name, source) in parser.macro_definitions() {
        runtime.set_object_member(macros, name, Variant::String(source.to_string()));
    }
    runtime.set_object_member(object, "macros", Variant::Object(macros));

    let macro_args = runtime.alloc_array_object(Vec::new());
    runtime.set_object_member(object, "macroArgs", Variant::Object(macro_args));

    let mut frames = Vec::new();
    for frame in parser.call_stack() {
        frames.push(Variant::Object(krkr2_call_frame_to_object(
            runtime, parser, frame,
        )?));
    }
    let call_stack = runtime.alloc_array_object(frames);
    runtime.set_object_member(object, "callStack", Variant::Object(call_stack));

    let storage = parser.cur_storage().unwrap_or_default();
    runtime.set_object_member(object, "storageName", Variant::String(storage.to_string()));
    runtime.set_object_member(
        object,
        "storageShortName",
        Variant::String(storage_short_name(storage)),
    );
    runtime.set_object_member(
        object,
        "curLine",
        Variant::Integer(parser.cur_line().unwrap_or(1).saturating_sub(1) as i64),
    );
    runtime.set_object_member(
        object,
        "curPos",
        Variant::Integer(parser.cur_pos().unwrap_or(0) as i64),
    );
    runtime.set_object_member(object, "lineBuffer", Variant::String(String::new()));
    runtime.set_object_member(object, "lineBufferUsing", Variant::Integer(0));
    runtime.set_object_member(
        object,
        "curLabel",
        Variant::String(parser.cur_label().unwrap_or_default().to_string()),
    );
    set_krkr2_condition_members(runtime, object, &snapshot.krkr2_condition_state());
    runtime.set_object_member(object, "macroArgStackBase", Variant::Integer(0));
    runtime.set_object_member(object, "macroArgStackDepth", Variant::Integer(0));

    Ok(object)
}

fn krkr2_call_frame_to_object(
    runtime: &mut Runtime<KrkrHost>,
    parser: &KagParser,
    frame: &CallFrame,
) -> Result<ObjectHandle> {
    let object = alloc_dictionary(runtime);
    let storage = frame.storage();
    let label = frame.current_label().unwrap_or_default();
    let (line, pos) = parser
        .line_pos_for_offset(storage, frame.offset())
        .unwrap_or((0, 0));
    let label_line = if label.is_empty() {
        0
    } else {
        parser.label_line(storage, label).unwrap_or(0)
    };

    runtime.set_object_member(object, "storage", Variant::String(storage.to_string()));
    runtime.set_object_member(object, "label", Variant::String(label.to_string()));
    runtime.set_object_member(
        object,
        "offset",
        Variant::Integer(line.saturating_sub(label_line) as i64),
    );
    runtime.set_object_member(
        object,
        "orgLineStr",
        Variant::String(frame.line_text().to_string()),
    );
    runtime.set_object_member(object, "lineBuffer", Variant::String(String::new()));
    runtime.set_object_member(object, "pos", Variant::Integer(pos as i64));
    runtime.set_object_member(object, "lineBufferUsing", Variant::Integer(0));
    runtime.set_object_member(object, "macroArgStackBase", Variant::Integer(0));
    runtime.set_object_member(object, "macroArgStackDepth", Variant::Integer(0));
    set_krkr2_condition_members(runtime, object, &frame.krkr2_condition_state());
    Ok(object)
}

fn set_krkr2_condition_members(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    state: &Krkr2ConditionState,
) {
    runtime.set_object_member(
        object,
        "ExcludeLevel",
        Variant::Integer(state.exclude_level),
    );
    runtime.set_object_member(object, "IfLevel", Variant::Integer(state.if_level));
    runtime.set_object_member(
        object,
        "ExcludeLevelStack",
        Variant::String(krkr2_int_stack_string(&state.exclude_level_stack)),
    );
    runtime.set_object_member(
        object,
        "IfLevelExecutedStack",
        Variant::String(krkr2_bool_stack_string(&state.if_level_executed_stack)),
    );
}

fn alloc_dictionary(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let object = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(object, "Dictionary");
    object
}

fn storage_short_name(storage: &str) -> String {
    storage
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(storage)
        .to_string()
}

fn krkr2_int_stack_string(values: &[i64]) -> String {
    let mut out = String::new();
    for value in values {
        out.push_str(&format!("{:08x}", *value as i32 as u32));
    }
    out
}

fn parse_krkr2_int_stack(value: &str) -> Vec<i64> {
    value
        .as_bytes()
        .chunks_exact(8)
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .filter_map(|chunk| u32::from_str_radix(chunk, 16).ok())
        .map(|value| value as i32 as i64)
        .collect()
}

fn krkr2_bool_stack_string(values: &[bool]) -> String {
    values
        .iter()
        .map(|value| if *value { '1' } else { '0' })
        .collect()
}

fn parse_krkr2_bool_stack(value: &str) -> Vec<bool> {
    value.chars().map(|ch| ch == '1').collect()
}

fn with_parser<F>(
    vm: &mut Vm<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    method: &str,
    mutation: bool,
    f: F,
) -> Result<Variant>
where
    F: FnOnce(&mut KagParser, &mut Vm<KrkrHost>, ObjectHandle) -> Result<Variant>,
{
    let handle = require_kag_this(this_obj, method)?;
    let handle = vm.runtime().bound_this(handle).unwrap_or(handle);
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
        if mutation && result.is_ok() {
            vm.runtime_mut().host_mut().mark_kag_parser_changed(handle);
        }
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

fn kag_parser_object_handle(runtime: &Runtime<KrkrHost>, value: &Variant) -> Option<ObjectHandle> {
    let handle = match value {
        Variant::Object(handle) => *handle,
        Variant::Closure(closure) => closure.object,
        _ => return None,
    };
    Some(runtime.bound_this(handle).unwrap_or(handle))
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

    if let Variant::Object(macros) = runtime.object_member(handle, "macros") {
        let definitions = runtime
            .object_members(macros)
            .into_iter()
            .filter_map(|(name, value)| match value {
                Variant::String(source) => Some((name, source)),
                _ => None,
            });
        parser.set_macro_definitions(definitions);
    }

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
    fn sync_parser_state(&mut self, parser: &KagParser) -> krkr_kag::Result<()> {
        let runtime = self.vm.runtime_mut();
        runtime
            .host_mut()
            .insert_kag_parser(self.owner, parser.clone());
        refresh_kag_parser_members_from_parser(runtime, self.owner, parser).map_err(kag_host_error)
    }

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
        let page = event
            .label
            .page_name
            .clone()
            .map(Variant::String)
            .unwrap_or(Variant::Void);
        self.call_event(
            "onLabel",
            vec![Variant::String(event.label.name.clone()), page],
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

    fn on_call_stack_depth(&mut self, depth: usize) -> krkr_kag::Result<()> {
        self.vm.runtime_mut().set_object_member(
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

fn snapshot_from_object(
    vm: &mut Vm<KrkrHost>,
    owner: ObjectHandle,
    parser: &mut KagParser,
    snapshot_object: ObjectHandle,
) -> Result<ParserSnapshot> {
    if let Variant::String(encoded) = vm.runtime().object_member(snapshot_object, "snapshot")
        && !encoded.is_empty()
    {
        let mut snapshot = ParserSnapshot::from_persistent_string(&encoded).map_err(kag_to_tjs)?;
        apply_snapshot_macros_from_object(vm.runtime(), parser, snapshot_object, &mut snapshot);
        ensure_snapshot_storages_loaded(vm, owner, parser, &snapshot)?;
        return Ok(snapshot);
    }

    snapshot_from_krkr2_object(vm, owner, parser, snapshot_object)
}

fn snapshot_from_krkr2_object(
    vm: &mut Vm<KrkrHost>,
    owner: ObjectHandle,
    parser: &mut KagParser,
    snapshot_object: ObjectHandle,
) -> Result<ParserSnapshot> {
    let storage = object_string(vm.runtime(), snapshot_object, "storageName")?;
    let current_storage = (!storage.is_empty()).then_some(storage);
    if let Some(storage) = current_storage.as_deref() {
        ensure_snapshot_storage_loaded(vm, owner, parser, storage)?;
    }

    let frame_objects = object_array_objects(vm.runtime(), snapshot_object, "callStack");
    for frame_object in &frame_objects {
        let storage = object_string(vm.runtime(), *frame_object, "storage")?;
        if !storage.is_empty() {
            ensure_snapshot_storage_loaded(vm, owner, parser, &storage)?;
        }
    }

    let current_label = object_string(vm.runtime(), snapshot_object, "curLabel")?;
    let current_label = (!current_label.is_empty()).then_some(current_label);
    let cursor_offset =
        krkr2_restore_cursor_offset(parser, current_storage.as_deref(), current_label.as_deref())?;
    let call_frames = frame_objects
        .into_iter()
        .map(|frame| krkr2_call_frame_from_object(vm.runtime(), parser, frame))
        .collect::<Result<Vec<_>>>()?;
    let macros = macros_from_snapshot_object(vm.runtime(), parser, snapshot_object);
    let condition = condition_state_from_object(vm.runtime(), snapshot_object)?;

    Ok(ParserSnapshot::from_krkr2_compatible(
        current_storage,
        cursor_offset,
        current_label,
        call_frames,
        macros,
        condition,
    ))
}

fn krkr2_restore_cursor_offset(
    parser: &KagParser,
    storage: Option<&str>,
    label: Option<&str>,
) -> Result<usize> {
    let Some(storage) = storage else {
        return Ok(0);
    };
    let Some(label) = label else {
        return Ok(0);
    };
    let line = parser.label_line(storage, label).ok_or_else(|| {
        kag_to_tjs(KagError::LabelNotFound {
            storage: storage.to_string(),
            label: label.to_string(),
        })
    })?;
    parser
        .offset_for_line_pos(storage, line, 0)
        .map_err(kag_to_tjs)
}

fn krkr2_call_frame_from_object(
    runtime: &Runtime<KrkrHost>,
    parser: &KagParser,
    frame: ObjectHandle,
) -> Result<Krkr2CallFrame> {
    let storage = object_string(runtime, frame, "storage")?;
    let label = object_string(runtime, frame, "label")?;
    let line_offset = object_usize(runtime, frame, "offset", 0)?;
    let pos = object_usize(runtime, frame, "pos", 0)?;
    let label_line = if label.is_empty() {
        0
    } else {
        parser.label_line(&storage, &label).unwrap_or(0)
    };
    let offset = parser
        .offset_for_line_pos(&storage, label_line.saturating_add(line_offset), pos)
        .map_err(kag_to_tjs)?;
    let mut line_text = object_string(runtime, frame, "orgLineStr")?;
    if line_text.is_empty() {
        line_text = parser
            .line_text_for_offset(&storage, offset)
            .unwrap_or_default()
            .to_string();
    }
    let label = (!label.is_empty()).then_some(label);
    let condition = condition_state_from_object(runtime, frame)?;
    Ok((storage, offset, line_text, label, condition))
}

fn macros_from_snapshot_object(
    runtime: &Runtime<KrkrHost>,
    parser: &KagParser,
    snapshot_object: ObjectHandle,
) -> Vec<(String, String)> {
    match runtime.object_member(snapshot_object, "macros") {
        Variant::Object(macros) => runtime
            .object_members(macros)
            .into_iter()
            .filter_map(|(name, value)| match value {
                Variant::String(source) => Some((name, source)),
                _ => None,
            })
            .collect(),
        Variant::Void => parser
            .macro_definitions()
            .map(|(name, source)| (name.to_string(), source.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

fn condition_state_from_object(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
) -> Result<Krkr2ConditionState> {
    let exclude_level = object_integer(runtime, object, "ExcludeLevel", -1)?;
    let exclude_level_stack =
        parse_krkr2_int_stack(&object_string(runtime, object, "ExcludeLevelStack")?);
    let if_level_executed_stack =
        parse_krkr2_bool_stack(&object_string(runtime, object, "IfLevelExecutedStack")?);
    let if_level = object_integer(
        runtime,
        object,
        "IfLevel",
        if_level_executed_stack.len() as i64,
    )?;
    Ok(Krkr2ConditionState {
        exclude_level,
        if_level,
        exclude_level_stack,
        if_level_executed_stack,
    })
}

fn object_string(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> Result<String> {
    match runtime.object_member(object, name) {
        Variant::Void => Ok(String::new()),
        value => value.to_tjs_string(),
    }
}

fn object_integer(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
    default: i64,
) -> Result<i64> {
    match runtime.object_member(object, name) {
        Variant::Void => Ok(default),
        value => value.to_integer(),
    }
}

fn object_usize(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
    default: usize,
) -> Result<usize> {
    let value = object_integer(runtime, object, name, default as i64)?;
    Ok(value.max(0) as usize)
}

fn object_array_objects(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Vec<ObjectHandle> {
    let Variant::Object(array) = runtime.object_member(object, name) else {
        return Vec::new();
    };
    runtime
        .array_elements(array)
        .unwrap_or_default()
        .iter()
        .filter_map(|value| match value {
            Variant::Object(object) => Some(*object),
            _ => None,
        })
        .collect()
}

fn apply_snapshot_macros_from_object(
    runtime: &Runtime<KrkrHost>,
    parser: &KagParser,
    snapshot_object: ObjectHandle,
    snapshot: &mut ParserSnapshot,
) {
    if !runtime.has_object_member(snapshot_object, "macros") {
        return;
    }

    match runtime.object_member(snapshot_object, "macros") {
        Variant::Object(macros) => {
            let definitions = runtime.object_members(macros).into_iter().filter_map(
                |(name, value)| match value {
                    Variant::String(source) => Some((name, source)),
                    _ => None,
                },
            );
            snapshot.set_macro_definitions(definitions);
        }
        Variant::Void => snapshot.set_macro_definitions(
            parser
                .macro_definitions()
                .map(|(name, source)| (name.to_string(), source.to_string())),
        ),
        _ => {}
    }
}

fn ensure_snapshot_storages_loaded(
    vm: &mut Vm<KrkrHost>,
    owner: ObjectHandle,
    parser: &mut KagParser,
    snapshot: &ParserSnapshot,
) -> Result<()> {
    for storage in snapshot.storage_names() {
        ensure_snapshot_storage_loaded(vm, owner, parser, storage)?;
    }
    Ok(())
}

fn ensure_snapshot_storage_loaded(
    vm: &mut Vm<KrkrHost>,
    owner: ObjectHandle,
    parser: &mut KagParser,
    storage: &str,
) -> Result<()> {
    if storage.is_empty() {
        return Ok(());
    }
    if parser.cur_storage() == Some(storage) {
        return Ok(());
    }
    match parser.set_cur_storage(storage.to_string()) {
        Ok(()) => Ok(()),
        Err(KagError::ScenarioNotLoaded { .. }) => {
            let mut host = TjsKagHost::new(vm, owner);
            parser
                .load_scenario_with(storage.to_string(), &mut host)
                .map_err(kag_to_tjs)
        }
        Err(error) => Err(kag_to_tjs(error)),
    }
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
