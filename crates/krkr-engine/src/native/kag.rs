use krkr_kag::{
    CallFrame, DebugLevel, KagError, KagHost, KagParser, LabelEvent, ParserSnapshot,
    ScenarioLoadEvent, ScriptEvent, Tag,
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
        let runtime = vm.runtime_mut();
        let object = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(object, "Dictionary");
        runtime.set_object_member(
            object,
            "snapshot",
            Variant::String(parser.store().to_persistent_string()),
        );
        let macros = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(macros, "Dictionary");
        for (name, source) in parser.macro_definitions() {
            runtime.set_object_member(macros, name, Variant::String(source.to_string()));
        }
        runtime.set_object_member(object, "macros", Variant::Object(macros));
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
    let Variant::String(encoded) = vm.runtime().object_member(snapshot_object, "snapshot") else {
        return Err(TjsError::runtime(
            "KAGParser.restore requires a persistent snapshot",
        ));
    };
    if encoded.is_empty() {
        return Err(TjsError::runtime(
            "KAGParser.restore requires a persistent snapshot",
        ));
    }

    let mut snapshot = ParserSnapshot::from_persistent_string(&encoded).map_err(kag_to_tjs)?;
    apply_snapshot_macros_from_object(vm.runtime(), parser, snapshot_object, &mut snapshot);
    ensure_snapshot_storages_loaded(vm, owner, parser, &snapshot)?;
    Ok(snapshot)
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
