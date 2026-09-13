//! `csvParser.dll` — the standalone CSVParser class.
//!
//! Reference: krkrz `src/plugins/win32/csvParser/Main.cpp` (623 lines) plus the
//! krkr2 trunk revision, which adds the text-stream mode argument to
//! `initStorage`/`parseStorage`. PARQUET links `csvParser.dll` by name even
//! though the same class is compiled into `PackinOne.dll`, so this module
//! installs the surface under that DLL's own name.
//!
//! Behaviour carried over from the reference:
//!
//! - `new CSVParser(target, separator, newline)`: `separator` is a character
//!   code (`#"\t"`/`asc("\t")` is 9) and `0` means "no separator at all", so a
//!   record yields a single field; `newline` is the string substituted for
//!   every line break inside a quoted field (default `"\r\n"`).
//! - `doLine(fields, lineNo)` fires on the constructor's `target` when it
//!   holds an object, else on the instance; it is called with the target as
//!   `this`, its return value is discarded, and an exception it throws
//!   propagates out of `parse`.
//! - A blank line yields an empty `Array` (not one empty field), a trailing
//!   newline produces no extra record, `""` is an empty field, and an
//!   unterminated quote keeps the trailing newline the scanner appended.
//! - `currentLineNumber` is read-only; assigning it raises the reference's
//!   read-only-property error.
//!
//! Deliberate divergences, all in the storage path: `initStorage` reads
//! through the host text path (which decodes the engine's configured
//! encoding, the KRKR ciphered text-stream modes and lazily materialized Web
//! assets) instead of copying raw bytes and decoding them as CP_UTF8/CP_ACP,
//! so the `utf8` flag selects nothing; a string passed in that position is
//! honoured as the trunk's text-stream mode. Scalar constructor/method
//! arguments are converted instead of throwing a variant-conversion error,
//! which keeps `new CSVParser(void, 9)` working the way the shipped PackinOne
//! build behaves. One quoted-field edge case also differs: a run of four
//! quotes (`""""`) yields one literal quote here, because the reference's
//! scan-increment-in-condition quirk consumes the fourth quote as the pair's
//! escape and yields two (`parse_csv_field` below handles `""` as the escape
//! pair, which is what the shipped build does for the same input).

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, ObjectHandle, Runtime, TjsHost, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "CSVParser class (init/initStorage/parse/parseStorage/getNextLine/clear, doLine event)",
    notes: "Full CSV state machine with the reference separator/newline handling and the doLine \
            convention; storage reads go through the engine text path so ciphered and lazily \
            fetched text decodes correctly.",
    install: |engine| engine.register_plugin(CsvParserPlugin),
};

pub struct CsvParserPlugin;

impl KrkrPlugin for CsvParserPlugin {
    fn name(&self) -> &str {
        "csvParser.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_csv_parser(runtime);
        runtime
            .host_mut()
            .log("csvParser.dll registered: CSVParser class (standalone)");
        Ok(())
    }
}

/// Members the reference registers on the class, plus the `clear` the shipped
/// PackinOne build carries. Shared with the module's surface test.
#[cfg(test)]
const CSV_PARSER_METHODS: &[&str] = &[
    "init",
    "initStorage",
    "parse",
    "parseStorage",
    "getNextLine",
    "clear",
];

fn install_csv_parser(runtime: &mut Runtime<KrkrHost>) {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let called_as_super_constructor = this_obj.is_some();
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "CSVParser");
            // Keep the native methods on the class object: a subclass instance
            // must inherit through the normal superclass chain, so a script
            // override such as a `doLine` on the subclass is not shadowed.
            if !called_as_super_constructor {
                install_csv_parser_members(runtime, instance);
            }
            reset_csv_state(runtime, instance, String::new(), String::new());
            let mut args = args.into_iter();
            let target = args.next().unwrap_or_default();
            let target = match target {
                // The reference stores `param[0]->AsObject()`; `null` and an
                // omitted argument both mean "fire `doLine` on the instance".
                Variant::Void | Variant::Null => Variant::Void,
                Variant::Object(_) | Variant::Closure(_) => target,
                other => {
                    return Err(TjsError::runtime(format!(
                        "Cannot convert the variable type ({other:?} to Object)"
                    )));
                }
            };
            runtime.set_object_member(instance, "target", target);
            let separator = args
                .next()
                .and_then(|value| value.to_integer().ok())
                .unwrap_or(i64::from(b','));
            let newline = match args.next() {
                Some(value) => value.to_tjs_string()?,
                None => "\r\n".to_string(),
            };
            runtime.set_object_member(instance, "__csvSeparator", Variant::Integer(separator));
            runtime.set_object_member(instance, "__csvNewline", Variant::String(newline));
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "CSVParser");
    install_csv_parser_members(runtime, handle);
    runtime.set_global_member("CSVParser", Variant::Object(handle));
}

