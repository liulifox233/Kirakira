use crate::error::{Result, TjsError};
use crate::runtime::Variant;

mod disasm;
mod instruction;
mod parse;
mod verify;

pub use self::instruction::{CallArgs, ExpandedArg, Instruction};

use self::disasm::disassemble_instruction;
use self::instruction::decode_instructions;
use self::parse::parse_bytecode;
use self::verify::verify_bytecode;

pub const BYTECODE_SIGNATURE: [u8; 8] = *b"TJS2100\0";

#[derive(Clone, Debug, PartialEq)]
pub struct BytecodeFile {
    pub data: DataPool,
    pub objects: Vec<CodeObject>,
    pub top_level: Option<usize>,
    pub debug_info: BytecodeDebugInfo,
}

impl BytecodeFile {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        parse_bytecode(bytes)
    }

    pub fn verify(&self) -> Result<()> {
        verify_bytecode(self)
    }

    pub fn disassemble_object(&self, object_index: usize) -> Result<Vec<String>> {
        let object = self
            .objects
            .get(object_index)
            .ok_or_else(|| TjsError::bytecode(format!("object {object_index} does not exist")))?;
        object
            .decode_instructions()
            .map(|instructions| {
                instructions
                    .iter()
                    .map(|inst| disassemble_instruction(self, object, inst))
                    .collect()
            })
            .map_err(|err| TjsError::bytecode(err.message))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BytecodeDebugInfo {
    pub sources: Vec<BytecodeSource>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BytecodeSource {
    pub name: String,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataPool {
    pub bytes: Vec<i8>,
    pub shorts: Vec<i16>,
    pub integers: Vec<i32>,
    pub longs: Vec<i64>,
    pub reals: Vec<f64>,
    pub strings: Vec<String>,
    pub octets: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CodeObject {
    pub parent: Option<usize>,
    pub name: usize,
    pub context_type: BytecodeContextType,
    pub max_variable_count: u32,
    pub variable_reserve_count: u32,
    pub max_frame_count: u32,
    pub func_decl_arg_count: u32,
    pub func_decl_unnamed_arg_array_base: u32,
    pub func_decl_collapse_base: Option<u32>,
    pub prop_setter: Option<usize>,
    pub prop_getter: Option<usize>,
    pub super_class_getter: Option<usize>,
    pub source_positions: Vec<SourcePosition>,
    pub code_words: Vec<i16>,
    pub data_slots: Vec<DataSlot>,
    pub super_class_getter_pointers: Vec<i32>,
    pub properties: Vec<PropertyRegistration>,
}

impl CodeObject {
    pub fn decode_instructions(&self) -> Result<Vec<Instruction>> {
        decode_instructions(&self.code_words)
    }

    pub fn name<'a>(&self, file: &'a BytecodeFile) -> Option<&'a str> {
        file.data.strings.get(self.name).map(String::as_str)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BytecodeContextType {
    TopLevel,
    Function,
    ExprFunction,
    Property,
    PropertySetter,
    PropertyGetter,
    Class,
    SuperClassGetter,
}

impl BytecodeContextType {
    fn from_i32(value: i32) -> Result<Self> {
        Ok(match value {
            0 => Self::TopLevel,
            1 => Self::Function,
            2 => Self::ExprFunction,
            3 => Self::Property,
            4 => Self::PropertySetter,
            5 => Self::PropertyGetter,
            6 => Self::Class,
            7 => Self::SuperClassGetter,
            _ => {
                return Err(TjsError::bytecode(format!(
                    "invalid context type id {value}"
                )));
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourcePosition {
    pub code_pos: u32,
    pub source_pos: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataSlot {
    pub ty: DataSlotType,
    pub index: i16,
}

impl DataSlot {
    pub fn value(&self, file: &BytecodeFile) -> Result<Variant> {
        Ok(match self.ty {
            DataSlotType::Unknown | DataSlotType::Void => Variant::Void,
            DataSlotType::Object => {
                if self.index == 0 {
                    Variant::Null
                } else {
                    return Err(TjsError::bytecode("unsupported object data slot index"));
                }
            }
            DataSlotType::InterObject | DataSlotType::InterGenerator => {
                let index = self.index_as_usize()?;
                require_index(index, file.objects.len(), "object")?;
                Variant::CodeObject(index)
            }
            DataSlotType::String => Variant::String(
                file.data
                    .strings
                    .get(self.index_as_usize()?)
                    .cloned()
                    .ok_or_else(|| TjsError::bytecode("invalid string data index"))?,
            ),
            DataSlotType::Octet => Variant::Octet(
                file.data
                    .octets
                    .get(self.index_as_usize()?)
                    .cloned()
                    .ok_or_else(|| TjsError::bytecode("invalid octet data index"))?,
            ),
            DataSlotType::Real => Variant::Real(
                *file
                    .data
                    .reals
                    .get(self.index_as_usize()?)
                    .ok_or_else(|| TjsError::bytecode("invalid real data index"))?,
            ),
            DataSlotType::Byte => Variant::Integer(i64::from(
                *file
                    .data
                    .bytes
                    .get(self.index_as_usize()?)
                    .ok_or_else(|| TjsError::bytecode("invalid byte data index"))?,
            )),
            DataSlotType::Short => Variant::Integer(i64::from(
                *file
                    .data
                    .shorts
                    .get(self.index_as_usize()?)
                    .ok_or_else(|| TjsError::bytecode("invalid short data index"))?,
            )),
            DataSlotType::Integer => Variant::Integer(i64::from(
                *file
                    .data
                    .integers
                    .get(self.index_as_usize()?)
                    .ok_or_else(|| TjsError::bytecode("invalid integer data index"))?,
            )),
            DataSlotType::Long => Variant::Integer(
                *file
                    .data
                    .longs
                    .get(self.index_as_usize()?)
                    .ok_or_else(|| TjsError::bytecode("invalid long data index"))?,
            ),
        })
    }

    fn index_as_usize(&self) -> Result<usize> {
        usize::try_from(self.index)
            .map_err(|_| TjsError::bytecode(format!("negative data slot index {}", self.index)))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataSlotType {
    Unknown,
    Void,
    Object,
    InterObject,
    String,
    Octet,
    Real,
    Byte,
    Short,
    Integer,
    Long,
    InterGenerator,
}

impl DataSlotType {
    fn from_i16(value: i16) -> Result<Self> {
        Ok(match value {
            -1 => Self::Unknown,
            0 => Self::Void,
            1 => Self::Object,
            2 => Self::InterObject,
            3 => Self::String,
            4 => Self::Octet,
            5 => Self::Real,
            6 => Self::Byte,
            7 => Self::Short,
            8 => Self::Integer,
            9 => Self::Long,
            10 => Self::InterGenerator,
            _ => {
                return Err(TjsError::bytecode(format!(
                    "invalid data slot type {value}"
                )));
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PropertyRegistration {
    pub name: usize,
    pub object: usize,
}

fn require_index(index: usize, len: usize, label: &str) -> Result<()> {
    if index < len {
        Ok(())
    } else {
        Err(TjsError::bytecode(format!(
            "{label} index {index} is outside length {len}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_bytecode_fixture() {
        let file = BytecodeFile::parse(&integer_return_bytecode()).expect("parse");
        assert_eq!(file.top_level, Some(0));
        assert_eq!(file.objects.len(), 1);
        assert_eq!(file.data.integers, vec![42]);
    }

    #[test]
    fn disassembles_inline_bytecode_fixture() {
        let file = BytecodeFile::parse(&integer_return_bytecode()).expect("parse");
        assert_eq!(
            file.disassemble_object(0).expect("disassemble"),
            vec![
                "00000000 const %0, *0 // *0 = 42".to_string(),
                "00000003 srv %0".to_string(),
                "00000005 ret".to_string(),
            ]
        );
    }

    #[test]
    fn rejects_bad_signature_and_size() {
        let mut bytes = integer_return_bytecode();
        bytes[0] = 0;
        assert!(BytecodeFile::parse(&bytes).is_err());

        let mut bytes = integer_return_bytecode();
        bytes[8] = 0;
        assert!(BytecodeFile::parse(&bytes).is_err());
    }

    #[test]
    fn treats_max_frame_count_as_inclusive_register_index() {
        let bytes = bytecode_file_with_max_frame(vec![8, 0], vec![1, 0, 0, 118, 0, 119], 0);
        let file = BytecodeFile::parse(&bytes).expect("parse");
        assert_eq!(file.objects[0].max_frame_count, 0);
    }

    fn integer_return_bytecode() -> Vec<u8> {
        bytecode_file(vec![8, 0], vec![1, 0, 0, 118, 0, 119])
    }

    fn bytecode_file(data_slots: Vec<i16>, code_words: Vec<i16>) -> Vec<u8> {
        bytecode_file_with_max_frame(data_slots, code_words, 1)
    }

    fn bytecode_file_with_max_frame(
        data_slots: Vec<i16>,
        code_words: Vec<i16>,
        max_frame_count: i32,
    ) -> Vec<u8> {
        let data_payload = data_pool();
        let object_payload = code_object(data_slots, code_words, max_frame_count);
        let mut objects_payload = Vec::new();
        push_i32(&mut objects_payload, 0);
        push_i32(&mut objects_payload, 1);
        objects_payload.extend_from_slice(b"TJS2");
        push_i32(&mut objects_payload, object_payload.len() as i32);
        objects_payload.extend_from_slice(&object_payload);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&BYTECODE_SIGNATURE);
        push_i32(&mut bytes, 0);
        bytes.extend_from_slice(b"DATA");
        push_i32(&mut bytes, (data_payload.len() + 8) as i32);
        bytes.extend_from_slice(&data_payload);
        bytes.extend_from_slice(b"OBJS");
        push_i32(&mut bytes, (objects_payload.len() + 8) as i32);
        bytes.extend_from_slice(&objects_payload);
        let size = bytes.len() as i32;
        bytes[8..12].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    fn data_pool() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 1);
        push_i32(&mut bytes, 42);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 1);
        push_utf16_string(&mut bytes, "global");
        push_i32(&mut bytes, 0);
        bytes
    }

    fn code_object(data_slots: Vec<i16>, code_words: Vec<i16>, max_frame_count: i32) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 2);
        push_i32(&mut bytes, max_frame_count);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, code_words.len() as i32);
        for word in &code_words {
            push_i16(&mut bytes, *word);
        }
        if code_words.len() % 2 == 1 {
            push_i16(&mut bytes, 0);
        }
        push_i32(&mut bytes, (data_slots.len() / 2) as i32);
        for word in data_slots {
            push_i16(&mut bytes, word);
        }
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        bytes
    }

    fn push_utf16_string(bytes: &mut Vec<u8>, text: &str) {
        let units = text.encode_utf16().collect::<Vec<_>>();
        push_i32(bytes, units.len() as i32);
        for unit in &units {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        if units.len() % 2 == 1 {
            push_i16(bytes, 0);
        }
    }

    fn push_i32(bytes: &mut Vec<u8>, value: i32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i16(bytes: &mut Vec<u8>, value: i16) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}
