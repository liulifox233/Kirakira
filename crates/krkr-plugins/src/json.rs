//! Functional reimplementation of wtnbgo's json.dll (reference: Main.cpp /
//! Writer.hpp by Go Watanabe).
//!
//! Attaches a lenient JSON parser/serializer to the global `Scripts` object:
//! `evalJSON`, `evalJSONStorage`, `saveJSON`, and `toJSONString`.
//!
//! The parser accepts the same JSON superset as the reference: `#` / `//` /
//! `/* */` comments, single- or double-quoted strings with `\b \f \t \r \n
//! \uXXXX \xXX` escapes, `:` / `=` / `=>` key separators, `,` / `;` pair
//! separators, trailing separators, and empty array slots (which become void).
//!
//! Intentional deviations from the reference C++ code:
//! - Unquoted bare words are accepted as object *keys* and stringified (the
//!   reference has an unused `isString` helper and errors on such keys; in key
//!   position `true`/`false`/`null`/`void` also stay plain strings here).
//! - An unterminated string at end-of-input is a parse error instead of the
//!   reference's infinite loop.
//! - The `utf8` flag of `saveJSON` is ignored: our storage layer handles text
//!   encoding itself in `write_text_storage`.
//! - Non-UTF-8 `evalJSONStorage` input is sniffed as UTF-16LE (BOM or NUL high
//!   bytes) and otherwise decoded as UTF-8 lossy, replacing the reference's
//!   system-codepage conversion.
//!
//! Quirk preserved on purpose: integer tokens go through a strtoll mimic, so
//! `1e5` parses as Integer 1 (strtoll stops at `e`), exactly like the
//! reference. Tokens containing `.` go through a strtod mimic and become Real.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

/// Single error the reference throws for any parse failure
/// (`TVPThrowExceptionMessage(TJS_W("JSONファイル のパースに失敗しました"))`);
/// the detailed cause only goes to the log there, so it is dropped here too.
const PARSE_ERROR_MESSAGE: &str = "JSONファイル のパースに失敗しました";

pub struct JsonPlugin;