fn install_csv_parser_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.set_object_member(handle, "target", Variant::Void);
    runtime.register_object_native(handle, "finalize", csv_finalize);
    // `init(text)` and `initStorage(storage [, mode])` both open with
    // `if (numparams < 1) return TJS_E_BADPARAMCOUNT` (`csvParser/Main.cpp:594`
    // and `:603`), so the registration floor is the reference's one mandatory
    // argument; the handlers keep the same guard as the reference body has.
    runtime.register_object_native_with_arg_count(
        handle,
        "init",
        NativeArgCount::AtLeast(1),
        csv_init,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "initStorage",
        NativeArgCount::AtLeast(1),
        csv_init_storage,
    );
    runtime.register_object_native(handle, "getNextLine", csv_get_next_line);
    runtime.register_object_native(handle, "parse", csv_parse);
    runtime.register_object_native(handle, "parseStorage", csv_parse_storage);
    runtime.register_object_native(handle, "clear", csv_clear);
    // `currentLineNumber` is read-only; the reference's setter is
    // `TJS_DENY_NATIVE_PROP_SETTER`, i.e. `TJS_E_ACCESSDENYED`.
    runtime.register_object_native_property(
        handle,
        "currentLineNumber",
        csv_current_line_number_get,
        |_runtime, _this_obj, _value| Err(TjsError::access_denied()),
    );
}

