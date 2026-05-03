use std::{collections::BTreeMap, sync::Arc};

use crate::bytecode::{BytecodeFile, CodeObject, Instruction};
use crate::error::{Result, TjsError};
use crate::runtime::{ObjectHandle, Variant};

pub(super) struct CallFrame {
    pub(super) file_id: usize,
    pub(super) file: Arc<BytecodeFile>,
    pub(super) code_handles: Vec<ObjectHandle>,
    pub(super) object: CodeObject,
    pub(super) instructions: Arc<[Instruction]>,
    pub(super) offset_to_index: Arc<BTreeMap<usize, usize>>,
    pub(super) frame: Frame,
    pub(super) pc: usize,
    pub(super) continuation: Continuation,
}

#[derive(Debug)]
pub(super) enum Continuation {
    Root,
    CallerRegister {
        dest: Option<i16>,
    },
    ReturnFixed {
        value: Variant,
        target: Box<Continuation>,
    },
    ClassBody {
        instance: ObjectHandle,
        class_handle: ObjectHandle,
        class_name: String,
        constructor_args: Vec<Variant>,
        run_constructor: bool,
        target: Box<Continuation>,
    },
}

pub(super) struct Frame {
    pub(super) regs: Vec<Variant>,
    pub(super) negative: Vec<Variant>,
    pub(super) caller_args: Vec<Variant>,
    pub(super) result: Variant,
    pub(super) flag: bool,
    pub(super) entries: Vec<ExceptionEntry>,
    pub(super) this_obj: Option<ObjectHandle>,
    pub(super) this_proxy: ObjectHandle,
    pub(super) super_proxy: ObjectHandle,
}

impl Frame {
    pub(super) fn new(
        object: &CodeObject,
        args: Vec<Variant>,
        this_obj: Option<ObjectHandle>,
        this_proxy: ObjectHandle,
        super_proxy: ObjectHandle,
    ) -> Result<Self> {
        let frame_len = object.max_frame_count.saturating_add(1).max(1) as usize;
        let mut regs = vec![Variant::Void; frame_len];
        regs[0] = Variant::Void;

        let negative_len = (object.max_variable_count
            + object.variable_reserve_count
            + object.func_decl_arg_count
            + 8) as usize
            + args.len();
        let mut negative = vec![Variant::Void; negative_len.max(1)];
        for (index, slot) in negative
            .iter_mut()
            .enumerate()
            .take(object.func_decl_arg_count as usize)
        {
            *slot = args.get(index).cloned().unwrap_or_default();
        }
        Ok(Self {
            regs,
            negative,
            caller_args: args,
            result: Variant::Void,
            flag: false,
            entries: Vec::new(),
            this_obj,
            this_proxy,
            super_proxy,
        })
    }

    pub(super) fn get(&self, reg: i16) -> Result<Variant> {
        if reg >= 0 {
            return self
                .regs
                .get(reg as usize)
                .cloned()
                .ok_or_else(|| TjsError::runtime(format!("register {reg} does not exist")));
        }
        match reg {
            -1 => Ok(self.this_obj.map(Variant::Object).unwrap_or(Variant::Null)),
            -2 => Ok(Variant::Object(self.this_proxy)),
            -3 => Ok(Variant::Object(self.super_proxy)),
            value => {
                let index = usize::try_from((-4 - value) as i32).expect("nonnegative");
                Ok(self.negative.get(index).cloned().unwrap_or_default())
            }
        }
    }

    pub(super) fn set(&mut self, reg: i16, value: Variant) -> Result<()> {
        if reg >= 0 {
            let slot = self
                .regs
                .get_mut(reg as usize)
                .ok_or_else(|| TjsError::runtime(format!("register {reg} does not exist")))?;
            *slot = value;
            return Ok(());
        }
        match reg {
            -3..=-1 => Err(TjsError::runtime(format!(
                "writing reserved register {reg} is not supported"
            ))),
            reg_value => {
                let index = usize::try_from((-4 - reg_value) as i32).expect("nonnegative");
                if index >= self.negative.len() {
                    self.negative.resize(index + 1, Variant::Void);
                }
                self.negative[index] = value;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExceptionEntry {
    pub(super) catch_pc: usize,
    pub(super) exception_reg: i16,
}
