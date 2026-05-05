use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{install_static_object, native_void, required_arg_string};

pub(crate) fn install_plugins(runtime: &mut Runtime<KrkrHost>) {
    let plugins = install_static_object(runtime, "Plugins");
    runtime.register_object_native(plugins, "link", plugins_link);
    runtime.register_object_native(plugins, "unlink", plugins_unlink);
    runtime.register_object_native(plugins, "getList", plugins_get_list);
    install_motionplayer_compat(runtime);
}

fn plugins_link(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Plugins.link")?;
    runtime.host_mut().link_plugin(&name);
    if name.to_ascii_lowercase().contains("motionplayer") {
        install_motionplayer_compat(runtime);
    }
    Ok(Variant::Void)
}

fn plugins_unlink(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Plugins.unlink")?;
    Ok(Variant::Integer(i64::from(
        runtime.host_mut().unlink_plugin(&name),
    )))
}

fn plugins_get_list(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let values = runtime
        .host()
        .linked_plugins()
        .map(|name| Variant::String(name.to_string()))
        .collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

fn install_motionplayer_compat(runtime: &mut Runtime<KrkrHost>) {
    let motion = match runtime.global_member("Motion") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "Motion");
            runtime.set_global_member("Motion", Variant::Object(handle));
            handle
        }
    };

    for (name, value) in [("enableD3D", 0), ("PlayFlagForce", 1), ("MaskModeAlpha", 1)] {
        runtime.set_object_member(motion, name, Variant::Integer(value));
    }

    let player = motion_player_constructor(runtime, "Player");
    let emote_player = motion_player_constructor(runtime, "EmotePlayer");
    let separate_adaptor = simple_constructor(runtime, "SeparateLayerAdaptor");
    let resource_manager = motion_resource_manager_constructor(runtime);
    runtime.set_object_member(motion, "Player", Variant::Object(player));
    runtime.set_object_member(motion, "EmotePlayer", Variant::Object(emote_player));
    runtime.set_object_member(
        motion,
        "SeparateLayerAdaptor",
        Variant::Object(separate_adaptor),
    );
    runtime.set_object_member(
        motion,
        "MotionResourceManager",
        Variant::Object(resource_manager),
    );
}

fn motion_player_constructor(
    runtime: &mut Runtime<KrkrHost>,
    class_name: &'static str,
) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, class_name);
            install_motion_player_members(runtime, instance);
            runtime.set_object_member(
                instance,
                "resourceManager",
                args.first().cloned().unwrap_or_default(),
            );
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, class_name);
    install_motion_player_members(runtime, handle);
    handle
}

fn install_motion_player_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    for (name, value) in [
        ("chara", Variant::String(String::new())),
        ("motion", Variant::String(String::new())),
        ("speed", Variant::Integer(1)),
        ("tickCount", Variant::Integer(0)),
        ("playing", Variant::Integer(0)),
        ("smoothing", Variant::Integer(0)),
        ("maskMode", Variant::Integer(0)),
        ("outline", Variant::Void),
        ("useD3D", Variant::Integer(0)),
    ] {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, value);
        }
    }

    for method in [
        "load",
        "show",
        "stop",
        "skipToSync",
        "progress",
        "initPhysics",
        "unserialize",
        "setColor",
        "setCoord",
        "setRotate",
        "setScale",
        "setDrawAffineTranslateMatrix",
        "draw",
        "clear",
    ] {
        runtime.register_object_native(handle, method, native_void);
    }
    runtime.register_object_native(handle, "play", motion_player_play);
    runtime.register_object_native(handle, "serialize", empty_string);
    runtime.register_object_native(handle, "setVariable", native_void);
    runtime.register_object_native(handle, "getVariable", zero);
    runtime.register_object_native(handle, "contains", zero);
    runtime.register_object_native(handle, "getMainTimelineLabelList", empty_array);
    runtime.register_object_native(handle, "getDiffTimelineLabelList", empty_array);
    runtime.register_object_native(handle, "getPlayingTimelineInfoList", empty_array);
    runtime.register_object_native(handle, "getLoopTimeline", zero);
}

fn motion_player_play(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) {
        runtime.set_object_member(this, "motion", args.first().cloned().unwrap_or_default());
        runtime.set_object_member(this, "playing", Variant::Integer(1));
    }
    Ok(Variant::Void)
}

fn motion_resource_manager_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "MotionResourceManager");
            install_motion_resource_manager_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "MotionResourceManager");
    install_motion_resource_manager_members(runtime, handle);
    handle
}

fn install_motion_resource_manager_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.set_object_member(handle, "resourceManager", Variant::Object(handle));
    runtime.register_object_native(handle, "addRef", return_this);
    runtime.register_object_native(handle, "release", native_void);
    runtime.register_object_native(handle, "loadResource", native_void);
    runtime.register_object_native(handle, "unloadResource", native_void);
}

fn simple_constructor(runtime: &mut Runtime<KrkrHost>, class_name: &'static str) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, class_name);
            runtime.register_object_native(instance, "getSubImageLayers", empty_array);
            runtime.register_object_native(instance, "addRef", return_this);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, class_name);
    runtime.register_object_native(handle, "getSubImageLayers", empty_array);
    runtime.register_object_native(handle, "addRef", return_this);
    handle
}

fn return_this(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(this_obj.map(Variant::Object).unwrap_or_default())
}

fn empty_array(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

fn empty_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(String::new()))
}

fn zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}
