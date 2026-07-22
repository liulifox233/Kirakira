use crate::error::{Result, TjsError};
use crate::runtime::Variant;

mod disasm;
mod instruction;
mod parse;
mod verify;

pub use self::disasm::DisasmOptions;
pub use self::instruction::{CallArgs, ExpandedArg, Instruction};

use self::disasm::{disassemble_file, disassemble_instruction, render_object_full};
use self::instruction::decode_instructions;
use self::parse::{parse_bytecode, parse_bytecode_unverified};
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

    pub fn parse_unverified(bytes: &[u8]) -> Result<Self> {
        parse_bytecode_unverified(bytes)
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

    /// Renders a full disassembly of the file: header, data pool, and code
    /// object sections, controlled by `options`.
    pub fn disassemble(&self, options: &DisasmOptions) -> Result<String> {
        if let Some(index) = options.object_index
            && index >= self.objects.len()
        {
            return Err(TjsError::bytecode(format!("object {index} does not exist")));
        }
        Ok(disassemble_file(self, options))
    }

    /// Renders one code object section: header fields, data slot table,
    /// properties, source positions, and instruction lines.
    pub fn disassemble_object_full(&self, object_index: usize) -> Result<String> {
        let object = self
            .objects
            .get(object_index)
            .ok_or_else(|| TjsError::bytecode(format!("object {object_index} does not exist")))?;
        let mut out = String::new();
        render_object_full(self, object, object_index, &mut out);
        Ok(out)
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

    pub(crate) fn effective_max_frame_count(&self) -> Result<u32> {
        let instructions = self.decode_instructions()?;
        Ok(max_positive_register(
            instructions.iter(),
            self.max_frame_count,
        ))
    }

    pub fn name<'a>(&self, file: &'a BytecodeFile) -> Option<&'a str> {
        file.data.strings.get(self.name).map(String::as_str)
    }
}

pub(crate) fn max_positive_register<'a>(
    instructions: impl IntoIterator<Item = &'a Instruction>,
    declared_max_frame_count: u32,
) -> u32 {
    let mut max_frame_count = declared_max_frame_count;
    for inst in instructions {
        for reg in inst.register_operands() {
            if reg >= 0 {
                max_frame_count = max_frame_count.max(reg as u32);
            }
        }
    }
    max_frame_count
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
    fn disassembles_full_file_fixture() {
        let file = BytecodeFile::parse(&integer_return_bytecode()).expect("parse");
        let dump = file
            .disassemble(&DisasmOptions::full())
            .expect("disassemble");
        assert_eq!(
            dump,
            concat!(
                "; objects=1 top_level=0\n",
                ".data\n",
                "  pools: bytes=0 shorts=0 integers=1 longs=0 reals=0 strings=1 octets=0\n",
                "  integer pool:\n",
                "    0: 42\n",
                "  string pool:\n",
                "    0: \"global\"\n",
                "\n",
                "=== object 0 name=\"global\" type=TopLevel parent=-\n",
                "  vars=0 reserve=2 frame=1 args=0 unnamed_base=0 collapse=-\n",
                "  setter=- getter=- super=-\n",
                "  data slots:\n",
                "    *0: Integer = 42\n",
                "  code:\n",
                "00000000 const %0, *0 // *0 = 42\n",
                "00000003 srv %0\n",
                "00000005 ret\n",
            )
        );
    }

    #[test]
    fn disassemble_honors_options() {
        let file = BytecodeFile::parse(&integer_return_bytecode()).expect("parse");

        let no_data = DisasmOptions {
            include_data_pool: false,
            ..DisasmOptions::full()
        };
        let dump = file.disassemble(&no_data).expect("disassemble");
        assert!(!dump.contains(".data"));
        assert!(dump.contains("=== object 0"));

        let matching = DisasmOptions {
            object_name_filter: Some("glob".to_string()),
            ..DisasmOptions::full()
        };
        assert!(
            file.disassemble(&matching)
                .expect("disassemble")
                .contains("=== object 0")
        );

        let missing = DisasmOptions {
            object_name_filter: Some("zzz".to_string()),
            ..DisasmOptions::full()
        };
        assert!(
            !file
                .disassemble(&missing)
                .expect("disassemble")
                .contains("=== object")
        );

        let wrong_index = DisasmOptions {
            object_index: Some(5),
            ..DisasmOptions::full()
        };
        assert!(file.disassemble(&wrong_index).is_err());

        let full_object = file.disassemble_object_full(0).expect("object");
        assert!(full_object.starts_with("=== object 0"));
        assert!(file.disassemble_object_full(5).is_err());
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

    #[test]
    fn accepts_krkr2_bytecode_with_underdeclared_max_frame_count() {
        let bytes =
            bytecode_file_with_max_frame(vec![3, 0], vec![124, 1, 103, 2, 1, 0, 118, 2, 119], 1);
        let file = BytecodeFile::parse(&bytes).expect("parse");

        assert_eq!(file.objects[0].max_frame_count, 1);
        assert_eq!(
            file.objects[0].effective_max_frame_count().expect("scan"),
            2
        );
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
