use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use crate::bytecode::{BytecodeContextType, BytecodeFile, CodeObject, Instruction};
use crate::debug::{DebugUi, Debugger};
use crate::error::{Result, TjsError};
use crate::vm::{SuspendedCallStack, Vm};

pub(crate) mod builtins;
pub mod object;
pub(crate) mod tjs_ns0;
pub mod value;

pub use self::object::{Object, ObjectKind};
pub use self::value::{Closure, ObjectHandle, Variant};

pub(crate) fn split_delimited_string(
    string: &str,
    delimiters: &str,
    purge_empty: bool,
) -> Vec<Variant> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for ch in string.chars() {
        if delimiters.contains(ch) {
            if !purge_empty || !current.is_empty() {
                parts.push(Variant::String(std::mem::take(&mut current)));
            } else {
                current.clear();
            }
        } else {
            current.push(ch);
        }
    }
    if !purge_empty || !current.is_empty() {
        parts.push(Variant::String(current));
    }
    parts
}

pub(crate) fn split_string_by_regex(
    string: &str,
    regex: &regex::Regex,
    purge_empty: bool,
) -> Vec<Variant> {
    // Matches krkrz `split_regex` semantics (oniguruma FIND_NOT_EMPTY):
    // empty matches are skipped, and the unmatched tail is always emitted
    // unless purged.
    let mut parts = Vec::new();
    let mut start = 0;
    for found in regex.find_iter(string) {
        if found.start() == found.end() {
            continue;
        }
        let piece = &string[start..found.start()];
        if !purge_empty || !piece.is_empty() {
            parts.push(Variant::String(piece.to_string()));
        }
        start = found.end();
    }
    let tail = &string[start..];
    if !purge_empty || !tail.is_empty() {
        parts.push(Variant::String(tail.to_string()));
    }
    parts
}

pub trait TjsHost {
    fn read_text(&mut self, name: &str, _mode: &str) -> Result<String> {
        Err(TjsError::runtime(format!(
            "host text read is not available for `{name}`"
        )))
    }

    fn read_binary(&mut self, name: &str, _mode: &str) -> Result<Vec<u8>> {
        Err(TjsError::runtime(format!(
            "host binary read is not available for `{name}`"
        )))
    }

    fn write_text(&mut self, name: &str, _mode: &str, _text: &str) -> Result<()> {
        Err(TjsError::runtime(format!(
            "host text write is not available for `{name}`"
        )))
    }

    fn write_binary(&mut self, name: &str, _mode: &str, _bytes: &[u8]) -> Result<()> {
        Err(TjsError::runtime(format!(
            "host binary write is not available for `{name}`"
        )))
    }

    fn now_millis(&mut self) -> i64 {
        0
    }

    fn log(&mut self, _message: &str) {}

