use std::{collections::BTreeMap, marker::PhantomData, sync::Arc};

use crate::bytecode::{BytecodeContextType, BytecodeFile, CodeObject, Instruction};
use crate::debug::{LocationKey, Pause, StopReason};
use crate::error::{Result, TjsError, TjsErrorKind, TjsSourceLocation, TjsStackFrame};
use crate::runtime::{
    Closure, NativeFunction, NoHost, ObjectHandle, ObjectKind, Runtime, TjsHost, Variant,
};

mod dispatch;
mod frame;
mod opcode;

pub(crate) use frame::Frame;
pub(crate) use frame::SuspendedCallStack;
use frame::{CallFrame, Continuation, ExceptionEntry};
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
    no_bound_instance_fallback: bool,
}

pub struct Vm<'bc, 'rt, H: TjsHost = NoHost> {
    file_id: usize,
    file: Arc<BytecodeFile>,
    runtime: &'rt mut Runtime<H>,
    code_handles: Vec<ObjectHandle>,
    // A mixin class initializes its members by executing several class bodies
    // against the same instance.  Some of those bodies (notably the
    // `__missing` mixin) enable missing-member dispatch before a later body
    // has declared its own fields.  Keep a nesting count so field
    // initializers can still create members while any class body is active.
    class_initialization_depth: BTreeMap<usize, usize>,
    _file_lifetime: PhantomData<&'bc BytecodeFile>,
}

