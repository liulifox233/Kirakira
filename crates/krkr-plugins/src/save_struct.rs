//! `savestruct.dll` — TJS struct (de)serialization for save data.
//!
//! Reference: krkrz `src/plugins/win32/saveStruct/Main.cpp` (346 lines) and its
//! krkr2 trunk revision, which adds `Scripts.toStructString(target, newline,
//! option)` and the `ssoIndent`/`ssoConst`/`ssoSort`/`ssoHidden` option
//! globals. PARQUET links `savestruct.dll` by name although the same group is
//! compiled into `PackinOne.dll`, so this module installs the surface under
//! that DLL's own name.
//!
//! Surface (reference registration, `Main.cpp:253-257,307-310`):
//!
//! - `Array.save2(filename, utf8=false, newline=0)` — one element per line,
//!   written as its plain string form (no quotes, no escaping).
//! - `Array.saveStruct2(filename, utf8=false, newline=0)` / `Array.toStructString(newline=0)`
//!   — the whole array as `[value,...]`.
//! - `Dictionary.saveStruct2(filename, utf8=false, newline=0)` /
//!   `Dictionary.toStructString(newline=0)` — `%["key"=>value,...]`, one entry
//!   per line.
//! - `Scripts.toStructString(target, newline=1, option=0)` (trunk) — the same
//!   text for any target value, returned as a String.
//!
//! Wire format, as the reference writes it: strings get only `\"` and `\\`
//! escapes, octets become `<% 61 62 %>` (lowercase), integers `int 5`, reals
//! `real 0x1.8000000000000p+0 /* 1.5 */`, void/null verbatim, and any object
//! that is not an Array is written as a dictionary. Files are meant to be read
//! back by *evaluating* them, which works because TJS lexes `int`/`real` as
//! unary casts; there is no loader in this plugin.
//!
//! This DLL is not the reference's *built-in* `Dictionary.saveStruct` /
//! `Array.saveStruct`, which takes a stream mode string instead of a
//! `newline`/`option` pair: `b` anywhere in it selects the `KBAD100\0` binary
//! pack and everything else the text stream, whose factory reads `o<size>` as
//! a seek and `z` as zlib compression
//! (`tjsDictionary.cpp:133-179`, `base/BinaryStream.cpp:52-76`,
//! `base/TextStream.cpp:343-640`).  That member lives in `krkr-tjs2`'s runtime
//! (`save_structured_value`), and KAGEX's `BookMarkIO_Standard` bookmarks --
//! `[BMP][struct]` with mode `"zo<size>"` -- go through it, never through
//! `saveStruct2`.
//!
//! Deliberate divergences:
//!
//! - `newline` 0 selects CRLF and 1 LF, as in krkrz; `toStructString` defaults
//!   to 0 (CRLF) on the Array/Dictionary members and to 1 (LF) on the trunk's
//!   `Scripts.toStructString`, matching each version's own default.
//! - Writes go through the engine's text storage path, which is UTF-8, so the
//!   `utf8` flag cannot select the ANSI code page; a failed write is logged
//!   and, like the reference, does not raise.
//! - `save2` converts scalar elements to text instead of failing on a
//!   non-String element (the reference's strict `GetString` throws).
//! - A dictionary member is never skipped by name: the reference filters by
//!   the `TJS_HIDDENMEMBER` flag, and this object model's dictionary
//!   instances carry only data, so `%["count" => 1]` serializes its key. Array
//!   instances, whose member map also holds the built-in Array methods, skip
//!   those (`scripts_ex::is_hidden_member`).
//! - `ssoHidden` therefore has no effect: only `__`-prefixed bookkeeping and
//!   built-in method members are hidden here, and no object exposes flags.
//!   `ssoSort` is inherent (members are kept sorted), `ssoIndent` and
//!   `ssoConst` are honoured.
//! - `Dictionary.toStructString`/`saveStruct2` stay on the class object, as
//!   the reference's `TJS_STATICMEMBER` registration requires, so a dictionary
//!   is serialized with `(Dictionary.toStructString incontextof dict)(...)`.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Array.save2/saveStruct2/toStructString, Dictionary.saveStruct2/toStructString, \
              Scripts.toStructString, sso* option globals",
    notes: "The reference wire format (int/real/octet escapes, dictionary-per-line) is written \
            through the engine text storage path; save2 accepts scalar elements, and ssoHidden \
            cannot apply because member flags are not exposed by this object model.",
    install: |engine| engine.register_plugin(SaveStructPlugin),
};

pub struct SaveStructPlugin;

impl KrkrPlugin for SaveStructPlugin {
    fn name(&self) -> &str {
        "savestruct.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_save_struct(runtime);
        runtime.host_mut().log(
            "savestruct.dll registered: Array/Dictionary struct writers and Scripts.toStructString",
        );
        Ok(())
    }
}

/// The trunk's option bits (`trunk/Main.cpp:5-14`).
const SSO_INDENT: i64 = 1;
const SSO_CONST: i64 = 2;
const SSO_SORT: i64 = 4;
const SSO_HIDDEN: i64 = 8;

