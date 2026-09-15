use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};

use regex::RegexBuilder;

use crate::compile_source_to_bytecode;
use crate::error::{Result, TjsError};
use crate::runtime::object::Object;
use crate::runtime::value::{ObjectHandle, Variant};
use crate::runtime::{
    Runtime, TjsHost, split_delimited_string, split_loaded_lines, split_string_by_regex,
};

pub(crate) fn install<H: TjsHost + 'static>(runtime: &mut Runtime<H>) {
    let array = runtime.register_global_native("Array", native_array::<H>);
    install_array_methods(runtime, array);
    let dictionary = runtime.register_global_native("Dictionary", native_dictionary::<H>);
    install_dictionary_methods(runtime, dictionary);
    let regexp = runtime.register_global_native("RegExp", native_regexp::<H>);
    install_empty_finalize(runtime, regexp);
    let date = runtime.register_global_native("Date", native_date::<H>);
    install_empty_finalize(runtime, date);
    let exception = runtime.register_global_native("Exception", native_exception::<H>);
    install_empty_finalize(runtime, exception);
    // TJS superclass constructors are called as `super.Exception(...)`.
    // Native constructors therefore expose their own named member, just as
    // script class objects do, so the superclass lookup resolves to the
    // constructor instead of a missing/void value.
    runtime.set_object_member(exception, "Exception", Variant::Object(exception));
    install_math(runtime);
}

/// krkrz's empty native `finalize` (`TJS_DECL_EMPTY_FINALIZE_METHOD`,
/// `tjsNative.h:380-383`): a method whose whole body is `return TJS_S_OK;`, so
/// the call answers void and exists only to be callable.  Every class below
/// declares it -- `Exception` (`tjsException.cpp:30`), `Math`
/// (`tjsMath.cpp:108`), `RegExp` (`tjsRegExp.cpp:210`), `Date`
/// (`tjsDate.cpp:51`) and `RandomGenerator` (`tjsRandomGenerator.cpp:267`) --
/// and a missing member aborts the caller with
/// `Member "finalize" does not exist`: PARQUET's `ConductorException extends
/// Exception` calls `global.Exception.finalize(...)` from its own `finalize`.
///
/// The declaration carries no `TJS_STATICMEMBER`, so
/// `tTJSNativeClass::CreateNew` copies it onto each instance as well
/// (`tjsNative.cpp:340-364`); a script subclass that defines its own
/// `finalize` keeps it, exactly as the engine-side registrations do
/// (`crates/krkr-engine/src/native/classes.rs`, M146).
fn install_empty_finalize<H: TjsHost + 'static>(runtime: &mut Runtime<H>, handle: ObjectHandle) {
    if matches!(
        runtime.object_member(handle, "finalize"),
        Variant::Closure(_)
    ) {
        return;
    }
    runtime.register_object_native(handle, "finalize", empty_finalize::<H>);
}

fn empty_finalize<H: TjsHost + 'static>(
    _runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
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
///
/// `new Dictionary(count)` sizes the member table up front
/// (`tjsDictionary.cpp:244-257`): an Integer count of 8 or more picks the
/// bucket count from [`symbol_table::hash_bits_for_count`], anything else
/// leaves the default 8 buckets.  The size is observable -- the same keys
/// enumerate in a different order from a table that started wider.
fn native_dictionary<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = match args.first() {
        Some(Variant::Integer(count)) if *count >= 8 => {
            runtime.alloc_dictionary_object_sized(*count)
        }
        _ => runtime.alloc_dictionary_object(),
    };
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
    install_empty_finalize(runtime, handle);
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
    install_empty_finalize(runtime, handle);
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
    install_empty_finalize(runtime, handle);
    Ok(Variant::Object(handle))
}

