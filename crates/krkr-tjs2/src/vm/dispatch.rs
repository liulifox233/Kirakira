use crate::bytecode::{BytecodeContextType, CallArgs, CodeObject, Instruction};
use crate::error::{Result, TjsError};
use crate::runtime::{Closure, Object, ObjectHandle, ObjectKind, TjsHost, Variant};

use super::opcode::{OpcodeForm, binary_family, execute_binary_value, opcode_form};
use super::{DispatchFlags, Frame, Vm};

impl<'bc, 'rt, H: TjsHost + 'static> Vm<'bc, 'rt, H> {
    pub(super) fn resolve_object(&self, value: Variant) -> Result<ObjectHandle> {
        match self.materialize_code_object(value) {
            Variant::Object(handle) => Ok(handle),
            Variant::Closure(closure) => Ok(closure.object),
            Variant::Null => Err(TjsError::runtime("null object access")),
            other => Err(TjsError::runtime(format!("{other} is not an object"))),
        }
    }

    pub(super) fn closure_parts(
        &self,
        value: Variant,
    ) -> Result<(ObjectHandle, Option<ObjectHandle>)> {
        match self.materialize_code_object(value) {
            Variant::Object(handle) => Ok((handle, None)),
            Variant::Closure(closure) => Ok((closure.object, closure.this_obj)),
            Variant::Null => Err(TjsError::runtime("null object access")),
            other => Err(TjsError::runtime(format!("{other} is not an object"))),
        }
    }

    pub(super) fn prop_get(
        &mut self,
        target: Variant,
        name: &str,
        flags: DispatchFlags,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Variant> {
        match target {
            Variant::String(value) => return self.string_property(&value, name),
            Variant::Octet(value) => return self.octet_property(&value, name),
            _ => {}
        }

        let (handle, closure_this) = self.closure_parts(target)?;
        self.prop_get_handle(handle, name, flags, closure_this.or(caller_this))
    }

    pub(super) fn prop_get_handle(
        &mut self,
        handle: ObjectHandle,
        name: &str,
        flags: DispatchFlags,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let kind = self.runtime.heap[handle.0].kind.clone();
        if let ObjectKind::Proxy { primary, fallback } = kind {
            if let Some(primary) = primary {
                let value = self.prop_get_handle(primary, name, flags, caller_this)?;
                if !matches!(value, Variant::Void) {
                    return Ok(value);
                }
            }
            return self.prop_get_handle(fallback, name, flags, caller_this);
        }

        let Some(value) = self.runtime.heap[handle.0].get_raw(name) else {
            if flags.must_exist {
                return Err(TjsError::runtime(format!("member `{name}` not found")));
            }
            return Ok(Variant::Void);
        };

        if !flags.ignore_prop
            && let Some(getter) = self.property_getter(value.clone(), caller_this)?
        {
            return Ok(getter);
        }
        Ok(value)
    }

    pub(super) fn prop_set(
        &mut self,
        target: Variant,
        name: &str,
        value: Variant,
        flags: DispatchFlags,
        caller_this: Option<ObjectHandle>,
    ) -> Result<()> {
        let (handle, closure_this) = self.closure_parts(target)?;
        self.prop_set_handle(handle, name, value, flags, closure_this.or(caller_this))
    }

    pub(super) fn prop_set_handle(
        &mut self,
        handle: ObjectHandle,
        name: &str,
        value: Variant,
        flags: DispatchFlags,
        caller_this: Option<ObjectHandle>,
    ) -> Result<()> {
        let kind = self.runtime.heap[handle.0].kind.clone();
        if let ObjectKind::Proxy { primary, fallback } = kind {
            if let Some(primary) = primary
                && (flags.ensure || self.runtime.heap[primary.0].get_raw(name).is_some())
            {
                return self.prop_set_handle(primary, name, value, flags, caller_this);
            }
            return self.prop_set_handle(fallback, name, value, flags, caller_this);
        }

        if !flags.ignore_prop
            && let Some(existing) = self.runtime.heap[handle.0].get_raw(name)
            && self
                .property_setter(existing, value.clone(), caller_this)?
                .is_some()
        {
            return Ok(());
        }

        let mut value = self.materialize_code_object(value);
        if let Variant::Closure(closure) = &mut value
            && closure.this_obj.is_none()
        {
            closure.this_obj = Some(handle);
        }
        self.runtime.heap[handle.0].set(name, value);
        Ok(())
    }

    pub(super) fn default_prop_get(
        &mut self,
        target: Variant,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let (handle, closure_this) = self.closure_parts(target)?;
        let kind = self.runtime.heap[handle.0].kind.clone();
        if let ObjectKind::InterCode {
            object_index,
            context: BytecodeContextType::Property,
        } = kind
        {
            let getter = self.file.objects[object_index].prop_getter;
            if let Some(getter) = getter {
                return self.execute_object_with_this(
                    getter,
                    Vec::new(),
                    closure_this.or(caller_this),
                );
            }
            return Err(TjsError::runtime("property has no getter"));
        }
        self.prop_get_handle(
            handle,
            "value",
            DispatchFlags::default(),
            closure_this.or(caller_this),
        )
    }

    pub(super) fn default_prop_set(
        &mut self,
        target: Variant,
        value: Variant,
        caller_this: Option<ObjectHandle>,
    ) -> Result<()> {
        let (handle, closure_this) = self.closure_parts(target)?;
        let kind = self.runtime.heap[handle.0].kind.clone();
        if let ObjectKind::InterCode {
            object_index,
            context: BytecodeContextType::Property,
        } = kind
        {
            let setter = self.file.objects[object_index].prop_setter;
            if let Some(setter) = setter {
                self.execute_object_with_this(setter, vec![value], closure_this.or(caller_this))?;
                return Ok(());
            }
            return Err(TjsError::runtime("property has no setter"));
        }
        self.prop_set_handle(
            handle,
            "value",
            value,
            DispatchFlags::default(),
            closure_this.or(caller_this),
        )
    }

    pub(super) fn property_getter(
        &mut self,
        value: Variant,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Option<Variant>> {
        let Ok((handle, closure_this)) = self.closure_parts(value) else {
            return Ok(None);
        };
        let ObjectKind::InterCode {
            object_index,
            context: BytecodeContextType::Property,
        } = self.runtime.heap[handle.0].kind
        else {
            return Ok(None);
        };
        let Some(getter) = self.file.objects[object_index].prop_getter else {
            return Ok(None);
        };
        Ok(Some(self.execute_object_with_this(
            getter,
            Vec::new(),
            closure_this.or(caller_this),
        )?))
    }

    pub(super) fn property_setter(
        &mut self,
        target: Variant,
        value: Variant,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Option<()>> {
        let Ok((handle, closure_this)) = self.closure_parts(target) else {
            return Ok(None);
        };
        let ObjectKind::InterCode {
            object_index,
            context: BytecodeContextType::Property,
        } = self.runtime.heap[handle.0].kind
        else {
            return Ok(None);
        };
        let Some(setter) = self.file.objects[object_index].prop_setter else {
            return Ok(None);
        };
        self.execute_object_with_this(setter, vec![value], closure_this.or(caller_this))?;
        Ok(Some(()))
    }

    pub(super) fn delete_member(&mut self, target: Variant, name: &str) -> Result<bool> {
        let handle = self.resolve_object(target)?;
        Ok(self.runtime.heap[handle.0].delete(name))
    }

    fn string_property(&self, value: &str, name: &str) -> Result<Variant> {
        if name == "count" || name == "length" {
            return Ok(Variant::Integer(value.encode_utf16().count() as i64));
        }
        if let Ok(index) = name.parse::<usize>() {
            let unit = value.encode_utf16().nth(index).unwrap_or(0);
            return Ok(Variant::String(String::from_utf16_lossy(&[unit])));
        }
        Ok(Variant::Void)
    }

    fn octet_property(&self, value: &[u8], name: &str) -> Result<Variant> {
        if name == "count" || name == "length" {
            return Ok(Variant::Integer(value.len() as i64));
        }
        if let Ok(index) = name.parse::<usize>() {
            return Ok(Variant::Integer(
                value.get(index).copied().map(i64::from).unwrap_or(0),
            ));
        }
        Ok(Variant::Void)
    }

    pub(super) fn execute_update_property(
        &mut self,
        frame: &mut Frame,
        object: &CodeObject,
        inst: &Instruction,
    ) -> Result<()> {
        let inc = matches!(inst.opcode, 19..=21);
        let value = match inst.opcode {
            19 | 23 => {
                let object_value = frame.get(inst.operands[1])?;
                let name = self.data_slot_string(object, inst.operands[2])?;
                self.operate_property(object_value, &name, None, frame.this_obj, |value, _| {
                    if inc {
                        value.increment()
                    } else {
                        value.decrement()
                    }
                })?
            }
            20 | 24 => {
                let object_value = frame.get(inst.operands[1])?;
                let name = self.key_from_variant(&frame.get(inst.operands[2])?)?;
                self.operate_property(object_value, &name, None, frame.this_obj, |value, _| {
                    if inc {
                        value.increment()
                    } else {
                        value.decrement()
                    }
                })?
            }
            21 | 25 => {
                let object_value = frame.get(inst.operands[1])?;
                let current = self.default_prop_get(object_value.clone(), frame.this_obj)?;
                let updated = if inc {
                    current.increment()?
                } else {
                    current.decrement()?
                };
                self.default_prop_set(object_value, updated.clone(), frame.this_obj)?;
                updated
            }
            _ => unreachable!("update property opcode checked by caller"),
        };
        if inst.operands[0] != 0 {
            frame.set(inst.operands[0], value)?;
        }
        Ok(())
    }

    pub(super) fn execute_binary(
        &mut self,
        frame: &mut Frame,
        object: &CodeObject,
        inst: &Instruction,
    ) -> Result<()> {
        match opcode_form(inst.opcode) {
            OpcodeForm::Slot => {
                let lhs = frame.get(inst.operands[0])?;
                let rhs = frame.get(inst.operands[1])?;
                let value = execute_binary_value(binary_family(inst.opcode), lhs, rhs)?;
                frame.set(inst.operands[0], value)?;
            }
            OpcodeForm::DirectProperty => {
                let object_value = frame.get(inst.operands[1])?;
                let name = self.data_slot_string(object, inst.operands[2])?;
                let rhs = frame.get(inst.operands[3])?;
                let family = binary_family(inst.opcode);
                let value = self.operate_property(
                    object_value,
                    &name,
                    Some(rhs),
                    frame.this_obj,
                    |value, rhs| execute_binary_value(family, value, rhs.expect("rhs present")),
                )?;
                if inst.operands[0] != 0 {
                    frame.set(inst.operands[0], value)?;
                }
            }
            OpcodeForm::IndirectProperty => {
                let object_value = frame.get(inst.operands[1])?;
                let name = self.key_from_variant(&frame.get(inst.operands[2])?)?;
                let rhs = frame.get(inst.operands[3])?;
                let family = binary_family(inst.opcode);
                let value = self.operate_property(
                    object_value,
                    &name,
                    Some(rhs),
                    frame.this_obj,
                    |value, rhs| execute_binary_value(family, value, rhs.expect("rhs present")),
                )?;
                if inst.operands[0] != 0 {
                    frame.set(inst.operands[0], value)?;
                }
            }
            OpcodeForm::DefaultProperty => {
                let object_value = frame.get(inst.operands[1])?;
                let rhs = frame.get(inst.operands[2])?;
                let current = self.default_prop_get(object_value.clone(), frame.this_obj)?;
                let updated = execute_binary_value(binary_family(inst.opcode), current, rhs)?;
                self.default_prop_set(object_value, updated.clone(), frame.this_obj)?;
                if inst.operands[0] != 0 {
                    frame.set(inst.operands[0], updated)?;
                }
            }
        }
        Ok(())
    }

    fn operate_property(
        &mut self,
        object_value: Variant,
        name: &str,
        rhs: Option<Variant>,
        caller_this: Option<ObjectHandle>,
        op: impl FnOnce(Variant, Option<Variant>) -> Result<Variant>,
    ) -> Result<Variant> {
        let current = self.prop_get(
            object_value.clone(),
            name,
            DispatchFlags::default(),
            caller_this,
        )?;
        let value = op(current, rhs)?;
        self.prop_set(
            object_value,
            name,
            value.clone(),
            DispatchFlags::default(),
            caller_this,
        )?;
        Ok(value)
    }

    pub(super) fn typeof_direct(
        &mut self,
        frame: &Frame,
        object: &CodeObject,
        inst: &Instruction,
        _flags: DispatchFlags,
    ) -> Result<Variant> {
        let object_value = frame.get(inst.operands[1])?;
        let name = self.data_slot_string(object, inst.operands[2])?;
        Ok(
            match self.prop_get(
                object_value,
                &name,
                DispatchFlags::must_exist(),
                frame.this_obj,
            ) {
                Ok(value) => Variant::String(value.typeof_name().to_string()),
                Err(_) => Variant::String("undefined".to_string()),
            },
        )
    }

    pub(super) fn typeof_indirect(
        &mut self,
        frame: &Frame,
        inst: &Instruction,
        _flags: DispatchFlags,
    ) -> Result<Variant> {
        let object_value = frame.get(inst.operands[1])?;
        let name = self.key_from_variant(&frame.get(inst.operands[2])?)?;
        Ok(
            match self.prop_get(
                object_value,
                &name,
                DispatchFlags::must_exist(),
                frame.this_obj,
            ) {
                Ok(value) => Variant::String(value.typeof_name().to_string()),
                Err(_) => Variant::String("undefined".to_string()),
            },
        )
    }

    pub(super) fn materialize_call_args(
        &mut self,
        frame: &Frame,
        object: &CodeObject,
        args: Option<&CallArgs>,
    ) -> Result<Vec<Variant>> {
        let Some(args) = args else {
            return Err(TjsError::runtime("call instruction has no argspec"));
        };
        match args {
            CallArgs::Normal(args) => args.iter().map(|reg| frame.get(*reg)).collect(),
            CallArgs::OmittedCallerArgs => Ok(frame.caller_args.clone()),
            CallArgs::Expanded(args) => {
                let mut values = Vec::new();
                for arg in args {
                    match arg.arg_type {
                        0 => values.push(frame.get(arg.reg)?),
                        1 => {
                            let value = frame.get(arg.reg)?;
                            values.extend(self.expand_argument(value)?);
                        }
                        2 => {
                            let start = object.func_decl_unnamed_arg_array_base as usize;
                            values.extend(frame.caller_args.iter().skip(start).cloned());
                        }
                        _ => {
                            return Err(TjsError::runtime(format!(
                                "invalid expanded argument type {}",
                                arg.arg_type
                            )));
                        }
                    }
                }
                Ok(values)
            }
        }
    }

    fn expand_argument(&self, value: Variant) -> Result<Vec<Variant>> {
        let handle = self.resolve_object(value)?;
        if let Some(elements) = self.runtime.heap[handle.0].array_elements() {
            return Ok(elements.to_vec());
        }
        let mut values = Vec::new();
        let mut index = 0;
        loop {
            let key = index.to_string();
            let Some(value) = self.runtime.heap[handle.0].get_raw(&key) else {
                break;
            };
            values.push(value);
            index += 1;
        }
        Ok(values)
    }

    pub(super) fn call_member_direct(
        &mut self,
        object_value: Variant,
        name: &str,
        args: Vec<Variant>,
        dest_reg: i16,
    ) -> Result<Variant> {
        if let Variant::String(value) = &object_value {
            return self.call_string_method(value.clone(), name, args);
        }
        if let Variant::Octet(value) = &object_value {
            return self.call_octet_method(value.clone(), name, args);
        }

        let (handle, closure_this) = self.closure_parts(object_value.clone())?;
        let member = self.prop_get_handle(
            handle,
            name,
            DispatchFlags::default(),
            closure_this.or(Some(handle)),
        )?;
        let value = self.call_value(member, closure_this.or(Some(handle)), args, false)?;
        if dest_reg == 0 {
            Ok(Variant::Void)
        } else {
            Ok(value)
        }
    }

    pub fn call_object_method(
        &mut self,
        object: ObjectHandle,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        self.call_member_direct(Variant::Object(object), name, args, 1)
    }

    pub(super) fn call_value(
        &mut self,
        callee: Variant,
        this_obj: Option<ObjectHandle>,
        args: Vec<Variant>,
        is_new: bool,
    ) -> Result<Variant> {
        match self.materialize_code_object(callee) {
            Variant::Closure(closure) => {
                self.call_handle(closure.object, closure.this_obj.or(this_obj), args, is_new)
            }
            Variant::Object(handle) => self.call_handle(handle, this_obj, args, is_new),
            other => Err(TjsError::runtime(format!("{other} is not callable"))),
        }
    }

    fn call_handle(
        &mut self,
        handle: ObjectHandle,
        this_obj: Option<ObjectHandle>,
        args: Vec<Variant>,
        is_new: bool,
    ) -> Result<Variant> {
        let kind = self.runtime.heap[handle.0].kind.clone();
        match kind {
            ObjectKind::InterCode {
                object_index,
                context,
            } => {
                if is_new || context == BytecodeContextType::Class {
                    self.create_new_inter_code(object_index, context, args)
                } else {
                    self.execute_object_with_this(object_index, args, this_obj)
                }
            }
            ObjectKind::NativeFunction { id } => {
                let function = self
                    .runtime
                    .native_functions
                    .get(id)
                    .cloned()
                    .ok_or_else(|| TjsError::runtime(format!("native function {id} missing")))?;
                function.call(self.runtime, this_obj, args)
            }
            ObjectKind::VmNativeFunction { id } => {
                let function = self
                    .runtime
                    .vm_native_functions
                    .get(id)
                    .cloned()
                    .ok_or_else(|| TjsError::runtime(format!("VM native function {id} missing")))?;
                function.call(self, this_obj, args)
            }
            ObjectKind::Proxy { primary, fallback } => {
                if let Some(primary) = primary {
                    self.call_handle(primary, this_obj, args, is_new)
                } else {
                    self.call_handle(fallback, this_obj, args, is_new)
                }
            }
            ObjectKind::Ordinary | ObjectKind::Array { .. } => {
                Err(TjsError::runtime("object is not callable"))
            }
        }
    }

    fn create_new_inter_code(
        &mut self,
        object_index: usize,
        context: BytecodeContextType,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        let instance = self.runtime.alloc_object(Object::default());
        let name = self.file.objects[object_index]
            .name(self.file)
            .unwrap_or("")
            .to_string();

        if context == BytecodeContextType::Class {
            self.add_class_info(instance, name.clone());
            self.execute_object_with_this(object_index, Vec::new(), Some(instance))?;
            let class_handle = self.code_handles[object_index];
            for info in self.runtime.heap[class_handle.0].class_infos.clone() {
                self.add_class_info(instance, info);
            }

            if !name.is_empty()
                && let Some(constructor) = self.runtime.heap[instance.0].get_raw(&name)
                && !matches!(constructor, Variant::Void)
            {
                self.call_value(constructor, Some(instance), args, false)?;
            }
        } else {
            self.execute_object_with_this(object_index, args, Some(instance))?;
        }
        Ok(Variant::Object(instance))
    }

    fn call_string_method(&self, value: String, name: &str, args: Vec<Variant>) -> Result<Variant> {
        match name {
            "toString" => Ok(Variant::String(value)),
            "substr" | "substring" => {
                let start = args
                    .first()
                    .map(Variant::to_integer)
                    .transpose()?
                    .unwrap_or(0);
                let len = args.get(1).map(Variant::to_integer).transpose()?;
                let chars = value.chars().collect::<Vec<_>>();
                let start = start.max(0) as usize;
                let end = len
                    .map(|len| start.saturating_add(len.max(0) as usize))
                    .unwrap_or(chars.len())
                    .min(chars.len());
                Ok(Variant::String(
                    chars[start.min(chars.len())..end].iter().collect(),
                ))
            }
            _ => Err(TjsError::runtime(format!(
                "string method `{name}` not found"
            ))),
        }
    }

    fn call_octet_method(
        &self,
        _value: Vec<u8>,
        name: &str,
        _args: Vec<Variant>,
    ) -> Result<Variant> {
        Err(TjsError::runtime(format!(
            "octet method `{name}` not found"
        )))
    }

    pub(super) fn key_from_variant(&self, value: &Variant) -> Result<String> {
        match value {
            Variant::Integer(value) => Ok(value.to_string()),
            _ => value.to_tjs_string(),
        }
    }

    pub(super) fn change_this(&self, value: &mut Variant, this_obj: ObjectHandle) -> Result<()> {
        match self.materialize_code_object(value.clone()) {
            Variant::Closure(mut closure) => {
                closure.this_obj = Some(this_obj);
                *value = Variant::Closure(closure);
                Ok(())
            }
            Variant::Object(object) => {
                *value = Variant::Closure(Closure::new(object, Some(this_obj)));
                Ok(())
            }
            other => Err(TjsError::runtime(format!("{other} is not a closure"))),
        }
    }

    pub(super) fn instance_of(&self, value: &Variant, class_name: &str) -> bool {
        let Ok(handle) = self.resolve_object(value.clone()) else {
            return false;
        };
        self.runtime.heap[handle.0]
            .class_infos
            .iter()
            .any(|info| info == class_name)
    }

    pub(super) fn add_class_info(&mut self, handle: ObjectHandle, info: String) {
        if info.is_empty()
            || self.runtime.heap[handle.0]
                .class_infos
                .iter()
                .any(|item| item == &info)
        {
            return;
        }
        self.runtime.heap[handle.0].class_infos.push(info);
    }

    pub(super) fn register_object_members(
        &mut self,
        object: &CodeObject,
        dest: ObjectHandle,
    ) -> Result<()> {
        for property in &object.properties {
            let name = self
                .file
                .data
                .strings
                .get(property.name)
                .cloned()
                .ok_or_else(|| TjsError::runtime("property name missing"))?;
            let handle = self.code_handles[property.object];
            self.runtime.heap[dest.0].set(name, Variant::Closure(Closure::new(handle, Some(dest))));
        }
        Ok(())
    }
}
