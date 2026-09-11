use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};

use regex::RegexBuilder;

use crate::compile_source_to_bytecode;
use crate::error::{Result, TjsError};
use crate::runtime::object::Object;
use crate::runtime::value::{ObjectHandle, Variant};
use crate::runtime::{Runtime, TjsHost, split_delimited_string, split_string_by_regex};

pub(crate) fn install<H: TjsHost + 'static>(runtime: &mut Runtime<H>) {
    let array = runtime.register_global_native("Array", native_array::<H>);
    install_array_methods(runtime, array);
    let dictionary = runtime.register_global_native("Dictionary", native_dictionary::<H>);
    install_dictionary_methods(runtime, dictionary);
    runtime.register_global_native("RegExp", native_regexp::<H>);
    runtime.register_global_native("Date", native_date::<H>);
    let exception = runtime.register_global_native("Exception", native_exception::<H>);
    // TJS superclass constructors are called as `super.Exception(...)`.
    // Native constructors therefore expose their own named member, just as
    // script class objects do, so the superclass lookup resolves to the
    // constructor instead of a missing/void value.
    runtime.set_object_member(exception, "Exception", Variant::Object(exception));
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

/// `new Dictionary()`: `tTJSDictionaryClass::CreateNew` builds a
/// `tTJSDictionaryObject` and runs `FuncCall(0, NULL, ...)` on it, which
/// copies only the class's *non-static* members.  Every Dictionary method is
/// registered with `TJS_STATICMEMBER` (`tjsDictionary.cpp:39-219`), so the new
/// object's member map stays empty -- the official manual states it outright:
/// "Dictionary クラスのオブジェクトは、作成された状態ではメンバを何一つ持って
/// いません" (`docs/tjs2/j/contents/dictionary.html`).  Instance-style
/// `dict.assign(src)` is an error there, and `dict.clear` reads as void, which
/// is what the KAGEX attribute chains depend on.
fn native_dictionary<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = runtime.alloc_object(Object::default());
    runtime.add_object_class_info(handle, "Dictionary");
    Ok(Variant::Object(handle))
}

fn native_regexp<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = runtime.alloc_object(Object::default());
    runtime.add_object_class_info(handle, "RegExp");
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
    runtime.register_object_native(handle, "_compile", regexp_compile_internal::<H>);
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
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_object(Object::default()));
    runtime.add_object_class_info(handle, "Exception");
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
    runtime.register_object_native(handle, "remove", array_remove::<H>);
    runtime.register_object_native(handle, "pop", array_pop::<H>);
    runtime.register_object_native(handle, "shift", array_shift::<H>);
    runtime.register_object_native(handle, "unshift", array_unshift::<H>);
    runtime.register_object_native(handle, "clear", array_clear::<H>);
    runtime.register_object_native(handle, "assign", array_assign::<H>);
    runtime.register_object_native(handle, "assignStruct", array_assign_struct::<H>);
    runtime.register_object_native(handle, "load", array_load::<H>);
    runtime.register_object_native(handle, "save", array_save::<H>);
    runtime.register_object_native(handle, "saveStruct", array_save_struct::<H>);
    runtime.register_object_native(handle, "loadStruct", array_load_struct::<H>);
    runtime.register_object_native(handle, "split", array_split::<H>);
    runtime.register_object_native(handle, "join", array_join::<H>);
    runtime.register_object_native(handle, "sort", array_sort::<H>);
    runtime.register_object_native(handle, "reverse", array_reverse::<H>);
    runtime.register_object_native(handle, "find", array_find::<H>);
}

