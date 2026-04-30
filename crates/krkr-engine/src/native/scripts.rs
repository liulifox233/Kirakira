use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::{
    host::KrkrHost,
    script::{execute_expression_on_runtime, execute_script_on_runtime},
};

use super::{arg_string, install_static_object, native_void, required_arg_string};

pub(crate) fn install_scripts(runtime: &mut Runtime<KrkrHost>) {
    let scripts = install_static_object(runtime, "Scripts");
    runtime.register_object_native(scripts, "execStorage", scripts_exec_storage);
    runtime.register_object_native(scripts, "evalStorage", scripts_eval_storage);
    runtime.register_object_native(scripts, "compileStorage", scripts_compile_storage);
    runtime.register_object_native(scripts, "exec", scripts_exec);
    runtime.register_object_native(scripts, "eval", scripts_eval);
    runtime.register_object_native(scripts, "dump", native_void);
    runtime.register_object_native(scripts, "getTraceString", scripts_get_trace_string);
    runtime.register_object_native(scripts, "dumpStringHeap", native_void);
    runtime.register_object_native(scripts, "setCallMissing", native_void);
    runtime.register_object_native(scripts, "getClassNames", scripts_get_class_names);
    runtime.set_object_member(
        scripts,
        "textEncoding",
        Variant::String(runtime.host().text_encoding().to_string()),
    );
}

fn scripts_exec_storage(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Scripts.execStorage")?;
    let source = runtime.host().read_text_storage(&name)?;
    execute_script_on_runtime(runtime, &name, &source)
}

fn scripts_eval_storage(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Scripts.evalStorage")?;
    let source = runtime.host().read_text_storage(&name)?;
    execute_expression_on_runtime(runtime, &name, &source)
}

fn scripts_compile_storage(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Err(TjsError::runtime(
        "Scripts.compileStorage is registered but bytecode writing is not implemented yet",
    ))
}

fn scripts_exec(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let source = required_arg_string(&args, 0, "Scripts.exec")?;
    let name = arg_string(&args, 1)?.unwrap_or_else(|| "inline.tjs".to_string());
    execute_script_on_runtime(runtime, &name, &source)
}

fn scripts_eval(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let source = required_arg_string(&args, 0, "Scripts.eval")?;
    let name = arg_string(&args, 1)?.unwrap_or_else(|| "inline.tjs".to_string());
    execute_expression_on_runtime(runtime, &name, &source)
}

fn scripts_get_trace_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(String::new()))
}

fn scripts_get_class_names(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(Variant::Object(handle)) = args.first().cloned() else {
        return Ok(Variant::Void);
    };
    let values = runtime
        .object_class_infos(handle)
        .iter()
        .cloned()
        .map(Variant::String)
        .collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}
