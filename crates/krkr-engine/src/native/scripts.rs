use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, TjsHost, Variant},
};
use std::collections::BTreeSet;

use crate::{
    host::KrkrHost,
    script::{
        execute_bytecode_if_present_on_runtime, execute_expression_on_runtime,
        execute_expression_on_runtime_with_this, execute_script_on_runtime,
        execute_script_on_runtime_with_this,
    },
};

use super::{arg_string, install_static_object, native_void, required_arg_string};

pub(crate) fn install_scripts(runtime: &mut Runtime<KrkrHost>) {
    let scripts = install_static_object(runtime, "Scripts");
    runtime.register_object_native(scripts, "execStorage", scripts_exec_storage);
    runtime.register_object_native(scripts, "evalStorage", scripts_eval_storage);
    runtime.register_object_native(scripts, "loadDataPack", scripts_load_data_pack);
    runtime.register_object_native(scripts, "compileStorage", scripts_compile_storage);
    runtime.register_object_native(scripts, "exec", scripts_exec);
    runtime.register_object_native(scripts, "eval", scripts_eval);
    runtime.register_object_native(scripts, "dump", native_void);
    runtime.register_object_native(scripts, "getTraceString", scripts_get_trace_string);
    runtime.register_object_native(scripts, "dumpStringHeap", native_void);
    runtime.register_object_native(scripts, "setCallMissing", scripts_set_call_missing);
    runtime.register_object_native(scripts, "getClassNames", scripts_get_class_names);
    runtime.register_object_native(scripts, "getObjectKeys", scripts_get_object_keys);
    runtime.register_object_native(scripts, "getObjectCount", scripts_get_object_count);
    runtime.register_object_native(scripts, "foreach", scripts_foreach);
    runtime.register_object_native(scripts, "equalStruct", scripts_equal_struct);
    runtime.set_object_member(scripts, "pfMemberEnsure", Variant::Integer(0x0000_0200));
    runtime.set_object_member(scripts, "pfMemberMustExist", Variant::Integer(0x0000_0400));
    runtime.set_object_member(scripts, "pfIgnoreProp", Variant::Integer(0x0000_0800));
    runtime.set_object_member(scripts, "pfHiddenMember", Variant::Integer(0x0000_1000));
    runtime.set_object_member(scripts, "pfStaticMember", Variant::Integer(0x0001_0000));
    runtime.set_object_member(
        scripts,
        "textEncoding",
        Variant::String(runtime.host().text_encoding().to_string()),
    );
}

fn scripts_set_call_missing(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = required_arg_object(&args, 0, "Scripts.setCallMissing")?;
    runtime.set_object_call_missing(handle, "missing");
    Ok(Variant::Void)
}

fn scripts_exec_storage(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Scripts.execStorage")?;
    let mode = arg_string(&args, 1)?.unwrap_or_default();
    if let Some(value) = read_binary_struct_storage(runtime, &name, &mode)? {
        return Ok(value);
    }
    let bytes = runtime.host_mut().read_binary(&name, &mode)?;
    if let Some(value) = execute_bytecode_if_present_on_runtime(runtime, &name, &bytes)? {
        return Ok(value);
    }
    let source = runtime.host_mut().read_text(&name, &mode)?;
    execute_script_on_runtime(runtime, &name, &source)
}

fn scripts_eval_storage(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Scripts.evalStorage")?;
    let mode = arg_string(&args, 1)?.unwrap_or_default();
    if let Some(value) = read_binary_struct_storage(runtime, &name, &mode)? {
        return Ok(value);
    }
    let bytes = runtime.host_mut().read_binary(&name, &mode)?;
    if let Some(value) = execute_bytecode_if_present_on_runtime(runtime, &name, &bytes)? {
        normalize_kag_system_variable_struct(runtime, &name, &value);
        return Ok(value);
    }
    let source = runtime.host_mut().read_text(&name, &mode)?;
    let value = execute_expression_on_runtime(runtime, &name, &source)?;
    normalize_kag_system_variable_struct(runtime, &name, &value);
    Ok(value)
}

