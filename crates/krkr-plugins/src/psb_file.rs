//! Compatibility implementation for wamsoft `psbfile.dll`
//! (`PSBFile` / `PSBValueClass`).
//!
//! PSB is M2's compact, typed object format.  The game uses `.ks.scn` PSB
//! documents for compiled scenarios, so reporting a successful load with an
//! empty root is observably different from the original plugin: the scenario
//! dispatcher sees no tags and immediately returns to title.  This module
//! decodes the standard PSB object tree and exposes its maps as TJS
//! dictionaries, arrays as TJS arrays, and resource values as TJS octets.
//!
//! Loading is **plaintext-first**.  The header's encryption field (the low two
//! bits of the u16 at 0x06) is a hint, not a claim: M2's own toolchain sets
//! bit0 on plaintext documents (PARQUET's `.pimg` files are `flags=0x0001`
//! despite carrying no ciphertext), so the structural parse always runs first
//! and a document that parses is accepted whatever the flag says.  v3 headers
//! are then validated against their own Adler-32 (bytes `0x08..0x28` against
//! the u32 at `0x28`), which catches plaintext corruption no key could fix.
//! Only a document whose structure genuinely fails *and* whose flag claims a
//! cipher is reported as key-required — the one error a Phase-2 key store
//! ([`PsbKeyStore`]) would convert into a real decode.  Everything else stays a
//! plain malformed-document error.  See the M45 survey
//! (`.tower/comms/inbox/20260912-psb-decrypt-study-*`) for the evidence.

use std::{collections::BTreeMap, str};

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "PSBFile / PSBValueClass",
    notes: "Decodes PSB object trees into TJS dictionaries/arrays/octets and mounts embedded resources.",
    install: |engine| engine.register_plugin(PsbFilePlugin),
};

/// Bit set in the header's encryption field that makes a failing document
/// report as key-required instead of malformed.
const PSB_FLAG_ENCRYPTION_HINT: u16 = 0x0003;
/// The v3 header protects bytes 0x08..0x28 with an Adler-32 stored at 0x28.
const PSB_V3_CHECKSUM_START: usize = 0x08;
const PSB_V3_CHECKSUM_END: usize = 0x28;
const PSB_V3_CHECKSUM_OFFSET: usize = 0x28;

pub struct PsbFilePlugin;

impl KrkrPlugin for PsbFilePlugin {
    fn name(&self) -> &str {
        "psbfile.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_psb_file_compat(runtime);
        Ok(())
    }
}

fn install_psb_file_compat(runtime: &mut Runtime<KrkrHost>) {
    let value_class = psb_value_constructor(runtime);
    let file_class = psb_file_constructor(runtime);
    runtime.set_global_member("PSBValueClass", Variant::Object(value_class));
    runtime.set_global_member("PSBFile", Variant::Object(file_class));
}

/// Fresh empty `PSBValueClass` instance (`count` == 0).
fn new_psb_value_instance(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let instance = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(instance, "PSBValueClass");
    runtime.set_object_member(instance, "count", Variant::Integer(0));
    instance
}

fn psb_value_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| new_psb_value_instance(runtime));
            runtime.add_object_class_info(instance, "PSBValueClass");
            runtime.set_object_member(instance, "count", Variant::Integer(0));
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "PSBValueClass");
    handle
}

fn psb_file_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "PSBFile");
            install_psb_file_members(runtime, instance);
            // The historic psbfile.dll API accepts both `new PSBFile()` plus
            // `.load(storage)` and `new PSBFile(storage)`.  Scenario code in
            // the wild uses the latter form, so a constructor that merely
            // creates an empty object silently makes sceneplay a no-op.
            if let Some(Variant::String(storage)) = args.first() {
                load_psb_storage(runtime, instance, storage);
            }
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "PSBFile");
    install_psb_file_members(runtime, handle);
    handle
}

fn install_psb_file_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    if matches!(runtime.object_member(handle, "root"), Variant::Void) {
        runtime.set_object_member(handle, "root", Variant::Void);
    }
    runtime.register_object_native(handle, "load", psb_file_load);
    runtime.register_object_native(handle, "clearStorageCache", native_void);
    runtime.register_object_native(handle, "finalize", native_void);
}

fn psb_file_load(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(Variant::String(storage)) = args.first() else {
        return Ok(Variant::Integer(0));
    };
    let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) else {
        return Ok(Variant::Integer(0));
    };
    Ok(Variant::Integer(i64::from(load_psb_storage(
        runtime, this, storage,
    ))))
}

fn load_psb_storage(runtime: &mut Runtime<KrkrHost>, this: ObjectHandle, storage: &str) -> bool {
    match runtime.host().read_binary_storage(storage) {
        Ok(data) => {
            let root = match PsbDocument::load(&data) {
                Ok(root) => root,
                Err(error) => {
                    runtime
                        .host_mut()
                        .log(&format!("psbfile: failed to parse `{storage}`: {error}"));
                    return false;
                }
            };
            mount_psb_resources(runtime, storage, &root);
            let root = psb_value_to_variant(runtime, &root, true);
            runtime.set_object_member(this, "root", root);
            true
        }
        Err(_) => false,
    }
}

