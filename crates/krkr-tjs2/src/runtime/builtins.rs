use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::compile_source_to_bytecode;
use crate::error::{Result, TjsError};
use crate::runtime::object::Object;
use crate::runtime::value::{ObjectHandle, Variant};
use crate::runtime::{Runtime, TjsHost};

pub(crate) fn install<H: TjsHost + 'static>(runtime: &mut Runtime<H>) {
    let array = runtime.register_global_native("Array", native_array::<H>);
    install_array_methods(runtime, array);
    let dictionary = runtime.register_global_native("Dictionary", native_dictionary::<H>);
    install_dictionary_methods(runtime, dictionary);
    runtime.register_global_native("RegExp", native_regexp::<H>);
    runtime.register_global_native("Date", native_date::<H>);
    runtime.register_global_native("Exception", native_exception::<H>);
    install_math(runtime);
}

fn native_array<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = runtime.alloc_object(Object::array(args));
    install_array_methods(runtime, handle);
    Ok(Variant::Object(handle))
}

fn native_dictionary<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = runtime.alloc_object(Object::default());
    install_dictionary_methods(runtime, handle);
    Ok(Variant::Object(handle))
}

fn native_regexp<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = runtime.alloc_object(Object::default());
    if let Some(pattern) = args.first() {
        runtime.heap[handle.0].set("pattern", pattern.clone());
    } else {
        runtime.heap[handle.0].set("pattern", Variant::String(String::new()));
    }
    if let Some(flags) = args.get(1) {
        runtime.heap[handle.0].set("flags", flags.clone());
    } else {
        runtime.heap[handle.0].set("flags", Variant::String(String::new()));
    }
    runtime.register_object_native(handle, "compile", regexp_compile::<H>);
    runtime.register_object_native(handle, "_compile", regexp_compile::<H>);
    runtime.register_object_native(handle, "test", regexp_test::<H>);
    runtime.register_object_native(handle, "match", regexp_match::<H>);
    runtime.register_object_native(handle, "exec", regexp_match::<H>);
    Ok(Variant::Object(handle))
}

fn native_date<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let timestamp = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or_else(|| runtime.host_mut().now_millis());
    let handle = runtime.alloc_object(Object::default());
    runtime.heap[handle.0].set("timestamp", Variant::Integer(timestamp));
    runtime.register_object_native(handle, "getTime", date_get_time::<H>);
    runtime.register_object_native(handle, "setTime", date_set_time::<H>);
    runtime.register_object_native(handle, "getTimezoneOffset", date_zero::<H>);
    runtime.register_object_native(handle, "getYear", date_zero::<H>);
    runtime.register_object_native(handle, "getMonth", date_zero::<H>);
    runtime.register_object_native(handle, "getDate", date_zero::<H>);
    runtime.register_object_native(handle, "getDay", date_zero::<H>);
    runtime.register_object_native(handle, "getHours", date_zero::<H>);
    runtime.register_object_native(handle, "getMinutes", date_zero::<H>);
    runtime.register_object_native(handle, "getSeconds", date_zero::<H>);
    runtime.register_object_native(handle, "parse", date_parse::<H>);
    Ok(Variant::Object(handle))
}

fn native_exception<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = runtime.alloc_object(Object::default());
    let message = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let trace = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    runtime.heap[handle.0].set("message", Variant::String(message));
    runtime.heap[handle.0].set("trace", Variant::String(trace));
    Ok(Variant::Object(handle))
}

fn install_math<H: TjsHost + 'static>(runtime: &mut Runtime<H>) {
    let math = runtime.alloc_object(Object::default());
    runtime.set_global_member("Math", Variant::Object(math));

    for (name, value) in [
        ("E", std::f64::consts::E),
        ("LOG2E", std::f64::consts::LOG2_E),
        ("LOG10E", std::f64::consts::LOG10_E),
        ("LN10", std::f64::consts::LN_10),
        ("LN2", std::f64::consts::LN_2),
        ("PI", std::f64::consts::PI),
        ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
        ("SQRT2", std::f64::consts::SQRT_2),
    ] {
        runtime.heap[math.0].set(name, Variant::Real(value));
    }

    runtime.register_object_native(math, "abs", math_unary::<H, { MathUnary::Abs as u8 }>);
    runtime.register_object_native(math, "acos", math_unary::<H, { MathUnary::Acos as u8 }>);
    runtime.register_object_native(math, "asin", math_unary::<H, { MathUnary::Asin as u8 }>);
    runtime.register_object_native(math, "atan", math_unary::<H, { MathUnary::Atan as u8 }>);
    runtime.register_object_native(math, "ceil", math_unary::<H, { MathUnary::Ceil as u8 }>);
    runtime.register_object_native(math, "exp", math_unary::<H, { MathUnary::Exp as u8 }>);
    runtime.register_object_native(math, "floor", math_unary::<H, { MathUnary::Floor as u8 }>);
    runtime.register_object_native(math, "log", math_unary::<H, { MathUnary::Log as u8 }>);
    runtime.register_object_native(math, "round", math_unary::<H, { MathUnary::Round as u8 }>);
    runtime.register_object_native(math, "sin", math_unary::<H, { MathUnary::Sin as u8 }>);
    runtime.register_object_native(math, "cos", math_unary::<H, { MathUnary::Cos as u8 }>);
    runtime.register_object_native(math, "sqrt", math_unary::<H, { MathUnary::Sqrt as u8 }>);
    runtime.register_object_native(math, "tan", math_unary::<H, { MathUnary::Tan as u8 }>);
    runtime.register_object_native(math, "atan2", math_atan2::<H>);
    runtime.register_object_native(math, "pow", math_pow::<H>);
    runtime.register_object_native(math, "max", math_max::<H>);
    runtime.register_object_native(math, "min", math_min::<H>);

    let random_state = AtomicU64::new(0x6d2b_79f5_aa55_1234);
    runtime.register_object_native(
        math,
        "random",
        move |_runtime: &mut Runtime<H>, _this_obj, _args| {
            let next = random_state
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, next_random_state)
                .unwrap_or_else(|value| value);
            Ok(Variant::Real(random_unit(next)))
        },
    );

    let random_generator =
        runtime.register_object_native(math, "RandomGenerator", random_generator::<H>);
    runtime.add_object_class_info(random_generator, "RandomGenerator");
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum MathUnary {
    Abs,
    Acos,
    Asin,
    Atan,
    Ceil,
    Exp,
    Floor,
    Log,
    Round,
    Sin,
    Cos,
    Sqrt,
    Tan,
}