    fn invalidate_object(&mut self, _handle: ObjectHandle) {}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoHost;

impl TjsHost for NoHost {}

pub trait NativeFunction<H: TjsHost>: Send + Sync {
    fn call(
        &self,
        runtime: &mut Runtime<H>,
        this_obj: Option<ObjectHandle>,
        args: Vec<Variant>,
    ) -> Result<Variant>;
}

impl<H, F> NativeFunction<H> for F
where
    H: TjsHost,
    F: Fn(&mut Runtime<H>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant> + Send + Sync,
{
    fn call(
        &self,
        runtime: &mut Runtime<H>,
        this_obj: Option<ObjectHandle>,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        self(runtime, this_obj, args)
    }
}

pub trait VmNativeFunction<H: TjsHost>: Send + Sync {
    fn call(
        &self,
        vm: &mut Vm<'_, '_, H>,
        this_obj: Option<ObjectHandle>,
        args: Vec<Variant>,
    ) -> Result<Variant>;
}

impl<H, F> VmNativeFunction<H> for F
where
    H: TjsHost,
    F: for<'bc, 'rt> Fn(
            &mut Vm<'bc, 'rt, H>,
            Option<ObjectHandle>,
            Vec<Variant>,
        ) -> Result<Variant>
        + Send
        + Sync,
{
    fn call(
        &self,
        vm: &mut Vm<'_, '_, H>,
        this_obj: Option<ObjectHandle>,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        self(vm, this_obj, args)
    }
}

pub trait NativeProperty<H: TjsHost>: Send + Sync {
    fn get(&self, runtime: &mut Runtime<H>, this_obj: Option<ObjectHandle>) -> Result<Variant>;

    fn set(
        &self,
        runtime: &mut Runtime<H>,
        this_obj: Option<ObjectHandle>,
        value: Variant,
    ) -> Result<()>;
}

struct NativePropertyAccessors<G, S> {
    getter: G,
    setter: S,
}

impl<H, G, S> NativeProperty<H> for NativePropertyAccessors<G, S>
where
    H: TjsHost,
    G: Fn(&mut Runtime<H>, Option<ObjectHandle>) -> Result<Variant> + Send + Sync,
    S: Fn(&mut Runtime<H>, Option<ObjectHandle>, Variant) -> Result<()> + Send + Sync,
{
    fn get(&self, runtime: &mut Runtime<H>, this_obj: Option<ObjectHandle>) -> Result<Variant> {
        (self.getter)(runtime, this_obj)
    }

    fn set(
        &self,
        runtime: &mut Runtime<H>,
        this_obj: Option<ObjectHandle>,
        value: Variant,
    ) -> Result<()> {
        (self.setter)(runtime, this_obj, value)
    }
}

pub struct Runtime<H: TjsHost = NoHost> {
    pub(crate) heap: Vec<Object>,
    pub(crate) global: ObjectHandle,
    pub(crate) script_files: Vec<ScriptFile>,
    pub(crate) native_functions: Vec<Arc<dyn NativeFunction<H>>>,
    pub(crate) vm_native_functions: Vec<Arc<dyn VmNativeFunction<H>>>,
    pub(crate) native_properties: Vec<Arc<dyn NativeProperty<H>>>,
    pub(crate) call_depth: usize,
    pub(crate) max_call_depth: usize,
    pub(crate) suspend_requested: bool,
    pub(crate) suspended_call: Option<SuspendedCallStack>,
    pub(crate) debugger: Option<Debugger>,
    pub(crate) debug_ui: Option<Box<dyn DebugUi<H>>>,
    host: H,
}

#[derive(Clone, Debug)]
pub(crate) struct ScriptFile {
    pub file: Arc<BytecodeFile>,
    pub code_handles: Vec<ObjectHandle>,
    pub decoded_objects: Vec<Option<DecodedScriptObject>>,
}

#[derive(Clone, Debug)]
pub(crate) struct DecodedScriptObject {
    pub object: CodeObject,
    pub instructions: Arc<[Instruction]>,
    pub offset_to_index: Arc<BTreeMap<usize, usize>>,
}

impl Runtime<NoHost> {
    pub fn new() -> Self {
        Self::with_host(NoHost)
    }
}

impl Default for Runtime<NoHost> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: TjsHost + 'static> Runtime<H> {
    pub fn with_host(host: H) -> Self {
        let mut runtime = Self {
            heap: vec![Object::default()],
            global: ObjectHandle(0),
            script_files: Vec::new(),
            native_functions: Vec::new(),
            vm_native_functions: Vec::new(),
            native_properties: Vec::new(),
            call_depth: 0,
            max_call_depth: 1024,
            suspend_requested: false,
            suspended_call: None,
            debugger: None,
            debug_ui: None,
            host,
        };
        builtins::install(&mut runtime);
        runtime
    }

    pub fn global_handle(&self) -> ObjectHandle {
        self.global
    }

    pub fn set_global_member(&mut self, name: impl Into<String>, value: Variant) {
        self.heap[self.global.0].set(name, value);
    }

    pub fn alloc_ordinary_object(&mut self) -> ObjectHandle {
        self.alloc_object(Object::default())
    }

    pub fn alloc_array_object(&mut self, elements: Vec<Variant>) -> ObjectHandle {
        let handle = self.alloc_object(Object::array(elements));
        builtins::install_array_methods(self, handle);
        handle
    }

    /// Allocates a TJS Dictionary complete with its builtin member surface.
    /// Native integrations that materialize structured external data should
    /// use this instead of only attaching Dictionary class metadata.
    pub fn alloc_dictionary_object(&mut self) -> ObjectHandle {
        let handle = self.alloc_ordinary_object();
        builtins::install_dictionary_methods(self, handle);
        handle
    }

    pub fn array_push(&mut self, object: ObjectHandle, value: Variant) -> bool {
        self.heap[object.0].array_push(value)
    }

    pub fn array_insert(&mut self, object: ObjectHandle, index: usize, value: Variant) -> bool {
        self.heap[object.0].array_insert(index, value)
    }

    pub fn array_remove_value(&mut self, object: ObjectHandle, value: &Variant) -> bool {
        self.heap[object.0].array_remove_value(value)
    }

    pub fn array_clear(&mut self, object: ObjectHandle) -> bool {
        self.heap[object.0].array_clear()
    }

    pub fn array_elements(&self, object: ObjectHandle) -> Option<&[Variant]> {
        self.heap.get(object.0)?.array_elements()
    }

    pub fn alloc_native_function<F>(&mut self, function: F) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        self.alloc_native(function, false)
    }

