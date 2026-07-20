//! PackinOne.dll compatibility shim.
//!
//! Bundle plugin combining fstat/savestruct/scriptsEx/systemEx/shrinkCopy/
//! layerExImage/layerExRaster/csvParser/process/tjsDataPack and more. Only the
//! surface games actually call is functional:
//!
//! - `CSVParser`: real CSV parser with wtnbgo/csvParser field semantics and
//!   its event convention: `parse`/`parseStorage` fire `doLine(fields, lineNo)`
//!   on the callback target per row (the instance's `target` member when it
//!   holds an object, otherwise the instance itself). Newlines embedded in
//!   quoted fields are kept as written instead of being normalized to CRLF.
//! - `Storages.saveOctet` / `Storages.loadOctet`: binary storage I/O.
//! - `System.urlencode` / `System.urldecode`: UTF-8 percent codec; decoding
//!   leaves `+` untouched (no form-style space mapping).
//! - `Scripts.loadDataPack`: decodes the binary dictionary/array formats used
//!   by packed UI definitions (`KBAD100` and `TJS/ns0`).
//! - `Scripts.clone`: recursively clones arrays and dictionaries and delegates
//!   other objects to their own `clone` method, matching scriptsEx.
//!
//! Everything else (System version/env shims, Layer effect methods, Process,
//! fstat, proxyfs, ...) is a no-op stub returning benign values.

use std::sync::atomic::{AtomicBool, Ordering};
use std::{collections::BTreeMap, sync::Arc};

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

pub struct PackinOnePlugin;

impl KrkrPlugin for PackinOnePlugin {
    fn name(&self) -> &str {
        "PackinOne.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_csv_parser(runtime);
        install_storages_octet(runtime);
        install_system_ex(runtime);
        install_layer_effects(runtime);
        install_data_pack(runtime);
        install_scripts_ex(runtime);
        install_window_and_plugins(runtime);
        install_process(runtime);
        install_misc_classes(runtime);
        runtime.host_mut().log(
            "PackinOne.dll compat registered: CSVParser, Storages octet I/O, System URL \
             codecs, Scripts.loadDataPack and Scripts.clone functional; fstat/savestruct/\
             remaining scriptsEx/systemEx/shrinkCopy/layerEx*/process are stubs",
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Shared helpers

fn ensure_global_object(runtime: &mut Runtime<KrkrHost>, name: &'static str) -> ObjectHandle {
    match runtime.global_member(name) {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, name);
            runtime.set_global_member(name, Variant::Object(handle));
            handle
        }
    }
}

fn first_arg_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        args.first().cloned().unwrap_or_default().to_tjs_string()?,
    ))
}

fn empty_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(String::new()))
}

fn zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn ignore_property_set(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _value: Variant,
) -> Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// CSVParser (functional)

fn install_csv_parser(runtime: &mut Runtime<KrkrHost>) {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "CSVParser");
            install_csv_parser_members(runtime, instance);
            set_csv_text(runtime, instance, String::new());
            runtime.set_object_member(instance, "__csvFile", Variant::String(String::new()));
            // new CSVParser(target?, separator?, newline?): only the callback
            // target is honored; separator/newline stay accepted-but-ignored.
            runtime.set_object_member(
                instance,
                "target",
                args.into_iter().next().unwrap_or_default(),
            );
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "CSVParser");
    install_csv_parser_members(runtime, handle);
    runtime.set_global_member("CSVParser", Variant::Object(handle));
}

fn install_csv_parser_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.set_object_member(handle, "target", Variant::Void);
    runtime.register_object_native(handle, "finalize", native_void);
    runtime.register_object_native(handle, "init", csv_init);
    runtime.register_object_native(handle, "initStorage", csv_init_storage);
    runtime.register_object_native(handle, "getNextLine", csv_get_next_line);
    runtime.register_object_native(handle, "parse", csv_parse);
    runtime.register_object_native(handle, "parseStorage", csv_parse_storage);
    runtime.register_object_native_property(
        handle,
        "currentLineNumber",
        csv_current_line_number_get,
        ignore_property_set,
    );
    runtime.register_object_native_property(handle, "file", csv_file_get, ignore_property_set);
    runtime.register_object_native_property(handle, "offset", csv_offset_get, ignore_property_set);
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