fn math_unary<H: TjsHost + 'static, const OP: u8>(
    _runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let input = args
        .first()
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    let value = match OP {
        value if value == MathUnary::Abs as u8 => input.abs(),
        value if value == MathUnary::Acos as u8 => input.acos(),
        value if value == MathUnary::Asin as u8 => input.asin(),
        value if value == MathUnary::Atan as u8 => input.atan(),
        value if value == MathUnary::Ceil as u8 => input.ceil(),
        value if value == MathUnary::Exp as u8 => input.exp(),
        value if value == MathUnary::Floor as u8 => input.floor(),
        value if value == MathUnary::Log as u8 => input.ln(),
        value if value == MathUnary::Round as u8 => input.round(),
        value if value == MathUnary::Sin as u8 => input.sin(),
        value if value == MathUnary::Cos as u8 => input.cos(),
        value if value == MathUnary::Sqrt as u8 => input.sqrt(),
        value if value == MathUnary::Tan as u8 => input.tan(),
        _ => unreachable!("known unary math operation"),
    };
    Ok(Variant::Real(value))
}

fn math_atan2<H: TjsHost + 'static>(
    _runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let y = args
        .first()
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    let x = args
        .get(1)
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    Ok(Variant::Real(y.atan2(x)))
}

fn math_pow<H: TjsHost + 'static>(
    _runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let base = args
        .first()
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    let exponent = args
        .get(1)
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    Ok(Variant::Real(base.powf(exponent)))
}

fn math_max<H: TjsHost + 'static>(
    _runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let mut max = f64::NEG_INFINITY;
    for arg in args {
        max = max.max(arg.to_real()?);
    }
    Ok(Variant::Real(max))
}

fn math_min<H: TjsHost + 'static>(
    _runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let mut min = f64::INFINITY;
    for arg in args {
        min = min.min(arg.to_real()?);
    }
    Ok(Variant::Real(min))
}

fn random_generator<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let seed = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or_else(|| runtime.host_mut().now_millis()) as u64;
    let handle = runtime.alloc_object(Object::default());
    runtime.heap[handle.0].set("state", Variant::Integer(seed as i64));
    runtime.register_object_native(handle, "random", random_generator_random::<H>);
    runtime.register_object_native(handle, "randomize", random_generator_randomize::<H>);
    runtime.register_object_native(handle, "random32", random_generator_random32::<H>);
    runtime.register_object_native(handle, "random63", random_generator_random63::<H>);
    runtime.register_object_native(handle, "random64", random_generator_random64::<H>);
    runtime.register_object_native(handle, "serialize", random_generator_serialize::<H>);
    Ok(Variant::Object(handle))
}

pub(crate) fn install_array_methods<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    handle: ObjectHandle,
) {
    runtime.add_object_class_info(handle, "Array");
    runtime.register_object_native(handle, "add", array_push::<H>);
    runtime.register_object_native(handle, "push", array_push::<H>);
    runtime.register_object_native(handle, "insert", array_insert::<H>);
    runtime.register_object_native(handle, "erase", array_erase::<H>);
    runtime.register_object_native(handle, "pop", array_pop::<H>);
    runtime.register_object_native(handle, "clear", array_clear::<H>);
    runtime.register_object_native(handle, "assign", array_assign::<H>);
    runtime.register_object_native(handle, "assignStruct", array_assign_struct::<H>);
    runtime.register_object_native(handle, "load", array_load::<H>);
    runtime.register_object_native(handle, "save", array_save::<H>);
    runtime.register_object_native(handle, "saveStruct", array_save_struct::<H>);
    runtime.register_object_native(handle, "loadStruct", array_load_struct::<H>);
    runtime.register_object_native(handle, "join", array_join::<H>);
    runtime.register_object_native(handle, "reverse", array_reverse::<H>);
}