/// The members this plugin installs on `Array`. Shared with the surface test.
#[cfg(test)]
const ARRAY_MEMBERS: &[&str] = &["save2", "saveStruct2", "toStructString"];
/// The members this plugin installs on `Dictionary`.
#[cfg(test)]
const DICTIONARY_MEMBERS: &[&str] = &["saveStruct2", "toStructString"];

fn install_save_struct(runtime: &mut Runtime<KrkrHost>) {
    install_array_writer(runtime);
    install_dictionary_writer(runtime);
    install_scripts_writer(runtime);
    for (name, value) in [
        ("ssoIndent", SSO_INDENT),
        ("ssoConst", SSO_CONST),
        ("ssoSort", SSO_SORT),
        ("ssoHidden", SSO_HIDDEN),
    ] {
        runtime.set_global_member(name, Variant::Integer(value));
    }
}

// ---------------------------------------------------------------------------
// Registration

/// Registers the Array writers on the class object and on every instance.
///
/// The reference registers them without `TJS_STATICMEMBER`, so
/// `tTJSNativeClass::CreateNew` copies them into each array instance
/// (`tjsNative.cpp:347`). This engine's arrays are built by the `Array`
/// constructor, so the constructor is wrapped: every instance it produces
/// (including `[...]` literals, which compile to a call to the global `Array`)
/// receives the three members.
///
/// The engine's own `Array` class object carries the builtin surface
/// (`add`, `saveStruct`, ...), and scripts read it off the *class object*:
/// KAGEX's `checkSave` writes its probe through
/// `(Array.saveStruct incontextof array)(...)` (KAGEX `system/MainWindow.tjs`
/// `checkSave`). Replacing the global with a bare wrapper made those lookups
/// fail (`checkSave失敗 : Member "saveStruct" does not exist`) and sent the
/// game's `saveDataLocation` to the personal path, so the wrapper inherits
/// every member the class object had.
fn install_array_writer(runtime: &mut Runtime<KrkrHost>) {
    let Variant::Object(class) = runtime.global_member("Array") else {
        return;
    };
    let builtin_members = runtime.object_members(class);
    install_array_members(runtime, class);
    let original = runtime.global_member("Array");
    let constructor = runtime.alloc_native_function(
        move |runtime: &mut Runtime<KrkrHost>, _this_obj, args: Vec<Variant>| {
            let value = runtime.call_function(original.clone(), args)?;
            if let Some(instance) = value.object_handle() {
                install_array_members(runtime, instance);
            }
            Ok(value)
        },
    );
    runtime.add_object_class_info(constructor, "Array");
    runtime.set_global_member("Array", Variant::Object(constructor));
    // The class object is now the wrapper, so the members have to live there
    // too: `Array.save2` and the manual's `(Array.save2 incontextof arr)(...)`
    // both read them off the class object.
    install_array_members(runtime, constructor);
    for (name, value) in builtin_members {
        if matches!(runtime.object_member(constructor, &name), Variant::Void) {
            runtime.set_object_member(constructor, name, value);
        }
    }
}

fn install_array_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "save2", array_save2);
    runtime.register_object_native(handle, "saveStruct2", array_save_struct2);
    runtime.register_object_native(handle, "toStructString", array_to_struct_string);
}

/// The reference registers both Dictionary writers with `TJS_STATICMEMBER`, so
/// they stay on the class object and a dictionary reaches them through
/// `(Dictionary.saveStruct2 incontextof dict)(...)`.
fn install_dictionary_writer(runtime: &mut Runtime<KrkrHost>) {
    let Variant::Object(class) = runtime.global_member("Dictionary") else {
        return;
    };
    runtime.register_object_native(class, "saveStruct2", dictionary_save_struct2);
    runtime.register_object_native(class, "toStructString", dictionary_to_struct_string);
}

/// The trunk's `Scripts.toStructString(target, newline=1, option=0)`
/// (`trunk/Main.cpp:363-380`).
fn install_scripts_writer(runtime: &mut Runtime<KrkrHost>) {
    let scripts = match runtime.global_member("Scripts") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "Scripts");
            runtime.set_global_member("Scripts", Variant::Object(handle));
            handle
        }
    };
    runtime.register_object_native(scripts, "toStructString", scripts_to_struct_string);
}

// ---------------------------------------------------------------------------
// Handlers