impl<'bc, 'rt, H: TjsHost + 'static> Vm<'bc, 'rt, H> {
    pub fn new(file_id: usize, runtime: &'rt mut Runtime<H>) -> Result<Self> {
        let file = runtime.script_file(file_id)?;
        let code_handles = runtime.script_code_handles(file_id)?;
        let vm = Self {
            file_id,
            file,
            runtime,
            code_handles,
            class_initialization_depth: BTreeMap::new(),
            _file_lifetime: PhantomData,
        };
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

    pub(super) fn begin_class_initialization(&mut self, instance: ObjectHandle) {
        *self
            .class_initialization_depth
            .entry(instance.0)
            .or_default() += 1;
    }

    pub(super) fn end_class_initialization(&mut self, instance: ObjectHandle) {
        let Some(depth) = self.class_initialization_depth.get_mut(&instance.0) else {
            return;
        };
        if *depth <= 1 {
            self.class_initialization_depth.remove(&instance.0);
        } else {
            *depth -= 1;
        }
    }

    pub(super) fn is_class_initializing(&self, instance: ObjectHandle) -> bool {
        self.class_initialization_depth
            .get(&instance.0)
            .is_some_and(|depth| *depth != 0)
    }

    pub fn runtime(&self) -> &Runtime<H> {
        self.runtime
    }

    pub fn runtime_mut(&mut self) -> &mut Runtime<H> {
        self.runtime
    }

    pub fn execute_top_level(&mut self) -> Result<Variant> {
        self.execute_top_level_with_this(Some(self.runtime.global))
    }

    pub(crate) fn execute_top_level_with_this(
        &mut self,
        this_obj: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let index = self
            .file
            .top_level
            .ok_or_else(|| TjsError::runtime("bytecode has no top-level object"))?;
        self.execute_object_with_this(index, Vec::new(), this_obj)
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
        let base_depth = self.runtime.call_depth;
        let frame =
            self.create_call_frame(file_id, object_index, args, this_obj, Continuation::Root)?;
        self.run_call_stack(vec![frame], base_depth)
    }

    pub(super) fn execute_file_object_with_this_at(
        &mut self,
        file_id: usize,
        object_index: usize,
        code_offset: usize,
        args: Vec<Variant>,
        this_obj: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let base_depth = self.runtime.call_depth;
        let mut frame =
            self.create_call_frame(file_id, object_index, args, this_obj, Continuation::Root)?;
        frame.pc = frame
            .offset_to_index
            .get(&code_offset)
            .copied()
            .ok_or_else(|| {
                TjsError::runtime(format!("invalid bytecode entry offset {code_offset}"))
            })?;
        self.run_call_stack(vec![frame], base_depth)
    }

    pub(super) fn execute_object_with_this(
        &mut self,
        object_index: usize,
        args: Vec<Variant>,
        this_obj: Option<ObjectHandle>,
    ) -> Result<Variant> {
        self.execute_file_object_with_this(self.file_id, object_index, args, this_obj)
    }

    pub(crate) fn resume_call_stack(&mut self, call_stack: SuspendedCallStack) -> Result<Variant> {
        self.run_call_stack(call_stack.stack, call_stack.base_depth)
    }

    fn create_call_frame(
        &mut self,
        file_id: usize,
        object_index: usize,
        args: Vec<Variant>,
        this_obj: Option<ObjectHandle>,
        continuation: Continuation,
    ) -> Result<CallFrame> {
        let file = self.runtime.script_file(file_id)?;
        let code_handles = self.runtime.script_code_handles(file_id)?;
        let decoded = self.runtime.decoded_script_object(file_id, object_index)?;
        self.runtime.enter_call_frame().map_err(|error| {
            error.with_stack_frame(self.stack_frame_for(&file, &decoded.object, 0))
        })?;
        let result = (|| {
            let caller_args = args.clone();
            let global = self.runtime.global;
            let this_obj = this_obj.or(Some(global));
            let this_proxy = self.runtime.alloc_proxy_bound(this_obj, global, None);
            let mut frame = Frame::new(&decoded.object, args, this_obj, this_proxy)?;
            if let Some(collapse_base) = decoded.object.func_decl_collapse_base {
                let base = collapse_base as usize;
                let rest = if caller_args.len() > base {
                    caller_args[base..].to_vec()
                } else {
                    Vec::new()
                };
                let array = self.runtime.alloc_array_object(rest);
                frame.set(-3 - collapse_base as i16, Variant::Object(array))?;
            }
            let pc = decoded
                .offset_to_index
                .get(&(0_usize))
                .copied()
                .unwrap_or(0);
            Ok(CallFrame {
                file_id,
                file,
                object_handle: code_handles
                    .get(object_index)
                    .copied()
                    .ok_or_else(|| TjsError::runtime(format!("object {object_index} missing")))?,
                code_handles,
                object: decoded.object,
                instructions: decoded.instructions,
                offset_to_index: decoded.offset_to_index,
                frame,
                pc,
                continuation,
            })
        })();
        if result.is_err() {
            self.runtime.leave_call_frame();
        }
        result
    }

    fn run_call_stack(&mut self, mut stack: Vec<CallFrame>, base_depth: usize) -> Result<Variant> {
        let result = loop {
            let Some(mut call_frame) = stack.pop() else {
                break Err(TjsError::runtime("VM call stack completed without a value"));
            };
            self.activate_call_frame(&call_frame);
            if call_frame.pc >= call_frame.instructions.len() {
                let value = call_frame.frame.result.clone();
                self.runtime.leave_call_frame();
                match self.complete_call_value(value, call_frame.continuation, &mut stack) {
                    Ok(Some(value)) => break Ok(value),
                    Ok(None) => continue,
                    Err(error) => break Err(error),
                }
            }

            let pc = call_frame.pc;
            let inst = call_frame.instructions[pc].clone();
            let next_pc = match next_instruction_index(
                &call_frame.offset_to_index,
                &call_frame.instructions,
                pc,
            ) {
                Ok(next_pc) => next_pc,
                Err(error) => {
                    break Err(self.with_active_stack(
                        error,
                        &stack,
                        Some((&call_frame, inst.offset)),
                    ));
                }
            };

            if self.runtime.debugger.is_some()
                && let Err(error) = self.debug_pre_execute(&mut call_frame, &stack, &inst)
            {
                break Err(error);
            }

            match self.execute_instruction(
                &call_frame.object,
                call_frame.object_handle,
                &mut call_frame.frame,
                &inst,
                next_pc,
                &call_frame.offset_to_index,
            ) {
                Ok(Step::Next(next)) => {
                    call_frame.pc = next;
                    if self.runtime.suspended_call.is_some() {
                        // A native host method may invoke a nested bytecode
                        // file (for example Scripts.execStorage). If that
                        // nested VM suspended on a remote resource, keep the
                        // complete caller stack below it so resume continues
                        // after the native call instead of losing any outer
                        // execution context. This matters when a resource
                        // load occurs inside a helper function: preserving
                        // only the current frame would drop that helper's
                        // caller and eventually leave an empty VM stack.
                        // Property getters are executed synchronously by the
                        // dispatch layer.  If such a getter starts an
                        // asynchronous resource load, `prop_get` returns a
                        // temporary `void` value while the getter's call
                        // stack is parked.  Re-enter the same property-get
                        // instruction after the nested stack resumes so the
                        // caller observes the getter's actual result (rather
                        // than passing `void` to the next instruction).  A
                        // native call, on the other hand, has already
                        // committed its side effects and should continue at
                        // the next instruction as before.
                        if matches!(inst.opcode, 103 | 107 | 115) {
                            call_frame.pc = pc;
                        }
                        self.merge_nested_suspend(&mut stack, call_frame);
                        break Ok(Variant::Void);
                    }
                    stack.push(call_frame);
                }
                Ok(Step::Return(value)) => {
                    self.runtime.leave_call_frame();
                    match self.complete_call_value(value, call_frame.continuation, &mut stack) {
                        Ok(Some(value)) => break Ok(value),
                        Ok(None) => {}
                        Err(error) => break Err(error),
                    }
                }
                Ok(Step::Call { frame, resume_pc }) => {
                    call_frame.pc = resume_pc;
                    stack.push(call_frame);
                    stack.push(*frame);
                }
                Ok(Step::Suspend { resume_pc }) => {
                    let object_name = call_frame
                        .object
                        .name(&call_frame.file)
                        .unwrap_or("<anonymous>");
                    self.runtime.host_mut().log(&format!(
                        "TJS VM suspend at file={} object={} pc={} resumePc={}",
                        self.file_id, object_name, pc, resume_pc
                    ));
                    call_frame.pc = resume_pc;
                    stack.push(call_frame);
                    self.runtime.suspended_call = Some(SuspendedCallStack { stack, base_depth });
                    break Ok(Variant::Void);
                }
                Err(error) => {
                    // A direct member call can resolve a lazy property getter
                    // before invoking the returned value.  When that getter
                    // suspends on an external resource, the dispatch layer
                    // observes a temporary `void` callee and reports
                    // "void is not callable".  Park the caller and retry the
                    // complete call instruction once the getter's nested
                    // stack has resumed, just like the property-read path
                    // above.  Native calls do not return an error while a
                    // nested resource stack is parked, so this is limited to
                    // the call opcodes.
                    if self.runtime.suspended_call.is_some() && matches!(inst.opcode, 99..=102) {
                        call_frame.pc = pc;
                        self.merge_nested_suspend(&mut stack, call_frame);
                        break Ok(Variant::Void);
                    }
                    if error.kind == TjsErrorKind::ResourcePending {
                        let object_name = call_frame
                            .object
                            .name(&call_frame.file)
                            .unwrap_or("<anonymous>");
                        self.runtime.host_mut().log(&format!(
                            "TJS VM resource pending at file={} object={} pc={}",
                            self.file_id, object_name, pc
                        ));
                        // Re-run the same instruction after the host has
                        // completed the asynchronous resource request. This
                        // is deliberately below native-call dispatch so a
                        // host read is not treated as a catchable script
                        // exception.
                        stack.push(call_frame);
                        self.runtime.suspended_call =
                            Some(SuspendedCallStack { stack, base_depth });
                        break Ok(Variant::Void);
                    }
                    // A debug-session quit must never become a catchable TJS
                    // exception.
                    if error.kind == TjsErrorKind::DebugQuit {
                        break Err(error);
                    }
                    let error = error.with_stack_frame(self.stack_frame_for(
                        &call_frame.file,
                        &call_frame.object,
                        inst.offset,
                    ));
                    if self
                        .runtime
                        .debugger
                        .as_ref()
                        .is_some_and(|debugger| debugger.break_on_exception())
                    {
                        let caught = !call_frame.frame.entries.is_empty()
                            || stack.iter().any(|frame| !frame.frame.entries.is_empty());
                        let reason = StopReason::Exception {
                            caught,
                            // Keep the structured call/member and stack
                            // context in debugger output. Printing only the
                            // leaf message (`void is not callable`) hides
                            // which callback or method supplied the bad
                            // callee.
                            message: error.to_string(),
                        };
                        let object_index = match self.runtime.heap.get(call_frame.object_handle.0) {
                            Some(object) => match object.kind {
                                ObjectKind::InterCode { object_index, .. } => Some(object_index),
                                _ => None,
                            },
                            None => None,
                        };
                        if let Err(quit) = self.debug_pause(
                            &mut call_frame,
                            &stack,
                            reason,
                            inst.offset,
                            object_index,
                        ) {
                            break Err(quit);
                        }
                    }
                    match self.unwind_to_catch(error, call_frame, &mut stack) {
                        Ok(()) => {}
                        Err(error) => break Err(error),
                    }
                }
            }
        };
        if result.is_err() {
            self.runtime.call_depth = base_depth;
        }
        result
    }

    fn unwind_to_catch(
        &mut self,
        mut error: TjsError,
        mut call_frame: CallFrame,
        stack: &mut Vec<CallFrame>,
    ) -> Result<()> {
        // krkrz executes try-blocks so that runtime errors raised inside
        // them (including errors from nested calls) are converted into
        // exception objects and execution resumes at the catch address.
        loop {
            if let Some(entry) = call_frame.frame.entries.pop() {
                let exception = self.make_runtime_exception(&error);
                call_frame.frame.set(entry.exception_reg, exception)?;
                call_frame.pc = entry.catch_pc;
                stack.push(call_frame);
                return Ok(());
            }
            let offset = call_frame
                .instructions
                .get(call_frame.pc)
                .map(|inst| inst.offset)
                .unwrap_or(0);
            error = error.with_stack_frame(self.stack_frame_for(
                &call_frame.file,
                &call_frame.object,
                offset,
            ));
            self.runtime.leave_call_frame();
            let Some(caller) = stack.pop() else {
                return Err(error);
            };
            call_frame = caller;
        }
    }

    fn make_runtime_exception(&mut self, error: &TjsError) -> Variant {
        let handle = self.runtime.alloc_object(Default::default());
        self.runtime.heap[handle.0].set("message", Variant::String(error.message.clone()));
        // KRKR exposes VM-generated failures to script catch blocks as an
        // `Exception` object, not as an untyped dictionary.  Keeping the
        // class marker and the unwound context on that object is important
        // for compatibility with system exception reporters: they use
        // `instanceof "Exception"` before printing `trace`.
        self.runtime
            .add_object_class_info(handle, "Exception".to_string());
        let trace = error
            .contexts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        self.runtime.heap[handle.0].set("trace", Variant::String(trace));
        Variant::Object(handle)
    }

    fn complete_call_value(
        &mut self,
        value: Variant,
        continuation: Continuation,
        stack: &mut Vec<CallFrame>,
    ) -> Result<Option<Variant>> {
        match continuation {
            Continuation::Root => Ok(Some(value)),
            Continuation::CallerRegister { dest } => {
                if let Some(dest) = dest
                    && let Some(caller) = stack.last_mut()
                {
                    caller.frame.set(dest, value)?;
                }
                Ok(None)
            }
            Continuation::ReturnFixed { value, target } => {
                self.complete_call_value(value, *target, stack)
            }
            Continuation::ClassBody {
                instance,
                class_handle,
                class_name,
                constructor_args,
                run_constructor,
                target,
            } => {
                self.end_class_initialization(instance);
                for info in self.runtime.heap[class_handle.0].class_infos.clone() {
                    self.add_class_info(instance, info);
                }
                // A plain class-body call is how KRKR initializes a parent
                // class (and how mixinclass.tjs probes a class on its
                // temporary __missing object).  That call must not replace
                // the instance's existing superclass link: doing so makes
                // the probe's deleted `finalize` resolve to the dynamic
                // wrapper finalizer.  The `new` entry point owns the leaf
                // instance link, while native constructors may install
                // their own native superclass instance as they run.
                if run_constructor {
                    self.runtime.heap[instance.0].super_class = Some(class_handle);
                }
                let object_value = Variant::Object(instance);
                if run_constructor
                    && !class_name.is_empty()
                    && let Some(constructor) = self.runtime.heap[instance.0].get_raw(&class_name)
                    && !matches!(constructor, Variant::Void)
                {
                    match self.call_value(
                        constructor,
                        Some(instance),
                        constructor_args,
                        false,
                        Continuation::ReturnFixed {
                            value: object_value,
                            target,
                        },
                    )? {
                        CallOutcome::Immediate(value, continuation) => {
                            return self.complete_call_value(value, continuation, stack);
                        }
                        CallOutcome::Frame(frame) => {
                            stack.push(*frame);
                            return Ok(None);
                        }
                    }
                }
                self.complete_call_value(object_value, *target, stack)
            }
        }
    }

    fn activate_call_frame(&mut self, frame: &CallFrame) {
        self.file_id = frame.file_id;
        self.file = Arc::clone(&frame.file);
        self.code_handles = frame.code_handles.clone();
    }

    fn with_active_stack(
        &self,
        mut error: TjsError,
        callers: &[CallFrame],
        current: Option<(&CallFrame, usize)>,
    ) -> TjsError {
        if let Some((frame, offset)) = current {
            error =
                error.with_stack_frame(self.stack_frame_for(&frame.file, &frame.object, offset));
        }
        for frame in callers.iter().rev() {
            let offset = frame
                .instructions
                .get(frame.pc)
                .map(|inst| inst.offset)
                .unwrap_or_else(|| {
                    frame
                        .instructions
                        .last()
                        .map(|inst| inst.offset)
                        .unwrap_or(0)
                });
            error =
                error.with_stack_frame(self.stack_frame_for(&frame.file, &frame.object, offset));
        }
        error
    }

    /// Debugger hook run before each instruction dispatch. Cheap fast path:
    /// returns immediately when no stop condition matches.
    fn debug_pre_execute(
        &mut self,
        call_frame: &mut CallFrame,
        stack: &[CallFrame],
        inst: &Instruction,
    ) -> Result<()> {
        let depth = stack.len() + 1;
        let object_index = match self.runtime.heap.get(call_frame.object_handle.0) {
            Some(object) => match object.kind {
                ObjectKind::InterCode { object_index, .. } => Some(object_index),
                _ => None,
            },
            None => None,
        };
        let Some(debugger) = self.runtime.debugger.as_mut() else {
            return Ok(());
        };
        let Some(reason) = debugger.check_tjs(
            &call_frame.file,
            call_frame.file_id,
            object_index,
            &call_frame.object,
            inst.offset,
            inst.opcode,
            depth,
        ) else {
            return Ok(());
        };
        self.debug_pause(call_frame, stack, reason, inst.offset, object_index)
    }

    /// Builds the backtrace, invokes the registered debug UI synchronously,
    /// and applies the returned action to the stepping state machine.
    fn debug_pause(
        &mut self,
        call_frame: &mut CallFrame,
        stack: &[CallFrame],
        reason: StopReason,
        offset: usize,
        object_index: Option<usize>,
    ) -> Result<()> {
        let Some(mut ui) = self.runtime.debug_ui.take() else {
            return Ok(());
        };
        let depth = stack.len() + 1;
        let mut backtrace = Vec::with_capacity(depth);
        backtrace.push(self.stack_frame_for(&call_frame.file, &call_frame.object, offset));
        for caller in stack.iter().rev() {
            let caller_offset = caller
                .instructions
                .get(caller.pc)
                .map(|inst| inst.offset)
                .unwrap_or_else(|| {
                    caller
                        .instructions
                        .last()
                        .map(|inst| inst.offset)
                        .unwrap_or(0)
                });
            backtrace.push(self.stack_frame_for(&caller.file, &caller.object, caller_offset));
        }
        let key: Option<LocationKey> = self
            .source_location_for(&call_frame.file, &call_frame.object, offset)
            .map(|location| {
                (
                    call_frame.file_id,
                    location.line.or(location.utf16_offset).unwrap_or(0),
                )
            });
        let action = {
            let mut pause = Pause::new_tjs(
                reason,
                self.runtime,
                &mut call_frame.frame,
                Arc::clone(&call_frame.file),
                object_index,
                backtrace,
            );
            ui.on_pause(&mut pause)
        };
        self.runtime.debug_ui = Some(ui);
        let Some(debugger) = self.runtime.debugger.as_mut() else {
            return Ok(());
        };
        debugger.apply_action(action, key, depth)
    }

    fn merge_nested_suspend(&mut self, stack: &mut Vec<CallFrame>, outer: CallFrame) {
        let Some(mut suspended) = self.runtime.suspended_call.take() else {
            return;
        };
        if let Some(root) = suspended.stack.first_mut() {
            root.continuation = Continuation::CallerRegister { dest: None };
        }
        // `stack` is ordered bottom-to-top and already contains callers of
        // `outer`; append the current frame and the nested suspended stack so
        // the normal pop/resume loop can unwind every frame in order.
        stack.push(outer);
        stack.append(&mut suspended.stack);
        suspended.stack = std::mem::take(stack);
        self.runtime.suspended_call = Some(suspended);
    }

    fn execute_instruction(
        &mut self,
        object: &CodeObject,
        object_handle: ObjectHandle,
        frame: &mut Frame,
        inst: &Instruction,
        next_pc: usize,
        offset_to_index: &BTreeMap<usize, usize>,
    ) -> Result<Step> {
        let mut pc = next_pc;
        match inst.opcode {
            0 | 127 => {}
            1 => {
                let mut value = self.data_slot_value(object, inst.operands[1])?;
                if let Variant::Closure(closure) = &mut value
                    && closure.this_obj.is_none()
                    && frame.this_obj != Some(self.runtime.global)
                    && matches!(
                        self.runtime.heap[closure.object.0].kind,
                        ObjectKind::InterCode {
                            context: BytecodeContextType::ExprFunction,
                            ..
                        }
                    )
                {
                    // Expression functions created inside an object method
                    // retain that context. Top-level expression functions
                    // remain unbound so assigning one to an object member can
                    // bind ObjThis to that destination, as krkr does.
                    closure.this_obj = frame.this_obj;
                }
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
            86 | 87 => self.eval_operator(frame, inst.operands[0], inst.opcode == 86)?,
            88 => {
                let class_name = frame.get(inst.operands[1])?.to_tjs_string()?;
                let value = self.instance_of(&frame.get(inst.operands[0])?, &class_name)?;
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
                    Ok(handle) => Variant::Integer(i64::from(self.invalidate_object(handle)?)),
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
                let continuation = Continuation::CallerRegister {
                    dest: (inst.operands[0] != 0).then_some(inst.operands[0]),
                };
                match self.call_value(
                    callee,
                    frame.this_obj,
                    args,
                    inst.opcode == 102,
                    continuation,
                )? {
                    CallOutcome::Immediate(value, Continuation::CallerRegister { dest }) => {
                        if let Some(dest) = dest {
                            frame.set(dest, value)?;
                        }
                        if self.runtime.suspend_requested {
                            self.runtime.suspend_requested = false;
                            return Ok(Step::Suspend { resume_pc: next_pc });
                        }
                    }
                    CallOutcome::Immediate(_, continuation) => {
                        return Err(TjsError::runtime(format!(
                            "unexpected immediate call continuation {continuation:?}"
                        )));
                    }
                    CallOutcome::Frame(call_frame) => {
                        return Ok(Step::Call {
                            frame: call_frame,
                            resume_pc: next_pc,
                        });
                    }
                }
            }
            100 => {
                let object_value = frame.get(inst.operands[1])?;
                let name = self.data_slot_string(object, inst.operands[2])?;
                let args = self.materialize_call_args(frame, object, inst.call_args.as_ref())?;
                let continuation = Continuation::CallerRegister {
                    dest: (inst.operands[0] != 0).then_some(inst.operands[0]),
                };
                match self.call_member_direct_cont(
                    object_value,
                    &name,
                    args,
                    frame.this_obj,
                    continuation,
                )? {
                    CallOutcome::Immediate(value, Continuation::CallerRegister { dest }) => {
                        if let Some(dest) = dest {
                            frame.set(dest, value)?;
                        }
                        if self.runtime.suspend_requested {
                            self.runtime.suspend_requested = false;
                            return Ok(Step::Suspend { resume_pc: next_pc });
                        }
                    }
                    CallOutcome::Immediate(_, continuation) => {
                        return Err(TjsError::runtime(format!(
                            "unexpected immediate member call continuation {continuation:?}"
                        )));
                    }
                    CallOutcome::Frame(call_frame) => {
                        return Ok(Step::Call {
                            frame: call_frame,
                            resume_pc: next_pc,
                        });
                    }
                }
            }
            101 => {
                let object_value = frame.get(inst.operands[1])?;
                let name = self.key_from_variant(&frame.get(inst.operands[2])?)?;
                let args = self.materialize_call_args(frame, object, inst.call_args.as_ref())?;
                let continuation = Continuation::CallerRegister {
                    dest: (inst.operands[0] != 0).then_some(inst.operands[0]),
                };
                match self.call_member_direct_cont(
                    object_value,
                    &name,
                    args,
                    frame.this_obj,
                    continuation,
                )? {
                    CallOutcome::Immediate(value, Continuation::CallerRegister { dest }) => {
                        if let Some(dest) = dest {
                            frame.set(dest, value)?;
                        }
                        if self.runtime.suspend_requested {
                            self.runtime.suspend_requested = false;
                            return Ok(Step::Suspend { resume_pc: next_pc });
                        }
                    }
                    CallOutcome::Immediate(_, continuation) => {
                        return Err(TjsError::runtime(format!(
                            "unexpected immediate member call continuation {continuation:?}"
                        )));
                    }
                    CallOutcome::Frame(call_frame) => {
                        return Ok(Step::Call {
                            frame: call_frame,
                            resume_pc: next_pc,
                        });
                    }
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
                    return Err(self.uncaught_exception_error(&thrown));
                };
                frame.set(entry.exception_reg, thrown)?;
                pc = entry.catch_pc;
            }
            123 => {
                let mut value = frame.get(inst.operands[0])?;
                let this = self.optional_object(frame.get(inst.operands[1])?)?;
                self.change_this(&mut value, this)?;
                frame.set(inst.operands[0], value)?;
            }
            124 => frame.set(inst.operands[0], Variant::Object(self.runtime.global))?,
            125 => {
                let object_handle = self.resolve_object(frame.get(inst.operands[0])?)?;
                let info = frame.get(inst.operands[1])?;
                if let Ok(getter_handle) = self.resolve_object(info.clone())
                    && matches!(
                        self.runtime.heap[object_handle.0].kind,
                        ObjectKind::InterCode {
                            context: BytecodeContextType::Class,
                            ..
                        }
                    )
                    && matches!(
                        self.runtime.heap[getter_handle.0].kind,
                        ObjectKind::InterCode {
                            context: BytecodeContextType::SuperClassGetter,
                            ..
                        }
                    )
                {
                    let Some(instance) = frame.this_obj else {
                        return Err(TjsError::runtime(
                            "class extender has no destination this object",
                        ));
                    };
                    match self.apply_class_extender(
                        object_handle,
                        getter_handle,
                        instance,
                        Continuation::CallerRegister { dest: None },
                    )? {
                        CallOutcome::Immediate(_, Continuation::CallerRegister { dest: None }) => {}
                        CallOutcome::Immediate(_, continuation) => {
                            return Err(TjsError::runtime(format!(
                                "unexpected class extender continuation {continuation:?}"
                            )));
                        }
                        CallOutcome::Frame(call_frame) => {
                            return Ok(Step::Call {
                                frame: call_frame,
                                resume_pc: next_pc,
                            });
                        }
                    }
                    return Ok(Step::Next(pc));
                }
                self.add_class_info(object_handle, info.to_tjs_string()?);
            }
            126 => {
                let Some(dest) = frame.this_obj else {
                    return Err(TjsError::runtime(
                        "regmember has no destination this object",
                    ));
                };
                self.register_object_members(object_handle, dest);
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
            crate::runtime::ObjectKind::NativeProperty { .. } => "NativeProperty".to_string(),
        };
        if !object.class_infos.is_empty() {
            label.push('<');
            label.push_str(&object.class_infos.join("|"));
            label.push('>');
        }
        format!("{label}#{}", handle.0)
    }

    fn format_uncaught_exception(&self, value: &Variant) -> String {
        let mut text = value.to_string();
        let handle = match value {
            Variant::Object(handle) => Some(*handle),
            Variant::Closure(closure) => Some(closure.object),
            _ => None,
        };
        let Some(handle) = handle else {
            return text;
        };
        let Some(object) = self.runtime.heap.get(handle.0) else {
            return text;
        };
        if !object.class_infos.is_empty() {
            text.push_str(" class=");
            text.push_str(&object.class_infos.join("|"));
        }
        let details = ["name", "message", "trace"]
            .into_iter()
            .filter_map(|name| {
                let value = self.raw_object_member(handle, name)?;
                let value = value.to_tjs_string().ok()?;
                if value.is_empty() {
                    None
                } else {
                    Some(format!("{name}={value:?}"))
                }
            })
            .collect::<Vec<_>>();
        if !details.is_empty() {
            text.push_str(" (");
            text.push_str(&details.join(", "));
            text.push(')');
        }
        text
    }

    /// Looks up diagnostic fields on an exception object without entering the
    /// VM again.  Thrown objects can be proxies or script instances whose
    /// `message`/`trace` members live on a superclass; inspecting only the
    /// object's own map used to reduce those failures to `<object #N>`.
    fn raw_object_member(&self, handle: ObjectHandle, name: &str) -> Option<Variant> {
        fn visit<H: TjsHost + 'static>(
            runtime: &Runtime<H>,
            handle: ObjectHandle,
            name: &str,
            seen: &mut Vec<usize>,
        ) -> Option<Variant> {
            if seen.contains(&handle.0) {
                return None;
            }
            seen.push(handle.0);
            let object = runtime.heap.get(handle.0)?;
            if let Some(value) = object.get_raw(name) {
                return Some(value);
            }
            if let ObjectKind::Proxy {
                primary, fallback, ..
            } = object.kind
            {
                if let Some(primary) = primary
                    && let Some(value) = visit(runtime, primary, name, seen)
                {
                    return Some(value);
                }
                if let Some(value) = visit(runtime, fallback, name, seen) {
                    return Some(value);
                }
            }
            object
                .super_class
                .and_then(|parent| visit(runtime, parent, name, seen))
        }

        // Keep the helper allocation-free in the common case.  The tiny
        // visited list only grows for proxy/superclass chains.
        let mut seen = Vec::new();
        visit(&self.runtime, handle, name, &mut seen)
    }

    fn uncaught_exception_error(&mut self, value: &Variant) -> TjsError {
        let text = self.format_uncaught_exception(value);
        let mut error = TjsError::runtime(format!("uncaught exception {text}"));
        let handle = match value {
            Variant::Object(handle) => Some(*handle),
            Variant::Closure(closure) => Some(closure.object),
            _ => None,
        };
        if let Some(handle) = handle {
            error.exception_object = Some(handle);
            if let Some(class) = self.runtime.heap.get(handle.0) {
                if let Some(class_name) = class.class_infos.first() {
                    error = error.with_exception_class(class_name.clone());
                }
                if let Some(message) = class
                    .get_raw("message")
                    .and_then(|value| value.to_tjs_string().ok())
                    .filter(|message| !message.is_empty())
                {
                    error = error.with_exception_message(message);
                }
            }
        }
        error
    }

    fn stack_frame_for(
        &self,
        file: &BytecodeFile,
        object: &CodeObject,
        bytecode_offset: usize,
    ) -> TjsStackFrame {
        let storage = file
            .debug_info
            .sources
            .first()
            .map(|source| source.name.clone());
        TjsStackFrame {
            storage,
            object_name: object.name(file).unwrap_or("<anonymous>").to_string(),
            context: format!("{:?}", object.context_type),
            bytecode_offset,
            source: self.source_location_for(file, object, bytecode_offset),
        }
    }

    fn source_location_for(
        &self,
        file: &BytecodeFile,
        object: &CodeObject,
        bytecode_offset: usize,
    ) -> Option<TjsSourceLocation> {
        let position = object
            .source_positions
            .iter()
            .take_while(|position| position.code_pos as usize <= bytecode_offset)
            .last()?;
        let source = file.debug_info.sources.first();
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

    fn no_bound_instance_fallback() -> Self {
        Self {
            no_bound_instance_fallback: true,
            ..Self::default()
        }
    }
}

enum Step {
    Next(usize),
    Return(Variant),
    Call {
        frame: Box<CallFrame>,
        resume_pc: usize,
    },
    Suspend {
        resume_pc: usize,
    },
}

pub(in crate::vm) enum CallOutcome {
    Immediate(Variant, Continuation),
    Frame(Box<CallFrame>),
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