fn install_dictionary_methods<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    handle: ObjectHandle,
) {
    runtime.add_object_class_info(handle, "Dictionary");
    runtime.register_object_native(handle, "clear", dictionary_clear::<H>);
    runtime.register_object_native(handle, "assign", dictionary_assign::<H>);
    runtime.register_object_native(handle, "assignStruct", dictionary_assign_struct::<H>);
    runtime.register_object_native(handle, "saveStruct", dictionary_save_struct::<H>);
    runtime.register_object_native(handle, "loadStruct", dictionary_load_struct::<H>);
}

fn require_this(this_obj: Option<ObjectHandle>, name: &str) -> Result<ObjectHandle> {
    this_obj.ok_or_else(|| TjsError::runtime(format!("{name} requires an object instance")))
}

fn array_push<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.add")?;
    for value in args {
        if !runtime.heap[handle.0].array_push(value) {
            return Err(TjsError::runtime("Array.add called on a non-array object"));
        }
    }
    Ok(Variant::Integer(
        runtime.heap[handle.0]
            .array_elements()
            .map(|items| items.len() as i64)
            .unwrap_or(0),
    ))
}

fn array_insert<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.insert")?;
    let Some(index) = args.first() else {
        return Ok(Variant::Void);
    };
    let len = runtime.heap[handle.0]
        .array_elements()
        .map(|items| items.len())
        .ok_or_else(|| TjsError::runtime("Array.insert called on a non-array object"))?;
    let index = index.to_integer()?.clamp(0, len as i64) as usize;
    for (offset, value) in args.into_iter().skip(1).enumerate() {
        if !runtime.heap[handle.0].array_insert(index + offset, value) {
            return Err(TjsError::runtime(
                "Array.insert called on a non-array object",
            ));
        }
    }
    Ok(Variant::Void)
}

fn array_erase<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.erase")?;
    let Some(index) = args.first() else {
        return Ok(Variant::Void);
    };
    let raw_index = index.to_integer()?;
    let len = runtime.heap[handle.0]
        .array_elements()
        .map(|items| items.len())
        .ok_or_else(|| TjsError::runtime("Array.erase called on a non-array object"))?;
    let index = if raw_index < 0 {
        len as i64 + raw_index
    } else {
        raw_index
    };
    if index < 0 || index >= len as i64 {
        return Err(TjsError::runtime("Array.erase index out of range"));
    }
    runtime.heap[handle.0]
        .array_erase(index as usize)
        .ok_or_else(|| TjsError::runtime("Array.erase called on a non-array object"))?;
    Ok(Variant::Void)
}

fn array_pop<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.pop")?;
    runtime.heap[handle.0]
        .array_pop()
        .ok_or_else(|| TjsError::runtime("Array.pop called on a non-array object"))
}

fn array_clear<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.clear")?;
    if !runtime.heap[handle.0].array_clear() {
        return Err(TjsError::runtime(
            "Array.clear called on a non-array object",
        ));
    }
    Ok(Variant::Void)
}

fn array_assign<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = require_this(this_obj, "Array.assign")?;
    let Some(Variant::Object(src)) = args.first().cloned() else {
        return Ok(Variant::Object(dest));
    };

    if !runtime.heap[dest.0].array_clear() {
        return Err(TjsError::runtime(
            "Array.assign called on a non-array object",
        ));
    }

    if let Some(elements) = runtime.heap[src.0].array_elements().map(Vec::from) {
        for value in elements {
            runtime.heap[dest.0].array_push(value);
        }
    } else {
        for (key, value) in runtime.heap[src.0].members.clone() {
            runtime.heap[dest.0].array_push(Variant::String(key));
            runtime.heap[dest.0].array_push(value);
        }
    }
    Ok(Variant::Object(dest))
}

fn array_assign_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = require_this(this_obj, "Array.assignStruct")?;
    let Some(Variant::Object(src)) = args.first().cloned() else {
        return Ok(Variant::Object(dest));
    };
    assign_array_struct(runtime, dest, src)?;
    Ok(Variant::Object(dest))
}

fn array_load<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.load")?;
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Ok(Variant::Integer(0));
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let Ok(text) = runtime.host_mut().read_text(&path, &mode) else {
        return Ok(Variant::Integer(0));
    };
    runtime.heap[handle.0] = Object::array(
        text.lines()
            .map(|line| Variant::String(line.to_string()))
            .collect(),
    );
    install_array_methods(runtime, handle);
    Ok(Variant::Integer(1))
}

fn array_save<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.save")?;
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Ok(Variant::Void);
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let elements = runtime.heap[handle.0]
        .array_elements()
        .ok_or_else(|| TjsError::runtime("Array.save called on a non-array object"))?;
    let lines = elements
        .iter()
        .map(Variant::to_tjs_string)
        .collect::<Result<Vec<_>>>()?
        .join("\n");
    runtime.host_mut().write_text(&path, &mode, &lines)?;
    Ok(Variant::Object(handle))
}