fn array_save2(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let file = required_filename(&args, "Array.save2")?;
    let settings = Settings::from_args(&args, 1, 0);
    let Some(array) = receiver(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let elements = runtime
        .array_elements(array)
        .map(Vec::from)
        .unwrap_or_default();
    let mut text = String::new();
    for element in elements {
        // `save2` writes the element's plain string form, one per line, with
        // no quoting or escaping (`Main.cpp:186-211`).
        text.push_str(&element.to_tjs_string()?);
        text.push_str(settings.newline);
    }
    write_struct_storage(runtime, &file, &text);
    Ok(Variant::Void)
}

fn array_save_struct2(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let file = required_filename(&args, "Array.saveStruct2")?;
    let settings = Settings::from_args(&args, 1, 0);
    let Some(array) = receiver(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let text = serialize(runtime, &Variant::Object(array), &settings)?;
    write_struct_storage(runtime, &file, &text);
    Ok(Variant::Void)
}

fn array_to_struct_string(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `toStructString` accepts zero arguments (`Main.cpp:239-252`).
    let settings = Settings::from_args(&args, 0, 0);
    let Some(array) = receiver(runtime, this_obj) else {
        return Ok(Variant::String(String::new()));
    };
    Ok(Variant::String(serialize(
        runtime,
        &Variant::Object(array),
        &settings,
    )?))
}

fn dictionary_save_struct2(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let file = required_filename(&args, "Dictionary.saveStruct2")?;
    let settings = Settings::from_args(&args, 1, 0);
    let Some(dictionary) = receiver(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let text = serialize(runtime, &Variant::Object(dictionary), &settings)?;
    write_struct_storage(runtime, &file, &text);
    Ok(Variant::Void)
}

fn dictionary_to_struct_string(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let settings = Settings::from_args(&args, 0, 0);
    let Some(dictionary) = receiver(runtime, this_obj) else {
        return Ok(Variant::String(String::new()));
    };
    Ok(Variant::String(serialize(
        runtime,
        &Variant::Object(dictionary),
        &settings,
    )?))
}

fn scripts_to_struct_string(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // The trunk's `Scripts.toStructString` requires the target and defaults
    // `newline` to 1 (LF) (`trunk/Main.cpp:367-371`).
    let Some(target) = args.first().cloned() else {
        return Err(TjsError::bad_param_count());
    };
    let settings = Settings::from_args(&args, 1, 1);
    let settings = settings.with_options(option_arg(&args, 2));
    Ok(Variant::String(serialize(runtime, &target, &settings)?))
}

// ---------------------------------------------------------------------------
// Shared helpers

/// The object a member call acts on, resolving a bound receiver.
fn receiver(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Option<ObjectHandle> {
    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

fn required_filename(args: &[Variant], what: &str) -> Result<String> {
    match args.first() {
        Some(value) => value.to_tjs_string(),
        None => {
            let _ = what;
            Err(TjsError::bad_param_count())
        }
    }
}

fn option_arg(args: &[Variant], index: usize) -> i64 {
    match args.get(index) {
        Some(Variant::Void) | None => 0,
        Some(value) => value.to_integer().unwrap_or(0),
    }
}

/// Writes the serialized document. The reference creates the stream with
/// `TVPCreateIStream` and silently writes nothing when that fails
/// (`Main.cpp:186-211`); a failure here is logged and, like the reference,
/// does not raise.
fn write_struct_storage(runtime: &mut Runtime<KrkrHost>, name: &str, text: &str) {
    if let Err(error) = runtime.host_mut().write_text_storage(name, "w", text) {
        runtime
            .host_mut()
            .log(&format!("savestruct.dll: cannot write {name}: {error}"));
    }
}

/// Serialization settings: the newline string and the trunk's option bits.
struct Settings {
    newline: &'static str,
    options: i64,
}

impl Settings {
    /// `newline` is 0 for CRLF and 1 for LF; any other value means CRLF, and
    /// an explicit `void` argument counts as absent (`Main.cpp:191-194`).
    fn from_args(args: &[Variant], newline_index: usize, default: i64) -> Self {
        let newline = match args.get(newline_index) {
            Some(Variant::Void) | None => default,
            Some(value) => value.to_integer().unwrap_or(default),
        };
        Self {
            newline: if newline == 1 { "\n" } else { "\r\n" },
            options: 0,
        }
    }

    fn with_options(mut self, options: i64) -> Self {
        self.options = options;
        self
    }

    fn indented(&self) -> bool {
        self.options & SSO_INDENT != 0
    }

    fn constant_prefix(&self) -> bool {
        self.options & SSO_CONST != 0
    }
}

fn serialize(
    runtime: &mut Runtime<KrkrHost>,
    value: &Variant,
    settings: &Settings,
) -> Result<String> {
    let mut out = String::new();
    write_value(runtime, &mut out, value, settings)?;
    Ok(out)
}

fn write_value(
    runtime: &mut Runtime<KrkrHost>,
    out: &mut String,
    value: &Variant,
    settings: &Settings,
) -> Result<()> {
    match value {
        Variant::Void => out.push_str("void"),
        Variant::Null => out.push_str("null"),
        Variant::String(text) => write_quoted(out, text),
        Variant::Octet(bytes) => {
            out.push_str("<% ");
            for byte in bytes {
                out.push_str(&format!("{byte:02x} "));
            }
            out.push_str("%>");
        }
        Variant::Integer(number) => {
            if !settings.constant_prefix() {
                out.push_str("int ");
            }
            out.push_str(&number.to_string());
        }
        Variant::Real(number) => {
            if !settings.constant_prefix() {
                out.push_str("real ");
            }
            out.push_str(&real_hex_string(*number));
            out.push_str(" /* ");
            out.push_str(&real_decimal_string(*number));
            out.push_str(" */");
        }
        Variant::Object(_) | Variant::Closure(_) => {
            let Some(handle) = value.object_handle() else {
                out.push_str("null");
                return Ok(());
            };
            if runtime.array_elements(handle).is_some() {
                write_array(runtime, out, handle, settings)?;
            } else {
                write_dictionary(runtime, out, handle, settings)?;
            }
        }
        Variant::CodeObject(_) => out.push_str("void"),
    }
    Ok(())
}

/// `quoteString` (`Main.cpp:5-25`): only `"` and `\` are escaped.
fn write_quoted(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out.push('"');
}

fn write_array(
    runtime: &mut Runtime<KrkrHost>,
    out: &mut String,
    array: ObjectHandle,
    settings: &Settings,
) -> Result<()> {
    let elements: Vec<Variant> = runtime
        .array_elements(array)
        .map(Vec::from)
        .unwrap_or_default();
    if settings.constant_prefix() {
        out.push_str("(const)");
    }
    out.push('[');
    for (index, element) in elements.iter().enumerate() {
        if index > 0 {
            out.push(',');
            if settings.indented() {
                out.push_str(settings.newline);
            }
        }
        write_value(runtime, out, element, settings)?;
    }
    out.push(']');
    Ok(())
}

/// Any object that is not an Array is written as a dictionary, exactly as the
/// reference's `getDictString` does (`Main.cpp:83-93`). Entries are separated
/// by `,` + newline in both versions.
fn write_dictionary(
    runtime: &mut Runtime<KrkrHost>,
    out: &mut String,
    dictionary: ObjectHandle,
    settings: &Settings,
) -> Result<()> {
    let members: Vec<(String, Variant)> = runtime
        .object_members(dictionary)
        .into_iter()
        .filter(|(key, _)| {
            settings.options & SSO_HIDDEN != 0
                || !crate::scripts_ex::is_hidden_member(runtime, dictionary, key)
        })
        .collect();
    if settings.constant_prefix() {
        out.push_str("(const)");
    }
    out.push_str("%[");
    for (index, (key, value)) in members.iter().enumerate() {
        if index > 0 {
            out.push(',');
            out.push_str(settings.newline);
            if settings.indented() {
                out.push_str("  ");
            }
        }
        // `write_quoted` already closed the key with `"`, so the reference's
        // `"=>` makes the full `"key"=>value` spelling.
        write_quoted(out, key);
        out.push_str("=>");
        write_value(runtime, out, value, settings)?;
    }
    out.push(']');
    Ok(())
}

/// `TJSRealToHexString` (`tjsVariant.cpp:259-306`), the same spelling the
/// engine writes for its own real literals (`real_hex_literal`,
/// `krkr-tjs2/src/runtime/builtins.rs:1341`): the special forms for
/// NaN/infinities/zeros, otherwise the sign, `0x1.`, the 13 significand hex
/// digits and `p` + the IEEE exponent as `%d` — no `+`, and a subnormal keeps
/// its raw significand with exponent `-1023`, exactly as the reference prints
/// it.
fn real_hex_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity".to_string()
        } else {
            "+Infinity".to_string()
        };
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "+0.0".to_string()
        };
    }
    let bits = value.to_bits();
    let prefix = if bits >> 63 == 1 { "-0x1." } else { "0x1." };
    let exponent = ((bits >> 52) & 0x7ff) as i64 - 1023;
    let fraction = bits & 0x000F_FFFF_FFFF_FFFF;
    format!("{prefix}{fraction:013X}p{exponent}")
}

/// `TJSRealToString` (`tjsVariant.cpp:228-256`) is `%.15lg`; the comment after
/// the hex form is informational, so a 15-significant-digit rendering in the
/// same scientific/plain split is close enough to read back.
fn real_decimal_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity".to_string()
        } else {
            "+Infinity".to_string()
        };
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "+0.0".to_string()
        };
    }
    let exponent = value.abs().log10().floor() as i32;
    if !(-4..15).contains(&exponent) {
        let text = format!("{:.14e}", value);
        let (mantissa, exponent) = text.split_once('e').unwrap_or((text.as_str(), "0"));
        let mantissa = trim_trailing_zeros(mantissa);
        let exponent: i32 = exponent.parse().unwrap_or(0);
        return format!("{mantissa}e{exponent:+03}");
    }
    let decimals = (14 - exponent).max(0) as usize;
    trim_trailing_zeros(&format!("{value:.decimals$}")).to_string()
}

