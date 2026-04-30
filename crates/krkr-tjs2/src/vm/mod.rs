use std::{collections::BTreeMap, marker::PhantomData, sync::Arc};

use crate::bytecode::{BytecodeFile, CodeObject, Instruction};
use crate::error::{Result, TjsError, TjsSourceLocation, TjsStackFrame};
use crate::runtime::{
    Closure, NativeFunction, NoHost, Object, ObjectHandle, Runtime, TjsHost, Variant,
};

mod dispatch;
mod frame;
mod opcode;

use frame::{ExceptionEntry, Frame};
use opcode::{branch_index, next_instruction_index};

pub fn execute_bytecode(bytes: &[u8]) -> Result<Variant> {
    Runtime::new().execute_bytecode(bytes)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct DispatchFlags {
    ensure: bool,
    must_exist: bool,
    ignore_prop: bool,
    hidden: bool,
}

pub struct Vm<'bc, 'rt, H: TjsHost = NoHost> {
    file_id: usize,
    file: Arc<BytecodeFile>,
    runtime: &'rt mut Runtime<H>,
    code_handles: Vec<ObjectHandle>,
    _file_lifetime: PhantomData<&'bc BytecodeFile>,
}

impl<'bc, 'rt, H: TjsHost + 'static> Vm<'bc, 'rt, H> {
    pub fn new(file_id: usize, runtime: &'rt mut Runtime<H>) -> Result<Self> {
        let file = runtime.script_file(file_id)?;
        let code_handles = runtime.script_code_handles(file_id)?;
        let mut vm = Self {
            file_id,
            file,
            runtime,
            code_handles,
            _file_lifetime: PhantomData,
        };
        vm.register_code_object_properties();
        Ok(vm)
    }

    pub fn set_global_member(&mut self, name: impl Into<String>, value: Variant) {
        let value = self.materialize_code_object(value);
        let global = self.runtime.global;
        self.runtime.heap[global.0].set(name, value);
    }

    pub fn register_global_native<F>(
        &mut self,
        name: impl Into<String>,
        function: F,
    ) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        self.runtime.register_global_native(name, function)
    }

    pub fn global_member(&self, name: &str) -> Variant {
        self.runtime.heap[self.runtime.global.0].get(name)
    }

    pub fn runtime(&self) -> &Runtime<H> {
        self.runtime
    }

    pub fn runtime_mut(&mut self) -> &mut Runtime<H> {
        self.runtime
    }

    pub fn execute_top_level(&mut self) -> Result<Variant> {
        let index = self
            .file
            .top_level
            .ok_or_else(|| TjsError::runtime("bytecode has no top-level object"))?;
        self.execute_object_with_this(index, Vec::new(), Some(self.runtime.global))
    }

    pub fn execute_object(&mut self, object_index: usize, args: Vec<Variant>) -> Result<Variant> {
        self.execute_object_with_this(object_index, args, Some(self.runtime.global))
    }

    pub(super) fn execute_file_object_with_this(
        &mut self,
        file_id: usize,
        object_index: usize,
        args: Vec<Variant>,
        this_obj: Option<ObjectHandle>,
    ) -> Result<Variant> {
        if file_id == self.file_id {
            return self.execute_object_with_this(object_index, args, this_obj);
        }
        let mut vm = Vm::new(file_id, self.runtime)?;
        vm.execute_object_with_this(object_index, args, this_obj)
    }

    pub(super) fn execute_object_with_this(
        &mut self,
        object_index: usize,
        args: Vec<Variant>,
        this_obj: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let object = self
            .file
            .objects
            .get(object_index)
            .cloned()
            .ok_or_else(|| TjsError::runtime(format!("object {object_index} does not exist")))?;
        let instructions = object.decode_instructions()?;
        let offset_to_index = instructions
            .iter()
            .enumerate()
            .map(|(index, inst)| (inst.offset, index))
            .collect::<BTreeMap<_, _>>();
        self.runtime
            .enter_call_frame()
            .map_err(|error| error.with_stack_frame(self.stack_frame(&object, 0)))?;
        let result = (|| {
            let caller_args = args.clone();
            let global = self.runtime.global;
            let this_obj = this_obj.or(Some(global));
            let this_proxy = self.runtime.alloc_proxy_bound(this_obj, global, None);
            let super_class = self.super_class_for_object(&object);
            let super_proxy = self.runtime.alloc_proxy_bound(
                super_class.or(this_obj),
                global,
                super_class.and(this_obj),
            );
            let mut frame = Frame::new(&object, args, this_obj, this_proxy, super_proxy)?;
            if let Some(collapse_base) = object.func_decl_collapse_base {
                let base = collapse_base as usize;
                let rest = if caller_args.len() > base {
                    caller_args[base..].to_vec()
                } else {
                    Vec::new()
                };
                let array = self.runtime.alloc_object(Object::array(rest));
                frame.set(-4 - collapse_base as i16, Variant::Object(array))?;
            }

            let mut pc = offset_to_index.get(&(0_usize)).copied().unwrap_or(0);
            while pc < instructions.len() {
                let inst = &instructions[pc];
                let next_pc = next_instruction_index(&offset_to_index, &instructions, pc).map_err(
                    |error| error.with_stack_frame(self.stack_frame(&object, inst.offset)),
                )?;
                match self
                    .execute_instruction(&object, &mut frame, inst, next_pc, &offset_to_index)
                    .map_err(|error| {
                        error.with_stack_frame(self.stack_frame(&object, inst.offset))
                    })? {
                    Step::Next(next) => pc = next,
                    Step::Return(value) => return Ok(value),
                }
            }

            Ok(frame.result)
        })();
        self.runtime.leave_call_frame();
        result
    }

    fn execute_instruction(
        &mut self,
        object: &CodeObject,
        frame: &mut Frame,
        inst: &Instruction,
        next_pc: usize,
        offset_to_index: &BTreeMap<usize, usize>,
    ) -> Result<Step> {
        let mut pc = next_pc;
        match inst.opcode {
            0 | 127 => {}
            1 => {
                let value = self.data_slot_value(object, inst.operands[1])?;
                frame.set(inst.operands[0], value)?;
            }
            2 => {
                let value = frame.get(inst.operands[1])?;
                frame.set(inst.operands[0], value)?;
            }
            3 => frame.set(inst.operands[0], Variant::Void)?,
            4 => {
                for reg in inst.operands[0]..inst.operands[0] + inst.operands[1] {
                    frame.set(reg, Variant::Void)?;
                }
            }
            5 => frame.flag = frame.get(inst.operands[0])?.is_truthy(),
            6 => frame.flag = !frame.get(inst.operands[0])?.is_truthy(),
            7 => {
                frame.flag = frame
                    .get(inst.operands[0])?
                    .normal_eq(&frame.get(inst.operands[1])?)
            }
            8 => {
                frame.flag = frame
                    .get(inst.operands[0])?
                    .discern_eq(&frame.get(inst.operands[1])?)
            }
            9 => {
                frame.flag = frame
                    .get(inst.operands[0])?
                    .less_than(&frame.get(inst.operands[1])?)?
            }
            10 => {
                frame.flag = frame
                    .get(inst.operands[0])?
                    .greater_than(&frame.get(inst.operands[1])?)?
            }
            11 => frame.set(inst.operands[0], Variant::Integer(i64::from(frame.flag)))?,
            12 => frame.set(inst.operands[0], Variant::Integer(i64::from(!frame.flag)))?,
            13 => {
                let value = frame.get(inst.operands[0])?.logical_not();
                frame.set(inst.operands[0], value)?;
            }
            14 => frame.flag = !frame.flag,
            15 => {
                if frame.flag {
                    pc = branch_index(offset_to_index, inst)?;
                }
            }
            16 => {
                if !frame.flag {
                    pc = branch_index(offset_to_index, inst)?;
                }
            }
            17 => pc = branch_index(offset_to_index, inst)?,
            18 | 22 => {
                let value = if inst.opcode == 18 {
                    frame.get(inst.operands[0])?.increment()?
                } else {
                    frame.get(inst.operands[0])?.decrement()?
                };
                frame.set(inst.operands[0], value)?;
            }
            19..=25 => self.execute_update_property(frame, object, inst)?,
            26..=81 => self.execute_binary(frame, object, inst)?,
            82 => {
                let value = frame.get(inst.operands[0])?.bit_not()?;
                frame.set(inst.operands[0], value)?;
            }
            83 => {
                let value = Variant::String(frame.get(inst.operands[0])?.typeof_name().to_string());
                frame.set(inst.operands[0], value)?;
            }
            84 => {
                let value = self.typeof_direct(frame, object, inst, DispatchFlags::must_exist())?;
                frame.set(inst.operands[0], value)?;
            }
            85 => {
                let value = self.typeof_indirect(frame, inst, DispatchFlags::must_exist())?;
                frame.set(inst.operands[0], value)?;
            }
            86 | 87 => {
                return Err(TjsError::runtime(
                    "eval/eexp bytecode execution is not wired to a host script context yet",
                ));
            }
            88 => {
                let class_name = frame.get(inst.operands[1])?.to_tjs_string()?;
                let value = self.instance_of(&frame.get(inst.operands[0])?, &class_name);
                frame.set(inst.operands[0], Variant::Integer(i64::from(value)))?;
            }
            89 => {
                let value = frame.get(inst.operands[0])?.char_code_of()?;
                frame.set(inst.operands[0], value)?;
            }
            90 => {
                let value = frame.get(inst.operands[0])?.char_from_code()?;
                frame.set(inst.operands[0], value)?;
            }
            91 => {
                let value = frame.get(inst.operands[0])?.to_number_variant()?;
                frame.set(inst.operands[0], value)?;
            }
            92 => {
                let value = frame.get(inst.operands[0])?.negate()?;
                frame.set(inst.operands[0], value)?;
            }
            93 => {
                let value = match self.resolve_object(frame.get(inst.operands[0])?) {
                    Ok(handle) => {
                        self.runtime.heap[handle.0].valid = false;
                        Variant::Integer(1)
                    }
                    Err(_) => Variant::Integer(0),
                };
                frame.set(inst.operands[0], value)?;
            }
            94 => {
                let value = match self.resolve_object(frame.get(inst.operands[0])?) {
                    Ok(handle) => Variant::Integer(i64::from(self.runtime.heap[handle.0].valid)),
                    Err(_) => Variant::Integer(1),
                };
                frame.set(inst.operands[0], value)?;
            }
            95 => {
                let value = Variant::Integer(frame.get(inst.operands[0])?.to_integer()?);
                frame.set(inst.operands[0], value)?;
            }
            96 => {
                let value = Variant::Real(frame.get(inst.operands[0])?.to_real()?);
                frame.set(inst.operands[0], value)?;
            }
            97 => {
                let value = Variant::String(frame.get(inst.operands[0])?.to_tjs_string()?);
                frame.set(inst.operands[0], value)?;
            }
            98 => {
                let value = frame.get(inst.operands[0])?.to_octet()?;
                frame.set(inst.operands[0], value)?;
            }
            99 | 102 => {
                let callee = frame.get(inst.operands[1])?;
                let args = self.materialize_call_args(frame, object, inst.call_args.as_ref())?;
                let value = self.call_value(callee, frame.this_obj, args, inst.opcode == 102)?;
                if inst.operands[0] != 0 {
                    frame.set(inst.operands[0], value)?;
                }
            }
            100 => {
                let object_value = frame.get(inst.operands[1])?;
                let name = self.data_slot_string(object, inst.operands[2])?;
                let args = self.materialize_call_args(frame, object, inst.call_args.as_ref())?;
                let value = self.call_member_direct(object_value, &name, args, inst.operands[0])?;
                if inst.operands[0] != 0 {
                    frame.set(inst.operands[0], value)?;
                }
            }
            101 => {
                let object_value = frame.get(inst.operands[1])?;
                let name = self.key_from_variant(&frame.get(inst.operands[2])?)?;
                let args = self.materialize_call_args(frame, object, inst.call_args.as_ref())?;
                let value = self.call_member_direct(object_value, &name, args, inst.operands[0])?;
                if inst.operands[0] != 0 {
                    frame.set(inst.operands[0], value)?;
                }
            }
            103 | 110 => {
                let flags = if inst.opcode == 110 {
                    DispatchFlags::ignore_prop()
                } else {
                    DispatchFlags::default()
                };
                let object_value = frame.get(inst.operands[1])?;
                let name = self.data_slot_string(object, inst.operands[2])?;
                let value = self.prop_get(object_value, &name, flags, frame.this_obj)?;
                frame.set(inst.operands[0], value)?;
            }
            104..=106 | 111 => {
                let flags = match inst.opcode {
                    105 => DispatchFlags::ensure(),
                    106 => DispatchFlags::ensure_hidden(),
                    111 => DispatchFlags::ensure_ignore_prop(),
                    _ => DispatchFlags::default(),
                };
                let object_value = frame.get(inst.operands[0])?;
                let name = self.data_slot_string(object, inst.operands[1])?;
                let value = frame.get(inst.operands[2])?;
                self.prop_set(object_value, &name, value, flags, frame.this_obj)?;
            }
            107 | 112 => {
                let flags = if inst.opcode == 112 {
                    DispatchFlags::ignore_prop()
                } else {
                    DispatchFlags::default()
                };
                let object_value = frame.get(inst.operands[1])?;
                let key = self.key_from_variant(&frame.get(inst.operands[2])?)?;
                let value = self.prop_get(object_value, &key, flags, frame.this_obj)?;
                frame.set(inst.operands[0], value)?;
            }
            108 | 109 | 113 => {
                let flags = match inst.opcode {
                    109 => DispatchFlags::ensure(),
                    113 => DispatchFlags::ensure_ignore_prop(),
                    _ => DispatchFlags::default(),
                };
                let object_value = frame.get(inst.operands[0])?;
                let key = self.key_from_variant(&frame.get(inst.operands[1])?)?;
                let value = frame.get(inst.operands[2])?;
                self.prop_set(object_value, &key, value, flags, frame.this_obj)?;
            }
            114 => {
                let object_value = frame.get(inst.operands[0])?;
                let value = frame.get(inst.operands[1])?;
                self.default_prop_set(object_value, value, frame.this_obj)?;
            }
            115 => {
                let object_value = frame.get(inst.operands[1])?;
                let value = self.default_prop_get(object_value, frame.this_obj)?;
                frame.set(inst.operands[0], value)?;
            }
            116 => {
                let object_value = frame.get(inst.operands[1])?;
                let name = self.data_slot_string(object, inst.operands[2])?;
                let value = self.delete_member(object_value, &name)?;
                if inst.operands[0] != 0 {
                    frame.set(inst.operands[0], Variant::Integer(i64::from(value)))?;
                }
            }
            117 => {
                let object_value = frame.get(inst.operands[1])?;
                let key = self.key_from_variant(&frame.get(inst.operands[2])?)?;
                let value = self.delete_member(object_value, &key)?;
                if inst.operands[0] != 0 {
                    frame.set(inst.operands[0], Variant::Integer(i64::from(value)))?;
                }
            }
            118 => frame.result = frame.get(inst.operands[0])?,
            119 => return Ok(Step::Return(frame.result.clone())),
            120 => {
                let catch_pc = branch_index(offset_to_index, inst)?;
                frame.entries.push(ExceptionEntry {
                    catch_pc,
                    exception_reg: inst.operands[1],
                });
            }
            121 => {
                frame.entries.pop();
            }
            122 => {
                let thrown = frame.get(inst.operands[0])?;
                let Some(entry) = frame.entries.pop() else {
                    return Err(TjsError::runtime(format!("uncaught exception {thrown}")));
                };
                frame.set(entry.exception_reg, thrown)?;
                pc = entry.catch_pc;
            }
            123 => {
                let mut value = frame.get(inst.operands[0])?;
                let this = self.resolve_object(frame.get(inst.operands[1])?)?;
                self.change_this(&mut value, this)?;
                frame.set(inst.operands[0], value)?;
            }
            124 => frame.set(inst.operands[0], Variant::Object(self.runtime.global))?,
            125 => {
                let object_handle = self.resolve_object(frame.get(inst.operands[0])?)?;
                let info = frame.get(inst.operands[1])?;
                let mut copied_object_infos = false;
                if let Ok(info_handle) = self.resolve_object(info.clone()) {
                    self.runtime.heap[object_handle.0].super_class = Some(info_handle);
                    let infos = self.runtime.heap[info_handle.0].class_infos.clone();
                    copied_object_infos = !infos.is_empty();
                    for info in infos {
                        self.add_class_info(object_handle, info);
                    }
                }
                if !copied_object_infos {
                    self.add_class_info(object_handle, info.to_tjs_string()?);
                }
            }
            126 => {
                let Some(dest) = frame.this_obj else {
                    return Err(TjsError::runtime(
                        "regmember has no destination this object",
                    ));
                };
                self.register_object_members(object, dest)?;
            }
            _ => {
                return Err(TjsError::runtime(format!(
                    "opcode {} ({}) is not implemented in the VM",
                    inst.opcode,
                    inst.mnemonic()
                )));
            }
        }
        Ok(Step::Next(pc))
    }

    fn register_code_object_properties(&mut self) {
        for (object_index, object) in self.file.objects.iter().enumerate() {
            if let Some(parent_index) = object.parent {
                let parent_handle = self.code_handles[parent_index];
                let object_handle = self.code_handles[object_index];
                let Some(name) = object.name(self.file.as_ref()).map(str::to_string) else {
                    continue;
                };
                let closure = Variant::Closure(Closure::new(object_handle, Some(parent_handle)));
                self.runtime.heap[parent_handle.0].set(name, closure);
            }

            let owner_handle = self.code_handles[object_index];
            for property in &object.properties {
                let Some(name) = self.file.data.strings.get(property.name).cloned() else {
                    continue;
                };
                let property_handle = self.code_handles[property.object];
                let closure = Variant::Closure(Closure::new(property_handle, Some(owner_handle)));
                self.runtime.heap[owner_handle.0].set(name, closure);
            }
        }
    }

    pub(super) fn data_slot_value(&self, object: &CodeObject, data_index: i16) -> Result<Variant> {
        let index = usize::try_from(data_index)
            .map_err(|_| TjsError::runtime(format!("negative data index {data_index}")))?;
        let value = object
            .data_slots
            .get(index)
            .ok_or_else(|| TjsError::runtime(format!("data slot {index} does not exist")))?
            .value(&self.file)?;
        Ok(self.materialize_code_object(value))
    }

    pub(super) fn data_slot_string(&self, object: &CodeObject, data_index: i16) -> Result<String> {
        match self.data_slot_value(object, data_index)? {
            Variant::String(value) => Ok(value),
            other => Err(TjsError::runtime(format!(
                "data slot {data_index} is {other}, expected string"
            ))),
        }
    }

    pub(super) fn materialize_code_object(&self, value: Variant) -> Variant {
        match value {
            Variant::CodeObject(index) => self
                .code_handles
                .get(index)
                .copied()
                .map(|handle| Variant::Closure(Closure::new(handle, None)))
                .unwrap_or(Variant::CodeObject(index)),
            value => value,
        }
    }

    fn super_class_for_object(&self, object: &CodeObject) -> Option<ObjectHandle> {
        let class_object = object.parent?;
        let class_handle = *self.code_handles.get(class_object)?;
        self.runtime.heap[class_handle.0].super_class
    }

    pub(super) fn value_debug_type(&self, value: &Variant) -> String {
        match value {
            Variant::Void => "void".to_string(),
            Variant::Null => "null".to_string(),
            Variant::Integer(_) => "Integer".to_string(),
            Variant::Real(_) => "Real".to_string(),
            Variant::String(_) => "String".to_string(),
            Variant::Octet(_) => "Octet".to_string(),
            Variant::CodeObject(index) => format!("CodeObject#{index}"),
            Variant::Closure(closure) => self.object_debug_type(closure.object, "closure"),
            Variant::Object(handle) => self.object_debug_type(*handle, "object"),
        }
    }

    pub(super) fn object_debug_type(&self, handle: ObjectHandle, fallback: &str) -> String {
        let Some(object) = self.runtime.heap.get(handle.0) else {
            return format!("{fallback}#{}", handle.0);
        };
        let mut label = match &object.kind {
            crate::runtime::ObjectKind::Ordinary => fallback.to_string(),
            crate::runtime::ObjectKind::Proxy { .. } => "proxy".to_string(),
            crate::runtime::ObjectKind::Array { .. } => "Array".to_string(),
            crate::runtime::ObjectKind::InterCode {
                file_id,
                object_index,
                context,
            } => {
                let name = self
                    .runtime
                    .script_file(*file_id)
                    .ok()
                    .and_then(|file| {
                        file.objects
                            .get(*object_index)
                            .and_then(|object| object.name(&file).map(str::to_string))
                    })
                    .unwrap_or_else(|| "<anonymous>".to_string());
                format!("{context:?} {name}")
            }
            crate::runtime::ObjectKind::NativeFunction { .. } => "NativeFunction".to_string(),
            crate::runtime::ObjectKind::VmNativeFunction { .. } => "VmNativeFunction".to_string(),
        };
        if !object.class_infos.is_empty() {
            label.push('<');
            label.push_str(&object.class_infos.join("|"));
            label.push('>');
        }
        format!("{label}#{}", handle.0)
    }

    fn stack_frame(&self, object: &CodeObject, bytecode_offset: usize) -> TjsStackFrame {
        let storage = self
            .file
            .debug_info
            .sources
            .first()
            .map(|source| source.name.clone());
        TjsStackFrame {
            storage,
            object_name: object
                .name(self.file.as_ref())
                .unwrap_or("<anonymous>")
                .to_string(),
            context: format!("{:?}", object.context_type),
            bytecode_offset,
            source: self.source_location(object, bytecode_offset),
        }
    }

    fn source_location(
        &self,
        object: &CodeObject,
        bytecode_offset: usize,
    ) -> Option<TjsSourceLocation> {
        let position = object
            .source_positions
            .iter()
            .take_while(|position| position.code_pos as usize <= bytecode_offset)
            .last()?;
        let source = self.file.debug_info.sources.first();
        let storage = source.map(|source| source.name.clone());
        let utf16_offset = position.source_pos as usize;
        let Some(text) = source.and_then(|source| source.text.as_deref()) else {
            return Some(TjsSourceLocation {
                storage,
                line: None,
                column: None,
                utf16_offset: Some(utf16_offset),
            });
        };
        let (line, column) = line_column_for_utf16_offset(text, utf16_offset);
        Some(TjsSourceLocation {
            storage,
            line: Some(line),
            column: Some(column),
            utf16_offset: Some(utf16_offset),
        })
    }
}

