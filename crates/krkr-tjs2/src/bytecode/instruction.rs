use crate::error::{Result, TjsError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instruction {
    pub offset: usize,
    pub opcode: u8,
    pub operands: Vec<i16>,
    pub call_args: Option<CallArgs>,
    pub len_words: usize,
}

impl Instruction {
    pub fn mnemonic(&self) -> &'static str {
        opcode_mnemonic(self.opcode)
    }

    pub(crate) fn register_operands(&self) -> Vec<i16> {
        let mut regs = Vec::new();
        match self.opcode {
            1 => regs.push(self.operands[0]),
            2 | 7..=10 | 88 | 123 | 125 => regs.extend_from_slice(&self.operands),
            3 | 5 | 6 | 11..=13 | 18 | 22 | 82 | 83 | 86 | 87 | 89..=98 | 118 | 122 | 124 => {
                regs.push(self.operands[0])
            }
            4 => regs.push(self.operands[0]),
            19 | 23 | 84 | 103 | 110 | 116 => {
                regs.push(self.operands[0]);
                regs.push(self.operands[1]);
            }
            20 | 24 | 85 | 107 | 112 | 117 => regs.extend_from_slice(&self.operands),
            21 | 25 | 114 | 115 => regs.extend_from_slice(&self.operands),
            26..=81 => match binary_form(self.opcode) {
                BinaryForm::Slot => regs.extend_from_slice(&self.operands[0..2]),
                BinaryForm::DirectProperty => {
                    regs.push(self.operands[0]);
                    regs.push(self.operands[1]);
                    regs.push(self.operands[3]);
                }
                BinaryForm::IndirectProperty => regs.extend_from_slice(&self.operands),
                BinaryForm::DefaultProperty => regs.extend_from_slice(&self.operands),
            },
            99 | 102 => {
                regs.push(self.operands[0]);
                regs.push(self.operands[1]);
                append_call_arg_registers(&mut regs, self.call_args.as_ref());
            }
            100 | 101 => {
                regs.push(self.operands[0]);
                regs.push(self.operands[1]);
                if self.opcode == 101 {
                    regs.push(self.operands[2]);
                }
                append_call_arg_registers(&mut regs, self.call_args.as_ref());
            }
            104..=106 | 111 => {
                regs.push(self.operands[0]);
                regs.push(self.operands[2]);
            }
            108 | 109 | 113 => regs.extend_from_slice(&self.operands),
            120 => regs.push(self.operands[1]),
            0 | 14 | 15..=17 | 119 | 121 | 126 | 127 => {}
            _ => {}
        }
        regs
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallArgs {
    Normal(Vec<i16>),
    OmittedCallerArgs,
    Expanded(Vec<ExpandedArg>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpandedArg {
    pub arg_type: i16,
    pub reg: i16,
}

fn append_call_arg_registers(regs: &mut Vec<i16>, args: Option<&CallArgs>) {
    match args {
        Some(CallArgs::Normal(args)) => regs.extend(args.iter().copied()),
        Some(CallArgs::Expanded(args)) => {
            for arg in args {
                if matches!(arg.arg_type, 0 | 1) {
                    regs.push(arg.reg);
                }
            }
        }
        Some(CallArgs::OmittedCallerArgs) | None => {}
    }
}

pub(super) fn decode_instructions(code_words: &[i16]) -> Result<Vec<Instruction>> {
    let mut instructions = Vec::new();
    let mut offset = 0;
    while offset < code_words.len() {
        let opcode_raw = code_words[offset];
        let opcode = u8::try_from(opcode_raw)
            .ok()
            .filter(|opcode| *opcode <= 127)
            .ok_or_else(|| TjsError::bytecode(format!("invalid opcode {opcode_raw}")))?;
        if is_call_opcode(opcode) {
            let instruction = decode_call_instruction(code_words, offset, opcode)?;
            offset += instruction.len_words;
            instructions.push(instruction);
            continue;
        }

        let len_words = fixed_opcode_words(opcode)?;
        if offset + len_words > code_words.len() {
            return Err(TjsError::bytecode(format!(
                "instruction {opcode} at {offset} overruns code area"
            )));
        }
        let operands = code_words[offset + 1..offset + len_words].to_vec();
        instructions.push(Instruction {
            offset,
            opcode,
            operands,
            call_args: None,
            len_words,
        });
        offset += len_words;
    }
    Ok(instructions)
}

fn decode_call_instruction(code_words: &[i16], offset: usize, opcode: u8) -> Result<Instruction> {
    let header_len = if matches!(opcode, 99 | 102) { 4 } else { 5 };
    if offset + header_len > code_words.len() {
        return Err(TjsError::bytecode("call instruction overruns code area"));
    }
    let arg_count = code_words[offset + header_len - 1];
    let mut len_words = header_len;
    let call_args = if arg_count >= 0 {
        let arg_count = usize::try_from(arg_count).expect("nonnegative i16 fits usize");
        len_words += arg_count;
        if offset + len_words > code_words.len() {
            return Err(TjsError::bytecode("normal call args overrun code area"));
        }
        CallArgs::Normal(code_words[offset + header_len..offset + len_words].to_vec())
    } else if arg_count == -1 {
        CallArgs::OmittedCallerArgs
    } else if arg_count == -2 {
        if offset + header_len + 1 > code_words.len() {
            return Err(TjsError::bytecode("expanded call has no record count"));
        }
        let record_count = code_words[offset + header_len];
        let record_count = usize::try_from(record_count)
            .map_err(|_| TjsError::bytecode("negative expanded call record count"))?;
        len_words += 1 + record_count * 2;
        if offset + len_words > code_words.len() {
            return Err(TjsError::bytecode("expanded call args overrun code area"));
        }
        let mut args = Vec::with_capacity(record_count);
        let mut cursor = offset + header_len + 1;
        for _ in 0..record_count {
            args.push(ExpandedArg {
                arg_type: code_words[cursor],
                reg: code_words[cursor + 1],
            });
            cursor += 2;
        }
        CallArgs::Expanded(args)
    } else {
        return Err(TjsError::bytecode(format!(
            "invalid call argument selector {arg_count}"
        )));
    };

    let operands = code_words[offset + 1..offset + header_len - 1].to_vec();
    Ok(Instruction {
        offset,
        opcode,
        operands,
        call_args: Some(call_args),
        len_words,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BinaryForm {
    Slot,
    DirectProperty,
    IndirectProperty,
    DefaultProperty,
}

pub(super) fn binary_form(opcode: u8) -> BinaryForm {
    match (opcode - 26) % 4 {
        0 => BinaryForm::Slot,
        1 => BinaryForm::DirectProperty,
        2 => BinaryForm::IndirectProperty,
        _ => BinaryForm::DefaultProperty,
    }
}

fn fixed_opcode_words(opcode: u8) -> Result<usize> {
    Ok(match opcode {
        0 | 14 | 119 | 121 | 126 | 127 => 1,
        3 | 5 | 6 | 11..=13 | 15..=18 | 22 | 82 | 83 | 86 | 87 | 89..=98 | 118 | 122 | 124 => 2,
        1 | 2 | 4 | 7..=10 | 21 | 25 | 88 | 114 | 115 | 120 | 123 | 125 => 3,
        19 | 20 | 23 | 24 | 84 | 85 | 103..=113 | 116 | 117 => 4,
        26..=81 => match binary_form(opcode) {
            BinaryForm::Slot => 3,
            BinaryForm::DirectProperty | BinaryForm::IndirectProperty => 5,
            BinaryForm::DefaultProperty => 4,
        },
        99..=102 => {
            return Err(TjsError::bytecode(
                "variable-size opcode requested as fixed",
            ));
        }
        _ => return Err(TjsError::bytecode(format!("unknown opcode {opcode}"))),
    })
}

fn is_call_opcode(opcode: u8) -> bool {
    matches!(opcode, 99..=102)
}

pub(super) fn opcode_mnemonic(opcode: u8) -> &'static str {
    if (26..=81).contains(&opcode) {
        let bases = [
            "lor", "land", "bor", "bxor", "band", "sar", "sal", "sr", "add", "sub", "mod", "div",
            "idiv", "mul",
        ];
        let suffixes = ["", "pd", "pi", "p"];
        let family = usize::from((opcode - 26) / 4);
        let suffix = usize::from((opcode - 26) % 4);
        return match (bases[family], suffixes[suffix]) {
            ("lor", "") => "lor",
            ("lor", "pd") => "lorpd",
            ("lor", "pi") => "lorpi",
            ("lor", "p") => "lorp",
            ("land", "") => "land",
            ("land", "pd") => "landpd",
            ("land", "pi") => "landpi",
            ("land", "p") => "landp",
            ("bor", "") => "bor",
            ("bor", "pd") => "borpd",
            ("bor", "pi") => "borpi",
            ("bor", "p") => "borp",
            ("bxor", "") => "bxor",
            ("bxor", "pd") => "bxorpd",
            ("bxor", "pi") => "bxorpi",
            ("bxor", "p") => "bxorp",
            ("band", "") => "band",
            ("band", "pd") => "bandpd",
            ("band", "pi") => "bandpi",
            ("band", "p") => "bandp",
            ("sar", "") => "sar",
            ("sar", "pd") => "sarpd",
            ("sar", "pi") => "sarpi",
            ("sar", "p") => "sarp",
            ("sal", "") => "sal",
            ("sal", "pd") => "salpd",
            ("sal", "pi") => "salpi",
            ("sal", "p") => "salp",
            ("sr", "") => "sr",
            ("sr", "pd") => "srpd",
            ("sr", "pi") => "srpi",
            ("sr", "p") => "srp",
            ("add", "") => "add",
            ("add", "pd") => "addpd",
            ("add", "pi") => "addpi",
            ("add", "p") => "addp",
            ("sub", "") => "sub",
            ("sub", "pd") => "subpd",
            ("sub", "pi") => "subpi",
            ("sub", "p") => "subp",
            ("mod", "") => "mod",
            ("mod", "pd") => "modpd",
            ("mod", "pi") => "modpi",
            ("mod", "p") => "modp",
            ("div", "") => "div",
            ("div", "pd") => "divpd",
            ("div", "pi") => "divpi",
            ("div", "p") => "divp",
            ("idiv", "") => "idiv",
            ("idiv", "pd") => "idivpd",
            ("idiv", "pi") => "idivpi",
            ("idiv", "p") => "idivp",
            ("mul", "") => "mul",
            ("mul", "pd") => "mulpd",
            ("mul", "pi") => "mulpi",
            ("mul", "p") => "mulp",
            _ => unreachable!(),
        };
    }

    match opcode {
        0 => "nop",
        1 => "const",
        2 => "cp",
        3 => "cl",
        4 => "ccl",
        5 => "tt",
        6 => "tf",
        7 => "ceq",
        8 => "cdeq",
        9 => "clt",
        10 => "cgt",
        11 => "setf",
        12 => "setnf",
        13 => "lnot",
        14 => "nf",
        15 => "jf",
        16 => "jnf",
        17 => "jmp",
        18 => "inc",
        19 => "incpd",
        20 => "incpi",
        21 => "incp",
        22 => "dec",
        23 => "decpd",
        24 => "decpi",
        25 => "decp",
        82 => "bnot",
        83 => "typeof",
        84 => "typeofd",
        85 => "typeofi",
        86 => "eval",
        87 => "eexp",
        88 => "chkins",
        89 => "asc",
        90 => "chr",
        91 => "num",
        92 => "chs",
        93 => "inv",
        94 => "chkinv",
        95 => "int",
        96 => "real",
        97 => "str",
        98 => "octet",
        99 => "call",
        100 => "calld",
        101 => "calli",
        102 => "new",
        103 => "gpd",
        104 => "spd",
        105 => "spde",
        106 => "spdeh",
        107 => "gpi",
        108 => "spi",
        109 => "spie",
        110 => "gpds",
        111 => "spds",
        112 => "gpis",
        113 => "spis",
        114 => "setp",
        115 => "getp",
        116 => "deld",
        117 => "deli",
        118 => "srv",
        119 => "ret",
        120 => "entry",
        121 => "extry",
        122 => "throw",
        123 => "chgthis",
        124 => "global",
        125 => "addci",
        126 => "regmember",
        127 => "debugger",
        _ => "unknown",
    }
}
