use super::{
    BYTECODE_SIGNATURE, BytecodeContextType, BytecodeFile, CodeObject, DataPool, DataSlot,
    DataSlotType, PropertyRegistration, SourcePosition,
};
use crate::error::{Result, TjsError};

pub(super) fn parse_bytecode(bytes: &[u8]) -> Result<BytecodeFile> {
    let mut reader = Reader::new(bytes);
    let signature = reader.read_bytes(8)?;
    if signature != BYTECODE_SIGNATURE {
        return Err(TjsError::bytecode("TJS2 bytecode signature mismatch"));
    }
    let file_size = reader.read_i32_nonnegative("file size")?;
    if file_size != bytes.len() {
        return Err(TjsError::bytecode(format!(
            "file size field {file_size} does not match input length {}",
            bytes.len()
        )));
    }

    reader.expect_tag(b"DATA")?;
    let data_chunk_size = reader.read_i32_nonnegative("DATA chunk size")?;
    let data_payload_start = reader.pos;
    let data_payload_end = checked_add(data_payload_start, data_chunk_size - 8)?;
    if data_payload_end > bytes.len() {
        return Err(TjsError::bytecode("DATA chunk overruns file"));
    }
    let data = parse_data_pool(reader.subreader(data_payload_end)?)?;
    reader.pos = data_payload_end;

    reader.expect_tag(b"OBJS")?;
    let objs_chunk_size = reader.read_i32_nonnegative("OBJS chunk size")?;
    let objs_payload_start = reader.pos;
    let objs_payload_end = checked_add(objs_payload_start, objs_chunk_size - 8)?;
    if objs_payload_end != bytes.len() {
        return Err(TjsError::bytecode(
            "OBJS chunk does not end at file boundary",
        ));
    }
    let (top_level, objects) = parse_objects(reader.subreader(objs_payload_end)?)?;
    reader.pos = objs_payload_end;
    reader.expect_end()?;

    let file = BytecodeFile {
        data,
        objects,
        top_level,
        debug_info: Default::default(),
    };
    file.verify()?;
    Ok(file)
}

fn parse_data_pool(mut reader: Reader<'_>) -> Result<DataPool> {
    let byte_count = reader.read_count("byte pool count")?;
    let mut bytes = Vec::with_capacity(byte_count);
    for _ in 0..byte_count {
        bytes.push(reader.read_i8()?);
    }
    reader.align_4()?;

    let short_count = reader.read_count("short pool count")?;
    let mut shorts = Vec::with_capacity(short_count);
    for _ in 0..short_count {
        shorts.push(reader.read_i16()?);
    }
    reader.align_4()?;

    let integer_count = reader.read_count("integer pool count")?;
    let mut integers = Vec::with_capacity(integer_count);
    for _ in 0..integer_count {
        integers.push(reader.read_i32()?);
    }

    let long_count = reader.read_count("long pool count")?;
    let mut longs = Vec::with_capacity(long_count);
    for _ in 0..long_count {
        longs.push(reader.read_i64()?);
    }

    let real_count = reader.read_count("real pool count")?;
    let mut reals = Vec::with_capacity(real_count);
    for _ in 0..real_count {
        reals.push(reader.read_f64()?);
    }

    let string_count = reader.read_count("string pool count")?;
    let mut strings = Vec::with_capacity(string_count);
    for _ in 0..string_count {
        let len = reader.read_count("string length")?;
        let mut units = Vec::with_capacity(len);
        for _ in 0..len {
            units.push(reader.read_u16()?);
        }
        if len % 2 == 1 {
            reader.read_u16()?;
        }
        strings.push(
            String::from_utf16(&units)
                .map_err(|_| TjsError::bytecode("string pool contains invalid UTF-16"))?,
        );
    }

    let octet_count = reader.read_count("octet pool count")?;
    let mut octets = Vec::with_capacity(octet_count);
    for _ in 0..octet_count {
        let len = reader.read_count("octet length")?;
        octets.push(reader.read_bytes(len)?.to_vec());
        reader.align_4()?;
    }

    reader.expect_end()?;
    Ok(DataPool {
        bytes,
        shorts,
        integers,
        longs,
        reals,
        strings,
        octets,
    })
}