fn array_save_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.saveStruct")?;
    save_structured_value(runtime, handle, &args)?;
    Ok(Variant::Object(handle))
}

fn array_load_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.loadStruct")?;
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Ok(Variant::Integer(0));
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    if mode.contains('b') {
        let Ok(bytes) = runtime.host_mut().read_binary(&path, &mode) else {
            return Ok(Variant::Integer(0));
        };
        let Some(Variant::Object(src)) = decode_binary_struct(runtime, &bytes)? else {
            return Ok(Variant::Integer(0));
        };
        assign_array_struct(runtime, handle, src)?;
        return Ok(Variant::Integer(1));
    }
    let Ok(text) = runtime.host_mut().read_text(&path, &mode) else {
        return Ok(Variant::Integer(0));
    };
    let wrapped = format!("return ({text});");
    if let Ok(Variant::Object(src)) =
        compile_source_to_bytecode(&path, &wrapped).and_then(|file| runtime.execute_file(&file))
    {
        assign_array_struct(runtime, handle, src)?;
        return Ok(Variant::Integer(1));
    }
    Ok(Variant::Integer(0))
}

fn array_join<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.join")?;
    let separator = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_else(|| ",".to_string());
    let elements = runtime.heap[handle.0]
        .array_elements()
        .ok_or_else(|| TjsError::runtime("Array.join called on a non-array object"))?;
    let parts = elements
        .iter()
        .map(Variant::to_tjs_string)
        .collect::<Result<Vec<_>>>()?;
    Ok(Variant::String(parts.join(&separator)))
}

fn array_reverse<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.reverse")?;
    let elements = runtime.heap[handle.0]
        .array_elements()
        .ok_or_else(|| TjsError::runtime("Array.reverse called on a non-array object"))?
        .iter()
        .cloned()
        .rev()
        .collect::<Vec<_>>();
    runtime.heap[handle.0] = Object::array(elements);
    install_array_methods(runtime, handle);
    Ok(Variant::Object(handle))
}

fn dictionary_clear<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Dictionary.clear")?;
    runtime.heap[handle.0].members.clear();
    install_dictionary_methods(runtime, handle);
    Ok(Variant::Void)
}

fn dictionary_assign<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = require_this(this_obj, "Dictionary.assign")?;
    let Some(Variant::Object(src)) = args.first().cloned() else {
        return Ok(Variant::Object(dest));
    };
    for (key, value) in runtime.heap[src.0].members.clone() {
        runtime.heap[dest.0].set(key, value);
    }
    Ok(Variant::Object(dest))
}

fn dictionary_assign_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = require_this(this_obj, "Dictionary.assignStruct")?;
    let Some(Variant::Object(src)) = args.first().cloned() else {
        return Ok(Variant::Object(dest));
    };
    assign_dictionary_struct(runtime, dest, src)?;
    Ok(Variant::Object(dest))
}

fn dictionary_save_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Dictionary.saveStruct")?;
    save_structured_value(runtime, handle, &args)?;
    Ok(Variant::Object(handle))
}

fn save_structured_value<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    handle: ObjectHandle,
    args: &[Variant],
) -> Result<()> {
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Ok(());
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    if mode.contains('b') {
        let mut serializer = BinaryStructSerializer::new(runtime);
        let mut bytes = Vec::from(BINARY_STRUCT_HEADER);
        serializer.value(&Variant::Object(handle), &mut bytes)?;
        runtime.host_mut().write_binary(&path, &mode, &bytes)?;
    } else {
        let mut serializer = StructTextSerializer::new(runtime);
        let text = serializer.value(&Variant::Object(handle), 0);
        runtime.host_mut().write_text(&path, &mode, &text)?;
    }
    Ok(())
}

fn dictionary_load_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Dictionary.loadStruct")?;
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Ok(Variant::Integer(0));
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    if mode.contains('b') {
        let Ok(bytes) = runtime.host_mut().read_binary(&path, &mode) else {
            return Ok(Variant::Integer(0));
        };
        let Some(Variant::Object(src)) = decode_binary_struct(runtime, &bytes)? else {
            return Ok(Variant::Integer(0));
        };
        assign_dictionary_struct(runtime, handle, src)?;
        return Ok(Variant::Integer(1));
    }
    let Ok(text) = runtime.host_mut().read_text(&path, &mode) else {
        return Ok(Variant::Integer(0));
    };
    let wrapped = format!("return ({text});");
    if let Ok(Variant::Object(src)) =
        compile_source_to_bytecode(&path, &wrapped).and_then(|file| runtime.execute_file(&file))
    {
        assign_dictionary_struct(runtime, handle, src)?;
        return Ok(Variant::Integer(1));
    }
    runtime.heap[handle.0].members.clear();
    install_dictionary_methods(runtime, handle);
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        runtime.heap[handle.0].set(key.to_string(), parse_struct_value(value));
    }
    Ok(Variant::Integer(1))
}