pub fn install_dictionary_methods<H: TjsHost + 'static>(
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
    if !runtime.heap[handle.0].array_extend(args) {
        return Err(TjsError::runtime("Array.add called on a non-array object"));
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
    if !runtime.heap[handle.0].array_insert_values(index, args.into_iter().skip(1)) {
        return Err(TjsError::runtime(
            "Array.insert called on a non-array object",
        ));
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

fn array_remove<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.remove")?;
    // Official: `if(numparams < 1) return TJS_E_BADPARAMCOUNT`
    // (`tjsArray.cpp:816`).
    let Some(value) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    let remove_all = args.get(1).map(Variant::is_truthy).unwrap_or(true);
    let removed = runtime.heap[handle.0]
        .array_remove_values(value, remove_all)
        .ok_or_else(|| TjsError::runtime("Array.remove called on a non-array object"))?;
    Ok(Variant::Integer(removed as i64))
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

fn array_shift<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.shift")?;
    let len = runtime.heap[handle.0]
        .array_elements()
        .map(|items| items.len())
        .ok_or_else(|| TjsError::runtime("Array.shift called on a non-array object"))?;
    if len == 0 {
        return Ok(Variant::Void);
    }
    runtime.heap[handle.0]
        .array_erase(0)
        .ok_or_else(|| TjsError::runtime("Array.shift called on a non-array object"))
}

fn array_unshift<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.unshift")?;
    if !runtime.heap[handle.0].array_prepend(args) {
        return Err(TjsError::runtime(
            "Array.unshift called on a non-array object",
        ));
    }
    Ok(Variant::Integer(
        runtime.heap[handle.0]
            .array_elements()
            .map(|items| items.len() as i64)
            .unwrap_or(0),
    ))
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
        // `tTJSArrayNI::Assign` (`tjsArray.cpp:1060-1085`) pushes `name, value`
        // for every enumerated member of a non-array source, skipping only
        // `TJS_HIDDENMEMBER` (`tDictionaryEnumCallback`, `:1088-1112`).  A
        // source key that happens to be named like a builtin (`clear`,
        // `count`, ...) is ordinary data and is copied.
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
    // Official: `if(numparams < 1) return TJS_E_BADPARAMCOUNT`
    // (`tjsArray.cpp:264`).
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Err(TjsError::bad_param_count());
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let text = runtime.host_mut().read_text(&path, &mode)?;
    runtime.heap[handle.0] = Object::array(
        text.lines()
            .map(|line| Variant::String(line.to_string()))
            .collect(),
    );
    install_array_methods(runtime, handle);
    Ok(Variant::Object(handle))
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
    if let Some(value) = load_binary_struct(runtime, &path, &mode)? {
        if let Variant::Object(src) = value
            && runtime.heap[handle.0].array_elements().is_some()
        {
            assign_array_struct(runtime, handle, src)?;
        }
        return Ok(value);
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

fn array_split<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.split")?;
    // Official: `if(numparams < 2) return TJS_E_BADPARAMCOUNT`
    // (`tjsArray.cpp:506`).
    if args.len() < 2 {
        return Err(TjsError::bad_param_count());
    }
    let string = args[1].to_tjs_string()?;
    let purge_empty = args
        .get(3)
        .filter(|value| !matches!(value, Variant::Void))
        .is_some_and(Variant::is_truthy);
    if let Some(regexp) = regexp_object_handle(runtime, &args[0]) {
        let regex = regexp_regex(runtime, regexp)?;
        runtime.heap[handle.0] = Object::array(split_string_by_regex(&string, &regex, purge_empty));
        install_array_methods(runtime, handle);
        return Ok(Variant::Object(handle));
    }
    let delimiters = args[0].to_tjs_string()?;
    runtime.heap[handle.0] =
        Object::array(split_delimited_string(&string, &delimiters, purge_empty));
    install_array_methods(runtime, handle);
    Ok(Variant::Object(handle))
}

fn array_sort<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.sort")?;
    if let Some(comparator @ (Variant::Object(_) | Variant::Closure(_) | Variant::CodeObject(_))) =
        args.first()
    {
        let mut elements = runtime.heap[handle.0]
            .array_elements()
            .ok_or_else(|| TjsError::runtime("Array.sort called on a non-array object"))?
            .to_vec();

        // TJS accepts a function as the first argument and interprets its
        // truthy return value as `lhs < rhs`. Use insertion sort here because
        // Rust's standard sorting callbacks cannot return script errors.
        for index in 1..elements.len() {
            let mut current = index;
            while current > 0 {
                let less = runtime
                    .call_function(
                        comparator.clone(),
                        vec![elements[current].clone(), elements[current - 1].clone()],
                    )?
                    .is_truthy();
                if !less {
                    break;
                }
                elements.swap(current, current - 1);
                current -= 1;
            }
        }

        runtime.heap[handle.0] = Object::array(elements);
        install_array_methods(runtime, handle);
        return Ok(Variant::Void);
    }
    let mode = args
        .first()
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()?
        .and_then(|value| value.chars().next())
        .filter(|mode| matches!(mode, '+' | '-' | '0' | '9' | 'a' | 'z'))
        .unwrap_or('+');
    if !runtime.heap[handle.0].array_sort_by(|lhs, rhs| array_sort_less(lhs, rhs, mode))? {
        return Err(TjsError::runtime("Array.sort called on a non-array object"));
    }
    Ok(Variant::Void)
}

