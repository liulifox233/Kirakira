use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, TjsHost, Variant},
};

use crate::{
    host::KrkrHost,
    script::{
        execute_bytecode_if_present_on_runtime, execute_expression_on_runtime,
        execute_script_on_runtime,
    },
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
    runtime.register_object_native(scripts, "setCallMissing", scripts_set_call_missing);
    runtime.register_object_native(scripts, "getClassNames", scripts_get_class_names);
    runtime.register_object_native(scripts, "getObjectKeys", scripts_get_object_keys);
    runtime.register_object_native(scripts, "getObjectCount", scripts_get_object_count);
    runtime.register_object_native(scripts, "foreach", scripts_foreach);
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
    execute_script_on_runtime(runtime, &name, &source)
}

fn scripts_eval(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let source = required_arg_string(&args, 0, "Scripts.eval")?;
    let name = arg_string(&args, 1)?.unwrap_or_else(|| "inline.tjs".to_string());
    execute_expression_on_runtime(runtime, &name, &source).map_err(|error| {
        TjsError::runtime(format!(
            "Scripts.eval failed for source `{}`: {error}",
            preview_source(&source)
        ))
    })
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
    // Mirrors the environment helper: clones the collection like
    // `new Array().assign(collection)` and invokes
    // func(elements[i], elements[i + 1], collection, *extra) for each pair.
    let Some(collection) = args.first().cloned() else {
        return Ok(Variant::Void);
    };
    if matches!(collection, Variant::Void | Variant::Null) {
        return Ok(Variant::Void);
    }
    let Some(func) = args.get(1).cloned() else {
        return Ok(Variant::Void);
    };
    let elements = match &collection {
        Variant::Object(handle) => {
            if let Some(items) = runtime.array_elements(*handle) {
                items.to_vec()
            } else {
                // krkrz dictionaries only carry user members on the instance;
                // the injected helper methods are not iterated.
                runtime
                    .object_members(*handle)
                    .into_iter()
                    .filter(|(key, _)| !is_hidden_member_name(key))
                    .flat_map(|(key, value)| [Variant::String(key), value])
                    .collect()
            }
        }
        _ => Vec::new(),
    };
    let count = elements.len();
    let mut index = 0;
    while index < count {
        let mut call_args = vec![
            elements[index].clone(),
            elements.get(index + 1).cloned().unwrap_or_default(),
            collection.clone(),
        ];
        call_args.extend(args.iter().skip(2).cloned());
        runtime.call_function(func.clone(), call_args)?;
        index += 2;
    }
    Ok(Variant::Void)
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
