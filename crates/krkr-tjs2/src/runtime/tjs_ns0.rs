use crate::{Result, TjsError, runtime::Runtime};

use super::Variant;
use super::builtins::install_dictionary_methods;

const HEADER_SIZE: usize = 16;
const MAGIC: &[u8] = b"TJS/";
const CHECK_NS0: &[u8] = b"ns0\0";
const CHECK_4S0: &[u8] = b"4s0\0";

pub(crate) fn decode_tjs_ns0<H: super::TjsHost + 'static>(
    runtime: &mut Runtime<H>,
    bytes: &[u8],
) -> Result<Variant> {
    if bytes.len() < HEADER_SIZE {
        return Err(TjsError::runtime("TJS/ns0 file too short"));
    }
    if &bytes[0..4] != MAGIC {
        return Err(TjsError::runtime("TJS/ns0 bad magic"));
    }
    let check = &bytes[4..8];
    let seed = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let crypt = u16::from_le_bytes([bytes[12], bytes[13]]);
    let iv_len = u16::from_le_bytes([bytes[14], bytes[15]]);

    if crypt != 0 {
        return Err(TjsError::runtime("encrypted TJS/ns0 is not supported"));
    }
    if iv_len != 0 {
        return Err(TjsError::runtime("TJS/ns0 with IV is not supported"));
    }

    let payload: Vec<u8> = match check {
        CHECK_NS0 => bytes[HEADER_SIZE..].to_vec(),
        CHECK_4S0 => {
            return Err(TjsError::runtime("LZ4-compressed TJS/4s0 is not supported"));
        }
        _ => return Err(TjsError::runtime("unsupported TJS/ns0 variant")),
    };

    if payload.len() < 4 {
        return Err(TjsError::runtime("TJS/ns0 payload too short"));
    }

    let mut decoder = TjsNs0Decoder {
        runtime,
        bytes: &payload[..payload.len() - 4],
        index: 0,
        checker: ByteChecker::new(seed),
    };
    let value = decoder.value()?;
    let expected = decoder.checker.final_check();
    let actual = u32::from_le_bytes([
        payload[payload.len() - 4],
        payload[payload.len() - 3],
        payload[payload.len() - 2],
        payload[payload.len() - 1],
    ]);
    if expected != actual {
        return Err(TjsError::runtime(format!(
            "TJS/ns0 checksum mismatch: expected {expected:08X}, got {actual:08X}"
        )));
    }
    Ok(value)
}

struct ByteChecker {
    seed: u32,
}

impl ByteChecker {
    fn new(seed: u32) -> Self {
        Self { seed }
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
        let mut s = self.seed.to_le_bytes();
        if type_code == 0 {
            return s[2];
        }
        Self::round(&mut s);
        self.seed = u32::from_le_bytes(s);
        s[2]
    }

    fn final_check(&mut self) -> u32 {
        let mut s = self.seed.to_le_bytes();
        Self::round(&mut s);
        Self::round(&mut s);
        Self::round(&mut s);
        s.swap(0, 2);
        u32::from_le_bytes(s)
    }
}

struct TjsNs0Decoder<'a, H: super::TjsHost> {
    runtime: &'a mut Runtime<H>,
    bytes: &'a [u8],
    index: usize,
    checker: ByteChecker,
}

impl<'a, H: super::TjsHost + 'static> TjsNs0Decoder<'a, H> {
    fn value(&mut self) -> Result<Variant> {
        let typ = self.read_u16()?;
        let type_byte = (typ & 0xff) as u8;
        let check_byte = (typ >> 8) as u8;
        let expected = self.checker.get_seed(type_byte);
        if check_byte != expected {
            return Err(TjsError::runtime(format!(
                "TJS/ns0 byte check failed: expected {expected}, got {check_byte}"
            )));
        }
        match type_byte {
            0x00 => Ok(Variant::Void),
            0x02 => Ok(Variant::String(self.read_string()?)),
            0x04 => Ok(Variant::Integer(self.read_i64()?)),
            0x05 => Ok(Variant::Real(self.read_f64()?)),
            0x81 => self.array(),
            0xC1 => self.dictionary(),
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
        let handle = self.runtime.alloc_ordinary_object();
        install_dictionary_methods(self.runtime, handle);
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
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i64(&mut self) -> Result<i64> {
        let bytes = self.read_bytes(8)?;
        Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_f64(&mut self) -> Result<f64> {
        let bytes = self.read_bytes(8)?;
        Ok(f64::from_bits(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])))
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
