//! Stub for wamsoft `psbfile.dll` (`PSBFile` / `PSBValueClass` classes).
//!
//! The real plugin parses M2/PSB motion files into a value tree. This stub
//! only reports whether the storage exists: `load()` returns 1 with an empty
//! `PSBValueClass` root on success, 0 on failure (games probe files, so it
//! must not throw). Real PSB parsing is future work.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

pub struct PsbFilePlugin;

impl KrkrPlugin for PsbFilePlugin {
    fn name(&self) -> &str {
        "psbfile.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_psb_file_compat(runtime);
        Ok(())
    }
}

fn install_psb_file_compat(runtime: &mut Runtime<KrkrHost>) {
    let value_class = psb_value_constructor(runtime);
    let file_class = psb_file_constructor(runtime);
    runtime.set_global_member("PSBValueClass", Variant::Object(value_class));
    runtime.set_global_member("PSBFile", Variant::Object(file_class));
}

/// Fresh empty `PSBValueClass` instance (`count` == 0).
fn new_psb_value_instance(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let instance = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(instance, "PSBValueClass");
    runtime.set_object_member(instance, "count", Variant::Integer(0));
    instance
}

fn psb_value_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| new_psb_value_instance(runtime));
            runtime.add_object_class_info(instance, "PSBValueClass");
            runtime.set_object_member(instance, "count", Variant::Integer(0));
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "PSBValueClass");
    handle
}

fn psb_file_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "PSBFile");
            install_psb_file_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "PSBFile");
    install_psb_file_members(runtime, handle);
    handle
}

fn install_psb_file_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    if matches!(runtime.object_member(handle, "root"), Variant::Void) {
        runtime.set_object_member(handle, "root", Variant::Void);
    }
    runtime.register_object_native(handle, "load", psb_file_load);
    runtime.register_object_native(handle, "clearStorageCache", native_void);
    runtime.register_object_native(handle, "finalize", native_void);
}

fn psb_file_load(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(Variant::String(storage)) = args.first() else {
        return Ok(Variant::Integer(0));
    };
    match runtime.host().read_binary_storage(storage) {
        Ok(_data) => {
            if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
            {
                let root = new_psb_value_instance(runtime);
                runtime.set_object_member(this, "root", Variant::Object(root));
            }
            Ok(Variant::Integer(1))
        }
        Err(_) => Ok(Variant::Integer(0)),
    }
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}