fn install_math<H: TjsHost + 'static>(runtime: &mut Runtime<H>) {
    let math = runtime.alloc_object(Object::default());
    runtime.set_global_member("Math", Variant::Object(math));
    install_empty_finalize(runtime, math);

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
    install_empty_finalize(runtime, random_generator);
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
    install_empty_finalize(runtime, handle);
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
    runtime.register_object_native(handle, "load", dictionary_load::<H>);
    runtime.register_object_native(handle, "loadStruct", dictionary_load_struct::<H>);
    runtime.register_object_native(handle, "save", dictionary_save::<H>);
    runtime.register_object_native(handle, "saveStruct", dictionary_save_struct::<H>);
    runtime.register_object_native(handle, "assign", dictionary_assign::<H>);
    runtime.register_object_native(handle, "assignStruct", dictionary_assign_struct::<H>);
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

/// `Array.assign` (`tjsArray.cpp:745-762`): clear the destination, then copy
/// every element of the source.
///
/// Both methods of this family answer `TJS_S_OK` without writing `result`, and
/// `tTJSNativeClassMethod::FuncCall` clears the result variant before the call
/// (`tjsNative.cpp:94`), so the script-visible value is void.  The source is
/// converted *after* the clear, so a call without an argument or with a
/// non-object one leaves the destination empty when it raises.
fn array_assign<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = require_this(this_obj, "Array.assign")?;
    // Official: `if(numparams < 1) return TJS_E_BADPARAMCOUNT`
    // (`tjsArray.cpp:750`).
    let Some(source) = args.first().cloned() else {
        return Err(TjsError::bad_param_count());
    };

    if !runtime.heap[dest.0].array_clear() {
        return Err(TjsError::runtime(
            "Array.assign called on a non-array object",
        ));
    }

    let Some(src) = closure_source_object(runtime, &source) else {
        // Official: `else TJS_eTJSError(TJSNullAccess)` (`:758`).  A void,
        // null or scalar source has no closure to copy from.
        return Err(TjsError::null_access());
    };
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
        for (key, value) in runtime.heap[src.0].member_entries() {
            runtime.heap[dest.0].array_push(Variant::String(key));
            runtime.heap[dest.0].array_push(value);
        }
    }
    Ok(Variant::Void)
}

/// `Array.assignStruct` (`tjsArray.cpp:764-782`), the structured twin of
/// [`array_assign`]: same `TJS_E_BADPARAMCOUNT` / `TJSNullAccess` protocol,
/// same void result, and the clear happens before the source conversion.
fn array_assign_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = require_this(this_obj, "Array.assignStruct")?;
    let Some(source) = args.first().cloned() else {
        return Err(TjsError::bad_param_count());
    };
    if !runtime.heap[dest.0].array_clear() {
        return Err(TjsError::runtime(
            "Array.assignStruct called on a non-array object",
        ));
    }
    let Some(src) = closure_source_object(runtime, &source) else {
        return Err(TjsError::null_access());
    };
    assign_array_struct(runtime, dest, src)?;
    Ok(Variant::Void)
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
    // The reference creates the read stream *before* it touches the receiver
    // (`tjsArray.cpp:270-273`), so a path that cannot be read leaves the items
    // exactly as they were.
    let text = runtime.host_mut().read_text(&path, &mode)?;
    // `ni->Items.clear()` and the refill work on the native instance behind
    // `this` (`tjsArray.cpp:270-332`), so the receiver object and every member
    // on it survive the call.  That is what `saveStruct.dll` needs: `save2`
    // and its siblings are copied onto each Array *instance*
    // (`tjsNative.cpp:340-364`, `Main.cpp:281-291`), and the games' settings
    // writer loads the file it is about to save back through.
    if !runtime.heap[handle.0].array_clear() {
        // Anything without an Array native instance is
        // `TJS_E_NATIVECLASSCRASH` (`TJS_GET_NATIVE_INSTANCE`,
        // `tjsNative.h:312-319`).
        return Err(TjsError::native_class_crash());
    }
    runtime.heap[handle.0].array_extend(split_loaded_lines(&text));
    // Official: `if(result) *result = tTJSVariant(objthis, objthis);`
    // (`tjsArray.cpp:358`).
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

