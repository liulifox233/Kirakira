use krkr_tjs2::{
    Result, TjsStackFrame,
    bytecode::BYTECODE_SIGNATURE,
    compile_source_to_bytecode,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

pub(crate) fn execute_script_on_runtime(
    runtime: &mut Runtime<KrkrHost>,
    source_name: &str,
    source: &str,
) -> Result<Variant> {
    let file = compile_source_to_bytecode(source_name, source)?;
    runtime.execute_file(&file)
}

pub(crate) fn execute_script_on_runtime_with_this(
    runtime: &mut Runtime<KrkrHost>,
    source_name: &str,
    source: &str,
    this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    let file = compile_source_to_bytecode(source_name, source)?;
    runtime.execute_file_with_this(&file, this_obj)
}

pub(crate) fn execute_bytecode_if_present_on_runtime(
    runtime: &mut Runtime<KrkrHost>,
    source_name: &str,
    bytes: &[u8],
) -> Result<Option<Variant>> {
    if bytes.starts_with(&BYTECODE_SIGNATURE) {
        return runtime.execute_bytecode(bytes).map(Some).map_err(|error| {
            error.with_stack_frame(TjsStackFrame {
                storage: Some(source_name.to_string()),
                object_name: "<bytecode load>".to_string(),
                context: "Bytecode".to_string(),
                bytecode_offset: 0,
                source: None,
            })
        });
    }
    let _ = source_name;
    Ok(None)
}

pub(crate) fn execute_expression_on_runtime(
    runtime: &mut Runtime<KrkrHost>,
    source_name: &str,
    source: &str,
) -> Result<Variant> {
    if source.trim().is_empty() {
        return Ok(Variant::Void);
    }
    let wrapped = format!("return ({source});");
    execute_script_on_runtime(runtime, source_name, &wrapped)
}

pub(crate) fn execute_expression_on_runtime_with_this(
    runtime: &mut Runtime<KrkrHost>,
    source_name: &str,
    source: &str,
    this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    if source.trim().is_empty() {
        return Ok(Variant::Void);
    }
    let wrapped = format!("return ({source});");
    execute_script_on_runtime_with_this(runtime, source_name, &wrapped, this_obj)
}
