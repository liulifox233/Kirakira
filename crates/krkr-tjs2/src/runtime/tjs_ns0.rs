use std::collections::BTreeSet;

use crate::{Result, TjsError, runtime::Runtime};

use super::{ObjectHandle, Variant};

/// The 16-byte header every `TJS/` container carries, ahead of the IV bytes
/// and the body: 0-3 `TJS/` (little-endian) or `TJS\` (big-endian), 4 the
/// compress id (`n` store / `4` raw-block LZ4) followed by `s0\0`, 8-11 the
/// seed, 12-13 the crypt mode, 14-15 the IV byte length.
pub const NS0_HEADER_SIZE: usize = 16;
/// The little-endian magic; the big-endian variant spells a backslash
/// (`PackinOne.dll` `FUN_1004e0d0`, `.rdata 0x1007b520`/`0x1007b528`).
pub const NS0_MAGIC_LE: &[u8; 4] = b"TJS/";
pub const NS0_MAGIC_BE: &[u8; 4] = b"TJS\\";
/// The compress id byte at header offset 4 and the full 4-byte marks behind
/// it (`.rdata 0x1007b51c` = `"n4"`).
pub const NS0_COMPRESS_STORE: u8 = b'n';
pub const NS0_COMPRESS_LZ4: u8 = b'4';
pub const NS0_STORE_MARK: &[u8; 4] = b"ns0\0";
pub const NS0_LZ4_MARK: &[u8; 4] = b"4s0\0";
/// The seed the writer substitutes when the digest leaves it zero: the `TJS`
/// bytes read as a little-endian `u32` (`FUN_1004e0d0`).
pub const NS0_DEFAULT_SEED: u32 = 0x0053_4A54;

/// The parsed 16-byte container header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ns0Header {
    pub big_endian: bool,
    pub compress: u8,
    pub seed: u32,
    pub cryptmode: u16,
    pub iv_length: u16,
}

/// Parses the fixed header of a `TJS/` data pack. The body itself (IV,
/// transform chain, serialization) is the caller's business: this only pins
/// what the fields mean.
pub fn parse_ns0_header(bytes: &[u8]) -> Result<Ns0Header> {
    if bytes.len() < NS0_HEADER_SIZE {
        return Err(TjsError::runtime("TJS/ns0 file too short"));
    }
    let big_endian = if &bytes[0..4] == NS0_MAGIC_LE {
        false
    } else if &bytes[0..4] == NS0_MAGIC_BE {
        true
    } else {
        return Err(TjsError::runtime("TJS/ns0 bad magic"));
    };
    let compress = bytes[4];
    let mark = &bytes[4..8];
    if mark != NS0_STORE_MARK && mark != NS0_LZ4_MARK {
        // `PackinOne.dll` `FUN_1004e0d0` raises this for an id outside the
        // `"n4"` table.
        return Err(TjsError::runtime(format!(
            "unknown compress method {compress:#04x}"
        )));
    }
    let read_u16 = |offset: usize| {
        let pair = [bytes[offset], bytes[offset + 1]];
        if big_endian {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        }
    };
    let read_u32 = |offset: usize| {
        let word = [
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ];
        if big_endian {
            u32::from_be_bytes(word)
        } else {
            u32::from_le_bytes(word)
        }
    };
    Ok(Ns0Header {
        big_endian,
        compress,
        seed: read_u32(8),
        cryptmode: read_u16(12),
        iv_length: read_u16(14),
    })
}

pub(crate) fn decode_tjs_ns0<H: super::TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    bytes: &[u8],
) -> Result<Variant> {
    let header = parse_ns0_header(bytes)?;
    if header.cryptmode != 0 {
        return Err(TjsError::runtime("encrypted TJS/ns0 is not supported"));
    }
    if header.iv_length != 0 {
        return Err(TjsError::runtime("TJS/ns0 with IV is not supported"));
    }
    if header.compress != NS0_COMPRESS_STORE {
        return Err(TjsError::runtime("LZ4-compressed TJS/4s0 is not supported"));
    }
    let start = NS0_HEADER_SIZE + header.iv_length as usize;
    let payload = bytes
        .get(start..)
        .ok_or_else(|| TjsError::runtime("TJS/ns0 payload too short"))?;
    decode_tjs_ns0_body(runtime, payload, header.seed, header.big_endian)
}