fn array_sort_less(lhs: &Variant, rhs: &Variant, mode: char) -> Result<bool> {
    match mode {
        '-' => rhs.less_than(lhs),
        '0' => Ok(lhs.to_real()? < rhs.to_real()?),
        '9' => Ok(lhs.to_real()? > rhs.to_real()?),
        'a' => Ok(lhs.to_tjs_string()? < rhs.to_tjs_string()?),
        'z' => Ok(lhs.to_tjs_string()? > rhs.to_tjs_string()?),
        _ => lhs.less_than(rhs),
    }
}

fn array_find<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.find")?;
    // Official: `if(numparams < 1) return TJS_E_BADPARAMCOUNT`
    // (`tjsArray.cpp:941`).
    let Some(needle) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    let elements = runtime.heap[handle.0]
        .array_elements()
        .ok_or_else(|| TjsError::runtime("Array.find called on a non-array object"))?;
    let len = elements.len() as i64;
    let mut start = args
        .get(1)
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0);
    if start < 0 {
        start += len;
    }
    let start = start.max(0);
    if start >= len {
        return Ok(Variant::Integer(-1));
    }
    for (index, element) in elements.iter().enumerate().skip(start as usize) {
        if needle.discern_eq(element) {
            return Ok(Variant::Integer(index as i64));
        }
    }
    Ok(Variant::Integer(-1))
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

/// `tTJSDictionaryNI::Clear` (`tjsDictionary.cpp:374-377`) is `Owner->Clear()`
/// and nothing else: every member of the destination goes away, and the class
/// surface is *not* re-registered on the instance (there is none to lose --
/// the methods live on the class object).
fn dictionary_clear<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_dictionary_instance(runtime, this_obj, "Dictionary.clear")?;
    runtime.heap[handle.0].members.clear();
    Ok(Variant::Void)
}