fn mount_psb_resources(runtime: &mut Runtime<KrkrHost>, storage: &str, root: &PsbValue) {
    let PsbValue::Object(entries) = root else {
        return;
    };
    let basename = storage
        .rsplit(['/', '\\', '>'])
        .next()
        .unwrap_or(storage)
        .to_lowercase();
    for (name, value) in entries {
        let PsbValue::Octet(bytes) = value else {
            continue;
        };
        let path = format!("psb://{basename}/{name}");
        if let Err(error) = runtime.host().mount_virtual_resource(&path, bytes.clone()) {
            runtime
                .host_mut()
                .log(&format!("psbfile: failed to mount `{path}`: {error}"));
        }
    }
}

/// Parses a PSB document into its value tree.  Exposed for offline
/// debugging tools (e.g. the `psb_dump` example); not part of the plugin
/// surface seen by games.
#[doc(hidden)]
pub fn debug_parse_psb(bytes: &[u8]) -> std::result::Result<PsbValue, String> {
    PsbDocument::load(bytes).map_err(|error| error.to_string())
}

/// Why a PSB document did not load.  The distinction is deliberate: a
/// key-required document is one a Phase-2 key store can still decode, while a
/// malformed document cannot be recovered by any key.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub enum PsbError {
    /// The document is not a PSB of a supported version, its v3 header
    /// checksum does not match, or its structure does not decode.
    Malformed(String),
    /// The document's structure does not decode *and* its header flags claim a
    /// cipher: the payload needs the game's key, which this crate does not
    /// have yet (Phase 2).
    KeyRequired {
        /// The header's encryption field (low two bits select the cipher).
        flags: u16,
    },
}

impl std::fmt::Display for PsbError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(reason) => write!(formatter, "{reason}"),
            Self::KeyRequired { flags } => write!(
                formatter,
                "encrypted PSB (flags {flags:#06x}) needs the game's key, which is not available; \
                 no cipher is registered yet (Phase 2)"
            ),
        }
    }
}

/// Where Phase 2's cipher registry slots in.
///
/// The mission that implements key-based PSB decryption adds a concrete store
/// (keys read from `GameProfile`/config) and wires it into
/// [`PsbDocument::load`]: call [`PsbDocument::load_plaintext`] first, then, on
/// [`PsbError::KeyRequired`], look up `flags` here and feed the decrypted bytes
/// back through the plaintext path.  Nothing registers one today, so the
/// plaintext path is the whole pipeline.  Note that in a genuinely encrypted
/// header the Adler-32 at 0x28 is ciphertext too, so a key-less parse cannot
/// tell truncation from ciphertext just by looking — which is why the checksum
/// gate only rejects a document whose *structure* also parsed.
#[allow(dead_code, reason = "Phase-2 seam: no key store exists yet")]
#[doc(hidden)]
pub trait PsbKeyStore {
    /// Decrypts a document the header says is encrypted, returning its
    /// plaintext PSB bytes.
    fn decrypt(&self, flags: u16, bytes: &[u8]) -> std::result::Result<Vec<u8>, String>;
}

#[derive(Debug, Clone, PartialEq)]
#[doc(hidden)]
pub enum PsbValue {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    String(String),
    Octet(Vec<u8>),
    Array(Vec<PsbValue>),
    Object(BTreeMap<String, PsbValue>),
}

#[derive(Debug, Clone, Copy)]
struct PsbArray {
    count: usize,
    entry_size: usize,
    data_offset: usize,
    serialized_size: usize,
}

/// The fixed header of a PSB v2/v3 document.
#[derive(Debug, Clone, Copy)]
struct PsbHeader {
    version: u16,
    flags: u16,
    names_offset: usize,
    strings_offset: usize,
    strings_data_offset: usize,
    chunk_offsets_offset: usize,
    chunk_lengths_offset: usize,
    chunk_data_offset: usize,
    root_offset: usize,
}

/// Why the plaintext decode of an already-parsed header failed.
enum PlaintextError {
    /// The document's bytes do not match its own declared structure.
    Structure(String),
    /// The v3 header checksum does not match.
    Integrity,
}

/// Standard PSB v2/v3 reader.  Its layout follows the public M2 PSB format
/// implementation in number201724/psbfile: the name trie is represented by
/// three packed arrays, and collections/dictionaries store relative offsets
/// to their child values.
struct PsbDocument<'a> {
    bytes: &'a [u8],
    version: u16,
    names_offset: usize,
    strings_offset: usize,
    strings_data_offset: usize,
    chunk_offsets_offset: usize,
    chunk_lengths_offset: usize,
    chunk_data_offset: usize,
    root_offset: usize,
    names: BTreeMap<u64, String>,
}

