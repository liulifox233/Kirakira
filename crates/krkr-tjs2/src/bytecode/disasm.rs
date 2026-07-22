use std::fmt::Write as _;

use super::instruction::{BinaryForm, CallArgs, Instruction, binary_form};
use super::{BytecodeFile, CodeObject, DataSlot};
use crate::runtime::Variant;

/// Options controlling [`BytecodeFile::disassemble`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisasmOptions {
    /// Render the data pool section. Defaults to `true` via
    /// [`DisasmOptions::full`].
    pub include_data_pool: bool,
    /// Only render code objects whose name contains this substring.
    pub object_name_filter: Option<String>,
    /// Only render the code object with this index.
    pub object_index: Option<usize>,
}

impl DisasmOptions {
    /// Renders everything: data pool plus all code objects.
    pub fn full() -> Self {
        Self {
            include_data_pool: true,
            object_name_filter: None,
            object_index: None,
        }
    }
}

pub(super) fn disassemble_file(file: &BytecodeFile, options: &DisasmOptions) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "; objects={} top_level={}",
        file.objects.len(),
        opt_index(file.top_level)
    );
    if options.include_data_pool {
        render_data_pool(file, &mut out);
    }
    for (index, object) in file.objects.iter().enumerate() {
        if let Some(wanted) = options.object_index
            && wanted != index
        {
            continue;
        }
        if let Some(filter) = &options.object_name_filter {
            let name = object.name(file).unwrap_or("");
            if !name.contains(filter.as_str()) {
                continue;
            }
        }
        let _ = writeln!(out);
        render_object_full(file, object, index, &mut out);
    }
    out
}

pub(super) fn render_object_full(
    file: &BytecodeFile,
    object: &CodeObject,
    index: usize,
    out: &mut String,
) {
    let name = object
        .name(file)
        .map(|name| format!("{name:?}"))
        .unwrap_or_else(|| format!("<invalid #{}>", object.name));
    let _ = writeln!(
        out,
        "=== object {index} name={name} type={:?} parent={}",
        object.context_type,
        opt_index(object.parent)
    );
    let _ = writeln!(
        out,
        "  vars={} reserve={} frame={} args={} unnamed_base={} collapse={}",
        object.max_variable_count,
        object.variable_reserve_count,
        object.max_frame_count,
        object.func_decl_arg_count,
        object.func_decl_unnamed_arg_array_base,
        object
            .func_decl_collapse_base
            .map(|base| base.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    let _ = writeln!(
        out,
        "  setter={} getter={} super={}",
        opt_index(object.prop_setter),
        opt_index(object.prop_getter),
        opt_index(object.super_class_getter)
    );
    if !object.data_slots.is_empty() {
        let _ = writeln!(out, "  data slots:");
        for (slot_index, slot) in object.data_slots.iter().enumerate() {
            let _ = writeln!(
                out,
                "    *{slot_index}: {:?} = {}",
                slot.ty,
                slot_value(file, slot)
            );
        }
    }
    if !object.properties.is_empty() {
        let _ = writeln!(out, "  properties:");
        for property in &object.properties {
            let property_name = file
                .data
                .strings
                .get(property.name)
                .map(|name| format!("{name:?}"))
                .unwrap_or_else(|| format!("<invalid #{}>", property.name));
            let _ = writeln!(out, "    {property_name} -> object {}", property.object);
        }
    }
    if !object.source_positions.is_empty() {
        let _ = writeln!(out, "  source positions:");
        for position in &object.source_positions {
            let _ = writeln!(
                out,
                "    code {} -> source {}",
                position.code_pos, position.source_pos
            );
        }
    }
    let _ = writeln!(out, "  code:");
    match object.decode_instructions() {
        Ok(instructions) => {
            for inst in &instructions {
                let _ = writeln!(out, "{}", disassemble_instruction(file, object, inst));
            }
        }
        Err(error) => {
            let _ = writeln!(out, "<disasm error: {}>", error.message);
        }
    }
}

fn render_data_pool(file: &BytecodeFile, out: &mut String) {
    let data = &file.data;
    let _ = writeln!(out, ".data");
    let _ = writeln!(
        out,
        "  pools: bytes={} shorts={} integers={} longs={} reals={} strings={} octets={}",
        data.bytes.len(),
        data.shorts.len(),
        data.integers.len(),
        data.longs.len(),
        data.reals.len(),
        data.strings.len(),
        data.octets.len()
    );
    render_pool(out, "byte", &data.bytes, i8::to_string);
    render_pool(out, "short", &data.shorts, i16::to_string);
    render_pool(out, "integer", &data.integers, i32::to_string);
    render_pool(out, "long", &data.longs, i64::to_string);
    render_pool(out, "real", &data.reals, |real| format!("{real:?}"));
    render_pool(out, "string", &data.strings, |text| format!("{text:?}"));
    render_pool(out, "octet", &data.octets, |bytes| {
        format!("<{} bytes>", bytes.len())
    });
}

fn render_pool<T>(out: &mut String, label: &str, values: &[T], format: impl Fn(&T) -> String) {
    if values.is_empty() {
        return;
    }
    let _ = writeln!(out, "  {label} pool:");
    for (index, value) in values.iter().enumerate() {
        let _ = writeln!(out, "    {index}: {}", format(value));
    }
}

fn slot_value(file: &BytecodeFile, slot: &DataSlot) -> String {
    match slot.value(file) {
        Ok(Variant::String(value)) => format!("{value:?}"),
        Ok(Variant::Octet(bytes)) => format!("<{} bytes>", bytes.len()),
        Ok(Variant::CodeObject(index)) => format!("object {index}"),
        Ok(value) => value.to_string(),
        Err(_) => "<invalid>".to_string(),
    }
}

fn opt_index(index: Option<usize>) -> String {
    index
        .map(|index| index.to_string())
        .unwrap_or_else(|| "-".to_string())
}

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