fn assign_array_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    dest: ObjectHandle,
    src: ObjectHandle,
) -> Result<()> {
    if !runtime.heap[dest.0].array_clear() {
        return Err(TjsError::runtime(
            "Array.assignStruct called on a non-array object",
        ));
    }
    let Some(elements) = runtime.heap[src.0].array_elements().map(Vec::from) else {
        return Ok(());
    };
    let mut stack = BTreeSet::new();
    stack.insert(src);
    for value in elements {
        let value = deep_clone_struct_value(runtime, &value, &mut stack)?;
        runtime.heap[dest.0].array_push(value);
    }
    install_array_methods(runtime, dest);
    Ok(())
}

fn assign_dictionary_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    dest: ObjectHandle,
    src: ObjectHandle,
) -> Result<()> {
    runtime.heap[dest.0].members.clear();
    install_dictionary_methods(runtime, dest);
    let entries = dictionary_struct_entries(runtime, src);
    let mut stack = BTreeSet::new();
    stack.insert(src);
    for (key, value) in entries {
        let value = deep_clone_struct_value(runtime, &value, &mut stack)?;
        runtime.heap[dest.0].set(key, value);
    }
    Ok(())
}

fn deep_clone_struct_value<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    value: &Variant,
    stack: &mut BTreeSet<ObjectHandle>,
) -> Result<Variant> {
    match value {
        Variant::Object(handle) if runtime.heap[handle.0].array_elements().is_some() => {
            if !stack.insert(*handle) {
                return Ok(Variant::Null);
            }
            let elements = runtime.heap[handle.0]
                .array_elements()
                .map(Vec::from)
                .unwrap_or_default();
            let dest = runtime.alloc_array_object(Vec::new());
            for element in elements {
                let element = deep_clone_struct_value(runtime, &element, stack)?;
                runtime.heap[dest.0].array_push(element);
            }
            stack.remove(handle);
            Ok(Variant::Object(dest))
        }
        Variant::Object(handle) if is_dictionary_object(runtime, *handle) => {
            if !stack.insert(*handle) {
                return Ok(Variant::Null);
            }
            let entries = dictionary_struct_entries(runtime, *handle);
            let dest = runtime.alloc_ordinary_object();
            install_dictionary_methods(runtime, dest);
            for (key, value) in entries {
                let value = deep_clone_struct_value(runtime, &value, stack)?;
                runtime.heap[dest.0].set(key, value);
            }
            stack.remove(handle);
            Ok(Variant::Object(dest))
        }
        _ => Ok(value.clone()),
    }
}

struct StructTextSerializer<'a, H: TjsHost> {
    runtime: &'a Runtime<H>,
    active: BTreeSet<ObjectHandle>,
}

impl<'a, H: TjsHost + 'static> StructTextSerializer<'a, H> {
    const MAX_DEPTH: usize = 32;

    fn new(runtime: &'a Runtime<H>) -> Self {
        Self {
            runtime,
            active: BTreeSet::new(),
        }
    }

    fn value(&mut self, value: &Variant, depth: usize) -> String {
        if depth > Self::MAX_DEPTH {
            return "void".to_string();
        }
        match value {
            Variant::Void => "void".to_string(),
            Variant::Null => "null".to_string(),
            Variant::Integer(value) => value.to_string(),
            Variant::Real(value) => real_literal(*value),
            Variant::String(value) => tjs_quote(value),
            Variant::Octet(value) => octet_literal(value),
            Variant::Object(handle) => self.object(*handle, depth),
            Variant::Closure(_) | Variant::CodeObject(_) => "null".to_string(),
        }
    }

    fn object(&mut self, handle: ObjectHandle, depth: usize) -> String {
        if !self.active.insert(handle) {
            return "null /* object recursion detected */".to_string();
        }
        let text = if let Some(elements) = self.runtime.heap[handle.0].array_elements() {
            self.array(elements, depth)
        } else if is_dictionary_object(self.runtime, handle) {
            self.dictionary(handle, depth)
        } else {
            "null".to_string()
        };
        self.active.remove(&handle);
        text
    }

    fn array(&mut self, elements: &[Variant], depth: usize) -> String {
        let indent = " ".repeat(depth);
        let child_indent = " ".repeat(depth + 1);
        let mut out = String::from("(const) [\n");
        for value in elements {
            out.push_str(&child_indent);
            out.push_str(&self.value(value, depth + 1));
            out.push_str(",\n");
        }
        out.push_str(&indent);
        out.push(']');
        out
    }

    fn dictionary(&mut self, handle: ObjectHandle, depth: usize) -> String {
        let indent = " ".repeat(depth);
        let child_indent = " ".repeat(depth + 1);
        let mut out = String::from("(const) %[\n");
        for (key, value) in dictionary_struct_entries(self.runtime, handle) {
            out.push_str(&child_indent);
            out.push_str(&tjs_quote(&key));
            out.push_str(" => ");
            out.push_str(&self.value(&value, depth + 1));
            out.push_str(",\n");
        }
        out.push_str(&indent);
        out.push(']');
        out
    }
}

fn is_native_member_name(key: &str) -> bool {
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
                | "insert"
                | "erase"
                | "pop"
                | "join"
                | "reverse"
                | "count"
                | "length"
        )
}