    pub fn alloc_native_constructor<F>(&mut self, function: F) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        self.alloc_native(function, true)
    }

    pub fn alloc_vm_native_function<F>(&mut self, function: F) -> ObjectHandle
    where
        F: VmNativeFunction<H> + 'static,
    {
        self.alloc_vm_native(function)
    }

    pub fn register_global_native<F>(
        &mut self,
        name: impl Into<String>,
        function: F,
    ) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        let handle = self.alloc_native(function, false);
        self.heap[self.global.0].set(name, Variant::Object(handle));
        handle
    }

    pub fn register_object_native<F>(
        &mut self,
        object: ObjectHandle,
        name: impl Into<String>,
        function: F,
    ) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        let handle = self.alloc_native(function, false);
        self.heap[object.0].set(name, Variant::Object(handle));
        handle
    }

    pub fn register_object_vm_native<F>(
        &mut self,
        object: ObjectHandle,
        name: impl Into<String>,
        function: F,
    ) -> ObjectHandle
    where
        F: VmNativeFunction<H> + 'static,
    {
        let handle = self.alloc_vm_native(function);
        self.heap[object.0].set(name, Variant::Object(handle));
        handle
    }

    pub fn register_object_native_property<G, S>(
        &mut self,
        object: ObjectHandle,
        name: impl Into<String>,
        getter: G,
        setter: S,
    ) -> ObjectHandle
    where
        G: Fn(&mut Runtime<H>, Option<ObjectHandle>) -> Result<Variant> + Send + Sync + 'static,
        S: Fn(&mut Runtime<H>, Option<ObjectHandle>, Variant) -> Result<()> + Send + Sync + 'static,
    {
        let handle = self.alloc_native_property(getter, setter);
        self.heap[object.0].set(name, Variant::Object(handle));
        handle
    }

    pub fn global_member(&self, name: &str) -> Variant {
        self.heap[self.global.0].get(name)
    }

    pub fn object_member(&self, object: ObjectHandle, name: &str) -> Variant {
        self.heap[object.0].get(name)
    }

    /// Reads a member through the normal TJS dispatch path, including a
    /// script or native property's getter. `object_member` intentionally
    /// exposes the raw member for VM/runtime bookkeeping; native integrations
    /// that need the value visible to TJS code should use this method.
    pub fn resolve_object_member(&mut self, object: ObjectHandle, name: &str) -> Result<Variant> {
        let file_id = self.call_context_file_id();
        let mut vm = Vm::new(file_id, self)?;
        vm.get_object_member(object, name)
    }

    pub fn object_members(&self, object: ObjectHandle) -> Vec<(String, Variant)> {
        self.heap[object.0].members.clone().into_iter().collect()
    }

    pub fn has_object_member(&self, object: ObjectHandle, name: &str) -> bool {
        self.heap[object.0].get_raw(name).is_some()
    }

    pub fn object_valid(&self, object: ObjectHandle) -> bool {
        self.heap.get(object.0).is_some_and(|object| object.valid)
    }

    pub fn bound_this(&self, object: ObjectHandle) -> Option<ObjectHandle> {
        match self.heap[object.0].kind {
            ObjectKind::Proxy { bind_this, .. } => bind_this,
            _ => None,
        }
    }

    pub fn set_object_member(
        &mut self,
        object: ObjectHandle,
        name: impl Into<String>,
        value: Variant,
    ) {
        self.heap[object.0].set(name, value);
    }

    pub fn set_object_call_missing(
        &mut self,
        object: ObjectHandle,
        missing_name: impl Into<String>,
    ) {
        let object = &mut self.heap[object.0];
        object.missing_name = missing_name.into();
        object.call_missing = !object.missing_name.is_empty();
    }

    pub fn object_member_is_property(&self, object: ObjectHandle, name: &str) -> bool {
        self.heap[object.0]
            .get_raw(name)
            .is_some_and(|value| self.variant_is_property(&value))
    }

    pub fn variant_is_property(&self, value: &Variant) -> bool {
        match value {
            Variant::Closure(closure) => self.object_is_property(closure.object),
            Variant::Object(handle) => self.object_is_property(*handle),
            _ => false,
        }
    }

    pub fn variant_is_native_property(&self, value: &Variant) -> bool {
        match value {
            Variant::Closure(closure) => self.object_is_native_property(closure.object),
            Variant::Object(handle) => self.object_is_native_property(*handle),
            _ => false,
        }
    }

    pub fn variant_is_native_function(&self, value: &Variant) -> bool {
        match value {
            Variant::Closure(closure) => self.object_is_native_function(closure.object),
            Variant::Object(handle) => self.object_is_native_function(*handle),
            _ => false,
        }
    }

    pub fn delete_object_member(&mut self, object: ObjectHandle, name: &str) -> bool {
        self.heap[object.0].delete(name)
    }

    pub fn add_object_class_info(&mut self, object: ObjectHandle, info: impl Into<String>) {
        let info = info.into();
        if info.is_empty()
            || self.heap[object.0]
                .class_infos
                .iter()
                .any(|item| item == &info)
        {
            return;
        }
        self.heap[object.0].class_infos.push(info);
    }

    pub fn object_class_infos(&self, object: ObjectHandle) -> &[String] {
        &self.heap[object.0].class_infos
    }

    pub fn object_super_class(&self, object: ObjectHandle) -> Option<ObjectHandle> {
        self.heap[object.0].super_class
    }

    pub fn set_object_super_class(&mut self, object: ObjectHandle, super_class: ObjectHandle) {
        self.heap[object.0].super_class = Some(super_class);
    }

    pub fn execute_bytecode(&mut self, bytes: &[u8]) -> Result<Variant> {
        let file = BytecodeFile::parse(bytes)?;
        self.execute_file(&file)
    }

    pub fn decode_binary_struct(&mut self, bytes: &[u8]) -> Result<Option<Variant>> {
        builtins::decode_binary_struct(self, bytes)
    }

    pub fn decode_tjs_ns0(&mut self, bytes: &[u8]) -> Result<Option<Variant>> {
        tjs_ns0::decode_tjs_ns0(self, bytes).map(Some)
    }

    pub fn execute_file(&mut self, file: &BytecodeFile) -> Result<Variant> {
        self.execute_file_with_this(file, Some(self.global))
    }

    /// Executes a compiled top-level script with an explicit TJS `this`
    /// context.  Native APIs such as `Scripts.exec` and `Scripts.eval` expose
    /// this as their fourth `context` parameter.
    pub fn execute_file_with_this(
        &mut self,
        file: &BytecodeFile,
        this_obj: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let file_id = self.install_script_file(Arc::new(file.clone()));
        let mut vm = Vm::new(file_id, self)?;
        vm.execute_top_level_with_this(this_obj)
    }

    pub fn request_suspend(&mut self) {
        self.suspend_requested = true;
    }

    /// Enables the interactive debugger and returns it for configuration
    /// (breakpoints, exception breaks, stepping).
    pub fn enable_debugger(&mut self) -> &mut Debugger {
        self.debugger.get_or_insert_with(Debugger::new)
    }

    pub fn debugger(&self) -> Option<&Debugger> {
        self.debugger.as_ref()
    }

    pub fn debugger_mut(&mut self) -> Option<&mut Debugger> {
        self.debugger.as_mut()
    }

    /// Registers the synchronous debug UI invoked whenever execution pauses.
    pub fn set_debug_ui(&mut self, ui: Box<dyn DebugUi<H>>) {
        self.debug_ui = Some(ui);
    }

    /// Takes the debug UI out so the VM/engine can invoke it while a
    /// [`crate::debug::Pause`] holds `&mut Runtime`. Callers must hand it back
    /// via [`Runtime::put_debug_ui`] once the pause ends.
    pub fn take_debug_ui(&mut self) -> Option<Box<dyn DebugUi<H>>> {
        self.debug_ui.take()
    }

    pub fn put_debug_ui(&mut self, ui: Box<dyn DebugUi<H>>) {
        self.debug_ui = Some(ui);
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended_call.is_some()
    }

    pub fn resume_suspended(&mut self) -> Result<Option<Variant>> {
        let Some(call_stack) = self.suspended_call.take() else {
            return Ok(None);
        };
        let file_id = call_stack.resume_file_id().unwrap_or(0);
        let mut vm = Vm::new(file_id, self)?;
        let value = vm.resume_call_stack(call_stack)?;
        if self.is_suspended() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    pub fn call_object_method(
        &mut self,
        object: ObjectHandle,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        let file_id = self.call_context_file_id();
        let mut vm = Vm::new(file_id, self)?;
        vm.call_object_method(object, name, args)
    }

    /// Dispatches a host callback while a VM call is suspended. The callback
    /// must be allowed to mutate host state (for example, closing a modal
    /// window) without consuming the suspended caller; the caller is resumed
    /// by the engine after the event returns.
    pub fn call_object_method_during_suspend(
        &mut self,
        object: ObjectHandle,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        let suspended = self.suspended_call.take();
        let result = self.call_object_method(object, name, args);
        if self.suspended_call.is_none() {
            self.suspended_call = suspended;
        }
        result
    }

    /// Invoke a method declared by a secondary TJS class extender, if one
    /// exists. TJS keeps every extender's class name on the instance even
    /// though the ordinary superclass link can represent only one chain.
    pub fn call_secondary_class_method(
        &mut self,
        object: ObjectHandle,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<bool> {
        let file_id = self.call_context_file_id();
        let mut vm = Vm::new(file_id, self)?;
        vm.call_secondary_class_method(object, name, args)
    }

    pub fn call_variant_method(
        &mut self,
        object: Variant,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<Variant> {
        let file_id = self.call_context_file_id();
        let mut vm = Vm::new(file_id, self)?;
        vm.call_variant_method(object, name, args)
    }

    pub fn call_function(&mut self, callee: Variant, args: Vec<Variant>) -> Result<Variant> {
        let file_id = self.call_context_file_id();
        let mut vm = Vm::new(file_id, self)?;
        vm.call_function(callee, args)
    }

    /// Gives an exception that escaped a VM call to the TJS
    /// `System.exceptionHandler`, matching KRKR2/KRKRZ's event boundary.
    ///
    /// The handler receives a normal TJS object with `message` and `trace`
    /// members.  When the VM retained the class of an escaped `throw`, that
    /// class is attached to the object as well, so script code such as
    /// `e instanceof "ConductorException"` keeps working.
    ///
    /// Returns `true` when the handler exists and returns a truthy value.  A
    /// missing/void handler is not an error and returns `false`; callers can
    /// then propagate the original host error.
    pub fn process_unhandled_exception(&mut self, error: &TjsError) -> Result<bool> {
        // Keep a host-visible diagnostic even when the project's own
        // System.exceptionHandler elects to swallow the failure.  KRKR games
        // commonly do exactly that for UI callbacks, which otherwise leaves
        // the host with only a blank screen and no indication of the failed
        // call site.  `Display` includes member/call and stack contexts.
        self.host_mut()
            .log(&format!("TJS exception at event boundary:\n{error}"));
        let handler = match self.global_member("System") {
            Variant::Object(system) => self.object_member(system, "exceptionHandler"),
            _ => Variant::Void,
        };
        if matches!(handler, Variant::Void | Variant::Null) {
            return Ok(false);
        }

        // The VM keeps the original thrown object alive through the event
        // boundary.  Passing it through preserves custom members and the
        // complete superclass chain exactly as KRKR does.
        if let Some(exception) = error
            .exception_object
            .filter(|handle| self.object_valid(*handle))
        {
            let result = self.call_function(handler, vec![Variant::Object(exception)])?;
            return Ok(result.is_truthy());
        }

        let exception = self.alloc_ordinary_object();
        self.set_object_member(
            exception,
            "message",
            Variant::String(
                error
                    .exception_message
                    .clone()
                    .unwrap_or_else(|| error.message.clone()),
            ),
        );
        let trace = error
            .contexts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        self.set_object_member(exception, "trace", Variant::String(trace));
        if let Some(class) = &error.exception_class {
            self.add_object_class_info(exception, class.clone());
        } else {
            // Native/runtime failures are represented by the standard TJS
            // Exception class so stock KAG error reporters include the
            // message and trace instead of reducing the log to a bare script
            // location.
            self.add_object_class_info(exception, "Exception".to_string());
        }

        let result = self.call_function(handler, vec![Variant::Object(exception)])?;
        Ok(result.is_truthy())
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub(crate) fn enter_call_frame(&mut self) -> Result<()> {
        if self.call_depth >= self.max_call_depth {
            return Err(TjsError::runtime(format!(
                "TJS call stack exceeded {} frames",
                self.max_call_depth
            )));
        }
        self.call_depth += 1;
        Ok(())
    }

    pub(crate) fn leave_call_frame(&mut self) {
        self.call_depth = self.call_depth.saturating_sub(1);
    }

    pub(crate) fn alloc_object(&mut self, object: Object) -> ObjectHandle {
        let handle = ObjectHandle(self.heap.len());
        self.heap.push(object);
        handle
    }

    fn object_is_property(&self, handle: ObjectHandle) -> bool {
        self.heap.get(handle.0).is_some_and(|object| {
            matches!(
                object.kind,
                ObjectKind::InterCode {
                    context: BytecodeContextType::Property,
                    ..
                } | ObjectKind::NativeProperty { .. }
            )
        })
    }

    fn object_is_native_property(&self, handle: ObjectHandle) -> bool {
        self.heap
            .get(handle.0)
            .is_some_and(|object| matches!(object.kind, ObjectKind::NativeProperty { .. }))
    }

    fn object_is_native_function(&self, handle: ObjectHandle) -> bool {
        self.heap.get(handle.0).is_some_and(|object| {
            matches!(
                object.kind,
                ObjectKind::NativeFunction { .. } | ObjectKind::VmNativeFunction { .. }
            )
        })
    }

    pub(crate) fn install_script_file(&mut self, file: Arc<BytecodeFile>) -> usize {
        let file_id = self.script_files.len();
        let mut code_handles = Vec::with_capacity(file.objects.len());
        for (index, object) in file.objects.iter().enumerate() {
            let handle = self.alloc_object(Object::new(ObjectKind::InterCode {
                file_id,
                object_index: index,
                context: object.context_type,
            }));
            if object.context_type == crate::bytecode::BytecodeContextType::Class
                && let Some(name) = object.name(&file)
                && !name.is_empty()
            {
                self.heap[handle.0].class_infos.push(name.to_string());
            }
            code_handles.push(handle);
        }
        self.register_code_object_properties(file.as_ref(), &code_handles);
        self.script_files.push(ScriptFile {
            decoded_objects: vec![None; file.objects.len()],
            file,
            code_handles,
        });
        file_id
    }

    fn register_code_object_properties(
        &mut self,
        file: &BytecodeFile,
        code_handles: &[ObjectHandle],
    ) {
        for (object_index, object) in file.objects.iter().enumerate() {
            if let Some(parent_index) = object.parent {
                let parent_handle = code_handles[parent_index];
                let object_handle = code_handles[object_index];
                let Some(name) = object.name(file).map(str::to_string) else {
                    continue;
                };
                let closure = Variant::Closure(Closure::new(object_handle, Some(parent_handle)));
                self.heap[parent_handle.0].set(name, closure);
            }

            let owner_handle = code_handles[object_index];
            for property in &object.properties {
                let Some(name) = file.data.strings.get(property.name).cloned() else {
                    continue;
                };
                let property_handle = code_handles[property.object];
                let closure = Variant::Closure(Closure::new(property_handle, Some(owner_handle)));
                self.heap[owner_handle.0].set(name, closure);
            }
        }
    }

    fn call_context_file_id(&mut self) -> usize {
        if self.script_files.is_empty() {
            self.install_script_file(Arc::new(BytecodeFile {
                data: Default::default(),
                objects: Vec::new(),
                top_level: None,
                debug_info: Default::default(),
            }))
        } else {
            self.script_files.len() - 1
        }
    }

    pub(crate) fn script_file(&self, file_id: usize) -> Result<Arc<BytecodeFile>> {
        self.script_files
            .get(file_id)
            .map(|script| Arc::clone(&script.file))
            .ok_or_else(|| TjsError::runtime(format!("script file {file_id} does not exist")))
    }

    pub(crate) fn script_code_handles(&self, file_id: usize) -> Result<Vec<ObjectHandle>> {
        self.script_files
            .get(file_id)
            .map(|script| script.code_handles.clone())
            .ok_or_else(|| TjsError::runtime(format!("script file {file_id} does not exist")))
    }

    pub(crate) fn decoded_script_object(
        &mut self,
        file_id: usize,
        object_index: usize,
    ) -> Result<DecodedScriptObject> {
        let script = self
            .script_files
            .get_mut(file_id)
            .ok_or_else(|| TjsError::runtime(format!("script file {file_id} does not exist")))?;
        if let Some(decoded) = script
            .decoded_objects
            .get(object_index)
            .and_then(Option::clone)
        {
            return Ok(decoded);
        }

        let object = script
            .file
            .objects
            .get(object_index)
            .cloned()
            .ok_or_else(|| TjsError::runtime(format!("object {object_index} does not exist")))?;
        let instructions = Arc::<[Instruction]>::from(object.decode_instructions()?);
        let offset_to_index = instructions
            .iter()
            .enumerate()
            .map(|(index, inst)| (inst.offset, index))
            .collect::<BTreeMap<_, _>>();
        let decoded = DecodedScriptObject {
            object,
            instructions,
            offset_to_index: Arc::new(offset_to_index),
        };
        let Some(slot) = script.decoded_objects.get_mut(object_index) else {
            return Err(TjsError::runtime(format!(
                "object {object_index} does not exist"
            )));
        };
        *slot = Some(decoded.clone());
        Ok(decoded)
    }

    pub(crate) fn alloc_proxy_bound(
        &mut self,
        primary: Option<ObjectHandle>,
        fallback: ObjectHandle,
        bind_this: Option<ObjectHandle>,
    ) -> ObjectHandle {
        self.alloc_object(Object::new(ObjectKind::Proxy {
            primary,
            fallback,
            bind_this,
        }))
    }

    pub(crate) fn alloc_native<F>(&mut self, function: F, constructable: bool) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        let id = self.native_functions.len();
        self.native_functions.push(Arc::new(function));
        self.alloc_object(Object::new(ObjectKind::NativeFunction {
            id,
            constructable,
        }))
    }

    pub(crate) fn alloc_vm_native<F>(&mut self, function: F) -> ObjectHandle
    where
        F: VmNativeFunction<H> + 'static,
    {
        let id = self.vm_native_functions.len();
        self.vm_native_functions.push(Arc::new(function));
        self.alloc_object(Object::new(ObjectKind::VmNativeFunction { id }))
    }

    pub(crate) fn alloc_native_property<G, S>(&mut self, getter: G, setter: S) -> ObjectHandle
    where
        G: Fn(&mut Runtime<H>, Option<ObjectHandle>) -> Result<Variant> + Send + Sync + 'static,
        S: Fn(&mut Runtime<H>, Option<ObjectHandle>, Variant) -> Result<()> + Send + Sync + 'static,
    {
        let id = self.native_properties.len();
        self.native_properties
            .push(Arc::new(NativePropertyAccessors { getter, setter }));
        self.alloc_object(Object::new(ObjectKind::NativeProperty { id }))
    }

    pub(crate) fn alloc_value_property(&mut self, initial: Variant) -> ObjectHandle {
        let value = Arc::new(Mutex::new(initial));
        let getter_value = Arc::clone(&value);
        let setter_value = Arc::clone(&value);
        self.alloc_native_property(
            move |_runtime, _this_obj| {
                getter_value
                    .lock()
                    .map(|value| value.clone())
                    .map_err(|_| TjsError::runtime("native value property lock poisoned"))
            },
            move |_runtime, _this_obj, value| {
                *setter_value
                    .lock()
                    .map_err(|_| TjsError::runtime("native value property lock poisoned"))? = value;
                Ok(())
            },
        )
    }
}
