//! AlphaMovie.dll compatibility stub (kaede-software alpha movie).
//!
//! The real plugin plays video files that carry a separate alpha channel.
//! This stub only validates that the movie file is readable from storage and
//! then reports a finished one-frame movie (`numOfFrame = 1`, `frame = 0`),
//! so polling TJS wrappers immediately see playback as complete and terminate
//! their wait loops.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

pub struct AlphaMoviePlugin;

impl KrkrPlugin for AlphaMoviePlugin {
    fn name(&self) -> &str {
        "AlphaMovie.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_alpha_movie(runtime);
        Ok(())
    }
}

fn install_alpha_movie(runtime: &mut Runtime<KrkrHost>) {
    // Keep a script-provided AlphaMovie class untouched.
    if matches!(runtime.global_member("AlphaMovie"), Variant::Object(_)) {
        return;
    }
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "AlphaMovie");
            install_alpha_movie_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "AlphaMovie");
    install_alpha_movie_members(runtime, handle);
    runtime.set_global_member("AlphaMovie", Variant::Object(handle));
}

fn install_alpha_movie_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    // The reference plugin marks numOfFrame/frame/screenWidth/screenHeight/
    // FPSScale/FPSRate as read-only; plain writable members are close enough
    // for the polling wrappers this stub targets.
    for (name, value) in [
        ("numOfFrame", 0),
        ("frame", 0),
        ("loop", 0),
        ("nextLoop", 0),
        ("preloadSamples", 0),
        ("isPlaying", 0),
        ("left", 0),
        ("top", 0),
        ("screenWidth", 0),
        ("screenHeight", 0),
        ("FPSScale", 1),
        ("FPSRate", 30),
    ] {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, Variant::Integer(value));
        }
    }

    runtime.register_object_native(handle, "open", alpha_movie_open);
    runtime.register_object_native(handle, "clear", native_void);
    runtime.register_object_native(handle, "showNextImage", alpha_movie_show_next_image);
    runtime.register_object_native(handle, "play", alpha_movie_play);
    runtime.register_object_native(handle, "stop", alpha_movie_stop);
    runtime.register_object_native(handle, "setPosition", alpha_movie_set_position);
    runtime.register_object_native(handle, "setNextMovieFile", alpha_movie_set_next_movie_file);
}

fn alpha_movie_open(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    if runtime.host().read_binary_storage(&name).is_err() {
        return Err(TjsError::runtime(format!(
            "can't open alpha movie file - {name}"
        )));
    }
    if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) {
        runtime.set_object_member(this, "__file", Variant::String(name));
        runtime.set_object_member(this, "numOfFrame", Variant::Integer(1));
        runtime.set_object_member(this, "frame", Variant::Integer(0));
    }
    Ok(Variant::Void)
}

fn alpha_movie_show_next_image(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) else {
        return Ok(Variant::Integer(0));
    };
    let frame = runtime
        .object_member(this, "frame")
        .to_integer()
        .unwrap_or(0);
    let num_frames = runtime
        .object_member(this, "numOfFrame")
        .to_integer()
        .unwrap_or(0);
    // Advance unless the last frame was reached; with the stub's single frame
    // this always reports frame 0, i.e. the finished state.
    if frame < num_frames - 1 {
        let frame = frame + 1;
        runtime.set_object_member(this, "frame", Variant::Integer(frame));
        return Ok(Variant::Integer(frame));
    }
    Ok(Variant::Integer(frame))
}

fn alpha_movie_play(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) {
        runtime.set_object_member(this, "isPlaying", Variant::Integer(1));
    }
    Ok(Variant::Void)
}

fn alpha_movie_stop(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) {
        runtime.set_object_member(this, "isPlaying", Variant::Integer(0));
    }
    Ok(Variant::Void)
}

fn alpha_movie_set_position(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) {
        let left = args
            .first()
            .map(Variant::to_integer)
            .transpose()?
            .unwrap_or(0);
        let top = args
            .get(1)
            .map(Variant::to_integer)
            .transpose()?
            .unwrap_or(0);
        runtime.set_object_member(this, "left", Variant::Integer(left));
        runtime.set_object_member(this, "top", Variant::Integer(top));
    }
    Ok(Variant::Void)
}

fn alpha_movie_set_next_movie_file(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) {
        let name = args
            .first()
            .map(Variant::to_tjs_string)
            .transpose()?
            .unwrap_or_default();
        runtime.set_object_member(this, "__nextFile", Variant::String(name));
    }
    Ok(Variant::Void)
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}