fn set_csv_text(runtime: &mut Runtime<KrkrHost>, this: ObjectHandle, text: String) {
    runtime.set_object_member(this, "__csvText", Variant::String(text));
    runtime.set_object_member(this, "__csvPos", Variant::Integer(0));
    runtime.set_object_member(this, "__csvLineNo", Variant::Integer(0));
}

fn csv_init(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = csv_this(runtime, this_obj) {
        let text = args.first().cloned().unwrap_or_default().to_tjs_string()?;
        set_csv_text(runtime, this, text);
    }
    Ok(Variant::Void)
}

fn csv_init_storage(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let bytes = runtime.host().read_binary_storage(&name)?;
    if let Some(this) = csv_this(runtime, this_obj) {
        runtime.set_object_member(this, "__csvFile", Variant::String(name));
        set_csv_text(runtime, this, decode_csv_text(&bytes));
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
    let text = match runtime.object_member(this, "__csvText") {
        Variant::String(text) => text,
        _ => return Ok(Variant::Void),
    };
    let pos = csv_state_integer(runtime, this, "__csvPos") as usize;
    if pos >= text.len() {
        return Ok(Variant::Void);
    }
    let (fields, next_pos) = parse_csv_record(text.as_bytes(), pos);
    let line_no = csv_state_integer(runtime, this, "__csvLineNo") + 1;
    runtime.set_object_member(this, "__csvPos", Variant::Integer(next_pos as i64));
    runtime.set_object_member(this, "__csvLineNo", Variant::Integer(line_no));
    let fields = fields.into_iter().map(Variant::String).collect();
    Ok(Variant::Object(runtime.alloc_array_object(fields)))
}

fn csv_parse(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = csv_this(runtime, this_obj) {
        if let Some(text) = args.first()
            && !matches!(text, Variant::Void)
        {
            set_csv_text(runtime, this, text.clone().to_tjs_string()?);
        }
        csv_fire_do_line(runtime, this);
    }
    Ok(Variant::Void)
}

fn csv_parse_storage(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let bytes = runtime.host().read_binary_storage(&name)?;
    if let Some(this) = csv_this(runtime, this_obj) {
        runtime.set_object_member(this, "__csvFile", Variant::String(name));
        set_csv_text(runtime, this, decode_csv_text(&bytes));
        csv_fire_do_line(runtime, this);
    }
    Ok(Variant::Void)
}

/// Fires `doLine(fields, lineNo)` for every remaining row, matching the
/// wtnbgo event convention: the callback target is the instance's `target`
/// member when it holds an object, otherwise the instance itself; a target
/// without `doLine` leaves the parser state untouched and fires nothing.
/// `doLine` failures are logged and ignored, as the reference ignores
/// FuncCall failures. `currentLineNumber` is updated before each call so it
/// reflects the row being fired and ends at the total row count.
fn csv_fire_do_line(runtime: &mut Runtime<KrkrHost>, this: ObjectHandle) {
    let target = match runtime.object_member(this, "target") {
        Variant::Object(handle) => handle,
        _ => this,
    };
    if matches!(runtime.object_member(target, "doLine"), Variant::Void) {
        return;
    }
    let text = match runtime.object_member(this, "__csvText") {
        Variant::String(text) => text,
        _ => return,
    };
    let bytes = text.as_bytes();
    let mut pos = csv_state_integer(runtime, this, "__csvPos") as usize;
    let mut line_no = csv_state_integer(runtime, this, "__csvLineNo");
    while pos < bytes.len() {
        let (fields, next_pos) = parse_csv_record(bytes, pos);
        pos = next_pos;
        line_no += 1;
        runtime.set_object_member(this, "__csvLineNo", Variant::Integer(line_no));
        let fields = fields.into_iter().map(Variant::String).collect();
        let fields = Variant::Object(runtime.alloc_array_object(fields));
        if let Err(error) =
            runtime.call_object_method(target, "doLine", vec![fields, Variant::Integer(line_no)])
        {
            runtime.host_mut().log(&format!(
                "PackinOne.dll: CSVParser doLine call failed at line {line_no}: {error}"
            ));
        }
    }
    runtime.set_object_member(this, "__csvPos", Variant::Integer(pos as i64));
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

fn csv_file_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    let file = csv_this(runtime, this_obj)
        .and_then(|this| match runtime.object_member(this, "__csvFile") {
            Variant::String(file) => Some(file),
            _ => None,
        })
        .unwrap_or_default();
    Ok(Variant::String(file))
}

fn csv_offset_get(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

/// UTF-8 (lossy), or UTF-16LE when a BOM is present; a UTF-8 BOM is stripped.
fn decode_csv_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Parses one record starting at `pos`, returning the fields and the position
/// of the next record. All structural characters are ASCII, so byte-level
/// scanning never splits a UTF-8 sequence. Blank lines yield zero fields and a
/// trailing newline does not produce an extra record (wtnbgo behavior).
fn parse_csv_record(bytes: &[u8], mut pos: usize) -> (Vec<String>, usize) {
    let mut fields = Vec::new();
    if is_eol(bytes, pos) {
        return (fields, skip_eol(bytes, pos));
    }
    loop {
        let (field, next_pos) = parse_csv_field(bytes, pos);
        fields.push(field);
        pos = next_pos;
        if pos < bytes.len() && bytes[pos] == b',' {
            pos += 1;
        } else {
            return (fields, skip_eol(bytes, pos));
        }
    }
}

fn parse_csv_field(bytes: &[u8], mut pos: usize) -> (String, usize) {
    if pos < bytes.len() && bytes[pos] == b'"' {
        pos += 1;
        let mut field = Vec::new();
        while pos < bytes.len() {
            if bytes[pos] == b'"' {
                if pos + 1 < bytes.len() && bytes[pos + 1] == b'"' {
                    field.push(b'"');
                    pos += 2;
                } else {
                    // wtnbgo: characters after the closing quote up to the
                    // separator are still appended to the field.
                    pos += 1;
                    while pos < bytes.len() && bytes[pos] != b',' && !is_eol(bytes, pos) {
                        field.push(bytes[pos]);
                        pos += 1;
                    }
                    break;
                }
            } else {
                field.push(bytes[pos]);
                pos += 1;
            }
        }
        (String::from_utf8_lossy(&field).into_owned(), pos)
    } else {
        let start = pos;
        while pos < bytes.len() && bytes[pos] != b',' && !is_eol(bytes, pos) {
            pos += 1;
        }
        (
            String::from_utf8_lossy(&bytes[start..pos]).into_owned(),
            pos,
        )
    }
}

fn is_eol(bytes: &[u8], pos: usize) -> bool {
    pos < bytes.len() && (bytes[pos] == b'\r' || bytes[pos] == b'\n')
}

fn skip_eol(bytes: &[u8], mut pos: usize) -> usize {
    if pos < bytes.len() && bytes[pos] == b'\r' {
        pos += 1;
    }
    if pos < bytes.len() && bytes[pos] == b'\n' {
        pos += 1;
    }
    pos
}

// ---------------------------------------------------------------------------
// Storages.saveOctet / loadOctet (functional)

fn install_storages_octet(runtime: &mut Runtime<KrkrHost>) {
    let storages = ensure_global_object(runtime, "Storages");
    runtime.register_object_native(storages, "saveOctet", storages_save_octet);
    runtime.register_object_native(storages, "loadOctet", storages_load_octet);
}

fn storages_save_octet(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let bytes = match args.get(1) {
        Some(Variant::Octet(bytes)) => bytes.clone(),
        Some(value) => value.to_tjs_string()?.into_bytes(),
        None => Vec::new(),
    };
    runtime
        .host_mut()
        .write_binary_storage(&name, "w", &bytes)?;
    Ok(Variant::Void)
}

fn storages_load_octet(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    match runtime.host().read_binary_storage(&name) {
        Ok(bytes) => Ok(Variant::Octet(bytes)),
        Err(_) => Ok(Variant::Void),
    }
}

// ---------------------------------------------------------------------------
// systemEx surface on System

fn install_system_ex(runtime: &mut Runtime<KrkrHost>) {
    let system = ensure_global_object(runtime, "System");
    runtime.register_object_native(system, "getOSVersion", system_get_os_version);
    runtime.register_object_native(system, "urlencode", system_urlencode);
    runtime.register_object_native(system, "urldecode", system_urldecode);
    runtime.register_object_native(system, "expandEnvString", first_arg_string);
    runtime.register_object_native(system, "readEnvValue", native_void);
    runtime.register_object_native(system, "writeEnvValue", native_void);
    runtime.register_object_native(system, "writeRegValue", native_void);
    runtime.register_object_native(system, "getAboutString", system_get_about_string);
    runtime.register_object_native(system, "confirm", zero);
    runtime.register_object_native(system, "waitForAppLock", native_void);
    runtime.register_object_native(system, "setDpiAwareness", native_void);
    runtime.register_object_native(system, "getKnownFolderPath", empty_string);
    runtime.register_object_native(system, "processApplicationMessages", native_void);
    runtime.register_object_native(system, "handleApplicationMessage", native_void);
    runtime.register_object_native(system, "setDefaultDllDirectories", native_void);
    runtime.register_object_native(system, "addDllDirectory", native_void);
    runtime.register_object_native(system, "removeDllDirectory", native_void);
    for (name, value) in [
        ("dacUnaware", 0),
        ("dacSystemAware", 1),
        ("dacPerMonitorAware", 2),
        ("dacPerMonitorAwareV2", 3),
        ("dacUnawareGdiScaled", 4),
        ("llsApplicationDir", 0x800),
        ("llsDefaultDirs", 0xA00),
        ("llsSystem32", 0x200),
        ("llsUserDirs", 0x400),
    ] {
        runtime.set_object_member(system, name, Variant::Integer(value));
    }
}

fn system_get_os_version(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // Fixed Windows 10 compatible values; this is a Windows-compat shim.
    let info = runtime.alloc_ordinary_object();
    for (name, value) in [
        ("major", Variant::Integer(10)),
        ("minor", Variant::Integer(0)),
        ("build", Variant::Integer(19045)),
        ("platform", Variant::Integer(2)),
        ("spmajor", Variant::Integer(0)),
        ("spminor", Variant::Integer(0)),
        ("servicepack", Variant::String(String::new())),
        ("suite", Variant::Integer(0)),
    ] {
        runtime.set_object_member(info, name, value);
    }
    Ok(Variant::Object(info))
}

fn system_get_about_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        "Kirakira (Kirikiri-compatible emulator)".to_string(),
    ))
}