/// `tTJSDictionaryNI::Assign` (`tjsDictionary.cpp:325-373`): copy every
/// enumerated member of the source onto the destination.
///
/// `dict.assign` never reaches this code -- the method is a static class
/// member, so script uses `(Dictionary.assign incontextof dict)(src, clear)`.
/// The destination is emptied first unless `clear` is false
/// (`if(clear) Owner->Clear();`, `:334`/`:347`), and the copy is unconditional:
/// the enumeration callback only skips `TJS_HIDDENMEMBER` (`:378-400`), so a
/// source key named like a builtin method (`clear`, `assign`, `count`, ...) is
/// ordinary data and lands on the destination like any other.
fn dictionary_assign<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = require_dictionary_instance(runtime, this_obj, "Dictionary.assign")?;
    // Official: `if(numparams < 1) return TJS_E_BADPARAMCOUNT`
    // (`tjsDictionary.cpp:353`).
    let Some(source) = args.first().cloned() else {
        return Err(TjsError::bad_param_count());
    };
    // Official: `bool clear = true; if(numparams >= 2 && param[1]->Type() !=
    // tvtVoid) clear = 0!=(tjs_int)*param[1];` (`tjsDictionary.cpp:349-351`).
    let clear = match args.get(1) {
        Some(value) if !matches!(value, Variant::Void) => value.to_integer()? != 0,
        _ => true,
    };
    let Some(src) = dictionary_assign_source(runtime, &source) else {
        // Official: `else TJS_eTJSError(TJSNullAccess)` (`:359`).
        return Err(TjsError::null_access());
    };
    if clear {
        runtime.heap[dest.0].members.clear();
    }
    if let Some(elements) = runtime.heap[src.0].array_elements().map(Vec::from) {
        // An Array source is a flat name/value stream (`:329-346`): the loop
        // reads a name, stringifies it, then consumes the next element as the
        // value, so a trailing unpaired element is dropped.
        for pair in elements.chunks_exact(2) {
            let name = pair[0].to_tjs_string()?;
            let value = pair[1].clone();
            runtime.heap[dest.0].set(name, value);
        }
        return Ok(Variant::Void);
    }
    // Snapshot after the clear, not before: `d.assign(d, 1)` assigns from the
    // already-emptied destination in the reference too.
    let members = runtime.heap[src.0].members.clone();
    for (key, value) in members {
        runtime.heap[dest.0].set(key, value);
    }
    Ok(Variant::Void)
}

/// The destination of a `Dictionary.assign`-family call.
///
/// Every Dictionary method starts with
/// `TJS_GET_NATIVE_INSTANCE(ni, tTJSDictionaryNI)`, which reads a Dictionary
/// native instance off `objthis`; anything else -- the class object above all
/// -- reports `TJS_E_NATIVECLASSCRASH` (`tjsNative.h:320-328`).  This runtime
/// has no native-instance table, so the equivalent question is whether the
/// receiver is a Dictionary: the object itself carries the class name, or its
/// class chain reaches the Dictionary class object (a script class that
/// extends `Dictionary`).
fn require_dictionary_instance<H: TjsHost + 'static>(
    runtime: &Runtime<H>,
    this_obj: Option<ObjectHandle>,
    name: &str,
) -> Result<ObjectHandle> {
    let handle = require_this(this_obj, name)?;
    if is_dictionary_receiver(runtime, handle) {
        Ok(handle)
    } else {
        Err(TjsError::native_class_crash())
    }
}

fn is_dictionary_receiver<H: TjsHost + 'static>(
    runtime: &Runtime<H>,
    handle: ObjectHandle,
) -> bool {
    let class = match runtime.global_member("Dictionary") {
        Variant::Object(class) => class,
        _ => return false,
    };
    // The class object itself is not a Dictionary instance: it carries the
    // class name for lookup purposes, but no native instance behind `this`.
    if handle == class {
        return false;
    }
    let mut current = Some(handle);
    while let Some(object) = current {
        if runtime.heap[object.0]
            .class_infos
            .iter()
            .any(|info| info == "Dictionary")
        {
            return true;
        }
        current = runtime.object_super_class(object);
    }
    false
}

/// `tTJSVariantClosure clo = param[0]->AsObjectClosureNoAddRef();` followed by
/// `if(clo.ObjThis) ... else if(clo.Object) ... else TJS_eTJSError(TJSNullAccess)`
/// (`tjsDictionary.cpp:353-359`): a bound closure assigns from its `ObjThis`,
/// any other object from its `Object`, and a non-object (void or null)
/// reports null access.
fn dictionary_assign_source<H: TjsHost + 'static>(
    runtime: &Runtime<H>,
    value: &Variant,
) -> Option<ObjectHandle> {
    match value {
        Variant::Object(handle) => Some(runtime.bound_this(*handle).unwrap_or(*handle)),
        Variant::Closure(closure) => Some(closure.this_obj.unwrap_or(closure.object)),
        _ => None,
    }
}