/// `Array.loadStruct` (`tjsArray.cpp:364-403`) reads one container and nothing
/// else.  The receiver has to carry an Array native instance (`:366`,
/// `TJS_GET_NATIVE_INSTANCE`), one parameter is mandatory (`:368`), its items
/// are cleared *before* the stream is opened (`ni->Items.clear()`, `:373`), and
/// then `tTJSBinarySerializer::IsBinary` decides the rest (`:379-385`): a
/// `KBAD100\0` pack is deserialized into the receiver (`CreateArray`'s
/// `RootArray` path, `tjsBinarySerializer.cpp:104-112`) and the call answers
/// that same receiver (`ReadArray`'s `tTJSVariant(array, array)`, `:268-280`),
/// while anything else -- a `TJS/ns0` data pack, a text struct, a path that
/// cannot be read -- is `TJS_E_INVALIDPARAM` (`:400-402`).  There is no text
/// fallback and no Integer success flag.
fn array_load_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.loadStruct")?;
    // Official: `TJS_GET_NATIVE_INSTANCE(ni, tTJSArrayNI)` (`tjsArray.cpp:366`)
    // before the arity check (`:368`) and before the items are cleared
    // (`:373`).
    if runtime.heap[handle.0].array_elements().is_none() {
        return Err(TjsError::native_class_crash());
    }
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Err(TjsError::bad_param_count());
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    runtime.heap[handle.0].array_clear();
    if load_binary_struct(runtime, &path, &mode, Some(handle))?.is_none() {
        return Err(TjsError::invalid_param());
    }
    Ok(Variant::Object(handle))
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

/// `Array.split` (`tjsArray.cpp:498-580`): the receiver's items are replaced
/// in place -- `ni->Items.resize(0)` (`:508`) and then the pushes -- never the
/// object itself, so every member on it and every alias of it survives, the
/// same rule `Array.load` follows (`:270-332`).  The receiver still has to be
/// an Array: `TJS_GET_NATIVE_INSTANCE` (`:505`) reports
/// `TJS_E_NATIVECLASSCRASH` for anything else.
fn array_split<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Array.split")?;
    // Official: `TJS_GET_NATIVE_INSTANCE(ni, tTJSArrayNI)` (`tjsArray.cpp:505`)
    // first, then `if(numparams < 2) return TJS_E_BADPARAMCOUNT` (`:506`).
    if runtime.heap[handle.0].array_elements().is_none() {
        return Err(TjsError::native_class_crash());
    }
    if args.len() < 2 {
        return Err(TjsError::bad_param_count());
    }
    runtime.heap[handle.0].array_clear();
    let string = args[1].to_tjs_string()?;
    let purge_empty = args
        .get(3)
        .filter(|value| !matches!(value, Variant::Void))
        .is_some_and(Variant::is_truthy);
    let elements = if let Some(regexp) = regexp_object_handle(runtime, &args[0]) {
        let regex = regexp_regex(runtime, regexp)?;
        split_string_by_regex(&string, &regex, purge_empty)
    } else {
        let delimiters = args[0].to_tjs_string()?;
        split_delimited_string(&string, &delimiters, purge_empty)
    };
    runtime.heap[handle.0].array_extend(elements);
    // Official: `if(result) *result = tTJSVariant(objthis, objthis)` on both
    // paths (`tjsArray.cpp:532`, `:575`).
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

/// `Dictionary.load` is a registered stub in the reference: it validates the
/// receiver's native instance and returns `TJS_S_OK` -- `// TODO: implement
/// Dictionary.load()` (`tjsDictionary.cpp:41-49`) -- so the call answers void
/// without reading a file.  It is part of the class surface, which is why it
/// is registered here even though it does nothing: a script that probes
/// `typeof Dictionary.load` has to see a method.
fn dictionary_load<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    require_dictionary_instance(runtime, this_obj, "Dictionary.load")?;
    Ok(Variant::Void)
}