impl<'a> PsbDocument<'a> {
    /// The plugin's entry point: plaintext first, header hint second.
    ///
    /// A flagged-but-plain document parses exactly like an unflagged one.
    /// Only a document the plaintext reader *cannot* decode, and whose header
    /// flags claim a cipher, is classified as key-required; every other
    /// failure keeps its specific malformed reason.
    fn load(bytes: &'a [u8]) -> std::result::Result<PsbValue, PsbError> {
        let Some(header) = Self::parse_header(bytes) else {
            return Err(PsbError::Malformed("not a PSB document".to_string()));
        };
        match Self::load_plaintext(bytes, header) {
            Ok(root) => Ok(root),
            // A failed checksum is corruption, not ciphertext: no key can
            // make the document match its own offset table again.
            Err(PlaintextError::Integrity) => Err(PsbError::Malformed(
                "PSB v3 header checksum does not match".to_string(),
            )),
            Err(PlaintextError::Structure(reason)) => {
                // Only a document that really looks like a PSB can claim a
                // cipher: a foreign file with a byte pattern in the flag
                // position must not be reported as key-required.
                if header.flags & PSB_FLAG_ENCRYPTION_HINT != 0 {
                    // Not necessarily true ciphertext — the M45 survey found
                    // M2 sets bit0 on plaintext `.pimg` documents — but it is
                    // the only recoverable case, and the one a Phase-2 key
                    // store (`PsbKeyStore`) exists to convert into a decode.
                    Err(PsbError::KeyRequired {
                        flags: header.flags,
                    })
                } else {
                    Err(PsbError::Malformed(reason))
                }
            }
        }
    }

    /// Parses the fixed 0x2c-byte v2/v3 header: magic, version, encryption
    /// flags and the seven offsets.  Returns `None` when the bytes cannot be a
    /// PSB at all, so a foreign file is never mistaken for a ciphered one.
    fn parse_header(bytes: &[u8]) -> Option<PsbHeader> {
        if bytes.len() < 0x28 || &bytes[..4] != b"PSB\0" {
            return None;
        }
        let version = read_u16(bytes, 4).ok()?;
        if !(2..=3).contains(&version) {
            return None;
        }
        Some(PsbHeader {
            version,
            flags: read_u16(bytes, 6).ok()?,
            names_offset: read_u32(bytes, 0x0c).ok()? as usize,
            strings_offset: read_u32(bytes, 0x10).ok()? as usize,
            strings_data_offset: read_u32(bytes, 0x14).ok()? as usize,
            chunk_offsets_offset: read_u32(bytes, 0x18).ok()? as usize,
            chunk_lengths_offset: read_u32(bytes, 0x1c).ok()? as usize,
            chunk_data_offset: read_u32(bytes, 0x20).ok()? as usize,
            root_offset: read_u32(bytes, 0x24).ok()? as usize,
        })
    }

    /// Decodes a document whose header has been parsed.  The header's
    /// encryption field is deliberately *not* consulted here: M2's toolchain
    /// sets bit0 on plaintext documents (PARQUET's `.pimg`), so the structure
    /// decides and `load` maps a genuine failure onto the key-required error.
    fn load_plaintext(
        bytes: &'a [u8],
        header: PsbHeader,
    ) -> std::result::Result<PsbValue, PlaintextError> {
        let mut document = Self {
            bytes,
            version: header.version,
            names_offset: header.names_offset,
            strings_offset: header.strings_offset,
            strings_data_offset: header.strings_data_offset,
            chunk_offsets_offset: header.chunk_offsets_offset,
            chunk_lengths_offset: header.chunk_lengths_offset,
            chunk_data_offset: header.chunk_data_offset,
            root_offset: header.root_offset,
            names: BTreeMap::new(),
        };
        document
            .validate_offsets()
            .map_err(PlaintextError::Structure)?;
        if !document.integrity_checksum_holds() {
            // The Adler-32 covers the offset table itself, so a mismatch means
            // the document was altered — a condition no key can recover.
            return Err(PlaintextError::Integrity);
        }
        document.names = document.decode_names().map_err(PlaintextError::Structure)?;
        document
            .decode_value(document.root_offset, 0)
            .map_err(PlaintextError::Structure)
    }

    fn validate_offsets(&self) -> std::result::Result<(), String> {
        for (name, offset) in [
            ("names", self.names_offset),
            ("strings", self.strings_offset),
            ("strings data", self.strings_data_offset),
            ("chunk offsets", self.chunk_offsets_offset),
            ("chunk lengths", self.chunk_lengths_offset),
            ("root", self.root_offset),
        ] {
            if offset >= self.bytes.len() {
                return Err(format!("PSB {name} offset is out of bounds"));
            }
        }
        // A PSB with no binary resources stores chunk data immediately at
        // EOF; that is a valid empty range.
        if self.chunk_data_offset > self.bytes.len() {
            return Err("PSB chunk data offset is out of bounds".to_string());
        }
        if self.bytes[self.root_offset] != 0x21 {
            return Err("PSB root is not an object".to_string());
        }
        Ok(())
    }