fn csv_this(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Option<ObjectHandle> {
    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

fn csv_state_integer(runtime: &Runtime<KrkrHost>, this: ObjectHandle, name: &str) -> i64 {
    match runtime.object_member(this, name) {
        Variant::Integer(value) => value,
        _ => 0,
    }
}

/// The constructor's `newline` string, substituted for every line break a
/// quoted field swallows (`Main.cpp:307`).
fn csv_newline(runtime: &Runtime<KrkrHost>, this: ObjectHandle) -> String {
    match runtime.object_member(this, "__csvNewline") {
        Variant::String(newline) => newline,
        _ => "\r\n".to_string(),
    }
}

/// The separator as a character. The reference keeps it in a `tjs_char` and
/// compares it directly (`Main.cpp:267-275`), so `0` never matches any
/// character in the NUL-terminated line buffer and every record becomes one
/// field.
fn csv_separator(runtime: &Runtime<KrkrHost>, this: ObjectHandle) -> Option<char> {
    let code = csv_state_integer(runtime, this, "__csvSeparator");
    if code == 0 {
        return None;
    }
    u32::try_from(code).ok().and_then(char::from_u32)
}

fn reset_csv_state(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    text: String,
    file: String,
) {
    runtime.set_object_member(this, "__csvText", Variant::String(text));
    runtime.set_object_member(this, "__csvRecords", Variant::Void);
    runtime.set_object_member(this, "__csvRecordIndex", Variant::Integer(0));
    runtime.set_object_member(this, "__csvLineNo", Variant::Integer(0));
    runtime.set_object_member(this, "__csvFile", Variant::String(file));
}

fn csv_finalize(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn csv_init(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if args.is_empty() {
        return Err(TjsError::bad_param_count());
    }
    let text = csv_text_argument(&args[0])?;
    if let Some(this) = csv_this(runtime, this_obj) {
        reset_csv_state(runtime, this, text, String::new());
    }
    Ok(Variant::Void)
}

fn csv_init_storage(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if args.is_empty() {
        return Err(TjsError::bad_param_count());
    }
    let name = args[0].to_tjs_string()?;
    let mode = csv_storage_mode(&args);
    let text = read_csv_storage(runtime, &name, &mode)?;
    if let Some(this) = csv_this(runtime, this_obj) {
        reset_csv_state(runtime, this, text, name);
    }
    Ok(Variant::Void)
}

fn csv_get_next_line(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = csv_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let Some((fields, line_no)) = csv_next_record(runtime, this) else {
        // At EOF the reference closes the parser; the next call answers void
        // without touching the line number (`Main.cpp:417`).
        return Ok(Variant::Void);
    };
    runtime.set_object_member(this, "__csvLineNo", Variant::Integer(line_no));
    let fields = fields.into_iter().map(Variant::String).collect();
    Ok(Variant::Object(runtime.alloc_array_object(fields)))
}

fn csv_parse(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = csv_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    // `parse(text)` re-initializes, `parse()` continues from the current
    // position (`Main.cpp:501-510`). An explicit `void` is an argument and
    // therefore re-initializes with the empty string, as in the reference.
    if let Some(text) = args.first() {
        reset_csv_state(runtime, this, csv_text_argument(text)?, String::new());
    }
    csv_fire_do_line(runtime, this)
}

fn csv_parse_storage(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = csv_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    if let Some(name) = args.first() {
        let name = name.to_tjs_string()?;
        let mode = csv_storage_mode(&args);
        let text = read_csv_storage(runtime, &name, &mode)?;
        reset_csv_state(runtime, this, text, name);
    }
    csv_fire_do_line(runtime, this)
}

fn csv_clear(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = csv_this(runtime, this_obj) {
        reset_csv_state(runtime, this, String::new(), String::new());
    }
    Ok(Variant::Void)
}

fn csv_current_line_number_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    let line_no = csv_this(runtime, this_obj)
        .map(|this| csv_state_integer(runtime, this, "__csvLineNo"))
        .unwrap_or(0);
    Ok(Variant::Integer(line_no))
}

/// The text an `init`/`parse` argument carries. `void` becomes the empty
/// string, matching the reference's `AsStringNoAddRef()`.
fn csv_text_argument(value: &Variant) -> Result<String> {
    match value {
        Variant::Void | Variant::Null => Ok(String::new()),
        value => value.to_tjs_string(),
    }
}

/// The storage read mode. krkr2's trunk treats a *string* second argument as
/// the text-stream mode string and keeps the numeric form as the `utf8` flag
/// (`trunk/Main.cpp:604-610`); this engine's text reader decodes the storage's
/// own encoding, so the flag itself has no effect.
fn csv_storage_mode(args: &[Variant]) -> String {
    for value in args.iter().skip(1).take(2) {
        if let Variant::String(mode) = value {
            return mode.clone();
        }
    }
    String::new()
}

/// Reads a CSV storage as text through the host text path, which decodes the
/// engine's configured encoding, the KRKR ciphered text-stream modes and a
/// lazily materialized Web asset. A failure other than a pending resource is
/// reported with the reference's `cannot open : <name>` message
/// (`Main.cpp:47`).
fn read_csv_storage(runtime: &mut Runtime<KrkrHost>, name: &str, mode: &str) -> Result<String> {
    match TjsHost::read_text(runtime.host_mut(), name, mode) {
        Ok(text) => Ok(text),
        Err(error) if error.kind == krkr_tjs2::TjsErrorKind::ResourcePending => Err(error),
        Err(_) => Err(TjsError::runtime(format!("cannot open : {name}"))),
    }
}

/// Fires `doLine(fields, lineNo)` for every remaining record, mirroring the
/// reference loop (`Main.cpp:434-449`): nothing happens when the parser has no
/// text or the callback target has no `doLine`; the callback runs with the
/// target as `this`; its return value is discarded; and an exception it throws
/// propagates out of `parse`, leaving the parser where it stopped.
fn csv_fire_do_line(runtime: &mut Runtime<KrkrHost>, this: ObjectHandle) -> Result<Variant> {
    let target = runtime
        .object_member(this, "target")
        .object_handle()
        .unwrap_or(this);
    let has_do_line = runtime
        .resolve_object_member(target, "doLine")
        .is_ok_and(|value| !matches!(value, Variant::Void));
    if !has_do_line {
        return Ok(Variant::Void);
    }
    while let Some((fields, line_no)) = csv_next_record(runtime, this) {
        runtime.set_object_member(this, "__csvLineNo", Variant::Integer(line_no));
        let fields = fields.into_iter().map(Variant::String).collect();
        let fields = Variant::Object(runtime.alloc_array_object(fields));
        runtime.call_object_method(target, "doLine", vec![fields, Variant::Integer(line_no)])?;
    }
    Ok(Variant::Void)
}

/// The parsed records, computed on first use and then kept on the instance.
///
/// The reference advances a position inside a buffer it holds, so a parse is
/// one pass over the text. Holding the text in this object model means every
/// read copies it, so the records are parsed once here and the per-record
/// calls only advance `__csvRecordIndex`; a parser that reads a 10 000-row
/// table no longer re-reads and re-scans the whole text 10 000 times.
fn csv_records(runtime: &mut Runtime<KrkrHost>, this: ObjectHandle) -> Option<ObjectHandle> {
    if let Variant::Object(records) = runtime.object_member(this, "__csvRecords") {
        return Some(records);
    }
    let text = match runtime.object_member(this, "__csvText") {
        Variant::String(text) => text,
        _ => return None,
    };
    let chars: Vec<char> = text.chars().collect();
    let separator = csv_separator(runtime, this);
    let newline = csv_newline(runtime, this);
    let mut position = 0;
    let mut records = Vec::new();
    while position < chars.len() {
        let (fields, next) = parse_csv_record(&chars, position, separator, &newline);
        if next <= position {
            // A record always consumes at least one character; stop rather
            // than loop forever if that ever stops holding.
            break;
        }
        let fields = fields.into_iter().map(Variant::String).collect();
        records.push(Variant::Object(runtime.alloc_array_object(fields)));
        position = next;
    }
    let records = runtime.alloc_array_object(records);
    runtime.set_object_member(this, "__csvRecords", Variant::Object(records));
    Some(records)
}

/// The next record's fields and its advanced line number, or `None` at the end
/// of the text.
fn csv_next_record(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
) -> Option<(Vec<String>, i64)> {
    let records = csv_records(runtime, this)?;
    let index = csv_state_integer(runtime, this, "__csvRecordIndex").max(0) as usize;
    let entry = runtime.array_elements(records)?.get(index)?.clone();
    let fields = entry.object_handle().and_then(|handle| {
        runtime.array_elements(handle).map(|fields| {
            fields
                .iter()
                .map(|field| match field {
                    Variant::String(text) => text.clone(),
                    other => other.to_tjs_string().unwrap_or_default(),
                })
                .collect()
        })
    })?;
    runtime.set_object_member(this, "__csvRecordIndex", Variant::Integer(index as i64 + 1));
    let line_no = csv_state_integer(runtime, this, "__csvLineNo") + 1;
    Some((fields, line_no))
}

fn is_line_break(ch: char) -> bool {
    ch == '\r' || ch == '\n'
}

/// Consumes a line break: `\r`, `\n` or `\r\n`, exactly like the reference's
/// reader (which never leaves a break character in a record).
fn skip_line_break(chars: &[char], mut pos: usize) -> usize {
    if pos < chars.len() && chars[pos] == '\r' {
        pos += 1;
    }
    if pos < chars.len() && chars[pos] == '\n' {
        pos += 1;
    }
    pos
}

/// Parses one record starting at `pos`, returning its fields and the position
/// after the record. A blank line yields no fields; a trailing separator ends
/// the record with an empty field; a quoted field swallows line breaks,
/// substituting `newline` for each of them.
fn parse_csv_record(
    chars: &[char],
    mut pos: usize,
    separator: Option<char>,
    newline: &str,
) -> (Vec<String>, usize) {
    if pos >= chars.len() {
        return (Vec::new(), pos);
    }
    if is_line_break(chars[pos]) {
        return (Vec::new(), skip_line_break(chars, pos));
    }
    let mut fields = Vec::new();
    loop {
        if pos < chars.len() && chars[pos] == '"' {
            let mut field = String::new();
            let mut scan = pos + 1;
            loop {
                if scan >= chars.len() {
                    // EOF inside a quoted field: the reference appends the
                    // newline before it notices the end of input
                    // (`Main.cpp:306-308`).
                    field.push_str(newline);
                    pos = scan;
                    break;
                }
                if chars[scan] == '"' {
                    if scan + 1 < chars.len() && chars[scan + 1] == '"' {
                        field.push('"');
                        scan += 2;
                        continue;
                    }
                    scan += 1;
                    // Characters after the closing quote up to the separator
                    // stay in the field (`Main.cpp:296-302`).
                    while scan < chars.len()
                        && !is_line_break(chars[scan])
                        && Some(chars[scan]) != separator
                    {
                        field.push(chars[scan]);
                        scan += 1;
                    }
                    pos = scan;
                    break;
                }
                if is_line_break(chars[scan]) {
                    field.push_str(newline);
                    scan = skip_line_break(chars, scan);
                    continue;
                }
                field.push(chars[scan]);
                scan += 1;
            }
            fields.push(field);
        } else {
            let mut field = String::new();
            while pos < chars.len() && !is_line_break(chars[pos]) && Some(chars[pos]) != separator {
                field.push(chars[pos]);
                pos += 1;
            }
            fields.push(field);
        }
        if pos < chars.len() && Some(chars[pos]) == separator {
            pos += 1;
            continue;
        }
        return (fields, skip_line_break(chars, pos));
    }
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
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        for name in CSV_PARSER_METHODS {
            let value = engine
                .execute_expression("inline.tjs", &format!("typeof CSVParser.{name}"))
                .expect("class member probe");
            assert_ne!(
                value,
                Variant::String("undefined".to_string()),
                "CSVParser.{name} is not installed"
            );
        }
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() { var p = new CSVParser(); return p.currentLineNumber; })()",
            )
            .expect("property probe");
        assert_eq!(value, Variant::Integer(0));
    }

    #[test]
    fn get_next_line_splits_records_and_numbers_blank_lines() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var parser = new CSVParser(null, 9);\n\
                     parser.init(\"name\\tvalue\\r\\nsoldier\\tfront\\r\\n\\r\\nlast\\trow\\r\\n\");\n\
                     var out = \"\";\n\
                     var line;\n\
                     while ((line = parser.getNextLine()) !== void) {\n\
                         out += parser.currentLineNumber + \"/\" + line.count + \"/\" + line.join(\"|\") + \";\";\n\
                     }\n\
                     return out;\n\
                 })()",
            )
            .expect("parse tab-separated records");

        assert_eq!(
            value,
            Variant::String("1/2/name|value;2/2/soldier|front;3/0/;4/2/last|row;".to_string())
        );
    }

    #[test]
    fn bang_separator_yields_one_field_per_record() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        // separator 0 is honoured: the reference never matches any character.
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var parser = new CSVParser(null, 0);\n\
                     parser.init(\"a,b\\r\\nc,d\");\n\
                     var first = parser.getNextLine();\n\
                     var second = parser.getNextLine();\n\
                     return first.count + \":\" + first[0] + \":\" + second[0];\n\
                 })()",
            )
            .expect("parse with no separator");

        assert_eq!(value, Variant::String("1:a,b:c,d".to_string()));
    }

    #[test]
    fn quoted_fields_keep_separators_escapes_and_the_constructor_newline() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var parser = new CSVParser(null, #\",\", \"|\");\n\
                     parser.init(\"\\\"a,b\\\"\\n\\\"x\\\"\\\"y\\\"\\n\\\"line1\\nline2\\\",tail\");\n\
                     var out = \"\";\n\
                     var line;\n\
                     while ((line = parser.getNextLine()) !== void) {\n\
                         out += line.count + \"/\" + line[0] + \";\";\n\
                     }\n\
                     return out;\n\
                 })()",
            )
            .expect("parse quoted fields");

        assert_eq!(
            value,
            Variant::String("1/a,b;1/x\"y;2/line1|line2;".to_string())
        );
    }

    #[test]
    fn records_are_parsed_once_and_quote_runs_keep_the_module_behaviour() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var parser = new CSVParser(null, #\",\");\n\
                     parser.init(\"\\\"\\\"\\\"\\\"\\n\\\"x\\\"\");\n\
                     var first = parser.getNextLine();\n\
                     var second = parser.getNextLine();\n\
                     return first[0].length + \":\" + second[0] + \":\" +\n\
                         parser.__csvRecords.count;\n\
                 })()",
            )
            .expect("quote runs");

        // A run of four quotes yields one literal quote here (the reference's
        // scan quirk yields two — documented divergence), and the whole text is
        // parsed into `__csvRecords` once: reading records only advances
        // `__csvRecordIndex`, so a per-record call no longer re-scans the text.
        assert_eq!(value, Variant::String("1:x:2".to_string()));
    }

    #[test]
    fn do_line_fires_on_a_script_assigned_target_including_subclasses() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     global.__rows = [];\n\
                     var target = new Dictionary();\n\
                     target.doLine = function(fields, line) {\n\
                         global.__rows.push(line + \":\" + fields[0]);\n\
                     };\n\
                     var parser = new CSVParser(target, #\",\");\n\
                     parser.parse(\"a,x\\nc,y\");\n\
                     return global.__rows.join(\"|\") + \" lines=\" + parser.currentLineNumber;\n\
                 })()",
            )
            .expect("doLine on an assigned target");
        assert_eq!(value, Variant::String("1:a|2:c lines=2".to_string()));

        // The class-load test (`tests/csvParser/startup.tjs`) subclasses the
        // native class and defines `doLine` on the subclass; `super.CSVParser()`
        // must leave that override in charge.
        engine
            .execute_script(
                "inline.tjs",
                "class Probe extends CSVParser {\n\
                     function doLine(columns, lineNo) {\n\
                         global.__rows.push(lineNo + \":\" + columns.join(\"/\"));\n\
                     }\n\
                     function Probe() { super.CSVParser(); }\n\
                 }",
            )
            .expect("define the subclass");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     global.__rows = [];\n\
                     var parser = new Probe();\n\
                     parser.parse(\"1,data1\\n2,data2\");\n\
                     return global.__rows.join(\"|\");\n\
                 })()",
            )
            .expect("subclass doLine");
        assert_eq!(value, Variant::String("1:1/data1|2:2/data2".to_string()));
    }

    #[test]
    fn init_storage_reads_a_file_and_round_trips_its_records() {
        let root = test_root("csv-init-storage");
        fs::write(root.join("table.csv"), b"name,value\nsoldier,front\n").expect("write csv");

        let mut engine = test_engine(&root);
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var parser = new CSVParser();\n\
                     parser.initStorage(\"table.csv\");\n\
                     var first = parser.getNextLine();\n\
                     var second = parser.getNextLine();\n\
                     return first.join(\"|\") + \":\" + second.join(\"|\") + \":\" +\n\
                         (parser.getNextLine() === void) + \":\" + parser.currentLineNumber;\n\
                 })()",
            )
            .expect("parse a storage");

        assert_eq!(
            value,
            Variant::String("name|value:soldier|front:1:2".to_string())
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn parsing_a_missing_storage_reports_the_reference_message() {
        let root = test_root("csv-missing");
        let mut engine = test_engine(&root);
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        let error = engine
            .execute_expression(
                "inline.tjs",
                "(function() { var parser = new CSVParser(); return parser.initStorage(\"absent.csv\"); })()",
            )
            .expect_err("a missing storage must fail");

        assert!(
            error.message.contains("cannot open : absent.csv"),
            "unexpected message: {}",
            error.message
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `init`/`initStorage` declare one mandatory argument
    /// (`csvParser/Main.cpp:594`/`:603`: `if (numparams < 1) return
    /// TJS_E_BADPARAMCOUNT`) and the module registers that count as the
    /// member's floor, so the short call is ncbind's parameter-count error —
    /// `TJS_E_BADPARAMCOUNT`, code -1004 — not just any failure. The games'
    /// shapes (`init(text)`, `initStorage("table.csv")`) pass their argument
    /// list and are covered by the tests above.
    #[test]
    fn argument_count_and_read_only_property_errors_match_the_reference() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        for (member, call) in [
            ("init", "parser.init()"),
            ("initStorage", "parser.initStorage()"),
        ] {
            let error = engine
                .execute_expression(
                    "inline.tjs",
                    &format!("(function() {{ var parser = new CSVParser(); return {call}; }})()"),
                )
                .expect_err(&format!("{member} without its argument must fail"));
            assert_eq!(
                error.kind,
                krkr_tjs2::TjsErrorKind::BadParamCount,
                "{member}: {}",
                error.message
            );
            assert_eq!(
                error.kind.tjs_error_code(),
                Some(-1004),
                "{member} must be the reference's TJS_E_BADPARAMCOUNT"
            );
            assert_eq!(error.message, "Invalid argument count", "{member}");
        }

        let error = engine
            .execute_expression(
                "inline.tjs",
                "(function() { var parser = new CSVParser(); parser.currentLineNumber = 7; })()",
            )
            .expect_err("currentLineNumber is read-only");
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );

        // A scalar target cannot become the `doLine` receiver.
        let error = engine
            .execute_expression("inline.tjs", "new CSVParser(5)")
            .expect_err("a scalar callback target must be rejected");
        assert!(
            error.message.contains("to Object"),
            "unexpected message: {}",
            error.message
        );
    }

    #[test]
    fn parse_without_an_argument_continues_from_the_current_position() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(CsvParserPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     global.__rows = [];\n\
                     var target = new Dictionary();\n\
                     target.doLine = function(fields, line) { global.__rows.push(line); };\n\
                     var parser = new CSVParser(target);\n\
                     parser.init(\"a\\nb\\nc\");\n\
                     parser.getNextLine();\n\
                     parser.parse();\n\
                     return global.__rows.join(\",\");\n\
                 })()",
            )
            .expect("continue after getNextLine");

        assert_eq!(value, Variant::String("2,3".to_string()));
    }

    fn test_root(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-csvparser-{name}-{}-{unique}",
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