/// `Dictionary.save`, the write-side twin of [`dictionary_load`]
/// (`tjsDictionary.cpp:110-118`, `// TODO: implement Dictionary.save();`).
fn dictionary_save<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    require_dictionary_instance(runtime, this_obj, "Dictionary.save")?;
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
    let Some(src) = closure_source_object(runtime, &source) else {
        // Official: `else TJS_eTJSError(TJSNullAccess)` (`:359`).
        return Err(TjsError::null_access());
    };
    if clear {
        runtime.heap[dest.0].members.clear();
    }
    if let Some(elements) = runtime.heap[src.0].array_elements().map(Vec::from) {
        // An Array source is a flat name/value stream (`:329-346`): the loop
        // reads a name, stringifies it, then consumes the next element as the
        // value, so a trailing unpaired element is dropped.  The table is
        // resized for the incoming items before the copy (`:332-334`).
        let reqcount = runtime.heap[dest.0].members.len() as i64 + elements.len() as i64;
        runtime.heap[dest.0].members.rebuild(reqcount);
        for pair in elements.chunks_exact(2) {
            let name = pair[0].to_tjs_string()?;
            let value = pair[1].clone();
            runtime.heap[dest.0].set(name, value);
        }
        return Ok(Variant::Void);
    }
    // Snapshot after the clear, not before: `d.assign(d, 1)` assigns from the
    // already-emptied destination in the reference too.
    let members = runtime.heap[src.0].member_entries();
    // `reserve area`: the enumeration to count the source runs first, then
    // `Owner->RebuildHash(reqcount)` sizes the table, then the copy runs
    // (`:348-359`).
    let reqcount = runtime.heap[dest.0].members.len() as i64 + members.len() as i64;
    runtime.heap[dest.0].members.rebuild(reqcount);
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
    // A class object is not a Dictionary instance: it carries the class name
    // for lookup purposes, but no native instance behind `this`
    // (`Runtime::is_dictionary_instance`).
    runtime.is_dictionary_instance(handle)
}

