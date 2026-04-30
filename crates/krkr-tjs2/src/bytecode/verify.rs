use std::collections::BTreeSet;

use super::instruction::{BinaryForm, CallArgs, Instruction, binary_form};
use super::{BytecodeFile, CodeObject, DataSlot, DataSlotType, require_index};
use crate::error::{Result, TjsError};

pub(super) fn verify_bytecode(file: &BytecodeFile) -> Result<()> {
    if let Some(top_level) = file.top_level {
        require_index(top_level, file.objects.len(), "top-level object")?;
    }
    for (index, object) in file.objects.iter().enumerate() {
        require_index(object.name, file.data.strings.len(), "object name")?;
        for target in [
            object.parent,
            object.prop_getter,
            object.prop_setter,
            object.super_class_getter,
        ]
        .into_iter()
        .flatten()
        {
            require_index(target, file.objects.len(), "object reference")?;
        }
        for property in &object.properties {
            require_index(property.name, file.data.strings.len(), "property name")?;
            require_index(property.object, file.objects.len(), "property object")?;
        }
        for slot in &object.data_slots {
            verify_data_slot(file, slot)?;
        }
        verify_source_positions(object)?;
        verify_instructions(index, object)?;
    }
    Ok(())
}

fn verify_data_slot(file: &BytecodeFile, slot: &DataSlot) -> Result<()> {
    let index = match slot.ty {
        DataSlotType::Unknown | DataSlotType::Void => return Ok(()),
        DataSlotType::Object => {
            if slot.index == 0 {
                return Ok(());
            }
            return Err(TjsError::verify(
                "object data slot only supports null index 0",
            ));
        }
        _ => slot.index_as_usize()?,
    };
    let len = match slot.ty {
        DataSlotType::InterObject | DataSlotType::InterGenerator => file.objects.len(),
        DataSlotType::String => file.data.strings.len(),
        DataSlotType::Octet => file.data.octets.len(),
        DataSlotType::Real => file.data.reals.len(),
        DataSlotType::Byte => file.data.bytes.len(),
        DataSlotType::Short => file.data.shorts.len(),
        DataSlotType::Integer => file.data.integers.len(),
        DataSlotType::Long => file.data.longs.len(),
        DataSlotType::Unknown | DataSlotType::Void | DataSlotType::Object => unreachable!(),
    };
    require_index(index, len, "data slot pool index").map_err(|err| TjsError::verify(err.message))
}

fn verify_source_positions(object: &CodeObject) -> Result<()> {
    let mut seen = BTreeSet::new();
    for position in &object.source_positions {
        if position.code_pos as usize >= object.code_words.len() {
            return Err(TjsError::verify(
                "source code position is outside code area",
            ));
        }
        if !seen.insert(position.code_pos) {
            return Err(TjsError::verify("duplicate source code position"));
        }
    }
    Ok(())
}

fn verify_instructions(object_index: usize, object: &CodeObject) -> Result<()> {
    let instructions = object.decode_instructions()?;
    for inst in instructions {
        verify_instruction_operands(object_index, object, &inst)?;
    }
    Ok(())
}

