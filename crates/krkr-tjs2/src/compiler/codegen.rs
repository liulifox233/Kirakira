use std::collections::BTreeMap;

use super::mir::{
    ArgList, ArgPart, ArrayElement, BinaryOp, CallTarget, CompareOp, Condition, ContextType,
    ConvertOp, DictionaryKey, DispatchFlags, EvalMode, MemberKey, MirConst, MirInst, MirModule,
    MirObject, ObjectId, Place, SlotId, Terminator, UnaryOp, UpdateOp, UpdateResultValue, Value,
};
use crate::bytecode::{
    BytecodeContextType, BytecodeDebugInfo, BytecodeFile, BytecodeSource, CodeObject, DataPool,
    DataSlot, DataSlotType, PropertyRegistration, SourcePosition,
};
use crate::error::{Result, TjsError};

pub fn compile_mir_to_bytecode(module: &MirModule) -> Result<BytecodeFile> {
    module.validate()?;
    let mut builder = ModuleCodegen::new(module);
    builder.compile()
}

struct ModuleCodegen<'a> {
    module: &'a MirModule,
    data: DataPool,
    object_index_by_id: BTreeMap<ObjectId, usize>,
}

impl<'a> ModuleCodegen<'a> {
    fn new(module: &'a MirModule) -> Self {
        let object_index_by_id = module
            .objects
            .iter()
            .enumerate()
            .map(|(index, object)| (object.id, index))
            .collect();
        Self {
            module,
            data: DataPool {
                strings: module.strings.clone(),
                ..DataPool::default()
            },
            object_index_by_id,
        }
    }

    fn compile(&mut self) -> Result<BytecodeFile> {
        let mut objects = Vec::with_capacity(self.module.objects.len());
        for object in &self.module.objects {
            let object = ObjectCodegen::new(self, object).compile()?;
            objects.push(object);
        }
        let top_level = Some(self.object_index(self.module.top_level)?);
        let file = BytecodeFile {
            data: self.data.clone(),
            objects,
            top_level,
            debug_info: BytecodeDebugInfo {
                sources: self
                    .module
                    .sources
                    .iter()
                    .map(|source| BytecodeSource {
                        name: source.name.clone(),
                        text: source.text.clone(),
                    })
                    .collect(),
            },
        };
        file.verify()?;
        Ok(file)
    }

    fn object_index(&self, id: ObjectId) -> Result<usize> {
        self.object_index_by_id
            .get(&id)
            .copied()
            .ok_or_else(|| TjsError::codegen(format!("object {} is not in bytecode map", id.0)))
    }

    fn string_index(&mut self, id: super::mir::StringId) -> Result<usize> {
        let text = self
            .module
            .strings
            .get(id.0 as usize)
            .cloned()
            .ok_or_else(|| TjsError::codegen(format!("string {} is missing", id.0)))?;
        Ok(self.intern_string(&text))
    }

    fn intern_string(&mut self, text: &str) -> usize {
        if let Some(index) = self.data.strings.iter().position(|value| value == text) {
            return index;
        }
        let index = self.data.strings.len();
        self.data.strings.push(text.to_string());
        index
    }

    fn add_integer(&mut self, value: i64) -> Result<DataSlot> {
        if let Ok(value) = i32::try_from(value) {
            let index = checked_i16(self.data.integers.len(), "integer pool")?;
            self.data.integers.push(value);
            Ok(DataSlot {
                ty: DataSlotType::Integer,
                index,
            })
        } else {
            let index = checked_i16(self.data.longs.len(), "long pool")?;
            self.data.longs.push(value);
            Ok(DataSlot {
                ty: DataSlotType::Long,
                index,
            })
        }
    }

    fn data_slot_for_const(&mut self, constant: &MirConst) -> Result<DataSlot> {
        Ok(match constant {
            MirConst::Void => DataSlot {
                ty: DataSlotType::Void,
                index: 0,
            },
            MirConst::NullObject => DataSlot {
                ty: DataSlotType::Object,
                index: 0,
            },
            MirConst::Integer(value) => self.add_integer(*value)?,
            MirConst::Real(value) => {
                let index = checked_i16(self.data.reals.len(), "real pool")?;
                self.data.reals.push(*value);
                DataSlot {
                    ty: DataSlotType::Real,
                    index,
                }
            }
            MirConst::String(id) => DataSlot {
                ty: DataSlotType::String,
                index: checked_i16(self.string_index(*id)?, "string pool")?,
            },
            MirConst::Octet(value) => {
                let index = checked_i16(self.data.octets.len(), "octet pool")?;
                self.data.octets.push(value.clone());
                DataSlot {
                    ty: DataSlotType::Octet,
                    index,
                }
            }
            MirConst::CodeObject(id) => DataSlot {
                ty: DataSlotType::InterObject,
                index: checked_i16(self.object_index(*id)?, "object table")?,
            },
        })
    }

    fn data_slot_for_string(&mut self, text: &str) -> Result<DataSlot> {
        Ok(DataSlot {
            ty: DataSlotType::String,
            index: checked_i16(self.intern_string(text), "string pool")?,
        })
    }
}

struct ObjectCodegen<'a, 'm> {
    module: &'m MirModule,
    module_codegen: &'a mut ModuleCodegen<'m>,
    object: &'m MirObject,
    code: Vec<i16>,
    data_slots: Vec<DataSlot>,
    source_positions: Vec<SourcePosition>,
    block_offsets: BTreeMap<super::mir::BlockId, usize>,
    patches: Vec<Patch>,
    next_reg: i16,
}

