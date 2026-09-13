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
//! - `Scripts.loadDataPack` / `Scripts.saveDataPack` / `Scripts.makeDataPackThumb`
//!   / `Scripts.makeDataPackDigest`: the tjsDataPack surface. The loader reads
//!   the binary dictionary/array formats used by packed UI definitions
//!   (`KBAD100` and `TJS/ns0`) under the exact storage name it is handed (the
//!   engine's `.pbd` alias stays as the fallback); the writer produces the
//!   bookmark-file anatomy the game's own IO uses — the captured thumbnail
//!   image leading, the engine-readable `KBAD100` pack appended, the digest
//!   seed in a footer. See the `tjsDataPack` section for the reference anchors
//!   and the deliberate divergences.
//! - `Scripts.clone`: recursively clones arrays and dictionaries and delegates
//!   other objects to their own `clone` method, matching scriptsEx.
//!
//! Everything else (System version/env shims, Layer effect methods, Process,
//! fstat, proxyfs, ...) is a no-op stub returning benign values. The
//! `safeEvalStorage` wrapper remains functional because KRKR startup scripts
//! use it to restore system variables before choosing their opening flow.

use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use krkr_engine::{KrkrHost, KrkrPlugin, plugin_api::layer::layer_bitmap_read};
use krkr_tjs2::{
    Result, TjsError, TjsErrorKind,
    runtime::{ObjectHandle, Runtime, TjsHost, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "CSVParser, Scripts DataPack (load/save/thumb/digest), Storages.saveOctet, System.getOSVersion, Layer effects",
    notes: "CSVParser, storages octet I/O, URL codecs and the DataPack load/save/thumb/digest quartet are functional; the rest of the bundle is no-op surface.",
    install: |engine| engine.register_plugin(PackinOnePlugin),
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
             codecs, Scripts.clone and the DataPack load/save/thumb/digest quartet are \
             functional; fstat/savestruct/remaining scriptsEx/systemEx/shrinkCopy/\
             layerEx*/process are stubs",
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
            let called_as_super_constructor = this_obj.is_some();
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "CSVParser");
            // Keep native methods on the CSVParser class object.  Installing
            // them directly on a subclass instance would shadow a script
            // override such as UIListParser.parseStorage; the reference TJS
            // native class participates in the normal superclass chain.
            if !called_as_super_constructor {
                install_csv_parser_members(runtime, instance);
            }
            set_csv_text(runtime, instance, String::new());
            runtime.set_object_member(instance, "__csvFile", Variant::String(String::new()));
            // new CSVParser(target?, separator?, newline?): the native KRKR
            // parser accepts the separator as a character code (the standard
            // config loaders pass `asc("\t")`).
            let mut args = args.into_iter();
            let target = args.next().unwrap_or_default();
            runtime.set_object_member(instance, "target", target);
            let separator = args
                .next()
                .and_then(|value| value.to_integer().ok())
                .and_then(|value| u8::try_from(value).ok())
                .filter(|value| *value != 0)
                .unwrap_or(b',');
            runtime.set_object_member(
                instance,
                "__csvSeparator",
                Variant::Integer(i64::from(separator)),
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

fn csv_separator(runtime: &Runtime<KrkrHost>, this: ObjectHandle) -> u8 {
    csv_state_integer(runtime, this, "__csvSeparator")
        .try_into()
        .ok()
        .filter(|value| *value != 0)
        .unwrap_or(b',')
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
    let text = read_csv_storage_text(runtime, &name)?;
    if let Some(this) = csv_this(runtime, this_obj) {
        runtime.set_object_member(this, "__csvFile", Variant::String(name));
        set_csv_text(runtime, this, text);
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
    let (fields, next_pos) = parse_csv_record(text.as_bytes(), pos, csv_separator(runtime, this));
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
    let text = read_csv_storage_text(runtime, &name)?;
    if let Some(this) = csv_this(runtime, this_obj) {
        runtime.set_object_member(this, "__csvFile", Variant::String(name));
        set_csv_text(runtime, this, text);
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
    // The game assigns `target` from `new`/`this` results, which are
    // self-bound closures; the object behind the binding is the callback
    // target.
    let target = runtime
        .object_member(this, "target")
        .object_handle()
        .unwrap_or(this);
    let has_do_line = runtime
        .resolve_object_member(target, "doLine")
        .is_ok_and(|value| !matches!(value, Variant::Void));
    if !has_do_line {
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
        let (fields, next_pos) = parse_csv_record(bytes, pos, csv_separator(runtime, this));
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

/// Reads a CSV/TSV storage as text.
///
/// The reference `csvParser.dll` opens the file with `TVPCreateTextStreamForRead`,
/// so it sees whatever the engine's text reader produces: plain UTF-16LE with a
/// BOM, one of the KiriKiri ciphered stream modes (`FE FE <mode> FF FE`), or a
/// legacy single-byte encoding.  Going through the host text path instead of
/// decoding the raw bytes here keeps all of those working — `fgimage/standposition.txt`
/// in particular is a mode 1 (bit-swapped UTF-16) stream, and decoding it as raw
/// bytes turned every row into a single garbage field.
///
/// The host read path is also what makes lazily materialized Web packages work:
/// a cache miss for a manifest-known file becomes a resumable ResourcePending
/// request rather than a permanent "missing file".
fn read_csv_storage_text(runtime: &mut Runtime<KrkrHost>, name: &str) -> Result<String> {
    TjsHost::read_text(runtime.host_mut(), name, "")
}

/// Parses one record starting at `pos`, returning the fields and the position
/// of the next record. All structural characters are ASCII, so byte-level
/// scanning never splits a UTF-8 sequence. Blank lines yield zero fields and a
/// trailing newline does not produce an extra record (wtnbgo behavior).
fn parse_csv_record(bytes: &[u8], mut pos: usize, separator: u8) -> (Vec<String>, usize) {
    let mut fields = Vec::new();
    if is_eol(bytes, pos) {
        return (fields, skip_eol(bytes, pos));
    }
    loop {
        let (field, next_pos) = parse_csv_field(bytes, pos, separator);
        fields.push(field);
        pos = next_pos;
        if pos < bytes.len() && bytes[pos] == separator {
            pos += 1;
        } else {
            return (fields, skip_eol(bytes, pos));
        }
    }
}

fn parse_csv_field(bytes: &[u8], mut pos: usize, separator: u8) -> (String, usize) {
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
                    while pos < bytes.len() && bytes[pos] != separator && !is_eol(bytes, pos) {
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
        while pos < bytes.len() && bytes[pos] != separator && !is_eol(bytes, pos) {
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

/// The `shrinkCopy`/`layerExImage`/`layerExRaster`/`LayerExBTOA` names the
/// bundle fills in.
const LAYER_EFFECT_METHODS: [&str; 18] = [
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
];

fn install_layer_effects(runtime: &mut Runtime<KrkrHost>) {
    let layer = match runtime.global_member("Layer") {
        Variant::Object(handle) => handle,
        _ => return,
    };
    for method in LAYER_EFFECT_METHODS {
        // Only fill a slot nothing owns. The bundle registers before the
        // layerEx* modules in catalog order, but `Plugins.link("packinone.dll")`
        // re-runs this registration, and their implementations are
        // `Variant::Object` natives -- not the `Closure`s this used to skip --
        // so a blanket overwrite reverted the real filters to no-ops.
        // An override a script made is not `Void` either and stays untouched.
        if matches!(runtime.object_member(layer, method), Variant::Void) {
            runtime.register_object_native(layer, method, native_void);
        }
    }
}

// ---------------------------------------------------------------------------
// tjsDataPack: the DataPack container

/// The binary struct pack header (`krkr-tjs2/src/runtime/builtins.rs`,
/// `BINARY_STRUCT_HEADER`) the engine's struct reader and this writer share.
const BINARY_STRUCT_HEADER: &[u8; 8] = b"KBAD100\0";

/// The footer this port appends after the pack, laid out
/// `[u32 pack_offset][u32 pack_len][u32 seed][u8 version]["KDPK"]`.
///
/// A file this port writes is `[thumbnail image][pack][footer]`, the anatomy
/// the game's own bookmark writers use: `BookMarkIO_Standard.rewrite` saves
/// the layer image and then runs `Dictionary.saveStruct(file, a0.size + "o")`
/// — the struct lands *after* the image, at the offset the digest
/// dictionary's `size` member records (system/MainWindow.tjs object 472,
/// `/tmp/m129/mainwindow.dis`). The same split lets a save slot's thumbnail
/// be the file itself, which is what the save screen loads
/// (`drawNormalItem` → `kag.getBookMarkFileNameAtNum` → `loadImages`), and
/// the pack inside is byte-for-byte the engine serializer's output. The
/// reference's own container also carries the digest seed and validates the
/// thumbnail octet ("saveDataPack: unknown thumboct format", PackinOne.dll
/// tjsDataPack module); the footer is this port's slot for both.
const DATA_PACK_FOOTER_MAGIC: &[u8; 4] = b"KDPK";
const DATA_PACK_FOOTER_VERSION: u8 = 1;
const DATA_PACK_FOOTER_LEN: usize = 17;

fn install_data_pack(runtime: &mut Runtime<KrkrHost>) {
    // tjsDataPack attaches these to the Scripts object (games call
    // `Scripts.loadDataPack(...)`); also expose them as globals for safety.
    let scripts = ensure_global_object(runtime, "Scripts");
    let global = runtime.global_handle();
    // The engine's canonical `loadDataPack` resolves a name without a `.pbd`
    // extension to `name.pbd` (the `PSDInfo.loadPBD` spelling). The reference
    // `tjsDataPack.dll` decodes the storage name it is handed, and KAGEX hands
    // it the bookmark's own file name — `BookMarkIO_DataPack.load` receives
    // `<saveDataLocation>data0.jpg` from `getBookMarkFileNameAtNum` — which the
    // `.pbd` rule would not find. The wrapper tries the exact name first and
    // falls back to the previous implementation (the engine's, also on a
    // `Plugins.link` re-registration) for every other spelling.
    let previous = runtime.object_member(scripts, "loadDataPack");
    for target in [scripts, global] {
        runtime.register_object_native(target, "saveDataPack", save_data_pack);
        runtime.register_object_native(target, "makeDataPackThumb", make_data_pack_thumb);
        runtime.register_object_native(target, "makeDataPackDigest", make_data_pack_digest);
        let previous = previous.clone();
        runtime.register_object_native(
            target,
            "loadDataPack",
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  args: Vec<Variant>| {
                load_data_pack(runtime, this_obj, args, &previous)
            },
        );
    }
}

/// `Scripts.loadDataPack(name[, options])`.
///
/// The reference `tjsDataPack.dll` reads the storage name it is handed; the
/// engine's implementation rewrites anything without a `.pbd` extension to
/// `name.pbd`. The exact name wins here, and the previous implementation —
/// the engine's, when this bundle is registered over it — handles the rest.
fn load_data_pack(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
    previous: &Variant,
) -> Result<Variant> {
    let name = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    match runtime.host_mut().read_binary(&name, "") {
        Ok(bytes) => return decode_data_pack(runtime, &bytes, &name),
        // A lazily materialized Web asset must keep its request alive.
        Err(error) if error.kind == TjsErrorKind::ResourcePending => return Err(error),
        Err(_) => {}
    }
    if matches!(previous, Variant::Void) {
        // Registered without the engine's Scripts: resolve the `.pbd` alias
        // here, the engine's rule.
        let storage_name = data_pack_storage_name(&name);
        let bytes = runtime.host_mut().read_binary(&storage_name, "")?;
        return decode_data_pack(runtime, &bytes, &storage_name);
    }
    runtime.call_function(previous.clone(), args)
}

fn decode_data_pack(
    runtime: &mut Runtime<KrkrHost>,
    bytes: &[u8],
    storage_name: &str,
) -> Result<Variant> {
    let Some(payload) = data_pack_payload(bytes) else {
        return Err(TjsError::runtime(format!(
            "Scripts.loadDataPack expected a binary data pack in `{storage_name}`"
        )));
    };
    if payload.starts_with(BINARY_STRUCT_HEADER) {
        return runtime.decode_binary_struct(payload)?.ok_or_else(|| {
            TjsError::runtime(format!(
                "Scripts.loadDataPack could not decode `{storage_name}`"
            ))
        });
    }
    runtime.decode_tjs_ns0(payload)?.ok_or_else(|| {
        TjsError::runtime(format!(
            "Scripts.loadDataPack could not decode `{storage_name}`"
        ))
    })
}

/// The pack a `DataPack` storage holds: a plain container (`KBAD100`, or
/// `TJS/ns0` for the packed UI definitions) *or* the pack inside a bookmark
/// file this port wrote — `[thumbnail image][pack][footer]`, with the
/// footer recording where the pack starts and how long it is.
fn data_pack_payload(bytes: &[u8]) -> Option<&[u8]> {
    if is_data_pack_container(bytes) {
        return Some(bytes);
    }
    let footer = parse_data_pack_footer(bytes)?;
    let start = footer.pack_offset as usize;
    let end = start.checked_add(footer.pack_len as usize)?;
    let payload = bytes.get(start..end)?;
    is_data_pack_container(payload).then_some(payload)
}

fn is_data_pack_container(bytes: &[u8]) -> bool {
    bytes.starts_with(BINARY_STRUCT_HEADER)
        || bytes.starts_with(b"TJS/ns0\0")
        || bytes.starts_with(b"TJS/4s0\0")
}

/// `Scripts.saveDataPack(name, data[, digest[, thumb]])`.
///
/// KAGEX's `BookMarkIO_DataPack.save` calls it with four arguments
/// (system/MainWindow.tjs object 474): `data` is the bookmark dictionary
/// (`id`/`core`/`user`/`history`), `digest` is `calcThumbnailSize()`'s
/// dictionary after `makeDataPackDigest` filled its `seed`, and `thumb` is
/// `makeDataPackThumb`'s encoded image (or void). The data is the pack's root
/// — the game's reader checks `id`/`core` on the value `loadDataPack` returns
/// — the pack is appended after the thumbnail image (so the save file is also
/// the slot's picture, the anatomy `BookMarkIO_Standard` writes), and the
/// digest seed rides in the footer. The reference's own container compresses
/// (LZ4) and/or encrypts the payload according to the digest dictionary's
/// `cryptmode`/`compress`/`iv` (set from `saveDataMode`, `main/Config.tjs:18`);
/// this port writes the plain form its own reader decodes.
fn save_data_pack(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let Some(data) = args.get(1).and_then(|value| value.object_handle()) else {
        // The reference raises `datapack type check failed: %1` for a subject
        // that is not a struct (PackinOne.dll tjsDataPack string pool).
        return Err(TjsError::runtime(format!(
            "saveDataPack: datapack type check failed: {name}"
        )));
    };
    let pack = encode_struct_pack(runtime, data)?;
    let seed = digest_seed(runtime, args.get(2));
    let thumb = pack_thumbnail(runtime, args.get(3));
    let mut bytes =
        Vec::with_capacity(thumb.as_ref().map_or(0, Vec::len) + pack.len() + DATA_PACK_FOOTER_LEN);
    if let Some(thumb) = &thumb {
        bytes.extend_from_slice(thumb);
    }
    let pack_offset = bytes.len();
    bytes.extend_from_slice(&pack);
    append_data_pack_footer(&mut bytes, pack_offset, pack.len(), seed);
    runtime.host_mut().write_binary(&name, "b", &bytes)?;
    Ok(Variant::Void)
}

/// `Scripts.makeDataPackThumb(layer, ext, quality, component)`.
///
/// `BookMarkIO_DataPack` picks the thumbnail format from `saveThumbnail` —
/// `jpg` for 3, `png` otherwise, `kdt` when thumbnails are off — and the save
/// path passes the captured layer to this function (object 474) before handing
/// its result to `saveDataPack`. This port has no JPEG/PNG encoder reachable
/// from the plugin, so the captured image is a 24-bit BMP of the layer's main
/// bitmap: the bytes are stored in the pack's footer, never re-encoded. `kdt`
/// means "no thumbnail", so it produces none.
fn make_data_pack_thumb(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let extension = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("kdt") {
        return Ok(Variant::Void);
    }
    let Some(layer) = args.first().and_then(|value| value.object_handle()) else {
        return Ok(Variant::Void);
    };
    match encode_layer_thumbnail(runtime, layer) {
        Ok(bytes) => Ok(Variant::Octet(bytes)),
        Err(error) => {
            runtime.host_mut().log(&format!(
                "PackinOne.dll: makeDataPackThumb could not capture the layer: {error}"
            ));
            Ok(Variant::Void)
        }
    }
}

/// `Scripts.makeDataPackDigest(subject, seed, key[, flag])`.
///
/// KAGEX derives save key material from it: `BookMarkIO_DataPack.save` calls
/// `makeDataPackDigest(data, System.getTickCount() & 0xffffffff, saveDataID)`
/// and stores the result as the digest dictionary's `seed` member (object
/// 474); `Initialize.tjs`'s `MakeLockKey` and `gridchain.calchash` use the
/// four-argument form, hashing *storage names* there. The reference DLL
/// bundles xxHash (and LZ4 for its container); this port keeps a plain FNV-1a
/// over the serialized subject — or the storage's bytes for a name — mixed
/// with the seed, key and flag, so the value is deterministic across runs.
fn make_data_pack_digest(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let mut hash = 0x811c_9dc5u32;
    if let Some(subject) = args.first() {
        match subject.object_handle() {
            Some(handle) => fnv1a(&mut hash, &encode_struct_pack(runtime, handle)?),
            None => {
                let text = subject.to_tjs_string()?;
                match runtime.host_mut().read_binary(&text, "") {
                    Ok(bytes) => fnv1a(&mut hash, &bytes),
                    Err(_) => fnv1a(&mut hash, text.as_bytes()),
                }
            }
        }
    }
    let seed = args
        .get(1)
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(0);
    let key = args.get(2).cloned().unwrap_or_default().to_tjs_string()?;
    let flag = args
        .get(3)
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(0);
    fnv1a(&mut hash, &(seed as u32).to_le_bytes());
    fnv1a(&mut hash, key.as_bytes());
    fnv1a(&mut hash, &(flag as u32).to_le_bytes());
    Ok(Variant::Integer(i64::from(hash)))
}

fn fnv1a(hash: &mut u32, bytes: &[u8]) {
    for &byte in bytes {
        *hash ^= u32::from(byte);
        *hash = hash.wrapping_mul(0x0100_0193);
    }
}

/// The digest dictionary's `seed`, the value `makeDataPackDigest` produced.
fn digest_seed(runtime: &Runtime<KrkrHost>, digest: Option<&Variant>) -> Option<u32> {
    let handle = digest.and_then(|value| value.object_handle())?;
    match runtime.object_member(handle, "seed") {
        Variant::Integer(seed) => Some(seed as u32),
        _ => None,
    }
}

/// The thumbnail image the save file should lead with.
///
/// The save path hands over `makeDataPackThumb`'s `Octet`; KAGEX's rewrite
/// path passes the *file name* it is rewriting (object 476 →
/// `saveDataPack(a0, a1, l0, a0)` in `rewriteBookMarkToFile`), which reuses
/// the pack's existing thumbnail; a raw layer is accepted the same way
/// `makeDataPackThumb` takes one.
fn pack_thumbnail(runtime: &mut Runtime<KrkrHost>, thumb: Option<&Variant>) -> Option<Vec<u8>> {
    match thumb {
        Some(Variant::Octet(bytes)) if !bytes.is_empty() => Some(bytes.clone()),
        Some(Variant::String(name)) => read_pack_thumbnail(runtime, name),
        Some(value) => value
            .object_handle()
            .and_then(|layer| encode_layer_thumbnail(runtime, layer).ok()),
        None => None,
    }
}

/// The image a previous save leads with, so a rewrite that passes its own
/// file name keeps the slot's picture (`BookMarkIO_Standard.rewrite` loads the
/// old image and re-saves the layer; the DataPack path's rewrite simply hands
/// the file back to `saveDataPack`).
fn read_pack_thumbnail(runtime: &mut Runtime<KrkrHost>, name: &str) -> Option<Vec<u8>> {
    if name.is_empty() {
        return None;
    }
    let bytes = match runtime.host_mut().read_binary(name, "") {
        Ok(bytes) => bytes,
        Err(_) => {
            let storage_name = data_pack_storage_name(name);
            runtime.host_mut().read_binary(&storage_name, "").ok()?
        }
    };
    let footer = parse_data_pack_footer(&bytes)?;
    let offset = footer.pack_offset as usize;
    (offset > 0 && offset <= bytes.len()).then(|| bytes[..offset].to_vec())
}

/// A 24-bit BMP of the layer's main bitmap, top-down in the engine's RGBA
/// store to bottom-up BGR rows.
fn encode_layer_thumbnail(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Result<Vec<u8>> {
    let layer = runtime.bound_this(layer).unwrap_or(layer);
    layer_bitmap_read(runtime, layer, |view| {
        let width = view.bitmap.width as usize;
        let height = view.bitmap.height as usize;
        let stride = view.bitmap.pitch as usize;
        let row_bytes = (width * 3).div_ceil(4) * 4;
        let image_size = row_bytes * height;
        let mut out = Vec::with_capacity(54 + image_size);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&((54 + image_size) as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(width as i32).to_le_bytes());
        out.extend_from_slice(&(height as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(image_size as u32).to_le_bytes());
        out.extend_from_slice(&2835u32.to_le_bytes());
        out.extend_from_slice(&2835u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for y in (0..height).rev() {
            let start = (y * stride).min(view.pixels.len());
            let end = (start + width * 4).min(view.pixels.len());
            for pixel in view.pixels[start..end].chunks_exact(4) {
                out.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
            let row_end = 54 + (height - y) * row_bytes;
            out.resize(row_end, 0);
        }
        out
    })
}

fn append_data_pack_footer(
    out: &mut Vec<u8>,
    pack_offset: usize,
    pack_len: usize,
    seed: Option<u32>,
) {
    out.extend_from_slice(&(pack_offset as u32).to_le_bytes());
    out.extend_from_slice(&(pack_len as u32).to_le_bytes());
    out.extend_from_slice(&seed.unwrap_or(0).to_le_bytes());
    out.push(DATA_PACK_FOOTER_VERSION);
    out.extend_from_slice(DATA_PACK_FOOTER_MAGIC);
}

/// The footer's contents, when the storage has one: where the pack sits, how
/// long it is, and the digest seed the save path computed.
struct DataPackFooter {
    pack_offset: u32,
    pack_len: u32,
    #[allow(dead_code)]
    seed: u32,
}

fn parse_data_pack_footer(bytes: &[u8]) -> Option<DataPackFooter> {
    if bytes.len() < DATA_PACK_FOOTER_LEN {
        return None;
    }
    let footer = &bytes[bytes.len() - DATA_PACK_FOOTER_LEN..];
    if footer[12] != DATA_PACK_FOOTER_VERSION || &footer[13..17] != DATA_PACK_FOOTER_MAGIC {
        return None;
    }
    Some(DataPackFooter {
        pack_offset: u32::from_le_bytes(footer[0..4].try_into().ok()?),
        pack_len: u32::from_le_bytes(footer[4..8].try_into().ok()?),
        seed: u32::from_le_bytes(footer[8..12].try_into().ok()?),
    })
}

/// Encodes `root` as a `KBAD100` struct pack.
///
/// The engine exposes only the *decoder* to plugins
/// (`Runtime::decode_binary_struct`), so the writer mirrors
/// `krkr-tjs2/src/runtime/builtins.rs`'s `BinaryStructSerializer` tag for tag:
/// same header, same scalar tags and widths, same string/octet/array/map
/// headers, cycles degrade to `null`. A `Dictionary` (class info) becomes a
/// map, an array becomes an array, any other object becomes `null` — exactly
/// the engine's own binary `saveStruct`.
fn encode_struct_pack(runtime: &Runtime<KrkrHost>, root: ObjectHandle) -> Result<Vec<u8>> {
    let mut writer = StructPackWriter::new(runtime);
    let mut bytes = Vec::from(BINARY_STRUCT_HEADER);
    writer.value(&Variant::Object(root), &mut bytes)?;
    Ok(bytes)
}

struct StructPackWriter<'a> {
    runtime: &'a Runtime<KrkrHost>,
    active: BTreeSet<ObjectHandle>,
}

impl<'a> StructPackWriter<'a> {
    fn new(runtime: &'a Runtime<KrkrHost>) -> Self {
        Self {
            runtime,
            active: BTreeSet::new(),
        }
    }

    fn value(&mut self, value: &Variant, out: &mut Vec<u8>) -> Result<()> {
        match value {
            Variant::Void => out.push(0xc1),
            Variant::Null => out.push(0xc0),
            Variant::Integer(value) => put_pack_integer(out, *value),
            Variant::Real(value) => {
                out.push(0xcb);
                out.extend_from_slice(&value.to_bits().to_le_bytes());
            }
            Variant::String(value) => put_pack_string(out, value)?,
            Variant::Octet(value) => put_pack_octet(out, value)?,
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
        let elements = self.runtime.array_elements(handle).map(Vec::from);
        if let Some(elements) = elements {
            put_pack_array_header(out, elements.len())?;
            for value in elements {
                self.value(&value, out)?;
            }
        } else if self
            .runtime
            .object_class_infos(handle)
            .iter()
            .any(|info| info == "Dictionary")
        {
            let entries = self.runtime.object_members(handle);
            put_pack_map_header(out, entries.len())?;
            for (key, value) in entries {
                put_pack_string(out, &key)?;
                self.value(&value, out)?;
            }
        } else {
            out.push(0xc0);
        }
        self.active.remove(&handle);
        Ok(())
    }
}

fn put_pack_integer(out: &mut Vec<u8>, value: i64) {
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
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn put_pack_string(out: &mut Vec<u8>, value: &str) -> Result<()> {
    let units = value.encode_utf16().collect::<Vec<_>>();
    put_pack_string_header(out, units.len())?;
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

fn put_pack_string_header(out: &mut Vec<u8>, len: usize) -> Result<()> {
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

fn put_pack_octet(out: &mut Vec<u8>, value: &[u8]) -> Result<()> {
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

fn put_pack_array_header(out: &mut Vec<u8>, len: usize) -> Result<()> {
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

fn put_pack_map_header(out: &mut Vec<u8>, len: usize) -> Result<()> {
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

// ---------------------------------------------------------------------------
// scriptsEx surface on Scripts

fn install_scripts_ex(runtime: &mut Runtime<KrkrHost>) {
    let scripts = ensure_global_object(runtime, "Scripts");
    runtime.register_object_native(scripts, "encodeTBPS", first_arg_string);
    runtime.register_object_native(scripts, "decodeTBPS", first_arg_string);
    runtime.register_object_native(scripts, "clone", scripts_clone);
    runtime.register_object_native(scripts, "isNullContext", scripts_is_null_context);
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
    runtime.register_object_native(scripts, "safeEvalStorage", safe_eval_storage);
}

/// Evaluates a persisted TJS/KSD value while preserving PackinOne's
/// error-tolerant contract. Returning void unconditionally makes a first-run
/// check appear true on every launch. ResourcePending must still escape so a
/// Web asset scheduler can fetch a lazy save file.
fn safe_eval_storage(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let scripts = match runtime.global_member("Scripts") {
        Variant::Object(handle) => handle,
        _ => return Ok(Variant::Void),
    };

    match runtime.call_object_method(scripts, "evalStorage", args) {
        Ok(value) => Ok(value),
        Err(error) if error.kind == TjsErrorKind::ResourcePending => Err(error),
        Err(error) => {
            runtime.host_mut().log(&format!(
                "PackinOne.dll: safeEvalStorage({name}) failed: {error}"
            ));
            Ok(Variant::Void)
        }
    }
}

/// scriptsEx exposes whether a function/object closure carries an ObjThis
/// context. Action.tjs uses this to bind unqualified completion callbacks to
/// the action instance before invoking them.
fn scripts_is_null_context(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let is_null =
        !matches!(args.first(), Some(Variant::Closure(closure)) if closure.this_obj.is_some());
    Ok(Variant::Integer(i64::from(is_null)))
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
    // `Scripts.clone(new Dictionary())` and every `this`-born argument arrive
    // as self-bound closures; clone the object behind the binding.
    let Some(source) = value.object_handle() else {
        return Ok(value.clone());
    };
    if let Some(dest) = cloned.get(&source) {
        return Ok(Variant::Object(*dest));
    }

    if let Some(elements) = runtime.array_elements(source).map(Vec::from) {
        let dest = runtime.alloc_array_object(Vec::new());
        cloned.insert(source, dest);
        for element in elements {
            let element = clone_scripts_value(runtime, &element, cloned)?;
            runtime.array_push(dest, element);
        }
        return Ok(Variant::Object(dest));
    }

    let is_dictionary = runtime
        .object_class_infos(source)
        .iter()
        .any(|class| class == "Dictionary");
    if is_dictionary {
        // The Dictionary constructor answers a self-bound instance.
        let constructor = runtime.global_member("Dictionary");
        let Some(dest) = runtime
            .call_function(constructor, Vec::new())?
            .object_handle()
        else {
            return Ok(value.clone());
        };
        cloned.insert(source, dest);
        for (name, member) in runtime.object_members(source) {
            if scripts_clone_builtin_member(&name) {
                continue;
            }
            let member = clone_scripts_value(runtime, &member, cloned)?;
            runtime.set_object_member(dest, name, member);
        }
        return Ok(Variant::Object(dest));
    }

    if !matches!(runtime.object_member(source, "clone"), Variant::Void)
        && let Ok(result) = runtime.call_object_method(source, "clone", Vec::new())
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
    use std::{fs, path::Path, time::SystemTime};

    use krkr_assets::ProjectStorage;
    use krkr_core::{FrameInput, Size};
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths};
    use krkr_tjs2::runtime::Closure;

    use super::*;

    /// `Scripts.saveDataPack` writes the pack KAGEX's save path needs: the
    /// data dictionary is the root (`id`/`core` are read back by
    /// `readBookMarkFromFile`), the file leads with the thumbnail image the
    /// save screen loads, and it is readable under the exact storage name the
    /// game hands to `loadDataPack` — `data0.jpg`, not its `.pbd` alias.
    #[test]
    fn save_data_pack_round_trips_through_the_shared_reader() {
        let root = test_root("packinone-datapack-save");
        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var data = %[id => "save-id", core => %[storeTime => 1234],
                                 user => %[], history => %[]];
                    var digest = %[width => 4, height => 2, ext => "jpg"];
                    digest.seed = Scripts.makeDataPackDigest(data, 7, "save-id");
                    global.thumbLayer = new Layer();
                    thumbLayer.setImageSize(4, 2);
                    thumbLayer.fillRect(0, 0, 4, 2, 0x80112233);
                    var thumb = Scripts.makeDataPackThumb(thumbLayer, "jpg", void, void);
                    Scripts.saveDataPack("savedata/data0.jpg", data, digest, thumb);
                    var loaded = Scripts.loadDataPack("savedata/data0.jpg");
                    return loaded.id + ":" + loaded.core.storeTime + ":" +
                        digest.seed + ":" +
                        (Storages.isExistentStorage("savedata/data0.jpg") ? 1 : 0);
                })()"#,
            )
            .expect("save a data pack");
        let Variant::String(text) = &value else {
            panic!("unexpected probe value {value:?}");
        };
        let fields: Vec<&str> = text.split(':').collect();
        assert_eq!(fields.len(), 4, "{text}");
        assert_eq!(&fields[..2], ["save-id", "1234"]);
        let seed: u32 = fields[2].parse().expect("digest seed");
        assert_eq!(fields[3], "1", "the pack is readable under its own name");

        let bytes = fs::read(root.join("savedata/data0.jpg")).expect("read the pack");
        let footer = parse_data_pack_footer(&bytes).expect("pack footer");
        assert_eq!(footer.seed, seed, "the digest seed travels in the footer");
        // A 4x2 24-bit BMP leads the file: `0x80112233` is an opaque enough
        // `112233` pixel, stored bottom-up as BGR with 4-byte row padding.
        let thumb = &bytes[..footer.pack_offset as usize];
        assert_eq!(thumb.len(), 54 + 12 * 2);
        assert_eq!(&thumb[..2], b"BM");
        assert_eq!(&thumb[54..57], [0x33, 0x22, 0x11]);
        let pack = &bytes[footer.pack_offset as usize..];
        assert!(
            pack.starts_with(b"KBAD100\0"),
            "the shared struct header follows the image"
        );
        assert_eq!(footer.pack_len as usize, pack.len() - DATA_PACK_FOOTER_LEN);

        // The save screen loads the slot's picture from the file itself
        // (`drawNormalItem` → `DataStore.getFileName` → `loadImages`), so the
        // leading image has to decode with the pack and footer trailing it.
        engine
            .execute_script(
                "inline.tjs",
                "global.slotView = new Layer();\n\
                 slotView.loadImages(\"savedata/data0.jpg\");",
            )
            .expect("load the save's leading thumbnail");
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "slotView.imageWidth + \"x\" + slotView.imageHeight",
                )
                .expect("thumbnail size"),
            Variant::String("4x2".to_string())
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "slotView.getMainPixel(0, 0)")
                .expect("thumbnail pixel"),
            Variant::Integer(0x112233)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The writer has to agree with the engine's own `saveStruct(..., "b")`
    /// byte for byte: the reader is shared, and a divergence would corrupt
    /// every save. With no digest and no thumbnail the pack is exactly the
    /// engine's output plus this port's fixed-size footer.
    #[test]
    fn save_data_pack_matches_the_engine_binary_struct_writer() {
        let root = test_root("packinone-datapack-bytes");
        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        engine
            .execute_script(
                "inline.tjs",
                r#"(function() {
                    var data = %[id => "x", core => %[storeTime => 5, name => "あ"],
                                 list => [1, 2, <% 01 02 %>], flag => null, neg => -2];
                    (Dictionary.saveStruct incontextof data)("engine.pbd", "b");
                    Scripts.saveDataPack("mine.pbd", data);
                })();"#,
            )
            .expect("write both packs");
        let engine_bytes = fs::read(root.join("engine.pbd")).expect("engine pack");
        let mine = fs::read(root.join("mine.pbd")).expect("plugin pack");
        assert_eq!(
            &mine[..mine.len() - DATA_PACK_FOOTER_LEN],
            engine_bytes.as_slice(),
            "the payload must be the engine serializer's output"
        );
        let footer = parse_data_pack_footer(&mine).expect("footer");
        assert_eq!(footer.pack_offset, 0, "no thumbnail leads this file");
        assert_eq!(
            footer.pack_len as usize,
            engine_bytes.len(),
            "the footer points at the whole engine payload"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The digest is deterministic and mixes its arguments; `makeDataPackThumb`
    /// answers an `Octet` image for a drawable layer and nothing for `kdt`.
    #[test]
    fn data_pack_digest_and_thumb_follow_their_contracts() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var data = %[id => "d", core => %[storeTime => 1]];
                    var first = Scripts.makeDataPackDigest(data, 1, "key");
                    var second = Scripts.makeDataPackDigest(data, 1, "key");
                    var other = Scripts.makeDataPackDigest(data, 2, "key");
                    global.layer = new Layer();
                    layer.setImageSize(2, 1);
                    layer.fillRect(0, 0, 2, 1, 0xff102030);
                    var thumb = Scripts.makeDataPackThumb(layer, "png");
                    var kdt = Scripts.makeDataPackThumb(layer, "kdt");
                    return (first == second) + ":" + (first != other) + ":" +
                        typeof thumb + ":" + typeof kdt;
                })()"#,
            )
            .expect("digest and thumbnail probes");
        assert_eq!(value, Variant::String("1:1:Octet:void".to_string()));
    }

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

        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression("inline.tjs", "Scripts.loadDataPack(\"probe.pbd\").answer")
            .expect("load data pack");

        assert_eq!(value, Variant::Integer(42));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn csv_parse_storage_requests_manifest_resource_before_reading() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-packinone-csv-pending-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");

        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        engine.set_external_resource_catalog(["title_first.func"]);

        // `csvProbe` has to outlive this script: the calls below are separate
        // executions and read it back through the global object.  An
        // unqualified store cannot create a name (`tTJSCustomObject::PropSet`
        // adds a member only under `TJS_MEMBERENSURE`, `tjsObject.cpp:1500-1505`,
        // and the compiler emits flags 0 for it), so the global receiver is
        // spelled out -- the same reason KRKR's own scripts write
        // `global.foo = ...`.
        let result = engine
            .execute_script("inline.tjs", "global.csvProbe = new CSVParser();")
            .expect("CSV parser allocation");
        assert_eq!(result, Variant::Void);
        let result = engine
            .execute_script("inline.tjs", "csvProbe.parseStorage(\"title_first.func\");")
            .expect("resource-pending VM execution parks and returns void");
        assert_eq!(result, Variant::Void);
        // The reference `csvParser.dll` opens the storage with
        // `TVPCreateTextStreamForRead`, so a lazily materialized package fetches
        // it as text — the same path that decodes KiriKiri's ciphered text
        // streams.
        assert_eq!(
            engine.take_external_resource_requests(),
            vec![("title_first.func".to_owned(), krkr_core::AssetKind::Text)]
        );

        engine
            .provide_external_resource("title_first.func", b"name,value\nstart,1\n".to_vec())
            .expect("provide fetched CSV");
        assert!(!engine.is_script_suspended());
        engine
            .update(
                krkr_engine::EngineInput::new(
                    FrameInput::new(Size::new(640.0, 360.0), 0.0),
                    vec![],
                ),
                std::time::Duration::ZERO,
            )
            .expect("suspended CSV call should resume after bytes arrive");
        let file = engine
            .execute_expression("inline.tjs", "csvProbe.__csvFile")
            .expect("CSV parser state should be visible after resume");
        assert_eq!(file, Variant::String("title_first.func".to_owned()));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn csv_parser_honors_constructor_separator() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-packinone-csv-separator-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");

        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var parser = new CSVParser(void, 9);
                    parser.init("name\tvalue\r\nsoldier\tfront");
                    var first = parser.getNextLine();
                    var second = parser.getNextLine();
                    return first.count + ":" + first[0] + ":" + first[1] + ":" +
                        second.count + ":" + second[0] + ":" + second[1];
                })()"#,
            )
            .expect("parse tab-separated records");

        assert_eq!(
            value,
            Variant::String("2:name:value:2:soldier:front".to_owned())
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn safe_eval_storage_restores_structured_system_state() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-packinone-safe-eval-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        fs::write(
            root.join("system-state.ksd"),
            "(const) %[\"notFirst\" => 1, \"language\" => \"en\",]",
        )
        .expect("write system state");

        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "Scripts.safeEvalStorage(\"system-state.ksd\").notFirst",
            )
            .expect("safe eval storage");

        assert_eq!(value, Variant::Integer(1));
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

        let mut engine = test_engine(&root);
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

    #[test]
    fn scripts_is_null_context_matches_scriptsex_objthis_semantics() {
        let mut runtime = Runtime::with_host(KrkrHost::default());
        let object = runtime.alloc_ordinary_object();

        assert_eq!(
            scripts_is_null_context(
                &mut runtime,
                None,
                vec![Variant::Closure(Closure::new(object, None))],
            )
            .expect("unbound closure"),
            Variant::Integer(1)
        );
        assert_eq!(
            scripts_is_null_context(
                &mut runtime,
                None,
                vec![Variant::Closure(Closure::new(object, Some(object)))],
            )
            .expect("bound closure"),
            Variant::Integer(0)
        );
    }

    /// `Scripts.clone` receives its argument through the VM, so a `new
    /// Dictionary()` (and every `this`-born object) arrives as a self-bound
    /// closure; the clone must copy the object behind the binding instead of
    /// returning the argument unchanged.
    #[test]
    fn scripts_clone_copies_objects_born_from_new() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var source = new Dictionary();
                    source.answer = 2;
                    source.nested = new Dictionary();
                    source.nested.answer = 3;
                    var copy = Scripts.clone(source);
                    copy.answer = 7;
                    copy.nested.answer = 9;
                    return source.answer + ":" + copy.answer + ":" +
                        source.nested.answer + ":" + copy.nested.answer;
                })()"#,
            )
            .expect("clone dictionary");
        assert_eq!(value, Variant::String("2:7:3:9".to_owned()));
    }

    /// `CSVParser.target` is normally assigned by the game, so it reads back
    /// as a script-born value; `doLine` has to be looked up on the object
    /// behind the binding, not fall back to the parser itself.
    #[test]
    fn csv_parser_fires_do_line_on_a_script_assigned_target() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    // A nested function addresses enclosing names through the
                    // this-proxy, so the collector lives on the global object.
                    global.__csvRows = [];
                    var target = new Dictionary();
                    target.doLine = function(fields, line) {
                        global.__csvRows.push(line + ":" + fields[0] + ":" + fields[1]);
                    };
                    var parser = new CSVParser();
                    parser.target = target;
                    parser.parse("a,b\nc,d\n");
                    return global.__csvRows.join("|") + " lines=" + parser.currentLineNumber;
                })()"#,
            )
            .expect("csv parser target");
        assert_eq!(value, Variant::String("1:a:b|2:c:d lines=2".to_owned()));
    }

    /// `Plugins.link("packinone.dll")` runs the bundle's `register` again
    /// (`krkr-engine/src/host.rs::plugin_to_install`). Catalog order puts the
    /// bundle before the layerEx* modules, so by the time a game links it the
    /// layer effects are the real implementations — `Variant::Object` natives,
    /// not the `Closure`s the old guard skipped, so the fill-in reverted them
    /// to no-ops (measured: `light(10, 0)` over `0x80404040` gave `0x4a4a4a`
    /// before the link and `0x404040` after).
    #[test]
    fn linking_the_bundle_keeps_the_real_layer_ex_natives() {
        let mut engine = layer_ex_engine();
        let members = layer_effect_members(&engine);
        let before = layer_probe(&mut engine);
        assert_eq!(before, LAYER_PROBE, "the layerEx* natives ran");

        engine
            .execute_script("link.tjs", r#"Plugins.link("packinone.dll");"#)
            .expect("link");

        assert_eq!(
            layer_probe(&mut engine),
            LAYER_PROBE,
            "the link reverted a real native to a no-op"
        );
        assert_eq!(
            layer_effect_members(&engine),
            members,
            "the link replaced a Layer member"
        );
    }

    /// When only the bundle is registered every name in the list is a member
    /// nothing owns, so the fill-in still has to cover them: the no-op is what
    /// keeps a game's `light` from failing with `Member "light" does not
    /// exist`. The link runs the same registration again and must not drop it.
    #[test]
    fn layer_effect_fill_in_still_covers_members_nothing_owns() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");

        let filled = |engine: &KrkrEngine| {
            layer_effect_members(engine)
                .iter()
                .all(|member| !matches!(member, Variant::Void))
        };
        assert!(filled(&engine), "the fill-in covers the absent members");

        engine
            .execute_script("link.tjs", r#"Plugins.link("packinone.dll");"#)
            .expect("link");
        assert!(filled(&engine), "the link re-runs the fill-in");

        // Callable, and harmless: the pixels keep their fill.
        assert_eq!(light_pixel(&mut engine), 0x404040);
        assert_eq!(alpha_value(&mut engine), 0x80);
    }

    /// The bundle then the layerEx* modules, in catalog order.
    fn layer_ex_engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("packinone");
        engine
            .register_plugin(crate::layer_ex_btoa::LayerExBtoaPlugin)
            .expect("layerExBTOA");
        engine
            .register_plugin(crate::layer_ex_image::LayerExImagePlugin)
            .expect("layerExImage");
        engine
            .register_plugin(crate::layer_ex_raster::LayerExRasterPlugin)
            .expect("layerExRaster");
        engine
    }

    /// The raw `Layer` member each filled-in name holds, so a link that swaps
    /// one is visible even where no plugin implements the name.
    fn layer_effect_members(engine: &KrkrEngine) -> Vec<Variant> {
        let runtime = engine.tjs_runtime();
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            panic!("the engine registers a Layer class object");
        };
        LAYER_EFFECT_METHODS
            .iter()
            .map(|method| runtime.object_member(layer, method))
            .collect()
    }

    /// The pixels a run of the three layer effect families leaves behind:
    /// `light`, `copyRaster` and `fillAlpha`.
    fn layer_probe(engine: &mut KrkrEngine) -> (i64, i64, i64) {
        (
            light_pixel(engine),
            raster_pixel(engine),
            alpha_value(engine),
        )
    }

    /// `LayerExImage.cpp:48-59` with brightness 10 over `0x80404040`:
    /// `clamp(channel + 10)` per channel. A no-op leaves the fill's `0x404040`.
    fn light_pixel(engine: &mut KrkrEngine) -> i64 {
        engine
            .execute_script(
                "probe-light.tjs",
                r#"
                global.probe = new Layer();
                probe.setImageSize(2, 1);
                probe.fillRect(0, 0, 2, 1, 0x80404040);
                probe.light(10, 0);
                "#,
            )
            .expect("light");
        read_integer(engine, "probe.getMainPixel(0, 0)")
    }

    /// `layerExRaster`'s `main.cpp:61-78` with `maxh = 0` and `time = 0`, which
    /// shifts every row by zero: a plain copy of `0x112233` onto transparent
    /// black. A no-op leaves the destination's `0x000000`.
    fn raster_pixel(engine: &mut KrkrEngine) -> i64 {
        engine
            .execute_script(
                "probe-raster.tjs",
                r#"
                global.rasterSource = new Layer();
                rasterSource.setImageSize(5, 2);
                rasterSource.fillRect(0, 0, 5, 2, 0x80112233);
                global.probe = new Layer();
                probe.setImageSize(5, 2);
                probe.fillRect(0, 0, 5, 2, 0x80000000);
                probe.copyRaster(rasterSource, 0, 4, 4, 0);
                "#,
            )
            .expect("copyRaster");
        read_integer(engine, "probe.getMainPixel(0, 0)")
    }

    /// `layerExBTOA`'s `fillAlpha` (`main.cpp:128-140`) raises the clip box's
    /// alpha byte to `0xff`. A no-op leaves the fill's `0x80`.
    fn alpha_value(engine: &mut KrkrEngine) -> i64 {
        engine
            .execute_script(
                "probe-alpha.tjs",
                r#"
                global.probe = new Layer();
                probe.setImageSize(2, 1);
                probe.fillRect(0, 0, 2, 1, 0x80112233);
                probe.fillAlpha();
                "#,
            )
            .expect("fillAlpha");
        read_integer(engine, "probe.getMaskPixel(0, 0)")
    }

    /// `light` + `copyRaster` + `fillAlpha`, each with its real
    /// implementation's result.
    const LAYER_PROBE: (i64, i64, i64) = (0x4a4a4a, 0x112233, 0xff);

    fn read_integer(engine: &mut KrkrEngine, expression: &str) -> i64 {
        engine
            .execute_expression("read.tjs", expression)
            .expect("expression")
            .to_integer()
            .expect("integer")
    }

    fn test_root(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-packinone-{name}-{}-{unique}",
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
