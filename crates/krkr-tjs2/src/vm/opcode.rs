use std::collections::BTreeMap;

use crate::bytecode::Instruction;
use crate::error::{Result, TjsError};
use crate::runtime::Variant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BinaryFamily {
    LogicalOr,
    LogicalAnd,
    BitOr,
    BitXor,
    BitAnd,
    ShiftArithmeticRight,
    ShiftLeft,
    ShiftLogicalRight,
    Add,
    Sub,
    Mod,
    Div,
    Idiv,
    Mul,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OpcodeForm {
    Slot,
    DirectProperty,
    IndirectProperty,
    DefaultProperty,
}

pub(super) fn execute_binary_value(
    family: BinaryFamily,
    lhs: Variant,
    rhs: Variant,
) -> Result<Variant> {
    match family {
        BinaryFamily::LogicalOr => Ok(Variant::Integer(i64::from(
            lhs.is_truthy() || rhs.is_truthy(),
        ))),
        BinaryFamily::LogicalAnd => Ok(Variant::Integer(i64::from(
            lhs.is_truthy() && rhs.is_truthy(),
        ))),
        BinaryFamily::BitOr => lhs.binary_int(&rhs, |a, b| a | b),
        BinaryFamily::BitXor => lhs.binary_int(&rhs, |a, b| a ^ b),
        BinaryFamily::BitAnd => lhs.binary_int(&rhs, |a, b| a & b),
        BinaryFamily::ShiftArithmeticRight => lhs.binary_int(&rhs, |a, b| a >> b),
        BinaryFamily::ShiftLeft => lhs.binary_int(&rhs, |a, b| a << b),
        BinaryFamily::ShiftLogicalRight => Ok(Variant::Integer(
            ((lhs.to_integer()? as u64) >> rhs.to_integer()?) as i64,
        )),
        BinaryFamily::Add => lhs.add(&rhs),
        BinaryFamily::Sub => lhs.sub(&rhs),
        BinaryFamily::Mod => lhs.modulo(&rhs),
        BinaryFamily::Div => lhs.div(&rhs),
        BinaryFamily::Idiv => lhs.idiv(&rhs),
        BinaryFamily::Mul => lhs.mul(&rhs),
    }
}

pub(super) fn binary_family(opcode: u8) -> BinaryFamily {
    match (opcode - 26) / 4 {
        0 => BinaryFamily::LogicalOr,
        1 => BinaryFamily::LogicalAnd,
        2 => BinaryFamily::BitOr,
        3 => BinaryFamily::BitXor,
        4 => BinaryFamily::BitAnd,
        5 => BinaryFamily::ShiftArithmeticRight,
        6 => BinaryFamily::ShiftLeft,
        7 => BinaryFamily::ShiftLogicalRight,
        8 => BinaryFamily::Add,
        9 => BinaryFamily::Sub,
        10 => BinaryFamily::Mod,
        11 => BinaryFamily::Div,
        12 => BinaryFamily::Idiv,
        13 => BinaryFamily::Mul,
        _ => unreachable!("opcode range checked by caller"),
    }
}

pub(super) fn opcode_form(opcode: u8) -> OpcodeForm {
    match (opcode - 26) % 4 {
        0 => OpcodeForm::Slot,
        1 => OpcodeForm::DirectProperty,
        2 => OpcodeForm::IndirectProperty,
        _ => OpcodeForm::DefaultProperty,
    }
}

pub(super) fn next_instruction_index(
    offset_to_index: &BTreeMap<usize, usize>,
    instructions: &[Instruction],
    pc: usize,
) -> Result<usize> {
    let next_offset = instructions[pc].offset + instructions[pc].len_words;
    if next_offset
        == instructions.last().expect("nonempty").offset
            + instructions.last().expect("nonempty").len_words
    {
        return Ok(instructions.len());
    }
    offset_to_index
        .get(&next_offset)
        .copied()
        .ok_or_else(|| TjsError::runtime(format!("no instruction at offset {next_offset}")))
}

pub(super) fn branch_index(
    offset_to_index: &BTreeMap<usize, usize>,
    inst: &Instruction,
) -> Result<usize> {
    let target = inst.offset as isize + isize::from(inst.operands[0]);
    if target < 0 {
        return Err(TjsError::runtime(format!(
            "negative branch target {target}"
        )));
    }
    offset_to_index
        .get(&(target as usize))
        .copied()
        .ok_or_else(|| TjsError::runtime(format!("no instruction at branch target {target}")))
}
