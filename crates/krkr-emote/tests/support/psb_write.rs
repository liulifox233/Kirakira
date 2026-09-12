// A minimal PSB v3/v4 writer for the synthetic-motion tests: it writes just
// enough of the container eluna's reader needs — the fixed header, a
// prefix-trie name table, the string table, the resource offset and length
// arrays with their data pool, and the root value bytecode.
//
// Container layout reference: `vendor/eluna/crates/eluna/src/psb/mod.rs`.
//
// The block above is a plain comment rather than a module doc because
// `crates/krkr-plugins/src/motion_player.rs`'s tests include this file verbatim
// (the same writer must build both sides' containers), and an included file
// cannot carry inner doc comments.

// PSB container constants. eluna keeps these private; the writer mirrors the
// layout of `vendor/eluna/crates/eluna/src/psb/mod.rs`.
const PSB_SIGNATURE: u32 = 0x0042_5350;
const PSB_TYPE_NULL: u8 = 0x01;
const PSB_TYPE_INTEGER_N: u8 = 0x04;
const PSB_TYPE_INTEGER_ARRAY_N: u8 = 0x0c;
const PSB_TYPE_STRING_N: u8 = 0x14;
const PSB_TYPE_RESOURCE_N: u8 = 0x18;
const PSB_TYPE_FLOAT: u8 = 0x1e;
const PSB_TYPE_LIST: u8 = 0x20;
const PSB_TYPE_OBJECT: u8 = 0x21;