fn is_dictionary_object<H: TjsHost>(runtime: &Runtime<H>, handle: ObjectHandle) -> bool {
    runtime.heap[handle.0]
        .class_infos
        .iter()
        .any(|info| info == "Dictionary")
}

fn dictionary_struct_entries<H: TjsHost>(
    runtime: &Runtime<H>,
    handle: ObjectHandle,
) -> Vec<(String, Variant)> {
    runtime.heap[handle.0]
        .members
        .iter()
        .filter(|(key, _)| !is_native_member_name(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn tjs_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            ch if ch.is_control() => out.push_str(&format!("\\x{:02x}", ch as u32)),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn real_literal(value: f64) -> String {
    if value.is_nan() {
        "0.0/0.0".to_string()
    } else if value.is_infinite() {
        if value.is_sign_negative() {
            "-1.0/0.0".to_string()
        } else {
            "1.0/0.0".to_string()
        }
    } else {
        let text = value.to_string();
        if text.contains(['.', 'e', 'E']) {
            text
        } else {
            format!("{text}.0")
        }
    }
}

fn octet_literal(bytes: &[u8]) -> String {
    let mut out = String::from("<%");
    for byte in bytes {
        out.push_str(&format!(" {byte:02x}"));
    }
    if !bytes.is_empty() {
        out.push(' ');
    }
    out.push_str("%>");
    out
}

const BINARY_STRUCT_HEADER: &[u8; 8] = b"KBAD100\0";

struct BinaryStructSerializer<'a, H: TjsHost> {
    runtime: &'a Runtime<H>,
    active: BTreeSet<ObjectHandle>,
}

impl<'a, H: TjsHost + 'static> BinaryStructSerializer<'a, H> {
    fn new(runtime: &'a Runtime<H>) -> Self {
        Self {
            runtime,
            active: BTreeSet::new(),
        }
    }

    fn value(&mut self, value: &Variant, out: &mut Vec<u8>) -> Result<()> {
        match value {
            Variant::Void => out.push(0xc1),
            Variant::Null => out.push(0xc0),
            Variant::Integer(value) => put_binary_integer(out, *value),
            Variant::Real(value) => {
                out.push(0xcb);
                out.extend_from_slice(&value.to_bits().to_le_bytes());
            }
            Variant::String(value) => put_binary_string(out, value)?,
            Variant::Octet(value) => put_binary_octet(out, value)?,
            Variant::Object(handle) => self.object(*handle, out)?,
            Variant::Closure(_) | Variant::CodeObject(_) => out.push(0xc0),
        }
        Ok(())
    }

    fn object(&mut self, handle: ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
        if !self.active.insert(handle) {
            out.push(0xc0);
            return Ok(());
        }
        if let Some(elements) = self.runtime.heap[handle.0].array_elements() {
            let elements = Vec::from(elements);
            put_binary_array_header(out, elements.len())?;
            for value in elements {
                self.value(&value, out)?;
            }
        } else if is_dictionary_object(self.runtime, handle) {
            let entries = dictionary_struct_entries(self.runtime, handle);
            put_binary_map_header(out, entries.len())?;
            for (key, value) in entries {
                put_binary_string(out, &key)?;
                self.value(&value, out)?;
            }
        } else {
            out.push(0xc0);
        }
        self.active.remove(&handle);
        Ok(())
    }
}

fn put_binary_integer(out: &mut Vec<u8>, value: i64) {
    if value < 0 {
        if value >= -32 {
            out.push(value as i8 as u8);
        } else if value >= i8::MIN as i64 {
            out.push(0xd0);
            out.push(value as i8 as u8);
        } else if value >= i16::MIN as i64 {
            out.push(0xd1);
            out.extend_from_slice(&(value as i16).to_le_bytes());
        } else if value >= i32::MIN as i64 {
            out.push(0xd2);
            out.extend_from_slice(&(value as i32).to_le_bytes());
        } else {
            out.push(0xd3);
            out.extend_from_slice(&value.to_le_bytes());
        }
    } else if value <= 0x7f {
        out.push(value as u8);
    } else if value <= u8::MAX as i64 {
        out.push(0xcc);
        out.push(value as u8);
    } else if value <= u16::MAX as i64 {
        out.push(0xcd);
        out.extend_from_slice(&(value as u16).to_le_bytes());
    } else if value <= u32::MAX as i64 {
        out.push(0xce);
        out.extend_from_slice(&(value as u32).to_le_bytes());
    } else {
        out.push(0xcf);
        out.extend_from_slice(&(value as u64).to_le_bytes());
    }
}

fn put_binary_string(out: &mut Vec<u8>, value: &str) -> Result<()> {
    let units = value.encode_utf16().collect::<Vec<_>>();
    put_binary_string_header(out, units.len())?;
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

fn put_binary_string_header(out: &mut Vec<u8>, len: usize) -> Result<()> {
    if len <= 0x1f {
        out.push(0xa0 + len as u8);
    } else if len <= u8::MAX as usize {
        out.push(0xc4);
        out.push(len as u8);
    } else if len <= u16::MAX as usize {
        out.push(0xc5);
        out.extend_from_slice(&(len as u16).to_le_bytes());
    } else if len <= u32::MAX as usize {
        out.push(0xc6);
        out.extend_from_slice(&(len as u32).to_le_bytes());
    } else {
        return Err(TjsError::runtime("binary string is too large"));
    }
    Ok(())
}

fn put_binary_octet(out: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    if value.len() <= 5 {
        out.push(0xd4 + value.len() as u8);
    } else if value.len() <= u16::MAX as usize {
        out.push(0xda);
        out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    } else if value.len() <= u32::MAX as usize {
        out.push(0xdb);
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    } else {
        return Err(TjsError::runtime("binary octet is too large"));
    }
    out.extend_from_slice(value);
    Ok(())
}

fn put_binary_array_header(out: &mut Vec<u8>, len: usize) -> Result<()> {
    if len <= 0x0f {
        out.push(0x90 + len as u8);
    } else if len <= u16::MAX as usize {
        out.push(0xdc);
        out.extend_from_slice(&(len as u16).to_le_bytes());
    } else if len <= u32::MAX as usize {
        out.push(0xdd);
        out.extend_from_slice(&(len as u32).to_le_bytes());
    } else {
        return Err(TjsError::runtime("binary array is too large"));
    }
    Ok(())
}

fn put_binary_map_header(out: &mut Vec<u8>, len: usize) -> Result<()> {
    if len <= 0x0f {
        out.push(0x80 + len as u8);
    } else if len <= u16::MAX as usize {
        out.push(0xde);
        out.extend_from_slice(&(len as u16).to_le_bytes());
    } else if len <= u32::MAX as usize {
        out.push(0xdf);
        out.extend_from_slice(&(len as u32).to_le_bytes());
    } else {
        return Err(TjsError::runtime("binary dictionary is too large"));
    }
    Ok(())
}

pub(crate) fn decode_binary_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    bytes: &[u8],
) -> Result<Option<Variant>> {
    let payload = if bytes.starts_with(BINARY_STRUCT_HEADER) {
        &bytes[BINARY_STRUCT_HEADER.len()..]
    } else {
        bytes
    };
    if payload.is_empty() {
        return Ok(None);
    }
    let mut decoder = BinaryStructDecoder {
        runtime,
        bytes: payload,
        index: 0,
    };
    decoder.value().map(Some)
}

struct BinaryStructDecoder<'a, H: TjsHost> {
    runtime: &'a mut Runtime<H>,
    bytes: &'a [u8],
    index: usize,
}