fn system_urlencode(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let text = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let mut encoded = String::new();
    for &byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    Ok(Variant::String(encoded))
}

fn system_urldecode(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let text = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos] == b'%'
            && pos + 3 <= bytes.len()
            && let (Some(high), Some(low)) = (hex_value(bytes[pos + 1]), hex_value(bytes[pos + 2]))
        {
            decoded.push(high << 4 | low);
            pos += 3;
        } else {
            // `+` is left as-is rather than mapped to a space.
            decoded.push(bytes[pos]);
            pos += 1;
        }
    }
    Ok(Variant::String(
        String::from_utf8_lossy(&decoded).into_owned(),
    ))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// shrinkCopy / layerExImage / layerExRaster surface on Layer (no-op)

fn install_layer_effects(runtime: &mut Runtime<KrkrHost>) {
    let layer = match runtime.global_member("Layer") {
        Variant::Object(handle) => handle,
        _ => return,
    };
    for method in [
        "shrinkCopy",
        "shrinkCopyFast",
        "doLine",
        "light",
        "colorize",
        "modulate",
        "noise",
        "generateWhiteNoise",
        "gaussianBlur",
        "copyRaster",
        "copyRightBlueToLeftAlpha",
        "copyBottomBlueToTopAlpha",
        "fillAlpha",
        "copyAlphaToProvince",
        "clipAlphaRect",
        "overwrapRect",
        "fillByProvince",
        "fillToProvince",
    ] {
        // Do not clobber script-side overrides.
        if !matches!(runtime.object_member(layer, method), Variant::Closure(_)) {
            runtime.register_object_native(layer, method, native_void);
        }
    }
}