fn trim_trailing_zeros(text: &str) -> &str {
    if !text.contains('.') {
        return text;
    }
    text.trim_end_matches('0').trim_end_matches('.')
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, time::SystemTime};

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths};
    use krkr_tjs2::runtime::Variant;

    use super::*;

    #[test]
    fn every_reference_member_is_installed() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        for name in ARRAY_MEMBERS {
            let value = engine
                .execute_expression("inline.tjs", &format!("typeof (new Array()).{name}"))
                .expect("array member probe");
            assert_ne!(
                value,
                Variant::String("undefined".to_string()),
                "Array.{name} is not installed on instances"
            );
            let value = engine
                .execute_expression("inline.tjs", &format!("typeof Array.{name}"))
                .expect("array class probe");
            assert_ne!(
                value,
                Variant::String("undefined".to_string()),
                "Array.{name} is not installed on the class object"
            );
        }
        for name in DICTIONARY_MEMBERS {
            let value = engine
                .execute_expression("inline.tjs", &format!("typeof Dictionary.{name}"))
                .expect("dictionary member probe");
            assert_ne!(
                value,
                Variant::String("undefined".to_string()),
                "Dictionary.{name} is not installed"
            );
        }
        assert_ne!(
            engine
                .execute_expression("inline.tjs", "typeof Scripts.toStructString")
                .expect("scripts member probe"),
            Variant::String("undefined".to_string())
        );
        for (name, value) in [
            ("ssoIndent", 1),
            ("ssoConst", 2),
            ("ssoSort", 4),
            ("ssoHidden", 8),
        ] {
            assert_eq!(
                engine
                    .execute_expression("inline.tjs", name)
                    .expect("option global"),
                Variant::Integer(value)
            );
        }
    }

    /// KAGEX's `checkSave` writes its probe *through the class object* —
    /// `(Array.saveStruct incontextof array)("<dir>savecheck")`
    /// (`system/MainWindow.tjs` `checkSave`) — and reads `Array.add` and the
    /// kindred builtins off it elsewhere. The wrapper the three writers need
    /// replaces the global `Array`, so it has to keep the builtin surface: a
    /// bare wrapper turned those lookups into `checkSave失敗 : Member
    /// "saveStruct" does not exist` at boot and sent the game's
    /// `saveDataLocation` to the personal path.
    ///
    /// The round trip below writes that probe both ways and reads each form
    /// with the reference's own reader: `Array.saveStruct(path)` writes text
    /// unless the mode names `b` (`tjsArray.cpp:483-491`), and
    /// `Array.loadStruct` reads the *binary* container and only that
    /// (`:379-385`, `:400-402`) — the text form is read by
    /// `Scripts.evalStorage`, the evaluator the whole text format exists for
    /// (`crates/krkr-engine/src/native/scripts.rs:123`).
    #[test]
    fn array_class_object_keeps_the_builtin_surface() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        for name in ["add", "save", "saveStruct", "load", "loadStruct", "assign"] {
            let value = engine
                .execute_expression("inline.tjs", &format!("typeof Array.{name}"))
                .expect("class member probe");
            assert_ne!(
                value,
                Variant::String("undefined".to_string()),
                "Array.{name} is not installed on the class object"
            );
        }
        for name in ARRAY_MEMBERS {
            let value = engine
                .execute_expression("inline.tjs", &format!("typeof Array.{name}"))
                .expect("class member probe");
            assert_ne!(
                value,
                Variant::String("undefined".to_string()),
                "Array.{name} is not installed on the class object"
            );
        }

        // The idiom itself has to work end to end, and a plain instance keeps
        // working from the same wrapper: write the probe the way `checkSave`
        // does, then use the instance methods.
        let root = test_root("savestruct-class-surface");
        let mut engine = test_engine(&root);
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var probe = new Array();
                    probe.add(7);
                    (Array.saveStruct incontextof probe)("savecheck");
                    (Array.saveStruct incontextof probe)("savecheck.ksd", "b");
                    var loaded = [];
                    (Array.loadStruct incontextof loaded)("savecheck.ksd");
                    var items = [];
                    items.add(1);
                    items.add(2);
                    return loaded.count + ":" + loaded[0] + ":" + items.count + ":" +
                        Scripts.evalStorage("savecheck")[0];
                })()"#,
            )
            .expect("class surface round trip");
        assert_eq!(value, Variant::String("1:7:2:7".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn to_struct_string_writes_the_reference_wire_format() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var array = [\"text\", 5, void, null, <%61 62 %>];\n\
                     return array.toStructString(1);\n\
                 })()",
            )
            .expect("array struct text");

        assert_eq!(
            value,
            Variant::String("[\"text\",int 5,void,null,<% 61 62 %>]".to_string())
        );
    }

    #[test]
    fn dictionary_text_is_one_entry_per_line() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var data = %[\"answer\" => 42, \"flag\" => true];\n\
                     return (Dictionary.toStructString incontextof data)(1);\n\
                 })()",
            )
            .expect("dictionary struct text");

        assert_eq!(
            value,
            Variant::String("%[\"answer\"=>int 42,\n\"flag\"=>int 1]".to_string())
        );
    }

    /// Serializing must not drop a dictionary field whose name happens to look
    /// like an Array/Dictionary method: the reference filters by the
    /// `TJS_HIDDENMEMBER` flag, and a dictionary here has only data members.
    #[test]
    fn serializer_keeps_dictionary_keys_named_like_builtins() {
        let root = test_root("savestruct-keys");
        let mut engine = test_engine(&root);
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var data = %[\"count\" => 1, \"length\" => 2, \"save\" => 3];\n\
                     (Dictionary.saveStruct2 incontextof data)(\"keys.ksd\");\n\
                     return (Dictionary.toStructString incontextof data)();\n\
                 })()",
            )
            .expect("serialize builtin-named keys");

        assert_eq!(
            value,
            Variant::String(
                // `EnumMembers` order, i.e. the member table's bucket walk:
                // `save` hashes into slot 0 of the default eight-slot table,
                // `length` and `count` share slot 1 with `length` chained in
                // front of the slot-holding `count`.
                "%[\"save\"=>int 3,\r\n\"length\"=>int 2,\r\n\"count\"=>int 1]".to_string()
            )
        );
        let bytes = fs::read(root.join("keys.ksd")).expect("read the saved struct");
        assert_eq!(
            decode_utf16_text(&bytes),
            value.to_tjs_string().expect("struct text")
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn save_struct2_and_save2_round_trip_through_storage() {
        let root = test_root("savestruct-round-trip");
        let mut engine = test_engine(&root);
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var data = %[\"name\" => \"soldier\", \"values\" => [1, 2]];\n\
                     (Dictionary.saveStruct2 incontextof data)(\"state.ksd\");\n\
                     var lines = [\"first\", \"second\"];\n\
                     lines.save2(\"lines.txt\");\n\
                     return \"ok\";\n\
                 })()",
            )
            .expect("write struct storages");
        assert_eq!(value, Variant::String("ok".to_string()));

        // The engine's text writer stores a KRKR text stream (UTF-16LE behind
        // a BOM), which is exactly what the reference format's reader —
        // `evalStorage` — decodes back.
        let struct_bytes = fs::read(root.join("state.ksd")).expect("read struct");
        assert_eq!(&struct_bytes[..2], &[0xff, 0xfe], "text stream BOM");
        assert_eq!(
            decode_utf16_text(&struct_bytes),
            "%[\"name\"=>\"soldier\",\r\n\"values\"=>[int 1,int 2]]"
        );
        let lines_bytes = fs::read(root.join("lines.txt")).expect("read lines");
        assert_eq!(decode_utf16_text(&lines_bytes), "first\r\nsecond\r\n");

        // The reference format is meant to be read back by evaluating it.
        let value = engine
            .execute_expression(
                "inline.tjs",
                "Scripts.evalStorage(\"state.ksd\").values[1] + Scripts.evalStorage(\"state.ksd\").name.length",
            )
            .expect("evaluate the saved struct");
        assert_eq!(value, Variant::Integer(2 + 7));
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// A binary `loadStruct` restores the receiver **in place, in file order**
    /// (`tTJSBinarySerializer`'s `RootDictionary`: `RootDictionary->RebuildHash(count)`,
    /// `tjsBinarySerializer.cpp:80-88`, then `AddDictionary`'s `PropSetByVS` per
    /// entry in the order the file holds them, `:282-330`).  Decoding into a
    /// temporary and copying the temporary's `EnumMembers` order would walk the
    /// buckets a second time and reverse the members of a colliding slot.
    ///
    /// The five keys below all hash into one bucket, so the file holds them as
    /// the saving dictionary enumerated them (`ab,y,w,l,e`) and the reference
    /// root -- `RebuildHash(5)`, sixteen slots -- enumerates
    /// `e,w,l,y,ab`.
    #[test]
    fn binary_load_struct_restores_the_receiver_in_file_order() {
        let root = test_root("savestruct-file-order");
        let mut engine = test_engine(&root);
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var saved = %[];\n\
                     saved.e = 1; saved.l = 2; saved.w = 3; saved.y = 4; saved.ab = 5;\n\
                     (Dictionary.saveStruct incontextof saved)(\"order.ksd\", \"b\");\n\
                     var loaded = %[];\n\
                     (Dictionary.loadStruct incontextof loaded)(\"order.ksd\", \"b\");\n\
                     return Scripts.getObjectKeys(loaded).join(\",\") + \":\" +\n\
                         loaded.e + loaded.w + loaded.ab;\n\
                 })()",
            )
            .expect("binary struct round trip");

        assert_eq!(value, Variant::String("e,w,l,y,ab:135".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn newlines_and_escapes_follow_the_reference() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var text = %[\"path\" => \"c:\\\\save \\\"x\\\"\"];\n\
                     return (Dictionary.toStructString incontextof text)();\n\
                 })()",
            )
            .expect("escaped dictionary");

        assert_eq!(
            value,
            Variant::String("%[\"path\"=>\"c:\\\\save \\\"x\\\"\"]".to_string())
        );
    }

    #[test]
    fn real_values_use_the_hex_form_and_scripts_writer_takes_a_target() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var data = %[\"scale\" => 1.5];\n\
                     var text = Scripts.toStructString(data, 1);\n\
                     var lines = Scripts.toStructString([1, 2], 1);\n\
                     var two = %[\"first\" => 1, \"second\" => 2];\n\
                     var lf = Scripts.toStructString(two, 1);\n\
                     var crlf = Scripts.toStructString(two, 0);\n\
                     return text + \"|\" + lines + \"|\" + (lf.indexOf(\"\\r\\n\") == -1) + \":\" +\n\
                         (crlf.indexOf(\"\\r\\n\") != -1) + \":\" +\n\
                         (Scripts.toStructString(%[\"a\" => 1, \"b\" => 2]).indexOf(\"\\n\") != -1);\n\
                 })()",
            )
            .expect("scripts struct text");

        assert_eq!(
            value,
            Variant::String(
                "%[\"scale\"=>real 0x1.8000000000000p0 /* 1.5 */]|[int 1,int 2]|1:1:1".to_string()
            )
        );
    }

    #[test]
    fn real_specials_and_subnormals_follow_the_reference_spelling() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var text = Scripts.toStructString(%[\"zero\" => 0.0, \"negative\" => -1.5], 1);\n\
                     var tiny = Scripts.toStructString(%[\"small\" => 5e-324], 1);\n\
                     var negzero = Scripts.toStructString(%[\"minus\" => -0.0], 1);\n\
                     return text + \"|\" + tiny + \"|\" + negzero;\n\
                 })()",
            )
            .expect("real specials");

        assert_eq!(
            value,
            Variant::String(
                "%[\"negative\"=>real -0x1.8000000000000p0 /* -1.5 */,\n\
                 \"zero\"=>real +0.0 /* +0.0 */]|\
                 %[\"small\"=>real 0x1.0000000000001p-1023 /* 4.94065645841247e-324 */]|\
                 %[\"minus\"=>real -0.0 /* -0.0 */]"
                    .to_string()
            )
        );
    }

    #[test]
    fn save_members_require_a_filename() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        for call in [
            "(new Array()).save2()",
            "(new Array()).saveStruct2()",
            "(Dictionary.saveStruct2 incontextof %[\"a\" => 1])()",
            "Scripts.toStructString()",
        ] {
            let error = engine
                .execute_expression("inline.tjs", call)
                .expect_err("a missing filename must fail");
            assert_eq!(error.message, "Invalid argument count", "{call}");
        }
    }

    /// The `[BMP][struct]` bookmark KAGEX's `BookMarkIO_Standard` writes, byte
    /// for byte.
    ///
    /// GINKA's `main/Config.tjs` sets `saveThumbnail = 1` -- so the getter at
    /// `system/MainWindow.tjs` object 464 picks `BookMarkIO_Standard` -- plus
    /// `thumbnailWidth = 137`, `thumbnailDepth = 24`, `scWidth/scHeight =
    /// 1280x720` and `saveDataMode = debugWindowEnabled ? "" : "z"`.  The
    /// standard saver (object 467) is
    ///
    /// ```text
    /// if (layer) layer.saveLayerImage(file, "bmp" + thumbnailDepth);
    /// (Dictionary.saveStruct incontextof data)(file, saveDataMode + "o" + info.size);
    /// ```
    ///
    /// and `BookMarkIO_Standard(dict)` (object 466) fills `info.size` with
    /// `(width * 3 + 3 >> 2 << 2) * height + 54` -- the *length of the BMP*,
    /// because the struct is appended at its end.  `info.height` is
    /// `(int)(thumbnailWidth * scHeight / scWidth)` = 77, so the file is a
    /// 137x77 24bpp BMP (54 + 412 * 77 = 31778 bytes) followed by the text
    /// struct at offset 31778, written in mode `"zo31778"`: the stream factory
    /// seeks the *existing* file to that offset (`base/TextStream.cpp:417`,
    /// `base/BinaryStream.cpp:52-76`) and then writes the `fe fe 02`
    /// crypt/compress signature, the UTF-16LE BOM, the compressed and the
    /// uncompressed size and the deflate stream (`:428-462`, `:487-489`).
    ///
    /// Loading is `safeEvalStorage(file, "o" + info.size)` (object 468) --
    /// KAGEX's `safeEvalStorage(data, *)` forwards the mode to
    /// `Scripts.safeEvalStorage`/`Scripts.evalStorage` -- which positions the
    /// read stream before it probes the BOM (`base/TextStream.cpp:76-140`).
    /// The test runs that whole chain too: `PackinOne`'s `Scripts.safeEvalStorage`
    /// forwards its own arguments to `evalStorage`, so the mode arrives as the
    /// `o<size>` read the game's loader performs.
    #[test]
    fn bookmarkio_standard_appends_a_compressed_struct_after_the_bmp() {
        let root = test_root("bookmarkio-standard");
        let mut engine = test_engine(&root);
        engine.register_plugin(SaveStructPlugin).expect("plugin");
        engine
            .register_plugin(crate::packinone::PackinOnePlugin)
            .expect("packinone plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var layer = new Layer();
                    layer.setImageSize(137, 77);
                    layer.fillRect(0, 0, 137, 77, 0x204080);
                    layer.saveLayerImage("savedata/data0.bmp", "bmp24");
                    layer.saveLayerImage("savedata/thumb-only.bmp", "bmp24");
                    var size = (((137 * 3 + 3) >> 2) << 2) * 77 + 54;
                    var data = %["id" => "GINKA", "core" => %["curLine" => 12]];
                    (Dictionary.saveStruct incontextof data)(
                        "savedata/data0.bmp", "z" + "o" + size);
                    (Dictionary.saveStruct incontextof data)("savedata/plain.ksd");
                    var viaEval = Scripts.evalStorage("savedata/data0.bmp", "o" + size);
                    var viaLoad = (Dictionary.loadStruct incontextof %[])(
                        "savedata/data0.bmp", "o" + size);
                    var viaSafe = Scripts.safeEvalStorage("savedata/data0.bmp", "o" + size);
                    // The `-debugwin=no` shape of the same call: `saveDataMode`
                    // is `""`, so the mode is the bare `"o" + size` and the
                    // struct is the plain text stream at the offset.  The
                    // shipped default is the other one (`startup.tjs` leaves
                    // `debugWindowEnabled` at `inXP3archivePacked` = 1), and the
                    // reader is content-driven, so both load through the same
                    // `evalStorage(file, "o" + size)`.
                    layer.saveLayerImage("savedata/data1.bmp", "bmp24");
                    (Dictionary.saveStruct incontextof data)(
                        "savedata/data1.bmp", "o" + size);
                    var plainEval = Scripts.evalStorage("savedata/data1.bmp", "o" + size);
                    return size + ":" + viaEval.id + ":" + viaEval.core.curLine +
                        ":" + viaLoad.id + ":" + viaSafe.core.curLine +
                        ":" + plainEval.core.curLine;
                })()"#,
            )
            .expect("write the bookmark");

        // 31778 is the game's own prediction for a 137x77 24bpp BMP, and the
        // BMP our engine writes for the same call has exactly that length --
        // which is what makes the game's offset the struct's offset.
        // size : evalStorage.id : evalStorage.core.curLine : loadStruct.id :
        // safeEvalStorage.core.curLine : plainEval.core.curLine
        const SIZE: usize = (137usize * 3).div_ceil(4) * 4 * 77 + 54;
        assert_eq!(SIZE, 31778);
        assert_eq!(
            value,
            Variant::String("31778:GINKA:12:GINKA:12:12".to_string())
        );

        let bytes = fs::read(root.join("savedata/data0.bmp")).expect("bookmark file");
        let thumbnail = fs::read(root.join("savedata/thumb-only.bmp")).expect("thumbnail");
        let plain = fs::read(root.join("savedata/plain.ksd")).expect("plain struct");
        assert_eq!(thumbnail.len(), SIZE);
        assert_eq!(&thumbnail[..2], b"BM");
        assert_eq!(
            u32::from_le_bytes(thumbnail[10..14].try_into().expect("off bits")),
            54
        );
        assert_eq!(
            i32::from_le_bytes(thumbnail[18..22].try_into().expect("width")),
            137
        );
        assert_eq!(
            i32::from_le_bytes(thumbnail[22..26].try_into().expect("height")),
            77
        );
        assert_eq!(
            u16::from_le_bytes(thumbnail[28..30].try_into().expect("depth")),
            24
        );
        // The offset write opened the file the thumbnail had just created
        // (`TJS_BS_UPDATE`) and wrote from the offset on: every byte before it
        // is untouched.
        assert_eq!(
            &bytes[..SIZE],
            &thumbnail[..],
            "the struct must not clobber the thumbnail"
        );

        // The framing the reference writes for a `z` mode: the two `0xfe` bytes
        // and the mode byte, the UTF-16LE BOM `TVPCreateTextStreamForWrite`
        // always emits, the two little-endian sizes the destructor back-fills
        // (`ZStream->total_out`/`total_in`), and a zlib stream
        // (`deflateInit`, so the payload starts with the `0x78` CMF byte).
        assert_eq!(&bytes[SIZE..SIZE + 5], &[0xfe, 0xfe, 0x02, 0xff, 0xfe]);
        let compressed = u64::from_le_bytes(bytes[SIZE + 5..SIZE + 13].try_into().expect("size"));
        let uncompressed =
            u64::from_le_bytes(bytes[SIZE + 13..SIZE + 21].try_into().expect("size"));
        assert_eq!(bytes.len(), SIZE + 21 + compressed as usize);
        assert_eq!(uncompressed as usize, plain.len() - 2);
        assert_eq!(bytes[SIZE + 21], 0x78);

        // The payload is byte-identical to the plain text stream's, minus its
        // BOM: the compressed mode changes the container, not the struct.
        let mut payload = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::ZlibDecoder::new(&bytes[SIZE + 21..]),
            &mut payload,
        )
        .expect("inflate the struct");
        assert_eq!(payload, plain[2..]);
        let text = String::from_utf16(
            &payload
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>(),
        )
        .expect("utf16 struct");
        assert!(text.starts_with("(const) %[\n"), "{text}");
        assert!(text.contains("\"core\" => (const) %[\n"), "{text}");

        // The bare `"o" + size` form appends the plain text stream and nothing
        // else: `TVPCreateTextStreamForWrite` writes only the BOM and the
        // UTF-16LE payload when the mode names neither `z` nor `c`, and the
        // offset write leaves the thumbnail in place.
        let plain_bookmark = fs::read(root.join("savedata/data1.bmp")).expect("plain bookmark");
        assert_eq!(plain_bookmark.len(), SIZE + plain.len());
        assert_eq!(&plain_bookmark[SIZE..], &plain[..]);
        assert_eq!(&plain_bookmark[..SIZE], &thumbnail[..]);

        // `o<size>` seeks and never limits: rewriting a shorter struct at the
        // same offset leaves the stale tail in place.  That is why KAGEX's
        // `BookMarkIO_Standard.rewrite` calls `Storages.truncateFile` when it
        // wants the file cut back, and why a plain `"z"` mode (no `o`, i.e.
        // `TJS_BS_WRITE`) is the truncating form.
        engine
            .execute_expression(
                "inline.tjs",
                r#"(Dictionary.saveStruct incontextof %["id" => "x"])(
                     "savedata/data0.bmp", "zo31778")"#,
            )
            .expect("shorter rewrite");
        let rewritten = fs::read(root.join("savedata/data0.bmp")).expect("bookmark file");
        assert_eq!(
            rewritten.len(),
            bytes.len(),
            "an `o` mode write never truncates the file"
        );
        assert_eq!(&rewritten[..SIZE], &thumbnail[..]);
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Decodes the engine's text-stream bytes (a UTF-16LE BOM plus UTF-16LE
    /// code units) back to text.
    fn decode_utf16_text(bytes: &[u8]) -> String {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    }

    fn test_root(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-savestruct-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    fn test_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("storage");
        KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine")
    }
}
