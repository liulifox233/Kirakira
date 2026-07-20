//! getSample.dll compatibility stub.
//!
//! The real plugin samples WaveSoundBuffer PCM output, mainly so lip-sync
//! scripts can react to voice playback. Kirakira does not expose decoded
//! sample data, so this stub reports silence: `getSample()` returns 0 and
//! `sampleValue` reads 0.0, which keeps lip-sync scripts idle instead of
//! failing on missing members.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

pub struct GetSamplePlugin;

impl KrkrPlugin for GetSamplePlugin {
    fn name(&self) -> &str {
        "getSample.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_get_sample(runtime);
        Ok(())
    }
}

fn install_get_sample(runtime: &mut Runtime<KrkrHost>) {
    let class = match runtime.global_member("WaveSoundBuffer") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "WaveSoundBuffer");
            runtime.set_global_member("WaveSoundBuffer", Variant::Object(handle));
            handle
        }
    };

    // Class-wide defaults, consulted while an instance has no own value yet.
    for (name, value) in [("sampleCountDefault", 100), ("sampleAheadDefault", 0)] {
        if matches!(runtime.object_member(class, name), Variant::Void) {
            runtime.set_object_member(class, name, Variant::Integer(value));
        }
    }

    // Script-provided members (closures) always win over these natives.
    if !is_script_member(runtime, class, "getSample") {
        runtime.register_object_native(class, "getSample", get_sample);
    }
    if !is_script_member(runtime, class, "setDefaultCounts") {
        runtime.register_object_native(class, "setDefaultCounts", set_default_counts);
    }
    if !is_script_member(runtime, class, "setDefaultAheads") {
        runtime.register_object_native(class, "setDefaultAheads", set_default_aheads);
    }
    if !is_script_member(runtime, class, "sampleValue") {
        runtime.register_object_native_property(
            class,
            "sampleValue",
            |_runtime, _this_obj| Ok(Variant::Real(0.0)),
            // Read-only; assigned values are silently dropped.
            |_runtime, _this_obj, _value| Ok(()),
        );
    }
    if !is_script_member(runtime, class, "sampleCount") {
        register_sample_property(runtime, class, "sampleCount", "sampleCountDefault", 100);
    }
    if !is_script_member(runtime, class, "sampleAhead") {
        register_sample_property(runtime, class, "sampleAhead", "sampleAheadDefault", 0);
    }
}

/// True when `name` was provided by game scripts (a closure) rather than by
/// native code; script overrides must not be replaced.
fn is_script_member(runtime: &Runtime<KrkrHost>, class: ObjectHandle, name: &str) -> bool {
    matches!(runtime.object_member(class, name), Variant::Closure(_))
}

/// Read/write integer property backed by a plain instance member that falls
/// back to the class-object default while the instance has no own value.
fn register_sample_property(
    runtime: &mut Runtime<KrkrHost>,
    class: ObjectHandle,
    name: &'static str,
    default_name: &'static str,
    fallback: i64,
) {
    runtime.register_object_native_property(
        class,
        name,
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                && let Some(value) = integer_member(runtime, this, name)
            {
                return Ok(Variant::Integer(value));
            }
            let default = integer_member(runtime, class, default_name).unwrap_or(fallback);
            Ok(Variant::Integer(default))
        },
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
            {
                let value = value.to_integer()?;
                runtime.set_object_member(this, name, Variant::Integer(value));
            }
            Ok(())
        },
    );
}

fn integer_member(runtime: &Runtime<KrkrHost>, handle: ObjectHandle, name: &str) -> Option<i64> {
    match runtime.object_member(handle, name) {
        Variant::Integer(value) => Some(value),
        Variant::Real(value) => Some(value as i64),
        _ => None,
    }
}

fn get_sample(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn set_default_counts(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    set_class_default(runtime, args, "sampleCountDefault")
}

fn set_default_aheads(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    set_class_default(runtime, args, "sampleAheadDefault")
}

fn set_class_default(
    runtime: &mut Runtime<KrkrHost>,
    args: Vec<Variant>,
    name: &str,
) -> Result<Variant> {
    let value = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0);
    if let Variant::Object(class) = runtime.global_member("WaveSoundBuffer") {
        runtime.set_object_member(class, name, Variant::Integer(value));
    }
    Ok(Variant::Void)
}