// ---------------------------------------------------------------------------
// tjsDataPack global functions

fn install_data_pack(runtime: &mut Runtime<KrkrHost>) {
    // tjsDataPack attaches these to the Scripts object (games call
    // `Scripts.loadDataPack(...)`); also expose them as globals for safety.
    let scripts = ensure_global_object(runtime, "Scripts");
    let global = runtime.global_handle();
    runtime.register_object_native(scripts, "loadDataPack", load_data_pack);
    runtime.register_object_native(global, "loadDataPack", load_data_pack);
    for target in [scripts, global] {
        runtime.register_object_native(target, "saveDataPack", zero);
        runtime.register_object_native(target, "makeDataPackThumb", native_void);
        runtime.register_object_native(target, "makeDataPackDigest", empty_string);
    }
}

fn load_data_pack(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let Ok(bytes) = runtime.host().read_binary_storage(&name) else {
        return Ok(Variant::Void);
    };
    let value = if bytes.starts_with(b"KBAD100\0") {
        runtime.decode_binary_struct(&bytes)?
    } else if bytes.starts_with(b"TJS/ns0\0") || bytes.starts_with(b"TJS/4s0\0") {
        runtime.decode_tjs_ns0(&bytes)?
    } else {
        None
    };
    Ok(value.unwrap_or(Variant::Void))
}

