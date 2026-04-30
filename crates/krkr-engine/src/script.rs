use krkr_tjs2::{
    Result, compile_source_to_bytecode,
    runtime::{Runtime, Variant},
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

pub(crate) fn execute_expression_on_runtime(
    runtime: &mut Runtime<KrkrHost>,
    source_name: &str,
    source: &str,
) -> Result<Variant> {
    let wrapped = format!("return ({source});");
    execute_script_on_runtime(runtime, source_name, &wrapped)
}
