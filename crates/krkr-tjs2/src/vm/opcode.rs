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

/// Instruction-index tables built once per decoded code object.
///
/// Both answers the dispatch loop needs -- the index of the sequential next
/// instruction and the index a branch operand targets -- follow from the
/// instruction stream alone, so they are resolved at decode time instead of
/// by a `BTreeMap<offset, index>` lookup per executed instruction and per
/// taken branch.  An entry the stream does not resolve (a gap after the
/// instruction, a negative target, a target that is not an instruction
/// start) stays [`MISSING`], and the VM rebuilds the reference's diagnostic
/// from the instruction itself -- the slow path only runs on bytecode the
/// decoder cannot have produced.
#[derive(Clone, Debug)]
pub(crate) struct JumpTable {
    next: Box<[u32]>,
    branch: Box<[u32]>,
}

/// Marks a table entry the instruction stream does not resolve.
const MISSING: u32 = u32::MAX;

impl JumpTable {
    pub(crate) fn build(
        instructions: &[Instruction],
        offset_to_index: &BTreeMap<usize, usize>,
    ) -> Self {
        let end_offset = instructions
            .last()
            .map(|instruction| instruction.offset + instruction.len_words);
        let mut next = Vec::with_capacity(instructions.len());
        let mut branch = Vec::with_capacity(instructions.len());
        for instruction in instructions {
            let next_offset = instruction.offset + instruction.len_words;
            next.push(if Some(next_offset) == end_offset {
                instructions.len() as u32
            } else {
                resolve(offset_to_index, next_offset)
            });
            branch.push(resolve_branch(instruction, offset_to_index));
        }
        Self {
            next: next.into_boxed_slice(),
            branch: branch.into_boxed_slice(),
        }
    }

    /// The index the dispatch loop continues at when `pc` does not branch.
    #[inline]
    pub(super) fn next_index(&self, instructions: &[Instruction], pc: usize) -> Result<usize> {
        let index = self.next[pc];
        if index != MISSING {
            return Ok(index as usize);
        }
        let instruction = &instructions[pc];
        Err(TjsError::runtime(format!(
            "no instruction at offset {}",
            instruction.offset + instruction.len_words
        )))
    }

    /// The index a branch operand of `instruction` resolves to.
    #[inline]
    pub(super) fn branch_index(&self, instruction: &Instruction, pc: usize) -> Result<usize> {
        let index = self.branch[pc];
        if index != MISSING {
            return Ok(index as usize);
        }
        let target = instruction.offset as isize + isize::from(instruction.operands[0]);
        if target < 0 {
            return Err(TjsError::runtime(format!(
                "negative branch target {target}"
            )));
        }
        Err(TjsError::runtime(format!(
            "no instruction at branch target {target}"
        )))
    }
}

fn resolve(offset_to_index: &BTreeMap<usize, usize>, offset: usize) -> u32 {
    offset_to_index
        .get(&offset)
        .map_or(MISSING, |index| *index as u32)
}

fn resolve_branch(instruction: &Instruction, offset_to_index: &BTreeMap<usize, usize>) -> u32 {
    let Some(operand) = instruction.operands.first() else {
        return MISSING;
    };
    let target = instruction.offset as isize + isize::from(*operand);
    if target < 0 {
        return MISSING;
    }
    resolve(offset_to_index, target as usize)
}