fn dictionary_assign_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = require_dictionary_instance(runtime, this_obj, "Dictionary.assignStruct")?;
    // Official: `if(numparams < 1) return TJS_E_BADPARAMCOUNT`, then
    // `else TJS_eTJSError(TJSNullAccess)` for a non-object source
    // (`tjsDictionary.cpp:190-208`).
    let Some(source) = args.first().cloned() else {
        return Err(TjsError::bad_param_count());
    };
    let Some(src) = dictionary_assign_source(runtime, &source) else {
        return Err(TjsError::null_access());
    };
    assign_dictionary_struct(runtime, dest, src)?;
    Ok(Variant::Void)
}

fn dictionary_save_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_dictionary_instance(runtime, this_obj, "Dictionary.saveStruct")?;
    save_structured_value(runtime, handle, &args)?;
    // Official: `if(result) *result = tTJSVariant(objthis, objthis)`
    // (`tjsDictionary.cpp:163`).
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
    if let Some(value) = load_binary_struct(runtime, &path, &mode)? {
        // A static `Dictionary.loadStruct(...)` call arrives with the class
        // object as `this`; KRKR deserializes into a throw-away dictionary in
        // that case, so never write the pack's members onto the class itself.
        if let Variant::Object(src) = value
            && !runtime.object_is_callable(handle)
        {
            assign_dictionary_struct(runtime, handle, src)?;
        }
        return Ok(value);
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
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        runtime.heap[handle.0].set(key.to_string(), parse_struct_value(value));
    }
    Ok(Variant::Integer(1))
}

/// Reads `path` and deserializes it when it holds a `KBAD100` struct pack.
///
/// `saveStruct` needs mode `"b"` to *write* the binary form, but KRKR's
/// `loadStruct` sniffs the header itself and accepts a binary pack in any mode
/// (`tjsDictionary.cpp` / `tjsArray.cpp`), returning the deserialized root
/// value rather than a success flag.  `None` means the file is not a binary
/// pack, which leaves the caller free to try the textual form.
fn load_binary_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    path: &str,
    mode: &str,
) -> Result<Option<Variant>> {
    let Ok(bytes) = runtime.host_mut().read_binary(path, mode) else {
        return Ok(None);
    };
    if !bytes.starts_with(BINARY_STRUCT_HEADER) {
        return Ok(None);
    }
    decode_binary_struct(runtime, &bytes)
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
            runtime.add_object_class_info(dest, "Dictionary");
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
        for (index, value) in elements.iter().enumerate() {
            out.push_str(&child_indent);
            out.push_str(&self.value(value, depth + 1));
            if index + 1 == elements.len() {
                out.push('\n');
            } else {
                out.push_str(",\n");
            }
        }
        out.push_str(&indent);
        out.push(']');
        out
    }

    fn dictionary(&mut self, handle: ObjectHandle, depth: usize) -> String {
        let indent = " ".repeat(depth);
        let child_indent = " ".repeat(depth + 1);
        let mut out = String::from("(const) %[\n");
        let entries = dictionary_struct_entries(self.runtime, handle);
        for (index, (key, value)) in entries.iter().enumerate() {
            out.push_str(&child_indent);
            out.push_str(&tjs_quote(key));
            out.push_str(" => ");
            out.push_str(&self.value(value, depth + 1));
            if index + 1 == entries.len() {
                out.push('\n');
            } else {
                out.push_str(",\n");
            }
        }
        out.push_str(&indent);
        out.push(']');
        out
    }
}

fn is_dictionary_object<H: TjsHost>(runtime: &Runtime<H>, handle: ObjectHandle) -> bool {
    runtime.heap[handle.0]
        .class_infos
        .iter()
        .any(|info| info == "Dictionary")
}