/// Loads Kirikiri's binary data-pack (`.pbd`) storage.
///
/// `PSDInfo.loadPBD` deliberately passes a storage *base* name here (unlike
/// `evalStorage`, which receives a complete storage name).  Keeping the
/// extension handling in this native entry point matches the KRKR API and is
/// important because ordinary storage lookup does not infer `.pbd`.
fn scripts_load_data_pack(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Scripts.loadDataPack")?;
    // The second `options` argument is used by some games to pass an outer IV
    // for encrypted packs.  Plain PBD packs, including the standard
    // PSDInfo-generated packs, are already a TJS binary struct and need no
    // transform before decoding.
    let storage_name = data_pack_storage_name(&name);
    let bytes = runtime.host_mut().read_binary(&storage_name, "")?;
    if bytes.starts_with(b"KBAD100\0") {
        return runtime.decode_binary_struct(&bytes)?.ok_or_else(|| {
            TjsError::runtime(format!(
                "Scripts.loadDataPack could not decode `{storage_name}`"
            ))
        });
    }
    if bytes.starts_with(b"TJS/ns0\0") || bytes.starts_with(b"TJS/4s0\0") {
        return runtime.decode_tjs_ns0(&bytes)?.ok_or_else(|| {
            TjsError::runtime(format!(
                "Scripts.loadDataPack could not decode `{storage_name}`"
            ))
        });
    }
    Err(TjsError::runtime(format!(
        "Scripts.loadDataPack expected a binary data pack in `{storage_name}`"
    )))
}

fn data_pack_storage_name(name: &str) -> String {
    if name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("pbd"))
    {
        name.to_string()
    } else {
        format!("{name}.pbd")
    }
}

fn read_binary_struct_storage(
    runtime: &mut Runtime<KrkrHost>,
    name: &str,
    mode: &str,
) -> Result<Option<Variant>> {
    let Ok(bytes) = runtime.host_mut().read_binary(name, mode) else {
        return Ok(None);
    };
    if bytes.starts_with(b"KBAD100\0") {
        return runtime.decode_binary_struct(&bytes);
    }
    if bytes.starts_with(b"TJS/ns0\0") || bytes.starts_with(b"TJS/4s0\0") {
        return runtime.decode_tjs_ns0(&bytes);
    }
    Ok(None)
}

fn normalize_kag_system_variable_struct(
    runtime: &mut Runtime<KrkrHost>,
    name: &str,
    value: &Variant,
) {
    if !name.ends_with("sc.ksd") {
        return;
    }
    let Variant::Object(scflags) = value else {
        return;
    };
    let Variant::Object(se_flags) = runtime.object_member(*scflags, "se") else {
        return;
    };
    let count = runtime
        .object_member(se_flags, "count")
        .to_integer()
        .unwrap_or(0)
        .max(0) as usize;
    let mut elements = Vec::with_capacity(count);
    for index in 0..count {
        elements.push(match runtime.object_member(se_flags, &index.to_string()) {
            Variant::Null => Variant::Void,
            value => value,
        });
    }
    while matches!(elements.last(), Some(Variant::Void)) {
        elements.pop();
    }
    let normalized = runtime.alloc_array_object(elements);
    runtime.set_object_member(*scflags, "se", Variant::Object(normalized));
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
    let context = match args.get(3) {
        Some(Variant::Object(handle)) => Some(*handle),
        _ => None,
    };
    execute_script_on_runtime_with_this(runtime, &name, &source, context)
}