#[derive(Clone, Copy, Debug)]
struct Patch {
    inst_offset: usize,
    operand_offset: usize,
    target: super::mir::BlockId,
}

enum EncodedCallArgs {
    Normal(Vec<i16>),
    OmittedCallerArgs,
    Expanded(Vec<(i16, i16)>),
}

impl<'a, 'm> ObjectCodegen<'a, 'm> {
    fn new(module_codegen: &'a mut ModuleCodegen<'m>, object: &'m MirObject) -> Self {
        let next_reg = 1 + checked_i16_lossy(object.frame.temps.len());
        Self {
            module: module_codegen.module,
            module_codegen,
            object,
            code: Vec::new(),
            data_slots: Vec::new(),
            source_positions: Vec::new(),
            block_offsets: BTreeMap::new(),
            patches: Vec::new(),
            next_reg,
        }
    }

    fn compile(mut self) -> Result<CodeObject> {
        for block in &self.object.blocks {
            self.record_block_source_position(block);
            self.block_offsets.insert(block.id, self.code.len());
            for region in self
                .object
                .exception_regions
                .iter()
                .filter(|region| region.entry == block.id)
            {
                let reg = self.slot_reg(region.exception_slot)?;
                self.emit_entry(region.catch, reg)?;
            }
            for inst in &block.insts {
                self.emit_inst(inst)?;
            }
            self.emit_terminator(&block.terminator)?;
        }

        self.patch_branches()?;

        let name = self.module_codegen.string_index(self.object.name)?;
        let parent = self
            .object
            .parent
            .map(|id| self.module_codegen.object_index(id))
            .transpose()?;
        let prop_setter = self
            .object
            .prop_setter
            .map(|id| self.module_codegen.object_index(id))
            .transpose()?;
        let prop_getter = self
            .object
            .prop_getter
            .map(|id| self.module_codegen.object_index(id))
            .transpose()?;
        let super_class_getter = self
            .object
            .super_class_getter
            .map(|id| self.module_codegen.object_index(id))
            .transpose()?;
        let properties = self
            .object
            .properties
            .iter()
            .map(|property| {
                Ok(PropertyRegistration {
                    name: self.module_codegen.string_index(property.name)?,
                    object: self.module_codegen.object_index(property.object)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(CodeObject {
            parent,
            name,
            context_type: context_type(self.object.context),
            max_variable_count: (self.object.args.declared.len() + self.object.frame.locals.len())
                as u32,
            variable_reserve_count: self.object.frame.variable_reserve_count,
            max_frame_count: self.next_reg.saturating_sub(1).max(0) as u32,
            func_decl_arg_count: self.object.args.declared.len() as u32,
            func_decl_unnamed_arg_array_base: self.object.args.unnamed_arg_array_base.unwrap_or(0),
            func_decl_collapse_base: self.object.args.collapse_base,
            prop_setter,
            prop_getter,
            super_class_getter,
            source_positions: self.source_positions,
            code_words: self.code,
            data_slots: self.data_slots,
            super_class_getter_pointers: Vec::new(),
            properties,
        })
    }

    fn emit_inst(&mut self, inst: &MirInst) -> Result<()> {
        match inst {
            MirInst::Nop => self.emit_op(0),
            MirInst::LoadConst { dst, value } => {
                let dst = self.slot_reg(*dst)?;
                let data = self.data_for_const(*value)?;
                self.emit(&[1, dst, data]);
            }
            MirInst::Copy { dst, src } => self.emit_value_to(*dst, *src)?,
            MirInst::Clear { dst } => {
                let dst = self.slot_reg(*dst)?;
                self.emit(&[3, dst]);
            }
            MirInst::ReadPlace { dst, place } => self.emit_read_place(*dst, place)?,
            MirInst::Assign {
                place,
                value,
                result,
            } => self.emit_assign(place, *value, *result)?,
            MirInst::AssignOp {
                place,
                op,
                rhs,
                result,
            } => self.emit_assign_op(place, *op, *rhs, *result)?,
            MirInst::Swap { left, right } => {
                let left_value = self.alloc_reg();
                let right_value = self.alloc_reg();
                self.emit_read_place_to_reg(left_value, left)?;
                self.emit_read_place_to_reg(right_value, right)?;
                self.emit_assign_reg(left, right_value, None)?;
                self.emit_assign_reg(right, left_value, None)?;
            }
            MirInst::Update {
                place,
                op,
                result,
                result_value,
            } => self.emit_update(place, *op, *result, *result_value)?,
            MirInst::Unary { dst, op, src } => self.emit_unary(*dst, *op, *src)?,
            MirInst::Binary { dst, op, lhs, rhs } => self.emit_binary(*dst, *op, *lhs, *rhs)?,
            MirInst::Compare { dst, op, lhs, rhs } => {
                self.emit_compare_flag(*op, *lhs, *rhs)?;
                self.emit(&[11, self.slot_reg(*dst)?]);
            }
            MirInst::Convert { dst, op, src } => self.emit_convert(*dst, *op, *src)?,
            MirInst::ToBoolean { dst, src } => {
                let src = self.ensure_reg(*src)?;
                self.emit(&[5, src, 11, self.slot_reg(*dst)?]);
            }
            MirInst::TypeOfValue { dst, value } => {
                self.emit_value_to(*dst, *value)?;
                self.emit(&[83, self.slot_reg(*dst)?]);
            }
            MirInst::TypeOfPlace { dst, place } => self.emit_typeof_place(*dst, place)?,
            MirInst::Delete { dst, place } => self.emit_delete(*dst, place)?,
            MirInst::Invalidate { dst, target } => {
                self.emit_value_to(*dst, *target)?;
                self.emit(&[93, self.slot_reg(*dst)?]);
            }
            MirInst::CheckInvalidated { dst, target } => {
                self.emit_value_to(*dst, *target)?;
                self.emit(&[94, self.slot_reg(*dst)?]);
            }
            MirInst::IsInstanceOf {
                dst,
                value,
                class_name,
            } => {
                self.emit_value_to(*dst, *value)?;
                let class_name = self.ensure_reg(*class_name)?;
                self.emit(&[88, self.slot_reg(*dst)?, class_name]);
            }
            MirInst::Call { dst, target, args } => self.emit_call(*dst, target, args)?,
            MirInst::New { dst, callee, args } => self.emit_new(*dst, *callee, args)?,
            MirInst::Eval { dst, source, mode } => {
                let reg = if let Some(dst) = dst {
                    self.emit_value_to(*dst, *source)?;
                    self.slot_reg(*dst)?
                } else {
                    self.ensure_reg(*source)?
                };
                self.emit(&[
                    if *mode == EvalMode::Expression {
                        86
                    } else {
                        87
                    },
                    reg,
                ]);
            }
            MirInst::ChangeThis {
                dst,
                closure,
                this_obj,
            } => {
                self.emit_value_to(*dst, *closure)?;
                let this_obj = self.ensure_reg(*this_obj)?;
                self.emit(&[123, self.slot_reg(*dst)?, this_obj]);
            }
            MirInst::LoadGlobal { dst } => self.emit(&[124, self.slot_reg(*dst)?]),
            MirInst::AddClassInfo { object, info } => {
                let object = self.ensure_reg(*object)?;
                let info = self.ensure_reg(*info)?;
                self.emit(&[125, object, info]);
            }
            MirInst::RegisterDeclaration {
                name,
                value,
                change_this,
                ..
            } => {
                if let Some(value) = value {
                    let value_reg = self.ensure_reg(*value)?;
                    if *change_this {
                        self.emit(&[123, value_reg, -1]);
                    }
                    let data = self.data_for_string_id(*name)?;
                    self.emit(&[111, -1, data, value_reg]);
                }
            }
            MirInst::RegisterMembers => self.emit_op(126),
            MirInst::ApplyClassExtender {
                class_object,
                getter,
            } => {
                let class = self.alloc_reg();
                let class_data = self.data_for_const_value(&MirConst::CodeObject(*class_object))?;
                self.emit(&[1, class, class_data]);
                let getter_reg = self.alloc_reg();
                let getter_data = self.data_for_const_value(&MirConst::CodeObject(*getter))?;
                self.emit(&[1, getter_reg, getter_data]);
                self.emit(&[125, class, getter_reg]);
            }
            MirInst::BuildArray { dst, elements } => self.emit_build_array(*dst, elements)?,
            MirInst::BuildDictionary { dst, entries } => {
                self.emit_build_dictionary(*dst, entries)?
            }
            MirInst::BuildRegExp {
                dst,
                pattern,
                flags,
            } => self.emit_build_regexp(*dst, *pattern, *flags)?,
            MirInst::InitDefaultArg { arg, value } => {
                let dst = self.slot_reg(SlotId::Arg(*arg))?;
                let src = self.ensure_reg(*value)?;
                self.emit(&[2, dst, src]);
            }
            MirInst::Debugger => self.emit_op(127),
        }
        Ok(())
    }

    fn emit_terminator(&mut self, terminator: &Terminator) -> Result<()> {
        match terminator {
            Terminator::Goto(target) => self.emit_jump(17, *target),
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                self.emit_condition_flag(cond)?;
                self.emit_jump(15, *then_block)?;
                self.emit_jump(17, *else_block)
            }
            Terminator::Return { value } => {
                if let Some(value) = value {
                    let reg = self.ensure_reg(*value)?;
                    self.emit(&[118, reg]);
                }
                self.emit_op(119);
                Ok(())
            }
            Terminator::Throw { value } => {
                let reg = self.ensure_reg(*value)?;
                self.emit(&[122, reg]);
                Ok(())
            }
            Terminator::LeaveTry { next, .. } => {
                self.emit_op(121);
                self.emit_jump(17, *next)
            }
            Terminator::Unreachable => {
                self.emit_op(119);
                Ok(())
            }
        }
    }

    fn emit_condition_flag(&mut self, cond: &Condition) -> Result<()> {
        match cond {
            Condition::Truthy(value) => {
                let reg = self.ensure_reg(*value)?;
                self.emit(&[5, reg]);
            }
            Condition::Falsey(value) => {
                let reg = self.ensure_reg(*value)?;
                self.emit(&[6, reg]);
            }
            Condition::ArgNeedsDefault(index) => {
                let lhs = self.slot_reg(SlotId::Arg(*index))?;
                let rhs = self.alloc_reg();
                let data = self.data_for_const_value(&MirConst::Void)?;
                self.emit(&[1, rhs, data, 8, lhs, rhs]);
            }
            Condition::Compare { op, lhs, rhs } => self.emit_compare_flag(*op, *lhs, *rhs)?,
        }
        Ok(())
    }

    fn emit_compare_flag(&mut self, op: CompareOp, lhs: Value, rhs: Value) -> Result<()> {
        let lhs = self.ensure_reg(lhs)?;
        let rhs = self.ensure_reg(rhs)?;
        match op {
            CompareOp::Equal => self.emit(&[7, lhs, rhs]),
            CompareOp::NotEqual => self.emit(&[7, lhs, rhs, 14]),
            CompareOp::DiscernEqual => self.emit(&[8, lhs, rhs]),
            CompareOp::DiscernNotEqual => self.emit(&[8, lhs, rhs, 14]),
            CompareOp::LessThan => self.emit(&[9, lhs, rhs]),
            CompareOp::GreaterThan => self.emit(&[10, lhs, rhs]),
            CompareOp::LessEqual => self.emit(&[10, lhs, rhs, 14]),
            CompareOp::GreaterEqual => self.emit(&[9, lhs, rhs, 14]),
        }
        Ok(())
    }

    fn emit_value_to(&mut self, dst: SlotId, value: Value) -> Result<()> {
        let dst = self.slot_reg(dst)?;
        match value {
            Value::Slot(src) => {
                let src = self.slot_reg(src)?;
                if dst != src {
                    self.emit(&[2, dst, src]);
                }
            }
            Value::Const(id) => {
                let data = self.data_for_const(id)?;
                self.emit(&[1, dst, data]);
            }
        }
        Ok(())
    }

    fn ensure_reg(&mut self, value: Value) -> Result<i16> {
        match value {
            Value::Slot(slot) => self.slot_reg(slot),
            Value::Const(id) => {
                let reg = self.alloc_reg();
                let data = self.data_for_const(id)?;
                self.emit(&[1, reg, data]);
                Ok(reg)
            }
        }
    }

    fn emit_read_place(&mut self, dst: SlotId, place: &Place) -> Result<()> {
        let dst = self.slot_reg(dst)?;
        self.emit_read_place_to_reg(dst, place)
    }

    fn emit_read_place_to_reg(&mut self, dst: i16, place: &Place) -> Result<()> {
        match place {
            Place::Slot(slot) => {
                let src = self.slot_reg(*slot)?;
                if dst != src {
                    self.emit(&[2, dst, src]);
                }
            }
            Place::Member { object, key, flags } => {
                let object = self.ensure_reg(*object)?;
                match key {
                    super::mir::MemberKey::Direct(name) => {
                        let data = self.data_for_string_id(*name)?;
                        self.emit(&[if flags.ignore_prop { 110 } else { 103 }, dst, object, data]);
                    }
                    super::mir::MemberKey::Computed(key) => {
                        let key = self.ensure_reg(*key)?;
                        self.emit(&[if flags.ignore_prop { 112 } else { 107 }, dst, object, key]);
                    }
                }
            }
            Place::DefaultProperty { object, .. } => {
                let object = self.ensure_reg(*object)?;
                self.emit(&[115, dst, object]);
            }
        }
        Ok(())
    }

    fn emit_assign(&mut self, place: &Place, value: Value, result: Option<SlotId>) -> Result<()> {
        let reg = self.ensure_reg(value)?;
        self.emit_assign_reg(place, reg, result)
    }

    fn emit_assign_reg(
        &mut self,
        place: &Place,
        value_reg: i16,
        result: Option<SlotId>,
    ) -> Result<()> {
        match place {
            Place::Slot(slot) => {
                let dst = self.slot_reg(*slot)?;
                if dst != value_reg {
                    self.emit(&[2, dst, value_reg]);
                }
            }
            Place::Member { object, key, flags } => {
                let object = self.ensure_reg(*object)?;
                match key {
                    super::mir::MemberKey::Direct(name) => {
                        let data = self.data_for_string_id(*name)?;
                        self.emit(&[set_direct_opcode(*flags), object, data, value_reg]);
                    }
                    super::mir::MemberKey::Computed(key) => {
                        let key = self.ensure_reg(*key)?;
                        self.emit(&[set_indirect_opcode(*flags), object, key, value_reg]);
                    }
                }
            }
            Place::DefaultProperty { object, .. } => {
                let object = self.ensure_reg(*object)?;
                self.emit(&[114, object, value_reg]);
            }
        }
        if let Some(result) = result {
            let result = self.slot_reg(result)?;
            if result != value_reg {
                self.emit(&[2, result, value_reg]);
            }
        }
        Ok(())
    }

    fn emit_assign_op(
        &mut self,
        place: &Place,
        op: BinaryOp,
        rhs: Value,
        result: Option<SlotId>,
    ) -> Result<()> {
        let rhs = self.ensure_reg(rhs)?;
        match place {
            Place::Slot(slot) => {
                let dst = self.slot_reg(*slot)?;
                self.emit(&[binary_opcode(op, 0), dst, rhs]);
                if let Some(result) = result {
                    let result = self.slot_reg(result)?;
                    if result != dst {
                        self.emit(&[2, result, dst]);
                    }
                }
            }
            Place::Member { object, key, flags } if flags.ignore_prop => {
                let result_reg = result
                    .map(|slot| self.slot_reg(slot))
                    .transpose()?
                    .unwrap_or(0);
                let object = self.ensure_reg(*object)?;
                self.emit_ignore_prop_assign_op(object, key, op, rhs, result_reg)?;
            }
            Place::Member { object, key, .. } => {
                let result_reg = result
                    .map(|slot| self.slot_reg(slot))
                    .transpose()?
                    .unwrap_or(0);
                let object = self.ensure_reg(*object)?;
                match key {
                    super::mir::MemberKey::Direct(name) => {
                        let data = self.data_for_string_id(*name)?;
                        self.emit(&[binary_opcode(op, 1), result_reg, object, data, rhs]);
                    }
                    super::mir::MemberKey::Computed(key) => {
                        let key = self.ensure_reg(*key)?;
                        self.emit(&[binary_opcode(op, 2), result_reg, object, key, rhs]);
                    }
                }
            }
            Place::DefaultProperty { object, .. } => {
                let result_reg = result
                    .map(|slot| self.slot_reg(slot))
                    .transpose()?
                    .unwrap_or(0);
                let object = self.ensure_reg(*object)?;
                self.emit(&[binary_opcode(op, 3), result_reg, object, rhs]);
            }
        }
        Ok(())
    }

    fn emit_update(
        &mut self,
        place: &Place,
        op: UpdateOp,
        result: Option<SlotId>,
        result_value: UpdateResultValue,
    ) -> Result<()> {
        match place {
            Place::Slot(slot) => {
                let reg = self.slot_reg(*slot)?;
                if matches!(result_value, UpdateResultValue::Old)
                    && let Some(result) = result
                {
                    self.emit(&[2, self.slot_reg(result)?, reg]);
                }
                self.emit(&[if op == UpdateOp::Inc { 18 } else { 22 }, reg]);
                if matches!(result_value, UpdateResultValue::New)
                    && let Some(result) = result
                {
                    let result = self.slot_reg(result)?;
                    if result != reg {
                        self.emit(&[2, result, reg]);
                    }
                }
            }
            Place::Member { object, key, flags } if flags.ignore_prop => {
                let object = self.ensure_reg(*object)?;
                self.emit_ignore_prop_update(object, key, op, result, result_value)?;
            }
            Place::Member { object, key, .. } => {
                let old = if matches!(result_value, UpdateResultValue::Old) {
                    let old = self.alloc_reg();
                    self.emit_read_place_to_reg(old, place)?;
                    Some(old)
                } else {
                    None
                };
                let result_reg = if matches!(result_value, UpdateResultValue::New) {
                    result
                        .map(|slot| self.slot_reg(slot))
                        .transpose()?
                        .unwrap_or(0)
                } else {
                    0
                };
                let object = self.ensure_reg(*object)?;
                match key {
                    super::mir::MemberKey::Direct(name) => {
                        let data = self.data_for_string_id(*name)?;
                        self.emit(&[
                            if op == UpdateOp::Inc { 19 } else { 23 },
                            result_reg,
                            object,
                            data,
                        ]);
                    }
                    super::mir::MemberKey::Computed(key) => {
                        let key = self.ensure_reg(*key)?;
                        self.emit(&[
                            if op == UpdateOp::Inc { 20 } else { 24 },
                            result_reg,
                            object,
                            key,
                        ]);
                    }
                }
                if let (Some(result), Some(old)) = (result, old) {
                    self.emit(&[2, self.slot_reg(result)?, old]);
                }
            }
            Place::DefaultProperty { object, .. } => {
                let old = if matches!(result_value, UpdateResultValue::Old) {
                    let old = self.alloc_reg();
                    self.emit_read_place_to_reg(old, place)?;
                    Some(old)
                } else {
                    None
                };
                let result_reg = if matches!(result_value, UpdateResultValue::New) {
                    result
                        .map(|slot| self.slot_reg(slot))
                        .transpose()?
                        .unwrap_or(0)
                } else {
                    0
                };
                let object = self.ensure_reg(*object)?;
                self.emit(&[
                    if op == UpdateOp::Inc { 21 } else { 25 },
                    result_reg,
                    object,
                ]);
                if let (Some(result), Some(old)) = (result, old) {
                    self.emit(&[2, self.slot_reg(result)?, old]);
                }
            }
        }
        Ok(())
    }

    fn emit_ignore_prop_assign_op(
        &mut self,
        object: i16,
        key: &MemberKey,
        op: BinaryOp,
        rhs: i16,
        result_reg: i16,
    ) -> Result<()> {
        let current = self.alloc_reg();
        match key {
            MemberKey::Direct(name) => {
                let data = self.data_for_string_id(*name)?;
                self.emit(&[110, current, object, data]);
                self.emit(&[binary_opcode(op, 0), current, rhs]);
                self.emit(&[111, object, data, current]);
            }
            MemberKey::Computed(key) => {
                let key = self.ensure_reg(*key)?;
                self.emit(&[112, current, object, key]);
                self.emit(&[binary_opcode(op, 0), current, rhs]);
                self.emit(&[113, object, key, current]);
            }
        }
        if result_reg != 0 {
            self.emit(&[2, result_reg, current]);
        }
        Ok(())
    }

    fn emit_ignore_prop_update(
        &mut self,
        object: i16,
        key: &MemberKey,
        op: UpdateOp,
        result: Option<SlotId>,
        result_value: UpdateResultValue,
    ) -> Result<()> {
        let current = self.alloc_reg();
        let (key_reg, data) = match key {
            MemberKey::Direct(name) => {
                let data = self.data_for_string_id(*name)?;
                self.emit(&[110, current, object, data]);
                (None, Some(data))
            }
            MemberKey::Computed(key) => {
                let key = self.ensure_reg(*key)?;
                self.emit(&[112, current, object, key]);
                (Some(key), None)
            }
        };

        if matches!(result_value, UpdateResultValue::Old)
            && let Some(result) = result
        {
            self.emit(&[2, self.slot_reg(result)?, current]);
        }

        self.emit(&[if op == UpdateOp::Inc { 18 } else { 22 }, current]);
        match (data, key_reg) {
            (Some(data), None) => self.emit(&[111, object, data, current]),
            (None, Some(key)) => self.emit(&[113, object, key, current]),
            _ => unreachable!("direct and indirect keys are mutually exclusive"),
        }

        if matches!(result_value, UpdateResultValue::New)
            && let Some(result) = result
        {
            self.emit(&[2, self.slot_reg(result)?, current]);
        }
        Ok(())
    }

    fn emit_unary(&mut self, dst: SlotId, op: UnaryOp, src: Value) -> Result<()> {
        self.emit_value_to(dst, src)?;
        let dst = self.slot_reg(dst)?;
        self.emit(&[unary_opcode(op), dst]);
        Ok(())
    }

    fn emit_convert(&mut self, dst: SlotId, op: ConvertOp, src: Value) -> Result<()> {
        self.emit_value_to(dst, src)?;
        let dst = self.slot_reg(dst)?;
        self.emit(&[convert_opcode(op), dst]);
        Ok(())
    }

    fn emit_binary(&mut self, dst: SlotId, op: BinaryOp, lhs: Value, rhs: Value) -> Result<()> {
        self.emit_value_to(dst, lhs)?;
        let dst = self.slot_reg(dst)?;
        let rhs = self.ensure_reg(rhs)?;
        self.emit(&[binary_opcode(op, 0), dst, rhs]);
        Ok(())
    }

    fn emit_typeof_place(&mut self, dst: SlotId, place: &Place) -> Result<()> {
        let dst = self.slot_reg(dst)?;
        match place {
            Place::Member { object, key, .. } => {
                let object = self.ensure_reg(*object)?;
                match key {
                    super::mir::MemberKey::Direct(name) => {
                        let data = self.data_for_string_id(*name)?;
                        self.emit(&[84, dst, object, data]);
                    }
                    super::mir::MemberKey::Computed(key) => {
                        let key = self.ensure_reg(*key)?;
                        self.emit(&[85, dst, object, key]);
                    }
                }
            }
            _ => {
                self.emit_read_place_to_reg(dst, place)?;
                self.emit(&[83, dst]);
            }
        }
        Ok(())
    }

    fn emit_delete(&mut self, dst: Option<SlotId>, place: &Place) -> Result<()> {
        let dst = dst
            .map(|slot| self.slot_reg(slot))
            .transpose()?
            .unwrap_or(0);
        match place {
            Place::Member { object, key, .. } => {
                let object = self.ensure_reg(*object)?;
                match key {
                    super::mir::MemberKey::Direct(name) => {
                        let data = self.data_for_string_id(*name)?;
                        self.emit(&[116, dst, object, data]);
                    }
                    super::mir::MemberKey::Computed(key) => {
                        let key = self.ensure_reg(*key)?;
                        self.emit(&[117, dst, object, key]);
                    }
                }
            }
            Place::Slot(_) | Place::DefaultProperty { .. } => {
                if dst != 0 {
                    let data = self.data_for_const_value(&MirConst::Integer(0))?;
                    self.emit(&[1, dst, data]);
                }
            }
        }
        Ok(())
    }

    fn emit_call(
        &mut self,
        dst: Option<SlotId>,
        target: &CallTarget,
        args: &ArgList,
    ) -> Result<()> {
        let dst = dst
            .map(|slot| self.slot_reg(slot))
            .transpose()?
            .unwrap_or(0);
        match target {
            CallTarget::Value(value) => {
                let callee = self.ensure_reg(*value)?;
                self.emit_call_words(99, &[dst, callee], args)?;
            }
            CallTarget::Member { object, key, .. } => {
                let object = self.ensure_reg(*object)?;
                match key {
                    super::mir::MemberKey::Direct(name) => {
                        let data = self.data_for_string_id(*name)?;
                        self.emit_call_words(100, &[dst, object, data], args)?;
                    }
                    super::mir::MemberKey::Computed(key) => {
                        let key = self.ensure_reg(*key)?;
                        self.emit_call_words(101, &[dst, object, key], args)?;
                    }
                }
            }
            CallTarget::DefaultProperty { object, .. } => {
                let object = self.ensure_reg(*object)?;
                let callee = self.alloc_reg();
                self.emit(&[115, callee, object]);
                self.emit_call_words(99, &[dst, callee], args)?;
            }
        }
        Ok(())
    }

    fn emit_new(&mut self, dst: Option<SlotId>, callee: Value, args: &ArgList) -> Result<()> {
        let dst = dst
            .map(|slot| self.slot_reg(slot))
            .transpose()?
            .unwrap_or(0);
        let callee = self.ensure_reg(callee)?;
        self.emit_call_words(102, &[dst, callee], args)
    }

    fn emit_call_words(&mut self, opcode: i16, operands: &[i16], args: &ArgList) -> Result<()> {
        let args = match args {
            ArgList::Normal(args) => {
                let mut regs = Vec::with_capacity(args.len());
                for arg in args {
                    regs.push(self.ensure_reg(*arg)?);
                }
                EncodedCallArgs::Normal(regs)
            }
            ArgList::OmittedCallerArgs => EncodedCallArgs::OmittedCallerArgs,
            ArgList::Expanded(args) => {
                let mut encoded = Vec::with_capacity(args.len());
                for arg in args {
                    encoded.push(match arg {
                        ArgPart::Normal(value) => (0, self.ensure_reg(*value)?),
                        ArgPart::Expand(value) => (1, self.ensure_reg(*value)?),
                        ArgPart::UnnamedExpand => (2, 0),
                    });
                }
                EncodedCallArgs::Expanded(encoded)
            }
        };
        self.emit_op(opcode);
        self.emit(operands);
        match args {
            EncodedCallArgs::Normal(args) => {
                self.emit_word(checked_i16(args.len(), "call argument count")?);
                for reg in args {
                    self.emit_word(reg);
                }
            }
            EncodedCallArgs::OmittedCallerArgs => self.emit_word(-1),
            EncodedCallArgs::Expanded(args) => {
                self.emit_word(-2);
                self.emit_word(checked_i16(args.len(), "expanded argument count")?);
                for (arg_type, reg) in args {
                    self.emit(&[arg_type, reg]);
                }
            }
        }
        Ok(())
    }

    fn emit_call_regs(&mut self, opcode: i16, operands: &[i16], args: &[i16]) -> Result<()> {
        self.emit_op(opcode);
        self.emit(operands);
        self.emit_word(checked_i16(args.len(), "call argument count")?);
        self.emit(args);
        Ok(())
    }

    fn emit_build_array(&mut self, dst: SlotId, elements: &[ArrayElement]) -> Result<()> {
        let global = self.alloc_reg();
        self.emit(&[124, global]);
        let mut args = Vec::with_capacity(elements.len());
        for element in elements {
            let reg = match element {
                ArrayElement::Value(value) | ArrayElement::Expand(value) => {
                    self.ensure_reg(*value)?
                }
                ArrayElement::Hole => {
                    let reg = self.alloc_reg();
                    let data = self.data_for_const_value(&MirConst::Void)?;
                    self.emit(&[1, reg, data]);
                    reg
                }
            };
            args.push(reg);
        }
        let data = self.data_for_string("Array")?;
        self.emit_call_regs(100, &[self.slot_reg(dst)?, global, data], &args)
    }

    fn emit_build_dictionary(
        &mut self,
        dst: SlotId,
        entries: &[super::mir::DictionaryEntry],
    ) -> Result<()> {
        let global = self.alloc_reg();
        self.emit(&[124, global]);
        let data = self.data_for_string("Dictionary")?;
        self.emit_call_words(
            100,
            &[self.slot_reg(dst)?, global, data],
            &ArgList::Normal(Vec::new()),
        )?;
        for entry in entries {
            let value = self.ensure_reg(entry.value)?;
            match &entry.key {
                DictionaryKey::Direct(name) => {
                    let key = self.data_for_string_id(*name)?;
                    self.emit(&[105, self.slot_reg(dst)?, key, value]);
                }
                DictionaryKey::Computed(key) => {
                    let key = self.ensure_reg(*key)?;
                    self.emit(&[109, self.slot_reg(dst)?, key, value]);
                }
            }
        }
        Ok(())
    }

    fn emit_build_regexp(
        &mut self,
        dst: SlotId,
        pattern: super::mir::StringId,
        flags: super::mir::StringId,
    ) -> Result<()> {
        let global = self.alloc_reg();
        self.emit(&[124, global]);
        let name = self.data_for_string("RegExp")?;
        let pattern_reg = self.alloc_reg();
        let pattern_data = self.data_for_const_value(&MirConst::String(pattern))?;
        self.emit(&[1, pattern_reg, pattern_data]);
        let flags_reg = self.alloc_reg();
        let flags_data = self.data_for_const_value(&MirConst::String(flags))?;
        self.emit(&[1, flags_reg, flags_data]);
        self.emit_call_regs(
            100,
            &[self.slot_reg(dst)?, global, name],
            &[pattern_reg, flags_reg],
        )
    }

    fn emit_entry(&mut self, catch: super::mir::BlockId, reg: i16) -> Result<()> {
        let inst_offset = self.code.len();
        self.emit(&[120, 0, reg]);
        self.patches.push(Patch {
            inst_offset,
            operand_offset: inst_offset + 1,
            target: catch,
        });
        Ok(())
    }

    fn emit_jump(&mut self, opcode: i16, target: super::mir::BlockId) -> Result<()> {
        let inst_offset = self.code.len();
        self.emit(&[opcode, 0]);
        self.patches.push(Patch {
            inst_offset,
            operand_offset: inst_offset + 1,
            target,
        });
        Ok(())
    }

    fn patch_branches(&mut self) -> Result<()> {
        for patch in &self.patches {
            let target = *self
                .block_offsets
                .get(&patch.target)
                .ok_or_else(|| TjsError::codegen(format!("missing block {}", patch.target.0)))?;
            let delta = target as isize - patch.inst_offset as isize;
            self.code[patch.operand_offset] = i16::try_from(delta)
                .map_err(|_| TjsError::codegen("branch offset does not fit i16"))?;
        }
        Ok(())
    }

    fn data_for_const(&mut self, id: super::mir::ConstId) -> Result<i16> {
        let constant = self
            .module
            .constants
            .get(id.0 as usize)
            .cloned()
            .ok_or_else(|| TjsError::codegen(format!("constant {} is missing", id.0)))?;
        self.data_for_const_value(&constant)
    }

    fn data_for_const_value(&mut self, value: &MirConst) -> Result<i16> {
        let slot = self.module_codegen.data_slot_for_const(value)?;
        self.add_data_slot(slot)
    }

    fn data_for_string(&mut self, text: &str) -> Result<i16> {
        let slot = self.module_codegen.data_slot_for_string(text)?;
        self.add_data_slot(slot)
    }

    fn data_for_string_id(&mut self, id: super::mir::StringId) -> Result<i16> {
        let index = self.module_codegen.string_index(id)?;
        self.add_data_slot(DataSlot {
            ty: DataSlotType::String,
            index: checked_i16(index, "string pool")?,
        })
    }

    fn add_data_slot(&mut self, slot: DataSlot) -> Result<i16> {
        let index = checked_i16(self.data_slots.len(), "object data area")?;
        self.data_slots.push(slot);
        Ok(index)
    }

    fn slot_reg(&self, slot: SlotId) -> Result<i16> {
        Ok(match slot {
            SlotId::Temp(id) => checked_i16(id.0 as usize + 1, "temp register")?,
            SlotId::Local(id) => {
                let offset = self.object.args.declared.len() + id.0 as usize;
                -3 - checked_i16(offset, "local register")?
            }
            SlotId::Arg(index) => -3 - checked_i16(index as usize, "arg register")?,
            SlotId::This => -1,
            SlotId::ThisProxy => -2,
        })
    }

    fn alloc_reg(&mut self) -> i16 {
        let reg = self.next_reg;
        self.next_reg += 1;
        reg
    }

    fn record_block_source_position(&mut self, block: &super::mir::BasicBlock) {
        let Some(span_id) = block.source_span else {
            return;
        };
        let Some(span) = self.module.spans.get(span_id.0 as usize) else {
            return;
        };
        let code_pos = self.code.len() as u32;
        if self
            .source_positions
            .last()
            .is_some_and(|position| position.code_pos == code_pos)
        {
            return;
        }
        self.source_positions.push(SourcePosition {
            code_pos,
            source_pos: span.utf16_start,
        });
    }

    fn emit_op(&mut self, opcode: i16) {
        self.code.push(opcode);
    }

    fn emit_word(&mut self, word: i16) {
        self.code.push(word);
    }

    fn emit(&mut self, words: &[i16]) {
        self.code.extend_from_slice(words);
    }
}

fn context_type(context: ContextType) -> BytecodeContextType {
    match context {
        ContextType::TopLevel => BytecodeContextType::TopLevel,
        ContextType::Function => BytecodeContextType::Function,
        ContextType::ExprFunction => BytecodeContextType::ExprFunction,
        ContextType::Property => BytecodeContextType::Property,
        ContextType::PropertySetter => BytecodeContextType::PropertySetter,
        ContextType::PropertyGetter => BytecodeContextType::PropertyGetter,
        ContextType::Class => BytecodeContextType::Class,
        ContextType::SuperClassGetter => BytecodeContextType::SuperClassGetter,
    }
}

fn unary_opcode(op: UnaryOp) -> i16 {
    match op {
        UnaryOp::LogicalNot => 13,
        UnaryOp::BitNot => 82,
        UnaryOp::Negate => 92,
        UnaryOp::Asc => 89,
        UnaryOp::Chr => 90,
    }
}

fn convert_opcode(op: ConvertOp) -> i16 {
    match op {
        ConvertOp::Number => 91,
        ConvertOp::Integer => 95,
        ConvertOp::Real => 96,
        ConvertOp::String => 97,
        ConvertOp::Octet => 98,
    }
}

fn binary_opcode(op: BinaryOp, form: i16) -> i16 {
    26 + binary_family_index(op) * 4 + form
}

fn binary_family_index(op: BinaryOp) -> i16 {
    match op {
        BinaryOp::LogicalOr => 0,
        BinaryOp::LogicalAnd => 1,
        BinaryOp::BitOr => 2,
        BinaryOp::BitXor => 3,
        BinaryOp::BitAnd => 4,
        BinaryOp::ShiftArithmeticRight => 5,
        BinaryOp::ShiftLeft => 6,
        BinaryOp::ShiftLogicalRight => 7,
        BinaryOp::Add => 8,
        BinaryOp::Sub => 9,
        BinaryOp::Mod => 10,
        BinaryOp::Div => 11,
        BinaryOp::Idiv => 12,
        BinaryOp::Mul => 13,
    }
}

fn set_direct_opcode(flags: DispatchFlags) -> i16 {
    if flags.ignore_prop {
        111
    } else if flags.hidden {
        106
    } else if flags.ensure {
        105
    } else {
        104
    }
}

fn set_indirect_opcode(flags: DispatchFlags) -> i16 {
    if flags.ignore_prop {
        113
    } else if flags.ensure {
        109
    } else {
        108
    }
}

fn checked_i16(value: usize, field: &str) -> Result<i16> {
    i16::try_from(value).map_err(|_| TjsError::codegen(format!("{field} exceeds i16 range")))
}

fn checked_i16_lossy(value: usize) -> i16 {
    i16::try_from(value).unwrap_or(i16::MAX - 1)
}
