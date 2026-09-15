//! PackinOne.dll compatibility shim.
//!
//! Bundle plugin combining fstat/savestruct/scriptsEx/systemEx/shrinkCopy/
//! layerExImage/layerExRaster/csvParser/process/tjsDataPack and more. Only the
//! surface games actually call is functional:
//!
//! - `CSVParser` and the shared `Scripts` helpers: the bundle compiles the
//!   standalone `csvParser.dll` and `scriptsEx.dll` sub-plugins in, so it
//!   installs *their* single implementation (`crate::csv_parser`,
//!   `crate::scripts_ex`) instead of carrying a copy — which DLL a game links
//!   no longer decides what `new CSVParser(...)`, `Scripts.clone`,
//!   `Scripts.getMD5HashString` and friends answer.
//! - `Storages.saveOctet` / `Storages.loadOctet`: binary storage I/O.
//! - `System.urlencode` / `System.urldecode`: UTF-8 percent codec; decoding
//!   leaves `+` untouched (no form-style space mapping).
//! - `Scripts.loadDataPack` / `Scripts.saveDataPack` / `Scripts.makeDataPackThumb`
//!   / `Scripts.makeDataPackDigest`: the tjsDataPack surface. The loader reads
//!   the reference container — an optional leading JPEG/PNG thumbnail, the
//!   16-byte `TJS/` header (seed, crypt mode, IV), then the LZ4-framed and/or
//!   ChaCha-encrypted `TJS/ns0` value stream — plus the plain `KBAD100` and
//!   `TJS/ns0` packs used by packed UI definitions, under the exact storage
//!   name it is handed (the engine's `.pbd` alias stays as the fallback); the
//!   writer emits that same container back, thumbnail leading, with the digest
//!   seed in the header. See the `tjsDataPack` section for the reference
//!   anchors and the deliberate divergences.
//!
//! Everything else (System version/env shims, Layer effect methods, Process,
//! fstat, proxyfs, ...) is a no-op stub returning benign values. The
//! `safeEvalStorage` wrapper remains functional because KRKR startup scripts
//! use it to restore system variables before choosing their opening flow.

use std::sync::atomic::{AtomicBool, Ordering};

use krkr_engine::{KrkrHost, KrkrPlugin, plugin_api::layer::layer_bitmap_read};
use krkr_tjs2::{
    Result, TjsError, TjsErrorKind,
    runtime::{ObjectHandle, Runtime, TjsHost, Variant, tjs_ns0 as ns0},
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
        // `PackinOne.dll` compiles the standalone sub-plugins in; the bundle
        // installs their one implementation instead of carrying copies, so the
        // DLL a game links no longer decides what the shared surfaces do.
        crate::csv_parser::install_csv_parser(runtime);
        install_storages_octet(runtime);
        install_system_ex(runtime);
        install_layer_effects(runtime);
        install_data_pack(runtime);
        crate::scripts_ex::install_scripts_ex(runtime);
        install_scripts_ex_extras(runtime);
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

// ---------------------------------------------------------------------------
// CSVParser: the standalone `csvParser` module installs the class
//
// `PackinOne.dll` compiles the same `csvParser.dll` source in, so the bundle
// installs the *one* implementation (`crate::csv_parser`) under the same
// global rather than carrying its own copy with different answers. Which DLL a
// game links therefore no longer decides what `new CSVParser(...)` does.

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
/// `BINARY_STRUCT_HEADER`) the engine's struct reader and the pack decoder
/// share. This port only *reads* it now: the reference container below is what
/// the writer emits.
const BINARY_STRUCT_HEADER: &[u8; 8] = b"KBAD100\0";

/// The footer this port's previous writer appended, laid out
/// `[u32 pack_offset][u32 pack_len][u32 seed][u8 version]["KDPK"]`
/// (M133). The reference container has no footer, so this is only parsed to
/// keep saves written by that build loading.
const DATA_PACK_FOOTER_MAGIC: &[u8; 4] = b"KDPK";
const DATA_PACK_FOOTER_VERSION: u8 = 1;
const DATA_PACK_FOOTER_LEN: usize = 17;

/// The raw LZ4 block size the reference frames at (`FUN_10051220`'s 0x1000
/// buffered stream is what feeds `LZ4CompressStream`).
const DATAPACK_LZ4_BLOCK: usize = 4096;

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

/// Decodes a `DataPack` storage: the reference container (optionally behind
/// the leading JPEG/PNG thumbnail a bookmark file starts with), a plain
/// `KBAD100` pack, or — for files this port wrote before the container switch
/// — the pack behind a `KDPK` footer.
fn decode_data_pack(
    runtime: &mut Runtime<KrkrHost>,
    bytes: &[u8],
    storage_name: &str,
) -> Result<Variant> {
    if let Some(pack) = reference_data_pack(bytes) {
        return decode_reference_data_pack(runtime, pack, storage_name);
    }
    if is_plain_data_pack(bytes) {
        return decode_plain_data_pack(runtime, bytes, storage_name);
    }
    if let Some(footer) = parse_data_pack_footer(bytes) {
        let start = footer.pack_offset as usize;
        let end = start.checked_add(footer.pack_len as usize);
        if let Some(payload) = end.and_then(|end| bytes.get(start..end))
            && is_plain_data_pack(payload)
        {
            return decode_plain_data_pack(runtime, payload, storage_name);
        }
    }
    Err(TjsError::runtime(format!(
        "Scripts.loadDataPack expected a binary data pack in `{storage_name}`"
    )))
}

/// The reference container behind its optional leading image: `TJS/` or
/// `TJS\` at offset 0, or right after the JPEG/PNG a bookmark file leads
/// with (`FUN_10050020` sniffs exactly these four magics).
fn reference_data_pack(bytes: &[u8]) -> Option<&[u8]> {
    if starts_with_ns0_magic(bytes) {
        return Some(bytes);
    }
    let end = image_end(bytes)?;
    let pack = bytes.get(end..)?;
    starts_with_ns0_magic(pack).then_some(pack)
}

fn starts_with_ns0_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(ns0::NS0_MAGIC_LE) || bytes.starts_with(ns0::NS0_MAGIC_BE)
}

/// Decodes the reference container: 16-byte header, IV, then the transform
/// chain — ChaCha over the whole stream, LZ4 raw-block framing — ahead of the
/// seeded value stream. The writer encrypts *after* compressing, so reading
/// undoes them in that order.
fn decode_reference_data_pack(
    runtime: &mut Runtime<KrkrHost>,
    pack: &[u8],
    storage_name: &str,
) -> Result<Variant> {
    let header = ns0::parse_ns0_header(pack).map_err(|error| {
        TjsError::runtime(format!(
            "Scripts.loadDataPack could not decode `{storage_name}`: {error}"
        ))
    })?;
    let iv_end = ns0::NS0_HEADER_SIZE + header.iv_length as usize;
    let iv = pack.get(ns0::NS0_HEADER_SIZE..iv_end).ok_or_else(|| {
        TjsError::runtime(format!(
            "Scripts.loadDataPack found a truncated data pack in `{storage_name}`"
        ))
    })?;
    let mut body = pack[iv_end..].to_vec();
    if header.cryptmode != 0 {
        datapack_chacha_apply(header.cryptmode, header.seed, iv, &mut body).map_err(|error| {
            TjsError::runtime(format!(
                "Scripts.loadDataPack could not decode `{storage_name}`: {error}"
            ))
        })?;
    }
    if header.compress == ns0::NS0_COMPRESS_LZ4 {
        body = datapack_lz4_deframe(&body).map_err(|error| {
            TjsError::runtime(format!(
                "Scripts.loadDataPack could not decode `{storage_name}`: {error}"
            ))
        })?;
    }
    runtime
        .decode_tjs_ns0_body(&body, header.seed, header.big_endian)
        .map_err(|error| {
            TjsError::runtime(format!(
                "Scripts.loadDataPack could not decode `{storage_name}`: {error}"
            ))
        })
}

fn decode_plain_data_pack(
    runtime: &mut Runtime<KrkrHost>,
    bytes: &[u8],
    storage_name: &str,
) -> Result<Variant> {
    if bytes.starts_with(BINARY_STRUCT_HEADER) {
        return runtime.decode_binary_struct(bytes)?.ok_or_else(|| {
            TjsError::runtime(format!(
                "Scripts.loadDataPack could not decode `{storage_name}`"
            ))
        });
    }
    runtime.decode_tjs_ns0(bytes)?.ok_or_else(|| {
        TjsError::runtime(format!(
            "Scripts.loadDataPack could not decode `{storage_name}`"
        ))
    })
}

fn is_plain_data_pack(bytes: &[u8]) -> bool {
    bytes.starts_with(BINARY_STRUCT_HEADER) || starts_with_ns0_magic(bytes)
}