#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Int(i64),
    Float(f32),
    Str(&'static str),
    Resource(u32),
    List(Vec<Value>),
    Object(Vec<(&'static str, Value)>),
}

pub fn object(fields: Vec<(&'static str, Value)>) -> Value {
    Value::Object(fields)
}

pub fn list(items: Vec<Value>) -> Value {
    Value::List(items)
}

pub fn int(value: i64) -> Value {
    Value::Int(value)
}

pub fn text(value: &'static str) -> Value {
    Value::Str(value)
}

pub fn float(value: f32) -> Value {
    Value::Float(value)
}

/// A minimal PSB v3/v4 writer: header, name btree, strings, resources, root.
#[derive(Default)]
pub struct PsbWriter {
    names: Vec<String>,
    strings: Vec<String>,
    resources: Vec<Vec<u8>>,
}

impl PsbWriter {
    pub fn add_resource(&mut self, bytes: Vec<u8>) -> Value {
        self.resources.push(bytes);
        Value::Resource((self.resources.len() - 1) as u32)
    }

    pub fn finish(mut self, version: u16, root: &Value) -> Vec<u8> {
        let root_blob = self.encode(root);
        let name_blob = self.name_btree();
        let string_table = self.string_table();
        let pool = self.string_pool();
        let resource_offsets = self.resource_offsets();
        let resource_lengths = self.resource_lengths();
        let resource_pool = self.resource_pool();

        let header_size = 0x2c;
        let name_offset = header_size;
        let string_offset = name_offset + name_blob.len();
        let string_data_offset = string_offset + string_table.len();
        let resource_offset = string_data_offset + pool.len();
        let resource_length_offset = resource_offset + resource_offsets.len();
        let resource_data_offset = resource_length_offset + resource_lengths.len();
        let root_offset = resource_data_offset + resource_pool.len();

        let mut out = Vec::with_capacity(root_offset + root_blob.len());
        out.extend(PSB_SIGNATURE.to_le_bytes());
        out.extend(version.to_le_bytes());
        out.extend(0u16.to_le_bytes()); // flags: unencrypted
        out.extend((header_size as u32).to_le_bytes()); // header/encryption offset
        out.extend((name_offset as u32).to_le_bytes());
        out.extend((string_offset as u32).to_le_bytes());
        out.extend((string_data_offset as u32).to_le_bytes());
        out.extend((resource_offset as u32).to_le_bytes());
        out.extend((resource_length_offset as u32).to_le_bytes());
        out.extend((resource_data_offset as u32).to_le_bytes());
        out.extend((root_offset as u32).to_le_bytes());
        out.extend(0u32.to_le_bytes()); // v3 header checksum word
        debug_assert_eq!(out.len(), header_size);

        out.extend(name_blob);
        out.extend(string_table);
        out.extend(pool);
        out.extend(resource_offsets);
        out.extend(resource_lengths);
        out.extend(resource_pool);
        out.extend(root_blob);
        out
    }

    fn encode(&mut self, value: &Value) -> Vec<u8> {
        match value {
            Value::Null => vec![PSB_TYPE_NULL],
            Value::Int(value) => encode_int(*value),
            Value::Float(value) => {
                let mut out = vec![PSB_TYPE_FLOAT];
                out.extend(value.to_le_bytes());
                out
            }
            Value::Str(value) => {
                let index = intern(&mut self.strings, value);
                encode_indexed(PSB_TYPE_STRING_N, index as u64)
            }
            Value::Resource(index) => encode_indexed(PSB_TYPE_RESOURCE_N, u64::from(*index)),
            Value::List(items) => {
                let mut blobs = Vec::with_capacity(items.len());
                let mut offsets = Vec::with_capacity(items.len());
                let mut cursor = 0u64;
                for item in items {
                    let blob = self.encode(item);
                    offsets.push(cursor);
                    cursor += blob.len() as u64;
                    blobs.push(blob);
                }
                let mut out = vec![PSB_TYPE_LIST];
                out.extend(write_uint_array(&offsets));
                for blob in blobs {
                    out.extend(blob);
                }
                out
            }
            Value::Object(fields) => {
                let mut blobs = Vec::with_capacity(fields.len());
                let mut offsets = Vec::with_capacity(fields.len());
                let mut cursor = 0u64;
                for (_, value) in fields {
                    let blob = self.encode(value);
                    offsets.push(cursor);
                    cursor += blob.len() as u64;
                    blobs.push(blob);
                }
                let ids: Vec<u64> = fields
                    .iter()
                    .map(|(name, _)| intern(&mut self.names, name) as u64)
                    .collect();

                let mut out = vec![PSB_TYPE_OBJECT];
                out.extend(write_uint_array(&ids));
                out.extend(write_uint_array(&offsets));
                for blob in blobs {
                    out.extend(blob);
                }
                out
            }
        }
    }

    /// Prefix-trie name table: `name_offsets`, `tree`, `indexes`.
    ///
    /// Read back as: `id = tree[index]`, then repeatedly `id = tree[id]`, and
    /// each node contributes `id - offsets[tree[id]]` as one byte (last byte of
    /// the name first). A node's children therefore have id `base(node) + byte`
    /// with `base(node)` stored in `offsets[id(node)]`, the root level starts at
    /// `offset 0 = 1` (first bytes become `1 + byte`), and every name gets a
    /// private index slot whose `tree` value is its last-byte node.
    fn name_btree(&self) -> Vec<u8> {
        #[derive(Default)]
        struct Node {
            children: Vec<(u8, usize)>,
            base: u64,
        }

        let mut nodes = vec![Node::default()]; // 0: sentinel; base 1 = root level
        nodes[0].base = 1;
        let mut ends = Vec::with_capacity(self.names.len());
        for name in &self.names {
            let mut current = 0usize;
            for byte in name.bytes() {
                assert!(byte != 0, "names must not contain NUL");
                let existing = nodes[current]
                    .children
                    .iter()
                    .find_map(|(child_byte, child)| (*child_byte == byte).then_some(*child));
                current = match existing {
                    Some(child) => child,
                    None => {
                        let child = nodes.len();
                        nodes.push(Node::default());
                        nodes[current].children.push((byte, child));
                        child
                    }
                };
            }
            ends.push(current);
        }

        // Disjoint 256-wide id ranges, one per trie node (root level is 1..=256).
        let mut next_base = 257u64;
        for node in nodes.iter_mut().skip(1) {
            node.base = next_base;
            next_base += 256;
        }

        let mut ids = vec![0u64; nodes.len()];
        let mut max_id = 256u64;
        for node in &nodes {
            for (byte, child) in &node.children {
                let id = node.base + u64::from(*byte);
                ids[*child] = id;
                max_id = max_id.max(id);
            }
        }

        let mut tree = vec![0u64; max_id as usize + 1];
        let mut offsets = vec![0u64; max_id as usize + 1];
        offsets[0] = nodes[0].base;
        for (parent, node) in nodes.iter().enumerate() {
            for (_, child) in &node.children {
                let id = ids[*child] as usize;
                tree[id] = ids[parent];
                offsets[id] = nodes[*child].base;
            }
        }

        let mut indexes = Vec::with_capacity(self.names.len());
        for (slot, end) in (max_id + 1..).zip(ends) {
            grow(&mut tree, slot as usize);
            tree[slot as usize] = ids[end];
            indexes.push(slot);
        }

        let mut out = write_uint_array(&offsets);
        out.extend(write_uint_array(&tree));
        out.extend(write_uint_array(&indexes));
        out
    }

    fn string_table(&self) -> Vec<u8> {
        let mut offsets = Vec::with_capacity(self.strings.len());
        let mut cursor = 0u64;
        for value in &self.strings {
            offsets.push(cursor);
            cursor += value.len() as u64 + 1;
        }
        write_uint_array(&offsets)
    }

    fn string_pool(&self) -> Vec<u8> {
        let mut pool = Vec::new();
        for value in &self.strings {
            pool.extend(value.as_bytes());
            pool.push(0);
        }
        pool
    }

    fn resource_offsets(&self) -> Vec<u8> {
        let mut offsets = Vec::with_capacity(self.resources.len());
        let mut cursor = 0u64;
        for bytes in &self.resources {
            offsets.push(cursor);
            cursor += bytes.len() as u64;
        }
        write_uint_array(&offsets)
    }

    fn resource_lengths(&self) -> Vec<u8> {
        write_uint_array(
            &self
                .resources
                .iter()
                .map(|bytes| bytes.len() as u64)
                .collect::<Vec<u64>>(),
        )
    }

    fn resource_pool(&self) -> Vec<u8> {
        self.resources.concat()
    }
}

fn intern(table: &mut Vec<String>, value: &str) -> usize {
    match table.iter().position(|entry| entry == value) {
        Some(index) => index,
        None => {
            table.push(value.to_owned());
            table.len() - 1
        }
    }
}

fn grow(table: &mut Vec<u64>, index: usize) {
    if table.len() <= index {
        table.resize(index + 1, 0);
    }
}

/// Length-prefixed u64 array (`PSB_TYPE_INTEGER_ARRAY_N`); the reader requires
/// both prefix and item sizes between 1 and 8 bytes.
fn write_uint_array(values: &[u64]) -> Vec<u8> {
    let length = values.len() as u64;
    let length_size = minimal_size(length).max(1);
    let item_size = minimal_size(values.iter().copied().max().unwrap_or(0)).max(1);
    let mut out = vec![PSB_TYPE_INTEGER_ARRAY_N + length_size];
    out.extend(&length.to_le_bytes()[..length_size as usize]);
    out.push(PSB_TYPE_INTEGER_ARRAY_N + item_size);
    for value in values {
        out.extend(&value.to_le_bytes()[..item_size as usize]);
    }
    out
}

/// Signed integer value; the reader sign-extends the stored bytes.
fn encode_int(value: i64) -> Vec<u8> {
    let size = minimal_signed_size(value);
    let mut out = vec![PSB_TYPE_INTEGER_N + size];
    out.extend(&value.to_le_bytes()[..size as usize]);
    out
}

fn minimal_signed_size(value: i64) -> u8 {
    for size in 1..=8u8 {
        let bytes = value.to_le_bytes();
        let mut candidate = [0u8; 8];
        candidate[..size as usize].copy_from_slice(&bytes[..size as usize]);
        if bytes[size as usize - 1] & 0x80 != 0 {
            for byte in &mut candidate[size as usize..] {
                *byte = 0xff;
            }
        }
        if i64::from_le_bytes(candidate) == value {
            return size;
        }
    }
    8
}

/// String/resource reference: type tag plus the table index.
fn encode_indexed(base: u8, index: u64) -> Vec<u8> {
    let size = minimal_size(index).clamp(1, 4);
    let mut out = vec![base + size];
    out.extend(&index.to_le_bytes()[..size as usize]);
    out
}

fn minimal_size(value: u64) -> u8 {
    let mut size = 0u8;
    while size < 8 && (value >> (size * 8)) != 0 {
        size += 1;
    }
    size
}