// ---------------------------------------------------------------------------
// scriptsEx surface on Scripts

fn install_scripts_ex(runtime: &mut Runtime<KrkrHost>) {
    let scripts = ensure_global_object(runtime, "Scripts");
    runtime.register_object_native(scripts, "encodeTBPS", first_arg_string);
    runtime.register_object_native(scripts, "decodeTBPS", first_arg_string);
    runtime.register_object_native(scripts, "clone", scripts_clone);
    let logged = Arc::new(AtomicBool::new(false));
    runtime.register_object_native(
        scripts,
        "getMD5HashString",
        move |runtime: &mut Runtime<KrkrHost>, _this_obj, _args| {
            if !logged.swap(true, Ordering::Relaxed) {
                runtime
                    .host_mut()
                    .log("PackinOne.dll: getMD5HashString returns an empty string stub");
            }
            Ok(Variant::String(String::new()))
        },
    );
    runtime.register_object_native(
        scripts,
        "safeEvalStorage",
        |runtime: &mut Runtime<KrkrHost>, _this_obj, args: Vec<Variant>| {
            let name = args.first().cloned().unwrap_or_default().to_tjs_string()?;
            runtime
                .host_mut()
                .log(&format!("PackinOne.dll: safeEvalStorage({name}) skipped"));
            Ok(Variant::Void)
        },
    );
}

fn scripts_clone(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let value = args.first().cloned().unwrap_or_default();
    clone_scripts_value(runtime, &value, &mut BTreeMap::new())
}

fn clone_scripts_value(
    runtime: &mut Runtime<KrkrHost>,
    value: &Variant,
    cloned: &mut BTreeMap<ObjectHandle, ObjectHandle>,
) -> Result<Variant> {
    let Variant::Object(source) = value else {
        return Ok(value.clone());
    };
    if let Some(dest) = cloned.get(source) {
        return Ok(Variant::Object(*dest));
    }

    if let Some(elements) = runtime.array_elements(*source).map(Vec::from) {
        let dest = runtime.alloc_array_object(Vec::new());
        cloned.insert(*source, dest);
        for element in elements {
            let element = clone_scripts_value(runtime, &element, cloned)?;
            runtime.array_push(dest, element);
        }
        return Ok(Variant::Object(dest));
    }

    let is_dictionary = runtime
        .object_class_infos(*source)
        .iter()
        .any(|class| class == "Dictionary");
    if is_dictionary {
        let constructor = runtime.global_member("Dictionary");
        let Variant::Object(dest) = runtime.call_function(constructor, Vec::new())? else {
            return Ok(value.clone());
        };
        cloned.insert(*source, dest);
        for (name, member) in runtime.object_members(*source) {
            if scripts_clone_builtin_member(&name) {
                continue;
            }
            let member = clone_scripts_value(runtime, &member, cloned)?;
            runtime.set_object_member(dest, name, member);
        }
        return Ok(Variant::Object(dest));
    }

    if !matches!(runtime.object_member(*source, "clone"), Variant::Void)
        && let Ok(result) = runtime.call_object_method(*source, "clone", Vec::new())
    {
        return Ok(result);
    }
    Ok(value.clone())
}

fn scripts_clone_builtin_member(name: &str) -> bool {
    matches!(
        name,
        "clear" | "assign" | "assignStruct" | "saveStruct" | "loadStruct"
    )
}

// ---------------------------------------------------------------------------
// Window.selectFileEx and Plugins surface