/// `Scripts.saveDataPack(name, data[, digest[, thumb]])`.
///
/// KAGEX's `BookMarkIO_DataPack.save` calls it with four arguments
/// (system/MainWindow.tjs object 474): `data` is the bookmark dictionary
/// (`id`/`core`/`user`/`history`), `digest` is `calcThumbnailSize()`'s
/// dictionary after `makeDataPackDigest` filled its `seed` (and carries
/// `compress`/`cryptmode`/`iv`, set from `saveDataMode`), and `thumb` is
/// `makeDataPackThumb`'s encoded image (or void). The file is the reference
/// container — `[thumbnail image][16-byte header][iv][LZ4-framed and/or
/// ChaCha-encrypted body]`, no offset footer: the image's own chunk structure
/// ends the thumbnail and the body follows the header. The writer runs the
/// same chain the reference does (`FUN_10052090`): the body is serialized
/// with the seeded checker, framed in raw LZ4 blocks when `compress` is set,
/// then ChaCha-encrypted when `cryptmode` is set.
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
    let options = data_pack_options(runtime, args.get(2));
    let thumb = pack_thumbnail(runtime, args.get(3));
    let mut body = runtime.encode_tjs_ns0_body(&Variant::Object(data), options.seed, false)?;
    if options.compress == ns0::NS0_COMPRESS_LZ4 {
        body = datapack_lz4_frame(&body);
    }
    if options.cryptmode != 0 {
        datapack_chacha_apply(options.cryptmode, options.seed, &options.iv, &mut body)?;
    }
    let mut bytes = Vec::with_capacity(
        thumb.as_ref().map_or(0, Vec::len) + ns0::NS0_HEADER_SIZE + options.iv.len() + body.len(),
    );
    if let Some(thumb) = &thumb {
        bytes.extend_from_slice(thumb);
    }
    append_data_pack_header(&mut bytes, &options);
    bytes.extend_from_slice(&body);
    runtime.host_mut().write_binary(&name, "b", &bytes)?;
    Ok(Variant::Void)
}

/// The header's digest-driven knobs (`BookMarkIO_DataPack`):
/// `compress` picks the `4s0` LZ4 framing, `cryptmode` the ChaCha parameter
/// set, `iv` the salt string (stored UTF-16LE plus its NUL terminator, the
/// byte form the real `m144-*.ksd` artifacts carry) and `seed` — when the
/// game leaves it zero — the writer's `"TJS"` default.
struct DataPackOptions {
    seed: u32,
    compress: u8,
    cryptmode: u16,
    iv: Vec<u8>,
}

fn data_pack_options(runtime: &Runtime<KrkrHost>, digest: Option<&Variant>) -> DataPackOptions {
    let handle = digest.and_then(|value| value.object_handle());
    let member = |name: &str| {
        handle
            .map(|handle| runtime.object_member(handle, name))
            .unwrap_or(Variant::Void)
    };
    let seed = digest_seed(runtime, digest)
        .filter(|seed| *seed != 0)
        .unwrap_or(ns0::NS0_DEFAULT_SEED);
    let compress = match member("compress") {
        Variant::Integer(value) if value != 0 => ns0::NS0_COMPRESS_LZ4,
        Variant::Real(value) if value != 0.0 => ns0::NS0_COMPRESS_LZ4,
        _ => ns0::NS0_COMPRESS_STORE,
    };
    let cryptmode = match member("cryptmode") {
        Variant::Integer(value) if (1..=6).contains(&value) => value as u16,
        _ => 0,
    };
    let iv = match member("iv") {
        // The reference stores the salt string as UTF-16LE *including its
        // NUL terminator* (`m144-*.ksd`: iv "kiri" → ivlen 10 = 2*(4+1); the
        // 20-character title artifact → 42 = 2*(20+1)).
        Variant::String(text) => {
            let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes.extend_from_slice(&[0, 0]);
            bytes
        }
        _ => Vec::new(),
    };
    DataPackOptions {
        seed,
        compress,
        cryptmode,
        iv,
    }
}

/// Writes the 16-byte header and the IV bytes (`FUN_1004e0d0`).
fn append_data_pack_header(out: &mut Vec<u8>, options: &DataPackOptions) {
    let mut header = [0u8; ns0::NS0_HEADER_SIZE];
    header[0..4].copy_from_slice(ns0::NS0_MAGIC_LE);
    header[4] = options.compress;
    header[5..8].copy_from_slice(b"s0\0");
    header[8..12].copy_from_slice(&options.seed.to_le_bytes());
    header[12..14].copy_from_slice(&options.cryptmode.to_le_bytes());
    header[14..16].copy_from_slice(&(options.iv.len() as u16).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&options.iv);
}

/// `Scripts.makeDataPackThumb(layer, ext, quality, component)`.
///
/// `BookMarkIO_DataPack` picks the thumbnail format from `saveThumbnail` —
/// `jpg` for 3, `png` otherwise, `kdt` when thumbnails are off — and the save
/// path passes the captured layer to this function (object 474) before handing
/// its result to `saveDataPack`. `kdt` means "no thumbnail", so it produces
/// none; the other extensions get a baseline JPEG or a PNG of the layer's main
/// bitmap (the reference's own encoder is IJG, so the *bytes* differ, the
/// decodable image does not).
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
    let quality = args
        .get(2)
        .and_then(|value| value.to_integer().ok())
        .filter(|quality| (1..=100).contains(quality))
        .unwrap_or(75) as u8;
    let jpeg = !extension.eq_ignore_ascii_case("png");
    let Some(layer) = args.first().and_then(|value| value.object_handle()) else {
        return Ok(Variant::Void);
    };
    match encode_layer_thumbnail(runtime, layer, jpeg, quality) {
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
/// four-argument form. The reference feeds the *big-endian* value
/// serialization of subject then key — including the per-value checker bytes,
/// zero for the zero seed the digest state starts with (`FUN_10053500`,
/// `FUN_10053450` over `VariantDigestState`/`XXH32Hasher`) — through
/// `XXH32(data, seed)` — the value a real-engine probe pinned
/// (`makeDataPackDigest(data, 7, "probe") = 882327524` for the `%["id" => ...]`
/// dictionary the M142 harness used). `flag = 1` selects the DLL's Blake2s
/// hasher (`FUN_10048910`, parameter block `[8, 4, 1, 1, 0, ...]`); this port
/// answers the XXH32 value either way and logs the substitution once.
fn make_data_pack_digest(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    static BLAKE2S_WARNED: AtomicBool = AtomicBool::new(false);
    let mut preimage = Vec::new();
    if let Some(subject) = args.first() {
        preimage.extend_from_slice(&digest_serialization(runtime, subject)?);
    }
    let seed = args
        .get(1)
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(0) as u32;
    let key = args.get(2).cloned().unwrap_or_default();
    preimage.extend_from_slice(&digest_serialization(runtime, &key)?);
    let flag = args
        .get(3)
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(0);
    if flag != 0 && !BLAKE2S_WARNED.swap(true, Ordering::Relaxed) {
        runtime.host_mut().log(
            "PackinOne.dll: makeDataPackDigest(flag=1) asks for Blake2s upstream; \
             this port answers XXH32 instead",
        );
    }
    Ok(Variant::Integer(i64::from(xxh32(&preimage, seed))))
}

/// One digest operand: the `TJS/ns0` big-endian serialization with the zero
/// seed's checker bytes and *without* the trailing final check (`FUN_10053450`
/// writes a single value into the digest state).
fn digest_serialization(runtime: &Runtime<KrkrHost>, value: &Variant) -> Result<Vec<u8>> {
    let mut body = runtime.encode_tjs_ns0_body(value, 0, true)?;
    body.truncate(body.len() - 4);
    Ok(body)
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
/// `makeDataPackThumb` takes one (as a JPEG, the `saveThumbnail = 3` default).
fn pack_thumbnail(runtime: &mut Runtime<KrkrHost>, thumb: Option<&Variant>) -> Option<Vec<u8>> {
    match thumb {
        Some(Variant::Octet(bytes)) if !bytes.is_empty() => Some(bytes.clone()),
        Some(Variant::String(name)) => read_pack_thumbnail(runtime, name),
        Some(value) => value
            .object_handle()
            .and_then(|layer| encode_layer_thumbnail(runtime, layer, true, 75).ok()),
        None => None,
    }
}

/// The image a previous save leads with, so a rewrite that passes its own
/// file name keeps the slot's picture (`BookMarkIO_Standard.rewrite` loads the
/// old image and re-saves the layer; the DataPack path's rewrite simply hands
/// the file back to `saveDataPack`). The image ends where its own chunk
/// structure says — that is how the reference recovers the header that
/// follows it — with the old `KDPK` footer's offset as the fallback for files
/// this port wrote before the container switch.
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
    let offset = image_end(&bytes).or_else(|| {
        let footer = parse_data_pack_footer(&bytes)?;
        Some(footer.pack_offset as usize)
    })?;
    (offset > 0 && offset <= bytes.len()).then(|| bytes[..offset].to_vec())
}

/// Encodes the layer's main bitmap as the save's leading image: a baseline
/// JPEG or a PNG, matching `makeDataPackThumb`'s extension choice.
fn encode_layer_thumbnail(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    jpeg: bool,
    quality: u8,
) -> Result<Vec<u8>> {
    let layer = runtime.bound_this(layer).unwrap_or(layer);
    layer_bitmap_read(runtime, layer, |view| {
        let width = view.bitmap.width as usize;
        let height = view.bitmap.height as usize;
        let stride = view.bitmap.pitch as usize;
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            let start = (y * stride).min(view.pixels.len());
            let end = (start + width * 4).min(view.pixels.len());
            rgba.extend_from_slice(&view.pixels[start..end]);
            rgba.resize((y + 1) * width * 4, 0);
        }
        if jpeg {
            encode_jpeg(&rgba, width, height, quality)
        } else {
            encode_png(&rgba, width, height)
        }
    })
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

// ---------------------------------------------------------------------------
// Reference container primitives: XXH32, BLAKE2s, ChaCha and raw-block LZ4

const XXH32_PRIME1: u32 = 0x9E37_79B1;
const XXH32_PRIME2: u32 = 0x85EB_CA77;
const XXH32_PRIME3: u32 = 0xC2B2_AE3D;
const XXH32_PRIME4: u32 = 0x27D4_EB2F;
const XXH32_PRIME5: u32 = 0x1656_67B1;

fn xxh32_round(acc: u32, input: u32) -> u32 {
    acc.wrapping_add(input.wrapping_mul(XXH32_PRIME2))
        .rotate_left(13)
        .wrapping_mul(XXH32_PRIME1)
}

/// XXH32, the hash behind `makeDataPackDigest` and the ChaCha nonce
/// (`PackinOne.dll`'s `XXH32Hasher`; the primes sit in its `.text`).
fn xxh32(data: &[u8], seed: u32) -> u32 {
    let read = |offset: usize| {
        u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    };
    let mut index = 0;
    let mut hash;
    if data.len() >= 16 {
        let mut v1 = seed.wrapping_add(XXH32_PRIME1).wrapping_add(XXH32_PRIME2);
        let mut v2 = seed.wrapping_add(XXH32_PRIME2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(XXH32_PRIME1);
        while index + 16 <= data.len() {
            v1 = xxh32_round(v1, read(index));
            v2 = xxh32_round(v2, read(index + 4));
            v3 = xxh32_round(v3, read(index + 8));
            v4 = xxh32_round(v4, read(index + 12));
            index += 16;
        }
        hash = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
    } else {
        hash = seed.wrapping_add(XXH32_PRIME5);
    }
    hash = hash.wrapping_add(data.len() as u32);
    while index + 4 <= data.len() {
        hash = hash
            .wrapping_add(read(index).wrapping_mul(XXH32_PRIME3))
            .rotate_left(17)
            .wrapping_mul(XXH32_PRIME4);
        index += 4;
    }
    while index < data.len() {
        hash = hash
            .wrapping_add(u32::from(data[index]).wrapping_mul(XXH32_PRIME5))
            .rotate_left(11)
            .wrapping_mul(XXH32_PRIME1);
        index += 1;
    }
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(XXH32_PRIME2);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(XXH32_PRIME3);
    hash ^= hash >> 16;
    hash
}

/// The BLAKE2s IV (SHA-256's constants) and message schedule.
const BLAKE2S_IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];
const BLAKE2S_SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];
/// The parameter block the reference initialises its cipher hash with:
/// 32-byte digest, then `4, 1, 1` and zeros (`FUN_10050bc0`; the digest
/// hasher's own block starts `8` at `FUN_10048910`).
const BLAKE2S_CIPHER_PARAM: [u8; 32] = [
    0x20, 4, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0,
];