    /// PSB v3 protects the header bytes `0x08..0x28` with an Adler-32 stored as
    /// a u32 at `0x28` — the same framing the format's own toolchain writes and
    /// the check xp3-brute performs on the v3 header.  v2 has no other
    /// integrity signal, so its documents keep going through the structural
    /// checks alone.
    fn integrity_checksum_holds(&self) -> bool {
        if self.version != 3 {
            return true;
        }
        let Ok(stored) = read_u32(self.bytes, PSB_V3_CHECKSUM_OFFSET) else {
            return false;
        };
        let Some(protected) = self.bytes.get(PSB_V3_CHECKSUM_START..PSB_V3_CHECKSUM_END) else {
            return false;
        };
        stored == adler32(protected)
    }

    fn decode_names(&self) -> std::result::Result<BTreeMap<u64, String>, String> {
        let charset = self.array_at(self.names_offset)?;
        let names_data_offset = self.names_offset + charset.serialized_size;
        let names_data = self.array_at(names_data_offset)?;
        let name_indexes_offset = names_data_offset + names_data.serialized_size;
        let name_indexes = self.array_at(name_indexes_offset)?;

        // This is deliberately the same representation used by psbfile.dll:
        // `nameIndexes` is indexed by an object's key id, and each entry
        // points into a compact backwards character chain in `namesData`.
        // The earlier breadth-first reconstruction omitted that final index
        // table, which could associate a valid name with the wrong object key.
        let mut names = BTreeMap::new();
        for key_id in 0..name_indexes.count {
            let name_index = name_indexes.get(self.bytes, key_id)? as usize;
            if name_index >= names_data.count {
                return Err("PSB name index is out of bounds".to_string());
            }
            let mut node = names_data.get(self.bytes, name_index)? as usize;
            let mut reversed = Vec::new();
            while node != 0 {
                if node >= names_data.count {
                    return Err("PSB name node is out of bounds".to_string());
                }
                let parent = names_data.get(self.bytes, node)? as usize;
                if parent >= charset.count {
                    return Err("PSB name parent is out of bounds".to_string());
                }
                let base = charset.get(self.bytes, parent)? as usize;
                let character = node
                    .checked_sub(base)
                    .filter(|character| *character <= u8::MAX as usize)
                    .ok_or_else(|| "PSB name character is invalid".to_string())?;
                reversed.push(character as u8);
                if reversed.len() > names_data.count {
                    return Err("PSB name chain is cyclic".to_string());
                }
                node = parent;
            }
            reversed.reverse();
            let name = str::from_utf8(&reversed)
                .map_err(|_| "PSB name is not UTF-8".to_string())?
                .to_string();
            names.insert(key_id as u64, name);
        }
        Ok(names)
    }

    fn array_at(&self, offset: usize) -> std::result::Result<PsbArray, String> {
        let kind = *self
            .bytes
            .get(offset)
            .ok_or_else(|| "PSB array header is out of bounds".to_string())?;
        if !(0x0d..=0x14).contains(&kind) {
            return Err(format!("invalid PSB packed-array type {kind:#x}"));
        }
        let count_bytes = (kind - 0x0c) as usize;
        let count_start = offset + 1;
        let count = self.read_unsigned(count_start, count_bytes)? as usize;
        let size_type_pos = count_start + count_bytes;
        let size_kind = *self
            .bytes
            .get(size_type_pos)
            .ok_or_else(|| "PSB array element width is out of bounds".to_string())?;
        // 0x0c represents a zero-byte element width.  The reference
        // psbfile.dll uses it for empty packed arrays (notably the resource
        // offset/length tables in scenarios without embedded resources).
        // A non-empty array cannot carry useful values at that width.
        if !(0x0c..=0x14).contains(&size_kind) {
            return Err("invalid PSB packed-array element width".to_string());
        }
        let entry_size = (size_kind - 0x0c) as usize;
        if entry_size == 0 && count != 0 {
            return Err("non-empty PSB packed-array has zero element width".to_string());
        }
        let data_offset = size_type_pos + 1;
        let data_length = count
            .checked_mul(entry_size)
            .ok_or_else(|| "PSB packed-array length overflow".to_string())?;
        let end = data_offset
            .checked_add(data_length)
            .ok_or_else(|| "PSB packed-array end overflow".to_string())?;
        if end > self.bytes.len() {
            return Err("PSB packed-array exceeds document".to_string());
        }
        Ok(PsbArray {
            count,
            entry_size,
            data_offset,
            serialized_size: 1 + count_bytes + 1 + data_length,
        })
    }

