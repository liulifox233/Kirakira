use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};

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
    runtime.register_object_native(handle, "assignStruct", dictionary_assign::<H>);
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

fn dictionary_save_struct<H: TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = require_this(this_obj, "Dictionary.saveStruct")?;
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Ok(Variant::Void);
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let mut serializer = DictionaryStructSerializer::new(runtime);
    let text = serializer.value(&Variant::Object(handle), 0);
    runtime.host_mut().write_text(&path, &mode, &text)?;
    Ok(Variant::Void)
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
    let Ok(text) = runtime.host_mut().read_text(&path, &mode) else {
        return Ok(Variant::Integer(0));
    };
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

struct DictionaryStructSerializer<'a, H: TjsHost> {
    runtime: &'a Runtime<H>,
    active: BTreeSet<ObjectHandle>,
}

impl<'a, H: TjsHost + 'static> DictionaryStructSerializer<'a, H> {
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
            Variant::Real(value) => value.to_string(),
            Variant::String(value) => tjs_quote(value),
            Variant::Octet(_) => "void".to_string(),
            Variant::Object(handle) => self.object(*handle, depth),
            Variant::Closure(_) | Variant::CodeObject(_) => "void".to_string(),
        }
    }

    fn object(&mut self, handle: ObjectHandle, depth: usize) -> String {
        if !self.active.insert(handle) {
            return "void".to_string();
        }
        let text = if let Some(elements) = self.runtime.heap[handle.0].array_elements() {
            let elements = elements
                .iter()
                .map(|value| self.value(value, depth + 1))
                .collect::<Vec<_>>();
            format!("[{}]", elements.join(", "))
        } else {
            let entries = self.runtime.heap[handle.0]
                .members
                .iter()
                .filter(|(key, _)| !is_native_member_name(key))
                .map(|(key, value)| {
                    format!("{} => {}", tjs_quote(key), self.value(value, depth + 1))
                })
                .collect::<Vec<_>>();
            format!("%[{}]", entries.join(", "))
        };
        self.active.remove(&handle);
        text
    }
}

fn is_native_member_name(key: &str) -> bool {
    matches!(
        key,
        "clear" | "assign" | "assignStruct" | "saveStruct" | "loadStruct"
    )
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
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
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