fn blake2s_mixing(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, x: u32, y: u32) {
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(x);
    state[d] = (state[d] ^ state[a]).rotate_right(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(12);
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(y);
    state[d] = (state[d] ^ state[a]).rotate_right(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(7);
}

fn blake2s_compress(state: &mut [u32; 8], block: &[u8], counter: u64, last: bool) {
    let mut message = [0u32; 16];
    for (index, word) in message.iter_mut().enumerate() {
        *word = u32::from_le_bytes([
            block[index * 4],
            block[index * 4 + 1],
            block[index * 4 + 2],
            block[index * 4 + 3],
        ]);
    }
    let mut working = [0u32; 16];
    working[..8].copy_from_slice(state);
    working[8..].copy_from_slice(&BLAKE2S_IV);
    working[12] ^= counter as u32;
    working[13] ^= (counter >> 32) as u32;
    if last {
        working[14] = !working[14];
    }
    for round in BLAKE2S_SIGMA {
        blake2s_mixing(
            &mut working,
            0,
            4,
            8,
            12,
            message[round[0]],
            message[round[1]],
        );
        blake2s_mixing(
            &mut working,
            1,
            5,
            9,
            13,
            message[round[2]],
            message[round[3]],
        );
        blake2s_mixing(
            &mut working,
            2,
            6,
            10,
            14,
            message[round[4]],
            message[round[5]],
        );
        blake2s_mixing(
            &mut working,
            3,
            7,
            11,
            15,
            message[round[6]],
            message[round[7]],
        );
        blake2s_mixing(
            &mut working,
            0,
            5,
            10,
            15,
            message[round[8]],
            message[round[9]],
        );
        blake2s_mixing(
            &mut working,
            1,
            6,
            11,
            12,
            message[round[10]],
            message[round[11]],
        );
        blake2s_mixing(
            &mut working,
            2,
            7,
            8,
            13,
            message[round[12]],
            message[round[13]],
        );
        blake2s_mixing(
            &mut working,
            3,
            4,
            9,
            14,
            message[round[14]],
            message[round[15]],
        );
    }
    for (index, value) in state.iter_mut().enumerate() {
        *value ^= working[index] ^ working[index + 8];
    }
}

/// BLAKE2s with an explicit parameter block (`FUN_10042c00` XORs the block
/// into the IV, which is the BLAKE2s initialisation).
fn blake2s(parts: &[&[u8]], param: &[u8; 32]) -> [u8; 32] {
    let mut state = BLAKE2S_IV;
    for (word, bytes) in state.iter_mut().zip(param.chunks_exact(4)) {
        *word ^= u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    let mut data = Vec::new();
    for part in parts {
        data.extend_from_slice(part);
    }
    let mut counter = 0u64;
    let mut offset = 0usize;
    // Every full block *except the last* is compressed as a non-final block;
    // an input whose length is a multiple of 64 (including zero) ends with
    // its final block already full — the reference's update/final pair never
    // appends a synthetic empty block.
    while offset + 64 < data.len() {
        let block = &data[offset..offset + 64];
        counter += 64;
        blake2s_compress(&mut state, block, counter, false);
        offset += 64;
    }
    let mut last = [0u8; 64];
    let tail = &data[offset..];
    last[..tail.len()].copy_from_slice(tail);
    counter += tail.len() as u64;
    blake2s_compress(&mut state, &last, counter, true);
    let mut digest = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    digest
}

/// The ChaCha filter's key hash (`FUN_10050bc0`): BLAKE2s-256 with the
/// cipher's parameter block over `[seed u32 LE][60 zero bytes][iv]`.
fn blake2s256(parts: &[&[u8]]) -> [u8; 32] {
    blake2s(parts, &BLAKE2S_CIPHER_PARAM)
}

/// The ChaCha parameter sets (`FUN_10051ec0`'s six cases): rounds (8/12/20,
/// i.e. ChaCha8/12/20) and the keystream batch in 64-byte blocks, whose
/// blocks 2..n are xorshift-stretched from the first instead of counter-stepped.
fn chacha_parameters(cryptmode: u16) -> Option<(usize, usize)> {
    match cryptmode {
        1 => Some((8, 16)),
        2 => Some((12, 8)),
        3 => Some((20, 4)),
        4 => Some((8, 1)),
        5 => Some((12, 1)),
        6 => Some((20, 1)),
        _ => None,
    }
}

fn chacha_quarter(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(12);
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(7);
}

fn chacha_block(input: &[u32; 16], rounds: usize) -> [u32; 16] {
    let mut working = *input;
    for _ in 0..rounds / 2 {
        chacha_quarter(&mut working, 0, 4, 8, 12);
        chacha_quarter(&mut working, 1, 5, 9, 13);
        chacha_quarter(&mut working, 2, 6, 10, 14);
        chacha_quarter(&mut working, 3, 7, 11, 15);
        chacha_quarter(&mut working, 0, 5, 10, 15);
        chacha_quarter(&mut working, 1, 6, 11, 12);
        chacha_quarter(&mut working, 2, 7, 8, 13);
        chacha_quarter(&mut working, 3, 4, 9, 14);
    }
    for index in 0..16 {
        working[index] = working[index].wrapping_add(input[index]);
    }
    working
}

/// The reference ChaCha stream (`BasicCryptFilter`): the state is
/// `sigma || BLAKE2s key || 64-bit counter || XXH32(iv, seed) || seed`, the
/// key is `BLAKE2s(seed LE || 60 zero bytes || iv)` with the cipher's
/// parameter block, and a batch of `blocks` blocks takes its first block from
/// the core (counter incremented once per batch) while the rest are per-word
/// xorshift (`v ^= v << 13; v ^= v >> 17; v ^= v << 5`, zero replaced by the
/// seed^nonce fallback) of the block before them (`FUN_10049240`,
/// `FUN_10050dc0`, `FUN_100510d0`).
struct ChaCha {
    key: [u32; 8],
    nonce: u32,
    seed: u32,
    rounds: usize,
    blocks: usize,
    fallback: u32,
    counter: u64,
    keystream: Vec<u8>,
    offset: usize,
}

impl ChaCha {
    fn new(cryptmode: u16, seed: u32, iv: &[u8]) -> Result<Self> {
        let (rounds, blocks) = chacha_parameters(cryptmode).ok_or_else(|| {
            TjsError::runtime(format!("unsupported ChaCha cryptmode {cryptmode}"))
        })?;
        let digest = blake2s256(&[&seed.to_le_bytes(), &[0u8; 60], iv]);
        let mut key = [0u32; 8];
        for (index, word) in key.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                digest[index * 4],
                digest[index * 4 + 1],
                digest[index * 4 + 2],
                digest[index * 4 + 3],
            ]);
        }
        let nonce = xxh32(iv, seed);
        let mixed = seed ^ nonce;
        let fallback = if mixed == 0 {
            if seed != 0 { seed } else { u32::MAX }
        } else {
            mixed
        };
        Ok(Self {
            key,
            nonce,
            seed,
            rounds,
            blocks,
            fallback,
            counter: 0,
            keystream: Vec::new(),
            offset: 0,
        })
    }

    fn refill(&mut self) {
        let counter = self.counter;
        self.counter += 1;
        let mut state = [0u32; 16];
        state[..4].copy_from_slice(&[0x6170_7865, 0x3320_646E, 0x7962_2D32, 0x6B20_6574]);
        state[4..12].copy_from_slice(&self.key);
        state[12] = counter as u32;
        state[13] = (counter >> 32) as u32;
        state[14] = self.nonce;
        state[15] = self.seed;
        let mut block = chacha_block(&state, self.rounds);
        self.keystream.clear();
        self.keystream.reserve(self.blocks * 64);
        for index in 0..self.blocks {
            if index > 0 {
                for word in block.iter_mut() {
                    let mut value = *word ^ (*word << 13);
                    value ^= value >> 17;
                    value ^= value << 5;
                    *word = if value == 0 { self.fallback } else { value };
                }
            }
            for word in block {
                self.keystream.extend_from_slice(&word.to_le_bytes());
            }
        }
        self.offset = 0;
    }

    fn apply(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            if self.offset == self.keystream.len() {
                self.refill();
            }
            *byte ^= self.keystream[self.offset];
            self.offset += 1;
        }
    }
}