    fn decode_value(&self, offset: usize, depth: usize) -> std::result::Result<PsbValue, String> {
        if depth > 256 {
            return Err("PSB nesting is too deep".to_string());
        }
        let kind = *self
            .bytes
            .get(offset)
            .ok_or_else(|| "PSB value offset is out of bounds".to_string())?;
        match kind {
            0x01 => Ok(PsbValue::Null),
            // PSB's wire values are false=2 and true=3.
            0x02 => Ok(PsbValue::Bool(false)),
            0x03 => Ok(PsbValue::Bool(true)),
            0x04..=0x0c => Ok(PsbValue::Integer(
                self.read_signed(offset + 1, (kind - 0x04) as usize)?,
            )),
            0x15..=0x18 => {
                let index = self.read_unsigned(offset + 1, (kind - 0x14) as usize)? as usize;
                let strings = self.array_at(self.strings_offset)?;
                if index >= strings.count {
                    return Err("PSB string index is out of bounds".to_string());
                }
                let string_offset = self
                    .strings_data_offset
                    .checked_add(strings.get(self.bytes, index)? as usize)
                    .ok_or_else(|| "PSB string offset overflow".to_string())?;
                Ok(PsbValue::String(self.c_string(string_offset)?))
            }
            0x19..=0x1c => self
                .decode_resource(self.read_unsigned(offset + 1, (kind - 0x18) as usize)? as usize),
            0x1d => Ok(PsbValue::Real(0.0)),
            0x1e => Ok(PsbValue::Real(
                f32::from_le_bytes(self.read_fixed(offset + 1)?) as f64,
            )),
            0x1f => Ok(PsbValue::Real(f64::from_le_bytes(
                self.read_fixed(offset + 1)?,
            ))),
            0x20 => self.decode_collection(offset, depth + 1),
            0x21 => self.decode_object(offset, depth + 1),
            other => Err(format!("unsupported PSB value type {other:#x}")),
        }
    }

    fn decode_collection(
        &self,
        offset: usize,
        depth: usize,
    ) -> std::result::Result<PsbValue, String> {
        let body = offset + 1;
        let offsets = self.array_at(body)?;
        let values_start = body + offsets.serialized_size;
        let mut values = Vec::with_capacity(offsets.count);
        for index in 0..offsets.count {
            let value_offset = values_start
                .checked_add(offsets.get(self.bytes, index)? as usize)
                .ok_or_else(|| "PSB collection offset overflow".to_string())?;
            values.push(self.decode_value(value_offset, depth)?);
        }
        Ok(PsbValue::Array(values))
    }

    fn decode_resource(&self, index: usize) -> std::result::Result<PsbValue, String> {
        let offsets = self.array_at(self.chunk_offsets_offset)?;
        let lengths = self.array_at(self.chunk_lengths_offset)?;
        if index >= offsets.count || index >= lengths.count {
            return Err("PSB resource index is out of bounds".to_string());
        }
        let offset = self
            .chunk_data_offset
            .checked_add(offsets.get(self.bytes, index)? as usize)
            .ok_or_else(|| "PSB resource offset overflow".to_string())?;
        let length = lengths.get(self.bytes, index)? as usize;
        let bytes = self
            .bytes
            .get(offset..offset.saturating_add(length))
            .ok_or_else(|| "PSB resource exceeds document".to_string())?;
        Ok(PsbValue::Octet(bytes.to_vec()))
    }

    fn decode_object(&self, offset: usize, depth: usize) -> std::result::Result<PsbValue, String> {
        let body = offset + 1;
        let keys = self.array_at(body)?;
        let values_array_offset = body + keys.serialized_size;
        let values = self.array_at(values_array_offset)?;
        if keys.count != values.count {
            return Err("PSB object key/value count differs".to_string());
        }
        let values_start = values_array_offset + values.serialized_size;
        let mut object = BTreeMap::new();
        for index in 0..keys.count {
            let key_id = keys.get(self.bytes, index)?;
            let name = self
                .names
                .get(&key_id)
                .ok_or_else(|| format!("PSB object references unknown key {key_id}"))?
                .clone();
            let value_offset = values_start
                .checked_add(values.get(self.bytes, index)? as usize)
                .ok_or_else(|| "PSB object offset overflow".to_string())?;
            object.insert(name, self.decode_value(value_offset, depth)?);
        }
        Ok(PsbValue::Object(object))
    }