/// The destination-side view of a Dictionary's members: every entry, in the
/// member map's order.  The reference's `SaveStructuredData` and
/// `AssignStructure` run `EnumMembers` over the object's own symbols and skip
/// only `TJS_HIDDENMEMBER` (`tjsDictionary.cpp:410-420`, `:452-470`), so a key
/// named `clear` or `count` is ordinary data -- which is why nothing here may
/// filter by name.  Builtin methods are not in this map to begin with: they
/// live on the class object.
fn dictionary_struct_entries<H: TjsHost>(
    runtime: &Runtime<H>,
    handle: ObjectHandle,
) -> Vec<(String, Variant)> {
    runtime.heap[handle.0]
        .members
        .iter()
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
    let literal = real_hex_literal(value);
    format!("{literal} /* {} */", real_comment_literal(value))
}

fn real_hex_literal(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    } else if value.is_infinite() {
        if value.is_sign_negative() {
            return "-Infinity".to_string();
        } else {
            return "+Infinity".to_string();
        }
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "+0.0".to_string()
        };
    }

    let bits = value.to_bits();
    let prefix = if bits & (1 << 63) != 0 {
        "-0x1."
    } else {
        "0x1."
    };
    let exponent = (((bits & 0x7ff0_0000_0000_0000) >> 52) as i32) - 1023;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    format!("{prefix}{fraction:013X}p{exponent}")
}

fn real_comment_literal(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "+Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else if value == 0.0 {
        if value.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "+0.0".to_string()
        }
    } else {
        value.to_string()
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
        if value >= i8::MIN as i64 {
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
        self.runtime.add_object_class_info(handle, "Dictionary");
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

fn regexp_compile_internal<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "RegExp._compile")?;
    // Official: `if(numparams != 1) return TJS_E_BADPARAMCOUNT`
    // (`tjsRegExp.cpp:257`).
    if args.len() != 1 {
        return Err(TjsError::bad_param_count());
    }
    let source = args[0].to_tjs_string()?;
    // Internal literal format used by precompiled bytecode: `//flags/expression`.
    let body = source
        .strip_prefix("//")
        .ok_or_else(|| TjsError::runtime("RegExp._compile: expression must start with `//`"))?;
    let slash = body.find('/').ok_or_else(|| {
        TjsError::runtime("RegExp._compile: expression is missing the flag terminator")
    })?;
    runtime.heap[handle.0].set("flags", Variant::String(body[..slash].to_string()));
    runtime.heap[handle.0].set("pattern", Variant::String(body[slash + 1..].to_string()));
    Ok(Variant::Object(handle))
}

pub(crate) fn regexp_object_handle<H: TjsHost>(
    runtime: &Runtime<H>,
    value: &Variant,
) -> Option<ObjectHandle> {
    let Variant::Object(handle) = value else {
        return None;
    };
    let object = &runtime.heap[handle.0];
    if matches!(object.get("pattern"), Variant::String(_))
        && !matches!(object.get("_compile"), Variant::Void)
    {
        Some(*handle)
    } else {
        None
    }
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
    let regex = regexp_regex(runtime, handle)?;
    Ok(Variant::Integer(i64::from(regex.is_match(&target))))
}

fn regexp_match<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "RegExp.match")?;
    let target = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let regex = regexp_regex(runtime, handle)?;
    let values = regex
        .captures(&target)
        .map(|captures| {
            captures
                .iter()
                .map(|capture| Variant::String(capture.map(|m| m.as_str()).unwrap_or("").into()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

pub(crate) fn regexp_regex<H: TjsHost>(
    runtime: &Runtime<H>,
    handle: ObjectHandle,
) -> Result<regex::Regex> {
    let pattern = runtime.heap[handle.0]
        .get("pattern")
        .to_tjs_string()
        .unwrap_or_default();
    let flags = runtime.heap[handle.0]
        .get("flags")
        .to_tjs_string()
        .unwrap_or_default();
    RegexBuilder::new(&pattern)
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        .build()
        .map_err(|error| TjsError::runtime(format!("RegExp compile failed: {error}")))
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