fn scripts_eval(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let source = required_arg_string(&args, 0, "Scripts.eval")?;
    let name = arg_string(&args, 1)?.unwrap_or_else(|| "inline.tjs".to_string());
    let context = match args.get(3) {
        Some(Variant::Object(handle)) => Some(*handle),
        _ => None,
    };
    execute_expression_on_runtime_with_this(runtime, &name, &source, context)
        .or_else(|error| {
            // KAG3's `applyInlineStringVariableExtract` generates an
            // interpolated `@'...'` source string. Some translated scripts
            // feed it an unescaped apostrophe (for example "Let's"), even
            // though the same text is otherwise a valid literal. KRKR's
            // dynamic evaluator accepts this common generated form; retry it
            // with an equivalent double-quoted interpolated delimiter only
            // after the original source failed to parse.
            let Some(rewritten) = retry_interpolated_single_quote_source(&source) else {
                return Err(error);
            };
            execute_expression_on_runtime_with_this(runtime, &name, &rewritten, context)
                .map_err(|_| error)
        })
        .map_err(|error| {
            TjsError::runtime(format!(
                "Scripts.eval failed for source `{}`: {error}",
                preview_source(&source)
            ))
        })
}

fn retry_interpolated_single_quote_source(source: &str) -> Option<String> {
    let body = source.strip_prefix("@'")?.strip_suffix('\'')?;
    let mut rewritten = String::with_capacity(source.len() + 2);
    rewritten.push_str("@\"");
    let mut escaped = false;
    for ch in body.chars() {
        if ch == '"' && !escaped {
            rewritten.push('\\');
        }
        rewritten.push(ch);
        if ch == '\\' {
            escaped = !escaped;
        } else {
            escaped = false;
        }
    }
    rewritten.push('"');
    Some(rewritten)
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

fn scripts_foreach(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(collection) = args.first().cloned() else {
        return Ok(Variant::Void);
    };
    if matches!(collection, Variant::Void | Variant::Null) {
        return Ok(Variant::Void);
    }
    let Some(func) = args.get(1).cloned() else {
        return Ok(Variant::Void);
    };
    let entries = match &collection {
        Variant::Object(handle) => {
            if let Some(items) = runtime.array_elements(*handle) {
                items
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, value)| (Variant::Integer(index as i64), value))
                    .collect::<Vec<_>>()
            } else {
                runtime
                    .object_members(*handle)
                    .into_iter()
                    .filter(|(key, _)| !is_hidden_member_name(key))
                    .map(|(key, value)| (Variant::String(key), value))
                    .collect()
            }
        }
        _ => Vec::new(),
    };
    for (key, value) in entries {
        let mut call_args = vec![key, value];
        call_args.extend(args.iter().skip(2).cloned());
        let result = runtime.call_function(func.clone(), call_args)?;
        if !matches!(result, Variant::Void) {
            return Ok(result);
        }
    }
    Ok(Variant::Void)
}

fn scripts_equal_struct(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let lhs = args.first().cloned().unwrap_or(Variant::Void);
    let rhs = args.get(1).cloned().unwrap_or(Variant::Void);
    let mut compared = BTreeSet::new();
    Ok(Variant::Integer(i64::from(equal_struct(
        runtime,
        &lhs,
        &rhs,
        &mut compared,
    ))))
}

fn equal_struct(
    runtime: &Runtime<KrkrHost>,
    lhs: &Variant,
    rhs: &Variant,
    compared: &mut BTreeSet<(ObjectHandle, ObjectHandle)>,
) -> bool {
    let (Some(lhs_handle), Some(rhs_handle)) =
        (variant_object_handle(lhs), variant_object_handle(rhs))
    else {
        return lhs.discern_eq(rhs);
    };
    if lhs_handle == rhs_handle {
        return true;
    }

    let lhs_array = runtime.array_elements(lhs_handle);
    let rhs_array = runtime.array_elements(rhs_handle);
    if let (Some(lhs_items), Some(rhs_items)) = (lhs_array, rhs_array) {
        if lhs_items.len() != rhs_items.len() {
            return false;
        }
        if !compared.insert((lhs_handle, rhs_handle)) {
            return true;
        }
        let lhs_items = lhs_items.to_vec();
        let rhs_items = rhs_items.to_vec();
        return lhs_items
            .iter()
            .zip(&rhs_items)
            .all(|(lhs, rhs)| equal_struct(runtime, lhs, rhs, compared));
    }
    if lhs_array.is_some() || rhs_array.is_some() {
        return false;
    }

    let lhs_dictionary = is_dictionary(runtime, lhs_handle);
    let rhs_dictionary = is_dictionary(runtime, rhs_handle);
    if lhs_dictionary && rhs_dictionary {
        if !compared.insert((lhs_handle, rhs_handle)) {
            return true;
        }
        let lhs_entries = struct_members(runtime, lhs_handle);
        let rhs_entries = struct_members(runtime, rhs_handle);
        return lhs_entries.len() == rhs_entries.len()
            && lhs_entries.iter().zip(&rhs_entries).all(
                |((lhs_key, lhs_value), (rhs_key, rhs_value))| {
                    lhs_key == rhs_key && equal_struct(runtime, lhs_value, rhs_value, compared)
                },
            );
    }

    false
}