impl<'a, H: TjsHost + 'static> BinaryStructDecoder<'a, H> {
    fn value(&mut self) -> Result<Variant> {
        let ty = self.read_u8()?;
        match ty {
            0x00..=0x7f => Ok(Variant::Integer(ty as i64)),
            0xe0..=0xff => Ok(Variant::Integer((ty as i8) as i64)),
            0xc0 => Ok(Variant::Null),
            0xc1 => Ok(Variant::Void),
            0xc2 => Ok(Variant::Integer(1)),
            0xc3 => Ok(Variant::Integer(0)),
            0xc4 => {
                let len = self.read_u8()? as usize;
                self.string(len)
            }
            0xc5 => {
                let len = self.read_u16()? as usize;
                self.string(len)
            }
            0xc6 => {
                let len = self.read_u32()? as usize;
                self.string(len)
            }
            0xca => Ok(Variant::Real(f32::from_bits(self.read_u32()?) as f64)),
            0xcb => Ok(Variant::Real(f64::from_bits(self.read_u64()?))),
            0xcc => Ok(Variant::Integer(self.read_u8()? as i64)),
            0xcd => Ok(Variant::Integer(self.read_u16()? as i64)),
            0xce => Ok(Variant::Integer(self.read_u32()? as i64)),
            0xcf => Ok(Variant::Integer(self.read_u64()? as i64)),
            0xd0 => Ok(Variant::Integer((self.read_u8()? as i8) as i64)),
            0xd1 => Ok(Variant::Integer(self.read_i16()? as i64)),
            0xd2 => Ok(Variant::Integer(self.read_i32()? as i64)),
            0xd3 => Ok(Variant::Integer(self.read_i64()?)),
            0xd4..=0xd9 => self.octet((ty - 0xd4) as usize),
            0xda => {
                let len = self.read_u16()? as usize;
                self.octet(len)
            }
            0xdb => {
                let len = self.read_u32()? as usize;
                self.octet(len)
            }
            0xdc => {
                let len = self.read_u16()? as usize;
                self.array(len)
            }
            0xdd => {
                let len = self.read_u32()? as usize;
                self.array(len)
            }
            0xde => {
                let len = self.read_u16()? as usize;
                self.dictionary(len)
            }
            0xdf => {
                let len = self.read_u32()? as usize;
                self.dictionary(len)
            }
            0xa0..=0xbf => self.string((ty - 0xa0) as usize),
            0x90..=0x9f => self.array((ty - 0x90) as usize),
            0x80..=0x8f => self.dictionary((ty - 0x80) as usize),
            _ => Err(TjsError::runtime("invalid binary struct tag")),
        }
    }

    fn string(&mut self, len: usize) -> Result<Variant> {
        let byte_len = len
            .checked_mul(2)
            .ok_or_else(|| TjsError::runtime("binary string is too large"))?;
        let bytes = self.read_bytes(byte_len)?;
        let units = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        Ok(Variant::String(String::from_utf16_lossy(&units)))
    }

    fn octet(&mut self, len: usize) -> Result<Variant> {
        Ok(Variant::Octet(self.read_bytes(len)?.to_vec()))
    }

    fn array(&mut self, len: usize) -> Result<Variant> {
        let handle = self.runtime.alloc_array_object(Vec::new());
        for _ in 0..len {
            let value = self.value()?;
            self.runtime.heap[handle.0].array_push(value);
        }
        Ok(Variant::Object(handle))
    }

    fn dictionary(&mut self, len: usize) -> Result<Variant> {
        let handle = self.runtime.alloc_ordinary_object();
        install_dictionary_methods(self.runtime, handle);
        for _ in 0..len {
            let Variant::String(key) = self.value()? else {
                return Err(TjsError::runtime("binary dictionary key is not a string"));
            };
            let value = self.value()?;
            self.runtime.heap[handle.0].set(key, value);
        }
        Ok(Variant::Object(handle))
    }

    fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_i16(&mut self) -> Result<i16> {
        let bytes = self.read_bytes(2)?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_i32(&mut self) -> Result<i32> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i64(&mut self) -> Result<i64> {
        let bytes = self.read_bytes(8)?;
        Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .index
            .checked_add(len)
            .ok_or_else(|| TjsError::runtime("binary struct index overflow"))?;
        if end > self.bytes.len() {
            return Err(TjsError::runtime("truncated binary struct"));
        }
        let bytes = &self.bytes[self.index..end];
        self.index = end;
        Ok(bytes)
    }
}

