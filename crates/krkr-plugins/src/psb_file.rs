//! Compatibility implementation for wamsoft `psbfile.dll`
//! (`PSBFile` / `PSBValueClass`).
//!
//! PSB is M2's compact, typed object format.  The game uses `.ks.scn` PSB
//! documents for compiled scenarios, so reporting a successful load with an
//! empty root is observably different from the original plugin: the scenario
//! dispatcher sees no tags and immediately returns to title.  This module
//! decodes the standard PSB object tree and exposes its maps as TJS
//! dictionaries, arrays as TJS arrays, and resource values as TJS octets.

use std::{collections::BTreeMap, str};

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

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
            let root = match PsbDocument::parse(&data) {
                Ok(root) => root,
                Err(error) => {
                    runtime
                        .host_mut()
                        .log(&format!("psbfile: failed to parse `{storage}`: {error}"));
                    return false;
                }
            };
            let root = psb_value_to_variant(runtime, &root, true);
            runtime.set_object_member(this, "root", root);
            true
        }
        Err(_) => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum PsbValue {
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

/// Standard PSB v2/v3 reader.  Its layout follows the public M2 PSB format
/// implementation in number201724/psbfile: the name trie is represented by
/// three packed arrays, and collections/dictionaries store relative offsets
/// to their child values.
struct PsbDocument<'a> {
    bytes: &'a [u8],
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
    fn parse(bytes: &'a [u8]) -> std::result::Result<PsbValue, String> {
        if bytes.len() < 0x28 || &bytes[..4] != b"PSB\0" {
            return Err("not a PSB document".to_string());
        }
        let version = read_u16(bytes, 4)?;
        if !(2..=3).contains(&version) {
            return Err(format!("unsupported PSB version {version}"));
        }
        // PSB v2/v3 reserves the first u32 of this block for encryption;
        // offsets begin at byte 0x0c.  Encrypted PSB needs its game key and is
        // deliberately reported as a failed load rather than decoded as junk.
        if read_u16(bytes, 6)? != 0 {
            return Err("encrypted PSB is unsupported".to_string());
        }
        let names_offset = read_u32(bytes, 0x0c)? as usize;
        let strings_offset = read_u32(bytes, 0x10)? as usize;
        let strings_data_offset = read_u32(bytes, 0x14)? as usize;
        let chunk_offsets_offset = read_u32(bytes, 0x18)? as usize;
        let chunk_lengths_offset = read_u32(bytes, 0x1c)? as usize;
        let chunk_data_offset = read_u32(bytes, 0x20)? as usize;
        let root_offset = read_u32(bytes, 0x24)? as usize;
        let mut document = Self {
            bytes,
            names_offset,
            strings_offset,
            strings_data_offset,
            chunk_offsets_offset,
            chunk_lengths_offset,
            chunk_data_offset,
            root_offset,
            names: BTreeMap::new(),
        };
        document.validate_offsets()?;
        document.names = document.decode_names()?;
        document.decode_value(document.root_offset, 0)
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
        if !(0x0d..=0x14).contains(&size_kind) {
            return Err("invalid PSB packed-array element width".to_string());
        }
        let entry_size = (size_kind - 0x0c) as usize;
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
        assert!(!matches!(
            runtime.object_member(nested, "assign"),
            Variant::Void
        ));
        assert_eq!(
            runtime.object_member(nested, "answer"),
            Variant::Integer(42)
        );
    }
}
