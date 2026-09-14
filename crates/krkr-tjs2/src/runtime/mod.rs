use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use crate::bytecode::{BytecodeContextType, BytecodeFile, CodeObject, Instruction};
use crate::debug::{DebugUi, Debugger};
use crate::error::{Result, TjsError};
use crate::vm::{JumpTable, SuspendedCallStack, Vm};

pub(crate) mod builtins;
pub mod object;
pub(crate) mod symbol_table;
/// The `TJS/` data-pack container: the 16-byte header and the seeded,
/// check-byte-carrying value stream (`PackinOne.dll`'s `tjsDataPack`).
pub mod tjs_ns0;
pub mod value;

#[cfg(test)]
mod array_tests;
#[cfg(test)]
mod dictionary_tests;

pub use self::object::{NativeArgCount, NativePropertyAccess, Object, ObjectKind};
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

/// `Array.load`'s line split (`tjsArray.cpp:276-327`): a `\n`, a `\r` or a
/// `\r\n` pair ends a line, and the file's lines become the elements in order.
/// A separator at the end of the file adds nothing -- the reference only
/// counts the trailing segment when it is non-empty (`:319-327`) -- but a
/// separator with an empty segment before it *does* contribute an element, so
/// `"\n"` is one empty line while `"a\nb"` is two.
pub(crate) fn split_loaded_lines(text: &str) -> Vec<Variant> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte != b'\r' && byte != b'\n' {
            index += 1;
            continue;
        }
        lines.push(Variant::String(text[start..index].to_string()));
        index += if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            2
        } else {
            1
        };
        start = index;
    }
    if start < bytes.len() {
        lines.push(Variant::String(text[start..].to_string()));
    }
    lines
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

    /// The objects a native entity registered for finalization with itself --
    /// the reference's `tTJSNI_BaseWindow::ObjectVector`, filled by
    /// `Window.Add` (`WindowIntf.cpp:705-711`) and locked once the window
    /// starts invalidating (`:223`).  `Window::Invalidate` finalizes every one
    /// of them in registration order when the window dies (`:222-243`), and
    /// only the VM can run the TJS half of `_Finalize` (the object's
    /// `finalize` member), so the list is taken from the host when `handle` is
    /// invalidated and drained.
    fn take_registered_objects(&mut self, _handle: ObjectHandle) -> Vec<ObjectHandle> {
        Vec::new()
    }
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

/// Runtime-armable tracing of calls into native (Rust) methods.
///
/// Native methods are the boundary where a script's intent becomes engine
/// geometry, so they are where a wrong value first becomes visible.  Hunting
/// such a bug used to mean adding `eprintln!`s to the engine and rebuilding,
/// which throws away a live session — expensive when reaching the repro takes
/// tens of thousands of frames.  Keeping the filter in the runtime instead lets
/// a debugger console arm and disarm traces on a running game.
#[derive(Debug, Default)]
pub(crate) struct NativeCallTrace {
    /// Qualified `Class.method` names, indexed by native function id.
    native_names: Vec<Option<String>>,
    /// Same, indexed by VM-native function id.
    vm_native_names: Vec<Option<String>>,
    /// Lowercased patterns; empty means tracing is off.
    patterns: Vec<String>,
}

impl NativeCallTrace {
    fn matches(&self, name: &str) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let lowered = name.to_ascii_lowercase();
        self.patterns.iter().any(|pattern| {
            // `copyRect` matches `Layer.copyRect`, `Layer.` matches every
            // Layer method, and `Layer.copyRect` matches exactly.
            lowered == *pattern
                || lowered.ends_with(&format!(".{pattern}"))
                || lowered.starts_with(pattern)
        })
    }
}

pub struct Runtime<H: TjsHost = NoHost> {
    pub(crate) heap: Vec<Object>,
    pub(crate) global: ObjectHandle,
    pub(crate) script_files: Vec<ScriptFile>,
    pub(crate) native_functions: Vec<Arc<dyn NativeFunction<H>>>,
    pub(crate) vm_native_functions: Vec<Arc<dyn VmNativeFunction<H>>>,
    pub(crate) native_properties: Vec<Arc<dyn NativeProperty<H>>>,
    pub(crate) native_call_trace: NativeCallTrace,
    pub(crate) call_depth: usize,
    pub(crate) max_call_depth: usize,
    pub(crate) suspend_requested: bool,
    pub(crate) suspended_call: Option<SuspendedCallStack>,
    pub(crate) debugger: Option<Debugger>,
    pub(crate) debug_ui: Option<Box<dyn DebugUi<H>>>,
    /// `LastRehashedTick` of the engine's event loop
    /// (`SystemControl.cpp:176-180`): the wall-clock tick of the last
    /// `TJSDoRehash`, so [`Runtime::tjs_rehash_tick`] can reproduce the
    /// reference's 1500 ms cadence.
    pub(crate) last_rehash_tick: i64,
    host: H,
}