/// Encrypts (or decrypts — the keystream is XOR) `data` in place with the
/// parameter set of `cryptmode`.
fn datapack_chacha_apply(cryptmode: u16, seed: u32, iv: &[u8], data: &mut [u8]) -> Result<()> {
    ChaCha::new(cryptmode, seed, iv)?.apply(data);
    Ok(())
}

/// `LZ4CompressStream` (`FUN_1004f800`): every 4096-byte chunk of the body
/// becomes `[u16 LE compressed length][raw LZ4 block]`. The framing is what
/// `4s0` names, and the reference decompresses each frame into one block
/// buffer, so frames must not exceed that size.
fn datapack_lz4_frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + body.len() / 255 + 16);
    for chunk in body.chunks(DATAPACK_LZ4_BLOCK) {
        let compressed = lz4_flex::block::compress(chunk);
        out.extend_from_slice(&(compressed.len() as u16).to_le_bytes());
        out.extend_from_slice(&compressed);
    }
    out
}

/// `LZ4DecompressStream` (`FUN_1004f700`): the inverse of the framing above.
fn datapack_lz4_deframe(framed: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < framed.len() {
        if index + 2 > framed.len() {
            return Err(TjsError::runtime("LZ4 frame length is truncated"));
        }
        let length = usize::from(u16::from_le_bytes([framed[index], framed[index + 1]]));
        index += 2;
        let limit = DATAPACK_LZ4_BLOCK + DATAPACK_LZ4_BLOCK / 255 + 16;
        if length == 0 || length > limit {
            return Err(TjsError::runtime("LZ4 frame has an implausible length"));
        }
        let block = framed
            .get(index..index + length)
            .ok_or_else(|| TjsError::runtime("LZ4 frame is truncated"))?;
        index += length;
        let start = out.len();
        out.resize(start + DATAPACK_LZ4_BLOCK, 0);
        let written =
            lz4_flex::block::decompress_into(block, &mut out[start..]).map_err(|error| {
                TjsError::runtime(format!("LZ4 frame does not decompress: {error}"))
            })?;
        out.truncate(start + written);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The leading image: chunk walkers (what the reference sniffer does) and the
// thumbnail encoders

/// Where the leading image ends and the container header starts. The
/// reference reads the file back through `JPGChunkStreamReader` /
/// `PNGChunkStreamReader`, which stop exactly at the image's own end marker
/// (`FUN_10050020` sniffs `TJS/`, `TJS\`, `\x89PNG` and `\xFF\xD8\xFF\xE0`).
fn image_end(bytes: &[u8]) -> Option<usize> {
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return jpeg_end(bytes);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return png_end(bytes);
    }
    None
}

/// Walks JPEG markers to the EOI that ends the image. Entropy-coded data is
/// skipped with its stuffing (`FF 00`) and restart markers respected.
fn jpeg_end(bytes: &[u8]) -> Option<usize> {
    let mut index = 2;
    loop {
        if index + 2 > bytes.len() || bytes[index] != 0xFF {
            return None;
        }
        let mut marker = bytes[index + 1];
        while marker == 0xFF {
            index += 1;
            if index + 1 >= bytes.len() {
                return None;
            }
            marker = bytes[index + 1];
        }
        match marker {
            0x00 => return None,
            0xD8 => index += 2,
            0x01 | 0xD0..=0xD7 => index += 2,
            0xD9 => return Some(index + 2),
            0xDA => {
                index += 2;
                if index + 2 > bytes.len() {
                    return None;
                }
                let length = usize::from(u16::from_be_bytes([bytes[index], bytes[index + 1]]));
                index += length;
                loop {
                    if index + 1 >= bytes.len() {
                        return None;
                    }
                    if bytes[index] == 0xFF && bytes[index + 1] != 0x00 {
                        if (0xD0..=0xD7).contains(&bytes[index + 1]) {
                            index += 2;
                            continue;
                        }
                        break;
                    }
                    index += 1;
                }
            }
            _ => {
                if index + 4 > bytes.len() {
                    return None;
                }
                let length = usize::from(u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]));
                if length < 2 {
                    return None;
                }
                index += 2 + length;
            }
        }
    }
}

/// Walks the PNG chunk list to IEND.
fn png_end(bytes: &[u8]) -> Option<usize> {
    let mut index = 8;
    loop {
        let length_bytes = bytes.get(index..index + 4)?;
        let length = u32::from_be_bytes([
            length_bytes[0],
            length_bytes[1],
            length_bytes[2],
            length_bytes[3],
        ]) as usize;
        let kind = bytes.get(index + 4..index + 8)?;
        index = index.checked_add(12)?.checked_add(length)?;
        if index > bytes.len() {
            return None;
        }
        if kind == b"IEND" {
            return Some(index);
        }
    }
}

/// The standard Annex K quantisation tables, natural (row-major) order.
const JPEG_LUMA_QUANT: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const JPEG_CHROMA_QUANT: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];
/// Natural-order index of each zigzag position.
const JPEG_ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
const JPEG_DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const JPEG_DC_LUMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const JPEG_DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const JPEG_DC_CHROMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const JPEG_AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const JPEG_AC_LUMA_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
const JPEG_AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const JPEG_AC_CHROMA_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

struct JpegHuffman {
    codes: [u16; 256],
    sizes: [u8; 256],
}

impl JpegHuffman {
    fn new(bits: &[u8; 16], values: &[u8]) -> Self {
        let mut table = Self {
            codes: [0; 256],
            sizes: [0; 256],
        };
        let mut code = 0u16;
        let mut index = 0;
        for (length, count) in bits.iter().enumerate() {
            for _ in 0..*count {
                let symbol = values[index] as usize;
                table.codes[symbol] = code;
                table.sizes[symbol] = length as u8 + 1;
                code += 1;
                index += 1;
            }
            code <<= 1;
        }
        table
    }

    fn emit(&self, symbol: u8, writer: &mut JpegBits) {
        writer.write(self.codes[symbol as usize], self.sizes[symbol as usize]);
    }
}

struct JpegBits {
    out: Vec<u8>,
    buffer: u32,
    bits: u32,
}

impl JpegBits {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            buffer: 0,
            bits: 0,
        }
    }

    fn write(&mut self, value: u16, length: u8) {
        self.buffer = (self.buffer << length) | u32::from(value);
        self.bits += u32::from(length);
        while self.bits >= 8 {
            self.bits -= 8;
            let byte = ((self.buffer >> self.bits) & 0xFF) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0x00);
            }
        }
    }

    fn flush(&mut self) {
        if self.bits > 0 {
            let pad = 8 - self.bits;
            self.write(((1u32 << pad) - 1) as u16, pad as u8);
        }
    }
}

