use std::sync::Arc;

use crate::bytecode::{BytecodeContextType, CallArgs, CodeObject, Instruction};
use crate::compiler::compile_source_to_bytecode;
use crate::error::{Result, TjsError, TjsMemberAccess, TjsMemberOperation};
use crate::runtime::builtins::{regexp_object_handle, regexp_regex};
use crate::runtime::{
    Closure, Object, ObjectHandle, ObjectKind, TjsHost, Variant, split_delimited_string,
    split_string_by_regex,
};

use super::opcode::{OpcodeForm, binary_family, execute_binary_value, opcode_form};
use super::{CallOutcome, Continuation, DispatchFlags, Frame, Vm};

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
        let receiver_type = self.value_debug_type(&target);
        match &target {
            Variant::Void if !flags.must_exist => return Ok(Variant::Void),
            Variant::String(value) => return self.string_property(value, name),
            Variant::Octet(value) => return self.octet_property(value, name),
            _ => {}
        }

        let (handle, closure_this) = self.closure_parts(target).map_err(|error| {
            error.with_member_access(TjsMemberAccess {
                operation: TjsMemberOperation::Getting,
                receiver_type: receiver_type.clone(),
                member_name: name.to_string(),
                callee_type: None,
            })
        })?;
        let effective_this = self.effective_member_this(closure_this, caller_this)?;
        self.prop_get_handle(handle, name, flags, effective_this)
            .map_err(|error| {
                error.with_member_access(TjsMemberAccess {
                    operation: TjsMemberOperation::Getting,
                    receiver_type,
                    member_name: name.to_string(),
                    callee_type: None,
                })
            })
    }

    pub(super) fn prop_get_handle(
        &mut self,
        handle: ObjectHandle,
        name: &str,
        flags: DispatchFlags,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let kind = self.runtime.heap[handle.0].kind.clone();
        if let ObjectKind::Proxy {
            primary,
            fallback,
            bind_this,
        } = kind
        {
            if let Some(primary) = primary {
                let value =
                    self.prop_get_handle(primary, name, flags, bind_this.or(caller_this))?;
                if !matches!(value, Variant::Void) {
                    return Ok(self.bind_proxy_value(value, bind_this));
                }
                if bind_this.is_some() && self.handle_class_name_matches(primary, name) {
                    return Ok(Variant::Closure(Closure::new(primary, bind_this)));
                }
            }
            if let Some(this_obj) = bind_this
                && !flags.no_bound_instance_fallback
                && let Some(value) = self.runtime.heap[this_obj.0].get_raw(name)
                && !self.runtime.variant_is_property(&value)
            {
                return Ok(value);
            }
            return self.prop_get_handle(fallback, name, flags, caller_this);
        }

        let Some(value) = self.runtime.heap[handle.0].get_raw(name) else {
            let bind_this = self.bound_super_this(handle, caller_this)?;
            if let Some(this_obj) = bind_this
                && self.handle_class_name_matches(handle, name)
            {
                return Ok(Variant::Closure(Closure::new(handle, Some(this_obj))));
            }
            if let Some(class_handle) = self.super_class_handle(handle)? {
                let receiver = caller_this.or(Some(handle));
                let value = self.prop_get_handle(class_handle, name, flags, receiver)?;
                if !matches!(value, Variant::Void) {
                    return Ok(self.bind_proxy_value(value, receiver));
                }
            }
            if let Some(this_obj) = bind_this
                && !flags.no_bound_instance_fallback
                && let Some(value) = self.runtime.heap[this_obj.0].get_raw(name)
                && !self.runtime.variant_is_property(&value)
            {
                return Ok(value);
            }
            if let Some(value) = self.call_get_missing(handle, name)? {
                return Ok(value);
            }
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
        let bind_this = self.bound_super_this(handle, caller_this)?;
        // Native methods carry no receiver of their own; bind them to the
        // object they were read from so a fetched method called as a plain
        // value still runs on the receiver (krkrz ObjThis semantics).
        if bind_this.is_none()
            && let Variant::Object(native) = &value
            && matches!(
                self.runtime.heap[native.0].kind,
                ObjectKind::NativeFunction { .. } | ObjectKind::VmNativeFunction { .. }
            )
        {
            return Ok(self.bind_proxy_value(value, Some(handle)));
        }
        Ok(self.bind_proxy_value(value, bind_this))
    }

    pub(super) fn prop_set(
        &mut self,
        target: Variant,
        name: &str,
        value: Variant,
        flags: DispatchFlags,
        caller_this: Option<ObjectHandle>,
    ) -> Result<()> {
        let receiver_type = self.value_debug_type(&target);
        let (handle, closure_this) = self.closure_parts(target).map_err(|error| {
            error.with_member_access(TjsMemberAccess {
                operation: TjsMemberOperation::Setting,
                receiver_type: receiver_type.clone(),
                member_name: name.to_string(),
                callee_type: None,
            })
        })?;
        let effective_this = self.effective_member_this(closure_this, caller_this)?;
        self.prop_set_handle(handle, name, value, flags, effective_this)
            .map_err(|error| {
                error.with_member_access(TjsMemberAccess {
                    operation: TjsMemberOperation::Setting,
                    receiver_type,
                    member_name: name.to_string(),
                    callee_type: None,
                })
            })
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
        if let ObjectKind::Proxy {
            primary,
            fallback,
            bind_this,
        } = kind
        {
            if let Some(primary) = primary {
                let primary_value = self.member_in_super_chain(primary, name)?;
                let primary_has_member = primary_value.is_some();
                if primary_has_member || (bind_this.is_none() && flags.ensure) {
                    if let Some(this_obj) = bind_this {
                        if let Some(existing) = primary_value.clone()
                            && (!flags.ignore_prop
                                || self.runtime.variant_is_native_property(&existing))
                            && self
                                .property_setter(existing, value.clone(), Some(this_obj))?
                                .is_some()
                        {
                            return Ok(());
                        }
                        self.set_bound_member(this_obj, name, value);
                        return Ok(());
                    }
                    if let Some(this_obj) = caller_this
                        && primary_has_member
                    {
                        if let Some(existing) = primary_value.clone()
                            && (!flags.ignore_prop
                                || self.runtime.variant_is_native_property(&existing))
                            && self
                                .property_setter(existing, value.clone(), Some(this_obj))?
                                .is_some()
                        {
                            return Ok(());
                        }
                        self.set_bound_member(this_obj, name, value);
                        return Ok(());
                    }
                    return self.prop_set_handle(primary, name, value, flags, caller_this);
                }
            }
            if let Some(this_obj) = bind_this {
                self.set_bound_member(this_obj, name, value);
                return Ok(());
            }
            return self.prop_set_handle(fallback, name, value, flags, caller_this);
        }

        let member_exists = self.runtime.heap[handle.0].get_raw(name).is_some();
        if let Some(existing) = self.runtime.heap[handle.0].get_raw(name)
            && (!flags.ignore_prop || self.runtime.variant_is_native_property(&existing))
            && self
                .property_setter(existing, value.clone(), caller_this)?
                .is_some()
        {
            return Ok(());
        }
        if !flags.ignore_prop {
            let mut current = self.super_class_handle(handle)?;
            while let Some(class_handle) = current {
                if let Some(existing) = self.runtime.heap[class_handle.0].get_raw(name)
                    && self
                        .property_setter(existing, value.clone(), caller_this.or(Some(handle)))?
                        .is_some()
                {
                    return Ok(());
                }
                current = self.super_class_handle(class_handle)?;
            }
        }
        if !member_exists && self.call_set_missing(handle, name, value.clone())? {
            return Ok(());
        }
        if let Some(this_obj) = self.bound_super_this(handle, caller_this)? {
            self.set_bound_member(this_obj, name, value);
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

    fn call_get_missing(&mut self, handle: ObjectHandle, name: &str) -> Result<Option<Variant>> {
        let Some(value_property) = self.call_missing_method(handle, false, name, Variant::Void)?
        else {
            return Ok(None);
        };
        let (result, property) = value_property;
        if result.is_truthy() {
            return self
                .default_prop_get(Variant::Object(property), Some(handle))
                .map(Some);
        }
        Ok(None)
    }

    fn call_set_missing(
        &mut self,
        handle: ObjectHandle,
        name: &str,
        value: Variant,
    ) -> Result<bool> {
        let Some((result, _property)) = self.call_missing_method(handle, true, name, value)? else {
            return Ok(false);
        };
        Ok(result.is_truthy())
    }

    fn call_missing_method(
        &mut self,
        handle: ObjectHandle,
        is_set: bool,
        name: &str,
        initial_value: Variant,
    ) -> Result<Option<(Variant, ObjectHandle)>> {
        let object = &self.runtime.heap[handle.0];
        if !object.call_missing || object.processing_missing {
            return Ok(None);
        }
        let missing_name = object.missing_name.clone();
        if missing_name.is_empty() {
            return Ok(None);
        }

        let value_property = self.runtime.alloc_value_property(initial_value);
        let args = vec![
            Variant::Integer(i64::from(is_set)),
            Variant::String(name.to_string()),
            Variant::Object(value_property),
        ];

        self.runtime.heap[handle.0].processing_missing = true;
        let result = (|| {
            let missing = self.prop_get_handle(
                handle,
                &missing_name,
                DispatchFlags::no_bound_instance_fallback(),
                Some(handle),
            )?;
            if matches!(missing, Variant::Void) {
                return Ok(Variant::Integer(0));
            }
            self.call_value_sync(missing, Some(handle), args)
        })();
        self.runtime.heap[handle.0].processing_missing = false;

        result.map(|result| Some((result, value_property)))
    }

    fn call_value_sync(
        &mut self,
        callee: Variant,
        this_obj: Option<ObjectHandle>,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        let base_depth = self.runtime.call_depth;
        match self.call_value(callee, this_obj, args, false, Continuation::Root)? {
            CallOutcome::Immediate(value, Continuation::Root) => Ok(value),
            CallOutcome::Immediate(_, continuation) => Err(TjsError::runtime(format!(
                "unexpected missing continuation {continuation:?}"
            ))),
            CallOutcome::Frame(frame) => self.run_call_stack(vec![*frame], base_depth),
        }
    }

    fn member_in_super_chain(
        &mut self,
        handle: ObjectHandle,
        name: &str,
    ) -> Result<Option<Variant>> {
        let mut current = Some(handle);
        while let Some(class_handle) = current {
            if let Some(value) = self.runtime.heap[class_handle.0].get_raw(name) {
                return Ok(Some(value));
            }
            current = self.super_class_handle(class_handle)?;
        }
        Ok(None)
    }
    fn set_bound_member(&mut self, handle: ObjectHandle, name: &str, value: Variant) {
        let mut value = self.materialize_code_object(value);
        if let Variant::Closure(closure) = &mut value
            && closure.this_obj.is_none()
        {
            closure.this_obj = Some(handle);
        }
        self.runtime.heap[handle.0].set(name, value);
    }

    pub(super) fn default_prop_get(
        &mut self,
        target: Variant,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let (handle, closure_this) = self.closure_parts(target)?;
        let kind = self.runtime.heap[handle.0].kind.clone();
        if let ObjectKind::InterCode {
            file_id,
            object_index,
            context: BytecodeContextType::Property,
        } = kind
        {
            let file = self.runtime.script_file(file_id)?;
            let getter = file.objects[object_index].prop_getter;
            if let Some(getter) = getter {
                let effective_this = self.effective_member_this(closure_this, caller_this)?;
                return self.execute_file_object_with_this(
                    file_id,
                    getter,
                    Vec::new(),
                    effective_this,
                );
            }
            return Err(TjsError::runtime("property has no getter"));
        }
        if let ObjectKind::NativeProperty { id } = kind {
            let property = self
                .runtime
                .native_properties
                .get(id)
                .cloned()
                .ok_or_else(|| TjsError::runtime(format!("native property {id} missing")))?;
            let effective_this = self.effective_member_this(closure_this, caller_this)?;
            return property.get(self.runtime, effective_this);
        }
        let effective_this = self.effective_member_this(closure_this, caller_this)?;
        self.prop_get_handle(handle, "value", DispatchFlags::default(), effective_this)
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
            file_id,
            object_index,
            context: BytecodeContextType::Property,
        } = kind
        {
            let file = self.runtime.script_file(file_id)?;
            let setter = file.objects[object_index].prop_setter;
            if let Some(setter) = setter {
                let effective_this = self.effective_member_this(closure_this, caller_this)?;
                self.execute_file_object_with_this(file_id, setter, vec![value], effective_this)?;
                return Ok(());
            }
            return Err(TjsError::runtime("property has no setter"));
        }
        if let ObjectKind::NativeProperty { id } = kind {
            let property = self
                .runtime
                .native_properties
                .get(id)
                .cloned()
                .ok_or_else(|| TjsError::runtime(format!("native property {id} missing")))?;
            let effective_this = self.effective_member_this(closure_this, caller_this)?;
            property.set(self.runtime, effective_this, value)?;
            return Ok(());
        }
        let effective_this = self.effective_member_this(closure_this, caller_this)?;
        self.prop_set_handle(
            handle,
            "value",
            value,
            DispatchFlags::default(),
            effective_this,
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
        match self.runtime.heap[handle.0].kind {
            ObjectKind::InterCode {
                file_id,
                object_index,
                context: BytecodeContextType::Property,
            } => {
                let file = self.runtime.script_file(file_id)?;
                let Some(getter) = file.objects[object_index].prop_getter else {
                    return Ok(None);
                };
                let effective_this = self.effective_member_this(closure_this, caller_this)?;
                Ok(Some(self.execute_file_object_with_this(
                    file_id,
                    getter,
                    Vec::new(),
                    effective_this,
                )?))
            }
            ObjectKind::NativeProperty { id } => {
                let property = self
                    .runtime
                    .native_properties
                    .get(id)
                    .cloned()
                    .ok_or_else(|| TjsError::runtime(format!("native property {id} missing")))?;
                let effective_this = self.effective_member_this(closure_this, caller_this)?;
                Ok(Some(property.get(self.runtime, effective_this)?))
            }
            _ => Ok(None),
        }
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
        match self.runtime.heap[handle.0].kind {
            ObjectKind::InterCode {
                file_id,
                object_index,
                context: BytecodeContextType::Property,
            } => {
                let file = self.runtime.script_file(file_id)?;
                let Some(setter) = file.objects[object_index].prop_setter else {
                    return Ok(None);
                };
                let effective_this = self.effective_member_this(closure_this, caller_this)?;
                self.execute_file_object_with_this(file_id, setter, vec![value], effective_this)?;
                Ok(Some(()))
            }
            ObjectKind::NativeProperty { id } => {
                let property = self
                    .runtime
                    .native_properties
                    .get(id)
                    .cloned()
                    .ok_or_else(|| TjsError::runtime(format!("native property {id} missing")))?;
                let effective_this = self.effective_member_this(closure_this, caller_this)?;
                property.set(self.runtime, effective_this, value)?;
                Ok(Some(()))
            }
            _ => Ok(None),
        }
    }

    pub(super) fn delete_member(&mut self, target: Variant, name: &str) -> Result<bool> {
        let receiver_type = self.value_debug_type(&target);
        let handle = self.resolve_object(target).map_err(|error| {
            error.with_member_access(TjsMemberAccess {
                operation: TjsMemberOperation::Deleting,
                receiver_type,
                member_name: name.to_string(),
                callee_type: None,
            })
        })?;
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
        let base_depth = self.runtime.call_depth;
        match self.call_member_direct_cont(object_value, name, args, None, Continuation::Root)? {
            CallOutcome::Immediate(value, Continuation::Root) => {
                Ok(if dest_reg == 0 { Variant::Void } else { value })
            }
            CallOutcome::Immediate(_, continuation) => Err(TjsError::runtime(format!(
                "unexpected immediate member call continuation {continuation:?}"
            ))),
            CallOutcome::Frame(frame) => {
                let value = self.run_call_stack(vec![*frame], base_depth)?;
                Ok(if dest_reg == 0 { Variant::Void } else { value })
            }
        }
    }

    pub(super) fn call_member_direct_cont(
        &mut self,
        object_value: Variant,
        name: &str,
        args: Vec<Variant>,
        caller_this: Option<ObjectHandle>,
        continuation: Continuation,
    ) -> Result<CallOutcome> {
        let receiver_type = self.value_debug_type(&object_value);
        if let Variant::String(value) = &object_value {
            return Ok(CallOutcome::Immediate(
                self.call_string_method(value.clone(), name, args)?,
                continuation,
            ));
        }
        if let Variant::Octet(value) = &object_value {
            return Ok(CallOutcome::Immediate(
                self.call_octet_method(value.clone(), name, args)?,
                continuation,
            ));
        }

        let (handle, closure_this) = self.closure_parts(object_value.clone()).map_err(|error| {
            error.with_member_access(TjsMemberAccess {
                operation: TjsMemberOperation::Calling,
                receiver_type: receiver_type.clone(),
                member_name: name.to_string(),
                callee_type: None,
            })
        })?;
        let mut member = if let Some(this_obj) = self.bound_super_this(handle, caller_this)?
            && self.handle_class_name_matches(handle, name)
        {
            self.runtime.heap[this_obj.0]
                .get_raw(name)
                .unwrap_or_else(|| Variant::Closure(Closure::new(handle, Some(this_obj))))
        } else {
            self.prop_get_handle(
                handle,
                name,
                DispatchFlags::no_bound_instance_fallback(),
                closure_this.or(Some(handle)),
            )
            .map_err(|error| {
                error.with_member_access(TjsMemberAccess {
                    operation: TjsMemberOperation::Calling,
                    receiver_type: receiver_type.clone(),
                    member_name: name.to_string(),
                    callee_type: None,
                })
            })?
        };
        // TJS2 emits one entry point per class extender in the superclass
        // getter. A class-qualified call searches those entries in reverse
        // declaration order and retains the caller's ObjThis. Keep this
        // lookup confined to calls: applying it to ordinary property reads
        // changes the meaning of class initialization.
        if matches!(member, Variant::Void)
            && let Some(this_obj) = caller_this
            && let Some(secondary) = self.secondary_class_member_for_call(handle, this_obj, name)?
        {
            member = secondary;
        }
        let bind_this = self.bound_super_this(handle, caller_this)?;
        let member = self.bind_proxy_value(member, bind_this);
        let callee_type = self.value_debug_type(&member);
        let receiver = self.receiver_this(handle);
        let receiver_this = if let Some(this_obj) = caller_this
            && self.is_class_in_instance_chain(this_obj, handle)?
        {
            this_obj
        } else {
            receiver
        };
        self.call_value(
            member,
            closure_this.or(Some(receiver_this)),
            args,
            false,
            continuation,
        )
        .map_err(|error| {
            error.with_member_access(TjsMemberAccess {
                operation: TjsMemberOperation::Calling,
                receiver_type,
                member_name: name.to_string(),
                callee_type: Some(callee_type),
            })
        })
    }

    fn secondary_class_member_for_call(
        &mut self,
        qualifier: ObjectHandle,
        instance: ObjectHandle,
        name: &str,
    ) -> Result<Option<Variant>> {
        if !matches!(
            self.runtime.heap[qualifier.0].kind,
            ObjectKind::InterCode {
                context: BytecodeContextType::Class,
                ..
            }
        ) || !self.is_class_in_instance_chain(instance, qualifier)?
        {
            return Ok(None);
        }

        if let Some(member) = self.superclass_getter_member_for_call(qualifier, instance, name)? {
            return Ok(Some(member));
        }

        // Source compiled by older Kirakira versions does not retain the
        // bytecode entry offsets above. Preserve its existing equivalent
        // fallback while parsed KRKR2/KRKRZ bytecode takes the exact path.
        let qualifier_names = self.runtime.heap[qualifier.0].class_infos.clone();
        let instance_names = self.runtime.heap[instance.0].class_infos.clone();
        let Some(start) = instance_names.iter().position(|instance_name| {
            qualifier_names
                .iter()
                .any(|qualifier_name| qualifier_name == instance_name)
        }) else {
            return Ok(None);
        };

        // `instance_names` is recorded in extender execution order. The
        // primary superclass chain was already searched above, so only a
        // later class body can supply the missing member here.
        for class_name in &instance_names[start + 1..] {
            let class_handle = match self.runtime.global_member(class_name) {
                Variant::Object(handle) => handle,
                Variant::Closure(closure) => closure.object,
                _ => continue,
            };
            if !matches!(
                self.runtime.heap[class_handle.0].kind,
                ObjectKind::InterCode {
                    context: BytecodeContextType::Class,
                    ..
                }
            ) {
                continue;
            }
            if let Some(member) = self.runtime.heap[class_handle.0].get_raw(name) {
                return Ok(Some(self.bind_proxy_value(member, Some(instance))));
            }
        }
        Ok(None)
    }

    fn superclass_getter_member_for_call(
        &mut self,
        class_handle: ObjectHandle,
        instance: ObjectHandle,
        name: &str,
    ) -> Result<Option<Variant>> {
        let ObjectKind::InterCode {
            file_id,
            object_index,
            context: BytecodeContextType::Class,
        } = self.runtime.heap[class_handle.0].kind
        else {
            return Ok(None);
        };
        let file = self.runtime.script_file(file_id)?;
        let Some(getter_index) = file.objects[object_index].super_class_getter else {
            return Ok(None);
        };
        let pointers = file.objects[getter_index]
            .super_class_getter_pointers
            .clone();
        if pointers.is_empty() {
            return Ok(None);
        }

        for pointer in pointers.into_iter().rev() {
            let code_offset = usize::try_from(pointer)
                .map_err(|_| TjsError::runtime("negative superclass getter entry offset"))?;
            let value = self.execute_file_object_with_this_preserving_active_at(
                file_id,
                getter_index,
                code_offset,
                Vec::new(),
                Some(self.runtime.global),
            )?;
            let super_handle = self.resolve_object(value)?;
            if let Some(member) = self.class_member_for_call(super_handle, instance, name)? {
                return Ok(Some(member));
            }
        }
        Ok(None)
    }

    fn class_member_for_call(
        &mut self,
        class_handle: ObjectHandle,
        instance: ObjectHandle,
        name: &str,
    ) -> Result<Option<Variant>> {
        if let Some(member) = self.runtime.heap[class_handle.0].get_raw(name) {
            return Ok(Some(self.bind_proxy_value(member, Some(instance))));
        }
        self.superclass_getter_member_for_call(class_handle, instance, name)
    }

    fn is_class_in_instance_chain(
        &mut self,
        instance: ObjectHandle,
        class_handle: ObjectHandle,
    ) -> Result<bool> {
        let mut seen = Vec::new();
        let mut current = self.super_class_handle(instance)?;
        while let Some(handle) = current {
            if handle == class_handle {
                return Ok(true);
            }
            if seen.contains(&handle.0) {
                break;
            }
            seen.push(handle.0);
            current = self.super_class_handle(handle)?;
        }
        // TJS2 multiple inheritance initializes one instance from several
        // class bodies, but only the first parent is reachable through the
        // super-class link; fall back to the recorded class names so the
        // other parents still count as part of the instance's chain. Only
        // applies to actual class objects — two instances of the same class
        // must not be treated as each other's class.
        if matches!(
            self.runtime.heap[class_handle.0].kind,
            ObjectKind::InterCode {
                context: BytecodeContextType::Class,
                ..
            }
        ) {
            let class_names = self.runtime.heap[class_handle.0].class_infos.clone();
            if !class_names.is_empty() {
                let instance_names = &self.runtime.heap[instance.0].class_infos;
                if class_names.iter().any(|name| instance_names.contains(name)) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn call_object_method(
        &mut self,
        object: ObjectHandle,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        self.call_member_direct(Variant::Object(object), name, args, 1)
    }

    pub(crate) fn call_secondary_class_method(
        &mut self,
        object: ObjectHandle,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<bool> {
        let class_infos = self.runtime.heap[object.0].class_infos.clone();
        for class_name in class_infos.into_iter().rev() {
            let class_handle = match self.runtime.global_member(&class_name) {
                Variant::Object(handle) => handle,
                Variant::Closure(closure) => closure.object,
                _ => continue,
            };
            // Native classes supply the ordinary fallback implementation.
            // This path is specifically for TJS secondary class bodies.
            if !matches!(
                self.runtime.heap[class_handle.0].kind,
                ObjectKind::InterCode {
                    context: BytecodeContextType::Class,
                    ..
                }
            ) {
                continue;
            }
            let Some(member) = self.runtime.heap[class_handle.0].get_raw(name) else {
                continue;
            };
            let member = self.bind_proxy_value(member, Some(object));
            let base_depth = self.runtime.call_depth;
            match self.call_value(
                member,
                Some(object),
                args.clone(),
                false,
                Continuation::Root,
            )? {
                CallOutcome::Immediate(_, Continuation::Root) => {}
                CallOutcome::Immediate(_, continuation) => {
                    return Err(TjsError::runtime(format!(
                        "unexpected immediate class method continuation {continuation:?}"
                    )));
                }
                CallOutcome::Frame(frame) => {
                    self.run_call_stack(vec![*frame], base_depth)?;
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub fn call_variant_method(
        &mut self,
        object: Variant,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        self.call_member_direct(object, name, args, 1)
    }

    pub fn call_function(&mut self, callee: Variant, args: Vec<Variant>) -> Result<Variant> {
        let base_depth = self.runtime.call_depth;
        match self.call_value(callee, None, args, false, Continuation::Root)? {
            CallOutcome::Immediate(value, Continuation::Root) => Ok(value),
            CallOutcome::Immediate(_, continuation) => Err(TjsError::runtime(format!(
                "unexpected immediate function call continuation {continuation:?}"
            ))),
            CallOutcome::Frame(frame) => self.run_call_stack(vec![*frame], base_depth),
        }
    }

    pub(super) fn invalidate_object(&mut self, handle: ObjectHandle) -> Result<bool> {
        if !self.runtime.heap[handle.0].valid {
            return Ok(false);
        }
        if self.runtime.heap[handle.0].invalidating {
            return Ok(false);
        }

        self.runtime.heap[handle.0].invalidating = true;
        let result = (|| {
            let finalize =
                self.prop_get_handle(handle, "finalize", DispatchFlags::default(), Some(handle))?;
            if !matches!(finalize, Variant::Void) {
                let base_depth = self.runtime.call_depth;
                match self.call_value(
                    finalize,
                    Some(handle),
                    Vec::new(),
                    false,
                    Continuation::Root,
                )? {
                    CallOutcome::Immediate(_, Continuation::Root) => {}
                    CallOutcome::Immediate(_, continuation) => {
                        return Err(TjsError::runtime(format!(
                            "unexpected finalize continuation {continuation:?}"
                        )));
                    }
                    CallOutcome::Frame(frame) => {
                        self.run_call_stack(vec![*frame], base_depth)?;
                    }
                }
            }
            Ok(())
        })();

        if let Err(error) = result {
            self.runtime.heap[handle.0].invalidating = false;
            return Err(error);
        }

        self.runtime.heap[handle.0].valid = false;
        self.runtime.heap[handle.0].invalidating = false;
        self.runtime.host_mut().invalidate_object(handle);
        Ok(true)
    }

    pub(super) fn call_value(
        &mut self,
        callee: Variant,
        this_obj: Option<ObjectHandle>,
        args: Vec<Variant>,
        is_new: bool,
        continuation: Continuation,
    ) -> Result<CallOutcome> {
        match self.materialize_code_object(callee) {
            Variant::Closure(closure) => {
                // A method that was registered bound to a class prototype
                // must still run on the caller's instance when invoked from
                // within a construction chain (krkrz superclass-constructor
                // semantics), otherwise state would land on the prototype.
                let effective_this = match (closure.this_obj, this_obj) {
                    (Some(bound), Some(caller))
                        if bound != caller
                            && matches!(
                                self.runtime.heap[bound.0].kind,
                                ObjectKind::InterCode {
                                    context: BytecodeContextType::Class,
                                    ..
                                }
                            )
                            && self.is_class_in_instance_chain(caller, bound)? =>
                    {
                        Some(caller)
                    }
                    _ => closure.this_obj.or(this_obj).or(Some(closure.object)),
                };
                self.call_handle(closure.object, effective_this, args, is_new, continuation)
            }
            Variant::Object(handle) => {
                self.call_handle(handle, this_obj, args, is_new, continuation)
            }
            other => Err(TjsError::runtime(format!("{other} is not callable"))),
        }
    }

    fn call_handle(
        &mut self,
        handle: ObjectHandle,
        this_obj: Option<ObjectHandle>,
        args: Vec<Variant>,
        is_new: bool,
        continuation: Continuation,
    ) -> Result<CallOutcome> {
        let kind = self.runtime.heap[handle.0].kind.clone();
        match kind {
            ObjectKind::InterCode {
                file_id,
                object_index,
                context,
            } => {
                if is_new {
                    self.create_new_inter_code(file_id, object_index, context, args, continuation)
                } else if context == BytecodeContextType::Class {
                    if let Some(instance) = this_obj.filter(|handle| *handle != self.runtime.global)
                    {
                        // A plain call to a class object with an instance
                        // `this` only runs the class body (member
                        // initializers); krkrz performs superclass
                        // initialization this way and never invokes the
                        // constructor here. The constructor runs separately
                        // through `new` or an explicit ctor call.
                        self.initialize_inter_code_class_body(
                            file_id,
                            object_index,
                            instance,
                            continuation,
                        )
                    } else {
                        self.create_new_inter_code(
                            file_id,
                            object_index,
                            context,
                            args,
                            continuation,
                        )
                    }
                } else {
                    Ok(CallOutcome::Frame(Box::new(self.create_call_frame(
                        file_id,
                        object_index,
                        args,
                        this_obj,
                        continuation,
                    )?)))
                }
            }
            ObjectKind::NativeFunction { id, constructable } => {
                let function = self
                    .runtime
                    .native_functions
                    .get(id)
                    .cloned()
                    .ok_or_else(|| TjsError::runtime(format!("native function {id} missing")))?;
                let native_this = if is_new && constructable {
                    None
                } else {
                    this_obj
                };
                Ok(CallOutcome::Immediate(
                    function.call(self.runtime, native_this, args)?,
                    continuation,
                ))
            }
            ObjectKind::VmNativeFunction { id } => {
                let function = self
                    .runtime
                    .vm_native_functions
                    .get(id)
                    .cloned()
                    .ok_or_else(|| TjsError::runtime(format!("VM native function {id} missing")))?;
                Ok(CallOutcome::Immediate(
                    function.call(self, this_obj, args)?,
                    continuation,
                ))
            }
            ObjectKind::Proxy {
                primary,
                fallback,
                bind_this,
            } => {
                let proxy_this = bind_this.or(this_obj);
                if let Some(primary) = primary {
                    self.call_handle(primary, proxy_this, args, is_new, continuation)
                } else {
                    self.call_handle(fallback, proxy_this, args, is_new, continuation)
                }
            }
            ObjectKind::Ordinary | ObjectKind::Array { .. } | ObjectKind::NativeProperty { .. } => {
                let object_type = self.object_debug_type(handle, "object");
                Err(TjsError::runtime(format!("{object_type} is not callable")))
            }
        }
    }

    fn create_new_inter_code(
        &mut self,
        file_id: usize,
        object_index: usize,
        context: BytecodeContextType,
        args: Vec<Variant>,
        continuation: Continuation,
    ) -> Result<CallOutcome> {
        let instance = self.runtime.alloc_object(Object::default());
        let file = self.runtime.script_file(file_id)?;
        let code_handles = self.runtime.script_code_handles(file_id)?;
        let name = file.objects[object_index]
            .name(file.as_ref())
            .unwrap_or("")
            .to_string();

        if context == BytecodeContextType::Class {
            self.add_class_info(instance, name.clone());
            let class_handle = code_handles[object_index];
            let frame = self.create_call_frame(
                file_id,
                object_index,
                Vec::new(),
                Some(instance),
                Continuation::ClassBody {
                    instance,
                    class_handle,
                    class_name: name,
                    constructor_args: args,
                    run_constructor: true,
                    target: Box::new(continuation),
                },
            )?;
            Ok(CallOutcome::Frame(Box::new(frame)))
        } else {
            let frame = self.create_call_frame(
                file_id,
                object_index,
                args,
                Some(instance),
                Continuation::ReturnFixed {
                    value: Variant::Object(instance),
                    target: Box::new(continuation),
                },
            )?;
            Ok(CallOutcome::Frame(Box::new(frame)))
        }
    }

    fn initialize_inter_code_class_body(
        &mut self,
        file_id: usize,
        object_index: usize,
        instance: ObjectHandle,
        continuation: Continuation,
    ) -> Result<CallOutcome> {
        let file = self.runtime.script_file(file_id)?;
        let code_handles = self.runtime.script_code_handles(file_id)?;
        let name = file.objects[object_index]
            .name(file.as_ref())
            .unwrap_or("")
            .to_string();

        if !name.is_empty() {
            self.add_class_info(instance, name.clone());
        }
        let class_handle = code_handles[object_index];
        // The superclass link is only attached once the class body has run
        // (see Continuation::ClassBody): while the body is executing, member
        // lookups on the under-construction instance must miss so that they
        // fall back to the global object, matching krkrz regmember semantics.
        let frame = self.create_call_frame(
            file_id,
            object_index,
            Vec::new(),
            Some(instance),
            Continuation::ClassBody {
                instance,
                class_handle,
                class_name: name,
                constructor_args: Vec::new(),
                run_constructor: false,
                target: Box::new(continuation),
            },
        )?;
        Ok(CallOutcome::Frame(Box::new(frame)))
    }

    pub(super) fn apply_class_extender(
        &mut self,
        class_handle: ObjectHandle,
        getter_handle: ObjectHandle,
        instance: ObjectHandle,
        continuation: Continuation,
    ) -> Result<CallOutcome> {
        let Some(super_handle) =
            self.resolve_super_class_from_getter(class_handle, getter_handle)?
        else {
            return Ok(CallOutcome::Immediate(Variant::Void, continuation));
        };

        match self.runtime.heap[super_handle.0].kind {
            ObjectKind::InterCode {
                file_id,
                object_index,
                context: BytecodeContextType::Class,
            } => {
                self.initialize_inter_code_class_body(file_id, object_index, instance, continuation)
            }
            _ => Ok(CallOutcome::Immediate(Variant::Void, continuation)),
        }
    }

    pub(super) fn super_class_handle(
        &mut self,
        handle: ObjectHandle,
    ) -> Result<Option<ObjectHandle>> {
        if let Some(super_handle) = self.runtime.heap[handle.0].super_class {
            return Ok(Some(super_handle));
        }

        let ObjectKind::InterCode {
            file_id,
            object_index,
            context: BytecodeContextType::Class,
        } = self.runtime.heap[handle.0].kind
        else {
            return Ok(None);
        };
        let file = self.runtime.script_file(file_id)?;
        let Some(getter_index) = file.objects[object_index].super_class_getter else {
            return Ok(None);
        };
        let code_handles = self.runtime.script_code_handles(file_id)?;
        let getter_handle = code_handles[getter_index];
        self.resolve_super_class_from_getter(handle, getter_handle)
    }

    fn resolve_super_class_from_getter(
        &mut self,
        class_handle: ObjectHandle,
        getter_handle: ObjectHandle,
    ) -> Result<Option<ObjectHandle>> {
        let ObjectKind::InterCode {
            file_id,
            object_index,
            context: BytecodeContextType::SuperClassGetter,
        } = self.runtime.heap[getter_handle.0].kind
        else {
            return Err(TjsError::runtime(
                "class extender does not reference a superclass getter",
            ));
        };

        let value = self.execute_file_object_with_this_preserving_active(
            file_id,
            object_index,
            Vec::new(),
            Some(self.runtime.global),
        )?;
        if matches!(value, Variant::Void | Variant::Null) {
            return Ok(None);
        }
        let super_handle = self.resolve_object(value)?;
        if self.runtime.heap[class_handle.0].super_class.is_none() {
            self.runtime.heap[class_handle.0].super_class = Some(super_handle);
        }
        Ok(Some(super_handle))
    }

    fn execute_file_object_with_this_preserving_active(
        &mut self,
        file_id: usize,
        object_index: usize,
        args: Vec<Variant>,
        this_obj: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let saved_file_id = self.file_id;
        let saved_file = Arc::clone(&self.file);
        let saved_code_handles = self.code_handles.clone();
        let result = self.execute_file_object_with_this(file_id, object_index, args, this_obj);
        self.file_id = saved_file_id;
        self.file = saved_file;
        self.code_handles = saved_code_handles;
        result
    }

    fn execute_file_object_with_this_preserving_active_at(
        &mut self,
        file_id: usize,
        object_index: usize,
        code_offset: usize,
        args: Vec<Variant>,
        this_obj: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let saved_file_id = self.file_id;
        let saved_file = Arc::clone(&self.file);
        let saved_code_handles = self.code_handles.clone();
        let result = self.execute_file_object_with_this_at(
            file_id,
            object_index,
            code_offset,
            args,
            this_obj,
        );
        self.file_id = saved_file_id;
        self.file = saved_file;
        self.code_handles = saved_code_handles;
        result
    }

    fn call_string_method(
        &mut self,
        value: String,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        match name {
            "toString" => Ok(Variant::String(value)),
            "escape" => Ok(Variant::String(escape_tjs_string_fragment(&value))),
            "sprintf" => Ok(Variant::String(sprintf_tjs_string(&value, &args)?)),
            "toLowerCase" => Ok(Variant::String(string_to_ascii_lowercase(&value))),
            "toUpperCase" => Ok(Variant::String(string_to_ascii_uppercase(&value))),
            "charAt" => {
                if args.len() != 1 {
                    return Err(TjsError::runtime("String.charAt requires one argument"));
                }
                let index = args[0].to_integer()?;
                let Some(ch) = usize::try_from(index)
                    .ok()
                    .and_then(|index| value.chars().nth(index))
                else {
                    return Ok(Variant::String(String::new()));
                };
                Ok(Variant::String(ch.to_string()))
            }
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
            "replace" => {
                let Some(pattern) = args.first() else {
                    return Ok(Variant::String(value));
                };
                let replacement = args
                    .get(1)
                    .map(Variant::to_tjs_string)
                    .transpose()?
                    .unwrap_or_default();
                self.string_replace(&value, pattern, &replacement)
            }
            "indexOf" => {
                let needle = args
                    .first()
                    .map(Variant::to_tjs_string)
                    .transpose()?
                    .unwrap_or_default();
                let start = args
                    .get(1)
                    .map(Variant::to_integer)
                    .transpose()?
                    .unwrap_or(0)
                    .max(0) as usize;
                Ok(Variant::Integer(string_index_of(&value, &needle, start)))
            }
            "lastIndexOf" => {
                let needle = args
                    .first()
                    .map(Variant::to_tjs_string)
                    .transpose()?
                    .unwrap_or_default();
                let end = args
                    .get(1)
                    .map(Variant::to_integer)
                    .transpose()?
                    .map(|value| value.max(0) as usize);
                Ok(Variant::Integer(string_last_index_of(&value, &needle, end)))
            }
            "split" => {
                let Some(pattern) = args.first() else {
                    return Err(TjsError::runtime(
                        "String.split requires a delimiter argument",
                    ));
                };
                let purge_empty = args
                    .get(2)
                    .filter(|value| !matches!(value, Variant::Void))
                    .is_some_and(Variant::is_truthy);
                if let Some(regexp) = regexp_object_handle(&self.runtime, pattern) {
                    let regex = regexp_regex(&self.runtime, regexp)?;
                    let array = self.runtime.alloc_array_object(split_string_by_regex(
                        &value,
                        &regex,
                        purge_empty,
                    ));
                    return Ok(Variant::Object(array));
                }
                let delimiters = pattern.to_tjs_string()?;
                let array = self.runtime.alloc_array_object(split_delimited_string(
                    &value,
                    &delimiters,
                    purge_empty,
                ));
                Ok(Variant::Object(array))
            }
            "trim" => {
                if !args.is_empty() {
                    return Err(TjsError::runtime("String.trim does not accept arguments"));
                }
                Ok(Variant::String(trim_tjs_ascii_space(&value).to_string()))
            }
            "reverse" => {
                if !args.is_empty() {
                    return Err(TjsError::runtime(
                        "String.reverse does not accept arguments",
                    ));
                }
                Ok(Variant::String(value.chars().rev().collect()))
            }
            "repeat" => {
                if args.len() != 1 {
                    return Err(TjsError::runtime("String.repeat requires one argument"));
                }
                let count = args[0].to_integer()?;
                if count <= 0 || value.is_empty() {
                    return Ok(Variant::String(String::new()));
                }
                Ok(Variant::String(value.repeat(count as usize)))
            }
            _ => Err(TjsError::runtime(format!(
                "string method `{name}` not found"
            ))),
        }
    }

    fn string_replace(&self, value: &str, pattern: &Variant, replacement: &str) -> Result<Variant> {
        if let Variant::Object(handle) = pattern {
            let object = &self.runtime.heap[handle.0];
            let pattern = object.get("pattern").to_tjs_string()?;
            let flags = object.get("flags").to_tjs_string()?;
            if pattern == "[^A-Za-z]" {
                let replaced = value
                    .chars()
                    .map(|ch| {
                        if ch.is_ascii_alphabetic() {
                            ch.to_string()
                        } else {
                            replacement.to_string()
                        }
                    })
                    .collect::<String>();
                return Ok(Variant::String(replaced));
            }
            if flags.contains('g') {
                return Ok(Variant::String(value.replace(&pattern, replacement)));
            }
            return Ok(Variant::String(value.replacen(&pattern, replacement, 1)));
        }

        let pattern = pattern.to_tjs_string()?;
        Ok(Variant::String(value.replacen(&pattern, replacement, 1)))
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

    fn receiver_this(&self, handle: ObjectHandle) -> ObjectHandle {
        match self.runtime.heap[handle.0].kind {
            ObjectKind::Proxy {
                primary: Some(primary),
                ..
            } => primary,
            _ => handle,
        }
    }

    fn bound_super_this(
        &mut self,
        class_handle: ObjectHandle,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Option<ObjectHandle>> {
        let Some(this_obj) = caller_this else {
            return Ok(None);
        };
        if this_obj == self.runtime.global {
            return Ok(None);
        }
        if self.is_class_in_instance_chain(this_obj, class_handle)? {
            return Ok(Some(this_obj));
        }
        Ok(None)
    }

    fn effective_member_this(
        &mut self,
        closure_this: Option<ObjectHandle>,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Option<ObjectHandle>> {
        let Some(caller_this) = caller_this else {
            return Ok(closure_this);
        };
        if caller_this == self.runtime.global {
            return Ok(closure_this.or(Some(caller_this)));
        }
        if let Some(closure_this) = closure_this {
            if closure_this == self.runtime.global
                || self.is_class_in_instance_chain(caller_this, closure_this)?
            {
                return Ok(Some(caller_this));
            }
            return Ok(Some(closure_this));
        }
        Ok(Some(caller_this))
    }

    fn bind_proxy_value(&self, value: Variant, bind_this: Option<ObjectHandle>) -> Variant {
        let Some(this_obj) = bind_this else {
            return value;
        };
        match self.materialize_code_object(value) {
            Variant::Closure(mut closure) => {
                closure.this_obj = Some(this_obj);
                Variant::Closure(closure)
            }
            Variant::Object(handle) => {
                // Only callable members are rebound to the caller's `this`;
                // data members (arrays, dictionaries, plain objects) keep
                // their identity, matching krkrz where ObjThis is assigned
                // when the member is written, not when it is read.
                match self.runtime.heap[handle.0].kind {
                    ObjectKind::InterCode { .. }
                    | ObjectKind::NativeFunction { .. }
                    | ObjectKind::VmNativeFunction { .. }
                    | ObjectKind::Proxy { .. } => {
                        Variant::Closure(Closure::new(handle, Some(this_obj)))
                    }
                    ObjectKind::Ordinary
                    | ObjectKind::Array { .. }
                    | ObjectKind::NativeProperty { .. } => Variant::Object(handle),
                }
            }
            value => value,
        }
    }

    fn handle_class_name_matches(&self, handle: ObjectHandle, name: &str) -> bool {
        if self.runtime.heap[handle.0]
            .class_infos
            .iter()
            .any(|info| info == name)
        {
            return true;
        }
        let ObjectKind::InterCode {
            file_id,
            object_index,
            context: BytecodeContextType::Class,
        } = self.runtime.heap[handle.0].kind
        else {
            return false;
        };
        self.runtime
            .script_file(file_id)
            .ok()
            .and_then(|file| {
                file.objects
                    .get(object_index)
                    .and_then(|object| object.name(&file).map(str::to_string))
            })
            .is_some_and(|class_name| class_name == name)
    }

    pub(super) fn key_from_variant(&self, value: &Variant) -> Result<String> {
        match value {
            Variant::Integer(value) => Ok(value.to_string()),
            _ => value.to_tjs_string(),
        }
    }

    pub(super) fn optional_object(&self, value: Variant) -> Result<Option<ObjectHandle>> {
        match self.materialize_code_object(value) {
            Variant::Object(handle) => Ok(Some(handle)),
            Variant::Closure(closure) => Ok(Some(closure.object)),
            Variant::Null => Ok(None),
            other => Err(TjsError::runtime(format!("{other} is not an object"))),
        }
    }

    pub(super) fn change_this(
        &self,
        value: &mut Variant,
        this_obj: Option<ObjectHandle>,
    ) -> Result<()> {
        match self.materialize_code_object(value.clone()) {
            Variant::Closure(mut closure) => {
                closure.this_obj = this_obj;
                *value = Variant::Closure(closure);
                Ok(())
            }
            Variant::Object(object) => {
                *value = this_obj
                    .map(|this_obj| Variant::Closure(Closure::new(object, Some(this_obj))))
                    .unwrap_or(Variant::Object(object));
                Ok(())
            }
            other => Err(TjsError::runtime(format!("{other} is not a closure"))),
        }
    }

    pub(super) fn eval_operator(
        &mut self,
        frame: &mut Frame,
        reg: i16,
        result_needed: bool,
    ) -> Result<()> {
        let source = frame.get(reg)?.to_tjs_string()?;
        if source.is_empty() {
            if result_needed {
                frame.set(reg, Variant::Void)?;
            }
            return Ok(());
        }

        let source = if result_needed {
            format!("return {source};")
        } else {
            format!("{source};")
        };
        let file = compile_source_to_bytecode("<eval>", &source)?;
        let top_level = file
            .top_level
            .ok_or_else(|| TjsError::runtime("eval bytecode has no top-level object"))?;
        let file_id = self.runtime.install_script_file(Arc::new(file));
        let value =
            self.execute_file_object_with_this(file_id, top_level, Vec::new(), frame.this_obj)?;
        if result_needed {
            frame.set(reg, value)?;
        }
        Ok(())
    }

    pub(super) fn instance_of(&mut self, value: &Variant, class_name: &str) -> Result<bool> {
        let Ok(handle) = self.resolve_object(value.clone()) else {
            return Ok(false);
        };
        if class_name == "Function" && self.handle_is_function(handle) {
            return Ok(true);
        }
        if class_name == "Property" && self.handle_is_property(handle) {
            return Ok(true);
        }
        let mut seen = Vec::new();
        let mut current = Some(handle);
        while let Some(handle) = current {
            if seen.contains(&handle.0) {
                return Ok(false);
            }
            seen.push(handle.0);
            if self.runtime.heap[handle.0]
                .class_infos
                .iter()
                .any(|info| info == class_name)
            {
                return Ok(true);
            }
            current = self.super_class_handle(handle)?;
        }
        Ok(false)
    }

    fn handle_is_function(&self, handle: ObjectHandle) -> bool {
        matches!(
            self.runtime.heap[handle.0].kind,
            ObjectKind::NativeFunction { .. }
                | ObjectKind::VmNativeFunction { .. }
                | ObjectKind::InterCode {
                    context: BytecodeContextType::Function | BytecodeContextType::ExprFunction,
                    ..
                }
        )
    }

    fn handle_is_property(&self, handle: ObjectHandle) -> bool {
        matches!(
            self.runtime.heap[handle.0].kind,
            ObjectKind::NativeProperty { .. }
                | ObjectKind::InterCode {
                    context: BytecodeContextType::Property
                        | BytecodeContextType::PropertyGetter
                        | BytecodeContextType::PropertySetter,
                    ..
                }
        )
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

    pub(super) fn register_object_members(&mut self, source: ObjectHandle, dest: ObjectHandle) {
        let members = self.runtime.heap[source.0].members.clone();
        for (name, value) in members {
            let mut value = self.materialize_code_object(value);
            match &mut value {
                Variant::Closure(closure) => closure.this_obj = Some(dest),
                Variant::Object(handle) => {
                    value = Variant::Closure(Closure::new(*handle, Some(dest)));
                }
                _ => {}
            }
            self.runtime.heap[dest.0].set(name, value);
        }
    }
}

fn string_index_of(value: &str, needle: &str, start_utf16: usize) -> i64 {
    let start = byte_index_for_utf16(value, start_utf16);
    value[start..]
        .find(needle)
        .map(|offset| utf16_len(&value[..start + offset]) as i64)
        .unwrap_or(-1)
}

fn string_last_index_of(value: &str, needle: &str, end_utf16: Option<usize>) -> i64 {
    let end = end_utf16
        .map(|end| byte_index_for_utf16(value, end))
        .unwrap_or(value.len());
    value[..end]
        .rfind(needle)
        .map(|offset| utf16_len(&value[..offset]) as i64)
        .unwrap_or(-1)
}

fn string_to_ascii_lowercase(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_uppercase() {
                ch.to_ascii_lowercase()
            } else {
                ch
            }
        })
        .collect()
}

fn string_to_ascii_uppercase(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_lowercase() {
                ch.to_ascii_uppercase()
            } else {
                ch
            }
        })
        .collect()
}

fn trim_tjs_ascii_space(value: &str) -> &str {
    value.trim_matches(|ch: char| ch > '\0' && ch <= ' ')
}

fn sprintf_tjs_string(format: &str, args: &[Variant]) -> Result<String> {
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    let mut arg_index = 0usize;

    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }

        let spec = PrintfSpec::parse(&mut chars, args, &mut arg_index)?;
        let formatted = format_printf_arg(&spec, args.get(arg_index))?;
        arg_index = arg_index.saturating_add(1);
        out.push_str(&formatted);
    }

    Ok(out)
}

#[derive(Debug, Default)]
struct PrintfSpec {
    left_adjust: bool,
    force_sign: bool,
    space_sign: bool,
    alternate: bool,
    zero_pad: bool,
    width: Option<usize>,
    precision: Option<usize>,
    conv: char,
}

impl PrintfSpec {
    fn parse<I>(
        chars: &mut std::iter::Peekable<I>,
        args: &[Variant],
        arg_index: &mut usize,
    ) -> Result<Self>
    where
        I: Iterator<Item = char>,
    {
        let mut spec = Self::default();
        loop {
            match chars.peek().copied() {
                Some('-') => spec.left_adjust = true,
                Some('+') => spec.force_sign = true,
                Some(' ') => spec.space_sign = true,
                Some('#') => spec.alternate = true,
                Some('0') => spec.zero_pad = true,
                _ => break,
            }
            chars.next();
        }

        spec.width = parse_printf_width(chars, args, arg_index)?;
        if chars.peek() == Some(&'.') {
            chars.next();
            spec.precision = Some(parse_printf_precision(chars, args, arg_index)?);
        }

        while matches!(chars.peek(), Some('h' | 'l' | 'L' | 'I' | 'z' | 't' | 'j')) {
            let modifier = chars.next();
            if modifier == Some('l') && chars.peek() == Some(&'l') {
                chars.next();
            }
            if modifier == Some('h') && chars.peek() == Some(&'h') {
                chars.next();
            }
            if modifier == Some('I') {
                while matches!(chars.peek(), Some('3' | '6' | '2' | '4' | '8')) {
                    chars.next();
                }
            }
        }

        spec.conv = chars
            .next()
            .ok_or_else(|| TjsError::runtime("incomplete sprintf format"))?;
        Ok(spec)
    }
}

fn parse_printf_width<I>(
    chars: &mut std::iter::Peekable<I>,
    args: &[Variant],
    arg_index: &mut usize,
) -> Result<Option<usize>>
where
    I: Iterator<Item = char>,
{
    if chars.peek() == Some(&'*') {
        chars.next();
        let width = args
            .get(*arg_index)
            .map(Variant::to_integer)
            .transpose()?
            .unwrap_or(0);
        *arg_index = (*arg_index).saturating_add(1);
        return Ok((width > 0).then_some(width as usize));
    }

    let mut width = 0usize;
    let mut has_width = false;
    while let Some(digit) = chars.peek().and_then(|ch| ch.to_digit(10)) {
        chars.next();
        has_width = true;
        width = width.saturating_mul(10).saturating_add(digit as usize);
    }
    Ok(has_width.then_some(width))
}

fn parse_printf_precision<I>(
    chars: &mut std::iter::Peekable<I>,
    args: &[Variant],
    arg_index: &mut usize,
) -> Result<usize>
where
    I: Iterator<Item = char>,
{
    if chars.peek() == Some(&'*') {
        chars.next();
        let precision = args
            .get(*arg_index)
            .map(Variant::to_integer)
            .transpose()?
            .unwrap_or(0)
            .max(0) as usize;
        *arg_index = (*arg_index).saturating_add(1);
        return Ok(precision);
    }

    let mut precision = 0usize;
    while let Some(digit) = chars.peek().and_then(|ch| ch.to_digit(10)) {
        chars.next();
        precision = precision.saturating_mul(10).saturating_add(digit as usize);
    }
    Ok(precision)
}

fn format_printf_arg(spec: &PrintfSpec, arg: Option<&Variant>) -> Result<String> {
    let value = arg.cloned().unwrap_or_default();
    match spec.conv {
        'd' | 'i' => Ok(format_printf_signed(spec, value.to_integer()?)),
        'u' => Ok(format_printf_unsigned(
            spec,
            value.to_integer()? as u64,
            10,
            false,
        )),
        'o' => Ok(format_printf_unsigned(
            spec,
            value.to_integer()? as u64,
            8,
            false,
        )),
        'x' => Ok(format_printf_unsigned(
            spec,
            value.to_integer()? as u64,
            16,
            false,
        )),
        'X' => Ok(format_printf_unsigned(
            spec,
            value.to_integer()? as u64,
            16,
            true,
        )),
        'f' | 'F' | 'e' | 'E' | 'g' | 'G' => Ok(format_printf_real(spec, value.to_real()?)),
        's' => {
            let text = value.to_tjs_string()?;
            Ok(pad_printf_text(
                limit_chars(&text, spec.precision),
                spec.width,
                spec.left_adjust,
            ))
        }
        'c' => format_printf_char(spec, &value),
        '%' => Ok("%".to_string()),
        conv => Err(TjsError::runtime(format!(
            "unsupported sprintf conversion `%{conv}`"
        ))),
    }
}

fn format_printf_signed(spec: &PrintfSpec, value: i64) -> String {
    let sign = if value < 0 {
        "-"
    } else if spec.force_sign {
        "+"
    } else if spec.space_sign {
        " "
    } else {
        ""
    };
    let magnitude = value.unsigned_abs();
    let digits = printf_digits(magnitude, 10, false);
    finish_printf_number(spec, sign, "", digits, magnitude == 0)
}

fn format_printf_unsigned(spec: &PrintfSpec, value: u64, radix: u32, uppercase: bool) -> String {
    let prefix = if spec.alternate && value != 0 {
        match radix {
            8 => "0",
            16 if uppercase => "0X",
            16 => "0x",
            _ => "",
        }
    } else {
        ""
    };
    let digits = printf_digits(value, radix, uppercase);
    finish_printf_number(spec, "", prefix, digits, value == 0)
}

fn printf_digits(value: u64, radix: u32, uppercase: bool) -> String {
    match (radix, uppercase) {
        (8, _) => format!("{value:o}"),
        (10, _) => value.to_string(),
        (16, false) => format!("{value:x}"),
        (16, true) => format!("{value:X}"),
        _ => unreachable!("supported printf radix"),
    }
}

fn finish_printf_number(
    spec: &PrintfSpec,
    sign: &str,
    prefix: &str,
    mut digits: String,
    is_zero: bool,
) -> String {
    if spec.precision == Some(0) && is_zero {
        digits.clear();
    }
    if let Some(precision) = spec.precision
        && digits.len() < precision
    {
        digits = "0".repeat(precision - digits.len()) + &digits;
    }

    let head = format!("{sign}{prefix}");
    let len = head.chars().count() + digits.chars().count();
    let width = spec.width.unwrap_or(0);
    if len >= width {
        return head + &digits;
    }

    let pad_len = width - len;
    if spec.left_adjust {
        return head + &digits + &" ".repeat(pad_len);
    }
    if spec.zero_pad && spec.precision.is_none() {
        return head + &"0".repeat(pad_len) + &digits;
    }
    " ".repeat(pad_len) + &head + &digits
}

fn format_printf_real(spec: &PrintfSpec, value: f64) -> String {
    let precision = spec.precision.unwrap_or(6);
    let sign = if value.is_sign_negative() {
        "-"
    } else if spec.force_sign {
        "+"
    } else if spec.space_sign {
        " "
    } else {
        ""
    };
    let value = value.abs();
    let mut digits = match spec.conv {
        'f' | 'F' => format!("{value:.precision$}"),
        'e' => format!("{value:.precision$e}"),
        'E' => format!("{value:.precision$E}"),
        'g' => printf_general(value, precision, false, spec.alternate),
        'G' => printf_general(value, precision, true, spec.alternate),
        _ => unreachable!("real conversion"),
    };
    if spec.alternate && !digits.contains('.') && !digits.contains('e') && !digits.contains('E') {
        digits.push('.');
    }
    finish_printf_real(spec, sign, &digits)
}

fn printf_general(value: f64, precision: usize, uppercase: bool, alternate: bool) -> String {
    let significant = precision.max(1);
    let abs = value.abs();
    let use_exp = abs != 0.0
        && (abs < 0.0001 || abs >= 10_f64.powi(i32::try_from(significant).unwrap_or(i32::MAX)));
    let mut text = if use_exp {
        let precision = significant.saturating_sub(1);
        if uppercase {
            format!("{value:.precision$E}")
        } else {
            format!("{value:.precision$e}")
        }
    } else {
        let integer_digits = if abs >= 1.0 {
            abs.log10().floor() as usize + 1
        } else {
            1
        };
        let precision = significant.saturating_sub(integer_digits);
        format!("{value:.precision$}")
    };
    if !alternate {
        strip_printf_float_zeros(&mut text);
    }
    text
}

fn strip_printf_float_zeros(text: &mut String) {
    let exponent = text
        .find('e')
        .or_else(|| text.find('E'))
        .map(|index| text.split_off(index));
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if let Some(exponent) = exponent {
        text.push_str(&exponent);
    }
}

fn finish_printf_real(spec: &PrintfSpec, sign: &str, digits: &str) -> String {
    let len = sign.chars().count() + digits.chars().count();
    let width = spec.width.unwrap_or(0);
    if len >= width {
        return format!("{sign}{digits}");
    }

    let pad_len = width - len;
    if spec.left_adjust {
        return format!("{sign}{digits}{}", " ".repeat(pad_len));
    }
    if spec.zero_pad {
        return format!("{sign}{}{}", "0".repeat(pad_len), digits);
    }
    format!("{}{}{}", " ".repeat(pad_len), sign, digits)
}

fn format_printf_char(spec: &PrintfSpec, value: &Variant) -> Result<String> {
    let text = match value {
        Variant::String(value) => value.chars().next().unwrap_or('\0').to_string(),
        _ => {
            let code = value.to_integer()? as u32;
            char::from_u32(code).unwrap_or('\0').to_string()
        }
    };
    Ok(pad_printf_text(text, spec.width, spec.left_adjust))
}

fn limit_chars(text: &str, limit: Option<usize>) -> String {
    match limit {
        Some(limit) => text.chars().take(limit).collect(),
        None => text.to_string(),
    }
}

fn pad_printf_text(text: String, width: Option<usize>, left_adjust: bool) -> String {
    let width = width.unwrap_or(0);
    let len = text.chars().count();
    if len >= width {
        return text;
    }
    let pad = " ".repeat(width - len);
    if left_adjust {
        text + &pad
    } else {
        pad + &text
    }
}

fn byte_index_for_utf16(value: &str, target: usize) -> usize {
    let mut units = 0usize;
    for (byte, ch) in value.char_indices() {
        if units >= target {
            return byte;
        }
        units += ch.len_utf16();
    }
    value.len()
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn escape_tjs_string_fragment(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\0' => escaped.push_str("\\0"),
            ch if ch.is_control() => {
                escaped.push_str(&format!("\\x{:02x}", ch as u32));
            }
            ch => escaped.push(ch),
        }
    }
    escaped
}