fn line_column_for_utf16_offset(text: &str, utf16_offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    let mut offset = 0;
    for ch in text.chars() {
        if offset >= utf16_offset {
            break;
        }
        offset += ch.len_utf16();
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

impl DispatchFlags {
    fn ensure() -> Self {
        Self {
            ensure: true,
            ..Self::default()
        }
    }

    fn ensure_hidden() -> Self {
        Self {
            ensure: true,
            hidden: true,
            ..Self::default()
        }
    }

    fn ignore_prop() -> Self {
        Self {
            ignore_prop: true,
            ..Self::default()
        }
    }

    fn ensure_ignore_prop() -> Self {
        Self {
            ensure: true,
            ignore_prop: true,
            ..Self::default()
        }
    }

    fn must_exist() -> Self {
        Self {
            must_exist: true,
            ..Self::default()
        }
    }
}

enum Step {
    Next(usize),
    Return(Variant),
}

#[cfg(test)]
mod tests {
    use crate::bytecode::{
        BytecodeContextType, BytecodeFile, CodeObject, DataPool, DataSlot, DataSlotType,
    };

    use super::*;

    #[test]
    fn executes_integer_return_fixture() {
        let file = file_with_code(
            vec![DataSlot {
                ty: DataSlotType::Integer,
                index: 0,
            }],
            DataPool {
                integers: vec![42],
                strings: vec!["global".to_string()],
                ..DataPool::default()
            },
            vec![1, 0, 0, 118, 0, 119],
            1,
        );
        let mut runtime = Runtime::new();
        let file_id = runtime.install_script_file(Arc::new(file));
        let mut vm = Vm::new(file_id, &mut runtime).expect("vm");
        assert_eq!(
            vm.execute_top_level().expect("execute"),
            Variant::Integer(42)
        );
    }

    #[test]
    fn executes_property_get_and_set_fixture() {
        let file = file_with_code(
            vec![
                DataSlot {
                    ty: DataSlotType::String,
                    index: 0,
                },
                DataSlot {
                    ty: DataSlotType::Integer,
                    index: 0,
                },
            ],
            DataPool {
                integers: vec![7],
                strings: vec!["foo".to_string(), "global".to_string()],
                ..DataPool::default()
            },
            vec![124, 0, 1, 1, 1, 111, 0, 0, 1, 103, 1, 0, 0, 118, 1, 119],
            2,
        );
        let mut runtime = Runtime::new();
        let file_id = runtime.install_script_file(Arc::new(file));
        let mut vm = Vm::new(file_id, &mut runtime).expect("vm");
        assert_eq!(
            vm.execute_top_level().expect("execute"),
            Variant::Integer(7)
        );
        assert_eq!(vm.global_member("foo"), Variant::Integer(7));
    }

    #[test]
    fn executes_try_catch_fixture() {
        let file = file_with_code(
            vec![DataSlot {
                ty: DataSlotType::String,
                index: 0,
            }],
            DataPool {
                strings: vec!["boom".to_string(), "global".to_string()],
                ..DataPool::default()
            },
            vec![120, 8, 0, 1, 1, 0, 122, 1, 118, 0, 119],
            2,
        );
        let mut runtime = Runtime::new();
        let file_id = runtime.install_script_file(Arc::new(file));
        let mut vm = Vm::new(file_id, &mut runtime).expect("vm");
        assert_eq!(
            vm.execute_top_level().expect("execute"),
            Variant::String("boom".to_string())
        );
    }

    #[test]
    fn executes_unary_and_typeof_fixture() {
        let file = file_with_code(
            vec![DataSlot {
                ty: DataSlotType::Integer,
                index: 0,
            }],
            DataPool {
                integers: vec![41],
                strings: vec!["global".to_string()],
                ..DataPool::default()
            },
            vec![1, 0, 0, 18, 0, 83, 0, 118, 0, 119],
            1,
        );
        let mut runtime = Runtime::new();
        let file_id = runtime.install_script_file(Arc::new(file));
        let mut vm = Vm::new(file_id, &mut runtime).expect("vm");
        assert_eq!(
            vm.execute_top_level().expect("execute"),
            Variant::String("Integer".to_string())
        );
    }

    fn file_with_code(
        data_slots: Vec<DataSlot>,
        data: DataPool,
        code_words: Vec<i16>,
        max_frame_count: u32,
    ) -> BytecodeFile {
        BytecodeFile {
            data,
            objects: vec![CodeObject {
                parent: None,
                name: 0,
                context_type: BytecodeContextType::TopLevel,
                max_variable_count: 0,
                variable_reserve_count: 2,
                max_frame_count,
                func_decl_arg_count: 0,
                func_decl_unnamed_arg_array_base: 0,
                func_decl_collapse_base: None,
                prop_setter: None,
                prop_getter: None,
                super_class_getter: None,
                source_positions: Vec::new(),
                code_words,
                data_slots,
                super_class_getter_pointers: Vec::new(),
                properties: Vec::new(),
            }],
            top_level: Some(0),
            debug_info: Default::default(),
        }
    }
}