impl KrkrPlugin for JsonPlugin {
    fn name(&self) -> &str {
        "json.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The reference attaches the methods only when `Scripts` resolves to
        // an object; keep the same guard.
        let Variant::Object(scripts) = runtime.global_member("Scripts") else {
            return Ok(());
        };
        runtime.register_object_native(scripts, "evalJSON", eval_json);
        runtime.register_object_native(scripts, "evalJSONStorage", eval_json_storage);
        runtime.register_object_native(scripts, "saveJSON", save_json);
        runtime.register_object_native(scripts, "toJSONString", to_json_string);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Parser core (pure, runtime-free)
// ---------------------------------------------------------------------------

/// Intermediate JSON value. Dictionaries keep insertion order so the
/// serializer round-trips member order like `EnumMembers` does.
#[derive(Clone, Debug, PartialEq)]
enum JValue {
    /// Both `null` and `void` parse to TJS void.
    Void,
    Integer(i64),
    Real(f64),
    Str(String),
    Array(Vec<JValue>),
    Dict(Vec<(String, JValue)>),
}

const UNICODE_BOM: char = '\u{FEFF}';

/// Character stream over the input with single-char pushback, mirroring the
/// reference's `IReader` (getc/ungetc/next).
struct Reader {
    chars: Vec<char>,
    pos: usize,
}

impl Reader {
    fn new(text: &str) -> Self {
        Self {
            chars: text.chars().collect(),
            pos: 0,
        }
    }

    /// Next raw character; `None` at EOF (position is not advanced then).
    fn getc(&mut self) -> Option<char> {
        let ch = self.chars.get(self.pos).copied();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    /// Push back the last character read. Only call after a `Some` from
    /// `getc`/`next`; the reference's ungetc-at-EOF quirk is not reproduced.
    fn ungetc(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
        }
    }

    /// Skip to the end of the current line.
    fn skip_to_eol(&mut self) {
        while let Some(c) = self.getc() {
            if c == '\n' || c == '\r' {
                break;
            }
        }
    }

    /// Skip whitespace (any char <= 0x20), U+FEFF, and comments; return the
    /// next significant character (`None` at EOF).
    fn next(&mut self) -> std::result::Result<Option<char>, String> {
        loop {
            match self.getc() {
                Some('#') => self.skip_to_eol(),
                Some('/') => match self.getc() {
                    Some('/') => self.skip_to_eol(),
                    Some('*') => loop {
                        match self.getc() {
                            None => return Err("コメントが閉じていません".to_string()),
                            Some('*') => match self.getc() {
                                Some('/') => break,
                                Some(_) => self.ungetc(),
                                None => return Err("コメントが閉じていません".to_string()),
                            },
                            Some(_) => {}
                        }
                    },
                    Some(_) => {
                        self.ungetc();
                        return Ok(Some('/'));
                    }
                    None => return Ok(Some('/')),
                },
                Some(c) if c != UNICODE_BOM && (c as u32) > 0x20 => return Ok(Some(c)),
                Some(_) => {}
                None => return Ok(None),
            }
        }
    }
}

/// `isNumberFirst`: a digit, `.`, `-`, or `+` starts a number.
fn is_number_first(c: char) -> bool {
    c.is_ascii_digit() || c == '.' || c == '-' || c == '+'
}

/// `isNumber`: number body characters (note `e`/`E` are included, so `1e5`
/// is lexed as one integer token — see `strtoll`).
fn is_number_char(c: char) -> bool {
    is_number_first(c) || c == 'e' || c == 'E'
}

/// Bare-word key characters, modeled on the reference's (unused) `isString`:
/// anything above 0x20 except structural characters. `=` is additionally
/// excluded so `key=value` / `key=>value` still split correctly.
fn is_bare_char(c: char) -> bool {
    (c as u32) > 0x20 && c != UNICODE_BOM && !",:]}/\"[{;=#=".contains(c)
}

/// Parse a complete text into a value, like the reference's `eval`: a single
/// value is read and trailing input is not validated.
fn parse_json(text: &str) -> std::result::Result<JValue, String> {
    let mut reader = Reader::new(text);
    parse_value(&mut reader)
}

fn parse_value(reader: &mut Reader) -> std::result::Result<JValue, String> {
    match reader.next()? {
        Some(quote @ ('"' | '\'')) => parse_quoted(reader, quote),
        Some('{') => parse_object(reader),
        Some('[') => parse_array(reader),
        Some(c) if is_number_first(c) => parse_number(reader, c),
        Some(c) if c.is_ascii_lowercase() => parse_keyword(reader, c),
        Some(c) => Err(format!("不明な文字です:{c}")),
        None => Err("不明な文字です:".to_string()),
    }
}

/// Object key. Unlike the reference (which parses keys as full values and
/// rejects anything but quoted strings, numbers, and the four keywords), bare
/// words are accepted here and used verbatim — a lenient superset.
fn parse_key(reader: &mut Reader) -> std::result::Result<String, String> {
    match reader.next()? {
        Some(quote @ ('"' | '\'')) => match parse_quoted(reader, quote)? {
            JValue::Str(text) => Ok(text),
            _ => unreachable!("parse_quoted always returns Str"),
        },
        Some(c) if is_number_first(c) => match parse_number(reader, c)? {
            JValue::Integer(value) => Ok(value.to_string()),
            JValue::Real(value) => Ok(real_to_string(value)),
            _ => unreachable!("parse_number always returns a number"),
        },
        Some(c) if is_bare_char(c) => {
            let mut key = String::from(c);
            loop {
                match reader.getc() {
                    Some(c) if is_bare_char(c) => key.push(c),
                    Some(_) => {
                        reader.ungetc();
                        break;
                    }
                    None => break,
                }
            }
            Ok(key)
        }
        Some(c) => Err(format!("不明な文字です:{c}")),
        None => Err("不明な文字です:".to_string()),
    }
}

fn parse_object(reader: &mut Reader) -> std::result::Result<JValue, String> {
    let mut members: Vec<(String, JValue)> = Vec::new();
    loop {
        match reader.next()? {
            None => return Err("オブジェクトは '}' で終了する必要があります".to_string()),
            Some('}') => return Ok(JValue::Dict(members)),
            Some(',') | Some(';') => reader.ungetc(),
            Some(_) => {
                reader.ungetc();
                let key = parse_key(reader)?;
                match reader.next()? {
                    Some(':') => {}
                    Some('=') => match reader.getc() {
                        Some('>') => {}
                        Some(_) => reader.ungetc(),
                        None => {}
                    },
                    _ => {
                        return Err(
                            "キーの後には ':' または '=' または '=>' が必要です".to_string()
                        );
                    }
                }
                let value = parse_value(reader)?;
                // TJS PropSet overwrites an existing member in place.
                match members.iter_mut().find(|(name, _)| name == &key) {
                    Some(existing) => existing.1 = value,
                    None => members.push((key, value)),
                }
            }
        }
        match reader.next()? {
            Some(';') | Some(',') => {}
            Some('}') => return Ok(JValue::Dict(members)),
            _ => return Err(" ',' または ';' または '}' が必要です".to_string()),
        }
    }
}

fn parse_array(reader: &mut Reader) -> std::result::Result<JValue, String> {
    let mut elements = Vec::new();
    loop {
        match reader.next()? {
            None => return Err("配列は ']' で終了する必要があります".to_string()),
            Some(']') => return Ok(JValue::Array(elements)),
            // Empty slot: register a void element for this column.
            Some(',') | Some(';') => {
                reader.ungetc();
                elements.push(JValue::Void);
            }
            Some(_) => {
                reader.ungetc();
                elements.push(parse_value(reader)?);
            }
        }
        match reader.next()? {
            Some(';') | Some(',') => {}
            Some(']') => return Ok(JValue::Array(elements)),
            _ => return Err(" ',' または ';' または ']' が必要です".to_string()),
        }
    }
}

fn parse_quoted(reader: &mut Reader, quote: char) -> std::result::Result<JValue, String> {
    let mut text = String::new();
    loop {
        match reader.getc() {
            // The reference loops forever on EOF here; report an error instead.
            None | Some('\0') | Some('\n') | Some('\r') => {
                return Err("文字列が終端していません".to_string());
            }
            Some(c) if c == quote => return Ok(JValue::Str(text)),
            Some('\\') => match reader.getc() {
                Some('b') => text.push('\u{8}'),
                Some('f') => text.push('\u{c}'),
                Some('t') => text.push('\t'),
                Some('r') => text.push('\r'),
                Some('n') => text.push('\n'),
                Some('u') => text.push(hex_escape_char(&take(reader, 4))),
                Some('x') => text.push(hex_escape_char(&take(reader, 2))),
                // Any other escape yields the character literally.
                Some(c) => text.push(c),
                None => return Err("文字列が終端していません".to_string()),
            },
            Some(c) => text.push(c),
        }
    }
}

/// Read up to `n` characters (fewer at EOF), like the reference's `next(n)`.
fn take(reader: &mut Reader, n: usize) -> String {
    let mut text = String::new();
    for _ in 0..n {
        match reader.getc() {
            Some(c) => text.push(c),
            None => break,
        }
    }
    text
}

/// `(tjs_char)std::stol(text, NULL, 16)`: parse the leading hex digits (none
/// parses as 0) and truncate to a UTF-16 unit. Values that are not valid
/// scalar values (lone surrogates) become U+FFFD since Rust strings cannot
/// hold them.
fn hex_escape_char(text: &str) -> char {
    let digits: String = text.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    let value = u32::from_str_radix(&digits, 16).unwrap_or(0) & 0xFFFF;
    char::from_u32(value).unwrap_or('\u{FFFD}')
}

fn parse_number(reader: &mut Reader, first: char) -> std::result::Result<JValue, String> {
    let mut text = String::from(first);
    let mut is_real = first == '.';
    loop {
        match reader.getc() {
            Some(c) if is_number_char(c) => {
                if c == '.' {
                    is_real = true;
                }
                text.push(c);
            }
            Some(_) => {
                reader.ungetc();
                break;
            }
            None => break,
        }
    }
    // Reference behavior: any '.' in the token makes it a (strtod) real,
    // otherwise it is a (strtoll) integer.
    if is_real {
        Ok(JValue::Real(strtod(&text)))
    } else {
        Ok(JValue::Integer(strtoll(&text)))
    }
}

fn parse_keyword(reader: &mut Reader, first: char) -> std::result::Result<JValue, String> {
    let mut word = String::from(first);
    loop {
        match reader.getc() {
            Some(c) if c.is_ascii_lowercase() => word.push(c),
            Some(_) => {
                reader.ungetc();
                break;
            }
            None => break,
        }
    }
    match word.as_str() {
        // tTJSVariant(bool) is an integer variant in TJS2.
        "true" => Ok(JValue::Integer(1)),
        "false" => Ok(JValue::Integer(0)),
        "null" | "void" => Ok(JValue::Void),
        _ => Err(format!("不明なキーワードです:{word}")),
    }
}

/// Mimic C `strtoll(text, NULL, 10)`: optional sign followed by a prefix of
/// ASCII digits; parsing stops at the first other character (so `1e5` becomes
/// 1, matching the reference plugin's quirk), no digits yields 0, and
/// overflow clamps to i64::MIN/MAX.
fn strtoll(text: &str) -> i64 {
    let mut chars = text.chars().peekable();
    let negative = match chars.peek() {
        Some('-') => {
            chars.next();
            true
        }
        Some('+') => {
            chars.next();
            false
        }
        _ => false,
    };
    // Accumulate in the negative domain so i64::MIN round-trips exactly.
    let mut value: i64 = 0;
    for c in chars {
        let Some(digit) = c.to_digit(10) else { break };
        let digit = i64::from(digit);
        value = if negative {
            value
                .checked_mul(10)
                .and_then(|v| v.checked_sub(digit))
                .unwrap_or(i64::MIN)
        } else {
            value
                .checked_mul(10)
                .and_then(|v| v.checked_add(digit))
                .unwrap_or(i64::MAX)
        };
    }
    value
}

/// Mimic C `strtod`: the longest valid `[sign] digits [. digits] [ (e|E)
/// [sign] digits ]` prefix is converted; anything else yields 0.0.
fn strtod(text: &str) -> f64 {
    let chars: Vec<char> = text.chars().collect();
    let mut idx = 0;
    if matches!(chars.get(idx), Some('-' | '+')) {
        idx += 1;
    }
    let int_start = idx;
    while matches!(chars.get(idx), Some(c) if c.is_ascii_digit()) {
        idx += 1;
    }
    let mut has_digits = idx > int_start;
    if matches!(chars.get(idx), Some('.')) {
        idx += 1;
        let frac_start = idx;
        while matches!(chars.get(idx), Some(c) if c.is_ascii_digit()) {
            idx += 1;
        }
        has_digits = has_digits || idx > frac_start;
    }
    if !has_digits {
        return 0.0;
    }
    // Length of the valid prefix; the exponent only counts when digits follow.
    let mut len = idx;
    if matches!(chars.get(idx), Some('e' | 'E')) {
        let mut exp = idx + 1;
        if matches!(chars.get(exp), Some('-' | '+')) {
            exp += 1;
        }
        let exp_start = exp;
        while matches!(chars.get(exp), Some(c) if c.is_ascii_digit()) {
            exp += 1;
        }
        if exp > exp_start {
            len = exp;
        }
    }
    chars[..len].iter().collect::<String>().parse().unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Serializer core (pure, runtime-free)
// ---------------------------------------------------------------------------

/// `TJSRealToString`-style real formatting; Rust's shortest round-trip
/// display is close enough to the TJS conversion for JSON output.
fn real_to_string(value: f64) -> String {
    format!("{value}")
}

/// Pretty-printer mirroring the reference `IWriter`: one space of indent per
/// depth, a newline after `{`/`[` (even when empty), members separated by
/// `,` + newline, and the closing bracket on its own dedented line.
struct JsonWriter {
    buf: String,
    indent: usize,
    newline: &'static str,
}

impl JsonWriter {
    fn new(newline_type: i64) -> Self {
        Self {
            buf: String::new(),
            indent: 0,
            newline: if newline_type == 1 { "\n" } else { "\r\n" },
        }
    }

    fn write(&mut self, text: &str) {
        self.buf.push_str(text);
    }

    fn write_ch(&mut self, c: char) {
        self.buf.push(c);
    }

    fn newline(&mut self) {
        self.buf.push_str(self.newline);
        for _ in 0..self.indent {
            self.buf.push(' ');
        }
    }

    fn add_indent(&mut self) {
        self.indent += 1;
        self.newline();
    }

    fn del_indent(&mut self) {
        self.indent -= 1;
        self.newline();
    }

    /// `quoteString`: escape `"`, `\`, the named control characters, and any
    /// other char below 0x20 as `\u00xx`.
    fn write_quoted(&mut self, text: &str) {
        self.write_ch('"');
        for c in text.chars() {
            match c {
                '"' => self.write("\\\""),
                '\\' => self.write("\\\\"),
                '\u{8}' => self.write("\\b"),
                '\u{c}' => self.write("\\f"),
                '\n' => self.write("\\n"),
                '\r' => self.write("\\r"),
                '\t' => self.write("\\t"),
                c if (c as u32) < 0x20 => {
                    self.write(&format!("\\u{:04x}", c as u32));
                }
                c => self.write_ch(c),
            }
        }
        self.write_ch('"');
    }

    /// `getVariantString`.
    fn write_value(&mut self, value: &JValue) {
        match value {
            JValue::Void => self.write("null"),
            JValue::Integer(value) => self.write(&value.to_string()),
            JValue::Real(value) => {
                let text = real_to_string(*value);
                // The reference strips a leading '+' from the real text.
                self.write(text.strip_prefix('+').unwrap_or(&text));
            }
            JValue::Str(text) => self.write_quoted(text),
            JValue::Array(elements) => {
                self.write_ch('[');
                self.add_indent();
                for (index, element) in elements.iter().enumerate() {
                    if index > 0 {
                        self.write_ch(',');
                        self.newline();
                    }
                    self.write_value(element);
                }
                self.del_indent();
                self.write_ch(']');
            }
            JValue::Dict(members) => {
                self.write_ch('{');
                self.add_indent();
                for (index, (key, member)) in members.iter().enumerate() {
                    if index > 0 {
                        self.write_ch(',');
                        self.newline();
                    }
                    self.write_quoted(key);
                    // The reference writes the value immediately after ':'.
                    self.write_ch(':');
                    self.write_value(member);
                }
                self.del_indent();
                self.write_ch('}');
            }
        }
    }
}

/// Serialize a value exactly like the reference's `IStringWriter` path.
fn write_json(value: &JValue, newline_type: i64) -> String {
    let mut writer = JsonWriter::new(newline_type);
    writer.write_value(value);
    writer.buf
}

// ---------------------------------------------------------------------------
// TJS glue
// ---------------------------------------------------------------------------

/// JValue -> Variant. Objects become ordinary objects with "Dictionary" class
/// info (like TJSCreateDictionaryObject), arrays become array objects.
fn jvalue_to_variant(runtime: &mut Runtime<KrkrHost>, value: &JValue) -> Result<Variant> {
    Ok(match value {
        JValue::Void => Variant::Void,
        JValue::Integer(value) => Variant::Integer(*value),
        JValue::Real(value) => Variant::Real(*value),
        JValue::Str(text) => Variant::String(text.clone()),
        JValue::Array(elements) => {
            let mut variants = Vec::with_capacity(elements.len());
            for element in elements {
                variants.push(jvalue_to_variant(runtime, element)?);
            }
            Variant::Object(runtime.alloc_array_object(variants))
        }
        JValue::Dict(members) => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "Dictionary");
            for (key, member) in members {
                let member = jvalue_to_variant(runtime, member)?;
                runtime.set_object_member(handle, key, member);
            }
            Variant::Object(handle)
        }
    })
}

/// Variant -> JValue, mirroring `getVariantString`'s type dispatch: array
/// objects serialize as JSON arrays, any other object as a dictionary whose
/// hidden members (`__`-prefixed, the TJS_HIDDENMEMBER convention) are
/// skipped, and non-value types (octet, invalid objects) become null.
fn variant_to_jvalue(runtime: &Runtime<KrkrHost>, value: &Variant) -> JValue {
    match value {
        Variant::Void | Variant::Null => JValue::Void,
        Variant::Integer(value) => JValue::Integer(*value),
        Variant::Real(value) => JValue::Real(*value),
        Variant::String(text) => JValue::Str(text.clone()),
        Variant::Octet(_) | Variant::CodeObject(_) => JValue::Void,
        Variant::Object(handle) => object_to_jvalue(runtime, *handle),
        Variant::Closure(closure) => object_to_jvalue(runtime, closure.object),
    }
}

fn object_to_jvalue(runtime: &Runtime<KrkrHost>, handle: ObjectHandle) -> JValue {
    if !runtime.object_valid(handle) {
        return JValue::Void;
    }
    // ObjectKind::Array is this runtime's equivalent of IsInstanceOf("Array").
    if let Some(elements) = runtime.array_elements(handle) {
        return JValue::Array(
            elements
                .iter()
                .map(|element| variant_to_jvalue(runtime, element))
                .collect(),
        );
    }
    JValue::Dict(
        runtime
            .object_members(handle)
            .into_iter()
            .filter(|(key, _)| !key.starts_with("__"))
            .map(|(key, member)| (key, variant_to_jvalue(runtime, &member)))
            .collect(),
    )
}

/// Integer-cast argument flag, like `(int)*param[n] != 0`.
fn int_arg(args: &[Variant], index: usize) -> i64 {
    args.get(index)
        .map(|value| value.to_integer().unwrap_or(0))
        .unwrap_or(0)
}

fn eval_json(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(text) = args.first() else {
        return Err(TjsError::runtime("evalJSON requires a JSON text argument"));
    };
    let text = text.to_tjs_string()?;
    let value = parse_json(&text).map_err(|_| TjsError::runtime(PARSE_ERROR_MESSAGE))?;
    jvalue_to_variant(runtime, &value)
}

fn eval_json_storage(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(file) = args.first() else {
        return Err(TjsError::runtime(
            "evalJSONStorage requires a storage name argument",
        ));
    };
    let file = file.to_tjs_string()?;
    let utf8 = int_arg(&args, 1) != 0;
    let bytes = runtime
        .host()
        .read_binary_storage(&file)
        .map_err(|_| TjsError::runtime(format!("cannot open : {file}")))?;
    let text = decode_storage_text(&bytes, utf8);
    let value = parse_json(&text).map_err(|_| TjsError::runtime(PARSE_ERROR_MESSAGE))?;
    jvalue_to_variant(runtime, &value)
}

fn save_json(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if args.len() < 2 {
        return Err(TjsError::runtime(
            "saveJSON requires a storage name and a value argument",
        ));
    }
    let file = args[0].to_tjs_string()?;
    // args[2] (the utf8 flag) is intentionally ignored: write_text_storage
    // owns the output encoding in our storage layer.
    let newline_type = int_arg(&args, 3);
    let value = variant_to_jvalue(runtime, &args[1]);
    runtime
        .host_mut()
        .write_text_storage(&file, "w", &write_json(&value, newline_type))?;
    Ok(Variant::Void)
}

fn to_json_string(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(value) = args.first() else {
        return Err(TjsError::runtime("toJSONString requires a value argument"));
    };
    let newline_type = int_arg(&args, 1);
    let value = variant_to_jvalue(runtime, value);
    Ok(Variant::String(write_json(&value, newline_type)))
}

// ---------------------------------------------------------------------------
// Storage text decoding (evalJSONStorage)
// ---------------------------------------------------------------------------

fn decode_storage_text(bytes: &[u8], utf8: bool) -> String {
    if !utf8 && looks_utf16le(bytes) {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// UTF-16LE sniffing for the non-utf8 path: a BOM is decisive, otherwise
/// ASCII-range JSON stored as UTF-16LE gives itself away by mostly-zero high
/// bytes.
fn looks_utf16le(bytes: &[u8]) -> bool {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return true;
    }
    let pairs = bytes.len() / 2;
    if pairs == 0 {
        return false;
    }
    let checked = pairs.min(32);
    let zero_high_bytes = (0..checked).filter(|i| bytes[2 * i + 1] == 0).count();
    zero_high_bytes * 2 > checked
}

// ---------------------------------------------------------------------------
// Tests (pure parser/serializer core only)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(members: Vec<(&str, JValue)>) -> JValue {
        JValue::Dict(
            members
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    #[test]
    fn parse_basic_values() {
        assert_eq!(parse_json("123").unwrap(), JValue::Integer(123));
        assert_eq!(parse_json("-45").unwrap(), JValue::Integer(-45));
        assert_eq!(parse_json("+7").unwrap(), JValue::Integer(7));
        assert_eq!(parse_json("1.5").unwrap(), JValue::Real(1.5));
        assert_eq!(parse_json(".5").unwrap(), JValue::Real(0.5));
        assert_eq!(parse_json("\"text\"").unwrap(), JValue::Str("text".into()));
        assert_eq!(parse_json("'text'").unwrap(), JValue::Str("text".into()));
        assert_eq!(parse_json("true").unwrap(), JValue::Integer(1));
        assert_eq!(parse_json("false").unwrap(), JValue::Integer(0));
        assert_eq!(parse_json("null").unwrap(), JValue::Void);
        assert_eq!(parse_json("void").unwrap(), JValue::Void);
    }

    #[test]
    fn parse_number_strtoll_quirk() {
        // No '.' in the token, so strtoll stops at 'e': 1e5 -> Integer 1.
        assert_eq!(parse_json("1e5").unwrap(), JValue::Integer(1));
        assert_eq!(parse_json("1.5e2").unwrap(), JValue::Real(150.0));
        // Overflow clamps like strtoll.
        assert_eq!(
            parse_json("9223372036854775808").unwrap(),
            JValue::Integer(i64::MAX)
        );
        assert_eq!(
            parse_json("-9223372036854775809").unwrap(),
            JValue::Integer(i64::MIN)
        );
        assert_eq!(parse_json("-9223372036854775808").unwrap(), JValue::Integer(i64::MIN));
    }

    #[test]
    fn parse_object_lenient_separators() {
        let value = parse_json("{a=1; b=>2, 'c': 3;}").unwrap();
        assert_eq!(
            value,
            dict(vec![
                ("a", JValue::Integer(1)),
                ("b", JValue::Integer(2)),
                ("c", JValue::Integer(3)),
            ])
        );
    }

    #[test]
    fn parse_object_nested_and_duplicate_keys() {
        let value = parse_json("{a:{b:[1,2]},a:3}").unwrap();
        // Later assignment overwrites the first member in place.
        assert_eq!(value, dict(vec![("a", JValue::Integer(3))]));
    }

    #[test]
    fn parse_array_empty_slots_and_trailing_separator() {
        assert_eq!(
            parse_json("[, ,]").unwrap(),
            JValue::Array(vec![JValue::Void, JValue::Void])
        );
        assert_eq!(
            parse_json("[1,]").unwrap(),
            JValue::Array(vec![JValue::Integer(1)])
        );
        assert_eq!(parse_json("[]").unwrap(), JValue::Array(vec![]));
        assert_eq!(parse_json("{}").unwrap(), JValue::Dict(vec![]));
    }

    #[test]
    fn parse_comments() {
        assert_eq!(
            parse_json("# line\n// line\n/* block */ [1, 2]").unwrap(),
            JValue::Array(vec![JValue::Integer(1), JValue::Integer(2)])
        );
        assert!(parse_json("/* unterminated").is_err());
    }

    #[test]
    fn parse_string_escapes() {
        assert_eq!(
            parse_json(r#"'a\'bA\x42\t\n\r\b\f'"#).unwrap(),
            JValue::Str("a'bAB\t\n\r\u{8}\u{c}".into())
        );
        // Unknown escapes yield the character literally.
        assert_eq!(parse_json(r#""a\qb""#).unwrap(), JValue::Str("aqb".into()));
        // Unterminated strings are errors (EOF, CR, LF).
        assert!(parse_json("\"abc").is_err());
        assert!(parse_json("\"abc\n\"").is_err());
    }

    #[test]
    fn parse_bom_and_whitespace_skipped() {
        assert_eq!(
            parse_json("\u{FEFF} \t\r\n [1]").unwrap(),
            JValue::Array(vec![JValue::Integer(1)])
        );
    }

    #[test]
    fn parse_errors() {
        assert!(parse_json("").is_err());
        assert!(parse_json("[").is_err());
        assert!(parse_json("[1").is_err());
        assert!(parse_json("{").is_err());
        assert!(parse_json("{a:}").is_err());
        assert!(parse_json("{a 1}").is_err());
        assert!(parse_json("tru").is_err());
        assert!(parse_json("True").is_err());
        assert!(parse_json("[1 2]").is_err());
    }

    #[test]
    fn write_formatting() {
        let value = dict(vec![
            ("a", JValue::Integer(1)),
            (
                "b",
                JValue::Array(vec![JValue::Void, JValue::Str("s".into())]),
            ),
        ]);
        assert_eq!(
            write_json(&value, 0),
            "{\r\n \"a\":1,\r\n \"b\":[\r\n  null,\r\n  \"s\"\r\n ]\r\n}"
        );
    }

    #[test]
    fn write_empty_containers_still_get_inner_newline() {
        assert_eq!(write_json(&JValue::Dict(vec![]), 0), "{\r\n \r\n}");
        assert_eq!(write_json(&JValue::Array(vec![]), 0), "[\r\n \r\n]");
    }

    #[test]
    fn write_lf_newline_type() {
        let value = dict(vec![("a", JValue::Integer(1))]);
        assert_eq!(write_json(&value, 1), "{\n \"a\":1\n}");
    }

    #[test]
    fn write_string_escapes() {
        let value = JValue::Str("a\"b\\c\u{1}\u{8}\u{c}\n\r\t".into());
        assert_eq!(
            write_json(&value, 0),
            "\"a\\\"b\\\\c\\u0001\\b\\f\\n\\r\\t\""
        );
    }

    #[test]
    fn write_real_strips_plus_and_keeps_shortest() {
        assert_eq!(write_json(&JValue::Real(0.5), 0), "0.5");
        assert_eq!(write_json(&JValue::Real(-1.25), 0), "-1.25");
    }

    #[test]
    fn roundtrip() {
        let source = "{name:'Kirakira', tags:[1, 2.5, true, null], nested:{a=>1; b='x'}}";
        let value = parse_json(source).unwrap();
        let text = write_json(&value, 0);
        assert_eq!(parse_json(&text).unwrap(), value);
    }

    #[test]
    fn storage_text_decoding() {
        // BOM wins.
        let utf16: Vec<u8> = "\u{FEFF}[1]"
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert_eq!(decode_storage_text(&utf16, false), "\u{FEFF}[1]");
        // NUL pattern without BOM.
        let utf16_nobom: Vec<u8> = "[1,2]"
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert!(looks_utf16le(&utf16_nobom));
        assert_eq!(decode_storage_text(&utf16_nobom, false), "[1,2]");
        // Plain UTF-8/ASCII stays UTF-8 even when not flagged.
        assert!(!looks_utf16le(b"{\"a\":1}"));
        assert_eq!(decode_storage_text(b"{\"a\":1}", false), "{\"a\":1}");
        // utf8 flag forces UTF-8 decoding.
        assert_eq!(decode_storage_text(&utf16_nobom, true), String::from_utf8_lossy(&utf16_nobom));
    }
}