/// Decodes a serialized `TJS/ns0` body — the value stream plus its trailing
/// 4-byte final check — with the header's seed and byte order. The caller
/// owns the transform chain (IV, decompression, decryption) in front of it.
pub fn decode_tjs_ns0_body<H: super::TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    payload: &[u8],
    seed: u32,
    big_endian: bool,
) -> Result<Variant> {
    if payload.len() < 4 {
        return Err(TjsError::runtime("TJS/ns0 payload too short"));
    }

    let mut decoder = TjsNs0Decoder {
        runtime,
        bytes: &payload[..payload.len() - 4],
        index: 0,
        big_endian,
        checker: ByteChecker::new(seed),
    };
    let value = decoder.value()?;
    let expected = decoder.checker.final_check();
    let check = &payload[payload.len() - 4..];
    let digits = [check[0], check[1], check[2], check[3]];
    let actual = if big_endian {
        u32::from_be_bytes(digits)
    } else {
        u32::from_le_bytes(digits)
    };
    if expected != actual {
        return Err(TjsError::runtime(format!(
            "TJS/ns0 checksum mismatch: expected {expected:08X}, got {actual:08X}"
        )));
    }
    Ok(value)
}

/// Serializes `value` as a `TJS/ns0` body with the header's seed and byte
/// order, appending the trailing final check. The serializer mirrors the
/// reference writer (`PackinOne.dll` `FUN_10052b10`): a one-byte type tag
/// followed by the checker byte for that tag, UTF-16 string units little-
/// endian (the reference writes the `tjs_char` array verbatim even in the
/// big-endian variant, `FUN_10050410`), u32 counts, an i64 for every integer
/// and an f64 for every real.
pub fn encode_tjs_ns0_body<H: super::TjsHost + 'static>(
    runtime: &Runtime<H>,
    value: &Variant,
    seed: u32,
    big_endian: bool,
) -> Result<Vec<u8>> {
    let mut encoder = TjsNs0Encoder {
        runtime,
        big_endian,
        checker: ByteChecker::new(seed),
        active: BTreeSet::new(),
    };
    let mut bytes = Vec::new();
    encoder.value(value, &mut bytes)?;
    let check = encoder.checker.final_check();
    put_u32(&mut bytes, check, big_endian);
    Ok(bytes)
}

/// The wire tags of `FUN_10052b10`/`FUN_100514c0`: the tag byte is followed by
/// one checker byte. `0x01` is a null object closure, `0x03` an octet.
const NS0_TAG_VOID: u8 = 0x00;
const NS0_TAG_NULL: u8 = 0x01;
const NS0_TAG_STRING: u8 = 0x02;
const NS0_TAG_OCTET: u8 = 0x03;
const NS0_TAG_INTEGER: u8 = 0x04;
const NS0_TAG_REAL: u8 = 0x05;
const NS0_TAG_ARRAY: u8 = 0x81;
const NS0_TAG_DICTIONARY: u8 = 0xC1;

/// The seeded per-value byte checker (`DefaultTypeByteChecker`).
///
/// The reference does not load the header seed into its four state bytes
/// verbatim: the writer and the reader both lay them out as
/// `[s0 ^ s3, s1, s2, 0]` (`FUN_100532e0`/`FUN_10053860`: every other seed byte
/// is copied through, so the round below — which only reads bytes 0 and 2 —
/// depends on the seed's top byte). The final check is the state after three
/// rounds and a 0<->2 swap, written (and compared) as a zero-extended 4-byte
/// value: the reader tests the file's trailing word against exactly three
/// bytes, so the top byte of a valid check is always zero. The 124 shipped
/// `.pbd` files all seed `0x00534A54` (top byte zero), which is why the fold
/// and the zeroed top byte are only visible for digest-derived seeds.
struct ByteChecker {
    state: [u8; 4],
}

impl ByteChecker {
    fn new(seed: u32) -> Self {
        let bytes = seed.to_le_bytes();
        Self {
            state: [bytes[0] ^ bytes[3], bytes[1], bytes[2], 0],
        }
    }

    fn round(seed: &mut [u8; 4]) {
        let a = seed[0] ^ seed[0].wrapping_mul(2);
        let mut b = a;
        b >>= 2;
        b ^= seed[2];
        b >>= 3;
        b ^= seed[2];
        b ^= a;
        seed[0] = seed[1];
        seed[1] = seed[2];
        seed[2] = b;
    }

    fn get_seed(&mut self, type_code: u8) -> u8 {
        if type_code == 0 {
            return self.state[2];
        }
        Self::round(&mut self.state);
        self.state[2]
    }