/// The 8x8 type-II DCT, normalised as `JPEG` expects: `1/4 C(u)C(v)`.
fn jpeg_fdct(samples: &[f32; 64]) -> [f32; 64] {
    let mut table = [[0f32; 8]; 8];
    for (value, row) in table.iter_mut().enumerate() {
        for (frequency, cell) in row.iter_mut().enumerate() {
            *cell =
                (((2 * value + 1) as f32) * (frequency as f32) * std::f32::consts::PI / 16.0).cos();
        }
    }
    let mut rows = [0f32; 64];
    for row in 0..8 {
        for frequency in 0..8 {
            let mut sum = 0.0;
            for value in 0..8 {
                sum += samples[row * 8 + value] * table[value][frequency];
            }
            rows[row * 8 + frequency] = sum;
        }
    }
    let mut out = [0f32; 64];
    for column in 0..8 {
        for frequency in 0..8 {
            let mut sum = 0.0;
            for value in 0..8 {
                sum += rows[value * 8 + column] * table[value][frequency];
            }
            let component_u = if frequency == 0 {
                std::f32::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            let component_v = if column == 0 {
                std::f32::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            out[frequency * 8 + column] = 0.25 * component_u * component_v * sum;
        }
    }
    out
}

fn jpeg_category(value: i32) -> u8 {
    if value == 0 {
        return 0;
    }
    (32 - value.unsigned_abs().leading_zeros()) as u8
}

fn jpeg_bits(value: i32, size: u8) -> u16 {
    if value >= 0 {
        value as u16
    } else {
        (value - 1 + (1i32 << size)) as u16
    }
}

struct JpegComponent {
    quant: [f32; 64],
    dc: JpegHuffman,
    ac: JpegHuffman,
    previous_dc: i32,
}

impl JpegComponent {
    fn encode(&mut self, samples: &[f32; 64], writer: &mut JpegBits) {
        let mut shifted = [0f32; 64];
        for (index, value) in samples.iter().enumerate() {
            shifted[index] = value - 128.0;
        }
        let transformed = jpeg_fdct(&shifted);
        let mut block = [0i32; 64];
        for zigzag in 0..64 {
            let natural = JPEG_ZIGZAG[zigzag];
            block[zigzag] = (transformed[natural] / self.quant[natural]).round() as i32;
        }
        let difference = block[0] - self.previous_dc;
        self.previous_dc = block[0];
        let size = jpeg_category(difference);
        self.dc.emit(size, writer);
        writer.write(jpeg_bits(difference, size), size);

        let mut run = 0;
        for &value in &block[1..] {
            if value == 0 {
                run += 1;
                continue;
            }
            while run >= 16 {
                self.ac.emit(0xF0, writer);
                run -= 16;
            }
            let size = jpeg_category(value);
            self.ac.emit(((run << 4) | i32::from(size)) as u8, writer);
            writer.write(jpeg_bits(value, size), size);
            run = 0;
        }
        if run > 0 {
            self.ac.emit(0x00, writer);
        }
    }
}

/// A baseline JPEG of an RGBA buffer: SOI, APP0/JFIF, the standard Annex K
/// tables, 4:4:4 sampling and one interleaved scan. The reference's own
/// encoder is IJG (`makeDataPackThumb`), so the bytes differ while the image
/// stays a decodable baseline JPEG.
fn encode_jpeg(rgba: &[u8], width: usize, height: usize, quality: u8) -> Vec<u8> {
    let quality = u32::from(quality.clamp(1, 100));
    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - quality * 2
    };
    let scaled = |base: &[u8; 64]| {
        let mut table = [0u8; 64];
        for (index, value) in base.iter().enumerate() {
            table[index] = ((u32::from(*value) * scale + 50) / 100).clamp(1, 255) as u8;
        }
        table
    };
    let luma_table = scaled(&JPEG_LUMA_QUANT);
    let chroma_table = scaled(&JPEG_CHROMA_QUANT);
    let as_quantiser = |table: &[u8; 64]| {
        let mut quantiser = [0f32; 64];
        for (index, value) in table.iter().enumerate() {
            quantiser[index] = f32::from(*value);
        }
        quantiser
    };
    let luma = as_quantiser(&luma_table);
    let chroma = as_quantiser(&chroma_table);

    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, 0xD8]);
    out.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
    out.extend_from_slice(b"JFIF\0");
    out.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
    for (id, table) in [(0x00u8, &luma_table), (0x01, &chroma_table)] {
        out.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43]);
        out.push(id);
        for zigzag in 0..64 {
            out.push(table[JPEG_ZIGZAG[zigzag]]);
        }
    }
    out.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
    out.extend_from_slice(&(height as u16).to_be_bytes());
    out.extend_from_slice(&(width as u16).to_be_bytes());
    out.push(3);
    for (id, quant) in [(1u8, 0u8), (2, 1), (3, 1)] {
        out.extend_from_slice(&[id, 0x11, quant]);
    }
    let tables = [
        (&JPEG_DC_LUMA_BITS[..], &JPEG_DC_LUMA_VALUES[..]),
        (&JPEG_AC_LUMA_BITS[..], &JPEG_AC_LUMA_VALUES[..]),
        (&JPEG_DC_CHROMA_BITS[..], &JPEG_DC_CHROMA_VALUES[..]),
        (&JPEG_AC_CHROMA_BITS[..], &JPEG_AC_CHROMA_VALUES[..]),
    ];
    for (id, (bits, values)) in [0x00u8, 0x10, 0x01, 0x11].into_iter().zip(tables) {
        out.extend_from_slice(&[0xFF, 0xC4]);
        out.extend_from_slice(&((19 + values.len()) as u16).to_be_bytes());
        out.push(id);
        out.extend_from_slice(bits);
        out.extend_from_slice(values);
    }
    out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x0C, 0x03]);
    for id in 1u8..=3 {
        out.extend_from_slice(&[id, if id == 1 { 0x00 } else { 0x11 }]);
    }
    out.extend_from_slice(&[0x00, 0x3F, 0x00]);

    let mut luma_component = JpegComponent {
        quant: luma,
        dc: JpegHuffman::new(&JPEG_DC_LUMA_BITS, &JPEG_DC_LUMA_VALUES),
        ac: JpegHuffman::new(&JPEG_AC_LUMA_BITS, &JPEG_AC_LUMA_VALUES),
        previous_dc: 0,
    };
    // Cb and Cr are the same quantisation/Huffman configuration but each
    // keeps its own DC predictor (they are separate components on the wire).
    let mut cb_component = JpegComponent {
        quant: chroma,
        dc: JpegHuffman::new(&JPEG_DC_CHROMA_BITS, &JPEG_DC_CHROMA_VALUES),
        ac: JpegHuffman::new(&JPEG_AC_CHROMA_BITS, &JPEG_AC_CHROMA_VALUES),
        previous_dc: 0,
    };
    let mut cr_component = JpegComponent {
        quant: chroma,
        dc: JpegHuffman::new(&JPEG_DC_CHROMA_BITS, &JPEG_DC_CHROMA_VALUES),
        ac: JpegHuffman::new(&JPEG_AC_CHROMA_BITS, &JPEG_AC_CHROMA_VALUES),
        previous_dc: 0,
    };

    let mut writer = JpegBits::new();
    for block_y in (0..height).step_by(8) {
        for block_x in (0..width).step_by(8) {
            let mut samples = [[0f32; 64]; 3];
            for y in 0..8 {
                for x in 0..8 {
                    let px = (block_x + x).min(width.saturating_sub(1));
                    let py = (block_y + y).min(height.saturating_sub(1));
                    let offset = (py * width + px) * 4;
                    let r = f32::from(rgba[offset]);
                    let g = f32::from(rgba[offset + 1]);
                    let b = f32::from(rgba[offset + 2]);
                    samples[0][y * 8 + x] = 0.299 * r + 0.587 * g + 0.114 * b;
                    samples[1][y * 8 + x] = -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0;
                    samples[2][y * 8 + x] = 0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0;
                }
            }
            luma_component.encode(&samples[0], &mut writer);
            cb_component.encode(&samples[1], &mut writer);
            cr_component.encode(&samples[2], &mut writer);
        }
    }
    writer.flush();
    out.extend_from_slice(&writer.out);
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut low = 1u32;
    let mut high = 0u32;
    for &byte in bytes {
        low = (low + u32::from(byte)) % 65521;
        high = (high + low) % 65521;
    }
    (high << 16) | low
}