/// `tTJSVariantClosure clo = param[0]->AsObjectClosureNoAddRef();` followed by
/// `if(clo.ObjThis) ... else if(clo.Object) ... else TJS_eTJSError(TJSNullAccess)`
/// (`tjsDictionary.cpp:353-359`, `tjsArray.cpp:752-758`): a bound closure
/// assigns from its `ObjThis`, any other object from its `Object`, and a
/// non-object (void, null or a scalar) reports null access.
fn closure_source_object<H: TjsHost + 'static>(
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
    let Some(src) = closure_source_object(runtime, &source) else {
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

/// `name` and `mode` are the reference's own pair
/// (`tjsDictionary.cpp:133-179`, `tjsArray.cpp:455-490`): the mode string is a
/// *stream mode* handed to the stream factory, not a flag this crate owns.  A
/// `b` anywhere in it (`TJS_strchr(mode.c_str(), TJS_W('b'))`,
/// `tjsDictionary.cpp:143`) selects the `KBAD100\0` binary serializer, and
/// every other mode the text stream writer (`:148-156`, `:160-166`).  The rest
/// of the grammar belongs to that factory
/// (`base/BinaryStream.cpp:52-76`, `base/TextStream.cpp:343-640`) and is only
/// meaningful because the mode reaches it untouched:
///
/// * `o<digits>` -- open the *existing* file in update mode and seek to that
///   offset.  `_TVPCreateStream` resolves anything but `TJS_BS_WRITE` through
///   `TVPGetPlacedPath` ("file must exist", `StorageIntf.cpp:1242-1244`), so
///   an `o` mode never creates the file and never truncates it.
/// * `z`, `z<level>` -- zlib-compress the UTF-16LE payload.  The stream writes
///   the `fe fe 02` mode signature, the `ff fe` BOM, a placeholder for the two
///   little-endian sizes, and back-fills them from `ZStream->total_out` and
///   `total_in` when it closes (`base/TextStream.cpp:428-462`, `:487-489`).
/// * `c`, `c<mode>` -- the simple crypt (`:381-386`), which this engine's
///   storage layer writes as mode 1.
///
/// KAGEX's `BookMarkIO_Standard.save` writes `saveDataMode + "o" + size`, and
/// GINKA's `main/Config.tjs` sets `saveDataMode = debugWindowEnabled ? "" :
/// "z"`: the thumbnail BMP is already in the file and the struct is appended
/// at its last byte, so both the offset and the compressor have to survive the
/// trip to the storage layer.
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

/// `Dictionary.loadStruct` (`tjsDictionary.cpp:51-121`) reads two containers.
/// One parameter is mandatory (`:53`); a receiver that carries a Dictionary
/// native instance is that call's `RootDictionary` and is cleared before the
/// stream is opened (`ni->Clear()`, `:57-62`), while the class object -- which
/// has no native instance -- is left alone and decoded into a throw-away
/// dictionary (`if(!dic) dic = ...`, `:78-80`).  Then:
///
/// * `tTJSBinarySerializer::IsBinary` (`:72-76`) picks a `KBAD100\0` pack, the
///   call answers the deserialized root (`if(result) *result = *var;`,
///   `:84-88`), and a data pack of any other kind falls through to the text
///   path below.
/// * The text path (`:104-110`) reads the file with
///   `tTJS::LoadTextDictionaryArray` (`tjs.cpp:607-624`), which compiles it as
///   a TJS **expression** -- `SetText(result, buffer, NULL, true)`,
///   `tjsScriptBlock.cpp:230-237`, "the script will be compiled as an
///   expression if isexpression is true" -- and answers that expression's
///   value.  This is the form the engine's own text writer produces
///   (`%["key" => value, ...]`, and the reference documents the format as one
///   "that can be interpreted as an expression"), so a text struct written by
///   `saveStruct` reads back through it.  It is wired, not vestigial:
///   `TJSCreateTextStreamForRead = TVPCreateTextStreamForRead`
///   (`base/ScriptMgnIntf.cpp:486`).
///
/// Two deliberate limits.  The reference gates the text path on `if(result)` --
/// the caller's result pointer, which a published native handler here never
/// sees -- so this implementation always runs it; a statement-position call
/// therefore evaluates the file where the reference would do nothing (the
/// receiver is cleared in both cases).  And the `key = value` line format is
/// not read: that parser was this crate's invention, not the reference's text
/// form.  An unreadable path, and a text stream the host cannot provide, are
/// `TJS_E_INVALIDPARAM` (`:121`).
fn dictionary_load_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Dictionary.loadStruct")?;
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Err(TjsError::bad_param_count());
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let root = runtime.is_dictionary_instance(handle).then_some(handle);
    if let Some(root) = root {
        runtime.heap[root.0].members.clear();
    }
    if let Some(value) = load_binary_struct(runtime, &path, &mode, root)? {
        return Ok(value);
    }
    let Ok(text) = runtime.host_mut().read_text(&path, &mode) else {
        return Err(TjsError::invalid_param());
    };
    // `SetText` returns before it touches the result for an empty file
    // (`tjsScriptBlock.cpp:238-239`), so the call answers void.
    if text.trim().is_empty() {
        return Ok(Variant::Void);
    }
    let wrapped = format!("return ({text});");
    let file = compile_source_to_bytecode(&path, &wrapped)?;
    runtime.execute_file(&file)
}