    fn final_check(&mut self) -> u32 {
        let mut state = self.state;
        Self::round(&mut state);
        Self::round(&mut state);
        Self::round(&mut state);
        state.swap(0, 2);
        u32::from_le_bytes(state)
    }
}

struct TjsNs0Decoder<'a, H: super::TjsHost> {
    runtime: &'a mut Runtime<H>,
    bytes: &'a [u8],
    index: usize,
    big_endian: bool,
    checker: ByteChecker,
}

impl<'a, H: super::TjsHost + 'static> TjsNs0Decoder<'a, H> {
    fn value(&mut self) -> Result<Variant> {
        // The tag word is `type | check << 8` in host order: little-endian
        // files store `[type, check]`, big-endian ones byte-swap the word to
        // `[check, type]` (the reference swaps every multi-byte field when its
        // big-endian flag is set), and reading it back in the same order puts
        // the type byte in the low half either way.
        let typ = self.read_u16()?;
        let (type_byte, check_byte) = ((typ & 0xff) as u8, (typ >> 8) as u8);
        let expected = self.checker.get_seed(type_byte);
        if check_byte != expected {
            return Err(TjsError::runtime(format!(
                "TJS/ns0 byte check failed: expected {expected}, got {check_byte}"
            )));
        }
        match type_byte {
            NS0_TAG_VOID => Ok(Variant::Void),
            NS0_TAG_NULL => Ok(Variant::Null),
            NS0_TAG_STRING => Ok(Variant::String(self.read_string()?)),
            NS0_TAG_OCTET => {
                let len = self.read_u32()? as usize;
                Ok(Variant::Octet(self.read_bytes(len)?.to_vec()))
            }
            NS0_TAG_INTEGER => Ok(Variant::Integer(self.read_i64()?)),
            NS0_TAG_REAL => Ok(Variant::Real(self.read_f64()?)),
            NS0_TAG_ARRAY => self.array(),
            NS0_TAG_DICTIONARY => self.dictionary(),
            _ => Err(TjsError::runtime(format!(
                "unsupported TJS/ns0 value type: {type_byte:#04X}"
            ))),
        }
    }

    fn array(&mut self) -> Result<Variant> {
        let len = self.read_u32()? as usize;
        let handle = self.runtime.alloc_array_object(Vec::new());
        for _ in 0..len {
            let value = self.value()?;
            self.runtime.heap[handle.0].array_push(value);
        }
        Ok(Variant::Object(handle))
    }

    fn dictionary(&mut self) -> Result<Variant> {
        let len = self.read_u32()? as usize;
        // The tagged dictionary is a TJS Dictionary and has to carry the
        // class, like the structs of the other pack format
        // (`BinaryStructDecoder::dictionary`, `builtins.rs:1765`): a miss
        // answers void only for a Dictionary *instance* (`tjsDictionary.cpp:
        // 720-731`), and KAGEX's bookmark code reads the members a save does
        // not hold with exactly that idiom (`MainConductor.restore`'s
        // `dic.runLine`, `LineModeEx.onRestore`'s `dic.language`).  A plain
        // object raises `Member "%1" does not exist` on the first such read.
        let handle = self.runtime.alloc_dictionary_object_sized(len as i64);
        for _ in 0..len {
            let key = self.read_string()?;
            let value = self.value()?;
            self.runtime.heap[handle.0].set(key, value);
        }
        Ok(Variant::Object(handle))
    }

    fn read_string(&mut self) -> Result<String> {
        let len = self.read_u32()? as usize;
        let byte_len = len
            .checked_mul(2)
            .ok_or_else(|| TjsError::runtime("TJS/ns0 string is too large"))?;
        let bytes = self.read_bytes(byte_len)?;
        let units = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        Ok(String::from_utf16_lossy(&units))
    }

    fn read_u16(&mut self) -> Result<u16> {
        let bytes = self.read_bytes(2)?;
        let pair = [bytes[0], bytes[1]];
        Ok(if self.big_endian {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    }

    fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.read_bytes(4)?;
        let word = [bytes[0], bytes[1], bytes[2], bytes[3]];
        Ok(if self.big_endian {
            u32::from_be_bytes(word)
        } else {
            u32::from_le_bytes(word)
        })
    }

    fn read_i64(&mut self) -> Result<i64> {
        let bytes = self.read_bytes(8)?;
        let digits = [
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ];
        Ok(if self.big_endian {
            i64::from_be_bytes(digits)
        } else {
            i64::from_le_bytes(digits)
        })
    }

    fn read_f64(&mut self) -> Result<f64> {
        let bytes = self.read_bytes(8)?;
        let digits = [
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ];
        Ok(if self.big_endian {
            f64::from_bits(u64::from_be_bytes(digits))
        } else {
            f64::from_bits(u64::from_le_bytes(digits))
        })
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .index
            .checked_add(len)
            .ok_or_else(|| TjsError::runtime("TJS/ns0 index overflow"))?;
        if end > self.bytes.len() {
            return Err(TjsError::runtime("truncated TJS/ns0 data"));
        }
        let bytes = &self.bytes[self.index..end];
        self.index = end;
        Ok(bytes)
    }
}

