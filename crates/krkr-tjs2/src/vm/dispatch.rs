use std::borrow::Cow;
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
    /// Host-side member read that mirrors the C++ side of KRKR: a missing
    /// member is `void`, never the script-facing `Member "%1" does not exist`
    /// error.
    pub(crate) fn get_object_member_probe(
        &mut self,
        object: ObjectHandle,
        name: &str,
    ) -> Result<Variant> {
        self.prop_get(
            Variant::Object(object),
            name,
            DispatchFlags::probe(),
            Some(object),
        )
    }

    pub(super) fn resolve_object(&self, value: Variant) -> Result<ObjectHandle> {
        match self.materialize_code_object(value) {
            Variant::Object(handle) => Ok(handle),
            Variant::Closure(closure) => Ok(closure.object),
            Variant::Null => Err(TjsError::null_access()),
            other => Err(TjsError::variant_convert_to_object(&other)),
        }
    }

    pub(super) fn closure_parts(
        &self,
        value: Variant,
    ) -> Result<(ObjectHandle, Option<ObjectHandle>)> {
        match self.materialize_code_object(value) {
            Variant::Object(handle) => Ok((handle, None)),
            Variant::Closure(closure) => Ok((closure.object, closure.this_obj)),
            Variant::Null => Err(TjsError::null_access()),
            other => Err(TjsError::variant_convert_to_object(&other)),
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
        // A receiver that is not an object fails the member read up front:
        // `GetPropertyDirect` converts the object expression with
        // `AsObjectClosureNoAddRef` before it looks at the name
        // (`tjsInterCodeExec.cpp:1593-1615`), and that conversion raises
        // `TJSThrowVariantConvertError` for a void value
        // (`tjsVariant.h:722-731`, `tjsVariant.cpp:142-152`).  Only a string
        // or octet receiver has a property reader of its own.
        match &target {
            Variant::String(value) => return self.string_property(value, MemberKind::Name(name)),
            Variant::Octet(value) => return self.octet_property(value, MemberKind::Name(name)),
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
                // `tTJSObjectProxy::PropGet` (`tjsInterCodeExec.cpp:284`) tries
                // the first object and moves to the second -- for a frame's
                // `%-2`, the global object -- only for TJS_E_MEMBERNOTFOUND. A
                // member that exists and holds void is a hit, so `probe` is
                // cleared here: with it the primary would report "absent" for
                // a void member and the global object's value would win.
                match self.prop_get_handle(
                    primary,
                    name,
                    flags.without_probe(),
                    bind_this.or(caller_this),
                ) {
                    Ok(value) => return Ok(self.bind_proxy_value(value, bind_this)),
                    Err(error) if error.is_member_not_found() => {}
                    Err(error) => return Err(error),
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

        let mut member = self.runtime.heap[handle.0].get_raw(name);
        if flags.must_exist && self.runtime.heap[handle.0].array_index_missing(name) {
            // `ARRAY_GET_VAL` (`tjsArray.cpp:1407`) reports an out-of-range
            // index to a `TJS_MEMBERMUSTEXIST` reader -- `typeof arr[9]` -- as
            // member-not-found, which the TYPEOFD opcode renders "undefined".
            member = None;
        }
        let Some(value) = member else {
            let bind_this = self.bound_super_this(handle, caller_this)?;
            if let Some(this_obj) = bind_this
                && self.handle_class_name_matches(handle, name)
            {
                return Ok(Variant::Closure(Closure::new(handle, Some(this_obj))));
            }
            if let Some(class_handle) = self.super_class_handle(handle)? {
                let receiver = self.inherited_member_this(handle, caller_this);
                // `tTJSInterCodeContext::PropGet` (`tjsInterCodeExec.cpp:3144`)
                // walks on only for TJS_E_MEMBERNOTFOUND, so a super class
                // member that exists with a void value is the answer.
                match self.prop_get_handle(
                    class_handle,
                    name,
                    flags.without_probe(),
                    receiver,
                ) {
                    Ok(value) => return Ok(self.bind_proxy_value(value, receiver)),
                    Err(error) if error.is_member_not_found() => {}
                    Err(error) => return Err(error),
                }
            }
            // tTJSInterCodeContext::PropGet on a class object consults every
            // superclass getter entry, not only the primary parent followed
            // above: `class C extends A, B` reads `C.fromB` through B.
            if self.has_secondary_extenders(handle)?
                && let Some(owner) = self.class_member_owner(handle, name, true)?
                && owner != handle
            {
                let receiver = self.inherited_member_this(handle, caller_this);
                match self.prop_get_handle(owner, name, flags.without_probe(), receiver) {
                    Ok(value) => return Ok(self.bind_proxy_value(value, receiver)),
                    Err(error) if error.is_member_not_found() => {}
                    Err(error) => return Err(error),
                }
            }
            // TYPEOFD/TYPEOFI pass MEMBERMUSTEXIST in KRKR2/Z.  A qualified
            // lookup such as `typeof Base.finalize` must stop at the class
            // chain and report undefined; falling back to the current
            // instance would mistake a child's override for Base's member.
            if let Some(this_obj) = bind_this
                && !flags.must_exist
                && !flags.no_bound_instance_fallback
                && let Some(value) = self.runtime.heap[this_obj.0].get_raw(name)
                && !self.runtime.variant_is_property(&value)
            {
                return Ok(value);
            }
            if let Some(value) = self.call_get_missing(handle, name)? {
                return Ok(value);
            }
            return self.missing_member(handle, name, flags);
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
        //
        // Native *constructors* are the exception: `RegisterNCM` stores the
        // class's own constructor with `val = dsp` and no ObjThis
        // (tjsNative.cpp:246, `TJS_END_NATIVE_CONSTRUCTOR_DECL`), and
        // instances receive their copy already bound when the native class is
        // initialized on them (`tTJSVariant(val, objthis)`, tjsNative.cpp:295
        // -> `install_class_name_constructor`).  Binding one here would make
        // `Layer.Layer(win, this)` -- read through `global.Layer` -- run on
        // whichever object the class was looked up from instead of on the
        // caller's `this`, which is what VM_CALLD resolves
        // (tjsInterCodeExec.cpp:2015).
        if bind_this.is_none()
            && let Variant::Object(native) = &value
            && matches!(
                self.runtime.heap[native.0].kind,
                ObjectKind::NativeFunction {
                    constructable: false,
                    ..
                } | ObjectKind::VmNativeFunction { .. }
            )
        {
            return Ok(self.bind_proxy_value(value, Some(handle)));
        }
        Ok(self.bind_proxy_value(value, bind_this))
    }

    /// Read a member whose operand is the raw value of a VM register or data
    /// area (`GET_STR_ID`/`GET_STR_IND`) instead of a name.
    ///
    /// The reference hands that value to `GetStringProperty` /
    /// `GetOctetProperty` unchanged (`tjsInterCodeExec.cpp:1682-1730`), so
    /// only this entry point can reach the reference's numeric-index branch;
    /// every other receiver keeps the name-based path, where the value is
    /// converted to a name the way `PropGet` does.
    pub(super) fn prop_get_member(
        &mut self,
        target: Variant,
        member: &Variant,
        flags: DispatchFlags,
        caller_this: Option<ObjectHandle>,
    ) -> Result<Variant> {
        if let Variant::String(value) = &target {
            let kind = member_kind(member)?;
            return self.string_property(value, kind);
        }
        if let Variant::Octet(value) = &target {
            let kind = member_kind(member)?;
            return self.octet_property(value, kind);
        }
        let name = self.key_from_variant(member)?;
        self.prop_get(target, &name, flags, caller_this)
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
        // `tTJSInterCodeContext::SetPropertyDirect` reaches `SetStringProperty`
        // / `SetOctetProperty` directly for a string/octet receiver
        // (`tjsInterCodeExec.cpp:1624-1656`), before any object conversion,
        // and no string or octet member is writable. The member-access
        // context is attached here because this arm answers the error itself
        // instead of running through `closure_parts` below.
        let string_set = match &target {
            Variant::String(receiver) => {
                Some(self.set_string_property(receiver, MemberKind::Name(name)))
            }
            Variant::Octet(receiver) => {
                Some(self.set_octet_property(receiver, MemberKind::Name(name)))
            }
            _ => None,
        };
        if let Some(result) = string_set {
            return result.map_err(|error| {
                error.with_member_access(TjsMemberAccess {
                    operation: TjsMemberOperation::Setting,
                    receiver_type,
                    member_name: name.to_string(),
                    callee_type: None,
                })
            });
        }
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

    /// Write a member whose operand is the raw value of a VM register or data
    /// area (`SET_STR_ID`/`SET_STR_IND`), the write-side counterpart of
    /// [`Vm::prop_get_member`].
    pub(super) fn prop_set_member(
        &mut self,
        target: Variant,
        member: &Variant,
        value: Variant,
        flags: DispatchFlags,
        caller_this: Option<ObjectHandle>,
    ) -> Result<()> {
        let member_set = match &target {
            Variant::String(receiver) => Some(
                member_kind(member).and_then(|kind| self.set_string_property(receiver, kind)),
            ),
            Variant::Octet(receiver) => Some(
                member_kind(member).and_then(|kind| self.set_octet_property(receiver, kind)),
            ),
            _ => None,
        };
        if let Some(result) = member_set {
            let receiver_type = self.value_debug_type(&target);
            return result.map_err(|error| {
                error.with_member_access(TjsMemberAccess {
                    operation: TjsMemberOperation::Setting,
                    receiver_type,
                    member_name: self.member_diagnostic_name(member),
                    callee_type: None,
                })
            });
        }
        let name = self.key_from_variant(member)?;
        self.prop_set(target, &name, value, flags, caller_this)
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

        // `tTJSNativeClass::FuncCall` copies every non-static member of the
        // native class onto the object it initializes (`tjsNative.cpp:293`),
        // and a class body registers its own members on the instance as well
        // (`tTJSInterCodeContext::CreateNew`, `tjsInterCodeExec.cpp:3211`), so
        // in KRKR a class-provided member *is* an own member: `Find` sees it
        // and `missing` is never consulted for it (`tTJSCustomObject::PropSet`,
        // `tjsObject.cpp:1475`).  Kirakira installs only part of that copy --
        // `install_methods` skips whatever the class chain already supplies --
        // so a chain-provided name has to count as present here too, or a
        // script write routes through `missing` into the script parent
        // (KAGEX's `ParentHackLayer` forwards event-handler writes that way and
        // then never receives the events as itself).
        // Class objects keep their own rule: a write to an inherited member
        // updates the class that owns it (see below), so only instances take
        // the chain into account here.
        let member_exists = self.runtime.heap[handle.0].get_raw(name).is_some()
            || (!self.is_bytecode_class(handle)
                && self.class_chain_provides_member(handle, name)?);
        if let Some(existing) = self.runtime.heap[handle.0].get_raw(name)
            && (!flags.ignore_prop || self.runtime.variant_is_native_property(&existing))
            && self
                .property_setter(existing, value.clone(), caller_this)?
                .is_some()
        {
            return Ok(());
        }
        if !flags.ignore_prop {
            let receiver = self.inherited_member_this(handle, caller_this);
            let mut current = self.super_class_handle(handle)?;
            while let Some(class_handle) = current {
                if let Some(existing) = self.runtime.heap[class_handle.0].get_raw(name)
                    && self
                        .property_setter(existing, value.clone(), receiver)?
                        .is_some()
                {
                    return Ok(());
                }
                current = self.super_class_handle(class_handle)?;
            }
        }
        // tTJSInterCodeContext::PropSet on a class object: the write is
        // first attempted without MEMBERENSURE on the class and then along
        // its superclass proxies, so an inherited member is updated on the
        // class that owns it (`Derived.count = 1` writes `Base.count`); only
        // a name nobody in the chain has is ensured on the class itself.
        // IGNOREPROP|MEMBERENSURE (regmember-style stores) skips the chain.
        let regmember_store = flags.ignore_prop && flags.ensure;
        if !member_exists
            && !regmember_store
            && self.is_bytecode_class(handle)
            && let Some(owner) = self.class_chain_owner(handle, name)?
            && owner != handle
        {
            return self.prop_set_handle(owner, name, value, flags, caller_this);
        }
        // Class field initializers run before the mixin constructors finish.
        // A preceding `__missing` module may already have enabled missing
        // dispatch on the shared instance, but declared fields must still be
        // installed locally instead of being routed to that hook.
        if !member_exists
            && !self.is_class_initializing(handle)
            && self.call_set_missing(handle, name, value.clone())?
        {
            return Ok(());
        }
        if let Some(this_obj) = self.bound_super_this(handle, caller_this)? {
            self.set_bound_member(this_obj, name, value);
            return Ok(());
        }
        if self
            .runtime
            .heap[handle.0]
            .array_negative_index_after_wrap(name)
        {
            // `tTJSArrayObject::PropSetByNum` (`tjsArray.cpp:1634`) fails such
            // a write instead of creating a member named `-n`, and it reports
            // `TJS_E_MEMBERNOTFOUND`: the failure is a miss, not a storage
            // error, so the kind is the one the class-chain walks branch on.
            return Err(TjsError::member_not_found(name));
        }
        let value = self.materialize_code_object(value);
        self.runtime.heap[handle.0].set(name, value);
        Ok(())
    }

    /// A read that fell through every lookup.  Official
    /// `tTJSCustomObject::PropGet` reports `TJS_E_MEMBERNOTFOUND`, which
    /// `TJSThrowFrom_tjs_error` turns into the script error `Member "%1" does
    /// not exist` (`tjsError.cpp:240-244`); only a TJS `Dictionary` maps a
    /// missing member back to void (`tTJSDictionaryObject::PropGet`,
    /// `tjsDictionary.cpp:721`).  Host-side probes and the dispatcher's own
    /// chain walks raise `flags.probe` and get void, mirroring the C++ side of
    /// KRKR, which treats `TJS_E_MEMBERNOTFOUND` as "absent".
    fn missing_member(
        &mut self,
        handle: ObjectHandle,
        name: &str,
        flags: DispatchFlags,
    ) -> Result<Variant> {
        if flags.probe {
            return Ok(Variant::Void);
        }
        let is_dictionary = self.runtime.heap[handle.0]
            .class_infos
            .iter()
            .any(|class| class == "Dictionary");
        if is_dictionary && !flags.must_exist {
            return Ok(Variant::Void);
        }
        Err(TjsError::member_not_found(name))
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
                DispatchFlags::no_bound_instance_fallback().with_probe(),
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
            // A script `property` with no getter denies reads:
            // `tTJSInterCodeContext::PropGet` answers `TJS_E_ACCESSDENYED`
            // when `PropGetter` is null (`tjsInterCodeExec.cpp:3138`).
            return Err(TjsError::access_denied());
        }
        if let ObjectKind::NativeProperty { id, access } = kind {
            if !access.allows_get() {
                // An official read-only or write-only property answers
                // `TJS_E_ACCESSDENYED` without running the accessor
                // (`tjsInterCodeExec.cpp:3138`).
                return Err(TjsError::access_denied());
            }
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
            // `tTJSInterCodeContext::PropSet` answers `TJS_E_ACCESSDENYED`
            // when `PropSetter` is null (`tjsInterCodeExec.cpp:3172`).
            return Err(TjsError::access_denied());
        }
        if let ObjectKind::NativeProperty { id, access } = kind {
            if !access.allows_set() {
                return Err(TjsError::access_denied());
            }
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
                // A script `property` with no getter answers
                // `TJS_E_ACCESSDENYED` through the default-property protocol
                // (`TJSDefaultPropGet` forwards everything but
                // NOTIMPL/INVALIDTYPE/INVALIDOBJECT, `tjsObject.cpp:1376-1379`),
                // which is what [`Vm::default_prop_get`] implements for the
                // `*property` operator.
                //
                // This member-read path deliberately keeps handing the
                // property object back instead, and that is a known deviation
                // from the reference (`tTJSInterCodeContext::PropGet` answers
                // TJS_E_ACCESSDENYED when `PropGetter` is null,
                // `tjsInterCodeExec.cpp:3134-3140`, and `TJSDefaultPropGet`
                // propagates it): the engine's KAGEX support reads a freshly
                // defined setter-only property to get that object
                // (`objectHookInjection`'s `var t1 = this.prop;
                //  ("property prop {...}")!; l0[l5] = t1 incontextof l4`),
                // pinned by `krkr-engine`'s
                // `layer_font_reads_back_bound_to_the_font_like_krkr`
                // (`engine.rs:13297`), which fails if -1007 is raised here.
                //
                // A follow-up engine mission owns the decision: the
                // reference's sanctioned way to read the property object
                // itself is `&obj.prop` (TJS_IGNOREPROP), which the engine
                // already implements.  If real KAGEX uses that form, the
                // engine test can be fixed and this path changed to
                // `Err(TjsError::access_denied())` like `default_prop_get`
                // above; until then the leniency stays, filed as a finding.
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
            ObjectKind::NativeProperty { id, access } => {
                if !access.allows_get() {
                    return Err(TjsError::access_denied());
                }
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
                // Same default-property protocol on the write side: a
                // `property` with no setter denies the write
                // (`TJSDefaultPropSet`, `tjsObject.cpp:1435-1438`,
                // `tjsInterCodeExec.cpp:3172`) instead of letting the write
                // overwrite the property object with a plain value.
                let Some(setter) = file.objects[object_index].prop_setter else {
                    return Err(TjsError::access_denied());
                };
                let effective_this = self.effective_member_this(closure_this, caller_this)?;
                self.execute_file_object_with_this(file_id, setter, vec![value], effective_this)?;
                Ok(Some(()))
            }
            ObjectKind::NativeProperty { id, access } => {
                if !access.allows_set() {
                    return Err(TjsError::access_denied());
                }
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

    /// `GetStringProperty` (`tjsInterCodeExec.cpp:46-100`).
    ///
    /// Only `length` names a member; every other name routes through the
    /// index rule: a name whose *first* character is a digit is parsed
    /// `TJS_atoi`-style and reads the UTF-16 code unit at that index
    /// (`:67-81`), and anything else is `TJS_E_MEMBERNOTFOUND`. A numeric
    /// member never looks at names at all (`:85-98`). The reference has no
    /// `count` member for strings -- `GetStringProperty` compares against
    /// `length` alone -- so `count` is a miss here too.
    fn string_property(&self, value: &str, member: MemberKind<'_>) -> Result<Variant> {
        match member {
            MemberKind::Name(name) => {
                if name == "length" {
                    return Ok(Variant::Integer(utf16_len(value) as i64));
                }
                if !is_index_name(name) {
                    return Err(TjsError::member_not_found(name));
                }
                self.string_index(value, tjs_atoi(name))
            }
            MemberKind::Index(index) => self.string_index(value, index),
        }
    }

    /// The index read behind both `GetStringProperty` branches: the reference
    /// treats an index equal to the length as an empty result, not as an
    /// error (`tjsInterCodeExec.cpp:73`), and anything outside that as
    /// `TJSRangeError`.
    fn string_index(&self, value: &str, index: i32) -> Result<Variant> {
        let len = utf16_len(value) as i64;
        let index = i64::from(index);
        if index == len {
            return Ok(Variant::String(String::new()));
        }
        if index < 0 || index > len {
            return Err(TjsError::range_error());
        }
        let unit = value.encode_utf16().nth(index as usize).unwrap_or(0);
        // Known deviation: `String` is UTF-8, so a code unit that is half of
        // an astral pair becomes U+FFFD here where the reference returns the
        // lone surrogate (`tTJSVariantString` holds UTF-16). The length and
        // the index arithmetic above still count code units, so only the
        // content of an astral half differs.
        Ok(Variant::String(String::from_utf16_lossy(&[unit])))
    }

    /// `GetOctetProperty` (`tjsInterCodeExec.cpp:128-170`).
    ///
    /// `length` is the byte count and an index reads one byte as an integer;
    /// every index outside the octet is `TJSRangeError`, and a name that is
    /// neither `length` nor an index is a miss. Unlike the string form, an
    /// index equal to the length *is* out of range here (`:152`).
    fn octet_property(&self, value: &[u8], member: MemberKind<'_>) -> Result<Variant> {
        match member {
            MemberKind::Name(name) => {
                if name == "length" {
                    return Ok(Variant::Integer(value.len() as i64));
                }
                if !is_index_name(name) {
                    return Err(TjsError::member_not_found(name));
                }
                self.octet_index(value, tjs_atoi(name))
            }
            MemberKind::Index(index) => self.octet_index(value, index),
        }
    }

    /// The index read behind both `GetOctetProperty` branches
    /// (`tjsInterCodeExec.cpp:150-155`, `:162-168`).
    fn octet_index(&self, value: &[u8], index: i32) -> Result<Variant> {
        if index < 0 || i64::from(index) >= value.len() as i64 {
            return Err(TjsError::range_error());
        }
        Ok(Variant::Integer(i64::from(value[index as usize])))
    }

    /// `SetStringProperty` (`tjsInterCodeExec.cpp:102-126`).
    ///
    /// No string member accepts a write: `length` and every index-shaped name
    /// report `TJS_E_ACCESSDENYED`, every numeric member does (`:122-125`),
    /// and any other name is a miss. The value being stored never matters --
    /// the reference throws before reading `param`.
    fn set_string_property(&self, _value: &str, member: MemberKind<'_>) -> Result<()> {
        match member {
            MemberKind::Name(name) if name == "length" || is_index_name(name) => {
                Err(TjsError::access_denied())
            }
            MemberKind::Name(name) => Err(TjsError::member_not_found(name)),
            MemberKind::Index(_) => Err(TjsError::access_denied()),
        }
    }

    /// `SetOctetProperty` (`tjsInterCodeExec.cpp:172-196`), the string form's
    /// rule over bytes.
    fn set_octet_property(&self, _value: &[u8], member: MemberKind<'_>) -> Result<()> {
        match member {
            MemberKind::Name(name) if name == "length" || is_index_name(name) => {
                Err(TjsError::access_denied())
            }
            MemberKind::Name(name) => Err(TjsError::member_not_found(name)),
            MemberKind::Index(_) => Err(TjsError::access_denied()),
        }
    }

    /// The member name a dispatch diagnostic reports for a member operand
    /// that arrived as a value: its name when it has one, else the rendering
    /// `key_from_variant` would have produced, else the value's debug type
    /// (an octet has no string conversion at all).
    fn member_diagnostic_name(&self, member: &Variant) -> String {
        self.key_from_variant(member)
            .unwrap_or_else(|_| self.value_debug_type(member))
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
                self.operate_property(
                    object_value,
                    OpMember::Name(&name),
                    None,
                    frame.this_obj,
                    |value, _| {
                        if inc {
                            value.increment()
                        } else {
                            value.decrement()
                        }
                    },
                )?
            }
            20 | 24 => {
                let object_value = frame.get(inst.operands[1])?;
                let member = frame.get(inst.operands[2])?;
                self.operate_property(
                    object_value,
                    OpMember::Value(&member),
                    None,
                    frame.this_obj,
                    |value, _| {
                        if inc {
                            value.increment()
                        } else {
                            value.decrement()
                        }
                    },
                )?
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
                    OpMember::Name(&name),
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
                let member = frame.get(inst.operands[2])?;
                let rhs = frame.get(inst.operands[3])?;
                let family = binary_family(inst.opcode);
                let value = self.operate_property(
                    object_value,
                    OpMember::Value(&member),
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

    /// The op protocol behind compound assignment and inc/dec on a member
    /// (`OperatePropertyDirect`/`OperatePropertyIndirect`/
    /// `OperatePropertyDirect0`/`OperatePropertyIndirect0`,
    /// `tjsInterCodeExec.cpp:1811-1985`).
    ///
    /// Every one of those functions takes the receiver's closure *before* the
    /// member (`:1816`, `:1842`, `:1925`, `:1950` call `AsObjectClosure`,
    /// which throws for anything that is not an object, `tjsVariant.h:710-719`),
    /// so a string or octet receiver fails with the conversion error here and
    /// its member is never read or written -- unlike a plain `"abc"[0] = x`
    /// store, which reaches `SetStringProperty`.
    fn operate_property(
        &mut self,
        object_value: Variant,
        member: OpMember<'_>,
        rhs: Option<Variant>,
        caller_this: Option<ObjectHandle>,
        op: impl FnOnce(Variant, Option<Variant>) -> Result<Variant>,
    ) -> Result<Variant> {
        let (handle, closure_this) = self.closure_parts(object_value)?;
        let receiver = match closure_this {
            Some(this_obj) => Variant::Closure(Closure::new(handle, Some(this_obj))),
            None => Variant::Object(handle),
        };
        // The member name is read only after that conversion succeeded
        // (`:1842-1853`, `:1950-1963`), so a failure there never masks the
        // receiver's own conversion error.
        let name: Cow<'_, str> = match member {
            OpMember::Name(name) => Cow::Borrowed(name),
            OpMember::Value(value) => Cow::Owned(self.key_from_variant(value)?),
        };
        let current = self.prop_get(
            receiver.clone(),
            &name,
            DispatchFlags::default(),
            caller_this,
        )?;
        let value = op(current, rhs)?;
        self.prop_set(
            receiver,
            &name,
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
        // A string/octet receiver is still read through [`Vm::prop_get`] with
        // the name rather than through `GetStringProperty`/`GetOctetProperty`
        // (`tjsInterCodeExec.cpp:2093-2105`); what this function pins is the
        // error mapping below.
        match self.prop_get(
            object_value,
            &name,
            DispatchFlags::must_exist(),
            frame.this_obj,
        ) {
            Ok(value) => Ok(Variant::String(value.typeof_name().to_string())),
            // `TypeOfMemberDirect` answers "undefined" for
            // `TJS_E_MEMBERNOTFOUND` only (`tjsInterCodeExec.cpp:2134-2139`);
            // every other failure -- a property object with no getter, a
            // receiver that is not an object -- reaches
            // `TJSThrowFrom_tjs_error(hr, name)` and propagates.
            Err(error) if error.is_member_not_found() => {
                Ok(Variant::String("undefined".to_string()))
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn typeof_indirect(
        &mut self,
        frame: &Frame,
        inst: &Instruction,
        _flags: DispatchFlags,
    ) -> Result<Variant> {
        let object_value = frame.get(inst.operands[1])?;
        let name = self.key_from_variant(&frame.get(inst.operands[2])?)?;
        // See [`Vm::typeof_direct`]: the same member-not-found-only mapping
        // applies (`TypeOfMemberIndirect`, `tjsInterCodeExec.cpp:2183-2193`).
        match self.prop_get(
            object_value,
            &name,
            DispatchFlags::must_exist(),
            frame.this_obj,
        ) {
            Ok(value) => Ok(Variant::String(value.typeof_name().to_string())),
            Err(error) if error.is_member_not_found() => {
                Ok(Variant::String("undefined".to_string()))
            }
            Err(error) => Err(error),
        }
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
        let receiver_type = self.value_debug_type(&object_value);
        // Host code has no frame, so nothing supplies `ra[-1]`: the object the
        // host is calling through stands in for it, which is what the
        // reference does when it drives a member call from C++ with an
        // explicit objthis.
        let caller_this = match self.materialize_code_object(object_value.clone()) {
            Variant::Object(handle) => Some(handle),
            Variant::Closure(closure) => closure.this_obj.or(Some(closure.object)),
            _ => None,
        };
        match self.call_member_direct_cont(object_value, name, args, caller_this, Continuation::Root)?
        {
            CallOutcome::Immediate(value, Continuation::Root) => {
                Ok(if dest_reg == 0 { Variant::Void } else { value })
            }
            CallOutcome::Immediate(_, continuation) => Err(TjsError::runtime(format!(
                "unexpected immediate member call continuation {continuation:?}"
            ))),
            CallOutcome::Frame(frame) => {
                let value = self
                    .run_call_stack(vec![*frame], base_depth)
                    .map_err(|error| {
                        error.with_member_access(TjsMemberAccess {
                            operation: TjsMemberOperation::Calling,
                            receiver_type: receiver_type.clone(),
                            member_name: name.to_string(),
                            callee_type: None,
                        })
                    })?;
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
            // A class-qualified constructor call (`super.Foo()`) resolves the
            // constructor on the class object itself, as
            // tTJSInterCodeContext::FuncCall does. Reading it off the instance
            // instead would pick the *derived* constructor whenever a subclass
            // reuses its base class name -- KAGEX does exactly that (`class
            // OptionSpeedSampleRender extends OptionSpeedSampleRender`) --
            // because regmember has already copied the derived member over the
            // inherited one, so the call recurses until the stack overflows.
            // Native classes hold no bytecode constructor; calling the class
            // object itself with the bound instance runs their initializer.
            matches!(
                self.runtime.heap[handle.0].kind,
                ObjectKind::InterCode {
                    context: BytecodeContextType::Class,
                    ..
                }
            )
            .then(|| self.runtime.heap[handle.0].get_raw(name))
            .flatten()
            .unwrap_or_else(|| Variant::Closure(Closure::new(handle, Some(this_obj))))
        } else {
            // VM_CALLD dispatches with `clo.ObjThis ? clo.ObjThis : ra[-1]`.
            // Class objects carry no ObjThis of their own in krkrz, so a
            // class-qualified call from one of the class's instances looks
            // the member up on behalf of that instance: property getters run
            // against it and the inherited-constructor emulation sees it.
            // An unqualified call inside a method body addresses the `%-2`
            // this-proxy, which is never an ObjThis of its own either:
            // tTJSObjectProxy::FuncCall forwards `OBJ1 = objthis ? objthis :
            // Dispatch1`, so the lookup runs on behalf of the real instance.
            // Binding it to the proxy would hand an inherited native method a
            // receiver with no native data behind it -- krkr2 keeps
            // WaveSoundBuffer's and VideoOverlay's methods on the class
            // object, so a script subclass reaches them exactly this way.
            let lookup_this = self
                .bound_super_this(handle, caller_this)?
                .or(closure_this)
                .or(caller_this);
            // A member *call* is `iTJSDispatch2::FuncCall`, which has no
            // Dictionary exception: only `tTJSDictionaryObject::PropGet` maps
            // a miss to void (`tjsDictionary.cpp:720-731`), while `FuncCall`
            // stays with `tTJSCustomObject::FuncCall` and reports
            // `TJS_E_MEMBERNOTFOUND` (`:713-722`).  `MEMBERMUSTEXIST` carries
            // that difference into this lookup: without it a missing member of
            // a Dictionary-classed receiver would read as void and surface as
            // "not a function" instead of the official miss, and the
            // `%-2` proxy would stop at the first object for calls too
            // (`tTJSObjectProxy::FuncCall` walks on `TJS_E_MEMBERNOTFOUND`,
            // `tjsInterCodeExec.cpp:307-330`).
            self.prop_get_handle(handle, name, DispatchFlags::call(), lookup_this)
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
        // `TJSDefaultFuncCall` (`tjsObject.cpp:1280-1312`) forwards a member
        // call only when the member *is* an object; every other value type is
        // `TJS_E_INVALIDTYPE` (-1005).  Calling a value rather than a named
        // member (`VM_CALL`) converts the callee instead
        // (`tjsInterCodeExec.cpp:2365`), which is why the two call forms
        // report different failures for the same non-object callee.
        if !matches!(member, Variant::Closure(_) | Variant::Object(_)) {
            return Err(
                TjsError::invalid_type().with_member_access(TjsMemberAccess {
                    operation: TjsMemberOperation::Calling,
                    receiver_type,
                    member_name: name.to_string(),
                    callee_type: Some(callee_type),
                }),
            );
        }
        // `CallFunctionDirect` (`tjsInterCodeExec.cpp:2406`) resolves the
        // member with `objthis = clo.ObjThis ? clo.ObjThis : ra[-1]` -- the
        // *object expression*'s own ObjThis, else the caller's `this`.  The
        // invocant never supplies it: class members are stored with no
        // ObjThis of their own, so a class-qualified call such as
        // `PreRenderFontEx.KAGLayerFinalizer(...)` -- how the KAGEX font
        // plugin calls back into the `finalize` it saved off `KAGLayer` --
        // must keep running on the caller's `this` (the layer instance), not
        // on the class object the member was read from.
        // A native class object keeps its constructor under the class name
        // with no ObjThis, which is how `Layer.Layer(win, this)` reaches the
        // caller's object even when `Layer` is only one of several parents.
        // Members that do carry an ObjThis -- every method `regmember` copied
        // onto an instance, and every native function the property read bound
        // to its receiver -- keep it, because `call_value` prefers the
        // callee's own binding.
        let call_this = self
            .bound_super_this(handle, caller_this)?
            .or(closure_this)
            .or_else(|| self.receiver_supplies_call_this(handle, name).then_some(handle))
            .or(caller_this);
        self.call_value(member, call_this, args, false, continuation)
        .map_err(|error| {
            error.with_member_access(TjsMemberAccess {
                operation: TjsMemberOperation::Calling,
                receiver_type,
                member_name: name.to_string(),
                callee_type: Some(callee_type),
            })
        })
    }

    /// Whether a member call on this receiver supplies the callee's `this`
    /// when neither the receiver value nor a `super` qualifier carries one.
    ///
    /// `CallFunctionDirect` uses `clo.ObjThis ? clo.ObjThis : ra[-1]`
    /// (`tjsInterCodeExec.cpp:2406`), and the reference hands scripts object
    /// values that carry their own ObjThis: `this` and `new` results are
    /// stored as `tTJSVariant(objthis, objthis)` (`:839`, `:2372`), and native
    /// accessors return `tTJSVariant(o, o)` the same way
    /// (`LayerIntf.cpp:6906`).  So a method read off an object normally runs
    /// on that object rather than on whichever object made the call -- KAGEX
    /// depends on it, because `objectHookInjection` installs its override
    /// functions directly onto the window (`l0[l5] = t1 incontextof l4`,
    /// `system_Utils.tjs`), leaving them without a binding of their own.
    ///
    /// Class objects keep the caller's `this`: `PreRenderFontEx.KAGLayerFinalizer(...)`
    /// (how the font plugin calls back into the `finalize` it saved off
    /// `KAGLayer`) and the native `Layer.Layer(win, this)` constructor must
    /// run on the caller's object, and `bound_super_this` already covers the
    /// class-qualified cases that do belong to the calling instance.
    fn receiver_supplies_call_this(&self, handle: ObjectHandle, name: &str) -> bool {
        // `handle_class_name_matches` covers the native class objects too:
        // `Layer.Layer(win, this)` reads the constructor stored under the class
        // name, and that member is what makes the call a class-qualified
        // construction instead of a method invocation.
        !self.handle_class_name_matches(handle, name)
            && handle != self.runtime.global
            // A `%-2` this-proxy receiver already forwards property access to
            // the instance that made it, so it keeps the caller's `this`
            // instead of turning the proxy itself into the callee's receiver.
            && !matches!(
                self.runtime.heap[handle.0].kind,
                ObjectKind::Proxy { .. }
                    | ObjectKind::InterCode {
                        context: BytecodeContextType::Class,
                        ..
                    }
            )
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

    /// The class object in `class_handle`'s extender chain that holds `name`
    /// as a raw member, searching the way tTJSInterCodeContext's
    /// TJS_DO_SUPERCLASS_PROXY does: the class itself, then every superclass
    /// getter entry in reverse declaration order, recursively. Only bytecode
    /// classes have extender entries; a native class ends the walk.
    fn class_member_owner(
        &mut self,
        class_handle: ObjectHandle,
        name: &str,
        skip_primary: bool,
    ) -> Result<Option<ObjectHandle>> {
        if self.runtime.heap[class_handle.0].get_raw(name).is_some() {
            return Ok(Some(class_handle));
        }
        let Some(pointers) = self.class_extender_pointers(class_handle)? else {
            return Ok(None);
        };
        let ObjectKind::InterCode {
            file_id,
            object_index,
            ..
        } = self.runtime.heap[class_handle.0].kind
        else {
            return Ok(None);
        };
        let file = self.runtime.script_file(file_id)?;
        let Some(getter_index) = file.objects[object_index].super_class_getter else {
            return Ok(None);
        };
        // Entry 0 is the primary parent, which `super_class_handle` callers
        // have already searched (and cached).
        let first = usize::from(skip_primary);
        for pointer in pointers.into_iter().skip(first).rev() {
            let code_offset = usize::try_from(pointer)
                .map_err(|_| TjsError::runtime("negative superclass getter entry offset"))?;
            let value = self.execute_file_object_with_this_preserving_active_at(
                file_id,
                getter_index,
                code_offset,
                Vec::new(),
                Some(self.runtime.global),
            )?;
            if matches!(value, Variant::Void | Variant::Null) {
                continue;
            }
            let super_handle = self.resolve_object(value)?;
            if super_handle == class_handle {
                continue;
            }
            if let Some(owner) = self.class_member_owner(super_handle, name, false)? {
                return Ok(Some(owner));
            }
        }
        Ok(None)
    }

    fn is_bytecode_class(&self, handle: ObjectHandle) -> bool {
        matches!(
            self.runtime.heap[handle.0].kind,
            ObjectKind::InterCode {
                context: BytecodeContextType::Class,
                ..
            }
        )
    }

    /// Superclass getter entry offsets of a bytecode class (one per
    /// `extends` operand, in declaration order), or `None` for anything that
    /// is not a bytecode class or extends nothing.
    fn class_extender_pointers(&mut self, handle: ObjectHandle) -> Result<Option<Vec<i32>>> {
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
        let pointers = &file.objects[getter_index].super_class_getter_pointers;
        if pointers.is_empty() {
            return Ok(None);
        }
        Ok(Some(pointers.clone()))
    }

    /// Whether `handle` is a bytecode class with more than one `extends`
    /// operand, i.e. members may live behind a superclass getter entry that
    /// the primary `super_class_handle` chain never visits.
    fn has_secondary_extenders(&mut self, handle: ObjectHandle) -> Result<bool> {
        Ok(self
            .class_extender_pointers(handle)?
            .is_some_and(|pointers| pointers.len() > 1))
    }

    /// The class object that a write through class object `handle` lands on
    /// in tTJSInterCodeContext::PropSet: the class itself when it already
    /// holds `name`, otherwise the nearest class in its extender chain that
    /// does. `None` when no class in the chain has the member (the caller
    /// then ensures it on `handle`). The cached primary chain is walked
    /// first; superclass getter code only runs for classes with several
    /// `extends` operands.
    fn class_chain_owner(
        &mut self,
        handle: ObjectHandle,
        name: &str,
    ) -> Result<Option<ObjectHandle>> {
        let mut seen = Vec::new();
        let mut current = Some(handle);
        while let Some(class_handle) = current {
            if seen.contains(&class_handle.0) {
                break;
            }
            seen.push(class_handle.0);
            if self.runtime.heap[class_handle.0].get_raw(name).is_some() {
                return Ok(Some(class_handle));
            }
            if self.has_secondary_extenders(class_handle)?
                && let Some(owner) = self.class_member_owner(class_handle, name, true)?
            {
                return Ok(Some(owner));
            }
            current = self.super_class_handle(class_handle)?;
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
        // krkrz native classes publish their constructor as a member named
        // after the class, so `KAGLayer.Layer(win, par)` from a class that
        // never declared its own constructor reaches Layer's initializer via
        // KAGLayer's superclass proxy. Kirakira runs a native initializer by
        // calling the class object with the instance bound.
        if self.handle_class_name_matches(class_handle, name) {
            return Ok(Some(Variant::Closure(Closure::new(
                class_handle,
                Some(instance),
            ))));
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

    pub(crate) fn call_primary_class_method(
        &mut self,
        object: ObjectHandle,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<bool> {
        let class_infos = self.runtime.heap[object.0].class_infos.clone();
        for class_name in class_infos {
            let class_handle = match self.runtime.global_member(&class_name) {
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
        let receiver_type = self.object_debug_type(handle, "object");
        let result = (|| {
            // `tTJSCustomObject::Finalize` (`tjsObject.cpp:451`) dispatches
            // `finalize` through `FuncCall` and *ignores its return value*, so
            // an object that does not carry the member -- an Array, a
            // Dictionary, a plain data object -- finalizes silently.  Only a
            // member that exists and throws propagates.
            let finalize = match self.prop_get_handle(
                handle,
                "finalize",
                DispatchFlags::default(),
                Some(handle),
            ) {
                Ok(value) => value,
                Err(error) if error.is_member_not_found() => Variant::Void,
                Err(error) => return Err(error),
            };
            if !matches!(finalize, Variant::Void) {
                let base_depth = self.runtime.call_depth;
                let outcome = self
                    .call_value(
                        finalize,
                        Some(handle),
                        Vec::new(),
                        false,
                        Continuation::Root,
                    )
                    .map_err(|error| {
                        error.with_member_access(TjsMemberAccess {
                            operation: TjsMemberOperation::Calling,
                            receiver_type: receiver_type.clone(),
                            member_name: "finalize".to_string(),
                            callee_type: None,
                        })
                    })?;
                match outcome {
                    CallOutcome::Immediate(_, Continuation::Root) => {}
                    CallOutcome::Immediate(_, continuation) => {
                        return Err(TjsError::runtime(format!(
                            "unexpected finalize continuation {continuation:?}"
                        )));
                    }
                    CallOutcome::Frame(frame) => {
                        self.run_call_stack(vec![*frame], base_depth)
                            .map_err(|error| {
                                error.with_member_access(TjsMemberAccess {
                                    operation: TjsMemberOperation::Calling,
                                    receiver_type: receiver_type.clone(),
                                    member_name: "finalize".to_string(),
                                    callee_type: None,
                                })
                            })?;
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
            // `VM_CALL` converts the callee with `AsObjectClosure()`
            // (`tjsInterCodeExec.cpp:2365`), so a value that is not an object
            // fails the conversion before any call happens; a null object
            // closure (`clo.Object == NULL`) is the null-access failure
            // (`tjsVariant.h:228-237`).  Only an *object* that is not callable
            // reaches `tTJSCustomObject::FuncCall`, which answers
            // `TJS_E_INVALIDTYPE` (-1005).
            Variant::Null => Err(TjsError::null_access()),
            other => Err(TjsError::variant_convert_to_object(&other)),
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
                } else if context == BytecodeContextType::Property {
                    // A property object is not a function:
                    // `tTJSInterCodeContext::FuncCall` with no member name
                    // answers `TJS_E_INVALIDTYPE` for `ctProperty`
                    // (`tjsInterCodeExec.cpp:3100-3101`).
                    Err(TjsError::invalid_type())
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
            ObjectKind::NativeFunction {
                id,
                constructable,
                arg_count,
            } => {
                if !arg_count.accepts(args.len()) {
                    // The declaration's argument-count contract is checked
                    // before the handler runs, the way every official native
                    // checks `numparams` first (`LayerIntf.cpp:7835`).
                    return Err(TjsError::bad_param_count());
                }
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
                if let Some(name) = self.runtime.native_call_name(id, false).map(str::to_string) {
                    self.runtime.trace_native_call(&name, native_this, &args);
                }
                Ok(CallOutcome::Immediate(
                    function.call(self.runtime, native_this, args)?,
                    continuation,
                ))
            }
            ObjectKind::VmNativeFunction { id, arg_count } => {
                if !arg_count.accepts(args.len()) {
                    return Err(TjsError::bad_param_count());
                }
                let function = self
                    .runtime
                    .vm_native_functions
                    .get(id)
                    .cloned()
                    .ok_or_else(|| TjsError::runtime(format!("VM native function {id} missing")))?;
                if let Some(name) = self.runtime.native_call_name(id, true).map(str::to_string) {
                    self.runtime.trace_native_call(&name, this_obj, &args);
                }
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
                // A data object is not callable; the reference reports
                // `TJS_E_INVALIDTYPE` (-1005) for the attempt.
                Err(TjsError::invalid_type())
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
            self.begin_class_initialization(instance);
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
        self.begin_class_initialization(instance);
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

    /// Whether `name` is declared anywhere in the object's class chain.
    ///
    /// In KRKR an instance owns a copy of every class member -- native classes
    /// copy their members when they are initialized on the object
    /// (`tTJSNativeClass::FuncCall`, `tjsNative.cpp:293`), script classes
    /// register theirs from the class body (`tTJSInterCodeContext::CreateNew`,
    /// `tjsInterCodeExec.cpp:3211`) -- so this is what `Find` would report for
    /// the instance's own member table (`tjsObject.cpp:1475`).
    fn class_chain_provides_member(&mut self, handle: ObjectHandle, name: &str) -> Result<bool> {
        let mut current = self.super_class_handle(handle)?;
        for _ in 0..64 {
            let Some(class_handle) = current else {
                return Ok(false);
            };
            if self.runtime.heap[class_handle.0].get_raw(name).is_some() {
                return Ok(true);
            }
            current = self.super_class_handle(class_handle)?;
        }
        Ok(false)
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
            "toLowerCase" => {
                // Official: `if(numargs != 0) TJSThrowFrom_tjs_error(
                // TJS_E_BADPARAMCOUNT)` (`tjsInterCodeExec.cpp:2659`).
                if !args.is_empty() {
                    return Err(TjsError::bad_param_count());
                }
                Ok(Variant::String(string_to_ascii_lowercase(&value)))
            }
            "toUpperCase" => {
                if !args.is_empty() {
                    return Err(TjsError::bad_param_count());
                }
                Ok(Variant::String(string_to_ascii_uppercase(&value)))
            }
            "charAt" => {
                if args.len() != 1 {
                    return Err(TjsError::bad_param_count());
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
                // Official accepts one or two arguments
                // (`tjsInterCodeExec.cpp:2697`).
                if args.is_empty() || args.len() > 2 {
                    return Err(TjsError::bad_param_count());
                }
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
                // Official: `if(numargs < 2) TJSThrowFrom_tjs_error(
                // TJS_E_BADPARAMCOUNT)` (`tjsInterCodeExec.cpp:2739`).
                if args.len() < 2 {
                    return Err(TjsError::bad_param_count());
                }
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
                // Official accepts one or two arguments
                // (`tjsInterCodeExec.cpp:2612`).
                if args.is_empty() || args.len() > 2 {
                    return Err(TjsError::bad_param_count());
                }
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
                // Official: `if(numargs < 1) TJSThrowFrom_tjs_error(
                // TJS_E_BADPARAMCOUNT)` (`tjsInterCodeExec.cpp:2761`).
                let Some(pattern) = args.first() else {
                    return Err(TjsError::bad_param_count());
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
                // Official: `if(numargs != 0)` (`tjsInterCodeExec.cpp:2798`).
                if !args.is_empty() {
                    return Err(TjsError::bad_param_count());
                }
                Ok(Variant::String(trim_tjs_ascii_space(&value).to_string()))
            }
            "reverse" => {
                // Official: `if(numargs != 0)` (`tjsInterCodeExec.cpp:2822`).
                if !args.is_empty() {
                    return Err(TjsError::bad_param_count());
                }
                Ok(Variant::String(value.chars().rev().collect()))
            }
            "repeat" => {
                // Official: `if(numargs != 1)` (`tjsInterCodeExec.cpp:2844`).
                if args.len() != 1 {
                    return Err(TjsError::bad_param_count());
                }
                let count = args[0].to_integer()?;
                if count <= 0 || value.is_empty() {
                    return Ok(Variant::String(String::new()));
                }
                Ok(Variant::String(value.repeat(count as usize)))
            }
            // `ProcessStringFunction` reports an unknown string method as
            // `TJS_E_MEMBERNOTFOUND` with the member name
            // (`tjsInterCodeExec.cpp:2869`), not as a method-specific error.
            _ => Err(TjsError::member_not_found(name)),
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
        // `ProcessOctetFunction` mirrors the string path: an unknown octet
        // method is `TJS_E_MEMBERNOTFOUND` with the member name
        // (`tjsInterCodeExec.cpp:2896`).
        Err(TjsError::member_not_found(name))
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

    /// The `this` a member inherited through a class chain runs on.
    ///
    /// `regmember` copies a class's members onto every instance with
    /// `val.ChangeClosureObjThis(Dest)` (`tjsInterCodeExec.cpp:3033`), so in the
    /// reference an inherited member reached through an instance always carries
    /// that instance as its ObjThis.  A receiver that *is* a class object keeps
    /// the caller's `this` instead: the class owns those members directly, with
    /// no ObjThis of their own, and the VM falls back to `ra[-1]`
    /// (`tjsInterCodeExec.cpp:1593`).
    fn inherited_member_this(
        &mut self,
        handle: ObjectHandle,
        caller_this: Option<ObjectHandle>,
    ) -> Option<ObjectHandle> {
        if self.is_bytecode_class(handle) {
            caller_this.or(Some(handle))
        } else {
            Some(handle)
        }
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
            other => Err(TjsError::variant_convert_to_object(&other)),
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
        // TJS exposes class definitions as first-class `Class` objects. They
        // are callable through `new`, but are not ordinary Function objects:
        // framework code commonly dispatches a configured `start` value by
        // testing Function first and Class second.
        if class_name == "Class"
            && matches!(
                self.runtime.heap[handle.0].kind,
                ObjectKind::InterCode {
                    context: BytecodeContextType::Class,
                    ..
                }
            )
        {
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

    pub(super) fn register_object_members(
        &mut self,
        source: ObjectHandle,
        dest: ObjectHandle,
    ) -> Result<()> {
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
            // tTJSInterCodeContext::RegisterObjectMember stores through
            // PropSet, so a destination with `missing` enabled sees every
            // member it does not already hold. KAGEX's class-name probe runs
            // a class body against such an object and relies on the hook
            // throwing here to stop the body before its field initialisers
            // run.
            if self.runtime.heap[dest.0].call_missing
                && self.runtime.heap[dest.0].get_raw(&name).is_none()
                && self.call_set_missing(dest, &name, value.clone())?
            {
                continue;
            }
            self.runtime.heap[dest.0].set(name, value);
        }
        Ok(())
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

/// The member operand of an op-protocol access (`OperateProperty*`,
/// `tjsInterCodeExec.cpp:1811-1985`): a name from the instruction's data
/// area, or the raw register value of the indirect form, which the reference
/// converts with `AsString` only after the receiver's closure.
enum OpMember<'a> {
    Name(&'a str),
    Value(&'a Variant),
}

/// The member operand of a string/octet property access, resolved the way
/// `GetStringProperty`/`GetOctetProperty` see it (`tjsInterCodeExec.cpp:50`,
/// `:106`, `:132`, `:176`): an Integer or Real member is an index, every
/// other member must be a string variant, and the rest raises the conversion
/// failure `member.GetString()` reports (`tjsVariant.cpp:790-795`).
enum MemberKind<'a> {
    Index(i32),
    Name(&'a str),
}

fn member_kind(member: &Variant) -> Result<MemberKind<'_>> {
    match member {
        Variant::String(name) => Ok(MemberKind::Name(name)),
        // `(tjs_int)member.AsInteger()`: the reference narrows the value to
        // its 32-bit `tjs_int`, so an out-of-range key truncates instead of
        // reporting a range error.
        Variant::Integer(value) => Ok(MemberKind::Index(*value as i32)),
        // A real outside the i64 range saturates in the first cast where the
        // reference's `(tjs_int64)` is undefined (x86 yields the integer
        // indefinite value, so `"abc"[1e20]` is index 0 there and a range
        // error here).
        Variant::Real(value) => Ok(MemberKind::Index(*value as i64 as i32)),
        other => Err(TjsError::variant_convert(other, "string")),
    }
}

/// The reference's "is this name an index" test: the *first* character must
/// be a digit (`tjsInterCodeExec.cpp:67`, `:147`). `TJS_atoi` tolerates
/// leading spaces and a sign, but that code sits inside this branch, so a
/// name like `" 1"` or `"-1"` is an ordinary member miss.
fn is_index_name(name: &str) -> bool {
    name.chars().next().is_some_and(|ch| ch.is_ascii_digit())
}

/// `TJS_atoi` (`tjsConfig.cpp:54-76`): skip spaces, accept one leading `-`,
/// then consume digits until the first non-digit, accumulating in the
/// reference's 32-bit `tjs_int` (so it wraps, and `"4294967296"` is index
/// 0).
fn tjs_atoi(name: &str) -> i32 {
    let mut chars = name.chars().peekable();
    let mut value: i32 = 0;
    while chars.next_if(|ch| *ch <= ' ').is_some() {}
    let negative = chars.next_if_eq(&'-').is_some();
    if negative {
        while chars.next_if(|ch| *ch <= ' ').is_some() {}
    }
    while let Some(digit) = chars.next_if(|ch| ch.is_ascii_digit()) {
        value = value
            .wrapping_mul(10)
            .wrapping_add(digit as i32 - '0' as i32);
    }
    if negative { value.wrapping_neg() } else { value }
}

fn escape_tjs_string_fragment(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            // Use a hexadecimal escape instead of `\\'`: callers such as
            // KAG's applyInlineStringVariableExtract embed this output in an
            // `@'...'` interpolated source string, where a literal quote must
            // never be able to terminate the generated source.
            '\'' => escaped.push_str("\\x27"),
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::compiler::compile_source_to_bytecode;
    use crate::error::TjsErrorKind;
    use crate::runtime::{NativeArgCount, NativePropertyAccess, NoHost, Runtime};

    use super::*;

    fn run(source: &str) -> Result<Variant> {
        let file = compile_source_to_bytecode("dispatch-test.tjs", source).expect("compile");
        Runtime::new().execute_file(&file)
    }

    fn run_with(runtime: &mut Runtime<NoHost>, source: &str) -> Result<Variant> {
        let file = compile_source_to_bytecode("dispatch-test.tjs", source).expect("compile");
        runtime.execute_file(&file)
    }

    fn failure(source: &str) -> TjsError {
        run(source).expect_err("script should fail")
    }

    fn vm_with(runtime: &mut Runtime<NoHost>) -> Vm<'_, '_, NoHost> {
        let file = compile_source_to_bytecode("dispatch-test.tjs", "return 0;").expect("compile");
        let file_id = runtime.install_script_file(Arc::new(file));
        Vm::new(file_id, runtime).expect("vm")
    }

    /// A VM-native handler needs a named function: a closure is not generic
    /// over the VM's lifetimes, which is what [`VmNativeFunction`] requires.
    /// [`NativeFunction`] is equally higher-ranked over `&mut Runtime`, so the
    /// native handlers below are named functions too.
    fn vm_native_void(
        _vm: &mut Vm<'_, '_, NoHost>,
        _this_obj: Option<ObjectHandle>,
        _args: Vec<Variant>,
    ) -> Result<Variant> {
        Ok(Variant::Void)
    }

    fn native_void(
        _runtime: &mut Runtime<NoHost>,
        _this_obj: Option<ObjectHandle>,
        _args: Vec<Variant>,
    ) -> Result<Variant> {
        Ok(Variant::Void)
    }

    #[test]
    fn missing_member_read_reports_the_official_member_not_found_text() {
        let error = failure("return no_such_global;");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.tjs_error_code(), Some(-1001));
        assert_eq!(error.message, "Member \"no_such_global\" does not exist");
        assert!(error.is_member_not_found());
    }

    /// A member read whose receiver is not an object fails the object
    /// conversion before the name is looked at: `GetPropertyDirect` takes
    /// `AsObjectClosureNoAddRef` on the object expression first
    /// (`tjsInterCodeExec.cpp:1593-1608`), which raises
    /// `TJSThrowVariantConvertError` for a void value (`tjsVariant.h:710-719`,
    /// `tjsVariant.cpp:142-151`).  A void receiver therefore raises instead of
    /// answering void.
    #[test]
    fn void_receiver_member_read_raises_the_conversion_error() {
        let error = failure("var empty = void; return empty.member;");
        assert_eq!(error.kind, TjsErrorKind::Runtime);
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((void) to Object)"
        );
    }

    /// The indirect form converts the receiver first as well
    /// (`GetPropertyIndirect`, `tjsInterCodeExec.cpp:1689-1705`).
    #[test]
    fn void_receiver_indirect_read_raises_the_conversion_error() {
        let error = failure("var empty = void; return empty[0];");
        assert_eq!(error.kind, TjsErrorKind::Runtime);
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((void) to Object)"
        );
    }

    /// `TypeOfMemberDirect` maps `TJS_E_MEMBERNOTFOUND` to "undefined" and
    /// lets every other failure propagate (`tjsInterCodeExec.cpp:2127-2143`),
    /// so the same void receiver raises through `typeof` too.
    #[test]
    fn typeof_member_expression_propagates_the_conversion_failure() {
        let error = failure("var empty = void; return typeof empty.member;");
        assert_eq!(error.kind, TjsErrorKind::Runtime);
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((void) to Object)"
        );
    }

    #[test]
    fn negative_array_index_write_reports_member_not_found() {
        // `tTJSArrayObject::PropSetByNum` (`tjsArray.cpp:1647`) reports the
        // unwrappable index as TJS_E_MEMBERNOTFOUND, and the name the message
        // carries is the index itself (`ThrowFrom_tjs_error_num`).
        let error = failure("var a = new Array(); a[-1] = 5;");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"-1\" does not exist");
        assert!(error.is_member_not_found());
    }

    #[test]
    fn unknown_string_method_reports_member_not_found() {
        let error = failure("return \"abc\".bogusMethod();");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"bogusMethod\" does not exist");
    }

    #[test]
    fn unknown_octet_method_reports_member_not_found() {
        let mut runtime = Runtime::new();
        let mut vm = vm_with(&mut runtime);
        let error = match vm.call_member_direct_cont(
            Variant::Octet(vec![1, 2, 3]),
            "bogusMethod",
            Vec::new(),
            None,
            Continuation::Root,
        ) {
            Ok(_) => panic!("octet method should be missing"),
            Err(error) => error,
        };
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"bogusMethod\" does not exist");
    }

    #[test]
    fn calling_a_non_function_value_reports_the_convert_error() {
        // `VM_CALL` converts the callee with `AsObjectClosure()`
        // (`tjsInterCodeExec.cpp:2365`), so a value that is not an object
        // fails that conversion rather than reaching `FuncCall`.
        let error = failure("var value = 5; return value();");
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((int)5 to Object)"
        );
        assert_eq!(error.tjs_error_code(), None);
    }

    #[test]
    fn calling_a_null_value_reports_the_null_access() {
        let error = failure("var value = null; return value();");
        assert_eq!(error.message, "Accessing to null object");
    }

    #[test]
    fn calling_a_property_object_reports_invalid_type() {
        // `tTJSInterCodeContext::FuncCall` answers `TJS_E_INVALIDTYPE` for a
        // property context (`tjsInterCodeExec.cpp:3100-3101`).
        let error = failure(
            "class Holder {\n\
             \x20   property Value {\n\
             \x20       setter(value) { this.stored = value; }\n\
             \x20   }\n\
             }\n\
             var holder = new Holder();\n\
             var accessor = holder.Value;\n\
             return accessor();",
        );
        assert_eq!(error.kind, TjsErrorKind::InvalidType, "{}", error.message);
        assert_eq!(
            error.message,
            "Not a function or invalid method/property type"
        );
    }

    #[test]
    fn calling_a_data_member_reports_invalid_type() {
        let error = failure(
            "class Holder { }\nvar holder = new Holder(); holder.member = 5;\nreturn holder.member();",
        );
        assert_eq!(error.kind, TjsErrorKind::InvalidType);
        assert_eq!(
            error.message,
            "Not a function or invalid method/property type"
        );
    }

    #[test]
    fn null_member_access_reports_the_official_null_text() {
        let error = failure("var value = null; return value.member;");
        assert_eq!(error.message, "Accessing to null object");
        assert_eq!(error.tjs_error_code(), None);
    }

    #[test]
    fn non_object_member_access_reports_the_convert_error() {
        // `TJSThrowVariantConvertError` renders the value with
        // `TJSVariantToReadableString`, which tags the type.
        let error = failure("var value = 5; return value.member;");
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((int)5 to Object)"
        );
    }

    #[test]
    fn readable_conversion_renders_each_value_shape() {
        assert_eq!(
            failure("var value = 1.5; return value.member;").message,
            "Cannot convert the variable type ((real)1.5 to Object)"
        );
        // A write reaches the conversion for a void value too: only the read
        // path treats a void target as an absent member.
        assert_eq!(
            failure("var value; value.member = 1;").message,
            "Cannot convert the variable type ((void) to Object)"
        );
    }

    #[test]
    fn property_without_a_getter_denies_the_default_property_read() {
        // The `*property` operator is the reference's default-property
        // dereference (`VM_GETP`, `tjsInterCodeExec.cpp:1664` ->
        // `PropGet(0, NULL, ...)`), and a script `property` with no getter
        // answers `TJS_E_ACCESSDENYED` there
        // (`tjsInterCodeExec.cpp:3138`).
        let error = failure(
            "class Holder {\n\
             \x20   property Value {\n\
             \x20       setter(value) { this.stored = value; }\n\
             \x20   }\n\
             }\n\
             var holder = new Holder();\n\
             var accessor = holder.Value;\n\
             return *accessor;",
        );
        assert_eq!(error.kind, TjsErrorKind::AccessDenied);
        assert_eq!(error.tjs_error_code(), Some(-1007));
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );

        // A native property with a denied getter reaches the same check on the
        // same dereference path.
        let mut runtime = Runtime::new();
        let target = runtime.alloc_ordinary_object();
        let accessor = runtime.register_object_native_property_with_access(
            target,
            "children",
            NativePropertyAccess::WriteOnly,
            |_runtime, _this| Ok(Variant::Integer(7)),
            |_runtime, _this, _value| Ok(()),
        );
        let file = compile_source_to_bytecode("dispatch-test.tjs", "return 0;").expect("compile");
        let file_id = runtime.install_script_file(Arc::new(file));
        let mut vm = Vm::new(file_id, &mut runtime).expect("vm");
        let error = vm
            .default_prop_get(Variant::Object(accessor), None)
            .expect_err("denied read");
        assert_eq!(error.kind, TjsErrorKind::AccessDenied);
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );
    }

    #[test]
    fn property_without_a_setter_denies_the_default_property_write() {
        // `VM_SETP` (`tjsInterCodeExec.cpp:1676`) is the reference's
        // `PropSet(0, NULL, ...)`, which a property with a denied setter
        // answers with `TJS_E_ACCESSDENYED`.  Script code cannot pass a
        // property object through a local (`var a = &obj.prop;` re-reads the
        // member and invokes the property), so the dereference is driven
        // through the VM directly.
        let mut runtime = Runtime::new();
        let target = runtime.alloc_ordinary_object();
        let accessor = runtime.register_object_native_property_with_access(
            target,
            "children",
            NativePropertyAccess::ReadOnly,
            |_runtime, _this| Ok(Variant::Integer(7)),
            |_runtime, _this, _value| Ok(()),
        );
        let file = compile_source_to_bytecode("dispatch-test.tjs", "return 0;").expect("compile");
        let file_id = runtime.install_script_file(Arc::new(file));
        let mut vm = Vm::new(file_id, &mut runtime).expect("vm");
        let error = vm
            .default_prop_set(Variant::Object(accessor), Variant::Integer(5), None)
            .expect_err("denied write");
        assert_eq!(error.kind, TjsErrorKind::AccessDenied, "{}", error.message);
        assert_eq!(error.tjs_error_code(), Some(-1007));
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );
    }

    #[test]
    fn member_read_hands_a_script_property_object_back() {
        // Reading the member itself does not dereference the property: the
        // engine hands the property object back, which is how KAGEX's
        // `objectHookInjection` obtains the property it installs on another
        // object (`krkr-engine`'s
        // `layer_font_reads_back_bound_to_the_font_like_krkr` pins it).
        let value = run("class Holder {\n\
             \x20   property Value {\n\
             \x20       setter(value) { this.stored = value; }\n\
             \x20   }\n\
             }\n\
             var holder = new Holder();\n\
             return holder.Value;")
        .expect("read");
        assert!(
            matches!(value, Variant::Closure(_) | Variant::Object(_)),
            "expected the property object, got {value:?}"
        );
    }

    #[test]
    fn property_without_a_setter_denies_writes() {
        let error = failure(
            "class Holder {\n\
             \x20   property Value {\n\
             \x20       getter() { return 5; }\n\
             \x20   }\n\
             }\n\
             var holder = new Holder();\n\
             holder.Value = 5;",
        );
        assert_eq!(error.kind, TjsErrorKind::AccessDenied);
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );
    }

    #[test]
    fn read_only_native_property_denies_script_writes() {
        let mut runtime = Runtime::new();
        let target = runtime.alloc_ordinary_object();
        runtime.set_global_member("target", Variant::Object(target));
        runtime.register_object_native_property_with_access(
            target,
            "children",
            NativePropertyAccess::ReadOnly,
            |_runtime, _this| Ok(Variant::Integer(7)),
            |_runtime, _this, _value| Ok(()),
        );

        assert_eq!(
            run_with(&mut runtime, "return target.children;").expect("read"),
            Variant::Integer(7)
        );
        let error = run_with(&mut runtime, "target.children = 1;").expect_err("denied write");
        assert_eq!(error.kind, TjsErrorKind::AccessDenied);
        assert_eq!(error.tjs_error_code(), Some(-1007));
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );

        // The denial is a script-facing policy, not a torn-down accessor:
        // restoring the access gives the setter back.
        let Variant::Object(property) = runtime.object_member(target, "children") else {
            panic!("children should be a native property object");
        };
        assert_eq!(
            runtime.native_property_access(property),
            Some(NativePropertyAccess::ReadOnly)
        );
        assert!(runtime.set_native_property_access(property, NativePropertyAccess::ReadWrite));
        run_with(&mut runtime, "target.children = 1;").expect("write after restoring access");
    }

    #[test]
    fn write_only_native_property_denies_script_reads() {
        let mut runtime = Runtime::new();
        let target = runtime.alloc_ordinary_object();
        runtime.set_global_member("target", Variant::Object(target));
        runtime.register_object_native_property_with_access(
            target,
            "writeonly",
            NativePropertyAccess::WriteOnly,
            |_runtime, _this| Ok(Variant::Integer(1)),
            |_runtime, _this, _value| Ok(()),
        );
        let error = run_with(&mut runtime, "return target.writeonly;").expect_err("denied read");
        assert_eq!(error.kind, TjsErrorKind::AccessDenied);
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );
        run_with(&mut runtime, "target.writeonly = 1;").expect("write");
    }

    #[test]
    fn deny_list_marks_registered_properties_read_only() {
        let mut runtime = Runtime::new();
        let target = runtime.alloc_ordinary_object();
        runtime.set_global_member("target", Variant::Object(target));
        runtime.register_object_native_property(
            target,
            "children",
            |_runtime, _this| Ok(Variant::Integer(1)),
            |_runtime, _this, _value| Ok(()),
        );
        runtime.register_object_native_property(
            target,
            "nodeVisible",
            |_runtime, _this| Ok(Variant::Integer(1)),
            |_runtime, _this, _value| Ok(()),
        );
        runtime.set_object_member(target, "plain", Variant::Integer(0));

        let unmatched = runtime
            .deny_native_property_writes(target, &["children", "nodeVisible", "plain", "nope"]);
        assert_eq!(unmatched, vec!["plain".to_string(), "nope".to_string()]);

        for name in ["children", "nodeVisible"] {
            let error =
                run_with(&mut runtime, &format!("target.{name} = 1;")).expect_err("denied write");
            assert_eq!(error.kind, TjsErrorKind::AccessDenied, "{name}");
        }
        run_with(&mut runtime, "target.plain = 1;").expect("data members stay writable");
        run_with(&mut runtime, "return target.children;").expect("getter still works");
    }

    #[test]
    fn declared_argument_counts_fail_with_bad_param_count() {
        let mut runtime = Runtime::new();
        let target = runtime.alloc_ordinary_object();
        runtime.set_global_member("target", Variant::Object(target));
        runtime.register_object_native_with_arg_count(
            target,
            "assignImages",
            NativeArgCount::AtLeast(1),
            native_void,
        );
        runtime.register_object_native_with_arg_count(
            target,
            "exact",
            NativeArgCount::Exactly(2),
            native_void,
        );
        runtime.register_object_vm_native_with_arg_count(
            target,
            "vmMethod",
            NativeArgCount::Exactly(1),
            vm_native_void,
        );

        let error = run_with(&mut runtime, "target.assignImages();").expect_err("too few");
        assert_eq!(error.kind, TjsErrorKind::BadParamCount);
        assert_eq!(error.tjs_error_code(), Some(-1004));
        assert_eq!(error.message, "Invalid argument count");

        for source in [
            "target.exact();",
            "target.exact(1);",
            "target.exact(1, 2, 3);",
            "target.vmMethod();",
            "target.vmMethod(1, 2);",
        ] {
            let error = run_with(&mut runtime, source).expect_err(source);
            assert_eq!(error.kind, TjsErrorKind::BadParamCount, "{source}");
            assert_eq!(error.message, "Invalid argument count", "{source}");
        }

        run_with(&mut runtime, "target.assignImages(1);").expect("declared minimum");
        run_with(&mut runtime, "target.exact(1, 2);").expect("declared exact count");
        run_with(&mut runtime, "target.vmMethod(1);").expect("declared vm count");
    }

    #[test]
    fn string_length_reads_code_units_and_count_is_a_miss() {
        assert_eq!(
            run(r#"return "abcd".length;"#).expect("length"),
            Variant::Integer(4)
        );
        // `GetStringProperty` (`tjsInterCodeExec.cpp:55`) compares against
        // `length` alone, so `count` -- an Array member -- is not a string
        // member.
        let error = failure(r#"return "abcd".count;"#);
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.tjs_error_code(), Some(-1001));
        assert_eq!(error.message, "Member \"count\" does not exist");
        assert_eq!(
            run(r#"return "😀".length;"#).expect("surrogate pair"),
            Variant::Integer(2)
        );
    }

    #[test]
    fn string_index_reads_the_utf16_code_unit() {
        assert_eq!(
            run(r#"return "abcd"[1] + "abcd"["2"] + "abcd"["3"];"#).expect("index"),
            Variant::String("bcd".to_string())
        );
        // `TJS_atoi` (`tjsConfig.cpp:54`) stops at the first non-digit, so a
        // name that merely *starts* with one is still an index.
        assert_eq!(
            run(r#"return "abcd"["1x"];"#).expect("atoi prefix"),
            Variant::String("b".to_string())
        );
        // A numeric key takes the reference's numeric branch, which narrows
        // through `(tjs_int)member.AsInteger()` (`tjsInterCodeExec.cpp:89`).
        assert_eq!(
            run(r#"return "abcd"[1.5] + "abcd"[1.9];"#).expect("real key"),
            Variant::String("bb".to_string())
        );
        assert_eq!(
            run(r#"return "abcd"["0"];"#).expect("zero"),
            Variant::String("a".to_string())
        );
    }

    #[test]
    fn string_names_that_are_not_digit_leading_are_ordinary_members() {
        // The reference's index test is the *first* character
        // (`tjsInterCodeExec.cpp:67`); `TJS_atoi`'s space and sign handling
        // lives inside that branch, so these names are plain misses -- only
        // numeric keys reach the negative index check.
        for (source, name) in [
            (r#"return "abc"["-1x"];"#, "-1x"),
            (r#"return "abc"["-1"];"#, "-1"),
            (r#"return "abc"[" 1"];"#, " 1"),
            (r#"return "abc"["\t1"];"#, "\t1"),
            (r#"return "abc"["+1"];"#, "+1"),
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::MemberNotFound, "{source}");
            assert_eq!(
                error.message,
                format!("Member \"{name}\" does not exist"),
                "{source}"
            );
        }
        // A negative *numeric* key does reach the string reader's range check
        // (`tjsInterCodeExec.cpp:92`).
        let error = failure(r#"return "abc"[-1];"#);
        assert_eq!(error.kind, TjsErrorKind::Runtime);
        assert_eq!(error.message, "The value is out of the range");
    }

    #[test]
    fn string_indices_truncate_to_32_bits() {
        // `(tjs_int)member.AsInteger()` and `TJS_atoi`'s 32-bit accumulator
        // (`tjsConfig.cpp:56`) both wrap, so 2^32 is index 0 either way.
        assert_eq!(
            run(r#"return "abc"[4294967296] + "abc"["4294967296"];"#).expect("wrapped index"),
            Variant::String("aa".to_string())
        );
        assert_eq!(
            run(r#"return "abc"[-0x100000000];"#).expect("wrapped negative key"),
            Variant::String("a".to_string())
        );
    }

    #[test]
    fn string_index_equal_to_the_length_is_empty_and_beyond_it_is_a_range_error() {
        // `GetStringProperty` answers "" for `n == len`
        // (`tjsInterCodeExec.cpp:73`) before its range check.
        assert_eq!(
            run(r#"return "abc"[3];"#).expect("length index"),
            Variant::String(String::new())
        );
        let error = failure(r#"return "abc"[4];"#);
        assert_eq!(error.kind, TjsErrorKind::Runtime);
        assert_eq!(error.message, "The value is out of the range");
    }

    #[test]
    fn non_string_member_keys_report_the_convert_failure() {
        // `member.GetString()` (`tjsVariant.h:790-795`) throws for anything
        // that is not a string variant, so the readers never reach the name
        // rule with a void, object or octet key.
        for (source, rendered) in [
            (r#"return "abc"[void];"#, "(void)"),
            (r#"return "abc"[null];"#, "(object)"),
            ("return (<% 10 20 FF %>)[<% 01 %>];", "(octet)<% 01 %>"),
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::Runtime, "{source}");
            assert_eq!(
                error.message,
                format!("Cannot convert the variable type ({rendered} to string)"),
                "{source}"
            );
        }
        // The write side takes the same branch before the read-only rule.
        let error = failure(r#"return "abc"[void] = 1;"#);
        assert_eq!(error.kind, TjsErrorKind::Runtime);
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((void) to string)"
        );
        let error = failure("var o = <% 10 %>; o[null] = 1;");
        assert_eq!(error.kind, TjsErrorKind::Runtime);
        assert_eq!(
            error.message,
            "Cannot convert the variable type ((object) to string)"
        );
    }

    #[test]
    fn unknown_string_member_reads_report_member_not_found() {
        let error = failure(r#"return "abcd".unknownProp;"#);
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.tjs_error_code(), Some(-1001));
        assert_eq!(error.message, "Member \"unknownProp\" does not exist");
        assert!(error.is_member_not_found());
        // A name that is neither `length` nor an index is a miss even when it
        // is empty (`GetStringProperty` compares the name directly).
        let error = failure(r#"return "abcd"[""];"#);
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"\" does not exist");
    }

    #[test]
    fn string_member_writes_report_access_denied_or_member_not_found() {
        for source in [
            r#"return "abc".length = 1;"#,
            r#"return "abc"[0] = "z";"#,
            r#"return "abc"[1.5] = "z";"#,
            r#"return "abc"[-1] = "z";"#,
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::AccessDenied, "{source}");
            assert_eq!(error.tjs_error_code(), Some(-1007), "{source}");
            assert_eq!(
                error.message, "Invalid operation for Read-only or Write-only property",
                "{source}"
            );
        }
        // A write is not a read: an out-of-range index is still -1007
        // (`SetStringProperty` never checks the index, `tjsInterCodeExec.cpp:115`).
        let error = failure(r#"return "abc"[9] = "z";"#);
        assert_eq!(error.kind, TjsErrorKind::AccessDenied);
        assert_eq!(error.message, "Invalid operation for Read-only or Write-only property");
        // A name that only *looks* like an index is a miss on the write side
        // too (`tjsInterCodeExec.cpp:115-120`).
        for source in [
            r#"return "abc"[" 1"] = "z";"#,
            r#"return "abc"["-1x"] = "z";"#,
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::MemberNotFound, "{source}");
        }
        let error = failure(r#"return "abc".unknownProp = 1;"#);
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"unknownProp\" does not exist");
    }

    #[test]
    fn octet_length_index_and_member_reads_match_the_reference() {
        assert_eq!(
            run("return (<% 10 20 FF %>).length;").expect("length"),
            Variant::Integer(3)
        );
        assert_eq!(
            run("return (<% 10 20 FF %>)[1] + (<% 10 20 FF %>)['2'] + (<% 10 20 FF %>)[1.5];")
                .expect("index"),
            Variant::Integer(32 + 255 + 32)
        );
        // An index equal to the length is out of range for octets, unlike
        // strings (`tjsInterCodeExec.cpp:152`).
        let error = failure("return (<% 10 20 FF %>)[3];");
        assert_eq!(error.kind, TjsErrorKind::Runtime);
        assert_eq!(error.message, "The value is out of the range");
        // A negative *numeric* index reaches the range check; the same text
        // as a name does not (`:160-168`).
        let error = failure("return (<% 10 20 FF %>)[-2];");
        assert_eq!(error.kind, TjsErrorKind::Runtime);
        assert_eq!(error.message, "The value is out of the range");
        for source in [
            "return (<% 10 20 FF %>)['-2x'];",
            "return (<% 10 20 FF %>)[' 1'];",
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::MemberNotFound, "{source}");
        }
        // 32-bit narrowing, as in the string reader.
        assert_eq!(
            run("return (<% 10 20 FF %>)[4294967297];").expect("wrapped index"),
            Variant::Integer(32)
        );
        let error = failure("return (<% 10 20 FF %>).unknownProp;");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"unknownProp\" does not exist");
        let error = failure("return (<% 10 20 FF %>).count;");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"count\" does not exist");
    }

    #[test]
    fn octet_member_writes_report_access_denied_or_member_not_found() {
        for source in [
            "return (<% 10 20 FF %>).length = 1;",
            "return (<% 10 20 FF %>)[0] = 1;",
            "return (<% 10 20 FF %>)[9] = 1;",
            "return (<% 10 20 FF %>)[-1] = 1;",
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::AccessDenied, "{source}");
            assert_eq!(error.tjs_error_code(), Some(-1007), "{source}");
            assert_eq!(
                error.message, "Invalid operation for Read-only or Write-only property",
                "{source}"
            );
        }
        for source in [
            "return (<% 10 20 FF %>).unknownProp = 1;",
            "return (<% 10 20 FF %>)[' 1'] = 1;",
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::MemberNotFound, "{source}");
        }
    }

    #[test]
    fn plain_string_and_octet_writes_reach_the_property_readers() {
        // A plain store goes to `SetStringProperty`/`SetOctetProperty`
        // (`tjsInterCodeExec.cpp:1624-1656`) and reports the official codes
        // there, where `prop_set` used to fall through to `closure_parts` and
        // answer the object conversion error.
        for source in [
            r#"return "abc".length = 1;"#,
            r#"return "abc"[0] = "z";"#,
            "return (<% 10 20 FF %>).length = 1;",
            "return (<% 10 20 FF %>)[0] = 1;",
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::AccessDenied, "{source}");
            assert_eq!(
                error.message, "Invalid operation for Read-only or Write-only property",
                "{source}"
            );
        }
        for source in [
            r#"return "abc"[" 1"] = "z";"#,
            "return (<% 10 20 FF %>)[' 1'] = 1;",
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::MemberNotFound, "{source}");
        }
    }

    #[test]
    fn compound_and_inc_dec_members_convert_the_receiver_first() {
        // The op protocol asks the receiver for its closure *before* touching
        // the member (`AsObjectClosure`, `tjsInterCodeExec.cpp:1816`, `:1842`,
        // `:1925`, `:1950`), so these forms fail with the conversion error and
        // never reach the string/octet property readers -- unlike the plain
        // stores pinned above.
        for (source, rendered) in [
            (r#"return "abc"[0] += "z";"#, "(string)\"abc\""),
            (r#"return "abc"["0"] += "z";"#, "(string)\"abc\""),
            (r#"return "abc".length++;"#, "(string)\"abc\""),
            (r#"return "abc"[0]++;"#, "(string)\"abc\""),
            ("return (<% 10 %>)[0] += 1;", "(octet)<% 10 %>"),
            ("return (<% 10 %>).length++;", "(octet)<% 10 %>"),
            (r#"return 5[0] += 1;"#, "(int)5"),
        ] {
            let error = failure(source);
            assert_eq!(error.kind, TjsErrorKind::Runtime, "{source}");
            assert_eq!(
                error.message,
                format!("Cannot convert the variable type ({rendered} to Object)"),
                "{source}"
            );
        }
        // Object receivers keep working through the same protocol.
        assert_eq!(
            run(r#"var o = %[]; o.x = 1; o.x += 2; o.x++; return o.x;"#).expect("object op"),
            Variant::Integer(4)
        );
        assert_eq!(
            run(r#"var a = [1, 2]; a[0] += 5; a[1]++; return a[0] + a[1];"#).expect("array op"),
            Variant::Integer(9)
        );
    }
}