/// Reads `path` and deserializes it when it holds a `KBAD100` struct pack.
///
/// `saveStruct` needs mode `"b"` to *write* the binary form, but KRKR's
/// `loadStruct` sniffs the header itself and accepts a binary pack in any mode
/// (`tjsDictionary.cpp:71-76` / `tjsArray.cpp:379-385`), returning the
/// deserialized root value rather than a success flag.  `None` means the path
/// is unreadable or is not a binary pack -- both are `TJS_E_INVALIDPARAM` to
/// the callers.
///
/// The mode reaches the storage layer with the read, which is what makes an
/// `o<size>` load work: the reference's binary stream seeks to the offset
/// before the header is sniffed (`base/BinaryStream.cpp:28-50`), and its text
/// stream does the same before it probes the BOM
/// (`base/TextStream.cpp:76-84`, `:97-140`) -- so a struct appended behind a
/// thumbnail is found by *where* it starts, never by the mode naming `b`.  The
/// `z` half of `BookMarkIO_Standard.load`'s `"o"+size` mode is likewise
/// content, not mode: the reader recognises the `fe fe 02` signature it finds
/// at the offset (`:97-140`), which is why a `"zo<size>"` write is read back by
/// an `"o<size>"` load.
fn load_binary_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    path: &str,
    mode: &str,
    root: Option<ObjectHandle>,
) -> Result<Option<Variant>> {
    let Ok(bytes) = runtime.host_mut().read_binary(path, mode) else {
        return Ok(None);
    };
    if !bytes.starts_with(BINARY_STRUCT_HEADER) {
        return Ok(None);
    }
    decode_binary_struct_with_root(runtime, &bytes, root)
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
    // `AssignStructure` reserves the destination for the incoming members
    // before the copy: count, `Owner->RebuildHash(reqcount)`, then copy
    // (`tjsDictionary.cpp:540-556`).
    let reqcount = runtime.heap[dest.0].members.len() as i64 + entries.len() as i64;
    runtime.heap[dest.0].members.rebuild(reqcount);
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
    // A member stored from `this` or `new` carries its binding; the value to
    // clone is the object behind it (`AsObjectNoAddRef`).
    let Some(handle) = value.object_handle() else {
        return Ok(value.clone());
    };
    if runtime.heap[handle.0].array_elements().is_some() {
        if !stack.insert(handle) {
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
        stack.remove(&handle);
        Ok(Variant::Object(dest))
    } else if is_dictionary_object(runtime, handle) {
        if !stack.insert(handle) {
            return Ok(Variant::Null);
        }
        let entries = dictionary_struct_entries(runtime, handle);
        let dest = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(dest, "Dictionary");
        for (key, value) in entries {
            let value = deep_clone_struct_value(runtime, &value, stack)?;
            runtime.heap[dest.0].set(key, value);
        }
        stack.remove(&handle);
        Ok(Variant::Object(dest))
    } else {
        Ok(value.clone())
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
            // A member stored from `this` or `new` carries its binding; the
            // serialized value is the object itself (`tTJSDictionary::
            // SaveStruct` walks the member variants and writes the object).
            // A member stored from `this` or `new` carries its binding; the
            // serialized value is what `tTJSVariantClosure::SelectObjectNoAddRef`
            // selects -- ObjThis when there is one (`tjsVariant.h:194`), which
            // for a self-bound value is the object itself
            // (`tjsDictionary.cpp:457`).
            Variant::Closure(closure) => {
                self.object(closure.this_obj.unwrap_or(closure.object), depth)
            }
            Variant::CodeObject(_) => "null".to_string(),
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
/// member table's order (the reference's bucket walk, `EnumMembers`).
/// The reference's `SaveStructuredData` and
/// `AssignStructure` run `EnumMembers` over the object's own symbols and skip
/// only `TJS_HIDDENMEMBER` (`tjsDictionary.cpp:410-420`, `:452-470`), so a key
/// named `clear` or `count` is ordinary data -- which is why nothing here may
/// filter by name.  Builtin methods are not in this map to begin with: they
/// live on the class object.
fn dictionary_struct_entries<H: TjsHost>(
    runtime: &Runtime<H>,
    handle: ObjectHandle,
) -> Vec<(String, Variant)> {
    runtime.heap[handle.0].member_entries()
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
            Variant::Closure(closure) => {
                self.object(closure.this_obj.unwrap_or(closure.object), out)?
            }
            Variant::CodeObject(_) => out.push(0xc0),
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
    decode_binary_struct_with_root(runtime, bytes, None)
}

/// Restores a struct pack, optionally *into* `root`.
///
/// The reference's serializer takes the destination container as its root
/// (`tTJSBinarySerializer(dic)`, `tjsDictionary.cpp:58-71`; the array ctor,
/// `tjsArray.cpp:383`): `CreateDictionary` sizes that dictionary's own table
/// with `RootDictionary->RebuildHash(count)` (`tjsBinarySerializer.cpp:80-88`)
/// and `ReadDictionary` then adds every member in the order the file holds them
/// (`:282-330` via `AddDictionary`'s `PropSetByVS`), while `CreateArray` hands
/// back the `RootArray` itself (`:104-112`).  Handing `root` here reproduces
/// that: the receiver is cleared by its caller, rebuilt at the count the stream
/// announces, and filled in file order -- one traversal, not a decode followed
/// by a copy in the decoded temporary's `EnumMembers` order.  A root of the
/// other kind is the reference's type mismatch
/// (`TJSThrowFrom_tjs_error(TJS_E_INVALIDPARAM)`, `:109-111`, `:90-92`); a
/// nested dictionary is still created fresh with `new Dictionary(count)`'s
/// sizing.
pub(crate) fn decode_binary_struct_with_root<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    bytes: &[u8],
    root: Option<ObjectHandle>,
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
        root_container: root,
    };
    decoder.value().map(Some)
}

struct BinaryStructDecoder<'a, H: TjsHost> {
    runtime: &'a mut Runtime<H>,
    bytes: &'a [u8],
    index: usize,
    /// The receiver a root container is restored into; taken by the outermost
    /// [`BinaryStructDecoder::value`] so only the top-level value can claim it,
    /// exactly like the reference's `RootDictionary`/`RootArray` being consumed
    /// by the `CreateDictionary`/`CreateArray` that reads them.
    root_container: Option<ObjectHandle>,
}