#[derive(Clone, Debug)]
pub(crate) struct ScriptFile {
    pub file: Arc<BytecodeFile>,
    pub code_handles: Arc<[ObjectHandle]>,
    pub decoded_objects: Vec<Option<DecodedScriptObject>>,
}

#[derive(Clone, Debug)]
pub(crate) struct DecodedScriptObject {
    pub object: CodeObject,
    pub instructions: Arc<[Instruction]>,
    pub offset_to_index: Arc<BTreeMap<usize, usize>>,
    /// Precomputed sequential-next and branch-target indices for
    /// `instructions`; see [`JumpTable`].
    pub jump: Arc<JumpTable>,
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
            native_call_trace: NativeCallTrace::default(),
            call_depth: 0,
            max_call_depth: 1024,
            suspend_requested: false,
            suspended_call: None,
            debugger: None,
            debug_ui: None,
            last_rehash_tick: 0,
            host,
        };
        builtins::install(&mut runtime);
        runtime
    }

    /// `TJSDoRehash()` (`tjsObject.cpp:362`): mark every object's member table
    /// stale.  Each object rebuilds lazily on its next member read
    /// (`:1396-1398`).  This is what `Scripts.rehash` calls, and what the
    /// reference's 1500 ms event-loop tick calls.
    pub fn tjs_do_rehash(&mut self) {
        symbol_table::tjs_do_rehash();
    }

    /// The engine's rehash tick: `if(!ContinuousEventCalling && tick >
    /// LastRehashedTick + 1500) { LastRehashedTick = tick; TJSDoRehash(); }`
    /// (`SystemControl.cpp:176-180`; krkr2 trunk runs the same rule from its
    /// window message loop, `MainFormUnit.cpp:713-719`).
    ///
    /// The reference measures `tick` in milliseconds since startup.
    pub fn tjs_rehash_tick(&mut self, now_millis: i64) {
        if now_millis > self.last_rehash_tick + symbol_table::REHASH_TICK_INTERVAL_MILLIS {
            self.last_rehash_tick = now_millis;
            self.tjs_do_rehash();
        }
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

    /// Allocates a TJS Dictionary: an ordinary object carrying the `Dictionary`
    /// class name and nothing else.  Native integrations that materialize
    /// structured external data should use this instead of only attaching
    /// Dictionary class metadata.
    ///
    /// The object deliberately gets no method members: the reference registers
    /// every Dictionary method with `TJS_STATICMEMBER`, so a Dictionary
    /// instance has an empty member map until script code fills it, and its
    /// methods are only reachable as
    /// `(Dictionary.method incontextof instance)(...)`.
    pub fn alloc_dictionary_object(&mut self) -> ObjectHandle {
        let handle = self.alloc_ordinary_object();
        self.add_object_class_info(handle, "Dictionary");
        handle
    }

    /// A Dictionary whose member table is sized for `count` members, the way
    /// `tTJSDictionaryClass::CreateNew` builds one
    /// (`tjsDictionary.cpp:244-257`): an integer count of 8 or more picks the
    /// bucket count from the size formula, anything else leaves the default
    /// 8 buckets.  This is how `tTJSBinarySerializer::CreateDictionary` builds
    /// every dictionary of a deserialized struct except the root
    /// (`tjsBinarySerializer.cpp:90-98`).
    pub fn alloc_dictionary_object_sized(&mut self, count: i64) -> ObjectHandle {
        let handle = self.alloc_dictionary_object();
        if count >= 8 {
            self.heap[handle.0].members.rebuild(count);
        }
        handle
    }

    /// True when `object` is a TJS `Dictionary` *instance*.
    ///
    /// The dispatch overrides of `tTJSDictionaryObject` -- a miss reading as
    /// void (`tjsDictionary.cpp:720-731`) above all -- belong to the object
    /// `tTJSDictionaryClass::CreateBaseTJSObject` builds
    /// (`tjsDictionary.cpp:235-238`), not to the `Dictionary` class object:
    /// that one is a `tTJSNativeClass`, i.e. a plain `tTJSCustomObject` for
    /// every protocol it does not override (`tjsNative.h`), so a miss on
    /// `Dictionary.whatever` raises `Member "%1" does not exist`.
    ///
    /// The receiver therefore has to be a constructed object -- a native
    /// function, a `vm-native` function or a script class object is a class
    /// object, never an instance whatever class names it carries -- whose own
    /// class info or whose class chain names `Dictionary`.
    pub fn is_dictionary_instance(&self, object: ObjectHandle) -> bool {
        if matches!(
            self.heap[object.0].kind,
            ObjectKind::NativeFunction { .. }
                | ObjectKind::VmNativeFunction { .. }
                | ObjectKind::InterCode {
                    context: BytecodeContextType::Class,
                    ..
                }
        ) {
            return false;
        }
        let mut current = Some(object);
        while let Some(object) = current {
            if self.heap[object.0]
                .class_infos
                .iter()
                .any(|info| info == "Dictionary")
            {
                return true;
            }
            current = self.object_super_class(object);
        }
        false
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
        self.alloc_native(function, false, NativeArgCount::Any)
    }

    /// Registers a native function together with its declared argument-count
    /// contract; a call that breaks it fails with `TJS_E_BADPARAMCOUNT`
    /// (-1004) before the handler runs.
    pub fn alloc_native_function_with_arg_count<F>(
        &mut self,
        arg_count: NativeArgCount,
        function: F,
    ) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        self.alloc_native(function, false, arg_count)
    }

    pub fn alloc_native_constructor<F>(&mut self, function: F) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        self.alloc_native(function, true, NativeArgCount::Any)
    }

    /// See [`Runtime::alloc_native_function_with_arg_count`].
    pub fn alloc_native_constructor_with_arg_count<F>(
        &mut self,
        arg_count: NativeArgCount,
        function: F,
    ) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        self.alloc_native(function, true, arg_count)
    }

    pub fn alloc_vm_native_function<F>(&mut self, function: F) -> ObjectHandle
    where
        F: VmNativeFunction<H> + 'static,
    {
        self.alloc_vm_native(function, NativeArgCount::Any)
    }

    /// See [`Runtime::alloc_native_function_with_arg_count`].
    pub fn alloc_vm_native_function_with_arg_count<F>(
        &mut self,
        arg_count: NativeArgCount,
        function: F,
    ) -> ObjectHandle
    where
        F: VmNativeFunction<H> + 'static,
    {
        self.alloc_vm_native(function, arg_count)
    }

    pub fn register_global_native<F>(
        &mut self,
        name: impl Into<String>,
        function: F,
    ) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        self.register_global_native_with_arg_count(name, NativeArgCount::Any, function)
    }

    /// See [`Runtime::alloc_native_function_with_arg_count`].
    pub fn register_global_native_with_arg_count<F>(
        &mut self,
        name: impl Into<String>,
        arg_count: NativeArgCount,
        function: F,
    ) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        let handle = self.alloc_native(function, false, arg_count);
        let name = name.into();
        self.name_native_call(handle, self.global, &name);
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
        self.register_object_native_with_arg_count(object, name, NativeArgCount::Any, function)
    }

    /// See [`Runtime::alloc_native_function_with_arg_count`]. Native methods
    /// whose official declaration validates `numparams` belong here, so the
    /// check is expressed once at the registration site instead of being
    /// repeated inside every handler.
    pub fn register_object_native_with_arg_count<F>(
        &mut self,
        object: ObjectHandle,
        name: impl Into<String>,
        arg_count: NativeArgCount,
        function: F,
    ) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        let handle = self.alloc_native(function, false, arg_count);
        let name = name.into();
        self.name_native_call(handle, object, &name);
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
        self.register_object_vm_native_with_arg_count(object, name, NativeArgCount::Any, function)
    }

    /// See [`Runtime::register_object_native_with_arg_count`].
    pub fn register_object_vm_native_with_arg_count<F>(
        &mut self,
        object: ObjectHandle,
        name: impl Into<String>,
        arg_count: NativeArgCount,
        function: F,
    ) -> ObjectHandle
    where
        F: VmNativeFunction<H> + 'static,
    {
        let handle = self.alloc_vm_native(function, arg_count);
        let name = name.into();
        self.name_native_call(handle, object, &name);
        self.heap[object.0].set(name, Variant::Object(handle));
        handle
    }

    /// Records the `Class.method` name a freshly allocated native function is
    /// about to be bound to, so [`set_native_call_traces`] can address it.
    fn name_native_call(&mut self, function: ObjectHandle, owner: ObjectHandle, name: &str) {
        let qualified = match self.heap[owner.0].class_infos.last() {
            Some(class) if owner != self.global => format!("{class}.{name}"),
            _ => name.to_string(),
        };
        match self.heap[function.0].kind {
            ObjectKind::NativeFunction { id, .. } => {
                if let Some(slot) = self.native_call_trace.native_names.get_mut(id) {
                    *slot = Some(qualified);
                }
            }
            ObjectKind::VmNativeFunction { id, .. } => {
                if let Some(slot) = self.native_call_trace.vm_native_names.get_mut(id) {
                    *slot = Some(qualified);
                }
            }
            _ => {}
        }
    }

    /// Arms native call tracing. Each pattern is matched case-insensitively
    /// against a native method's `Class.method` name: `copyRect` traces the
    /// method on every class, `Layer.` traces all of `Layer`, and
    /// `Layer.copyRect` traces exactly one. An empty list disables tracing.
    pub fn set_native_call_traces<I, S>(&mut self, patterns: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.native_call_trace.patterns = patterns
            .into_iter()
            .map(|pattern| pattern.as_ref().trim().to_ascii_lowercase())
            .filter(|pattern| !pattern.is_empty())
            .collect();
    }

    pub fn native_call_traces(&self) -> &[String] {
        &self.native_call_trace.patterns
    }

    /// Every `Class.method` name that [`set_native_call_traces`] can address,
    /// so a console can offer completion or check a pattern for typos.
    pub fn traceable_native_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .native_call_trace
            .native_names
            .iter()
            .chain(self.native_call_trace.vm_native_names.iter())
            .flatten()
            .cloned()
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Logs one traced native call through the host log, where the debugger's
    /// `logs` command can find it.
    pub(crate) fn trace_native_call(
        &mut self,
        name: &str,
        this_obj: Option<ObjectHandle>,
        args: &[Variant],
    ) {
        let this = this_obj
            .map(|handle| self.describe_traced_object(handle))
            .unwrap_or_else(|| "void".to_string());
        let args = args
            .iter()
            .map(|value| self.describe_traced_value(value))
            .collect::<Vec<_>>()
            .join(", ");
        self.host
            .log(&format!("native call {name} this={this} args=[{args}]"));
    }

    fn describe_traced_value(&self, value: &Variant) -> String {
        match value {
            Variant::Object(handle) => self.describe_traced_object(*handle),
            Variant::Closure(closure) => format!("closure#{}", closure.object.0),
            Variant::String(text) if text.chars().count() > 48 => {
                let head: String = text.chars().take(48).collect();
                format!("{head:?}…")
            }
            other => other.to_string(),
        }
    }

    fn describe_traced_object(&self, handle: ObjectHandle) -> String {
        match self.heap[handle.0].class_infos.last() {
            Some(class) => format!("{class}#{}", handle.0),
            None => format!("#{}", handle.0),
        }
    }

    pub(crate) fn native_call_name(&self, id: usize, vm_native: bool) -> Option<&str> {
        let names = if vm_native {
            &self.native_call_trace.vm_native_names
        } else {
            &self.native_call_trace.native_names
        };
        let name = names.get(id)?.as_deref()?;
        self.native_call_trace.matches(name).then_some(name)
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
        self.register_object_native_property_with_access(
            object,
            name,
            NativePropertyAccess::ReadWrite,
            getter,
            setter,
        )
    }

    /// Registers a native property with an explicit script access policy.
    ///
    /// A denied direction never reaches the accessor: a script read of a
    /// `WriteOnly` property and a script write to a `ReadOnly` one fail with
    /// `TJS_E_ACCESSDENYED` (-1007) and the official text, exactly as the
    /// reference's `TJS_DENY_NATIVE_PROP_SETTER` properties do. The accessors
    /// stay in place for the engine's own use — host writes go through
    /// [`Runtime::set_object_member`], which does not consult the policy.
    pub fn register_object_native_property_with_access<G, S>(
        &mut self,
        object: ObjectHandle,
        name: impl Into<String>,
        access: NativePropertyAccess,
        getter: G,
        setter: S,
    ) -> ObjectHandle
    where
        G: Fn(&mut Runtime<H>, Option<ObjectHandle>) -> Result<Variant> + Send + Sync + 'static,
        S: Fn(&mut Runtime<H>, Option<ObjectHandle>, Variant) -> Result<()> + Send + Sync + 'static,
    {
        let handle = self.alloc_native_property_with_access(getter, setter, access);
        self.heap[object.0].set(name, Variant::Object(handle));
        handle
    }

    /// The script access policy of a native property object, or `None` when
    /// `property` is not one.
    pub fn native_property_access(&self, property: ObjectHandle) -> Option<NativePropertyAccess> {
        match self.heap.get(property.0).map(|object| &object.kind) {
            Some(ObjectKind::NativeProperty { access, .. }) => Some(*access),
            _ => None,
        }
    }

    /// Changes the script access policy of an existing native property.
    /// Returns false when `property` is not a native property.
    pub fn set_native_property_access(
        &mut self,
        property: ObjectHandle,
        access: NativePropertyAccess,
    ) -> bool {
        let Some(object) = self.heap.get_mut(property.0) else {
            return false;
        };
        let ObjectKind::NativeProperty {
            access: current, ..
        } = &mut object.kind
        else {
            return false;
        };
        *current = access;
        true
    }

    /// Declares a deny-list of read-only properties on `object`, the shape the
    /// reference writes as `TJS_DENY_NATIVE_PROP_SETTER` in its class
    /// registration — `Layer`, `Window`, `System` and the rest each carry one.
    ///
    /// Every named member that is a native property becomes
    /// [`NativePropertyAccess::ReadOnly`]: script keeps its getter and a script
    /// write fails with `TJS_E_ACCESSDENYED` (-1007), while the engine's own
    /// writes (host-side member stores) are unaffected. Marking a namespace
    /// after the class is assembled is what makes the list declarative: the
    /// registration code does not have to repeat the policy for every
    /// property.
    ///
    /// Returns the names that were not a native property of `object`, so the
    /// caller can assert its deny-list still matches the class.
    ///
    /// The denial applies to the two store shapes as the reference has them:
    /// a plain `obj.prop = v` meets the denied setter and fails with -1007,
    /// while a store carrying `TJS_IGNOREPROP` (`&obj.prop = v`,
    /// regmember-style) skips the property object entirely and overwrites the
    /// member (`tTJSCustomObject::PropSet`, `tjsObject.cpp:1519-1541`), which
    /// is how KAGEX replaces `Layer.font` with its font hook.
    pub fn deny_native_property_writes(
        &mut self,
        object: ObjectHandle,
        names: &[&str],
    ) -> Vec<String> {
        let mut unmatched = Vec::new();
        for name in names {
            // The member is the property object, which an install that
            // preserves script properties stores as a self-bound value; read
            // the object behind the binding.
            let member = self.heap[object.0].get_raw(name);
            let Some(handle) = member.as_ref().and_then(Variant::object_handle) else {
                unmatched.push((*name).to_string());
                continue;
            };
            if !self.set_native_property_access(handle, NativePropertyAccess::ReadOnly) {
                unmatched.push((*name).to_string());
            }
        }
        unmatched
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
    ///
    /// A missing member reads back as `void` the way the C++ side of KRKR sees
    /// `TJS_E_MEMBERNOTFOUND` from `PropGet`; script-level reads go through the
    /// VM opcodes instead and raise `Member "%1" does not exist`
    /// (`tTJSCustomObject::PropGet`, `tjsError.cpp:240-244`).
    pub fn resolve_object_member(&mut self, object: ObjectHandle, name: &str) -> Result<Variant> {
        self.resolve_object_member_lenient(object, name)
    }

    fn resolve_object_member_lenient(
        &mut self,
        object: ObjectHandle,
        name: &str,
    ) -> Result<Variant> {
        let file_id = self.call_context_file_id();
        let mut vm = Vm::new(file_id, self)?;
        vm.get_object_member_probe(object, name)
    }

    pub fn object_members(&self, object: ObjectHandle) -> Vec<(String, Variant)> {
        self.heap[object.0].member_entries()
    }

    pub fn has_object_member(&self, object: ObjectHandle, name: &str) -> bool {
        self.heap[object.0].get_raw(name).is_some()
    }

    /// Whether calling this object would dispatch to code. Lets an inspector
    /// separate an object's methods from the data members that describe its
    /// state.
    pub fn object_is_callable(&self, object: ObjectHandle) -> bool {
        self.heap.get(object.0).is_some_and(|object| {
            matches!(
                object.kind,
                ObjectKind::InterCode { .. }
                    | ObjectKind::NativeFunction { .. }
                    | ObjectKind::VmNativeFunction { .. }
            )
        })
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

    /// Decodes a `TJS/ns0` body — the serialized values plus their trailing
    /// 4-byte final check — with the container header's seed and byte order.
    /// The caller handles the transform chain in front of the body (IV,
    /// LZ4 framing, ChaCha), which `Scripts.loadDataPack` does.
    pub fn decode_tjs_ns0_body(
        &mut self,
        payload: &[u8],
        seed: u32,
        big_endian: bool,
    ) -> Result<Variant> {
        tjs_ns0::decode_tjs_ns0_body(self, payload, seed, big_endian)
    }

    /// Serializes `value` as a `TJS/ns0` body with the header's seed and byte
    /// order, appending the trailing final check.
    pub fn encode_tjs_ns0_body(
        &self,
        value: &Variant,
        seed: u32,
        big_endian: bool,
    ) -> Result<Vec<u8>> {
        tjs_ns0::encode_tjs_ns0_body(self, value, seed, big_endian)
    }

    pub fn execute_file(&mut self, file: &BytecodeFile) -> Result<Variant> {
        self.execute_file_with_this(file, Some(self.global))
    }

    /// Executes a compiled top-level script with an explicit TJS `this`
    /// context.  Native APIs such as `Scripts.exec` and `Scripts.eval` expose
    /// this as their fourth `context` parameter, and `None` models the NULL
    /// context of their omitted/void argument (`base/ScriptMgnIntf.cpp:1283-1341`):
    /// because the file is a top-level context, `tTJSInterCodeContext::FuncCall`
    /// substitutes the global object for it
    /// (`tjsInterCodeExec.cpp:3083-3087`), so `None` still runs the script
    /// with `this` = the global object.  A *function* only reads `this` as the
    /// null object when it is dispatched directly with a NULL context
    /// (`:3089-3099`, `:839`, reached at `:3063`, `:3135`, `:3172`); a
    /// variant/closure call first substitutes the callee object itself
    /// (`tjsVariant.h:226-232`).
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

    /// Invoke the first script class body in an instance's recorded
    /// construction order.  This is used for native events which are
    /// delivered to the owning object (for example Layer.onPaint): normal
    /// member lookup must give the most-derived script extender first refusal,
    /// while the secondary-class helper intentionally walks from the base for
    /// `SUPER`-style fallbacks.
    pub fn call_primary_class_method(
        &mut self,
        object: ObjectHandle,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<bool> {
        let file_id = self.call_context_file_id();
        let mut vm = Vm::new(file_id, self)?;
        vm.call_primary_class_method(object, name, args)
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

    /// Whether reading this member goes through a native getter. Such a member
    /// stores an accessor rather than the value, so an inspector has to resolve
    /// it to show anything meaningful.
    pub fn object_is_native_property(&self, handle: ObjectHandle) -> bool {
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
            code_handles: code_handles.into(),
        });
        file_id
    }

    /// Publishes each code object's `properties` table the way
    /// `tTJSByteCodeLoader` does: every entry is registered on the object's
    /// *parent* context. The official compiler records `(Name, this)` on a
    /// method, property, or nested class whose parent is a function or class,
    /// so this is what turns a class object into a member table its
    /// `regmember` can copy onto instances, and what makes
    /// `Outer.Inner` resolve on the class object itself.
    ///
    /// Nothing else is registered here: top-level declarations reach the
    /// global object through the top-level code (`spds`), expression
    /// functions and superclass getters are anonymous, and property accessors
    /// hang off their property object. Registering every child by name used
    /// to leave a class object carrying its own superclass getter under the
    /// class name, and `(anonymous)` members that `regmember` then copied onto
    /// every instance.
    fn register_code_object_properties(
        &mut self,
        file: &BytecodeFile,
        code_handles: &[ObjectHandle],
    ) {
        for object in &file.objects {
            if object.properties.is_empty() {
                continue;
            }
            let Some(parent_index) = object.parent else {
                continue;
            };
            let parent_handle = code_handles[parent_index];
            for property in &object.properties {
                let Some(name) = file.data.strings.get(property.name).cloned() else {
                    continue;
                };
                let Some(member_handle) = code_handles.get(property.object).copied() else {
                    continue;
                };
                // `tTJSByteCodeLoader::ReadObjects` registers `val =
                // objs[pobj]` -- a bare object variant whose ObjThis stays
                // NULL -- with `PropSet(TJS_MEMBERENSURE|TJS_IGNOREPROP, ...,
                // obj)`.  A class's members therefore carry no receiver of
                // their own: instances get bound copies from `regmember`
                // (`ChangeClosureObjThis(Dest)`), and a call that reaches the
                // member through the class object runs on the caller's `this`
                // (`TJS_SELECT_OBJTHIS`).  Binding here instead would hand
                // `PreRenderFontEx.KAGLayerFinalizer(...)` -- the KAGEX font
                // plugin calling the `finalize` it saved off `KAGLayer` -- the
                // class object as `this`, where the layer's own members are
                // not reachable.
                let closure = Variant::Closure(Closure::new(member_handle, None));
                self.heap[parent_handle.0].set(name, closure);
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

    pub(crate) fn script_code_handles(&self, file_id: usize) -> Result<Arc<[ObjectHandle]>> {
        self.script_files
            .get(file_id)
            .map(|script| Arc::clone(&script.code_handles))
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
        let jump = JumpTable::build(&instructions, &offset_to_index);
        let decoded = DecodedScriptObject {
            object,
            instructions,
            offset_to_index: Arc::new(offset_to_index),
            jump: Arc::new(jump),
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

    pub(crate) fn alloc_native<F>(
        &mut self,
        function: F,
        constructable: bool,
        arg_count: NativeArgCount,
    ) -> ObjectHandle
    where
        F: NativeFunction<H> + 'static,
    {
        let id = self.native_functions.len();
        self.native_functions.push(Arc::new(function));
        self.native_call_trace.native_names.push(None);
        self.alloc_object(Object::new(ObjectKind::NativeFunction {
            id,
            constructable,
            arg_count,
        }))
    }

    pub(crate) fn alloc_vm_native<F>(
        &mut self,
        function: F,
        arg_count: NativeArgCount,
    ) -> ObjectHandle
    where
        F: VmNativeFunction<H> + 'static,
    {
        let id = self.vm_native_functions.len();
        self.vm_native_functions.push(Arc::new(function));
        self.native_call_trace.vm_native_names.push(None);
        self.alloc_object(Object::new(ObjectKind::VmNativeFunction { id, arg_count }))
    }

    pub(crate) fn alloc_native_property<G, S>(&mut self, getter: G, setter: S) -> ObjectHandle
    where
        G: Fn(&mut Runtime<H>, Option<ObjectHandle>) -> Result<Variant> + Send + Sync + 'static,
        S: Fn(&mut Runtime<H>, Option<ObjectHandle>, Variant) -> Result<()> + Send + Sync + 'static,
    {
        self.alloc_native_property_with_access(getter, setter, NativePropertyAccess::ReadWrite)
    }

    pub(crate) fn alloc_native_property_with_access<G, S>(
        &mut self,
        getter: G,
        setter: S,
        access: NativePropertyAccess,
    ) -> ObjectHandle
    where
        G: Fn(&mut Runtime<H>, Option<ObjectHandle>) -> Result<Variant> + Send + Sync + 'static,
        S: Fn(&mut Runtime<H>, Option<ObjectHandle>, Variant) -> Result<()> + Send + Sync + 'static,
    {
        let id = self.native_properties.len();
        self.native_properties
            .push(Arc::new(NativePropertyAccessors { getter, setter }));
        self.alloc_object(Object::new(ObjectKind::NativeProperty { id, access }))
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