fn parse_struct_value(value: &str) -> Variant {
    if value == "void" {
        Variant::Void
    } else if value == "null" {
        Variant::Null
    } else if let Ok(value) = value.parse::<i64>() {
        Variant::Integer(value)
    } else if let Some(value) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        Variant::String(value.to_string())
    } else {
        Variant::String(value.to_string())
    }
}

fn regexp_compile<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "RegExp.compile")?;
    if let Some(pattern) = args.first() {
        runtime.heap[handle.0].set("pattern", pattern.clone());
    }
    if let Some(flags) = args.get(1) {
        runtime.heap[handle.0].set("flags", flags.clone());
    }
    Ok(Variant::Object(handle))
}

fn regexp_test<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "RegExp.test")?;
    let target = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let pattern = runtime.heap[handle.0]
        .get("pattern")
        .to_tjs_string()
        .unwrap_or_default();
    Ok(Variant::Integer(i64::from(target.contains(&pattern))))
}

fn regexp_match<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let matched = regexp_test(runtime, this_obj, args)?.is_truthy();
    Ok(Variant::Integer(i64::from(matched)))
}

fn date_get_time<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Date.getTime")?;
    Ok(runtime.heap[handle.0].get("timestamp"))
}

fn date_set_time<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Date.setTime")?;
    let timestamp = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0);
    runtime.heap[handle.0].set("timestamp", Variant::Integer(timestamp));
    Ok(Variant::Integer(timestamp))
}

fn date_zero<H: TjsHost + 'static>(
    _runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn date_parse<H: TjsHost + 'static>(
    _runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let value = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0);
    Ok(Variant::Integer(value))
}

fn random_generator_random<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let state = advance_random_object(runtime, this_obj, "RandomGenerator.random")?;
    Ok(Variant::Real(random_unit(state)))
}

fn random_generator_randomize<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "RandomGenerator.randomize")?;
    let seed = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or_else(|| runtime.host_mut().now_millis()) as u64;
    runtime.heap[handle.0].set("state", Variant::Integer(seed as i64));
    Ok(Variant::Void)
}

fn random_generator_random32<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(
        (advance_random_object(runtime, this_obj, "RandomGenerator.random32")? >> 32) as i64,
    ))
}

fn random_generator_random63<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(
        (advance_random_object(runtime, this_obj, "RandomGenerator.random63")? & i64::MAX as u64)
            as i64,
    ))
}

fn random_generator_random64<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(
        advance_random_object(runtime, this_obj, "RandomGenerator.random64")? as i64,
    ))
}

fn random_generator_serialize<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "RandomGenerator.serialize")?;
    Ok(runtime.heap[handle.0].get("state"))
}

fn advance_random_object<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    method: &str,
) -> Result<u64> {
    let handle = require_this(this_obj, method)?;
    let state = runtime.heap[handle.0].get("state").to_integer()? as u64;
    let next = next_random_state(state).expect("LCG always returns a value");
    runtime.heap[handle.0].set("state", Variant::Integer(next as i64));
    Ok(next)
}

fn next_random_state(value: u64) -> Option<u64> {
    Some(value.wrapping_mul(6364136223846793005).wrapping_add(1))
}

fn random_unit(value: u64) -> f64 {
    ((value >> 11) as f64) * (1.0 / ((1_u64 << 53) as f64))
}