impl<'a, H: TjsHost + 'static> BinaryStructDecoder<'a, H> {
    fn value(&mut self) -> Result<Variant> {
        let root = self.root_container.take();
        self.value_into(root)
    }

    fn value_into(&mut self, root: Option<ObjectHandle>) -> Result<Variant> {
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
                self.array(len, root)
            }
            0xdd => {
                let len = self.read_u32()? as usize;
                self.array(len, root)
            }
            0xde => {
                let len = self.read_u16()? as usize;
                self.dictionary(len, root)
            }
            0xdf => {
                let len = self.read_u32()? as usize;
                self.dictionary(len, root)
            }
            0xa0..=0xbf => self.string((ty - 0xa0) as usize),
            0x90..=0x9f => self.array((ty - 0x90) as usize, root),
            0x80..=0x8f => self.dictionary((ty - 0x80) as usize, root),
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

    fn array(&mut self, len: usize, root: Option<ObjectHandle>) -> Result<Variant> {
        // `CreateArray` (`tjsBinarySerializer.cpp:104-112`): the receiver of a
        // top-level array stream is the `RootArray` itself -- its items were
        // cleared by the caller and are appended here in file order
        // (`InsertArray`'s `Add`, `:120-130`) -- and a dictionary root under an
        // array stream is the reference's type mismatch.
        let handle = match root {
            Some(handle) if self.runtime.heap[handle.0].array_elements().is_some() => handle,
            Some(_) => return Err(TjsError::invalid_param()),
            None => self.runtime.alloc_array_object(Vec::new()),
        };
        for _ in 0..len {
            let value = self.value()?;
            self.runtime.heap[handle.0].array_push(value);
        }
        Ok(Variant::Object(handle))
    }

    fn dictionary(&mut self, len: usize, root: Option<ObjectHandle>) -> Result<Variant> {
        // The root of a pack is restored *into the receiver* in file order:
        // `RootDictionary->RebuildHash(count)` sizes the receiver's table
        // (`tjsBinarySerializer.cpp:80-88`) and the entries are added in the
        // order the file holds them (`ReadDictionary`/`AddDictionary`,
        // `:282-330`).  A nested dictionary is created with its member count
        // (`CreateDictionary`'s `CreateNew(count)` path, `:90-98`), and a root
        // of the other kind is the same type mismatch as above.
        let handle = match root {
            Some(handle) if self.runtime.is_dictionary_instance(handle) => {
                self.runtime.heap[handle.0].members.rebuild(len as i64);
                handle
            }
            Some(_) => return Err(TjsError::invalid_param()),
            None => self.runtime.alloc_dictionary_object_sized(len as i64),
        };
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
    // `AsObjectNoAddRef()`: the object behind the value, whether it arrived as
    // a plain object or as a closure.  A host hands out self-bound values
    // (`tTJSVariant(objthis, objthis)`, the shape the reference gives `new`'s
    // result and its self-bound members), and the native reads the object the
    // binding points at.
    let handle = match value {
        Variant::Object(handle) => *handle,
        Variant::Closure(closure) => closure.object,
        _ => return None,
    };
    let object = &runtime.heap[handle.0];
    if matches!(object.get("pattern"), Variant::String(_))
        && !matches!(object.get("_compile"), Variant::Void)
    {
        Some(handle)
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