fn verify_instruction_operands(
    object_index: usize,
    object: &CodeObject,
    inst: &Instruction,
) -> Result<()> {
    let code_len = object.code_words.len();
    match inst.opcode {
        1 => {
            require_reg(object, inst.operands[0])?;
            require_data(object, inst.operands[1])?;
        }
        2 | 7..=10 | 88 | 123 | 125 => {
            for operand in &inst.operands {
                require_reg(object, *operand)?;
            }
        }
        3 | 5 | 6 | 11..=13 | 18 | 22 | 82 | 83 | 86 | 87 | 89..=98 | 118 | 122 | 124 => {
            require_reg(object, inst.operands[0])?;
        }
        4 => {
            require_reg(object, inst.operands[0])?;
            if inst.operands[1] < 0 {
                return Err(TjsError::verify("ccl count is negative"));
            }
        }
        15..=17 => require_branch(code_len, inst.offset, inst.operands[0])?,
        19 | 23 | 84 | 103 | 110 | 116 => {
            require_reg(object, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
            require_data(object, inst.operands[2])?;
        }
        20 | 24 | 85 | 107 | 112 | 117 => {
            require_reg(object, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
            require_reg(object, inst.operands[2])?;
        }
        21 | 25 | 114 | 115 => {
            require_reg(object, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
        }
        26..=81 => verify_binary_operands(object, inst)?,
        99 | 102 => {
            require_reg(object, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
            verify_call_args(object, inst.call_args.as_ref())?;
        }
        100 | 101 => {
            require_reg(object, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
            if inst.opcode == 100 {
                require_data(object, inst.operands[2])?;
            } else {
                require_reg(object, inst.operands[2])?;
            }
            verify_call_args(object, inst.call_args.as_ref())?;
        }
        104..=106 | 111 => {
            require_reg(object, inst.operands[0])?;
            require_data(object, inst.operands[1])?;
            require_reg(object, inst.operands[2])?;
        }
        108 | 109 | 113 => {
            require_reg(object, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
            require_reg(object, inst.operands[2])?;
        }
        120 => {
            require_branch(code_len, inst.offset, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
        }
        0 | 14 | 119 | 121 | 126 | 127 => {}
        _ => {
            return Err(TjsError::verify(format!(
                "object {object_index} contains unsupported opcode {}",
                inst.opcode
            )));
        }
    }
    Ok(())
}

fn verify_binary_operands(object: &CodeObject, inst: &Instruction) -> Result<()> {
    match binary_form(inst.opcode) {
        BinaryForm::Slot => {
            require_reg(object, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
        }
        BinaryForm::DirectProperty => {
            require_reg(object, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
            require_data(object, inst.operands[2])?;
            require_reg(object, inst.operands[3])?;
        }
        BinaryForm::IndirectProperty => {
            for operand in &inst.operands {
                require_reg(object, *operand)?;
            }
        }
        BinaryForm::DefaultProperty => {
            require_reg(object, inst.operands[0])?;
            require_reg(object, inst.operands[1])?;
            require_reg(object, inst.operands[2])?;
        }
    }
    Ok(())
}

fn verify_call_args(object: &CodeObject, args: Option<&CallArgs>) -> Result<()> {
    let Some(args) = args else {
        return Err(TjsError::verify("call opcode has no argspec"));
    };
    match args {
        CallArgs::Normal(args) => {
            for arg in args {
                require_reg(object, *arg)?;
            }
        }
        CallArgs::OmittedCallerArgs => {}
        CallArgs::Expanded(args) => {
            for arg in args {
                if !matches!(arg.arg_type, 0..=2) {
                    return Err(TjsError::verify("invalid expanded call argument type"));
                }
                require_reg(object, arg.reg)?;
            }
        }
    }
    Ok(())
}

fn require_reg(object: &CodeObject, reg: i16) -> Result<()> {
    let min = -i64::from(
        object.max_variable_count + object.variable_reserve_count + object.func_decl_arg_count + 4,
    );
    let max = i64::from(object.max_frame_count);
    let reg = i64::from(reg);
    if reg < min || reg > max {
        return Err(TjsError::verify(format!(
            "register {reg} is outside frame range {min}..={max}"
        )));
    }
    Ok(())
}

fn require_data(object: &CodeObject, index: i16) -> Result<()> {
    let index = usize::try_from(index)
        .map_err(|_| TjsError::verify(format!("negative data operand {index}")))?;
    if index >= object.data_slots.len() {
        return Err(TjsError::verify(format!(
            "data operand {index} is outside data area length {}",
            object.data_slots.len()
        )));
    }
    Ok(())
}

fn require_branch(code_len: usize, offset: usize, delta: i16) -> Result<()> {
    let target = offset as isize + isize::from(delta);
    if target < 0 || target as usize >= code_len {
        return Err(TjsError::verify(format!(
            "branch target {target} is outside code length {code_len}"
        )));
    }
    Ok(())
}
