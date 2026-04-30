use std::sync::Arc;

use crate::bytecode::BytecodeFile;
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

pub struct Runtime<H: TjsHost = NoHost> {
    pub(crate) heap: Vec<Object>,
    pub(crate) global: ObjectHandle,
    pub(crate) script_files: Vec<ScriptFile>,
    pub(crate) native_functions: Vec<Arc<dyn NativeFunction<H>>>,
    pub(crate) vm_native_functions: Vec<Arc<dyn VmNativeFunction<H>>>,
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
            call_depth: 0,
            max_call_depth: 32,
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
        let mut object = Object::array(elements);
        object.class_infos.push("Array".to_string());
        self.alloc_object(object)
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

    pub fn global_member(&self, name: &str) -> Variant {
        self.heap[self.global.0].get(name)
    }

    pub fn object_member(&self, object: ObjectHandle, name: &str) -> Variant {
        self.heap[object.0].get(name)
    }

    pub fn set_object_member(
        &mut self,
        object: ObjectHandle,
        name: impl Into<String>,
        value: Variant,
    ) {
        self.heap[object.0].set(name, value);
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

    pub fn execute_bytecode(&mut self, bytes: &[u8]) -> Result<Variant> {
        let file = BytecodeFile::parse(bytes)?;
        self.execute_file(&file)
    }

    pub fn execute_file(&mut self, file: &BytecodeFile) -> Result<Variant> {
        let file_id = self.install_script_file(Arc::new(file.clone()));
        let mut vm = Vm::new(file_id, self)?;
        vm.execute_top_level()
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
}