fn variant_object_handle(value: &Variant) -> Option<ObjectHandle> {
    match value {
        Variant::Object(handle) => Some(*handle),
        Variant::Closure(closure) => Some(closure.object),
        _ => None,
    }
}

fn is_dictionary(runtime: &Runtime<KrkrHost>, handle: ObjectHandle) -> bool {
    runtime
        .object_class_infos(handle)
        .iter()
        .any(|name| name == "Dictionary")
}

fn struct_members(runtime: &Runtime<KrkrHost>, handle: ObjectHandle) -> Vec<(String, Variant)> {
    runtime
        .object_members(handle)
        .into_iter()
        .filter(|(key, _)| !is_hidden_member_name(key))
        .collect()
}

fn scripts_get_object_keys(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = required_arg_object(&args, 0, "Scripts.getObjectKeys")?;
    let mut keys = runtime
        .object_members(handle)
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| !is_hidden_member_name(key))
        .collect::<Vec<_>>();
    keys.sort();
    let values = keys.into_iter().map(Variant::String).collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

fn scripts_get_object_count(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = required_arg_object(&args, 0, "Scripts.getObjectCount")?;
    let count = runtime
        .object_members(handle)
        .into_iter()
        .filter(|(key, _)| !is_hidden_member_name(key))
        .count();
    Ok(Variant::Integer(count as i64))
}

fn required_arg_object(args: &[Variant], index: usize, method: &str) -> Result<ObjectHandle> {
    match args.get(index) {
        Some(Variant::Object(handle)) => Ok(*handle),
        Some(Variant::Closure(closure)) => Ok(closure.object),
        Some(Variant::Null) => Err(TjsError::runtime(format!("{method} got null object"))),
        Some(other) => Err(TjsError::runtime(format!(
            "{method} requires object argument {index}, got {}",
            other.type_name()
        ))),
        None => Err(TjsError::runtime(format!(
            "{method} requires argument {index}"
        ))),
    }
}

fn is_hidden_member_name(key: &str) -> bool {
    key.starts_with("__")
        || matches!(
            key,
            "clear"
                | "assign"
                | "assignStruct"
                | "saveStruct"
                | "loadStruct"
                | "load"
                | "save"
                | "add"
                | "push"
                | "split"
                | "insert"
                | "erase"
                | "remove"
                | "pop"
                | "shift"
                | "unshift"
                | "join"
                | "sort"
                | "reverse"
                | "find"
                | "count"
                | "length"
        )
}

fn preview_source(source: &str) -> String {
    let mut preview = String::new();
    for ch in source.chars().take(80) {
        match ch {
            '\n' => preview.push_str("\\n"),
            '\r' => preview.push_str("\\r"),
            '\t' => preview.push_str("\\t"),
            _ => preview.push(ch),
        }
    }
    if source.chars().count() > 80 {
        preview.push_str("...");
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::retry_interpolated_single_quote_source;

    #[test]
    fn retries_only_standalone_single_quoted_interpolated_source() {
        assert_eq!(
            retry_interpolated_single_quote_source("@'\"Let's go\"'"),
            Some("@\"\\\"Let's go\\\"\"".to_string())
        );
        assert_eq!(retry_interpolated_single_quote_source("value + 1"), None);
    }
}