struct TjsNs0Encoder<'a, H: super::TjsHost> {
    runtime: &'a Runtime<H>,
    big_endian: bool,
    checker: ByteChecker,
    active: BTreeSet<ObjectHandle>,
}

impl<'a, H: super::TjsHost + 'static> TjsNs0Encoder<'a, H> {
    /// Writes one value's tag word: the type byte plus the checker byte the
    /// same seed produces for it (`FUN_100514c0` writes them as two bytes).
    fn tag(&mut self, tag: u8, out: &mut Vec<u8>) {
        let check = self.checker.get_seed(tag);
        let word = u16::from(tag) | u16::from(check) << 8;
        put_u16(out, word, self.big_endian);
    }

    fn value(&mut self, value: &Variant, out: &mut Vec<u8>) -> Result<()> {
        match value {
            // TJS2 folds `null` and `void` onto one type; the reference's
            // null *object* closure is the one that gets its own `0x01` tag.
            Variant::Void | Variant::Null => self.tag(NS0_TAG_VOID, out),
            Variant::String(value) => {
                self.tag(NS0_TAG_STRING, out);
                put_string(out, value, self.big_endian)?;
            }
            Variant::Octet(value) => {
                self.tag(NS0_TAG_OCTET, out);
                put_u32(out, value.len() as u32, self.big_endian);
                out.extend_from_slice(value);
            }
            Variant::Integer(value) => {
                self.tag(NS0_TAG_INTEGER, out);
                put_i64(out, *value, self.big_endian);
            }
            Variant::Real(value) => {
                self.tag(NS0_TAG_REAL, out);
                let digits = value.to_bits();
                if self.big_endian {
                    out.extend_from_slice(&digits.to_be_bytes());
                } else {
                    out.extend_from_slice(&digits.to_le_bytes());
                }
            }
            Variant::Object(handle) => self.object(*handle, out)?,
            Variant::Closure(closure) => {
                self.object(closure.this_obj.unwrap_or(closure.object), out)?
            }
            Variant::CodeObject(_) => self.tag(NS0_TAG_NULL, out),
        }
        Ok(())
    }

    fn object(&mut self, handle: ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
        if !self.active.insert(handle) {
            self.tag(NS0_TAG_NULL, out);
            return Ok(());
        }
        if let Some(elements) = self.runtime.array_elements(handle).map(Vec::from) {
            self.tag(NS0_TAG_ARRAY, out);
            put_u32(out, elements.len() as u32, self.big_endian);
            for value in elements {
                self.value(&value, out)?;
            }
        } else {
            let class_infos = self.runtime.object_class_infos(handle);
            // The reference writes every non-array closure as a dictionary
            // (`FUN_10052b10` checks only `Array`). A classed engine object (a
            // Layer, the KAG object, ...) has no portable representation, so
            // it degrades to the writer's null-object tag; a Dictionary -- the
            // shape the decoder builds for a tagged dictionary -- serializes
            // as one, and a class-less object counts as a plain dictionary.
            let is_dictionary =
                class_infos.is_empty() || class_infos.iter().any(|info| info == "Dictionary");
            if !is_dictionary {
                self.tag(NS0_TAG_NULL, out);
                self.active.remove(&handle);
                return Ok(());
            }
            self.tag(NS0_TAG_DICTIONARY, out);
            let entries = self.runtime.object_members(handle);
            put_u32(out, entries.len() as u32, self.big_endian);
            for (key, value) in entries {
                put_string(out, &key, self.big_endian)?;
                self.value(&value, out)?;
            }
        }
        self.active.remove(&handle);
        Ok(())
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16, big_endian: bool) {
    if big_endian {
        out.extend_from_slice(&value.to_be_bytes());
    } else {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32, big_endian: bool) {
    if big_endian {
        out.extend_from_slice(&value.to_be_bytes());
    } else {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn put_i64(out: &mut Vec<u8>, value: i64, big_endian: bool) {
    if big_endian {
        out.extend_from_slice(&value.to_be_bytes());
    } else {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

/// A `TJS/ns0` string: u32 count of UTF-16 units, then the units themselves.
/// The reference writes the `tjs_char` array verbatim (`FUN_10050410`), so the
/// units stay little-endian even under the big-endian flag.
fn put_string(out: &mut Vec<u8>, value: &str, big_endian: bool) -> Result<()> {
    let units = value.encode_utf16().collect::<Vec<_>>();
    let len =
        u32::try_from(units.len()).map_err(|_| TjsError::runtime("TJS/ns0 string is too large"))?;
    put_u32(out, len, big_endian);
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::runtime::Runtime;

    use super::*;

    fn plain_container(body: &[u8], seed: u32, big_endian: bool) -> Vec<u8> {
        let mut bytes = if big_endian {
            NS0_MAGIC_BE.to_vec()
        } else {
            NS0_MAGIC_LE.to_vec()
        };
        bytes.extend_from_slice(NS0_STORE_MARK);
        bytes.extend_from_slice(&if big_endian {
            seed.to_be_bytes()
        } else {
            seed.to_le_bytes()
        });
        put_u16(&mut bytes, 0, big_endian); // cryptmode
        put_u16(&mut bytes, 0, big_endian); // IV length
        bytes.extend_from_slice(body);
        bytes
    }

    /// The hand-built `%["answer" => 42]` pack: a zero seed makes every
    /// byte-check and the final check zero, so the payload is one dictionary
    /// tag, its key, one integer and that zero final check.
    fn answer_pack() -> Vec<u8> {
        let mut bytes = b"TJS/ns0\0".to_vec();
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // seed
        bytes.extend_from_slice(&0_u16.to_le_bytes()); // crypt
        bytes.extend_from_slice(&0_u16.to_le_bytes()); // IV length
        bytes.extend_from_slice(&0x00c1_u16.to_le_bytes()); // Dictionary
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&6_u32.to_le_bytes());
        for unit in "answer".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&0x0004_u16.to_le_bytes()); // Integer
        bytes.extend_from_slice(&42_i64.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // final check
        bytes
    }

    #[test]
    fn decoded_dictionary_contains_only_packed_members() {
        let mut runtime = Runtime::new();
        let Variant::Object(dictionary) =
            decode_tjs_ns0(&mut runtime, &answer_pack()).expect("decode")
        else {
            panic!("expected dictionary");
        };

        assert_eq!(
            runtime.object_member(dictionary, "answer"),
            Variant::Integer(42)
        );
        // The raw member map holds the packed keys only: the class a Dictionary
        // instance carries registers no method members either
        // (`alloc_dictionary_object`).
        assert!(matches!(
            runtime.object_member(dictionary, "assign"),
            Variant::Void
        ));
        assert!(matches!(
            runtime.object_member(dictionary, "clear"),
            Variant::Void
        ));
    }

    /// A decoded dictionary is a Dictionary *instance*, so the miss protocol
    /// comes with it: a member the save does not hold reads as void through the
    /// VM's own read path (`missing_member`'s Dictionary branch,
    /// `tjsDictionary.cpp:720-731`) -- the `dic.runLine === void` idiom KAGEX's
    /// bookmark code depends on (`MainConductor.restore`, `MainWindow.tjs`),
    /// not `Member "runLine" does not exist`.
    #[test]
    fn decoded_dictionary_reads_an_unset_member_as_void() {
        let mut runtime = Runtime::new();
        let Variant::Object(dictionary) =
            decode_tjs_ns0(&mut runtime, &answer_pack()).expect("decode")
        else {
            panic!("expected dictionary");
        };
        assert!(runtime.is_dictionary_instance(dictionary));

        runtime.set_global_member("decoded", Variant::Object(dictionary));
        let file =
            crate::compile_source_to_bytecode("decoded-probe.tjs", "return decoded.runLine;")
                .expect("compile");
        assert_eq!(
            runtime
                .execute_file(&file)
                .expect("an unset member of a decoded dictionary reads as void"),
            Variant::Void
        );
    }

    /// The exact bytes the writer emits for the shape the existing test
    /// decodes: one tag+check pair per value, a raw key string, an i64 and the
    /// zero final check the zero seed produces.
    #[test]
    fn encoder_matches_the_hand_built_dictionary_bytes() {
        let mut runtime = Runtime::new();
        let handle = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(handle, "Dictionary");
        runtime.heap[handle.0].set("answer".to_string(), Variant::Integer(42));
        let body =
            encode_tjs_ns0_body(&runtime, &Variant::Object(handle), 0, false).expect("encode");

        let mut expected = Vec::new();
        expected.extend_from_slice(&0x00c1_u16.to_le_bytes());
        expected.extend_from_slice(&1_u32.to_le_bytes());
        expected.extend_from_slice(&6_u32.to_le_bytes());
        for unit in "answer".encode_utf16() {
            expected.extend_from_slice(&unit.to_le_bytes());
        }
        expected.extend_from_slice(&0x0004_u16.to_le_bytes());
        expected.extend_from_slice(&42_i64.to_le_bytes());
        expected.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(body, expected);
    }

    /// A save-shaped value: nested dictionaries, an array with mixed types,
    /// a string, an octet and a null.
    fn sample_value(runtime: &mut Runtime) -> ObjectHandle {
        let root = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(root, "Dictionary");
        runtime.heap[root.0].set("id".to_string(), Variant::String("slot".to_string()));
        let core = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(core, "Dictionary");
        runtime.heap[core.0].set("storeTime".to_string(), Variant::Integer(1_234_567_890));
        runtime.heap[core.0].set("real".to_string(), Variant::Real(1.5));
        runtime.heap[core.0].set("neg".to_string(), Variant::Integer(-7));
        runtime.heap[root.0].set("core".to_string(), Variant::Object(core));
        let list = runtime.alloc_array_object(vec![
            Variant::Integer(1),
            Variant::String("two".to_string()),
            Variant::Octet(vec![1, 2]),
            Variant::Null,
        ]);
        runtime.heap[root.0].set("list".to_string(), Variant::Object(list));
        let nested = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(nested, "Dictionary");
        runtime.heap[nested.0].set("deep".to_string(), Variant::Integer(3));
        runtime.heap[root.0].set("nested".to_string(), Variant::Object(nested));
        root
    }

    fn assert_sample(runtime: &Runtime, root: ObjectHandle) {
        assert_eq!(
            runtime.object_member(root, "id"),
            Variant::String("slot".to_string())
        );
        let Variant::Object(core) = runtime.object_member(root, "core") else {
            panic!("expected core");
        };
        assert_eq!(
            runtime.object_member(core, "storeTime"),
            Variant::Integer(1_234_567_890)
        );
        assert_eq!(runtime.object_member(core, "real"), Variant::Real(1.5));
        assert_eq!(runtime.object_member(core, "neg"), Variant::Integer(-7));
        let Variant::Object(list) = runtime.object_member(root, "list") else {
            panic!("expected list");
        };
        assert_eq!(
            runtime.array_elements(list),
            Some(
                [
                    Variant::Integer(1),
                    Variant::String("two".to_string()),
                    Variant::Octet(vec![1, 2]),
                    // TJS folds `null` onto `void`: the wire has no separate
                    // null value, so it reads back as void.
                    Variant::Void,
                ]
                .as_slice()
            )
        );
        let Variant::Object(nested) = runtime.object_member(root, "nested") else {
            panic!("expected nested");
        };
        assert_eq!(runtime.object_member(nested, "deep"), Variant::Integer(3));
    }

    /// A flat save-shaped dictionary (no nested dictionaries) whose member
    /// walk is stable enough for byte-level re-encoding.
    fn flat_value(runtime: &mut Runtime) -> ObjectHandle {
        let root = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(root, "Dictionary");
        runtime.heap[root.0].set("id".to_string(), Variant::String("slot".to_string()));
        runtime.heap[root.0].set("count".to_string(), Variant::Integer(-3));
        runtime.heap[root.0].set("ratio".to_string(), Variant::Real(0.5));
        runtime.heap[root.0].set("blob".to_string(), Variant::Octet(vec![9, 8, 7]));
        let list = runtime.alloc_array_object(vec![
            Variant::Integer(1),
            Variant::String("two".to_string()),
        ]);
        runtime.heap[root.0].set("list".to_string(), Variant::Object(list));
        root
    }

    #[test]
    fn encoded_bodies_round_trip_through_the_decoder() {
        let mut runtime = Runtime::new();
        let root = sample_value(&mut runtime);
        for seed in [0_u32, NS0_DEFAULT_SEED, 0xDEAD_BEEF, u32::MAX] {
            let body =
                encode_tjs_ns0_body(&runtime, &Variant::Object(root), seed, false).expect("encode");
            let decoded = decode_tjs_ns0_body(&mut runtime, &body, seed, false).expect("decode");
            let Variant::Object(decoded) = decoded else {
                panic!("expected a dictionary");
            };
            assert_sample(&runtime, decoded);
        }

        // A decoded *flat* dictionary re-encodes byte for byte: the decoder
        // restores the Dictionary class the writer keys off, so the decoded
        // value is already the shape that serializes as a dictionary tag.
        let flat = flat_value(&mut runtime);
        let body = encode_tjs_ns0_body(&runtime, &Variant::Object(flat), 0x1234_5678, false)
            .expect("encode");
        let Variant::Object(decoded) =
            decode_tjs_ns0_body(&mut runtime, &body, 0x1234_5678, false).expect("decode")
        else {
            panic!("expected a dictionary");
        };
        assert_eq!(
            runtime.object_member(decoded, "count"),
            Variant::Integer(-3)
        );
        let again = encode_tjs_ns0_body(&runtime, &Variant::Object(decoded), 0x1234_5678, false)
            .expect("re-encode");
        assert_eq!(body, again);
    }

    /// The checker's initial state folds the seed's top byte into byte 0, and
    /// the final check's top byte is always zero — the writer's
    /// `[s0 ^ s3, s1, s2]` assembly (`FUN_100532e0`) and the reader's
    /// zero-extended comparison (`FUN_10053860`) agree on both. A seed whose
    /// top byte is zero (every shipped `.pbd` uses `0x00534A54`) makes the
    /// fold a no-op, which is why the corpus cannot see it.
    #[test]
    fn checker_folds_the_top_seed_byte_and_zeroes_the_final_check() {
        let mut folded = ByteChecker::new(0x1234_5678);
        let mut naive = ByteChecker::new(0x0034_5678);
        assert_ne!(folded.get_seed(0xC1), naive.get_seed(0xC1));
        assert_eq!(
            ByteChecker::new(NS0_DEFAULT_SEED).state,
            [0x54, 0x4A, 0x53, 0]
        );
        let check = ByteChecker::new(0x1234_5678).final_check();
        assert_eq!(check >> 24, 0, "the final check's top byte is zero");
    }

    #[test]
    fn corrupted_check_bytes_and_trailing_checks_are_rejected() {
        let mut runtime = Runtime::new();
        let root = sample_value(&mut runtime);
        let body = encode_tjs_ns0_body(&runtime, &Variant::Object(root), 7, false).expect("encode");

        let mut flipped = body.clone();
        flipped[1] ^= 0x01;
        assert!(decode_tjs_ns0_body(&mut runtime, &flipped, 7, false).is_err());

        let mut retagged = body.clone();
        retagged[0] = 0x00; // Void keeps the checker still, so the check byte sticks out
        assert!(decode_tjs_ns0_body(&mut runtime, &retagged, 7, false).is_err());

        let mut trailing = body.clone();
        let last = trailing.len() - 1;
        trailing[last] ^= 0xFF;
        assert!(decode_tjs_ns0_body(&mut runtime, &trailing, 7, false).is_err());
    }

    #[test]
    fn big_endian_bodies_round_trip_and_swapped_seeds_fail() {
        let mut runtime = Runtime::new();
        let root = sample_value(&mut runtime);
        let body = encode_tjs_ns0_body(&runtime, &Variant::Object(root), 0x1122_3344, true)
            .expect("encode");
        // The dictionary tag sits in the low byte even though the word is
        // stored big-endian: `[check, tag]`.
        assert_eq!(body[1], NS0_TAG_DICTIONARY);
        let decoded = decode_tjs_ns0_body(&mut runtime, &body, 0x1122_3344, true).expect("decode");
        let Variant::Object(decoded) = decoded else {
            panic!("expected dictionary");
        };
        assert_sample(&runtime, decoded);
        assert!(decode_tjs_ns0_body(&mut runtime, &body, 0x1122_3300, true).is_err());

        // A big-endian container decodes through the full header path, and a
        // little-endian body relabelled as big-endian does not.
        let container = plain_container(&body, 0x1122_3344, true);
        assert!(parse_ns0_header(&container).expect("header").big_endian);
        let Variant::Object(decoded) = decode_tjs_ns0(&mut runtime, &container).expect("decode")
        else {
            panic!("expected dictionary");
        };
        assert_sample(&runtime, decoded);
        let little = encode_tjs_ns0_body(&runtime, &Variant::Object(root), 0x1122_3344, false)
            .expect("encode");
        let container = plain_container(&little, 0x1122_3344, true);
        assert!(decode_tjs_ns0(&mut runtime, &container).is_err());
    }

    /// Order-insensitive value fingerprint: the engine's member table walks
    /// its hash buckets, so two decoded copies of the same dictionary may
    /// enumerate differently while carrying identical data.
    fn fingerprint(runtime: &Runtime, value: &Variant) -> String {
        match value {
            Variant::Void | Variant::Null => "v".to_string(),
            Variant::Integer(value) => format!("i{value}"),
            Variant::Real(value) => format!("f{value}"),
            Variant::String(value) => format!("s{}:{value}", value.chars().count()),
            Variant::Octet(value) => format!("o{}", value.len()),
            Variant::Object(handle) => {
                if let Some(elements) = runtime.array_elements(*handle) {
                    let parts: Vec<String> = elements
                        .iter()
                        .map(|element| fingerprint(runtime, element))
                        .collect();
                    format!("a[{}]", parts.join(","))
                } else {
                    let mut parts: Vec<String> = runtime
                        .object_members(*handle)
                        .iter()
                        .map(|(key, member)| format!("{key}={}", fingerprint(runtime, member)))
                        .collect();
                    parts.sort();
                    format!("d{{{}}}", parts.join(","))
                }
            }
            Variant::Closure(_) | Variant::CodeObject(_) => "c".to_string(),
        }
    }

    /// Session verification: decode every real reference `.pbd` handed over
    /// via `KRKR_NS0_CORPUS` (a directory of extracted files), re-encode the
    /// decoded value with the file's own seed, decode that again and compare
    /// the two values. Returns silently without the environment variable so
    /// the checked-in suite stays self-contained.
    #[test]
    fn real_reference_corpus_decodes_and_round_trips() {
        let Ok(directory) = std::env::var("KRKR_NS0_CORPUS") else {
            return;
        };
        let mut runtime = Runtime::new();
        let mut files = 0usize;
        let mut entries = std::fs::read_dir(&directory)
            .expect("corpus directory")
            .collect::<std::io::Result<Vec<_>>>()
            .expect("corpus entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("pbd") {
                continue;
            }
            let bytes = std::fs::read(&path).expect("corpus file");
            let header = parse_ns0_header(&bytes).expect("container header");
            assert_eq!(header.compress, NS0_COMPRESS_STORE, "{path:?}");
            assert_eq!(header.cryptmode, 0, "{path:?}");
            let value = decode_tjs_ns0(&mut runtime, &bytes)
                .unwrap_or_else(|error| panic!("decode {path:?}: {error}"));
            let Variant::Object(root) = value else {
                files += 1;
                continue;
            };
            // No class-info fix-up: the encoder's dictionary rule has to cope
            // with the decoder's plain objects on its own.
            let body = encode_tjs_ns0_body(&runtime, &Variant::Object(root), header.seed, false)
                .unwrap_or_else(|error| panic!("encode {path:?}: {error}"));
            let Variant::Object(again) =
                decode_tjs_ns0_body(&mut runtime, &body, header.seed, false)
                    .unwrap_or_else(|error| panic!("re-decode {path:?}: {error}"))
            else {
                panic!("re-decode {path:?} lost the dictionary");
            };
            assert_eq!(
                fingerprint(&runtime, &Variant::Object(root)),
                fingerprint(&runtime, &Variant::Object(again)),
                "round-trip changed the value of {path:?}"
            );
            files += 1;
        }
        assert!(files > 100, "only {files} corpus files decoded");
        eprintln!("decoded and round-tripped {files} real .pbd files");
    }

    #[test]
    fn plain_container_round_trips_through_decode_tjs_ns0() {
        let mut runtime = Runtime::new();
        let handle = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(handle, "Dictionary");
        runtime.heap[handle.0].set("answer".to_string(), Variant::Integer(42));
        let body =
            encode_tjs_ns0_body(&runtime, &Variant::Object(handle), 7, false).expect("encode");
        let container = plain_container(&body, 7, false);
        let header = parse_ns0_header(&container).expect("header");
        assert_eq!(header.compress, NS0_COMPRESS_STORE);
        assert_eq!(header.cryptmode, 0);
        assert_eq!(header.iv_length, 0);
        let Variant::Object(decoded) = decode_tjs_ns0(&mut runtime, &container).expect("decode")
        else {
            panic!("expected dictionary");
        };
        assert_eq!(
            runtime.object_member(decoded, "answer"),
            Variant::Integer(42)
        );
    }
}