fn parse_objects(mut reader: Reader<'_>) -> Result<(Option<usize>, Vec<CodeObject>)> {
    let top_level_raw = reader.read_i32()?;
    let top_level = optional_index(top_level_raw, "top-level object")?;
    let object_count = reader.read_count("object count")?;
    let mut objects = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        reader.expect_tag(b"TJS2")?;
        let payload_size = reader.read_i32_nonnegative("object payload size")?;
        let payload_end = checked_add(reader.pos, payload_size)?;
        let object = parse_code_object(reader.subreader(payload_end)?)?;
        reader.pos = payload_end;
        objects.push(object);
    }
    reader.expect_end()?;
    Ok((top_level, objects))
}

fn parse_code_object(mut reader: Reader<'_>) -> Result<CodeObject> {
    let parent = optional_index(reader.read_i32()?, "parent object")?;
    let name = reader.read_index("object name")?;
    let context_type = BytecodeContextType::from_i32(reader.read_i32()?)?;
    let max_variable_count = reader.read_u32_field("max_variable_count")?;
    let variable_reserve_count = reader.read_u32_field("variable_reserve_count")?;
    let max_frame_count = reader.read_u32_field("max_frame_count")?;
    let func_decl_arg_count = reader.read_u32_field("func_decl_arg_count")?;
    let func_decl_unnamed_arg_array_base =
        reader.read_u32_field("func_decl_unnamed_arg_array_base")?;
    let func_decl_collapse_base = optional_u32(reader.read_i32()?, "func_decl_collapse_base")?;
    let prop_setter = optional_index(reader.read_i32()?, "prop_setter")?;
    let prop_getter = optional_index(reader.read_i32()?, "prop_getter")?;
    let super_class_getter = optional_index(reader.read_i32()?, "super_class_getter")?;

    let source_pos_count = reader.read_count("source position count")?;
    let mut code_positions = Vec::with_capacity(source_pos_count);
    for _ in 0..source_pos_count {
        code_positions.push(reader.read_u32_field("source code position")?);
    }
    let mut source_positions = Vec::with_capacity(source_pos_count);
    for code_pos in code_positions {
        source_positions.push(SourcePosition {
            code_pos,
            source_pos: reader.read_u32_field("source source position")?,
        });
    }

    let code_word_count = reader.read_count("code word count")?;
    let mut code_words = Vec::with_capacity(code_word_count);
    for _ in 0..code_word_count {
        code_words.push(reader.read_i16()?);
    }
    if code_word_count % 2 == 1 {
        reader.read_i16()?;
    }

    let data_count = reader.read_count("data area count")?;
    let mut data_slots = Vec::with_capacity(data_count);
    for _ in 0..data_count {
        data_slots.push(DataSlot {
            ty: DataSlotType::from_i16(reader.read_i16()?)?,
            index: reader.read_i16()?,
        });
    }

    let super_pointer_count = reader.read_count("super class getter pointer count")?;
    let mut super_class_getter_pointers = Vec::with_capacity(super_pointer_count);
    for _ in 0..super_pointer_count {
        super_class_getter_pointers.push(reader.read_i32()?);
    }

    let property_count = reader.read_count("property count")?;
    let mut properties = Vec::with_capacity(property_count);
    for _ in 0..property_count {
        properties.push(PropertyRegistration {
            name: reader.read_index("property name")?,
            object: reader.read_index("property object")?,
        });
    }

    reader.expect_end()?;
    Ok(CodeObject {
        parent,
        name,
        context_type,
        max_variable_count,
        variable_reserve_count,
        max_frame_count,
        func_decl_arg_count,
        func_decl_unnamed_arg_array_base,
        func_decl_collapse_base,
        prop_setter,
        prop_getter,
        super_class_getter,
        source_positions,
        code_words,
        data_slots,
        super_class_getter_pointers,
        properties,
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            end: bytes.len(),
        }
    }

    fn subreader(&self, end: usize) -> Result<Reader<'a>> {
        if end < self.pos || end > self.end {
            return Err(TjsError::bytecode("subreader range is out of bounds"));
        }
        Ok(Reader {
            bytes: self.bytes,
            pos: self.pos,
            end,
        })
    }

    fn expect_end(&self) -> Result<()> {
        if self.pos == self.end {
            Ok(())
        } else {
            Err(TjsError::bytecode(format!(
                "trailing bytes: cursor {} expected end {}",
                self.pos, self.end
            )))
        }
    }

    fn expect_tag(&mut self, tag: &[u8; 4]) -> Result<()> {
        let actual = self.read_bytes(4)?;
        if actual == tag {
            Ok(())
        } else {
            Err(TjsError::bytecode(format!(
                "expected tag {:?}, found {:?}",
                String::from_utf8_lossy(tag),
                String::from_utf8_lossy(actual)
            )))
        }
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = checked_add(self.pos, len)?;
        if end > self.end {
            return Err(TjsError::bytecode("unexpected end of bytecode"));
        }
        let bytes = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn read_i8(&mut self) -> Result<i8> {
        Ok(i8::from_le_bytes([self.read_bytes(1)?[0]]))
    }

    fn read_u16(&mut self) -> Result<u16> {
        let bytes: [u8; 2] = self.read_bytes(2)?.try_into().expect("length checked");
        Ok(u16::from_le_bytes(bytes))
    }

    fn read_i16(&mut self) -> Result<i16> {
        let bytes: [u8; 2] = self.read_bytes(2)?.try_into().expect("length checked");
        Ok(i16::from_le_bytes(bytes))
    }

    fn read_i32(&mut self) -> Result<i32> {
        let bytes: [u8; 4] = self.read_bytes(4)?.try_into().expect("length checked");
        Ok(i32::from_le_bytes(bytes))
    }

    fn read_i64(&mut self) -> Result<i64> {
        let bytes: [u8; 8] = self.read_bytes(8)?.try_into().expect("length checked");
        Ok(i64::from_le_bytes(bytes))
    }

    fn read_f64(&mut self) -> Result<f64> {
        let bytes: [u8; 8] = self.read_bytes(8)?.try_into().expect("length checked");
        Ok(f64::from_le_bytes(bytes))
    }

    fn read_i32_nonnegative(&mut self, field: &str) -> Result<usize> {
        usize::try_from(self.read_i32()?)
            .map_err(|_| TjsError::bytecode(format!("{field} is negative")))
    }

    fn read_count(&mut self, field: &str) -> Result<usize> {
        self.read_i32_nonnegative(field)
    }

    fn read_index(&mut self, field: &str) -> Result<usize> {
        self.read_i32_nonnegative(field)
    }

    fn read_u32_field(&mut self, field: &str) -> Result<u32> {
        u32::try_from(self.read_i32()?)
            .map_err(|_| TjsError::bytecode(format!("{field} is negative")))
    }

    fn align_4(&mut self) -> Result<()> {
        while !self.pos.is_multiple_of(4) {
            self.read_bytes(1)?;
        }
        Ok(())
    }
}

fn optional_index(value: i32, field: &str) -> Result<Option<usize>> {
    if value == -1 {
        return Ok(None);
    }
    usize::try_from(value)
        .map(Some)
        .map_err(|_| TjsError::bytecode(format!("{field} is negative")))
}

fn optional_u32(value: i32, field: &str) -> Result<Option<u32>> {
    if value == -1 {
        return Ok(None);
    }
    u32::try_from(value)
        .map(Some)
        .map_err(|_| TjsError::bytecode(format!("{field} is negative")))
}

fn checked_add(lhs: usize, rhs: usize) -> Result<usize> {
    lhs.checked_add(rhs)
        .ok_or_else(|| TjsError::bytecode("bytecode offset overflow"))
}