/// A minimal PNG: 8-bit RGBA, one IDAT of stored deflate blocks (a thumbnail
/// is a handful of kilobytes; compression is not the point) and the format's
/// CRC-32/Adler-32.
fn encode_png(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut raw = Vec::with_capacity(height * (1 + width * 4));
    for y in 0..height {
        raw.push(0);
        let start = (y * width * 4).min(rgba.len());
        let end = (start + width * 4).min(rgba.len());
        raw.extend_from_slice(&rgba[start..end]);
        raw.resize(y * (1 + width * 4) + 1 + width * 4, 0);
    }
    let mut zlib = Vec::with_capacity(raw.len() + raw.len() / 65_535 * 5 + 6);
    zlib.extend_from_slice(&[0x78, 0x01]);
    let mut start = 0;
    loop {
        let take = (raw.len() - start).min(0xFFFF);
        let last = start + take == raw.len();
        zlib.push(if last { 1 } else { 0 });
        zlib.extend_from_slice(&(take as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(take as u16)).to_le_bytes());
        zlib.extend_from_slice(&raw[start..start + take]);
        start += take;
        if last {
            break;
        }
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    out
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

/// The `Scripts` members only the bundle carries.
///
/// Everything shared with `scriptsEx.dll` — `getObjectKeys`,
/// `getObjectCount`, `getObjectContext`, `isNullContext`, `equalStruct`,
/// `equalStructNumericLoose`, `foreach`, `getMD5HashString`, `clone`,
/// `rehash` — is installed by [`crate::scripts_ex::install_scripts_ex`], the
/// one implementation both DLLs register. `encodeTBPS`/`decodeTBPS` and
/// `safeEvalStorage` are this bundle's own surface.
fn install_scripts_ex_extras(runtime: &mut Runtime<KrkrHost>) {
    let scripts = ensure_global_object(runtime, "Scripts");
    runtime.register_object_native(scripts, "encodeTBPS", first_arg_string);
    runtime.register_object_native(scripts, "decodeTBPS", first_arg_string);
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

    use super::*;

    /// A real-engine container without any transform (`m144-plain.ksd`),
    /// written by krkrz 1.4.0.8 + PackinOne.dll under wine for the probe data
    /// `%["id" => "probe", "core" => %["storeTime" => 1, "name" => "あ"],
    /// "list" => [1, 2, <%01 02%>]]` with `seed = 882327524`, `iv = "kiri"`.
    const REAL_ENGINE_PLAIN: &[u8] = &[
        0x54, 0x4a, 0x53, 0x2f, 0x6e, 0x73, 0x30, 0x00, 0xe4, 0x3f, 0x97, 0x34, 0x00, 0x00, 0x0a,
        0x00, 0x6b, 0x00, 0x69, 0x00, 0x72, 0x00, 0x69, 0x00, 0x00, 0x00, 0xc1, 0xf6, 0x03, 0x00,
        0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x63, 0x00, 0x6f, 0x00, 0x72, 0x00, 0x65, 0x00, 0xc1,
        0xab, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x6e, 0x00, 0x61, 0x00, 0x6d, 0x00,
        0x65, 0x00, 0x02, 0x02, 0x01, 0x00, 0x00, 0x00, 0x42, 0x30, 0x09, 0x00, 0x00, 0x00, 0x73,
        0x00, 0x74, 0x00, 0x6f, 0x00, 0x72, 0x00, 0x65, 0x00, 0x54, 0x00, 0x69, 0x00, 0x6d, 0x00,
        0x65, 0x00, 0x04, 0x18, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
        0x00, 0x69, 0x00, 0x64, 0x00, 0x02, 0xe1, 0x05, 0x00, 0x00, 0x00, 0x70, 0x00, 0x72, 0x00,
        0x6f, 0x00, 0x62, 0x00, 0x65, 0x00, 0x04, 0x00, 0x00, 0x00, 0x6c, 0x00, 0x69, 0x00, 0x73,
        0x00, 0x74, 0x00, 0x81, 0xfb, 0x03, 0x00, 0x00, 0x00, 0x04, 0xcd, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x04, 0xf6, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03,
        0xe5, 0x02, 0x00, 0x00, 0x00, 0x01, 0x02, 0x99, 0xa3, 0xac, 0x00,
    ];

    /// The same harness data with both transforms (`m144-4s0-chacha.ksd`):
    /// `4s0` LZ4 framing under cryptmode 1's ChaCha keystream.
    const REAL_ENGINE_4S0_CHACHA: &[u8] = &[
        0x54, 0x4a, 0x53, 0x2f, 0x34, 0x73, 0x30, 0x00, 0xe4, 0x3f, 0x97, 0x34, 0x01, 0x00, 0x0a,
        0x00, 0x6b, 0x00, 0x69, 0x00, 0x72, 0x00, 0x69, 0x00, 0x00, 0x00, 0x5f, 0x74, 0x5f, 0xdb,
        0x37, 0xc6, 0xd9, 0xad, 0x2c, 0x47, 0x0c, 0x72, 0x0e, 0x49, 0xfe, 0xee, 0xa5, 0x7f, 0x0e,
        0x23, 0xd0, 0xd6, 0x6a, 0x03, 0x16, 0xe0, 0x72, 0x4e, 0x5f, 0xfe, 0x5b, 0x29, 0xc7, 0x56,
        0x4b, 0x25, 0x30, 0xcf, 0xbb, 0xc4, 0x21, 0xd6, 0x88, 0x3b, 0x72, 0x76, 0x72, 0x14, 0x2e,
        0x55, 0x30, 0xbb, 0x0a, 0x7d, 0xd3, 0xcf, 0x8a, 0xcc, 0xa2, 0x5a, 0x33, 0x81, 0xa2, 0x70,
        0xc4, 0x15, 0xc2, 0x35, 0x94, 0x1f, 0x54, 0x62, 0x1e, 0xd8, 0x52, 0x43, 0xd0, 0xf2, 0xc8,
        0xb5, 0x88, 0x58, 0x28, 0x4c, 0xeb, 0x9c, 0xde, 0xf2, 0x0c, 0x10, 0x30, 0xe0, 0xc1, 0xc2,
        0x4d, 0x1c, 0x9e, 0xaa, 0x76, 0xc1, 0xfe, 0x55, 0x12, 0x0f, 0x67, 0x23, 0x19, 0x3e, 0x6f,
        0x63, 0xdc, 0xde, 0x62, 0xf9, 0x45, 0xb7, 0x21, 0x77, 0x8f, 0xb9, 0x0e, 0xd3, 0xbd, 0x0f,
        0xfe, 0xbb, 0x9e, 0x5e, 0xd4, 0x41,
    ];

    /// `Scripts.saveDataPack` writes the reference container: the JPEG
    /// thumbnail the save screen loads leads the file, then the 16-byte
    /// `TJS/4s0` header with the digest's seed/cryptmode/iv, then the LZ4-
    /// framed and ChaCha-encrypted body + check — and `loadDataPack` reads it
    /// back under the exact storage name the game hands over (`data0.jpg`,
    /// not its `.pbd` alias).
    #[test]
    fn save_data_pack_writes_the_reference_container() {
        let root = test_root("packinone-datapack-save");
        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var data = %[id => "save-id", core => %[storeTime => 1234],
                                 user => %[], history => %[]];
                    var digest = %[width => 4, height => 2, ext => "jpg",
                                  compress => 1, cryptmode => 1, iv => "title"];
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
        // A JPEG leads the file (`saveThumbnail = 3` picks `jpg`).
        assert_eq!(&bytes[..2], [0xFF, 0xD8]);
        let image_end = image_end(&bytes).expect("jpeg thumbnail end");
        let pack = &bytes[image_end..];
        let header = ns0::parse_ns0_header(pack).expect("container header");
        assert_eq!(
            header.compress,
            ns0::NS0_COMPRESS_LZ4,
            "digest.compress selects the 4s0 framing"
        );
        assert_eq!(header.cryptmode, 1);
        // The IV is the salt string as UTF-16LE plus its NUL: `2*(5+1)`.
        assert_eq!(header.iv_length, 12);
        assert_eq!(header.seed, seed, "the digest seed lands in the header");
        let iv: Vec<u8> = "title"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .chain([0, 0])
            .collect();
        let iv_end = ns0::NS0_HEADER_SIZE + iv.len();
        assert_eq!(&pack[ns0::NS0_HEADER_SIZE..iv_end], iv.as_slice());
        // The body decrypts and deframes back into the seeded value stream.
        let mut body = pack[iv_end..].to_vec();
        datapack_chacha_apply(1, seed, &iv, &mut body).expect("decrypt");
        let body = datapack_lz4_deframe(&body).expect("deframe");
        assert!(body.len() > 4, "the body carries the trailing check");

        // The save screen loads the slot's picture from the file itself
        // (`drawNormalItem` → `DataStore.getFileName` → `loadImages`), so the
        // leading JPEG has to decode with the container trailing it.
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
        let pixel = read_integer(&mut engine, "slotView.getMainPixel(0, 0)");
        let (red, green, blue) = ((pixel >> 16) & 0xFF, (pixel >> 8) & 0xFF, pixel & 0xFF);
        assert!(
            (red - 0x11).abs() <= 24 && (green - 0x22).abs() <= 24 && (blue - 0x33).abs() <= 24,
            "the thumbnail's pixel stays near 0x112233 through JPEG: {pixel:#08x}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Without a digest the writer emits the plain `TJS/ns0` container whose
    /// body is exactly the seeded value stream — the shape the 124 shipped
    /// `.pbd` files already prove.
    #[test]
    fn save_data_pack_without_a_digest_writes_a_plain_ns0_container() {
        let root = test_root("packinone-datapack-plain");
        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        engine
            .execute_script(
                "inline.tjs",
                r#"(function() {
                    var data = %[id => "x", core => %[storeTime => 5, name => "あ"],
                                 list => [1, 2, <% 01 02 %>], flag => null, neg => -2];
                    Scripts.saveDataPack("mine.pbd", data);
                })();"#,
            )
            .expect("write the pack");
        let bytes = fs::read(root.join("mine.pbd")).expect("plugin pack");
        let header = ns0::parse_ns0_header(&bytes).expect("header");
        assert_eq!(header.compress, ns0::NS0_COMPRESS_STORE);
        assert_eq!(header.cryptmode, 0);
        assert_eq!(header.iv_length, 0);
        assert_eq!(
            header.seed,
            ns0::NS0_DEFAULT_SEED,
            "the writer's default seed"
        );
        assert!(
            !bytes.ends_with(DATA_PACK_FOOTER_MAGIC),
            "the reference container has no footer"
        );
        // The bytes after the header are the seeded body: re-decoding them
        // with the header's seed reproduces the data.
        let value = engine
            .execute_expression("inline.tjs", "Scripts.loadDataPack(\"mine.pbd\").core.name")
            .expect("load the pack");
        assert_eq!(value, Variant::String("あ".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The digest is deterministic, mixes its arguments, and hashes the
    /// big-endian value serialization: for the key `"abc"` the preimage is
    /// the string tag, its u32 big-endian length and the UTF-16LE units, with
    /// the zero seed's zero check bytes.
    #[test]
    fn data_pack_digest_hashes_the_big_endian_serialization() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var first = Scripts.makeDataPackDigest(void, 0, "abc");
                    var second = Scripts.makeDataPackDigest(void, 0, "abc");
                    var other = Scripts.makeDataPackDigest(void, 2, "abc");
                    return first + ":" + second + ":" + other;
                })()"#,
            )
            .expect("digest probes");
        let Variant::String(text) = &value else {
            panic!("unexpected digest value {value:?}");
        };
        let numbers: Vec<u32> = text
            .split(':')
            .map(|part| part.parse().expect("digest number"))
            .collect();
        assert_eq!(numbers.len(), 3, "{text}");
        assert_eq!(numbers[0], numbers[1]);
        assert_ne!(numbers[0], numbers[2]);
        let mut preimage = vec![0x00, 0x00]; // the void subject's tag + zero check
        preimage.extend_from_slice(&[0x00, 0x02]); // zero check byte, string tag
        preimage.extend_from_slice(&3_u32.to_be_bytes());
        for unit in "abc".encode_utf16() {
            preimage.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(numbers[0], xxh32(&preimage, 0));
    }

    /// `makeDataPackThumb` answers an `Octet` image for a drawable layer —
    /// JPEG for the `jpg`/default spelling, PNG for `png` — and nothing for
    /// `kdt`.
    #[test]
    fn data_pack_thumb_follows_its_contracts() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    global.layer = new Layer();
                    layer.setImageSize(2, 1);
                    layer.fillRect(0, 0, 2, 1, 0xff102030);
                    var jpeg = Scripts.makeDataPackThumb(layer, "jpg");
                    var png = Scripts.makeDataPackThumb(layer, "png");
                    var kdt = Scripts.makeDataPackThumb(layer, "kdt");
                    global.jpegHead = jpeg[0] + ":" + jpeg[1];
                    global.pngHead = png[0] + ":" + png[1];
                    return (jpeg != void) + ":" + (png != void) + ":" +
                        typeof kdt + ":" + jpegHead + ":" + pngHead;
                })()"#,
            )
            .expect("thumbnail probes");
        assert_eq!(
            value,
            Variant::String("1:1:void:255:216:137:80".to_string()),
            "JPEG starts FFD8, PNG starts 89 50"
        );
    }

    /// XXH32 against the official `libxxhash` (0.8.3) for the classic sanity
    /// strings and for two multi-block inputs, including a seeded one.
    #[test]
    fn xxh32_matches_the_published_vectors() {
        let vectors: &[(&str, u32)] = &[
            ("", 0x02CC_5D05),
            ("a", 0x550D_7456),
            ("abc", 0x32D1_53FF),
            ("message digest", 0x7C94_8494),
            ("abcdefghijklmnopqrstuvwxyz", 0x63A1_4D5F),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                0x9C28_5E64,
            ),
        ];
        for (input, expected) in vectors {
            assert_eq!(xxh32(input.as_bytes(), 0), *expected, "{input:?}");
        }
        let long: Vec<u8> = (0..256u16)
            .map(|byte| byte as u8)
            .cycle()
            .take(768)
            .collect();
        assert_eq!(xxh32(&long, 0), 0xCDB9_46B1);
        assert_eq!(xxh32(&long, 42), 0x9E5B_105A);
    }

    /// BLAKE2s through the unkeyed parameter block (the published vectors) and
    /// through the cipher's own block, whose key bytes a real-engine artifact
    /// pins: the DLL's key hash is BLAKE2s, not SHA-256 (it carries no SHA-256
    /// round constants, and `FUN_10042d70` is BLAKE2s's compression).
    #[test]
    fn blake2s_matches_the_published_and_real_engine_vectors() {
        let hex = |digest: [u8; 32]| {
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let mut unkeyed = [0u8; 32];
        unkeyed[0..4].copy_from_slice(&[0x20, 0, 1, 1]);
        assert_eq!(
            hex(blake2s(&[b""], &unkeyed)),
            "69217a3079908094e11121d042354a7c1f55b6482ca1a51e1b250dfd1ed0eef9"
        );
        assert_eq!(
            hex(blake2s(&[b"abc"], &unkeyed)),
            "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982"
        );
        let mut iv = Vec::new();
        for unit in "kiri".encode_utf16() {
            iv.extend_from_slice(&unit.to_le_bytes());
        }
        iv.extend_from_slice(&[0, 0]);
        assert_eq!(
            hex(blake2s256(&[
                &0x3497_3FE4_u32.to_le_bytes(),
                &[0u8; 60],
                &iv
            ])),
            "67fd820ca5fc08d07885343b87e622cc553e9a2b27ace65d3579605dc5657ea9"
        );

        // Lengths around the block boundary: a 64-multiple input must end with
        // its full final block, not a synthetic empty one. The key preimage is
        // `64 + 2*(title_chars+1)` bytes, so a 31-mod-32-character title lands
        // exactly on a multiple of 64.
        let vectors: &[(usize, &str)] = &[
            (
                55,
                "f4495470f226c8c214be08fdfad4bc4a2a9dbea9136a210df0d4b64929e6fc14",
            ),
            (
                56,
                "e290dd270b467f34ab1c002d340fa016257ff19e5833fdbbf2cb401c3b2817de",
            ),
            (
                63,
                "e57cb79487dd57902432b250733813bd96a84efce59f650fac26e6696aefafc3",
            ),
            (
                64,
                "56f34e8b96557e90c1f24b52d0c89d51086acf1b00f634cf1dde9233b8eaaa3e",
            ),
            (
                65,
                "1b53ee94aaf34e4b159d48de352c7f0661d0a40edff95a0b1639b4090e974472",
            ),
            (
                127,
                "f18417b39d617ab1c18fdf91ebd0fc6d5516bb34cf39364037bce81fa04cecb1",
            ),
            (
                128,
                "1fa877de67259d19863a2a34bcc6962a2b25fcbf5cbecd7ede8f1fa36688a796",
            ),
            (
                129,
                "5bd169e67c82c2c2e98ef7008bdf261f2ddf30b1c00f9e7f275bb3e8a28dc9a2",
            ),
        ];
        let pattern: Vec<u8> = (0..200).map(|index| (index % 251) as u8).collect();
        for (length, expected) in vectors {
            assert_eq!(
                hex(blake2s(&[&pattern[..*length]], &unkeyed)),
                *expected,
                "length {length}"
            );
        }
    }

    /// The four containers M142's real-engine session wrote (`m144-*.ksd`,
    /// krkrz 1.4.0.8 + 少女世界/GINKA's PackinOne.dll) decode through
    /// `Scripts.loadDataPack` to the probe dictionary the harness passed —
    /// including the `4s0` + ChaCha variant, which pins the header, the IV
    /// bytes, the LZ4 framing, the whole keystream and the checker against the
    /// reference implementation.
    #[test]
    fn real_engine_containers_decode_through_load_data_pack() {
        for (label, bytes) in [
            ("plain", REAL_ENGINE_PLAIN),
            ("4s0+chacha", REAL_ENGINE_4S0_CHACHA),
        ] {
            let root = test_root(&format!("packinone-real-{label}"));
            fs::write(root.join("probe.ksd"), bytes).expect("write fixture");
            let mut engine = test_engine(&root);
            engine.register_plugin(PackinOnePlugin).expect("plugin");
            let value = engine
                .execute_expression(
                    "inline.tjs",
                    r#"(function() {
                        var d = Scripts.loadDataPack("probe.ksd");
                        return d.id + "|" + d.core.storeTime + "|" + d.core.name + "|" +
                            d.list.count + "|" + d.list[0] + "|" + d.list[1];
                    })()"#,
                )
                .expect("load the real-engine container");
            assert_eq!(
                value,
                Variant::String("probe|1|あ|3|1|2".to_string()),
                "{label}"
            );
            fs::remove_dir_all(root).expect("cleanup");
        }
    }

    /// The digest is deterministic and travels in the header. **Known
    /// divergence**: a real-engine probe printed `882327524` for this data
    /// (`m144/probe-output.txt`), while this port's XXH32-over-the-big-endian-
    /// serialization model gives `1715496921`. The reference feeds its digest
    /// state (`VariantDigestState`) through a collection order and checker path
    /// the decompilation does not fully pin (the value pointer is stored into
    /// the state before each operand is written), and nothing re-verifies the
    /// digest — `isValidBookMarkData` checks only `core`/`id`/`storeTime` — so
    /// the value is recorded here rather than chased. The header seed, the
    /// checker and the ChaCha key are driven by whatever value this function
    /// returns, so the port stays self-consistent.
    #[test]
    fn data_pack_digest_is_deterministic() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                r#"(function() {
                    var data = %["id" => "probe",
                                 "core" => %["storeTime" => 1, "name" => "あ"],
                                 "list" => [1, 2, <%01 02%>]];
                    var first = Scripts.makeDataPackDigest(data, 7, "probe");
                    var second = Scripts.makeDataPackDigest(data, 7, "probe");
                    return first + ":" + second;
                })()"#,
            )
            .expect("digest probe");
        let Variant::String(text) = &value else {
            panic!("unexpected digest value {value:?}");
        };
        assert_eq!(
            text, "1715496921:1715496921",
            "deterministic (reference: 882327524)"
        );
    }

    /// The ChaCha core is the standard one: RFC 8439 §2.3.2's block, built
    /// from the RFC's own key/nonce/counter through the same layout the
    /// reference cipher uses (`sigma || key || counter || nonce`).
    #[test]
    fn chacha_block_matches_rfc_8439() {
        let mut state = [0u32; 16];
        state[..4].copy_from_slice(&[0x6170_7865, 0x3320_646E, 0x7962_2D32, 0x6B20_6574]);
        for (index, word) in state[4..12].iter_mut().enumerate() {
            let base = (index * 4) as u8;
            *word = u32::from_le_bytes([base, base + 1, base + 2, base + 3]);
        }
        state[12] = 1;
        state[13] = 0x0900_0000;
        state[14] = 0x4A00_0000;
        state[15] = 0;
        let block = chacha_block(&state, 20);
        let mut bytes = Vec::new();
        for word in block {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        let expected: [u8; 64] = [
            0x10, 0xf1, 0xe7, 0xe4, 0xd1, 0x3b, 0x59, 0x15, 0x50, 0x0f, 0xdd, 0x1f, 0xa3, 0x20,
            0x71, 0xc4, 0xc7, 0xd1, 0xf4, 0xc7, 0x33, 0xc0, 0x68, 0x03, 0x04, 0x22, 0xaa, 0x9a,
            0xc3, 0xd4, 0x6c, 0x4e, 0xd2, 0x82, 0x64, 0x46, 0x07, 0x9f, 0xaa, 0x09, 0x14, 0xc2,
            0xd7, 0x05, 0xd9, 0x8b, 0x02, 0xa2, 0xb5, 0x12, 0x9c, 0xd1, 0xde, 0x16, 0x4e, 0xb9,
            0xcb, 0xd0, 0x83, 0xe8, 0xa2, 0x50, 0x3c, 0x4e,
        ];
        assert_eq!(bytes, expected);
    }

    /// The `4s0` framing round-trips, keeps the raw-block boundaries and
    /// leaves a body past one block intact.
    #[test]
    fn lz4_framing_round_trips() {
        let body: Vec<u8> = (0..(DATAPACK_LZ4_BLOCK * 2 + 77))
            .map(|index| (index % 251) as u8)
            .collect();
        let framed = datapack_lz4_frame(&body);
        let unframed = datapack_lz4_deframe(&framed).expect("deframe");
        assert_eq!(unframed, body);
        // The first frame's u16 length covers a block that decompresses to at
        // most the reference's 4096-byte capacity.
        let first = usize::from(u16::from_le_bytes([framed[0], framed[1]]));
        assert!(first > 0 && first < framed.len());
        assert!(datapack_lz4_deframe(&framed[..framed.len() - 1]).is_err());
    }

    /// The ChaCha stream is symmetric, deterministic in the seed and differs
    /// between parameter sets.
    #[test]
    fn chacha_stream_is_symmetric_and_seed_dependent() {
        let plaintext: Vec<u8> = (0..3000u32).map(|index| index as u8).collect();
        for cryptmode in 1..=6u16 {
            let mut ciphertext = plaintext.clone();
            datapack_chacha_apply(cryptmode, 0x1234_5678, b"iv", &mut ciphertext).expect("encrypt");
            assert_ne!(ciphertext, plaintext, "cryptmode {cryptmode}");
            datapack_chacha_apply(cryptmode, 0x1234_5678, b"iv", &mut ciphertext).expect("decrypt");
            assert_eq!(ciphertext, plaintext, "cryptmode {cryptmode}");
        }
        let mut first = plaintext.clone();
        let mut second = plaintext.clone();
        let mut third = plaintext.clone();
        datapack_chacha_apply(1, 1, b"iv", &mut first).expect("encrypt");
        datapack_chacha_apply(2, 1, b"iv", &mut second).expect("encrypt");
        datapack_chacha_apply(1, 2, b"iv", &mut third).expect("encrypt");
        assert_ne!(first, second);
        assert_ne!(first, third);
    }

    /// The thumbnail walkers stop exactly at the image's own end marker.
    #[test]
    fn image_end_walks_jpeg_and_png_structures() {
        let rgba = [0x11u8, 0x22, 0x33, 0xFF].repeat(16);
        let jpeg = encode_jpeg(&rgba, 4, 4, 75);
        assert_eq!(image_end(&jpeg), Some(jpeg.len()));
        let png = encode_png(&rgba, 4, 4);
        assert_eq!(image_end(&png), Some(png.len()));
        // A container behind the image is found where the image ends.
        let mut file = jpeg.clone();
        file.extend_from_slice(b"TJS/ns0\0rest");
        assert_eq!(image_end(&file), Some(jpeg.len()));
        assert_eq!(
            reference_data_pack(&file).map(|pack| &pack[..8]),
            Some(&b"TJS/ns0\0"[..])
        );
        // Truncated images have no end.
        assert_eq!(image_end(&jpeg[..jpeg.len() - 3]), None);
        assert_eq!(image_end(&png[..png.len() - 3]), None);
    }

    /// The DQT values of a baseline JPEG, in file order.
    fn jpeg_quantisation_tables(jpeg: &[u8]) -> Vec<[u8; 64]> {
        let mut tables = Vec::new();
        let mut index = 2;
        while index + 4 <= jpeg.len() {
            if jpeg[index] != 0xFF {
                break;
            }
            let marker = jpeg[index + 1];
            if marker == 0xDA {
                break;
            }
            let length = usize::from(u16::from_be_bytes([jpeg[index + 2], jpeg[index + 3]]));
            if marker == 0xDB {
                let mut position = index + 4;
                while position + 65 <= index + 2 + length {
                    tables.push(jpeg[position + 1..position + 65].try_into().unwrap());
                    position += 65;
                }
            }
            index += 2 + length;
        }
        tables
    }

    /// The DQT segment must carry the *scaled* tables the encoder quantises
    /// with: quantising by one table and advertising another made every
    /// quality but 50 needlessly lossy.
    #[test]
    fn jpeg_encoder_writes_the_scaled_quantisation_tables() {
        let rgba = [0x11u8, 0x22, 0x33, 0xFF].repeat(16 * 8);
        let tables = jpeg_quantisation_tables(&encode_jpeg(&rgba, 16, 8, 100));
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0], [1u8; 64], "q100 scales every entry to 1");
        assert_eq!(tables[1], [1u8; 64]);
        let tables = jpeg_quantisation_tables(&encode_jpeg(&rgba, 16, 8, 75));
        assert_eq!(tables[0][0], 8, "q75 scales the luma 16 to 8");
        assert_eq!(tables[1][0], 9, "q75 scales the chroma 17 to 9");
        assert_ne!(tables[0], tables[1]);
    }

    /// Flat colours survive the JPEG round trip through the engine's own
    /// decoder: both chroma components need their own DC predictor and the
    /// quantisation has to match the DQT. The tolerance is deliberately tight
    /// (the pre-review encoder decoded the left block as (0, 47, 219) instead
    /// of (0x11, 0x22, 0x33)).
    #[test]
    fn jpeg_thumbnail_keeps_flat_colours_through_the_engines_decoder() {
        let root = test_root("packinone-jpeg");
        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.thumb = new Layer();
                thumb.setImageSize(32, 8);
                thumb.fillRect(0, 0, 16, 8, 0xff112233);
                thumb.fillRect(16, 0, 16, 8, 0xffcc4411);
                var jpeg = Scripts.makeDataPackThumb(thumb, "jpg", 75, void);
                Storages.saveOctet("thumb.jpg", jpeg);
                global.view = new Layer();
                view.loadImages("thumb.jpg");
                "#,
            )
            .expect("encode and reload the thumbnail");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "view.imageWidth + \"x\" + view.imageHeight")
                .expect("thumbnail size"),
            Variant::String("32x8".to_string())
        );
        for (x, expected) in [(4i64, 0x112233i64), (28, 0xCC4411)] {
            let pixel = read_integer(&mut engine, &format!("view.getMainPixel({x}, 4)"));
            for shift in [16u32, 8, 0] {
                let actual = (pixel >> shift) & 0xFF;
                let want = (expected >> shift) & 0xFF;
                assert!(
                    (actual - want).abs() <= 6,
                    "pixel {x}: {pixel:#08x} should be near {expected:#08x}"
                );
            }
        }
        fs::remove_dir_all(root).expect("cleanup");
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

    /// The bundle's `loadDataPack` reads the storage name it is handed first
    /// and only then the engine's `name.pbd` alias. `tjsDataPack.dll` loads
    /// the name it is given (KAGEX passes the bookmark's own file name,
    /// `<saveDataLocation>data0.jpg`), while `PSDInfo.loadPBD` passes a base
    /// name and needs the alias — so the exact name must win when both exist.
    #[test]
    fn load_data_pack_prefers_the_exact_name_over_the_pbd_alias() {
        let root = test_root("packinone-datapack-alias");
        let pack = |answer: u8| {
            let mut bytes = b"KBAD100\0\x81\xa6".to_vec();
            for unit in "answer".encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes.push(answer);
            bytes
        };
        fs::write(root.join("data0.jpg"), pack(42)).expect("write the bookmark file");
        fs::write(root.join("data0.jpg.pbd"), pack(43)).expect("write its alias twin");
        fs::write(root.join("base.pbd"), pack(44)).expect("write a base-named pack");

        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     return Scripts.loadDataPack(\"data0.jpg\").answer + \":\" +\n\
                         Scripts.loadDataPack(\"base\").answer;\n\
                 })()",
            )
            .expect("load data packs");

        assert_eq!(
            value,
            Variant::String("42:44".to_string()),
            "the exact name wins; the .pbd alias stays as the fallback"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Files the port wrote before the container switch still load: a leading
    /// image, the `KBAD100` pack and the old 17-byte `KDPK` footer.
    #[test]
    fn load_data_pack_reads_the_ports_old_footer_format() {
        let root = test_root("packinone-datapack-legacy");
        let mut pack = b"KBAD100\0\x81\xa6".to_vec();
        for unit in "answer".encode_utf16() {
            pack.extend_from_slice(&unit.to_le_bytes());
        }
        pack.push(42);
        let mut bytes = b"BMthumbnail".to_vec();
        let pack_offset = bytes.len();
        bytes.extend_from_slice(&pack);
        bytes.extend_from_slice(&(pack_offset as u32).to_le_bytes());
        bytes.extend_from_slice(&(pack.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&7_u32.to_le_bytes());
        bytes.push(DATA_PACK_FOOTER_VERSION);
        bytes.extend_from_slice(DATA_PACK_FOOTER_MAGIC);
        fs::write(root.join("old.pbd"), bytes).expect("write the legacy pack");

        let mut engine = test_engine(&root);
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression("inline.tjs", "Scripts.loadDataPack(\"old.pbd\").answer")
            .expect("load the legacy data pack");

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

    /// `scriptsEx` exposes whether a function/object closure carries an
    /// `ObjThis` context; Action.tjs uses this to bind unqualified completion
    /// callbacks to the action instance before invoking them. The bundle
    /// installs the standalone module's implementation, so the probe goes
    /// through the script surface the game sees.
    #[test]
    fn scripts_is_null_context_matches_scriptsex_objthis_semantics() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(PackinOnePlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var plain = function() {};\n\
                     var bound = (Scripts.getObjectContext incontextof Scripts);\n\
                     return Scripts.isNullContext(plain) + \":\" +\n\
                         Scripts.isNullContext(bound);\n\
                 })()",
            )
            .expect("context probes");

        assert_eq!(value, Variant::String("1:0".to_string()));
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
