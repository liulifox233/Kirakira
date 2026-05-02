use std::sync::Arc;

use crate::bytecode::{BytecodeContextType, BytecodeFile};
use crate::error::{Result, TjsError};
use crate::vm::Vm;

mod builtins;
pub mod object;
pub mod value;

pub use self::object::{Object, ObjectKind};
pub use self::value::{Closure, ObjectHandle, Variant};

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
    host: H,
}

#[derive(Clone, Debug)]
pub(crate) struct ScriptFile {
    pub file: Arc<BytecodeFile>,
    pub code_handles: Vec<ObjectHandle>,
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

    pub fn execute_file(&mut self, file: &BytecodeFile) -> Result<Variant> {
        let file_id = self.install_script_file(Arc::new(file.clone()));
        let mut vm = Vm::new(file_id, self)?;
        vm.execute_top_level()
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

    pub fn call_function(&mut self, callee: Variant, args: Vec<Variant>) -> Result<Variant> {
        let file_id = self.call_context_file_id();
        let mut vm = Vm::new(file_id, self)?;
        vm.call_function(callee, args)
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
        self.script_files.push(ScriptFile { file, code_handles });
        file_id
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
}
