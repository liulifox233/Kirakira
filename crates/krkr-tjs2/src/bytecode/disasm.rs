use super::instruction::{BinaryForm, CallArgs, Instruction, binary_form};
use super::{BytecodeFile, CodeObject};
use crate::runtime::Variant;

pub(super) fn disassemble_instruction(
    file: &BytecodeFile,
    object: &CodeObject,
    inst: &Instruction,
) -> String {
    let prefix = format!("{:08} {}", inst.offset, inst.mnemonic());
    match inst.opcode {
        0 | 14 | 119 | 121 | 126 | 127 => prefix,
        1 => format!(
            "{prefix} {}, {} // {} = {}",
            reg(inst.operands[0]),
            data_ref(inst.operands[1]),
            data_ref(inst.operands[1]),
            data_value(file, object, inst.operands[1])
        ),
        2 | 7..=10 | 88 | 123 | 125 => format!(
            "{prefix} {}, {}",
            reg(inst.operands[0]),
            reg(inst.operands[1])
        ),
        3 | 5 | 6 | 11..=13 | 18 | 22 | 82 | 83 | 86 | 87 | 89..=98 | 118 | 122 | 124 => {
            format!("{prefix} {}", reg(inst.operands[0]))
        }
        4 => format!("{prefix} {}, {}", reg(inst.operands[0]), inst.operands[1]),
        15..=17 => format!(
            "{prefix} {:09}",
            branch_target(inst.offset, inst.operands[0])
        ),
        19 | 23 | 84 | 103 | 110 | 116 => format!(
            "{prefix} {}, {}.{} // {} = {}",
            reg(inst.operands[0]),
            reg(inst.operands[1]),
            data_ref(inst.operands[2]),
            data_ref(inst.operands[2]),
            data_value(file, object, inst.operands[2])
        ),
        20 | 24 | 85 | 107 | 112 | 117 => format!(
            "{prefix} {}, {}[{}]",
            reg(inst.operands[0]),
            reg(inst.operands[1]),
            reg(inst.operands[2])
        ),
        21 | 25 | 114 | 115 => {
            format!(
                "{prefix} {}, {}",
                reg(inst.operands[0]),
                reg(inst.operands[1])
            )
        }
        26..=81 => disassemble_binary(file, object, inst, &prefix),
        99 | 102 => format!(
            "{prefix} {}, {}{}",
            reg(inst.operands[0]),
            reg(inst.operands[1]),
            call_args(inst.call_args.as_ref())
        ),
        100 => format!(
            "{prefix} {}, {}.{}{} // {} = {}",
            reg(inst.operands[0]),
            reg(inst.operands[1]),
            data_ref(inst.operands[2]),
            call_args(inst.call_args.as_ref()),
            data_ref(inst.operands[2]),
            data_value(file, object, inst.operands[2])
        ),
        101 => format!(
            "{prefix} {}, {}[{}]{}",
            reg(inst.operands[0]),
            reg(inst.operands[1]),
            reg(inst.operands[2]),
            call_args(inst.call_args.as_ref())
        ),
        104..=106 | 111 => format!(
            "{prefix} {}.{}, {} // {} = {}",
            reg(inst.operands[0]),
            data_ref(inst.operands[1]),
            reg(inst.operands[2]),
            data_ref(inst.operands[1]),
            data_value(file, object, inst.operands[1])
        ),
        108 | 109 | 113 => format!(
            "{prefix} {}[{}], {}",
            reg(inst.operands[0]),
            reg(inst.operands[1]),
            reg(inst.operands[2])
        ),
        120 => format!(
            "{prefix} {:09}, {}",
            branch_target(inst.offset, inst.operands[0]),
            reg(inst.operands[1])
        ),
        _ => {
            let operands = inst
                .operands
                .iter()
                .map(i16::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            if operands.is_empty() {
                prefix
            } else {
                format!("{prefix} {operands}")
            }
        }
    }
}

fn disassemble_binary(
    file: &BytecodeFile,
    object: &CodeObject,
    inst: &Instruction,
    prefix: &str,
) -> String {
    match binary_form(inst.opcode) {
        BinaryForm::Slot => format!(
            "{prefix} {}, {}",
            reg(inst.operands[0]),
            reg(inst.operands[1])
        ),
        BinaryForm::DirectProperty => format!(
            "{prefix} {}, {}.{}, {} // {} = {}",
            reg(inst.operands[0]),
            reg(inst.operands[1]),
            data_ref(inst.operands[2]),
            reg(inst.operands[3]),
            data_ref(inst.operands[2]),
            data_value(file, object, inst.operands[2])
        ),
        BinaryForm::IndirectProperty => format!(
            "{prefix} {}, {}[{}], {}",
            reg(inst.operands[0]),
            reg(inst.operands[1]),
            reg(inst.operands[2]),
            reg(inst.operands[3])
        ),
        BinaryForm::DefaultProperty => format!(
            "{prefix} {}, {}, {}",
            reg(inst.operands[0]),
            reg(inst.operands[1]),
            reg(inst.operands[2])
        ),
    }
}

fn data_value(file: &BytecodeFile, object: &CodeObject, data_index: i16) -> String {
    let Ok(index) = usize::try_from(data_index) else {
        return "<invalid>".to_string();
    };
    let Some(slot) = object.data_slots.get(index) else {
        return "<invalid>".to_string();
    };
    match slot.value(file) {
        Ok(Variant::String(value)) => format!("{value:?}"),
        Ok(value) => value.to_string(),
        Err(_) => "<invalid>".to_string(),
    }
}

fn reg(index: i16) -> String {
    format!("%{index}")
}

fn data_ref(index: i16) -> String {
    format!("*{index}")
}

fn branch_target(offset: usize, delta: i16) -> isize {
    offset as isize + isize::from(delta)
}

fn call_args(args: Option<&CallArgs>) -> String {
    match args {
        Some(CallArgs::Normal(regs)) => {
            let rendered = regs
                .iter()
                .map(|value| reg(*value))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({rendered})")
        }
        Some(CallArgs::OmittedCallerArgs) => "(...)".to_string(),
        Some(CallArgs::Expanded(args)) => {
            let rendered = args
                .iter()
                .map(|arg| format!("{}:{}", arg.arg_type, reg(arg.reg)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({rendered})")
        }
        None => "(<invalid>)".to_string(),
    }
}