    fn c_string(&self, offset: usize) -> std::result::Result<String, String> {
        let tail = self
            .bytes
            .get(offset..)
            .ok_or_else(|| "PSB string begins out of bounds".to_string())?;
        let end = tail
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| "PSB string is unterminated".to_string())?;
        str::from_utf8(&tail[..end])
            .map(str::to_string)
            .map_err(|_| "PSB string is not UTF-8".to_string())
    }

    fn read_unsigned(&self, offset: usize, size: usize) -> std::result::Result<u64, String> {
        if size > 8
            || offset
                .checked_add(size)
                .is_none_or(|end| end > self.bytes.len())
        {
            return Err("PSB integer is out of bounds".to_string());
        }
        let mut value = 0_u64;
        for (index, byte) in self.bytes[offset..offset + size].iter().enumerate() {
            value |= u64::from(*byte) << (index * 8);
        }
        Ok(value)
    }

    fn read_signed(&self, offset: usize, size: usize) -> std::result::Result<i64, String> {
        if size == 0 {
            return Ok(0);
        }
        let value = self.read_unsigned(offset, size)?;
        let shift = 64 - size * 8;
        Ok(((value << shift) as i64) >> shift)
    }

    fn read_fixed<const N: usize>(&self, offset: usize) -> std::result::Result<[u8; N], String> {
        let slice = self
            .bytes
            .get(offset..offset + N)
            .ok_or_else(|| "PSB fixed-size value is out of bounds".to_string())?;
        slice
            .try_into()
            .map_err(|_| "PSB fixed-size value has wrong length".to_string())
    }
}

impl PsbArray {
    fn get(self, bytes: &[u8], index: usize) -> std::result::Result<u64, String> {
        if index >= self.count {
            return Err("PSB packed-array index is out of bounds".to_string());
        }
        let offset = self.data_offset + index * self.entry_size;
        let mut value = 0_u64;
        for (shift, byte) in bytes[offset..offset + self.entry_size].iter().enumerate() {
            value |= u64::from(*byte) << (shift * 8);
        }
        Ok(value)
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> std::result::Result<u16, String> {
    bytes
        .get(offset..offset + 2)
        .and_then(|slice| slice.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| "PSB header is truncated".to_string())
}

fn read_u32(bytes: &[u8], offset: usize) -> std::result::Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|slice| slice.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "PSB header is truncated".to_string())
}

/// Adler-32 as specified by RFC 1950 / zlib, which is the checksum PSB v3
/// stores in its header (verified against PARQUET's plaintext `.pimg`
/// documents, all 95 of which match).
fn adler32(data: &[u8]) -> u32 {
    let mut low = 1_u32;
    let mut high = 0_u32;
    for byte in data {
        low = (low + u32::from(*byte)) % 65521;
        high = (high + low) % 65521;
    }
    (high << 16) | low
}