fn install_window_and_plugins(runtime: &mut Runtime<KrkrHost>) {
    let window = ensure_global_object(runtime, "Window");
    runtime.register_object_native(window, "selectFileEx", native_void);

    let plugins = ensure_global_object(runtime, "Plugins");
    runtime.register_object_native(plugins, "setCurrentDirectory", native_void);
    let bundled = [
        "fstat.dll",
        "savestruct.dll",
        "scriptsEx.dll",
        "systemEx.dll",
        "addFont.dll",
        "shrinkCopy.dll",
        "layerExBTOA.dll",
        "layerExImage.dll",
        "process.dll",
        "proxyfs.dll",
        "tlgSliceLoader.dll",
        "tjsDataPack.dll",
        "packinone.dll",
    ];
    let list = bundled
        .iter()
        .map(|name| Variant::String(name.to_string()))
        .collect();
    let list = runtime.alloc_array_object(list);
    runtime.set_object_member(plugins, "PackinOneList", Variant::Object(list));
}

// ---------------------------------------------------------------------------
// Process class (stub)

fn install_process(runtime: &mut Runtime<KrkrHost>) {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "Process");
            install_process_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Process");
    install_process_members(runtime, handle);
    runtime.set_global_member("Process", Variant::Object(handle));
}

fn install_process_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for (name, value) in [
        ("status", Variant::Integer(0)),
        ("exitcode", Variant::Integer(0)),
        ("stdout", Variant::String(String::new())),
        ("error", Variant::String(String::new())),
        ("message", Variant::String(String::new())),
        ("failed", Variant::Integer(0)),
        ("timeout", Variant::Integer(0)),
        ("onExecuted", Variant::Void),
        ("onOutput", Variant::Void),
    ] {
        runtime.set_object_member(handle, name, value);
    }
    runtime.register_object_native(handle, "open", zero);
    runtime.register_object_native(handle, "terminate", native_void);
    runtime.register_object_native(handle, "sendSignal", native_void);
    runtime.register_object_native(handle, "commandExecute", zero);
}

// ---------------------------------------------------------------------------
// MemoryStreamHolder / ProxyStorageMap / StoragesFstat (minimal stubs)

fn install_misc_classes(runtime: &mut Runtime<KrkrHost>) {
    for class_name in ["MemoryStreamHolder", "ProxyStorageMap", "StoragesFstat"] {
        let handle = runtime.alloc_native_constructor(
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  _args: Vec<Variant>| {
                let instance = this_obj
                    .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                    .filter(|handle| *handle != runtime.global_handle())
                    .unwrap_or_else(|| runtime.alloc_ordinary_object());
                runtime.add_object_class_info(instance, class_name);
                install_misc_class_members(runtime, instance, class_name);
                Ok(Variant::Object(instance))
            },
        );
        runtime.add_object_class_info(handle, class_name);
        install_misc_class_members(runtime, handle, class_name);
        runtime.set_global_member(class_name, Variant::Object(handle));
    }
}

fn install_misc_class_members(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &str,
) {
    runtime.register_object_native(handle, "finalize", native_void);
    match class_name {
        "MemoryStreamHolder" => {
            runtime.set_object_member(handle, "length", Variant::Integer(0));
        }
        "ProxyStorageMap" => {
            runtime.register_object_native(handle, "proxy", native_void);
        }
        "StoragesFstat" => {
            for name in ["size", "mtime", "ctime", "atime", "attrib"] {
                runtime.set_object_member(handle, name, Variant::Integer(0));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use krkr_engine::KrkrEngine;

    use super::*;

    #[test]
    fn load_data_pack_decodes_binary_struct_storage() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-packinone-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");

        let mut bytes = b"KBAD100\0\x81\xa6".to_vec();
        for unit in "answer".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.push(42);
        fs::write(root.join("probe.pbd"), bytes).expect("write data pack");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression("inline.tjs", "Scripts.loadDataPack(\"probe.pbd\").answer")
            .expect("load data pack");

        assert_eq!(value, Variant::Integer(42));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn scripts_clone_deeply_copies_arrays_and_dictionaries() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-packinone-clone-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");

        let mut engine = KrkrEngine::for_project(&root).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var source = %[nested: [1, %[value: 2]]];
                    var copy = Scripts.clone(source);
                    copy.nested[0] = 9;
                    copy.nested[1].value = 7;
                    return source.nested[0] + ":" + source.nested[1].value + ":" +
                        copy.nested[0] + ":" + copy.nested[1].value;
                })()"#,
            )
            .expect("clone structured value");

        assert_eq!(value, Variant::String("1:2:9:7".to_owned()));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