fn psb_value_to_variant(runtime: &mut Runtime<KrkrHost>, value: &PsbValue, root: bool) -> Variant {
    match value {
        // PSBFile's PSBNull::toTJSVal returns a default tTJSVariant, i.e.
        // TJS `void`, rather than the language's distinct `null` value.
        // ScenePlayer uses `=== void` for omitted optional fields, so mapping
        // it to null sends valid scenarios down the wrong branches.
        PsbValue::Null => Variant::Void,
        PsbValue::Bool(value) => Variant::Integer(i64::from(*value)),
        PsbValue::Integer(value) => Variant::Integer(*value),
        PsbValue::Real(value) => Variant::Real(*value),
        PsbValue::String(value) => Variant::String(value.clone()),
        PsbValue::Octet(bytes) => Variant::Octet(bytes.clone()),
        PsbValue::Array(values) => {
            let values = values
                .iter()
                .map(|value| psb_value_to_variant(runtime, value, false))
                .collect();
            Variant::Object(runtime.alloc_array_object(values))
        }
        PsbValue::Object(values) => {
            // psbfile's `root` is a custom object and nested maps are full
            // TJS dictionaries (including assign/clear/etc.), not merely an
            // object annotated with a Dictionary class name.
            let object = if root {
                runtime.alloc_ordinary_object()
            } else {
                runtime.alloc_dictionary_object()
            };
            for (name, value) in values {
                let value = psb_value_to_variant(runtime, value, false);
                runtime.set_object_member(object, name, value);
            }
            Variant::Object(object)
        }
    }
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte offsets inside [`minimal_document`]; the builder asserts every one
    /// of them, so a layout tweak cannot silently invalidate the constants.
    const NAMES_OFFSET: usize = 0x2c;
    const NAMES_NODES_OFFSET: usize = 0x2f;
    const NAME_INDEXES_OFFSET: usize = 0x32;
    const STRINGS_OFFSET: usize = 0x35;
    const STRINGS_DATA_OFFSET: usize = 0x38;
    const CHUNK_OFFSETS_OFFSET: usize = 0x38;
    const CHUNK_LENGTHS_OFFSET: usize = 0x3b;
    const CHUNK_DATA_OFFSET: usize = 0x3e;
    const ROOT_OFFSET: usize = 0x3e;
    const ROOT_LENGTH: usize = 7;

    /// A complete PSB v3 the reader accepts, built the way the format's own
    /// toolchain lays a document out: the name trie is three packed arrays
    /// (charset, parent-node references, then one index per key id), the
    /// string table, then the resource tables, then the root object.
    ///
    /// This document has an empty root object and no keys, so it exercises the
    /// whole admission path — magic, version, the header checksum, the packed
    /// arrays, the string and resource tables, and value decoding — without
    /// hand-encoding a name chain (M45's survey could not recover the trie's
    /// exact chain grammar from the available samples, and a wrong guess here
    /// would test the fixture instead of the reader).
    fn minimal_document(flags: u16) -> Vec<u8> {
        // The three empty trie arrays: charset, namesData and nameIndexes.
        let charset = vec![0x0d_u8, 0x00, 0x0c];
        let names_data = vec![0x0d_u8, 0x00, 0x0c];
        let name_indexes = vec![0x0d_u8, 0x00, 0x0c];
        // One empty string pool and two empty resource tables.
        let strings = vec![0x0d_u8, 0x00, 0x0c];
        let chunk_offsets = vec![0x0d_u8, 0x00, 0x0c];
        let chunk_lengths = vec![0x0d_u8, 0x00, 0x0c];
        // Root object with no keys and no values.
        let root = vec![0x21_u8, 0x0d, 0x00, 0x0c, 0x0d, 0x00, 0x0c];

        // Assemble the payload, recording where each table begins, so the
        // constants the tests use are checked against what actually lands in
        // the file.
        let mut payload = Vec::new();
        payload.extend_from_slice(&charset);
        let names_nodes_offset = NAMES_OFFSET + payload.len();
        payload.extend_from_slice(&names_data);
        let name_indexes_offset = NAMES_OFFSET + payload.len();
        payload.extend_from_slice(&name_indexes);
        let strings_offset = NAMES_OFFSET + payload.len();
        payload.extend_from_slice(&strings);
        let strings_data_offset = NAMES_OFFSET + payload.len();
        let chunk_offsets_offset = strings_data_offset;
        payload.extend_from_slice(&chunk_offsets);
        let chunk_lengths_offset = NAMES_OFFSET + payload.len();
        payload.extend_from_slice(&chunk_lengths);
        let chunk_data_offset = NAMES_OFFSET + payload.len();
        let root_offset = chunk_data_offset;
        payload.extend_from_slice(&root);
        for (actual, expected) in [
            (names_nodes_offset, NAMES_NODES_OFFSET),
            (name_indexes_offset, NAME_INDEXES_OFFSET),
            (strings_offset, STRINGS_OFFSET),
            (strings_data_offset, STRINGS_DATA_OFFSET),
            (chunk_offsets_offset, CHUNK_OFFSETS_OFFSET),
            (chunk_lengths_offset, CHUNK_LENGTHS_OFFSET),
            (chunk_data_offset, CHUNK_DATA_OFFSET),
            (root_offset, ROOT_OFFSET),
        ] {
            assert_eq!(actual, expected, "fixture offset");
        }
        assert_eq!(
            payload.len() + NAMES_OFFSET,
            ROOT_OFFSET + ROOT_LENGTH,
            "document length"
        );

        let mut bytes = vec![0_u8; NAMES_OFFSET];
        bytes[..4].copy_from_slice(b"PSB\0");
        bytes[4..6].copy_from_slice(&3_u16.to_le_bytes());
        bytes[6..8].copy_from_slice(&flags.to_le_bytes());
        for (offset, value) in [
            (0x0c, NAMES_OFFSET),
            (0x10, strings_offset),
            (0x14, strings_data_offset),
            (0x18, chunk_offsets_offset),
            (0x1c, chunk_lengths_offset),
            (0x20, chunk_data_offset),
            (0x24, root_offset),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
        }
        bytes.extend_from_slice(&payload);

        let checksum = adler32(&bytes[PSB_V3_CHECKSUM_START..PSB_V3_CHECKSUM_END]);
        bytes[PSB_V3_CHECKSUM_OFFSET..PSB_V3_CHECKSUM_OFFSET + 4]
            .copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    fn minimal_root() -> PsbValue {
        PsbValue::Object(BTreeMap::new())
    }

    #[test]
    fn flagged_but_plaintext_v3_document_loads_like_a_plain_one() {
        let mut bytes = minimal_document(0x0001);
        assert_eq!(PsbDocument::load(&bytes), Ok(minimal_root()));
        // The flag is a hint, not a claim: flipping it back and forth must not
        // change what the document decodes to (M45: M2 sets bit0 on plaintext
        // `.pimg` documents).
        bytes[6] ^= 0x01;
        assert_eq!(PsbDocument::load(&bytes), Ok(minimal_root()));
    }

    #[test]
    fn v3_header_checksum_holds_only_for_the_intact_header() {
        let bytes = minimal_document(0x0001);
        let stored = u32::from_le_bytes(bytes[0x28..0x2c].try_into().unwrap());
        assert_eq!(
            stored,
            adler32(&bytes[PSB_V3_CHECKSUM_START..PSB_V3_CHECKSUM_END])
        );
        let mut altered = bytes.clone();
        altered[0x0c] ^= 0x01;
        assert_ne!(
            stored,
            adler32(&altered[PSB_V3_CHECKSUM_START..PSB_V3_CHECKSUM_END])
        );
    }

    #[test]
    fn parseable_document_with_a_broken_header_checksum_is_malformed() {
        let mut bytes = minimal_document(0x0001);
        // Flip a bit in the header's chunkData offset — the checksum protects
        // exactly this region, and the offset is unused here (the document has
        // no resources), so the structure still parses and only the checksum
        // fails.
        bytes[0x20] ^= 0x01;
        assert!(matches!(
            PsbDocument::load(&bytes),
            Err(PsbError::Malformed(reason)) if reason.contains("checksum")
        ));
    }

    #[test]
    fn struct_corrupt_document_requiring_a_key_returns_key_required() {
        let mut bytes = minimal_document(0x0003);
        // The root offset points at the magic instead of an object marker:
        // genuine structural failure with the cipher flag set.
        bytes[0x24..0x28].copy_from_slice(&0_u32.to_le_bytes());
        assert!(matches!(
            PsbDocument::load(&bytes),
            Err(PsbError::KeyRequired { flags: 0x0003 })
        ));
    }

    #[test]
    fn truncation_is_never_classified_as_key_required() {
        for flags in [0x0000, 0x0003] {
            let bytes = minimal_document(flags);
            // Cut inside the protected header region: the document is not even
            // complete enough to have a structure to decode, so the header's
            // claim cannot change the verdict.
            assert!(matches!(
                PsbDocument::load(&bytes[..0x20]),
                Err(PsbError::Malformed(_))
            ));
        }
    }

    #[test]
    fn unprefixed_bytes_are_malformed_not_key_required() {
        // The encryption field is only trusted once the document claims to be
        // a PSB at all.
        let mut bytes = minimal_document(0x0003);
        bytes[..4].copy_from_slice(b"XXX\0");
        assert!(matches!(
            PsbDocument::load(&bytes),
            Err(PsbError::Malformed(_))
        ));
    }

    #[test]
    fn empty_packed_array_accepts_zero_byte_elements() {
        let bytes = [0x0d, 0x00, 0x0c];
        let document = PsbDocument {
            bytes: &bytes,
            version: 0,
            names_offset: 0,
            strings_offset: 0,
            strings_data_offset: 0,
            chunk_offsets_offset: 0,
            chunk_lengths_offset: 0,
            chunk_data_offset: 0,
            root_offset: 0,
            names: BTreeMap::new(),
        };
        let array = document.array_at(0).expect("empty PSB array");
        assert_eq!(array.count, 0);
        assert_eq!(array.entry_size, 0);
        assert_eq!(array.serialized_size, 3);
    }

    #[test]
    fn non_empty_packed_array_rejects_zero_byte_elements() {
        let bytes = [0x0d, 0x01, 0x0c];
        let document = PsbDocument {
            bytes: &bytes,
            version: 0,
            names_offset: 0,
            strings_offset: 0,
            strings_data_offset: 0,
            chunk_offsets_offset: 0,
            chunk_lengths_offset: 0,
            chunk_data_offset: 0,
            root_offset: 0,
            names: BTreeMap::new(),
        };
        assert!(document.array_at(0).is_err());
    }

    #[test]
    fn psb_null_is_tjs_void_and_nested_maps_are_real_dictionaries() {
        let mut runtime = Runtime::with_host(KrkrHost::default());
        let value = PsbValue::Object(BTreeMap::from([
            ("optional".to_string(), PsbValue::Null),
            (
                "nested".to_string(),
                PsbValue::Object(BTreeMap::from([(
                    "answer".to_string(),
                    PsbValue::Integer(42),
                )])),
            ),
        ]));

        let Variant::Object(root) = psb_value_to_variant(&mut runtime, &value, true) else {
            panic!("PSB root must materialize as an object");
        };
        assert!(matches!(
            runtime.object_member(root, "optional"),
            Variant::Void
        ));
        let Variant::Object(nested) = runtime.object_member(root, "nested") else {
            panic!("nested PSB map must materialize as an object");
        };
        assert!(
            runtime
                .object_class_infos(nested)
                .iter()
                .any(|name| name == "Dictionary")
        );
        // A Dictionary instance carries no members of its own: every Dictionary
        // method is registered with `TJS_STATICMEMBER` on the class object
        // (`tjsDictionary.cpp:41-219`) and `tTJSNativeClass::FuncCall` copies
        // only non-static members onto an instance (`tjsNative.cpp:340-364`),
        // so the class name asserted above is what marks this map as a
        // Dictionary and `assign` reads void (the official manual says it
        // outright: 「作成された状態ではメンバを何一つ持っていません」).
        assert!(matches!(
            runtime.object_member(nested, "assign"),
            Variant::Void
        ));
        assert_eq!(
            runtime.object_member(nested, "answer"),
            Variant::Integer(42)
        );
    }
}
