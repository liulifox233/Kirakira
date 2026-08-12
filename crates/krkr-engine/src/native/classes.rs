use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use krkr_core::{
    AudioBus, AudioCommand, AudioLoadPolicy, Color, ImageUpload, LayerImage, LayerNode, Size,
    TransitionMethod, TransitionParams, TransitionScrollFrom, TransitionScrollStay,
};
use krkr_font::{FontSpec, FontSystem, TextLayout, TextStyle};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{Closure, ObjectHandle, Runtime, TjsHost, Variant},
};

use crate::host::{
    CompletedImageLoad, ImageLoadRequest, ImageLoadTarget, KagLayerSlot, KrkrHost,
    LayerRenderTarget, NativeTransitionCompletion, TraceCategory,
};
use crate::scheduler::AsyncTriggerMode;

use super::{
    native_void, register_stub_method,
    video::{
        install_video_native_properties, install_video_overlay_methods,
        install_video_overlay_property_placeholders,
    },
};

pub(crate) fn install_native_class(
    runtime: &mut Runtime<KrkrHost>,
    spec: &'static NativeClassSpec,
    global: bool,
) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            construct_native_instance(runtime, spec, this_obj, args)
        },
    );
    runtime.add_object_class_info(handle, spec.name);
    install_methods(runtime, handle, spec.name, spec.methods);
    install_special_methods(runtime, handle, spec.name);
    install_native_properties(runtime, handle, spec.name);
    install_methods(runtime, handle, spec.name, spec.static_methods);
    install_properties(runtime, handle, spec.static_properties);
    if global {
        runtime.set_global_member(spec.name, Variant::Object(handle));
    }
    handle
}

fn construct_native_instance(
    runtime: &mut Runtime<KrkrHost>,
    spec: &'static NativeClassSpec,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let existing_this = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle());
    let handle = existing_this.unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(handle, spec.name);
    runtime.set_object_member(
        handle,
        "__className",
        Variant::String(spec.name.to_string()),
    );
    if let Variant::Object(class_handle) = runtime.global_member(spec.name)
        && runtime.object_super_class(handle).is_none()
    {
        runtime.set_object_super_class(handle, class_handle);
    }
    // WaveSoundBuffer and VideoOverlay keep their native methods on the class
    // object only: script subclasses override `open`/`play`/`stop` (GINKA's
    // Movie extends VideoOverlay) and forward with `SUPER.*()`, so installing
    // the natives directly on each instance would shadow those overrides.
    if !matches!(spec.name, "WaveSoundBuffer" | "VideoOverlay") {
        install_methods(runtime, handle, spec.name, spec.methods);
        install_special_methods(runtime, handle, spec.name);
    }
    // These are native events, not instance method implementations. Keeping
    // Layer's placeholders on the instance shadows overrides supplied by
    // script base classes (notably MessageLayer.onPaint, which drives GINKA's
    // message renderer). Leave the no-op declarations on the Layer class for
    // `SUPER.*()` calls, but let normal member lookup reach script overrides
    // on each instance.
    if spec.name == "Layer" {
        runtime.delete_object_member(handle, "onTransitionCompleted");
        runtime.delete_object_member(handle, "onPaint");
    }
    // VideoOverlay has the same shadowing problem for script *properties*
    // (Movie.left/top/audioVolume/...): skip placeholders that a script class
    // in the chain already declares.
    if spec.name == "VideoOverlay" {
        install_video_overlay_property_placeholders(runtime, handle);
    } else {
        install_properties(runtime, handle, spec.properties);
    }
    apply_constructor_defaults(runtime, handle, spec.name, &args)?;
    install_instance_native_properties(runtime, handle, spec.name);
    Ok(Variant::Object(handle))
}

fn install_methods(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &'static str,
    methods: &'static [&'static str],
) {
    for method in methods {
        if member_visible_in_chain(runtime, handle, method) {
            continue;
        }
        register_stub_method(runtime, handle, class_name, method);
    }
}

// Native stubs are placeholders so `SUPER.method()` resolves when nothing
// else provides the member.  Installing one on an instance whose class chain
// already supplies `method` (a script class body or a parent native class)
// would shadow that implementation — krkr2 only declares these on the native
// class itself — so skip the stub whenever the whole chain has the member.
fn member_visible_in_chain(runtime: &Runtime<KrkrHost>, handle: ObjectHandle, name: &str) -> bool {
    let mut current = Some(handle);
    while let Some(object) = current {
        if !matches!(runtime.object_member(object, name), Variant::Void) {
            return true;
        }
        current = runtime.object_super_class(object);
    }
    false
}

fn install_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    properties: &'static [&'static str],
) {
    for property in properties {
        if runtime.has_object_member(handle, property) {
            continue;
        }
        runtime.set_object_member(handle, *property, Variant::Void);
    }
}

fn install_native_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &'static str,
) {
    match class_name {
        "Layer" => install_layer_native_properties(runtime, handle, false),
        "Window" => install_window_native_properties(runtime, handle, false),
        "WaveSoundBuffer" => install_wave_native_properties(runtime, handle, false),
        "VideoOverlay" => install_video_native_properties(runtime, handle, false),
        _ => {}
    }
}

fn install_instance_native_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &'static str,
) {
    match class_name {
        "Layer" => install_layer_native_properties(runtime, handle, true),
        "Window" => install_window_native_properties(runtime, handle, true),
        "WaveSoundBuffer" => install_wave_native_properties(runtime, handle, true),
        "VideoOverlay" => install_video_native_properties(runtime, handle, true),
        _ => {}
    }
}

fn install_layer_native_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    preserve_script_properties: bool,
) {
    for &property in LAYER_NATIVE_PROPERTIES {
        if preserve_script_properties && runtime.object_member_is_property(handle, property) {
            continue;
        }
        let property_handle = runtime.register_object_native_property(
            handle,
            property,
            move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                layer_native_property_get(runtime, this_obj, property)
            },
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  value: Variant| {
                layer_native_property_set(runtime, this_obj, property, value)
            },
        );
        if preserve_script_properties {
            runtime.set_object_member(
                handle,
                property,
                Variant::Closure(Closure::new(property_handle, Some(handle))),
            );
        }
    }
}

fn install_window_native_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    preserve_script_properties: bool,
) {
    for &property in WINDOW_NATIVE_PROPERTIES {
        if preserve_script_properties && runtime.object_member_is_property(handle, property) {
            continue;
        }
        let property_handle = runtime.register_object_native_property(
            handle,
            property,
            move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                window_native_property_get(runtime, this_obj, property)
            },
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  value: Variant| {
                window_native_property_set(runtime, this_obj, property, value)
            },
        );
        if preserve_script_properties {
            runtime.set_object_member(
                handle,
                property,
                Variant::Closure(Closure::new(property_handle, Some(handle))),
            );
        }
    }
}

fn install_wave_native_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    preserve_script_properties: bool,
) {
    for &property in WAVE_NATIVE_PROPERTIES {
        if preserve_script_properties && runtime.object_member_is_property(handle, property) {
            continue;
        }
        let property_handle = runtime.register_object_native_property(
            handle,
            property,
            move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                wave_native_property_get(runtime, this_obj, property)
            },
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  value: Variant| {
                wave_native_property_set(runtime, this_obj, property, value)
            },
        );
        if preserve_script_properties {
            runtime.set_object_member(
                handle,
                property,
                Variant::Closure(Closure::new(property_handle, Some(handle))),
            );
        }
    }
}

fn apply_constructor_defaults(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &str,
    args: &[Variant],
) -> Result<()> {
    match class_name {
        "Rect" => {
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
            let right = args
                .get(2)
                .map(Variant::to_integer)
                .transpose()?
                .unwrap_or(0);
            let bottom = args
                .get(3)
                .map(Variant::to_integer)
                .transpose()?
                .unwrap_or(0);
            runtime.set_object_member(handle, "left", Variant::Integer(left));
            runtime.set_object_member(handle, "top", Variant::Integer(top));
            runtime.set_object_member(handle, "right", Variant::Integer(right));
            runtime.set_object_member(handle, "bottom", Variant::Integer(bottom));
            runtime.set_object_member(handle, "width", Variant::Integer(right - left));
            runtime.set_object_member(handle, "height", Variant::Integer(bottom - top));
        }
        "Timer" => {
            runtime.set_object_member(handle, "enabled", Variant::Integer(0));
            runtime.set_object_member(handle, "interval", Variant::Integer(1000));
            runtime.set_object_member(handle, "capacity", Variant::Integer(1));
            runtime.set_object_member(handle, "mode", Variant::Integer(0));
            runtime.set_object_member(
                handle,
                "__actionOwner",
                args.first().cloned().unwrap_or_default(),
            );
            runtime.set_object_member(
                handle,
                "__actionName",
                Variant::String(action_name_from_constructor_args(args)?),
            );
            runtime.host_mut().register_timer(handle);
        }
        "AsyncTrigger" => {
            runtime.set_object_member(handle, "cached", Variant::Integer(1));
            runtime.set_object_member(handle, "mode", Variant::Integer(0));
            runtime.set_object_member(
                handle,
                "__actionOwner",
                args.first().cloned().unwrap_or_default(),
            );
            runtime.set_object_member(
                handle,
                "__actionName",
                Variant::String(action_name_from_constructor_args(args)?),
            );
            runtime.host_mut().register_async_trigger(handle);
        }
        "Window" => {
            set_window_property_storage(runtime, handle, "visible", Variant::Integer(0));
            runtime.set_object_member(handle, "caption", Variant::String(String::new()));
            set_window_property_storage(runtime, handle, "left", Variant::Integer(0));
            set_window_property_storage(runtime, handle, "top", Variant::Integer(0));
            set_window_property_storage(runtime, handle, "width", Variant::Integer(0));
            set_window_property_storage(runtime, handle, "height", Variant::Integer(0));
            set_window_property_storage(runtime, handle, "innerWidth", Variant::Integer(0));
            set_window_property_storage(runtime, handle, "innerHeight", Variant::Integer(0));
            set_window_property_storage(runtime, handle, "focusedLayer", Variant::Null);
            runtime.set_object_member(handle, "fullScreen", Variant::Integer(0));
            set_window_property_storage(runtime, handle, "zoomNumer", Variant::Integer(100));
            set_window_property_storage(runtime, handle, "zoomDenom", Variant::Integer(100));
            let children = runtime.alloc_array_object(Vec::new());
            runtime
                .host_mut()
                .register_native_window(handle, Some(children));
            set_window_property_storage(runtime, handle, "children", Variant::Object(children));
            let menu = alloc_menu_item_object(runtime, Some(handle), String::new());
            runtime.set_object_member(handle, "menu", Variant::Object(menu));
            let draw_device =
                construct_native_instance(runtime, &BASIC_DRAW_DEVICE_CLASS, None, Vec::new())?;
            runtime.set_object_member(handle, "drawDevice", draw_device);
            if let Variant::Object(window_class) = runtime.global_member("Window")
                && matches!(
                    runtime.object_member(window_class, "mainWindow"),
                    Variant::Void
                )
            {
                runtime.set_object_member(window_class, "mainWindow", Variant::Object(handle));
            }
        }
        "MenuItem" => {
            let owner = args.first().cloned().unwrap_or_default();
            let caption = args
                .get(1)
                .map(Variant::to_tjs_string)
                .transpose()?
                .unwrap_or_default();
            runtime.set_object_member(handle, "owner", owner);
            runtime.set_object_member(handle, "caption", Variant::String(caption));
            runtime.set_object_member(handle, "shortcut", Variant::String(String::new()));
            runtime.set_object_member(handle, "checked", Variant::Integer(0));
            runtime.set_object_member(handle, "enabled", Variant::Integer(1));
            runtime.set_object_member(handle, "visible", Variant::Integer(1));
            runtime.set_object_member(handle, "radio", Variant::Integer(0));
            runtime.set_object_member(handle, "group", Variant::Integer(0));
            let children = runtime.alloc_array_object(Vec::new());
            runtime.set_object_member(handle, "children", Variant::Object(children));
        }
        "Layer" => {
            let window = args.first().cloned().unwrap_or_default();
            let parent = args.get(1).cloned().unwrap_or_default();
            let window_object =
                variant_object(&window).map(|window| runtime.bound_this(window).unwrap_or(window));
            let parent_object =
                variant_object(&parent).map(|parent| runtime.bound_this(parent).unwrap_or(parent));
            let preserve_existing_attachment = window_object.is_none()
                && parent_object.is_none()
                && (runtime.host().native_layer_window(handle).is_some()
                    || runtime.host().native_layer_parent(handle).is_some());
            if !preserve_existing_attachment {
                let is_primary =
                    window_object.is_some() && matches!(parent, Variant::Void | Variant::Null);
                let stored_window = window_object
                    .map(Variant::Object)
                    .unwrap_or_else(|| window.clone());
                let stored_parent = parent_object
                    .map(Variant::Object)
                    .unwrap_or_else(|| parent.clone());
                runtime.set_object_member(handle, "__actionOwner", stored_window.clone());
                let children = runtime.alloc_array_object(Vec::new());
                let layer_id = runtime.host_mut().register_native_layer(
                    handle,
                    format!("native:{}", handle.0),
                    window_object,
                    parent_object,
                    Some(children),
                    is_primary,
                );
                set_layer_property_storage(runtime, handle, "window", stored_window.clone());
                set_layer_property_storage(runtime, handle, "parent", stored_parent.clone());
                runtime.set_object_member(
                    handle,
                    "__nativeLayerId",
                    Variant::Integer(layer_id as i64),
                );
                set_layer_property_storage(runtime, handle, "children", Variant::Object(children));
                set_layer_property_storage(runtime, handle, "left", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "top", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "width", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "height", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "imageLeft", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "imageTop", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "imageWidth", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "imageHeight", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "order", Variant::Integer(0));
                set_layer_property_storage(
                    runtime,
                    handle,
                    "absoluteOrderMode",
                    Variant::Integer(0),
                );
                set_layer_property_storage(
                    runtime,
                    handle,
                    "visible",
                    Variant::Integer(i64::from(is_primary)),
                );
                set_layer_property_storage(runtime, handle, "enabled", Variant::Integer(1));
                set_layer_property_storage(runtime, handle, "nodeEnabled", Variant::Integer(1));
                set_layer_property_storage(runtime, handle, "nodeVisible", Variant::Integer(1));
                set_layer_property_storage(runtime, handle, "callOnPaint", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "opacity", Variant::Integer(255));
                set_layer_property_storage(
                    runtime,
                    handle,
                    "type",
                    Variant::Integer(if is_primary { 1 } else { 2 }),
                );
                set_layer_property_storage(
                    runtime,
                    handle,
                    "neutralColor",
                    Variant::Integer(if is_primary { 0xffff_ffff } else { 0x00ff_ffff }),
                );
                set_layer_property_storage(runtime, handle, "face", Variant::Integer(128));
                set_layer_property_storage(runtime, handle, "hitType", Variant::Integer(0));
                set_layer_property_storage(
                    runtime,
                    handle,
                    "hitThreshold",
                    Variant::Integer(if is_primary { 0 } else { 16 }),
                );
                set_layer_property_storage(
                    runtime,
                    handle,
                    "isPrimary",
                    Variant::Integer(i64::from(is_primary)),
                );
                runtime.set_object_member(handle, "focusable", Variant::Integer(0));
                runtime.set_object_member(handle, "joinFocusChain", Variant::Integer(1));
                runtime.set_object_member(handle, "focused", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "cursor", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "hint", Variant::String(String::new()));
                set_layer_property_storage(runtime, handle, "showParentHint", Variant::Integer(1));
                let font = construct_native_instance(runtime, &FONT_CLASS, None, Vec::new())?;
                set_layer_property_storage(runtime, handle, "font", font);
                if is_primary && let Some(window) = window_object {
                    set_window_property_storage(
                        runtime,
                        window,
                        "primaryLayer",
                        Variant::Object(handle),
                    );
                }
                if let Some(parent) = parent_object {
                    let children = ensure_child_array(runtime, parent);
                    runtime.array_push(children, Variant::Object(handle));
                }
            }
        }
        "Font" => {
            runtime.set_object_member(handle, "face", Variant::String(String::new()));
            runtime.set_object_member(handle, "height", Variant::Integer(0));
            runtime.set_object_member(handle, "bold", Variant::Integer(0));
            runtime.set_object_member(handle, "italic", Variant::Integer(0));
            runtime.set_object_member(handle, "strikeout", Variant::Integer(0));
            runtime.set_object_member(handle, "underline", Variant::Integer(0));
            runtime.set_object_member(handle, "angle", Variant::Integer(0));
            runtime.set_object_member(handle, "faceIsFileName", Variant::Integer(0));
            runtime.set_object_member(handle, "rasterizer", Variant::String(String::new()));
        }
        "Bitmap" | "BitmapLayerTreeOwner" => {
            runtime.set_object_member(handle, "width", Variant::Integer(0));
            runtime.set_object_member(handle, "height", Variant::Integer(0));
        }
        "BasicDrawDevice" => {
            runtime.set_object_member(handle, "interface", Variant::Void);
            runtime.set_object_member(handle, "enableD3D", Variant::Integer(0));
            runtime.set_object_member(handle, "preferredDrawer", Variant::Integer(0));
        }
        "WaveSoundBuffer" => {
            runtime.set_object_member(handle, "status", Variant::String("unload".to_string()));
            runtime.set_object_member(handle, "volume", Variant::Integer(100000));
            runtime.set_object_member(handle, "pan", Variant::Integer(0));
            runtime.set_object_member(handle, "looping", Variant::Integer(0));
            // KRKR's BGM helper records the effective loop mode in flags[0].
            // It assumes this mutable array exists on every sound buffer.
            let flags = runtime.alloc_array_object(vec![Variant::Integer(0)]);
            runtime.set_object_member(handle, "flags", Variant::Object(flags));
            set_wave_property_storage(
                runtime,
                handle,
                "sampleCount",
                Variant::Integer(wave_default_sample_property(runtime, "sampleCount")),
            );
            set_wave_property_storage(
                runtime,
                handle,
                "sampleAhead",
                Variant::Integer(wave_default_sample_property(runtime, "sampleAhead")),
            );
            let id = runtime.host_mut().register_native_audio_buffer(handle);
            runtime.set_object_member(handle, "__nativeAudioId", Variant::Integer(id.0 as i64));
        }
        _ => {}
    }
    Ok(())
}

fn ensure_child_array(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) -> ObjectHandle {
    match runtime.object_member(handle, "children") {
        Variant::Object(children) => children,
        _ => {
            let children = runtime.alloc_array_object(Vec::new());
            runtime.set_object_member(handle, "children", Variant::Object(children));
            children
        }
    }
}

fn install_special_methods(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &'static str,
) {
    if class_name == "Timer" {
        install_timer_methods(runtime, handle);
    } else if class_name == "MenuItem" {
        install_menu_item_methods(runtime, handle);
    } else if class_name == "Layer" {
        install_layer_methods(runtime, handle);
    } else if class_name == "Font" {
        install_font_methods(runtime, handle);
    } else if class_name == "ImageFunction" {
        install_image_function_methods(runtime, handle);
    } else if class_name == "AsyncTrigger" {
        install_async_trigger_methods(runtime, handle);
    } else if class_name == "Window" {
        install_window_methods(runtime, handle);
    } else if class_name == "WaveSoundBuffer" {
        install_wave_sound_buffer_methods(runtime, handle);
    } else if class_name == "VideoOverlay" {
        install_video_overlay_methods(runtime, handle);
    }
}

fn alloc_menu_item_object(
    runtime: &mut Runtime<KrkrHost>,
    owner: Option<ObjectHandle>,
    caption: String,
) -> ObjectHandle {
    let handle = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(handle, "MenuItem");
    install_methods(runtime, handle, "MenuItem", MENU_ITEM_CLASS.methods);
    install_menu_item_methods(runtime, handle);
    install_properties(runtime, handle, MENU_ITEM_CLASS.properties);
    runtime.set_object_member(
        handle,
        "owner",
        owner.map(Variant::Object).unwrap_or_default(),
    );
    runtime.set_object_member(handle, "caption", Variant::String(caption));
    runtime.set_object_member(handle, "shortcut", Variant::String(String::new()));
    runtime.set_object_member(handle, "checked", Variant::Integer(0));
    runtime.set_object_member(handle, "enabled", Variant::Integer(1));
    runtime.set_object_member(handle, "visible", Variant::Integer(1));
    runtime.set_object_member(handle, "radio", Variant::Integer(0));
    runtime.set_object_member(handle, "group", Variant::Integer(0));
    runtime.set_object_member(handle, "parent", Variant::Void);
    let children = runtime.alloc_array_object(Vec::new());
    runtime.set_object_member(handle, "children", Variant::Object(children));
    handle
}

fn install_menu_item_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "add", menu_item_add);
    runtime.register_object_native(handle, "insert", menu_item_insert);
    runtime.register_object_native(handle, "remove", menu_item_remove);
    runtime.register_object_native(handle, "clear", menu_item_clear);
    runtime.register_object_native(handle, "click", menu_item_noop);
    runtime.register_object_native(handle, "onClick", menu_item_noop);
    runtime.register_object_native(handle, "popup", menu_item_noop);
}

fn menu_item_children(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    method: &str,
) -> Result<ObjectHandle> {
    let this =
        this_obj.ok_or_else(|| krkr_tjs2::TjsError::runtime(format!("{method} requires this")))?;
    match runtime.object_member(this, "children") {
        Variant::Object(children) => Ok(children),
        _ => {
            let children = runtime.alloc_array_object(Vec::new());
            runtime.set_object_member(this, "children", Variant::Object(children));
            Ok(children)
        }
    }
}

fn menu_item_add(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let item = args.into_iter().next().unwrap_or_default();
    let children = menu_item_children(runtime, this_obj, "MenuItem.add")?;
    runtime.array_push(children, item.clone());
    menu_item_set_parent(runtime, this_obj, &item);
    Ok(item)
}

fn menu_item_insert(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let item = args.first().cloned().unwrap_or_default();
    let index = args
        .get(1)
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(i64::MAX)
        .max(0) as usize;
    let children = menu_item_children(runtime, this_obj, "MenuItem.insert")?;
    runtime.array_insert(children, index, item.clone());
    menu_item_set_parent(runtime, this_obj, &item);
    Ok(item)
}

fn menu_item_remove(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let item = args.into_iter().next().unwrap_or_default();
    let children = menu_item_children(runtime, this_obj, "MenuItem.remove")?;
    runtime.array_remove_value(children, &item);
    if let Variant::Object(child) = &item {
        runtime.set_object_member(*child, "parent", Variant::Void);
    }
    Ok(Variant::Void)
}

fn menu_item_clear(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let children = menu_item_children(runtime, this_obj, "MenuItem.clear")?;
    let items = runtime
        .array_elements(children)
        .map(<[Variant]>::to_vec)
        .unwrap_or_default();
    for item in items {
        if let Variant::Object(child) = item {
            runtime.set_object_member(child, "parent", Variant::Void);
        }
    }
    runtime.array_clear(children);
    Ok(Variant::Void)
}

fn menu_item_set_parent(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    item: &Variant,
) {
    let (Some(this), Variant::Object(child)) = (this_obj, item) else {
        return;
    };
    runtime.set_object_member(*child, "parent", Variant::Object(this));
}

fn menu_item_noop(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn install_window_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_native_method_preserving_script(runtime, handle, "finalize", window_finalize);
    register_native_method_preserving_script(runtime, handle, "close", window_close);
    register_native_method_preserving_script(runtime, handle, "showModal", window_show_modal);
    register_native_method_preserving_script(
        runtime,
        handle,
        "onCloseQuery",
        window_on_close_query,
    );
    runtime.register_object_native(handle, "add", window_add);
    runtime.register_object_native(handle, "remove", window_remove);
    runtime.register_object_native(handle, "setPos", window_set_pos);
    runtime.register_object_native(handle, "setSize", window_set_size);
    runtime.register_object_native(handle, "setInnerSize", window_set_inner_size);
}

fn window_finalize(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn window_show_modal(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = require_window_this(runtime, this_obj, "Window.showModal")?;
    set_window_property_storage(runtime, this, "visible", Variant::Integer(1));
    set_window_property_storage(runtime, this, "__nativeClosed", Variant::Integer(0));
    set_window_property_storage(runtime, this, "__nativeModal", Variant::Integer(1));
    runtime.host_mut().push_modal_window(this);
    runtime.request_suspend();
    Ok(Variant::Void)
}

fn window_close(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = require_window_this(runtime, this_obj, "Window.close")?;
    set_window_property_storage(runtime, this, "visible", Variant::Integer(0));
    set_window_property_storage(runtime, this, "__nativeClosed", Variant::Integer(1));
    if is_main_window(runtime, this) {
        runtime.host_mut().request_termination();
    }
    Ok(Variant::Void)
}

fn window_on_close_query(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = require_window_this(runtime, this_obj, "Window.onCloseQuery")?;
    let can_close = args.first().map(Variant::is_truthy).unwrap_or(true);
    runtime.set_object_member(
        this,
        "__nativeCanClose",
        Variant::Integer(i64::from(can_close)),
    );
    if can_close {
        set_window_property_storage(runtime, this, "visible", Variant::Integer(0));
        set_window_property_storage(runtime, this, "__nativeClosed", Variant::Integer(1));
        if is_main_window(runtime, this) {
            runtime.host_mut().request_termination();
        }
    }
    Ok(Variant::Void)
}

fn require_window_this(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    method: &str,
) -> Result<ObjectHandle> {
    let this = this_obj.ok_or_else(|| TjsError::runtime(format!("{method} requires this")))?;
    Ok(runtime.bound_this(this).unwrap_or(this))
}

fn is_main_window(runtime: &Runtime<KrkrHost>, window: ObjectHandle) -> bool {
    let Variant::Object(window_class) = runtime.global_member("Window") else {
        return false;
    };
    let Variant::Object(main_window) = runtime.object_member(window_class, "mainWindow") else {
        return false;
    };
    runtime.bound_this(main_window).unwrap_or(main_window) == window
}

fn window_add(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Window.add requires this"))?;
    let item = args.first().cloned().unwrap_or_default();
    let Variant::Object(item_handle) = item else {
        return Ok(Variant::Void);
    };
    let children = ensure_child_array(runtime, this);
    runtime.array_remove_value(children, &Variant::Object(item_handle));
    runtime.array_push(children, Variant::Object(item_handle));
    runtime
        .host_mut()
        .add_native_window_child(this, item_handle);
    if runtime.host().native_layer(item_handle).is_some() {
        set_layer_property_storage(runtime, item_handle, "window", Variant::Object(this));
        if runtime.host().native_window_primary_layer(this) == Some(item_handle) {
            set_window_property_storage(
                runtime,
                this,
                "primaryLayer",
                Variant::Object(item_handle),
            );
        }
        if runtime.host().native_window_focused_layer(this) == Some(item_handle) {
            set_window_property_storage(
                runtime,
                this,
                "focusedLayer",
                Variant::Object(item_handle),
            );
        }
    }
    Ok(Variant::Void)
}

fn window_remove(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Window.remove requires this"))?;
    let item = args.first().cloned().unwrap_or_default();
    let children = ensure_child_array(runtime, this);
    runtime.array_remove_value(children, &item);
    if let Variant::Object(item_handle) = &item {
        runtime
            .host_mut()
            .remove_native_window_child(this, *item_handle);
    }
    if runtime.object_member(this, "primaryLayer") == item {
        set_window_property_storage(runtime, this, "primaryLayer", Variant::Void);
    }
    if runtime.object_member(this, "focusedLayer") == item {
        set_window_property_storage(runtime, this, "focusedLayer", Variant::Null);
    }
    Ok(Variant::Void)
}

fn window_set_pos(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = require_window_this(runtime, this_obj, "Window.setPos")?;
    let left = optional_integer(&args, 0)?.unwrap_or(0);
    let top = optional_integer(&args, 1)?.unwrap_or(0);
    set_window_property_storage(runtime, this, "left", Variant::Integer(left));
    set_window_property_storage(runtime, this, "top", Variant::Integer(top));

    let primary_layer = runtime.host().native_window_primary_layer(this);
    if let Some(layer) = primary_layer
        && runtime.host().native_layer(layer).is_some()
    {
        set_layer_property_storage(runtime, layer, "left", Variant::Integer(left));
        set_layer_property_storage(runtime, layer, "top", Variant::Integer(top));
        if let Some(layer_id) = runtime.host().native_layer(layer)
            && let Some(node) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id)
        {
            node.left = left as f32;
            node.top = top as f32;
        }
    }

    Ok(Variant::Void)
}

fn window_set_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Window.setSize requires this"))?;
    let width = optional_integer(&args, 0)?.unwrap_or(0).max(0);
    let height = optional_integer(&args, 1)?.unwrap_or(0).max(0);
    set_window_size_members(runtime, this, width, height);
    Ok(Variant::Void)
}

fn window_set_inner_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Window.setInnerSize requires this"))?;
    let width = optional_integer(&args, 0)?.unwrap_or(0).max(0);
    let height = optional_integer(&args, 1)?.unwrap_or(0).max(0);
    set_window_size_members(runtime, this, width, height);
    Ok(Variant::Void)
}

fn set_window_size_members(
    runtime: &mut Runtime<KrkrHost>,
    window: ObjectHandle,
    width: i64,
    height: i64,
) {
    set_window_property_storage(runtime, window, "width", Variant::Integer(width));
    set_window_property_storage(runtime, window, "height", Variant::Integer(height));
    set_window_property_storage(runtime, window, "innerWidth", Variant::Integer(width));
    set_window_property_storage(runtime, window, "innerHeight", Variant::Integer(height));
}

fn install_layer_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_native_method_preserving_script(runtime, handle, "finalize", layer_void);
    register_native_method_preserving_script(runtime, handle, "asLayer", layer_as_layer);
    register_native_method_preserving_script(runtime, handle, "loadImages", layer_load_images);
    register_native_method_preserving_script(
        runtime,
        handle,
        "saveLayerImage",
        layer_save_layer_image,
    );
    register_native_method_preserving_script(runtime, handle, "freeImage", layer_free_image);
    register_native_method_preserving_script(runtime, handle, "setPos", layer_set_pos);
    register_native_method_preserving_script(runtime, handle, "setSize", layer_set_size);
    register_native_method_preserving_script(runtime, handle, "setImagePos", layer_set_image_pos);
    register_native_method_preserving_script(runtime, handle, "setImageSize", layer_set_image_size);
    register_native_method_preserving_script(
        runtime,
        handle,
        "setSizeToImageSize",
        layer_set_size_to_image_size,
    );
    register_native_method_preserving_script(
        runtime,
        handle,
        "setDefaultCursor",
        layer_set_default_cursor,
    );
    register_native_method_preserving_script(runtime, handle, "bringToFront", layer_bring_to_front);
    register_native_method_preserving_script(runtime, handle, "bringToBack", layer_bring_to_back);
    register_native_method_preserving_script(runtime, handle, "assignImages", layer_assign_images);
    register_native_method_preserving_script(runtime, handle, "exchangeInfo", layer_exchange_info);
    register_native_method_preserving_script(
        runtime,
        handle,
        "beginTransition",
        layer_begin_transition,
    );
    register_native_method_preserving_script(
        runtime,
        handle,
        "stopTransition",
        layer_stop_transition,
    );
    // Keep a no-op declaration on the native Layer class so a script override
    // may safely call `SUPER.onTransitionCompleted()`.  Constructor instances
    // remove their own declaration below, allowing the actual script method
    // to be found through normal inheritance.
    if runtime.object_super_class(handle).is_none() {
        runtime.register_object_native(handle, "onTransitionCompleted", native_void);
    }
    register_native_method_preserving_script(runtime, handle, "fillRect", layer_fill_rect);
    register_native_method_preserving_script(runtime, handle, "colorRect", layer_color_rect);
    register_native_method_preserving_script(runtime, handle, "copyRect", layer_copy_rect);
    register_native_method_preserving_script(runtime, handle, "operateRect", layer_operate_rect);
    register_native_method_preserving_script(runtime, handle, "piledCopy", layer_piled_copy);
    register_native_method_preserving_script(runtime, handle, "stretchCopy", layer_stretch_copy);
    register_native_method_preserving_script(runtime, handle, "affineCopy", layer_affine_copy);
    register_native_method_preserving_script(runtime, handle, "drawText", layer_draw_text);
    register_native_method_preserving_script(runtime, handle, "drawGlyph", layer_draw_glyph);
    register_native_method_preserving_script(runtime, handle, "getProvincePixel", layer_zero);
    register_native_method_preserving_script(runtime, handle, "getLayerAt", layer_get_layer_at);
    register_native_method_preserving_script(runtime, handle, "update", layer_update);
    register_native_method_preserving_script(runtime, handle, "focus", layer_focus);
    register_native_method_preserving_script(runtime, handle, "focusPrev", layer_focus_prev);
    register_native_method_preserving_script(runtime, handle, "focusNext", layer_focus_next);
    register_native_method_preserving_script(runtime, handle, "setMode", layer_void);
    register_native_method_preserving_script(runtime, handle, "removeMode", layer_void);
    register_native_method_preserving_script(runtime, handle, "releaseCapture", layer_void);
    register_native_method_preserving_script(runtime, handle, "onClick", layer_on_click);
    register_native_method_preserving_script(runtime, handle, "onHitTest", layer_on_hit_test);
    register_native_method_preserving_script(runtime, handle, "onKeyDown", layer_on_key_down);
    register_native_method_preserving_script(runtime, handle, "onKeyUp", layer_on_key_up);
    register_native_method_preserving_script(
        runtime,
        handle,
        "onSearchPrevFocusable",
        layer_set_focus_work,
    );
    register_native_method_preserving_script(
        runtime,
        handle,
        "onSearchNextFocusable",
        layer_set_focus_work,
    );
    register_native_method_preserving_script(
        runtime,
        handle,
        "onBeforeFocus",
        layer_set_focus_work,
    );
}

fn install_font_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_native_method_preserving_script(runtime, handle, "getTextWidth", font_get_text_width);
    register_native_method_preserving_script(
        runtime,
        handle,
        "getTextHeight",
        font_get_text_height,
    );
    register_native_method_preserving_script(runtime, handle, "getEscWidthX", font_get_esc_width_x);
    register_native_method_preserving_script(runtime, handle, "getEscWidthY", font_get_esc_width_y);
    register_native_method_preserving_script(
        runtime,
        handle,
        "getEscHeightX",
        font_get_esc_height_x,
    );
    register_native_method_preserving_script(
        runtime,
        handle,
        "getEscHeightY",
        font_get_esc_height_y,
    );
    register_native_method_preserving_script(
        runtime,
        handle,
        "getGlyphDrawRect",
        font_get_glyph_draw_rect,
    );
    register_native_method_preserving_script(runtime, handle, "getList", font_get_list);
    register_native_method_preserving_script(
        runtime,
        handle,
        "mapPrerenderedFont",
        font_map_prerendered_font,
    );
    register_native_method_preserving_script(
        runtime,
        handle,
        "unmapPrerenderedFont",
        font_unmap_prerendered_font,
    );
}

fn install_image_function_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_native_method_preserving_script(runtime, handle, "drawText", image_function_draw_text);
    register_native_method_preserving_script(
        runtime,
        handle,
        "drawGlyph",
        image_function_draw_glyph,
    );
}

type NativeMethod =
    fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>;

const LAYER_NATIVE_PROPERTIES: &[&str] = &[
    "window",
    "parent",
    "children",
    "order",
    "absolute",
    "absoluteOrderMode",
    "visible",
    "nodeVisible",
    "opacity",
    "isPrimary",
    "left",
    "top",
    "width",
    "height",
    "imageLeft",
    "imageTop",
    "imageWidth",
    "imageHeight",
    "type",
    "face",
    "hitType",
    "hitThreshold",
    "cursor",
    "cursorX",
    "cursorY",
    "hint",
    "showParentHint",
    "enabled",
    "nodeEnabled",
    "callOnPaint",
    "neutralColor",
    "hasImage",
    "font",
];

const WINDOW_NATIVE_PROPERTIES: &[&str] = &[
    "visible",
    "left",
    "top",
    "width",
    "height",
    "innerWidth",
    "innerHeight",
    "children",
    "primaryLayer",
    "focusedLayer",
];

fn window_native_property_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    Ok(runtime
        .host()
        .native_window_property(this, name)
        .unwrap_or_else(|| runtime.object_member(this, name)))
}

fn window_native_property_set(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
    value: Variant,
) -> Result<()> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(());
    };
    let value = normalize_window_property_value(name, value)?;
    set_window_property_storage(runtime, this, name, value);
    Ok(())
}

fn normalize_window_property_value(name: &str, value: Variant) -> Result<Variant> {
    match name {
        "visible" => Ok(Variant::Integer(i64::from(value.is_truthy()))),
        "left" | "top" => Ok(Variant::Integer(value.to_integer()?)),
        "width" | "height" | "innerWidth" | "innerHeight" => {
            Ok(Variant::Integer(value.to_integer()?.max(0)))
        }
        _ => Ok(value),
    }
}

fn set_window_property_storage(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &str,
    value: Variant,
) {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    runtime
        .host_mut()
        .set_native_window_property(handle, name, value.clone());
    runtime.set_object_member(handle, name, value);
}

fn layer_native_property_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    if matches!(name, "cursorX" | "cursorY") {
        return Ok(layer_cursor_position_value(runtime, this, name));
    }
    if name == "hasImage" {
        let has_image = render_layer_target(runtime, this)?
            .as_ref()
            .and_then(|target| render_layer_snapshot(runtime, target))
            .is_some_and(|layer| layer.image.is_some());
        return Ok(Variant::Integer(i64::from(has_image)));
    }
    Ok(layer_property_value(runtime, this, name))
}

// Layer.cursorX/cursorY report the current mouse cursor position in the
// layer's local coordinate system, so they must be computed on read rather
// than stored.
fn layer_cursor_position_value(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &str,
) -> Variant {
    let (Some(layer_id), Some(cursor)) = (
        runtime.host().native_layer(handle),
        runtime.host().cursor_position(),
    ) else {
        return Variant::Void;
    };
    let Some(origin) = runtime.host().layer_tree().absolute_position(layer_id) else {
        return Variant::Void;
    };
    let value = if name == "cursorX" {
        cursor.x - origin.x
    } else {
        cursor.y - origin.y
    };
    Variant::Integer(value.round() as i64)
}

fn layer_native_property_set(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
    value: Variant,
) -> Result<()> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(());
    };
    let previous_type = (name == "type")
        .then(|| {
            layer_property_value(runtime, this, "type")
                .to_integer()
                .ok()
        })
        .flatten();
    let value = normalize_layer_property_value(name, value)?;
    if name == "hasImage" {
        set_layer_has_image(runtime, this, value.is_truthy())?;
        return Ok(());
    }
    set_layer_property_storage(runtime, this, name, value.clone());
    if name == "type" {
        let layer_type = value.to_integer()?;
        if previous_type != Some(layer_type) {
            set_layer_property_storage(
                runtime,
                this,
                "neutralColor",
                Variant::Integer(neutral_color_for_layer_type(layer_type)),
            );
        }
    }
    apply_layer_property_to_render(runtime, this, name, &value)
}

fn layer_property_value(runtime: &Runtime<KrkrHost>, handle: ObjectHandle, name: &str) -> Variant {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    if let Some(value) = runtime.host().native_layer_property(handle, name) {
        return value;
    }
    let direct = runtime.object_member(handle, name);
    if !runtime.variant_is_property(&direct) && !matches!(direct, Variant::Void) {
        return direct;
    }
    let stored = runtime.object_member(handle, &layer_property_backing_key(name));
    if !matches!(stored, Variant::Void) {
        return stored;
    }
    if runtime.variant_is_property(&direct) {
        Variant::Void
    } else {
        direct
    }
}

fn layer_property_i64(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &str,
    fallback: i64,
) -> Result<i64> {
    match layer_property_value(runtime, handle, name) {
        Variant::Void => Ok(fallback),
        value => value.to_integer(),
    }
}

fn set_layer_property_storage(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &str,
    value: Variant,
) {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    runtime
        .host_mut()
        .set_native_layer_property(handle, name, value.clone());
    runtime.set_object_member(
        handle,
        layer_property_backing_key(name).into_owned(),
        value.clone(),
    );
    if !runtime.object_member_is_property(handle, name) {
        runtime.set_object_member(handle, name, value);
    }
}

pub(crate) fn set_layer_call_on_paint(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    value: bool,
) {
    set_layer_property_storage(
        runtime,
        handle,
        "callOnPaint",
        Variant::Integer(i64::from(value)),
    );
}

pub(crate) fn complete_layer_before_draw(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
) -> Result<()> {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    if runtime.host().native_layer(handle).is_none() {
        return Ok(());
    }

    if !layer_property_value(runtime, handle, "callOnPaint").is_truthy() {
        return Ok(());
    }

    // KRKR2 tTJSNI_BaseLayer::BeforeCompletion clears this flag before
    // immediately dispatching onPaint, so an update issued by onPaint itself
    // remains pending for the next completion.
    set_layer_call_on_paint(runtime, handle, false);
    runtime
        .call_object_method(handle, "onPaint", Vec::new())
        .map(|_| ())
}

fn complete_layer_subtree_before_draw(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    visited: &mut BTreeSet<ObjectHandle>,
) -> Result<()> {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    if !visited.insert(handle) {
        return Ok(());
    }
    complete_layer_before_draw(runtime, handle)?;
    for child in runtime.host().native_layer_children(handle) {
        complete_layer_subtree_before_draw(runtime, child, visited)?;
    }
    Ok(())
}

pub(crate) fn complete_pending_layer_paints(runtime: &mut Runtime<KrkrHost>) -> Result<()> {
    let roots = runtime.host().native_layer_roots();
    let mut visited = BTreeSet::new();
    for root in roots {
        complete_layer_subtree_before_draw(runtime, root, &mut visited)?;
    }
    Ok(())
}

fn normalize_layer_property_value(name: &str, value: Variant) -> Result<Variant> {
    match name {
        "width" | "height" | "imageWidth" | "imageHeight" | "clipWidth" | "clipHeight" => {
            Ok(Variant::Integer(value.to_integer()?.max(0)))
        }
        "opacity" => Ok(Variant::Integer(value.to_integer()?.clamp(0, 255))),
        "neutralColor" => Ok(Variant::Integer(value.to_integer()? & 0xffff_ffff)),
        "hasImage" => Ok(Variant::Integer(i64::from(value.is_truthy()))),
        "left" | "top" | "imageLeft" | "imageTop" | "order" | "absolute" | "absoluteOrderMode"
        | "visible" | "nodeVisible" | "enabled" | "nodeEnabled" | "type" | "face" | "hitType"
        | "hitThreshold" | "cursor" | "isPrimary" | "showParentHint" | "callOnPaint" => {
            Ok(Variant::Integer(value.to_integer()?))
        }
        _ => Ok(value),
    }
}

fn neutral_color_for_layer_type(layer_type: i64) -> i64 {
    match layer_type {
        // KRKR2 tTJSNI_BaseLayer::SetType uses transparent white for these
        // blend modes.
        0 | 1 | 2 | 4 | 5 | 6 | 7 | 9 | 15 | 16 | 23 | 25 => 0x00ff_ffff,
        // Photoshop overlay/hard-light/soft-light use transparent middle gray.
        18..=20 => 0x0080_8080,
        // Additive/lighten/screen and the remaining Photoshop modes use
        // transparent black.
        _ => 0x0000_0000,
    }
}

fn set_layer_has_image(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    has_image: bool,
) -> Result<()> {
    let Some(target) = render_layer_target(runtime, handle)? else {
        return Ok(());
    };
    if !has_image {
        mutate_render_layer(runtime, &target, LayerNode::clear_image);
        runtime
            .host_mut()
            .clear_layer_image_storage_for_target(&target);
        sync_layer_image_members(runtime, handle, 0, 0);
        return Ok(());
    }
    if render_layer_snapshot(runtime, &target).is_some_and(|layer| layer.image.is_some()) {
        return Ok(());
    }

    let width = layer_property_i64(runtime, handle, "imageWidth", 0)?
        .max(layer_property_i64(runtime, handle, "width", 0)?)
        .max(1) as u32;
    let height = layer_property_i64(runtime, handle, "imageHeight", 0)?
        .max(layer_property_i64(runtime, handle, "height", 0)?)
        .max(1) as u32;
    let neutral = layer_property_value(runtime, handle, "neutralColor")
        .to_integer()
        .unwrap_or(0x00ff_ffff);
    let mut pixels = vec![0; width as usize * height as usize * 4];
    let rgba = packed_color_to_rgba(neutral);
    if rgba != [0, 0, 0, 0] {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&rgba);
        }
    }
    let image = runtime.host_mut().create_layer_image(width, height, pixels);
    mutate_render_layer(runtime, &target, |layer| layer.set_image(image));
    sync_layer_image_members(runtime, handle, width as i64, height as i64);
    Ok(())
}

fn apply_layer_property_to_render(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &str,
    value: &Variant,
) -> Result<()> {
    let Some(target) = render_layer_target(runtime, handle)? else {
        return Ok(());
    };
    let integer = match name {
        "left" | "top" | "width" | "height" | "imageLeft" | "imageTop" | "imageWidth"
        | "imageHeight" | "visible" | "enabled" | "nodeEnabled" | "opacity" | "type" | "face"
        | "hitType" | "hitThreshold" | "order" | "absolute" => Some(value.to_integer()?),
        _ => None,
    };
    if let Some(integer) = integer {
        mutate_render_layer(runtime, &target, |layer| match name {
            "left" => layer.left = integer as f32,
            "top" => layer.top = integer as f32,
            "width" => layer.width = integer.max(0) as f32,
            "height" => layer.height = integer.max(0) as f32,
            "imageLeft" => layer.image_left = integer as f32,
            "imageTop" => layer.image_top = integer as f32,
            "imageWidth" => layer.image_width = integer.max(0) as f32,
            "imageHeight" => layer.image_height = integer.max(0) as f32,
            "visible" => layer.visible = integer != 0,
            "enabled" => layer.enabled = integer != 0,
            "nodeEnabled" => layer.node_enabled = integer != 0,
            "opacity" => layer.opacity = integer.clamp(0, 255) as u8,
            "type" => layer.layer_type = integer as i32,
            "face" => layer.face = integer as i32,
            "hitType" => layer.hit_type = integer as i32,
            "hitThreshold" => {
                layer.hit_threshold = integer.clamp(i32::MIN as i64, i32::MAX as i64) as i32
            }
            "order" | "absolute" => {
                layer.z_order = integer.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            }
            _ => {}
        });
    }
    if name == "parent" {
        let parent =
            variant_object(value).map(|parent| runtime.bound_this(parent).unwrap_or(parent));
        let old_parent = runtime.host().native_layer_parent(handle);
        let updated = runtime
            .host_mut()
            .set_native_layer_parent(handle, parent, value.clone());
        if updated && old_parent != parent {
            if let Some(old_parent) = old_parent
                && let Variant::Object(children) =
                    layer_property_value(runtime, old_parent, "children")
            {
                runtime.array_remove_value(children, &Variant::Object(handle));
            }
            if let Some(parent) = parent {
                let children = ensure_child_array(runtime, parent);
                runtime.array_remove_value(children, &Variant::Object(handle));
                runtime.array_push(children, Variant::Object(handle));
            }
        }
    } else if name == "window" {
        let window =
            variant_object(value).map(|window| runtime.bound_this(window).unwrap_or(window));
        runtime
            .host_mut()
            .set_native_layer_window(handle, window, value.clone());
    }
    runtime.host_mut().apply_layer_instance_to_render(handle);
    Ok(())
}

fn layer_property_backing_key(name: &str) -> Cow<'static, str> {
    match name {
        "window" => Cow::Borrowed("__nativeLayerProperty$window"),
        "parent" => Cow::Borrowed("__nativeLayerProperty$parent"),
        "children" => Cow::Borrowed("__nativeLayerProperty$children"),
        "order" => Cow::Borrowed("__nativeLayerProperty$order"),
        "absolute" => Cow::Borrowed("__nativeLayerProperty$absolute"),
        "absoluteOrderMode" => Cow::Borrowed("__nativeLayerProperty$absoluteOrderMode"),
        "visible" => Cow::Borrowed("__nativeLayerProperty$visible"),
        "nodeVisible" => Cow::Borrowed("__nativeLayerProperty$nodeVisible"),
        "opacity" => Cow::Borrowed("__nativeLayerProperty$opacity"),
        "isPrimary" => Cow::Borrowed("__nativeLayerProperty$isPrimary"),
        "left" => Cow::Borrowed("__nativeLayerProperty$left"),
        "top" => Cow::Borrowed("__nativeLayerProperty$top"),
        "width" => Cow::Borrowed("__nativeLayerProperty$width"),
        "height" => Cow::Borrowed("__nativeLayerProperty$height"),
        "imageLeft" => Cow::Borrowed("__nativeLayerProperty$imageLeft"),
        "imageTop" => Cow::Borrowed("__nativeLayerProperty$imageTop"),
        "imageWidth" => Cow::Borrowed("__nativeLayerProperty$imageWidth"),
        "imageHeight" => Cow::Borrowed("__nativeLayerProperty$imageHeight"),
        "type" => Cow::Borrowed("__nativeLayerProperty$type"),
        "face" => Cow::Borrowed("__nativeLayerProperty$face"),
        "hitType" => Cow::Borrowed("__nativeLayerProperty$hitType"),
        "hitThreshold" => Cow::Borrowed("__nativeLayerProperty$hitThreshold"),
        "cursor" => Cow::Borrowed("__nativeLayerProperty$cursor"),
        "hint" => Cow::Borrowed("__nativeLayerProperty$hint"),
        "showParentHint" => Cow::Borrowed("__nativeLayerProperty$showParentHint"),
        "enabled" => Cow::Borrowed("__nativeLayerProperty$enabled"),
        "nodeEnabled" => Cow::Borrowed("__nativeLayerProperty$nodeEnabled"),
        "font" => Cow::Borrowed("__nativeLayerProperty$font"),
        _ => Cow::Owned(format!("__nativeLayerProperty${name}")),
    }
}

const WAVE_NATIVE_PROPERTIES: &[&str] = &[
    "looping",
    "volume",
    "volume2",
    "pan",
    "sampleValue",
    "sampleCount",
    "sampleAhead",
    "globalVolume",
    "globalFocusMode",
    "useVisBuffer",
];

fn wave_native_property_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
) -> Result<Variant> {
    if name == "globalVolume" {
        return Ok(Variant::Integer(
            runtime.host().native_audio_global_volume(),
        ));
    }
    if matches!(name, "globalFocusMode" | "useVisBuffer") {
        return Ok(runtime.object_member(
            runtime.global_handle(),
            &wave_static_property_backing_key(name),
        ));
    }
    if name == "sampleValue" {
        return Ok(Variant::Real(0.0));
    }
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    if matches!(name, "sampleCount" | "sampleAhead") {
        let value = runtime.object_member(this, &wave_property_backing_key(name));
        return Ok(match value {
            Variant::Void => Variant::Integer(wave_default_sample_property(runtime, name)),
            value => value,
        });
    }
    let value = runtime
        .host()
        .native_audio_buffer(this)
        .map(|buffer| match name {
            "looping" => Variant::Integer(i64::from(buffer.looping)),
            "volume" => Variant::Integer(buffer.volume),
            "volume2" => Variant::Integer(buffer.volume2),
            "pan" => Variant::Integer(buffer.pan),
            _ => Variant::Void,
        })
        .unwrap_or_else(|| runtime.object_member(this, &wave_property_backing_key(name)));
    Ok(value)
}

fn wave_native_property_set(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
    value: Variant,
) -> Result<()> {
    if name == "globalVolume" {
        let volume = value.to_integer()?.clamp(0, 100000);
        runtime.host_mut().set_native_audio_global_volume(volume);
        runtime.set_object_member(
            runtime.global_handle(),
            wave_static_property_backing_key(name),
            Variant::Integer(volume),
        );
        return Ok(());
    }
    if matches!(name, "globalFocusMode" | "useVisBuffer") {
        runtime.set_object_member(
            runtime.global_handle(),
            wave_static_property_backing_key(name),
            value,
        );
        return Ok(());
    }
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(());
    };
    match name {
        "sampleValue" => {}
        "sampleCount" | "sampleAhead" => {
            let value = value.to_integer()?.max(0);
            set_wave_property_storage(runtime, this, name, Variant::Integer(value));
        }
        "looping" => {
            let looping = value.is_truthy();
            runtime.host_mut().set_native_audio_looping(this, looping);
            set_wave_property_storage(runtime, this, name, Variant::Integer(i64::from(looping)));
        }
        "volume" => {
            let volume = value.to_integer()?.clamp(0, 100000);
            runtime.host_mut().set_native_audio_volume(this, volume);
            set_wave_property_storage(runtime, this, name, Variant::Integer(volume));
        }
        "volume2" => {
            let volume = value.to_integer()?.clamp(0, 100000);
            runtime.host_mut().set_native_audio_volume2(this, volume);
            set_wave_property_storage(runtime, this, name, Variant::Integer(volume));
        }
        "pan" => {
            let pan = value.to_integer()?.clamp(-100000, 100000);
            runtime.host_mut().set_native_audio_pan(this, pan);
            set_wave_property_storage(runtime, this, name, Variant::Integer(pan));
        }
        _ => {}
    }
    Ok(())
}

fn set_wave_property_storage(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &str,
    value: Variant,
) {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    runtime.set_object_member(handle, wave_property_backing_key(name), value);
}

fn wave_property_backing_key(name: &str) -> String {
    format!("__nativeWaveProperty${name}")
}

fn wave_static_property_backing_key(name: &str) -> String {
    format!("__nativeWaveStaticProperty${name}")
}

fn wave_default_sample_property(runtime: &Runtime<KrkrHost>, name: &str) -> i64 {
    match runtime.object_member(
        runtime.global_handle(),
        &wave_static_property_backing_key(name),
    ) {
        Variant::Integer(value) => value,
        Variant::Real(value) => value as i64,
        _ if name == "sampleCount" => 100,
        _ => 0,
    }
}

fn register_native_method_preserving_script(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
    function: NativeMethod,
) {
    if matches!(runtime.object_member(handle, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native(handle, name, function);
}

fn install_async_trigger_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    runtime.register_object_native(handle, "trigger", async_trigger_trigger);
    runtime.register_object_native(handle, "cancel", async_trigger_cancel);
    register_native_method_preserving_script(runtime, handle, "onFire", async_trigger_on_fire);
}

fn install_timer_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_native_method_preserving_script(runtime, handle, "onTimer", timer_on_timer);
}

fn install_wave_sound_buffer_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "open", wave_sound_buffer_open);
    runtime.register_object_native(handle, "play", wave_sound_buffer_play);
    runtime.register_object_native(handle, "stop", wave_sound_buffer_stop);
    runtime.register_object_native(handle, "fade", wave_sound_buffer_fade);
    runtime.register_object_native(handle, "stopFade", wave_sound_buffer_stop_fade);
    runtime.register_object_native(handle, "setPos", wave_sound_buffer_set_pos);
    runtime.register_object_native(
        handle,
        "setDefaultCounts",
        wave_sound_buffer_set_default_counts,
    );
    runtime.register_object_native(
        handle,
        "setDefaultAheads",
        wave_sound_buffer_set_default_aheads,
    );
    runtime.register_object_native(handle, "freeDirectSound", native_wave_noop);
    runtime.register_object_native(handle, "getVisBuffer", native_wave_noop);
}

fn wave_sound_buffer_open(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = native_audio_this(runtime, this_obj, "WaveSoundBuffer.open")?;
    let storage = args
        .first()
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()?
        .ok_or_else(|| TjsError::runtime("WaveSoundBuffer.open requires storage"))?;
    runtime
        .host_mut()
        .open_native_audio_storage(this, storage)?;
    let opened_storage = runtime
        .host()
        .native_audio_buffer(this)
        .and_then(|buffer| buffer.storage.clone())
        .unwrap_or_default();
    runtime.host_mut().trace(
        TraceCategory::Audio,
        &format!("WaveSoundBuffer.open: {opened_storage}"),
    );
    runtime.set_object_member(this, "status", Variant::String("stop".to_string()));
    runtime.set_object_member(this, "position", Variant::Integer(0));
    runtime.set_object_member(this, "samplePosition", Variant::Integer(0));
    Ok(Variant::Void)
}

fn wave_sound_buffer_play(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = native_audio_this(runtime, this_obj, "WaveSoundBuffer.play")?;
    sync_wave_buffer_settings(runtime, this)?;
    let bus = if runtime
        .host()
        .native_audio_buffer(this)
        .is_some_and(|buffer| buffer.looping)
    {
        AudioBus::Bgm
    } else {
        AudioBus::SoundEffect
    };
    let play_storage = runtime
        .host()
        .native_audio_buffer(this)
        .and_then(|buffer| buffer.storage.clone())
        .unwrap_or_else(|| "<unopened>".to_string());
    runtime.host_mut().trace(
        TraceCategory::Audio,
        &format!("WaveSoundBuffer.play: {play_storage} bus={bus:?}"),
    );
    runtime
        .host_mut()
        .queue_native_audio_play(this, bus, AudioLoadPolicy::Auto)?;
    runtime.set_object_member(this, "status", Variant::String("play".to_string()));
    runtime.set_object_member(this, "paused", Variant::Integer(0));
    call_wave_status_changed(runtime, this)?;
    Ok(Variant::Void)
}

fn wave_sound_buffer_stop(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = native_audio_this(runtime, this_obj, "WaveSoundBuffer.stop")?;
    let stop_storage = runtime
        .host()
        .native_audio_buffer(this)
        .and_then(|buffer| buffer.storage.clone())
        .unwrap_or_else(|| "<unopened>".to_string());
    runtime.host_mut().trace(
        TraceCategory::Audio,
        &format!("WaveSoundBuffer.stop: {stop_storage}"),
    );
    runtime.host_mut().cancel_audio_fade_completion(this);
    if let Some(id) = runtime
        .host()
        .native_audio_buffer(this)
        .map(|buffer| buffer.id)
    {
        runtime.host_mut().queue_audio_command(AudioCommand::Stop {
            id,
            fade_seconds: 0.0,
        });
    }
    runtime.host_mut().mark_native_audio_stopped(this);
    runtime.set_object_member(this, "status", Variant::String("stop".to_string()));
    runtime.set_object_member(this, "paused", Variant::Integer(0));
    call_wave_status_changed(runtime, this)?;
    Ok(Variant::Void)
}

pub(crate) fn call_wave_status_changed(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
) -> Result<()> {
    let callback = runtime.object_member(this, "onStatusChanged");
    if !matches!(callback, Variant::Void) && !runtime.variant_is_native_function(&callback) {
        return runtime
            .call_object_method(this, "onStatusChanged", Vec::new())
            .map(|_| ());
    }
    if runtime.call_secondary_class_method(this, "onStatusChanged", Vec::new())? {
        return Ok(());
    }
    if !matches!(callback, Variant::Void) {
        runtime
            .call_object_method(this, "onStatusChanged", Vec::new())
            .map(|_| ())?;
    }
    Ok(())
}

fn wave_sound_buffer_stop_fade(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = native_audio_this(runtime, this_obj, "WaveSoundBuffer.stopFade")?;
    runtime.host_mut().cancel_audio_fade_completion(this);
    Ok(Variant::Void)
}

fn wave_sound_buffer_fade(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = native_audio_this(runtime, this_obj, "WaveSoundBuffer.fade")?;
    let (target, millis) = if let Some(options) = args.first().and_then(variant_object) {
        let target = object_member_i64(runtime, options, "volume")?.unwrap_or(0);
        let millis = match object_member_i64(runtime, options, "time")? {
            Some(value) => value,
            None => object_member_i64(runtime, options, "period")?.unwrap_or(0),
        }
        .max(0);
        (target.saturating_mul(1000), millis)
    } else {
        (
            optional_integer(&args, 0)?.unwrap_or(0),
            optional_integer(&args, 1)?.unwrap_or(0).max(0),
        )
    };
    set_wave_property_storage(runtime, this, "volume", Variant::Integer(target));
    let fade_seconds = millis as f32 / 1000.0;
    let fade_storage = runtime
        .host()
        .native_audio_buffer(this)
        .and_then(|buffer| buffer.storage.clone())
        .unwrap_or_else(|| "<unopened>".to_string());
    runtime.host_mut().trace(
        TraceCategory::Audio,
        &format!("WaveSoundBuffer.fade: {fade_storage} target={target} millis={millis}"),
    );
    runtime
        .host_mut()
        .set_native_audio_volume_with_fade(this, target, fade_seconds);
    if runtime.host().native_audio_buffer(this).is_some() {
        runtime
            .host_mut()
            .schedule_audio_fade_completion(this, millis);
    }
    Ok(Variant::Void)
}

fn wave_sound_buffer_set_pos(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = native_audio_this(runtime, this_obj, "WaveSoundBuffer.setPos")?;
    let position = optional_integer(&args, 0)?.unwrap_or(0).max(0);
    runtime.set_object_member(this, "position", Variant::Integer(position));
    runtime.set_object_member(this, "samplePosition", Variant::Integer(position));
    Ok(Variant::Void)
}

fn wave_sound_buffer_set_default_counts(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let count = optional_integer(&args, 0)?.unwrap_or(100).max(0);
    runtime.set_object_member(
        runtime.global_handle(),
        wave_static_property_backing_key("sampleCount"),
        Variant::Integer(count),
    );
    Ok(Variant::Void)
}

fn wave_sound_buffer_set_default_aheads(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let ahead = optional_integer(&args, 0)?.unwrap_or(0).max(0);
    runtime.set_object_member(
        runtime.global_handle(),
        wave_static_property_backing_key("sampleAhead"),
        Variant::Integer(ahead),
    );
    Ok(Variant::Void)
}

fn native_wave_noop(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn native_audio_this(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    method: &str,
) -> Result<ObjectHandle> {
    let this = this_obj.ok_or_else(|| TjsError::runtime(format!("{method} requires this")))?;
    Ok(runtime.bound_this(this).unwrap_or(this))
}

fn sync_wave_buffer_settings(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) -> Result<()> {
    if runtime.host().native_audio_buffer(handle).is_none() {
        return Err(TjsError::runtime("WaveSoundBuffer is not initialized"));
    }
    Ok(())
}

fn async_trigger_trigger(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("AsyncTrigger.trigger requires this"))?;
    let mode = match runtime.object_member(this, "mode").to_integer()? {
        1 => AsyncTriggerMode::Exclusive,
        2 => AsyncTriggerMode::AtIdle,
        _ => AsyncTriggerMode::Normal,
    };
    let cached = runtime.object_member(this, "cached").is_truthy();
    runtime
        .host_mut()
        .trigger_async_with_mode(this, mode, cached);
    Ok(Variant::Void)
}

fn async_trigger_cancel(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("AsyncTrigger.cancel requires this"))?;
    runtime.host_mut().cancel_async(this);
    Ok(Variant::Void)
}

fn timer_on_timer(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    invoke_tvp_action(runtime, this_obj, args, "Timer.onTimer")
}

fn async_trigger_on_fire(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    invoke_tvp_action(runtime, this_obj, args, "AsyncTrigger.onFire")
}

fn invoke_tvp_action(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
    context: &str,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime(format!("{context} requires this")))?;
    let this = runtime.bound_this(this).unwrap_or(this);
    let owner = runtime.object_member(this, "__actionOwner");
    if matches!(owner, Variant::Void | Variant::Null) {
        return Ok(Variant::Void);
    }
    let action_name = runtime
        .object_member(this, "__actionName")
        .to_tjs_string()?;
    if action_name.is_empty() {
        runtime.call_function(owner, args)
    } else {
        runtime.call_variant_method(owner, &action_name, args)
    }
}

fn action_name_from_constructor_args(args: &[Variant]) -> Result<String> {
    args.get(1)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()
        .map(|name| name.unwrap_or_else(|| "action".to_string()))
}

fn this_layer_id(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<(ObjectHandle, u64)> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    let this = runtime.bound_this(this).unwrap_or(this);
    let id = runtime
        .object_member(this, "__nativeLayerId")
        .to_integer()? as u64;
    Ok((this, id))
}

fn this_render_layer_target(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<(ObjectHandle, Option<LayerRenderTarget>)> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    let this = runtime.bound_this(this).unwrap_or(this);
    Ok((this, render_layer_target(runtime, this)?))
}

fn render_layer_target(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
) -> Result<Option<LayerRenderTarget>> {
    register_kag_layer_slots_from_tjs(runtime);
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    Ok(runtime.host().layer_render_target(handle))
}

pub(crate) fn register_kag_layer_slots_from_tjs(runtime: &mut Runtime<KrkrHost>) {
    let mut slots = BTreeMap::new();
    let Variant::Object(kag) = runtime.global_member("kag") else {
        runtime.host_mut().replace_kag_layer_slots(slots);
        return;
    };

    for page in ["fore", "back"] {
        let Variant::Object(page_object) = runtime.object_member(kag, page) else {
            continue;
        };
        if let Variant::Object(base) = runtime.object_member(page_object, "base") {
            slots.insert(
                runtime.bound_this(base).unwrap_or(base),
                KagLayerSlot::new(page, "base"),
            );
        }
        collect_kag_layer_array_slots(runtime, &mut slots, page, page_object, "layers", false);
        collect_kag_layer_array_slots(runtime, &mut slots, page, page_object, "messages", true);
    }

    runtime.host_mut().replace_kag_layer_slots(slots);
}

fn collect_kag_layer_array_slots(
    runtime: &Runtime<KrkrHost>,
    slots: &mut BTreeMap<ObjectHandle, KagLayerSlot>,
    page: &str,
    page_object: ObjectHandle,
    member: &str,
    message_layers: bool,
) {
    let Variant::Object(array) = runtime.object_member(page_object, member) else {
        return;
    };
    if let Some(elements) = runtime.array_elements(array) {
        for (index, value) in elements.iter().enumerate() {
            insert_kag_layer_slot(runtime, slots, page, index, message_layers, value);
        }
        return;
    }
    let Ok(count) = runtime.object_member(array, "count").to_integer() else {
        return;
    };
    for index in 0..count.max(0) {
        let value = runtime.object_member(array, &index.to_string());
        insert_kag_layer_slot(runtime, slots, page, index as usize, message_layers, &value);
    }
}

fn insert_kag_layer_slot(
    runtime: &Runtime<KrkrHost>,
    slots: &mut BTreeMap<ObjectHandle, KagLayerSlot>,
    page: &str,
    index: usize,
    message_layer: bool,
    value: &Variant,
) {
    let Variant::Object(candidate) = value else {
        return;
    };
    let handle = runtime.bound_this(*candidate).unwrap_or(*candidate);
    let layer = if message_layer {
        format!("message{index}")
    } else {
        index.to_string()
    };
    slots.insert(handle, KagLayerSlot::new(page, &layer));
}

fn kag_layer_target(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
) -> Option<LayerRenderTarget> {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    runtime
        .host()
        .kag_layer_slot(handle)
        .cloned()
        .map(LayerRenderTarget::Kag)
}

fn render_layer_snapshot(
    runtime: &Runtime<KrkrHost>,
    target: &LayerRenderTarget,
) -> Option<LayerNode> {
    match target {
        LayerRenderTarget::Native(layer_id) => {
            runtime.host().layer_tree().layer(*layer_id).cloned()
        }
        LayerRenderTarget::Kag(slot) => runtime.host().kag_layer(&slot.page, &slot.layer).cloned(),
    }
}

fn registered_render_layer_target(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
) -> Option<LayerRenderTarget> {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    runtime.host().layer_render_target(handle)
}

fn mutate_render_layer<R>(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
    mutate: impl FnOnce(&mut LayerNode) -> R,
) -> Option<R> {
    match target {
        LayerRenderTarget::Native(layer_id) => runtime
            .host_mut()
            .layer_tree_mut()
            .layer_mut(*layer_id)
            .map(mutate),
        LayerRenderTarget::Kag(slot) => Some(runtime.host_mut().mutate_kag_layer(
            &slot.page,
            &slot.layer,
            mutate,
        )),
    }
}

#[derive(Clone, Copy)]
struct LayerLoadImageOptions {
    visible: Option<bool>,
    left: Option<i64>,
    top: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    opacity: Option<i64>,
    replace_size_when_unspecified: bool,
}

fn apply_loaded_image_to_layer(
    layer: &mut LayerNode,
    image: LayerImage,
    image_size: Size,
    options: LayerLoadImageOptions,
) {
    layer.set_image(image);
    if let Some(visible) = options.visible {
        layer.visible = visible;
    }
    if let Some(left) = options.left {
        layer.left = left as f32;
    }
    if let Some(top) = options.top {
        layer.top = top as f32;
    }
    if options.replace_size_when_unspecified {
        layer.width = options
            .width
            .map_or(image_size.width, |width| width.max(0) as f32);
        layer.height = options
            .height
            .map_or(image_size.height, |height| height.max(0) as f32);
    } else {
        if let Some(width) = options.width {
            layer.width = width.max(0) as f32;
        }
        if let Some(height) = options.height {
            layer.height = height.max(0) as f32;
        }
    }
    if let Some(opacity) = options.opacity {
        layer.opacity = opacity.clamp(0, 255) as u8;
    }
}

fn layer_load_images(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    let Some(source) = args.first() else {
        return Err(TjsError::runtime("Layer.loadImages requires storage"));
    };
    let storage = load_images_storage(runtime, source)?
        .filter(|storage| !storage.is_empty())
        .ok_or_else(|| TjsError::runtime("Layer.loadImages requires storage"))?;
    let options = source_object(source);
    let left = match options {
        Some(options) => object_optional_integer(runtime, options, "left").transpose()?,
        None => None,
    };
    let top = match options {
        Some(options) => object_optional_integer(runtime, options, "top").transpose()?,
        None => None,
    };
    let width = match options {
        Some(options) => object_optional_integer(runtime, options, "width").transpose()?,
        None => None,
    };
    let height = match options {
        Some(options) => object_optional_integer(runtime, options, "height").transpose()?,
        None => None,
    };
    let opacity = match options {
        Some(options) => object_optional_integer(runtime, options, "opacity").transpose()?,
        None => None,
    };
    let explicit_page = match options {
        Some(options) => object_optional_string(runtime, options, "page")?,
        None => None,
    };
    let explicit_layer = match options {
        Some(options) => object_optional_string(runtime, options, "layer")?,
        None => None,
    };
    let has_explicit_target = explicit_page.is_some() || explicit_layer.is_some();
    let load_options = LayerLoadImageOptions {
        visible: match options {
            Some(options) => object_optional_integer(runtime, options, "visible")
                .transpose()?
                .map(|value| value != 0)
                .or_else(|| has_explicit_target.then_some(true)),
            None => None,
        },
        left,
        top,
        width,
        height,
        opacity,
        replace_size_when_unspecified: has_explicit_target,
    };

    let image = runtime.host_mut().load_image_storage_for_script(&storage)?;
    let size = image.size();

    if has_explicit_target {
        let page = explicit_page.unwrap_or_else(|| "back".to_string());
        let layer_name = explicit_layer.unwrap_or_else(|| "base".to_string());
        runtime
            .host_mut()
            .mutate_kag_layer(&page, &layer_name, |layer| {
                apply_loaded_image_to_layer(layer, image, size, load_options);
            });
        runtime.host_mut().record_layer_image_storage(
            &LayerRenderTarget::Kag(KagLayerSlot::new(&page, &layer_name)),
            &storage,
        );
    } else {
        match render_layer_target(runtime, this)? {
            Some(target) => {
                mutate_render_layer(runtime, &target, |layer| {
                    apply_loaded_image_to_layer(layer, image, size, load_options);
                });
                runtime
                    .host_mut()
                    .record_layer_image_storage(&target, &storage);
            }
            None => {
                let load_options = LayerLoadImageOptions {
                    replace_size_when_unspecified: true,
                    ..load_options
                };
                runtime
                    .host_mut()
                    .mutate_kag_layer("back", "base", |layer| {
                        apply_loaded_image_to_layer(layer, image, size, load_options);
                    });
                runtime.host_mut().record_layer_image_storage(
                    &LayerRenderTarget::Kag(KagLayerSlot::new("back", "base")),
                    &storage,
                );
            }
        }
    }
    sync_layer_image_members(runtime, this, size.width as i64, size.height as i64);
    mark_image_modified(runtime, this);
    if let Some(visible) = load_options.visible {
        set_layer_property_storage(
            runtime,
            this,
            "visible",
            Variant::Integer(i64::from(visible)),
        );
    }
    if let Some(left) = left {
        set_layer_property_storage(runtime, this, "left", Variant::Integer(left));
    }
    if let Some(top) = top {
        set_layer_property_storage(runtime, this, "top", Variant::Integer(top));
    }
    if let Some(width) = width {
        set_layer_property_storage(runtime, this, "width", Variant::Integer(width.max(0)));
    }
    if let Some(height) = height {
        set_layer_property_storage(runtime, this, "height", Variant::Integer(height.max(0)));
    }
    if let Some(opacity) = opacity {
        set_layer_property_storage(
            runtime,
            this,
            "opacity",
            Variant::Integer(opacity.clamp(0, 255)),
        );
    }
    Ok(Variant::Void)
}

fn layer_save_layer_image(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let Some(path) = args.first().filter(|value| !matches!(value, Variant::Void)) else {
        return Err(TjsError::runtime("Layer.saveLayerImage requires storage"));
    };
    let path = path.to_tjs_string()?;
    let mode = args
        .get(1)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_else(|| "bmp".to_string());
    let Some(target) = target else {
        return Ok(Variant::Void);
    };
    let layer = render_layer_snapshot(runtime, &target)
        .ok_or_else(|| TjsError::runtime("Layer.saveLayerImage target is not available"))?;
    let width = layer
        .image
        .as_ref()
        .map(|image| image.upload.width)
        .unwrap_or_else(|| {
            layer_property_value(runtime, this, "imageWidth")
                .to_integer()
                .unwrap_or(0)
                .max(0) as u32
        });
    let height = layer
        .image
        .as_ref()
        .map(|image| image.upload.height)
        .unwrap_or_else(|| {
            layer_property_value(runtime, this, "imageHeight")
                .to_integer()
                .unwrap_or(0)
                .max(0) as u32
        });
    let pixels = layer
        .image
        .as_ref()
        .map(|image| image.upload.rgba.as_ref().to_vec())
        .unwrap_or_else(|| vec![0; width as usize * height as usize * 4]);
    let bytes = encode_layer_bmp(width, height, &pixels, &mode)?;
    runtime.host_mut().write_binary(&path, "", &bytes)?;
    Ok(Variant::Void)
}

pub(crate) fn apply_completed_resource_loads(runtime: &mut Runtime<KrkrHost>) -> Result<()> {
    let completions = runtime.host_mut().take_completed_image_loads();
    for completion in completions {
        apply_completed_image_load(runtime, completion)?;
    }
    Ok(())
}

pub(crate) fn apply_completed_image_load(
    runtime: &mut Runtime<KrkrHost>,
    completion: CompletedImageLoad,
) -> Result<()> {
    let image = completion.image;
    let size = image.size();
    apply_image_to_target(runtime, &completion.request, Some(image), Some(size))?;
    if let Some(this) = completion.request.owner
        && runtime.object_valid(this)
    {
        sync_layer_image_members(runtime, this, size.width as i64, size.height as i64);
        mark_image_modified(runtime, this);
        apply_layer_load_property_storage(runtime, this, &completion.request);
    }
    Ok(())
}

fn apply_image_to_target(
    runtime: &mut Runtime<KrkrHost>,
    request: &ImageLoadRequest,
    image: Option<krkr_core::LayerImage>,
    image_size: Option<krkr_core::Size>,
) -> Result<()> {
    match &request.target {
        ImageLoadTarget::Kag { page, layer } => {
            let image = image.clone();
            runtime.host_mut().mutate_kag_layer(page, layer, |layer| {
                if let Some(image) = image {
                    layer.set_image(image);
                }
                apply_layer_load_geometry(layer, request, image_size);
            });
            runtime.host_mut().record_layer_image_storage(
                &LayerRenderTarget::Kag(KagLayerSlot::new(page, layer)),
                &request.storage,
            );
        }
    }
    Ok(())
}

fn apply_layer_load_geometry(
    layer: &mut LayerNode,
    request: &ImageLoadRequest,
    image_size: Option<krkr_core::Size>,
) {
    layer.visible = request.visible;
    if let Some(left) = request.left {
        layer.left = left as f32;
    }
    if let Some(top) = request.top {
        layer.top = top as f32;
    }
    if let Some(width) = request.width {
        layer.width = width.max(0) as f32;
    } else if let Some(size) = image_size {
        layer.width = size.width;
    }
    if let Some(height) = request.height {
        layer.height = height.max(0) as f32;
    } else if let Some(size) = image_size {
        layer.height = size.height;
    }
    if let Some(opacity) = request.opacity {
        layer.opacity = opacity.clamp(0, 255) as u8;
    }
}

fn apply_layer_load_property_storage(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    request: &ImageLoadRequest,
) {
    set_layer_property_storage(
        runtime,
        this,
        "visible",
        Variant::Integer(i64::from(request.visible)),
    );
    if let Some(left) = request.left {
        set_layer_property_storage(runtime, this, "left", Variant::Integer(left));
    }
    if let Some(top) = request.top {
        set_layer_property_storage(runtime, this, "top", Variant::Integer(top));
    }
    if let Some(width) = request.width {
        set_layer_property_storage(runtime, this, "width", Variant::Integer(width.max(0)));
    }
    if let Some(height) = request.height {
        set_layer_property_storage(runtime, this, "height", Variant::Integer(height.max(0)));
    }
    if let Some(opacity) = request.opacity {
        set_layer_property_storage(
            runtime,
            this,
            "opacity",
            Variant::Integer(opacity.clamp(0, 255)),
        );
    }
}

fn encode_layer_bmp(width: u32, height: u32, rgba: &[u8], mode: &str) -> Result<Vec<u8>> {
    let pixel_bytes = match mode {
        "bmp" | "bmp32" => 4usize,
        "bmp24" => 3usize,
        "bmp8" => 1usize,
        _ => {
            return Err(TjsError::runtime(format!(
                "invalid image save type `{mode}`"
            )));
        }
    };
    let width_usize = width as usize;
    let height_usize = height as usize;
    let min_len = width_usize
        .checked_mul(height_usize)
        .and_then(|len| len.checked_mul(4))
        .ok_or_else(|| TjsError::runtime("Layer.saveLayerImage image is too large"))?;
    if rgba.len() < min_len {
        return Err(TjsError::runtime(
            "Layer.saveLayerImage image buffer is too small",
        ));
    }

    let row_stride = (width_usize * pixel_bytes).div_ceil(4) * 4;
    let palette_size = if pixel_bytes == 1 { 1024 } else { 0 };
    let pixel_offset = 14 + 40 + palette_size;
    let file_size = pixel_offset + row_stride * height_usize;
    let mut out = Vec::with_capacity(file_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&((pixel_bytes * 8) as u16).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    if pixel_bytes == 1 {
        for r in 0..6u16 {
            for g in 0..7u16 {
                for b in 0..6u16 {
                    out.extend_from_slice(&[
                        (r * 255 / 5) as u8,
                        (g * 255 / 6) as u8,
                        (b * 255 / 5) as u8,
                        0,
                    ]);
                }
            }
        }
        for _ in 252..256 {
            out.extend_from_slice(&[0, 0, 0, 0]);
        }
    }

    let mut row = vec![0u8; row_stride];
    for y in (0..height_usize).rev() {
        row.fill(0);
        for x in 0..width_usize {
            let src = (y * width_usize + x) * 4;
            let r = rgba[src];
            let g = rgba[src + 1];
            let b = rgba[src + 2];
            let a = rgba[src + 3];
            match pixel_bytes {
                4 => {
                    let dest = x * 4;
                    row[dest] = b;
                    row[dest + 1] = g;
                    row[dest + 2] = r;
                    row[dest + 3] = a;
                }
                3 => {
                    let dest = x * 3;
                    row[dest] = b;
                    row[dest + 1] = g;
                    row[dest + 2] = r;
                }
                1 => {
                    let ri = nearest_palette_index(r, 5);
                    let gi = nearest_palette_index(g, 6);
                    let bi = nearest_palette_index(b, 5);
                    row[x] = (ri * 42 + gi * 6 + bi) as u8;
                }
                _ => unreachable!("known pixel width"),
            }
        }
        out.extend_from_slice(&row);
    }
    Ok(out)
}

fn nearest_palette_index(value: u8, max_index: u16) -> u16 {
    ((value as u16 * max_index + 127) / 255).min(max_index)
}

fn load_images_storage(runtime: &Runtime<KrkrHost>, value: &Variant) -> Result<Option<String>> {
    if let Some(object) = source_object(value) {
        return match runtime.object_member(object, "storage") {
            Variant::Void => Ok(None),
            storage => storage.to_tjs_string().map(Some),
        };
    }
    value.to_tjs_string().map(Some)
}

fn object_optional_integer(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Option<Result<i64>> {
    match runtime.object_member(object, name) {
        Variant::Void => None,
        value => Some(value.to_integer()),
    }
}

fn object_optional_real(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Option<Result<f64>> {
    match runtime.object_member(object, name) {
        Variant::Void => None,
        value => Some(value.to_real()),
    }
}

fn object_optional_string(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Result<Option<String>> {
    match runtime.object_member(object, name) {
        Variant::Void => Ok(None),
        value => value.to_tjs_string().map(Some),
    }
}

fn transition_params_from_options(
    runtime: &mut Runtime<KrkrHost>,
    method: &str,
    options: Option<ObjectHandle>,
) -> Result<(TransitionParams, Option<ImageUpload>)> {
    let method = method.to_ascii_lowercase();
    let mut params = TransitionParams {
        method: TransitionMethod::from_name(&method),
        ..TransitionParams::default()
    };
    match params.method {
        TransitionMethod::RotateVanish => {
            params.accel = 2.0;
            params.twist_accel = 2.0;
        }
        TransitionMethod::RotateSwap => {
            params.twist = 1.0;
        }
        _ => {}
    }

    let Some(options) = options else {
        return Ok((params, None));
    };

    if let Some(value) = object_optional_real(runtime, options, "vague").transpose()? {
        params.vague = (value as f32).max(0.0);
    }
    if let Some(value) = object_optional_scroll_from(runtime, options, "from")? {
        params.scroll_from = value;
    }
    if let Some(value) = object_optional_scroll_stay(runtime, options, "stay")? {
        params.scroll_stay = value;
    }
    if let Some(value) = object_optional_real(runtime, options, "wavetype").transpose()? {
        params.wave_type = value as f32;
    }
    if let Some(value) = object_optional_real(runtime, options, "maxh").transpose()? {
        params.max_h = (value as f32).max(0.0);
    }
    if let Some(value) = object_optional_real(runtime, options, "maxomega").transpose()? {
        params.max_omega = (value as f32).max(0.0);
    }
    if let Some(value) = object_optional_color(runtime, options, "bgcolor1")? {
        params.bg_color1 = value;
    }
    if let Some(value) = object_optional_color(runtime, options, "bgcolor2")? {
        params.bg_color2 = value;
    }
    if let Some(value) = object_optional_real(runtime, options, "maxsize").transpose()? {
        params.max_size = (value as f32).max(1.0);
    }
    if let Some(value) = object_optional_color(runtime, options, "bgcolor")? {
        params.bg_color = value;
    }
    if let Some(value) = object_optional_real(runtime, options, "factor").transpose()? {
        params.factor = (value as f32).max(0.0);
    }
    if let Some(value) = object_optional_real(runtime, options, "accel").transpose()? {
        params.accel = value as f32;
    }
    if let Some(value) = object_optional_real(runtime, options, "twist").transpose()? {
        params.twist = value as f32;
    }
    if let Some(value) = object_optional_real(runtime, options, "twistaccel").transpose()? {
        params.twist_accel = value as f32;
    }
    if let Some(value) = object_optional_real(runtime, options, "centerx").transpose()? {
        params.center_x = value as f32;
    }
    if let Some(value) = object_optional_real(runtime, options, "centery").transpose()? {
        params.center_y = value as f32;
    }
    if let Some(value) = object_optional_real(runtime, options, "rwidth").transpose()? {
        params.ripple_width = (value as f32).max(1.0);
    }
    if let Some(value) = object_optional_real(runtime, options, "roundness").transpose()? {
        params.roundness = (value as f32).max(0.01);
    }
    if let Some(value) = object_optional_real(runtime, options, "speed").transpose()? {
        params.speed = (value as f32).max(0.01);
    }
    if let Some(value) = object_optional_real(runtime, options, "maxdrift").transpose()? {
        params.max_drift = (value as f32).max(0.0);
    }

    let rule_image_upload = if params.method == TransitionMethod::Universal {
        match object_optional_string(runtime, options, "rule")? {
            Some(rule) if !rule.is_empty() => {
                Some(runtime.host_mut().load_image_storage(&rule)?.upload)
            }
            _ => None,
        }
    } else {
        None
    };
    Ok((params, rule_image_upload))
}

fn object_optional_scroll_from(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Result<Option<TransitionScrollFrom>> {
    match runtime.object_member(object, name) {
        Variant::Void => Ok(None),
        Variant::String(value) => Ok(Some(match value.as_str() {
            "left" => TransitionScrollFrom::Left,
            "top" => TransitionScrollFrom::Top,
            "right" => TransitionScrollFrom::Right,
            "bottom" => TransitionScrollFrom::Bottom,
            _ => TransitionScrollFrom::Left,
        })),
        value => Ok(Some(match value.to_integer()? {
            1 => TransitionScrollFrom::Top,
            2 => TransitionScrollFrom::Right,
            3 => TransitionScrollFrom::Bottom,
            _ => TransitionScrollFrom::Left,
        })),
    }
}

fn object_optional_scroll_stay(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Result<Option<TransitionScrollStay>> {
    match runtime.object_member(object, name) {
        Variant::Void => Ok(None),
        Variant::String(value) => Ok(Some(match value.as_str() {
            "stayfore" => TransitionScrollStay::StayDest,
            "stayback" => TransitionScrollStay::StaySrc,
            _ => TransitionScrollStay::NoStay,
        })),
        value => Ok(Some(match value.to_integer()? {
            1 => TransitionScrollStay::StayDest,
            2 => TransitionScrollStay::StaySrc,
            _ => TransitionScrollStay::NoStay,
        })),
    }
}

fn object_optional_color(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Result<Option<Color>> {
    let value = runtime.object_member(object, name);
    if matches!(value, Variant::Void) {
        return Ok(None);
    }
    let color = match value {
        Variant::String(text) => match text.as_str() {
            "black" => 0x000000,
            "white" => 0xffffff,
            "red" => 0xff0000,
            "green" => 0x00ff00,
            "blue" => 0x0000ff,
            _ => {
                let text = text
                    .strip_prefix("0x")
                    .or_else(|| text.strip_prefix("0X"))
                    .or_else(|| text.strip_prefix('#'))
                    .unwrap_or(&text);
                i64::from_str_radix(text, 16).unwrap_or(0)
            }
        },
        value => value.to_integer()?,
    };
    Ok(Some(Color::rgb_u8(
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
    )))
}

fn source_object(value: &Variant) -> Option<ObjectHandle> {
    match value {
        Variant::Object(handle) => Some(*handle),
        _ => None,
    }
}

fn layer_free_image(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.clear_image();
    }
    runtime.host_mut().clear_layer_image_storage(layer_id);
    sync_layer_image_members(runtime, this, 0, 0);
    Ok(Variant::Void)
}

fn layer_set_default_cursor(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Layer.setDefaultCursor requires this"))?;
    let cursor = args.first().cloned().unwrap_or_default();
    runtime.set_object_member(this, "cursor", cursor.clone());
    runtime.set_object_member(this, "defaultCursor", cursor);
    Ok(Variant::Void)
}

fn layer_set_pos(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let left = optional_integer(&args, 0)?.unwrap_or(0);
    let top = optional_integer(&args, 1)?.unwrap_or(0);
    let width = optional_integer(&args, 2)?.map(|value| value.max(0));
    let height = optional_integer(&args, 3)?.map(|value| value.max(0));
    if let Some(target) = target {
        mutate_render_layer(runtime, &target, |layer| {
            layer.left = left as f32;
            layer.top = top as f32;
            if let Some(width) = width {
                layer.width = width as f32;
            }
            if let Some(height) = height {
                layer.height = height as f32;
            }
        });
    }
    set_layer_property_storage(runtime, this, "left", Variant::Integer(left));
    set_layer_property_storage(runtime, this, "top", Variant::Integer(top));
    if let Some(width) = width {
        set_layer_property_storage(runtime, this, "width", Variant::Integer(width));
    }
    if let Some(height) = height {
        set_layer_property_storage(runtime, this, "height", Variant::Integer(height));
    }
    Ok(Variant::Void)
}

fn layer_set_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let width = optional_integer(&args, 0)?.unwrap_or(0).max(0);
    let height = optional_integer(&args, 1)?.unwrap_or(0).max(0);
    if let Some(target) = target {
        mutate_render_layer(runtime, &target, |layer| {
            layer.width = width as f32;
            layer.height = height as f32;
        });
    }
    set_layer_property_storage(runtime, this, "width", Variant::Integer(width));
    set_layer_property_storage(runtime, this, "height", Variant::Integer(height));
    Ok(Variant::Void)
}

fn layer_set_image_pos(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let left = optional_integer(&args, 0)?.unwrap_or(0);
    let top = optional_integer(&args, 1)?.unwrap_or(0);
    if let Some(target) = target {
        mutate_render_layer(runtime, &target, |layer| {
            layer.image_left = left as f32;
            layer.image_top = top as f32;
        });
    }
    set_layer_property_storage(runtime, this, "imageLeft", Variant::Integer(left));
    set_layer_property_storage(runtime, this, "imageTop", Variant::Integer(top));
    Ok(Variant::Void)
}

fn layer_set_image_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let width = optional_integer(&args, 0)?.unwrap_or(0).max(0);
    let height = optional_integer(&args, 1)?.unwrap_or(0).max(0);
    let existing_image = target
        .as_ref()
        .and_then(|target| render_layer_snapshot(runtime, target))
        .and_then(|layer| layer.image);
    let image = resize_layer_image(runtime, existing_image, width as u32, height as u32);
    if let Some(target) = target {
        mutate_render_layer(runtime, &target, |layer| {
            layer.image_width = width as f32;
            layer.image_height = height as f32;
            layer.image = image;
        });
    }
    set_layer_property_storage(runtime, this, "imageWidth", Variant::Integer(width));
    set_layer_property_storage(runtime, this, "imageHeight", Variant::Integer(height));
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

fn resize_layer_image(
    runtime: &mut Runtime<KrkrHost>,
    existing: Option<LayerImage>,
    width: u32,
    height: u32,
) -> Option<LayerImage> {
    if width == 0 || height == 0 {
        return None;
    }

    let Some(existing) = existing else {
        return Some(runtime.host_mut().create_layer_image(
            width,
            height,
            vec![0; width as usize * height as usize * 4],
        ));
    };

    if existing.upload.width == width && existing.upload.height == height {
        return Some(existing);
    }

    let mut rgba = vec![0; width as usize * height as usize * 4];
    let copy_width = existing.upload.width.min(width) as usize;
    let copy_height = existing.upload.height.min(height) as usize;
    let source_stride = existing.upload.width as usize * 4;
    let dest_stride = width as usize * 4;
    let copy_len = copy_width * 4;
    for row in 0..copy_height {
        let source_start = row * source_stride;
        let dest_start = row * dest_stride;
        rgba[dest_start..dest_start + copy_len]
            .copy_from_slice(&existing.upload.rgba[source_start..source_start + copy_len]);
    }

    Some(runtime.host_mut().create_layer_image(width, height, rgba))
}

fn layer_set_size_to_image_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let width = layer_property_value(runtime, this, "imageWidth")
        .to_integer()?
        .max(0);
    let height = layer_property_value(runtime, this, "imageHeight")
        .to_integer()?
        .max(0);
    let replacement_image = if width > 0 && height > 0 {
        let needs_image = target
            .as_ref()
            .and_then(|target| render_layer_snapshot(runtime, target))
            .and_then(|layer| layer.image)
            .map(|image| image.upload.width != width as u32 || image.upload.height != height as u32)
            .unwrap_or(true);
        needs_image.then(|| {
            runtime.host_mut().create_layer_image(
                width as u32,
                height as u32,
                vec![0; width as usize * height as usize * 4],
            )
        })
    } else {
        None
    };
    let replaces_content = width == 0 || height == 0 || replacement_image.is_some();
    if let Some(target) = target {
        mutate_render_layer(runtime, &target, |layer| {
            layer.image_width = width as f32;
            layer.image_height = height as f32;
            layer.width = width as f32;
            layer.height = height as f32;
            if width == 0 || height == 0 {
                layer.image = None;
            } else if let Some(image) = replacement_image {
                layer.image = Some(image);
            }
        });
    }
    set_layer_property_storage(runtime, this, "width", Variant::Integer(width));
    set_layer_property_storage(runtime, this, "height", Variant::Integer(height));
    if replaces_content {
        mark_image_modified(runtime, this);
    }
    Ok(Variant::Void)
}

fn layer_assign_images(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let Some(source) = args.first().and_then(variant_object) else {
        return Ok(Variant::Void);
    };
    if let Some(target) = target {
        copy_layer_images(runtime, this, &target, source)?;
    }
    Ok(Variant::Void)
}

fn layer_exchange_info(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    let Some(comp) = variant_object(&runtime.object_member(this, "comp"))
        .map(|comp| runtime.bound_this(comp).unwrap_or(comp))
    else {
        return Ok(Variant::Void);
    };
    exchange_native_layer_info(runtime, this, comp)
}

fn exchange_native_layer_info(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    comp: ObjectHandle,
) -> Result<Variant> {
    let Some(this_layer_id) = native_layer_id(runtime, this)? else {
        return Ok(Variant::Void);
    };
    let Some(comp_layer_id) = native_layer_id(runtime, comp)? else {
        return Ok(Variant::Void);
    };

    let Some(this_layer) = runtime.host().layer_tree().layer(this_layer_id).cloned() else {
        return Ok(Variant::Void);
    };
    let Some(comp_layer) = runtime.host().layer_tree().layer(comp_layer_id).cloned() else {
        return Ok(Variant::Void);
    };
    let this_is_primary = layer_property_value(runtime, this, "isPrimary").is_truthy();
    let comp_is_primary = layer_property_value(runtime, comp, "isPrimary").is_truthy();

    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(this_layer_id) {
        copy_render_state(layer, &comp_layer);
    }
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(comp_layer_id) {
        copy_render_state(layer, &this_layer);
    }
    apply_layer_node_state_to_script(runtime, this, &comp_layer);
    apply_layer_node_state_to_script(runtime, comp, &this_layer);
    set_layer_property_storage(
        runtime,
        this,
        "isPrimary",
        Variant::Integer(i64::from(comp_is_primary)),
    );
    set_layer_property_storage(
        runtime,
        comp,
        "isPrimary",
        Variant::Integer(i64::from(this_is_primary)),
    );

    Ok(Variant::Void)
}

fn layer_begin_transition(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    finish_current_transition(runtime)?;
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    let source = args
        .get(2)
        .and_then(variant_object)
        .or_else(|| variant_object(&runtime.object_member(this, "comp")));
    let with_children = args
        .get(1)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(1)
        != 0;
    let method = args
        .first()
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_else(|| "crossfade".to_string());
    let options = args.get(3).and_then(variant_object);
    let duration = match options {
        Some(options) => object_optional_integer(runtime, options, "time")
            .transpose()?
            .unwrap_or(0),
        None => 0,
    }
    .max(0) as u64;
    let (transition_params, rule_image_upload) =
        transition_params_from_options(runtime, &method, options)?;
    let source_layer_id = source
        .map(|source| native_layer_id(runtime, source))
        .transpose()?
        .flatten();
    let mut suppressed_images = BTreeSet::new();
    if let Some(source_layer_id) = source_layer_id {
        suppressed_images.insert(source_layer_id);
    }
    let frozen = runtime
        .host()
        .layer_tree()
        .draw_model_suppressing_images(&suppressed_images);
    let comp = variant_object(&runtime.object_member(this, "comp"))
        .map(|comp| runtime.bound_this(comp).unwrap_or(comp));
    let paired_comp = source
        .map(|source| runtime.bound_this(source).unwrap_or(source))
        .is_some_and(|source| Some(source) == comp);
    if let Some(target) = render_layer_target(runtime, this)? {
        if let Some(source) = source {
            materialize_kag_back_to_native(runtime, source)?;
            copy_layer_images(runtime, this, &target, source)?;
        }
        mutate_render_layer(runtime, &target, |layer| {
            layer.visible = true;
            layer.renderable = true;
        });
    }
    set_layer_property_storage(runtime, this, "visible", Variant::Integer(1));
    let live_layer_overrides =
        kag_base_children_transition_live_overrides(runtime, this, source, with_children)?;
    if duration == 0 {
        if !paired_comp
            && let Some(source_layer_id) = source_layer_id
            && let Some(source_layer) = runtime
                .host_mut()
                .layer_tree_mut()
                .layer_mut(source_layer_id)
        {
            source_layer.renderable = false;
        }
        finish_immediate_transition(runtime, this, source)?;
    } else {
        runtime.host_mut().begin_native_transition(
            Duration::from_millis(duration),
            transition_params,
            rule_image_upload,
            frozen,
            suppressed_images,
            live_layer_overrides,
            NativeTransitionCompletion {
                dest: this,
                source,
                paired_comp,
            },
        );
    }
    Ok(Variant::Void)
}

fn materialize_kag_back_to_native(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
) -> Result<()> {
    let Some(LayerRenderTarget::Kag(slot)) = kag_layer_target(runtime, handle) else {
        return Ok(());
    };
    if slot.page != "back" {
        return Ok(());
    }
    let Some(layer_id) = native_layer_id(runtime, handle)? else {
        return Ok(());
    };
    let Some(snapshot) = runtime.host().kag_layer(&slot.page, &slot.layer).cloned() else {
        return Ok(());
    };
    if let Some(native_layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        let renderable = native_layer.renderable;
        copy_render_content(native_layer, &snapshot);
        native_layer.renderable = renderable;
    }
    Ok(())
}

fn kag_base_children_transition_live_overrides(
    runtime: &mut Runtime<KrkrHost>,
    dest: ObjectHandle,
    source: Option<ObjectHandle>,
    with_children: bool,
) -> Result<BTreeMap<u64, LayerNode>> {
    if !with_children {
        return Ok(BTreeMap::new());
    }
    if !matches!(
        kag_layer_target(runtime, dest),
        Some(LayerRenderTarget::Kag(slot)) if slot.page == "fore" && slot.layer == "base"
    ) {
        return Ok(BTreeMap::new());
    }
    let Some(source) = source else {
        return Ok(BTreeMap::new());
    };
    if !matches!(
        kag_layer_target(runtime, source),
        Some(LayerRenderTarget::Kag(slot)) if slot.page == "back" && slot.layer == "base"
    ) {
        return Ok(BTreeMap::new());
    }

    let mut overrides = BTreeMap::new();
    let pending_layers = runtime.host().pending_kag_layer_names();
    for layer_name in pending_layers {
        if layer_name == "base" {
            // Base itself is the transition target copied in layer_begin_transition.
            // This path only projects staged child/message layers into the live tree.
            continue;
        }
        let Some(source_layer) = kag_layer_object_snapshot(runtime, "back", &layer_name)? else {
            continue;
        };
        if let Some(back_handle) = kag_page_layer_handle(runtime, "back", &layer_name)
            && let Some(back_layer_id) = native_layer_id(runtime, back_handle)?
            && let Some(back_layer) = runtime.host_mut().layer_tree_mut().layer_mut(back_layer_id)
        {
            copy_render_content(back_layer, &source_layer);
            back_layer.renderable = false;
        }

        let Some(fore_handle) = kag_page_layer_handle(runtime, "fore", &layer_name) else {
            continue;
        };
        let Some(layer_id) = native_layer_id(runtime, fore_handle)? else {
            continue;
        };
        let Some(mut override_layer) = runtime.host().layer_tree().layer(layer_id).cloned() else {
            continue;
        };
        copy_render_content(&mut override_layer, &source_layer);
        override_layer.renderable = true;
        if let Some(dest_layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
            copy_render_content(dest_layer, &source_layer);
            dest_layer.renderable = true;
        }
        overrides.insert(layer_id, override_layer);
    }
    Ok(overrides)
}

fn kag_layer_object_snapshot(
    runtime: &Runtime<KrkrHost>,
    page: &str,
    layer: &str,
) -> Result<Option<LayerNode>> {
    let Some(mut snapshot) = runtime.host().kag_layer(page, layer).cloned() else {
        return Ok(None);
    };
    if layer == "base" || layer == "background" {
        return Ok(Some(snapshot));
    }
    let Some(handle) = kag_page_layer_handle(runtime, page, layer) else {
        return Ok(Some(snapshot));
    };
    apply_script_layer_members(runtime, handle, &mut snapshot)?;
    Ok(Some(snapshot))
}

fn apply_script_layer_members(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
    layer: &mut LayerNode,
) -> Result<()> {
    layer.left = layer_member_i64(runtime, handle, "left", layer.left as i64)? as f32;
    layer.top = layer_member_i64(runtime, handle, "top", layer.top as i64)? as f32;
    layer.width =
        layer_member_i64(runtime, handle, "width", layer.width.max(0.0) as i64)?.max(0) as f32;
    layer.height =
        layer_member_i64(runtime, handle, "height", layer.height.max(0.0) as i64)?.max(0) as f32;
    layer.image_left =
        layer_member_i64(runtime, handle, "imageLeft", layer.image_left as i64)? as f32;
    layer.image_top = layer_member_i64(runtime, handle, "imageTop", layer.image_top as i64)? as f32;
    layer.image_width = layer_member_i64(
        runtime,
        handle,
        "imageWidth",
        layer.image_width.max(0.0) as i64,
    )?
    .max(0) as f32;
    layer.image_height = layer_member_i64(
        runtime,
        handle,
        "imageHeight",
        layer.image_height.max(0.0) as i64,
    )?
    .max(0) as f32;
    layer.visible = layer_member_i64(runtime, handle, "visible", i64::from(layer.visible))? != 0;
    layer.opacity =
        layer_member_i64(runtime, handle, "opacity", i64::from(layer.opacity))?.clamp(0, 255) as u8;
    layer.enabled = layer_member_i64(runtime, handle, "enabled", i64::from(layer.enabled))? != 0;
    layer.node_enabled = layer_member_i64(
        runtime,
        handle,
        "nodeEnabled",
        i64::from(layer.node_enabled),
    )? != 0;
    layer.layer_type =
        layer_member_i64(runtime, handle, "type", i64::from(layer.layer_type))? as i32;
    layer.face = layer_member_i64(runtime, handle, "face", i64::from(layer.face))? as i32;
    layer.hit_type =
        layer_member_i64(runtime, handle, "hitType", i64::from(layer.hit_type))? as i32;
    layer.hit_threshold = layer_member_i64(
        runtime,
        handle,
        "hitThreshold",
        i64::from(layer.hit_threshold),
    )?
    .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    Ok(())
}

fn layer_member_i64(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &str,
    fallback: i64,
) -> Result<i64> {
    layer_property_i64(runtime, handle, name, fallback)
}

fn kag_page_layer_handle(
    runtime: &Runtime<KrkrHost>,
    page: &str,
    layer: &str,
) -> Option<ObjectHandle> {
    let Variant::Object(kag) = runtime.global_member("kag") else {
        return None;
    };
    let Variant::Object(page_object) = runtime.object_member(kag, page) else {
        return None;
    };
    if layer == "base" || layer == "background" {
        return variant_object(&runtime.object_member(page_object, "base"))
            .map(|handle| runtime.bound_this(handle).unwrap_or(handle));
    }

    let (array_name, index) = if let Some(index) = layer.strip_prefix("message") {
        ("messages", index)
    } else {
        ("layers", layer)
    };
    let Variant::Object(array) = runtime.object_member(page_object, array_name) else {
        return None;
    };
    variant_object(&runtime.object_member(array, index))
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

fn copy_render_state(dest: &mut LayerNode, source: &LayerNode) {
    copy_render_content(dest, source);
    dest.renderable = source.renderable;
}

fn apply_layer_node_state_to_script(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    layer: &LayerNode,
) {
    for (name, value) in [
        ("left", layer.left.round() as i64),
        ("top", layer.top.round() as i64),
        ("width", layer.width.max(0.0).round() as i64),
        ("height", layer.height.max(0.0).round() as i64),
        ("imageLeft", layer.image_left.round() as i64),
        ("imageTop", layer.image_top.round() as i64),
        ("imageWidth", layer.image_width.max(0.0).round() as i64),
        ("imageHeight", layer.image_height.max(0.0).round() as i64),
        ("visible", i64::from(layer.visible)),
        ("enabled", i64::from(layer.enabled)),
        ("nodeEnabled", i64::from(layer.node_enabled)),
        ("opacity", i64::from(layer.opacity)),
        ("type", i64::from(layer.layer_type)),
        ("face", i64::from(layer.face)),
        ("hitType", i64::from(layer.hit_type)),
        ("hitThreshold", i64::from(layer.hit_threshold)),
    ] {
        set_layer_property_storage(runtime, handle, name, Variant::Integer(value));
    }
}

fn copy_render_content(dest: &mut LayerNode, source: &LayerNode) {
    dest.left = source.left;
    dest.top = source.top;
    dest.width = source.width;
    dest.height = source.height;
    dest.image_left = source.image_left;
    dest.image_top = source.image_top;
    dest.image_width = source.image_width;
    dest.image_height = source.image_height;
    dest.visible = source.visible;
    dest.enabled = source.enabled;
    dest.node_enabled = source.node_enabled;
    dest.opacity = source.opacity;
    dest.layer_type = source.layer_type;
    dest.face = source.face;
    dest.hit_type = source.hit_type;
    dest.hit_threshold = source.hit_threshold;
    dest.image = source.image.clone();
}

fn layer_stop_transition(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    runtime.host_mut().complete_native_transition_for(this);
    finish_completed_native_transitions(runtime)?;
    Ok(Variant::Void)
}

fn layer_fill_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let Some((x, y, width, height)) = rect_args(&args)? else {
        return Ok(Variant::Void);
    };
    let color = required_integer(&args, 4, "Layer.fillRect color")?;
    let rgba = if layer_property_value(runtime, this, "neutralColor")
        .to_integer()
        .is_ok_and(|neutral| neutral == color)
    {
        packed_color_to_rgba(color)
    } else {
        color_to_rgba(color, None)
    };
    if let Some(target) = target {
        fill_layer_pixels(runtime, &target, x, y, width, height, rgba)?;
        mark_image_modified(runtime, this);
    }
    Ok(Variant::Void)
}

fn layer_color_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let Some((x, y, width, height)) = rect_args(&args)? else {
        return Ok(Variant::Void);
    };
    let color = required_integer(&args, 4, "Layer.colorRect color")?;
    let opacity = optional_integer(&args, 5)?;
    let rgba = color_to_rgba(color, opacity);
    if let Some(target) = target {
        fill_layer_pixels(runtime, &target, x, y, width, height, rgba)?;
        mark_image_modified(runtime, this);
    }
    Ok(Variant::Void)
}

fn layer_copy_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    copy_rect_impl(runtime, this_obj, args, false)
}

fn layer_operate_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    copy_rect_impl(runtime, this_obj, args, true)
}

fn layer_piled_copy(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, dest_target) = this_render_layer_target(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let dx = optional_integer(&args, 0)?.unwrap_or(0);
    let dy = optional_integer(&args, 1)?.unwrap_or(0);
    let Some(source_object) = args.get(2).and_then(variant_object) else {
        return Ok(Variant::Void);
    };
    let sx = optional_integer(&args, 3)?.unwrap_or(0);
    let sy = optional_integer(&args, 4)?.unwrap_or(0);
    let width = optional_integer(&args, 5)?.unwrap_or(0);
    let height = optional_integer(&args, 6)?.unwrap_or(0);
    if width <= 0 || height <= 0 {
        return Ok(Variant::Void);
    }
    let Some(dest_target) = dest_target else {
        return Ok(Variant::Void);
    };

    complete_layer_subtree_before_draw(runtime, source_object, &mut BTreeSet::new())?;
    register_kag_layer_slots_from_tjs(runtime);
    let mut layers = Vec::new();
    let mut visited = BTreeSet::new();
    collect_piled_render_layers(
        runtime,
        source_object,
        0.0,
        0.0,
        None,
        1.0,
        false,
        &mut visited,
        &mut layers,
    );
    if layers.is_empty() {
        return Ok(Variant::Void);
    }

    mutate_layer_pixels_min(
        runtime,
        &dest_target,
        dest_min_extent(dx, width),
        dest_min_extent(dy, height),
        |pixels, image_width, image_height| {
            for layer in &layers {
                composite_piled_layer(
                    pixels,
                    image_width,
                    image_height,
                    layer,
                    dx,
                    dy,
                    sx,
                    sy,
                    width,
                    height,
                );
            }
        },
    )?;
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

fn layer_stretch_copy(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, dest_target) = this_render_layer_target(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let dx = optional_integer(&args, 0)?.unwrap_or(0);
    let dy = optional_integer(&args, 1)?.unwrap_or(0);
    let dest_width = optional_integer(&args, 2)?.unwrap_or(0);
    let dest_height = optional_integer(&args, 3)?.unwrap_or(0);
    let Some(source_object) = args.get(4).and_then(variant_object) else {
        return Ok(Variant::Void);
    };
    let sx = optional_integer(&args, 5)?.unwrap_or(0);
    let sy = optional_integer(&args, 6)?.unwrap_or(0);
    let source_width = optional_integer(&args, 7)?.unwrap_or(0);
    let source_height = optional_integer(&args, 8)?.unwrap_or(0);
    if dest_width <= 0 || dest_height <= 0 || source_width <= 0 || source_height <= 0 {
        return Ok(Variant::Void);
    }
    let Some(dest_target) = dest_target else {
        return Ok(Variant::Void);
    };
    complete_layer_before_draw(runtime, source_object)?;
    let Some(source_target) = render_layer_target(runtime, source_object)? else {
        return Ok(Variant::Void);
    };
    let Some(source_image) =
        render_layer_snapshot(runtime, &source_target).and_then(|layer| layer.image)
    else {
        return Ok(Variant::Void);
    };
    let source_pixels = source_image.upload.rgba.as_ref().to_vec();
    let source_texture_width = source_image.upload.width;
    let source_texture_height = source_image.upload.height;

    mutate_layer_pixels_min(
        runtime,
        &dest_target,
        dest_min_extent(dx, dest_width),
        dest_min_extent(dy, dest_height),
        |pixels, image_width, image_height| {
            stretch_copy_pixels(
                pixels,
                image_width,
                image_height,
                &source_pixels,
                source_texture_width,
                source_texture_height,
                dx,
                dy,
                dest_width,
                dest_height,
                sx,
                sy,
                source_width,
                source_height,
            );
        },
    )?;
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

// KRKR2 Layer.affineCopy(src, sx, sy, sw, sh, affine,
//                         x0/a, y0/b, x1/c, y1/d, x2/tx, y2/ty, mode=0)
// performs an opaque affine blit. The points form maps source (0,0),
// (sw,0), (0,sh); matrix mode supplies the equivalent affine coefficients.
fn layer_affine_copy(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if args.len() < 12 {
        return Err(TjsError::runtime("Layer.affineCopy requires 12 arguments"));
    }
    let (this, dest_target) = this_render_layer_target(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let Some(source_object) = args.first().and_then(variant_object) else {
        return Err(TjsError::runtime(
            "Layer.affineCopy requires a source Layer",
        ));
    };
    let sx = args[1].to_integer()?;
    let sy = args[2].to_integer()?;
    let source_width = args[3].to_integer()?;
    let source_height = args[4].to_integer()?;
    if source_width <= 0 || source_height <= 0 {
        return Ok(Variant::Void);
    }
    let affine = args[5].is_truthy();
    let values = args[6..12]
        .iter()
        .map(Variant::to_real)
        .collect::<Result<Vec<_>>>()?;
    let Some(dest_target) = dest_target else {
        return Ok(Variant::Void);
    };
    complete_layer_before_draw(runtime, source_object)?;
    let Some(source_target) = render_layer_target(runtime, source_object)? else {
        return Ok(Variant::Void);
    };
    let Some(source_image) =
        render_layer_snapshot(runtime, &source_target).and_then(|layer| layer.image)
    else {
        return Ok(Variant::Void);
    };
    let source_pixels = source_image.upload.rgba.as_ref().to_vec();
    let texture_width = source_image.upload.width;
    let texture_height = source_image.upload.height;
    let points = if affine {
        let (a, b, c, d, tx, ty) = (
            values[0], values[1], values[2], values[3], values[4], values[5],
        );
        [
            (tx, ty),
            (a * source_width as f64 + tx, b * source_width as f64 + ty),
            (c * source_height as f64 + tx, d * source_height as f64 + ty),
        ]
    } else {
        [
            (values[0], values[1]),
            (values[2], values[3]),
            (values[4], values[5]),
        ]
    };
    mutate_layer_pixels(runtime, &dest_target, |pixels, dest_width, dest_height| {
        affine_copy_pixels(
            pixels,
            dest_width,
            dest_height,
            &source_pixels,
            texture_width,
            texture_height,
            sx,
            sy,
            source_width,
            source_height,
            points,
        );
    })?;
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

fn copy_rect_impl(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
    alpha_blend: bool,
) -> Result<Variant> {
    let (this, dest_target) = this_render_layer_target(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let dx = optional_integer(&args, 0)?.unwrap_or(0);
    let dy = optional_integer(&args, 1)?.unwrap_or(0);
    let Some(source_object) = args.get(2).and_then(variant_object) else {
        return Ok(Variant::Void);
    };
    let sx = optional_integer(&args, 3)?.unwrap_or(0);
    let sy = optional_integer(&args, 4)?.unwrap_or(0);
    let width = optional_integer(&args, 5)?.unwrap_or(0);
    let height = optional_integer(&args, 6)?.unwrap_or(0);
    if width <= 0 || height <= 0 {
        return Ok(Variant::Void);
    }
    let Some(dest_target) = dest_target else {
        return Ok(Variant::Void);
    };
    complete_layer_before_draw(runtime, source_object)?;
    let Some(source_target) = render_layer_target(runtime, source_object)? else {
        return Ok(Variant::Void);
    };
    let Some(source_image) =
        render_layer_snapshot(runtime, &source_target).and_then(|layer| layer.image)
    else {
        return Ok(Variant::Void);
    };
    let source_pixels = source_image.upload.rgba.as_ref().to_vec();
    let source_width = source_image.upload.width;
    let source_height = source_image.upload.height;

    mutate_layer_pixels(
        runtime,
        &dest_target,
        |pixels, image_width, image_height| {
            copy_pixels(
                pixels,
                image_width,
                image_height,
                &source_pixels,
                source_width,
                source_height,
                dx,
                dy,
                sx,
                sy,
                width,
                height,
                alpha_blend,
            );
        },
    )?;
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

fn layer_draw_text(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let Some(target) = target else {
        return Ok(Variant::Void);
    };
    let x = optional_integer(&args, 0)?.unwrap_or(0);
    let y = optional_integer(&args, 1)?.unwrap_or(0);
    let text = args
        .get(2)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let color = optional_integer(&args, 3)?.unwrap_or(0x00ff_ffff);
    let opacity = optional_integer(&args, 4)?;
    let font = layer_font_spec(runtime, this)?;
    ensure_font_file_loaded(runtime, &font)?;
    let style = TextStyle {
        color: color_to_rgba(color, opacity),
        anti_alias: optional_integer(&args, 5)?.is_none_or(|value| value != 0),
        shadow: None,
    };
    let effect = text_draw_effect(&args, opacity)?;
    let layout = runtime.host().font_system().layout_text(&font, &text);
    let metrics = layout.metrics();
    let min_width =
        (x.max(0) as f32 + metrics.width.ceil() + effect.max_right() as f32).max(1.0) as u32;
    let min_height =
        (y.max(0) as f32 + metrics.height.ceil() + effect.max_bottom() as f32).max(1.0) as u32;
    mutate_layer_pixels_min_with_host(
        runtime,
        &target,
        min_width,
        min_height,
        |host, pixels, width, height| {
            let font_system = host.font_system();
            effect.draw(
                font_system,
                &font,
                &layout,
                pixels,
                width,
                height,
                x as i32,
                y as i32,
            );
            font_system.draw_text_layout_to_rgba(
                &font, style, pixels, width, height, x as i32, y as i32, &layout,
            );
        },
    )?;
    runtime
        .host_mut()
        .record_native_text_draw(&target, text, x, y);
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

fn layer_draw_glyph(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let Some(target) = target else {
        return Ok(Variant::Void);
    };
    let x = optional_integer(&args, 0)?.unwrap_or(0);
    let y = optional_integer(&args, 1)?.unwrap_or(0);
    let glyph = args
        .get(2)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let text = glyph
        .chars()
        .next()
        .map(|ch| ch.to_string())
        .unwrap_or_default();
    if text.is_empty() {
        return Ok(Variant::Void);
    }
    let color = optional_integer(&args, 3)?.unwrap_or(0x00ff_ffff);
    let opacity = optional_integer(&args, 4)?;
    let font = layer_font_spec(runtime, this)?;
    ensure_font_file_loaded(runtime, &font)?;
    let style = TextStyle {
        color: color_to_rgba(color, opacity),
        anti_alias: optional_integer(&args, 5)?.is_none_or(|value| value != 0),
        shadow: None,
    };
    let effect = text_draw_effect(&args, opacity)?;
    let layout = runtime.host().font_system().layout_text(&font, &text);
    let metrics = layout.metrics();
    let min_width =
        (x.max(0) as f32 + metrics.width.ceil() + effect.max_right() as f32).max(1.0) as u32;
    let min_height =
        (y.max(0) as f32 + metrics.height.ceil() + effect.max_bottom() as f32).max(1.0) as u32;
    mutate_layer_pixels_min_with_host(
        runtime,
        &target,
        min_width,
        min_height,
        |host, pixels, width, height| {
            let font_system = host.font_system();
            effect.draw(
                font_system,
                &font,
                &layout,
                pixels,
                width,
                height,
                x as i32,
                y as i32,
            );
            font_system.draw_text_layout_to_rgba(
                &font, style, pixels, width, height, x as i32, y as i32, &layout,
            );
        },
    )?;
    runtime
        .host_mut()
        .record_native_text_draw(&target, text, x, y);
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

#[derive(Clone, Copy, Debug)]
struct TextDrawEffect {
    color: [u8; 4],
    width: i32,
    offset_x: i32,
    offset_y: i32,
    anti_alias: bool,
}

impl TextDrawEffect {
    fn none(anti_alias: bool) -> Self {
        Self {
            color: [0, 0, 0, 0],
            width: 0,
            offset_x: 0,
            offset_y: 0,
            anti_alias,
        }
    }

    fn is_visible(self) -> bool {
        self.color[3] != 0 && (self.width > 0 || self.offset_x != 0 || self.offset_y != 0)
    }

    fn max_right(self) -> i32 {
        if !self.is_visible() {
            return 0;
        }
        (self.offset_x + self.width).max(self.width).max(0)
    }

    fn max_bottom(self) -> i32 {
        if !self.is_visible() {
            return 0;
        }
        (self.offset_y + self.width).max(self.width).max(0)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        self,
        font_system: &FontSystem,
        font: &FontSpec,
        layout: &TextLayout,
        pixels: &mut [u8],
        width: u32,
        height: u32,
        x: i32,
        y: i32,
    ) {
        if !self.is_visible() {
            return;
        }
        let style = TextStyle {
            color: self.color,
            anti_alias: self.anti_alias,
            shadow: None,
        };
        if self.width > 0 && self.offset_x == 0 && self.offset_y == 0 {
            for dy in -self.width..=self.width {
                for dx in -self.width..=self.width {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    font_system.draw_text_layout_to_rgba(
                        font,
                        style,
                        pixels,
                        width,
                        height,
                        x + dx,
                        y + dy,
                        layout,
                    );
                }
            }
            return;
        }
        let spread = self.width.max(0);
        for dy in -spread..=spread {
            for dx in -spread..=spread {
                font_system.draw_text_layout_to_rgba(
                    font,
                    style,
                    pixels,
                    width,
                    height,
                    x + self.offset_x + dx,
                    y + self.offset_y + dy,
                    layout,
                );
            }
        }
    }
}

fn text_draw_effect(args: &[Variant], opacity: Option<i64>) -> Result<TextDrawEffect> {
    let anti_alias = optional_integer(args, 5)?.is_none_or(|value| value != 0);
    let Some(level) = optional_integer(args, 6)? else {
        return Ok(TextDrawEffect::none(anti_alias));
    };
    let effect_color = optional_integer(args, 7)?.unwrap_or(0);
    let effect_width = optional_integer(args, 8)?.unwrap_or(0).max(0) as i32;
    let offset_x = optional_integer(args, 9)?.unwrap_or(0) as i32;
    let offset_y = optional_integer(args, 10)?.unwrap_or(0) as i32;
    let level = level.clamp(0, 255);
    let alpha = opacity.map_or(level, |opacity| (level * opacity.clamp(0, 255) + 127) / 255);
    Ok(TextDrawEffect {
        color: color_to_rgba(effect_color, Some(alpha)),
        width: effect_width,
        offset_x,
        offset_y,
        anti_alias,
    })
}

fn image_function_draw_text(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    dispatch_image_function_layer_call(runtime, this_obj, args, layer_draw_text)
}

fn image_function_draw_glyph(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    dispatch_image_function_layer_call(runtime, this_obj, args, layer_draw_glyph)
}

fn dispatch_image_function_layer_call(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
    function: NativeMethod,
) -> Result<Variant> {
    if let Some(target) = args.first().and_then(variant_object)
        && native_layer_id(runtime, target)?.is_some()
    {
        return function(runtime, Some(target), args.into_iter().skip(1).collect());
    }
    if let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this))
        && native_layer_id(runtime, this)?.is_some()
    {
        return function(runtime, Some(this), args);
    }
    Ok(Variant::Void)
}

fn layer_update(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer.update requires this"))?;
    set_layer_property_storage(runtime, this, "callOnPaint", Variant::Integer(1));
    if !runtime.host_mut().request_layer_paint(this) {
        set_layer_property_storage(runtime, this, "callOnPaint", Variant::Integer(0));
    }
    Ok(Variant::Void)
}

fn layer_focus(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer.focus requires this"))?;
    if layer_set_focus_to(runtime, this, true)? {
        Ok(Variant::Object(this))
    } else {
        Ok(Variant::Null)
    }
}

fn layer_focus_prev(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer.focusPrev requires this"))?;
    layer_focus_relative(runtime, this, false)
}

fn layer_focus_next(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer.focusNext requires this"))?;
    layer_focus_relative(runtime, this, true)
}

fn layer_focus_relative(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    forward: bool,
) -> Result<Variant> {
    let Some(window) = layer_window_object(runtime, this) else {
        return Ok(Variant::Null);
    };
    let focusables = focusable_layers_for_window(runtime, window, this);
    if focusables.is_empty() {
        return Ok(Variant::Null);
    }

    let focused = focused_layer(runtime, window);
    let target = if let Some(focused) = focused {
        let Some(index) = focusables.iter().position(|layer| *layer == focused) else {
            return focus_first_layer(runtime, focusables[0], forward);
        };
        if focusables.len() == 1 {
            return Ok(Variant::Null);
        }
        let next_index = if forward {
            (index + 1) % focusables.len()
        } else {
            (index + focusables.len() - 1) % focusables.len()
        };
        focusables[next_index]
    } else {
        focusables[0]
    };

    let method = if forward {
        "onSearchNextFocusable"
    } else {
        "onSearchPrevFocusable"
    };
    let target = layer_focus_work(runtime, this, method, Some(target), Vec::new())?;
    let Some(target) = target else {
        return Ok(Variant::Null);
    };
    focus_first_layer(runtime, target, forward)
}

fn focus_first_layer(
    runtime: &mut Runtime<KrkrHost>,
    target: ObjectHandle,
    forward: bool,
) -> Result<Variant> {
    if layer_set_focus_to(runtime, target, forward)? {
        Ok(Variant::Object(target))
    } else {
        Ok(Variant::Null)
    }
}

fn layer_set_focus_to(
    runtime: &mut Runtime<KrkrHost>,
    target: ObjectHandle,
    direction: bool,
) -> Result<bool> {
    if !layer_is_node_focusable(runtime, target) {
        return Ok(false);
    }
    let Some(window) = layer_window_object(runtime, target) else {
        return Ok(false);
    };
    let previous = focused_layer(runtime, window);
    let target = layer_focus_work(
        runtime,
        target,
        "onBeforeFocus",
        Some(target),
        vec![
            previous.map(Variant::Object).unwrap_or(Variant::Null),
            Variant::Integer(i64::from(direction)),
        ],
    )?;
    let Some(target) = target else {
        return Ok(false);
    };
    if !layer_is_node_focusable(runtime, target) || previous == Some(target) {
        return Ok(false);
    }

    if let Some(previous) = previous {
        runtime.set_object_member(previous, "focused", Variant::Integer(0));
        if !matches!(runtime.object_member(previous, "onBlur"), Variant::Void) {
            runtime.call_object_method(previous, "onBlur", vec![Variant::Object(target)])?;
        }
    }

    runtime.set_object_member(target, "focused", Variant::Integer(1));
    set_window_property_storage(runtime, window, "focusedLayer", Variant::Object(target));
    if !matches!(runtime.object_member(target, "onFocus"), Variant::Void) {
        runtime.call_object_method(
            target,
            "onFocus",
            vec![
                previous.map(Variant::Object).unwrap_or(Variant::Null),
                Variant::Integer(i64::from(direction)),
            ],
        )?;
    }
    Ok(true)
}

fn layer_focus_work(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    method: &str,
    candidate: Option<ObjectHandle>,
    mut extra_args: Vec<Variant>,
) -> Result<Option<ObjectHandle>> {
    let candidate_value = candidate.map(Variant::Object).unwrap_or(Variant::Null);
    runtime.set_object_member(layer, "__nativeFocusWork", candidate_value.clone());
    let mut args = vec![candidate_value];
    args.append(&mut extra_args);
    if !matches!(runtime.object_member(layer, method), Variant::Void) {
        runtime.call_object_method(layer, method, args)?;
    }
    Ok(match runtime.object_member(layer, "__nativeFocusWork") {
        Variant::Object(handle) => Some(runtime.bound_this(handle).unwrap_or(handle)),
        _ => None,
    })
}

fn layer_set_focus_work(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    let value = args.first().cloned().unwrap_or(Variant::Null);
    runtime.set_object_member(this, "__nativeFocusWork", value);
    Ok(Variant::Void)
}

fn focused_layer(runtime: &Runtime<KrkrHost>, window: ObjectHandle) -> Option<ObjectHandle> {
    runtime
        .host()
        .native_window_focused_layer(window)
        .or_else(|| match runtime.object_member(window, "focusedLayer") {
            Variant::Object(handle) => Some(runtime.bound_this(handle).unwrap_or(handle)),
            _ => None,
        })
}

fn layer_window_object(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> Option<ObjectHandle> {
    variant_object(&layer_property_value(runtime, layer, "window"))
        .map(|window| runtime.bound_this(window).unwrap_or(window))
}

fn focusable_layers_for_window(
    runtime: &Runtime<KrkrHost>,
    window: ObjectHandle,
    fallback_root: ObjectHandle,
) -> Vec<ObjectHandle> {
    let root = runtime
        .host()
        .native_window_primary_layer(window)
        .unwrap_or_else(|| match runtime.object_member(window, "primaryLayer") {
            Variant::Object(root) => runtime.bound_this(root).unwrap_or(root),
            _ => fallback_root,
        });
    let mut layers = Vec::new();
    let mut visited = BTreeSet::new();
    collect_focusable_layers(runtime, root, &mut visited, &mut layers);
    layers
}

fn collect_focusable_layers(
    runtime: &Runtime<KrkrHost>,
    layer: ObjectHandle,
    visited: &mut BTreeSet<ObjectHandle>,
    layers: &mut Vec<ObjectHandle>,
) {
    let layer = runtime.bound_this(layer).unwrap_or(layer);
    if !visited.insert(layer) {
        return;
    }
    if layer_is_node_focusable(runtime, layer) && layer_joins_focus_chain(runtime, layer) {
        layers.push(layer);
    }
    for child in layer_children(runtime, layer) {
        collect_focusable_layers(runtime, child, visited, layers);
    }
}

fn layer_children(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> Vec<ObjectHandle> {
    let children = runtime.host().native_layer_children(layer);
    if !children.is_empty() {
        return children;
    }
    let Variant::Object(children) = layer_property_value(runtime, layer, "children") else {
        return Vec::new();
    };
    let count = runtime
        .object_member(children, "count")
        .to_integer()
        .unwrap_or(0)
        .max(0);
    (0..count)
        .filter_map(|index| {
            variant_object(&runtime.object_member(children, &index.to_string()))
                .map(|child| runtime.bound_this(child).unwrap_or(child))
        })
        .collect()
}

fn layer_joins_focus_chain(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> bool {
    match runtime.object_member(layer, "joinFocusChain") {
        Variant::Void => true,
        value => value.is_truthy(),
    }
}

fn layer_is_node_focusable(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> bool {
    if !runtime.object_member(layer, "focusable").is_truthy()
        || !layer_property_value(runtime, layer, "visible").is_truthy()
        || !layer_property_value(runtime, layer, "enabled").is_truthy()
    {
        return false;
    }

    let mut parent = variant_object(&layer_property_value(runtime, layer, "parent"))
        .map(|parent| runtime.bound_this(parent).unwrap_or(parent));
    while let Some(layer) = parent {
        if !layer_property_value(runtime, layer, "visible").is_truthy()
            || !layer_property_value(runtime, layer, "enabled").is_truthy()
        {
            return false;
        }
        parent = variant_object(&layer_property_value(runtime, layer, "parent"))
            .map(|parent| runtime.bound_this(parent).unwrap_or(parent));
    }
    true
}

fn layer_on_key_down(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    if key_event_should_process(&args) {
        let key = optional_integer(&args, 0)?.unwrap_or(0);
        let shift = optional_integer(&args, 1)?.unwrap_or(0);
        layer_default_key_down(runtime, this, key, shift)?;
    }
    Ok(Variant::Void)
}

fn layer_on_key_up(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    if key_event_should_process(&args) {
        let key = optional_integer(&args, 0)?.unwrap_or(0);
        let shift = optional_integer(&args, 1)?.unwrap_or(0);
        layer_default_key_up(runtime, this, key, shift)?;
    }
    Ok(Variant::Void)
}

fn key_event_should_process(args: &[Variant]) -> bool {
    args.get(2).is_none_or(Variant::is_truthy)
}

fn layer_default_key_down(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    key: i64,
    shift: i64,
) -> Result<()> {
    let no_shift = shift & ((1 << 0) | (1 << 1) | (1 << 2)) == 0;
    if no_shift && matches!(key, 0x09 | 0x27 | 0x28) {
        layer_focus_relative(runtime, this, true)?;
    } else if key == 0x25 || key == 0x26 || (key == 0x09 && shift & (1 << 0) != 0) {
        layer_focus_relative(runtime, this, false)?;
    } else if no_shift && matches!(key, 0x0d | 0x1b) {
        layer_fire_parent_key_event(runtime, this, "onKeyDown", key, shift)?;
    }
    Ok(())
}

fn layer_default_key_up(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    key: i64,
    shift: i64,
) -> Result<()> {
    let no_shift = shift & ((1 << 0) | (1 << 1) | (1 << 2)) == 0;
    if no_shift && matches!(key, 0x0d | 0x1b) {
        layer_fire_parent_key_event(runtime, this, "onKeyUp", key, shift)?;
    }
    Ok(())
}

fn layer_fire_parent_key_event(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    method: &str,
    key: i64,
    shift: i64,
) -> Result<()> {
    let Some(parent) = variant_object(&layer_property_value(runtime, this, "parent"))
        .map(|parent| runtime.bound_this(parent).unwrap_or(parent))
    else {
        return Ok(());
    };
    if !layer_property_value(runtime, parent, "nodeEnabled").is_truthy()
        || matches!(runtime.object_member(parent, method), Variant::Void)
    {
        return Ok(());
    }
    runtime
        .call_object_method(
            parent,
            method,
            vec![
                Variant::Integer(key),
                Variant::Integer(shift),
                Variant::Integer(1),
            ],
        )
        .map(|_| ())
}

fn layer_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn layer_as_layer(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // krkr base scripts call `asLayer()` on layer wrappers to obtain the
    // underlying native layer; our script layers are native layers, so the
    // receiver itself is the answer.
    Ok(this_obj.map(Variant::Object).unwrap_or_default())
}

fn layer_on_click(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    let Some(window) = variant_object(&layer_property_value(runtime, this, "window"))
        .map(|window| runtime.bound_this(window).unwrap_or(window))
    else {
        return Ok(Variant::Void);
    };
    if matches!(runtime.object_member(window, "action"), Variant::Void) {
        return Ok(Variant::Void);
    }

    let event = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(event, "Dictionary");
    runtime.set_object_member(event, "target", Variant::Object(this));
    runtime.set_object_member(event, "type", Variant::String("onClick".to_string()));
    runtime
        .call_object_method(window, "action", vec![Variant::Object(event)])
        .map(|_| Variant::Void)
}

fn layer_on_hit_test(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    let hit = args.get(2).is_some_and(Variant::is_truthy);
    runtime.set_object_member(
        this,
        "__nativeHitTestWork",
        Variant::Integer(i64::from(hit)),
    );
    Ok(Variant::Void)
}

// Mirrors krkr2 `Layer.getLayerAt(x, y, excludeSelf=false, getDisabled=false)`
// (LayerIntf.cpp `tTJSNI_BaseLayer::GetMostFrontChildAt`): the point is given
// in this layer's coordinates, converted to primary coordinates, then its
// owning window's primary layer is searched front-to-back. Invisible subtrees
// and layers whose rectangle does not contain the point are skipped; htMask
// compares image alpha against `hitThreshold`, while a layer without a mask
// image cannot hit. A script `onHitTest` may veto; a disabled layer blocks the
// search and returns null (unless getDisabled).
fn layer_get_layer_at(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Null);
    };
    if args.len() < 2 {
        return Err(TjsError::runtime("Layer.getLayerAt requires x and y"));
    }
    let x = args[0].to_integer()?;
    let y = args[1].to_integer()?;
    let exclude_self = args.get(2).is_some_and(Variant::is_truthy);
    let get_disabled = args.get(3).is_some_and(Variant::is_truthy);
    let Some(this_layer) = runtime.host().native_layer(this) else {
        return Ok(Variant::Null);
    };
    let Some(origin) = runtime.host().layer_tree().absolute_position(this_layer) else {
        return Ok(Variant::Null);
    };
    let point_x = origin.x + x as f32;
    let point_y = origin.y + y as f32;

    // KRKR2 delegates to the owner manager, which always starts from that
    // window's primary layer rather than unrelated layer-tree roots.
    let primary = runtime
        .host()
        .native_layer_window(this)
        .and_then(|window| runtime.host().native_window_primary_layer(window))
        .and_then(|primary| runtime.host().native_layer(primary))
        .or_else(|| render_root_for_layer(runtime.host().layer_tree(), this_layer));
    let Some(primary) = primary else {
        return Ok(Variant::Null);
    };

    // Build the candidate order: children before self. `renderable=false`
    // represents the engine's staged back-page copy and must not participate.
    let mut candidates = Vec::new();
    {
        let tree = runtime.host().layer_tree();
        collect_front_child_candidates(tree, primary, 0.0, 0.0, point_x, point_y, &mut candidates);
    }

    for layer_id in candidates {
        if exclude_self && layer_id == this_layer {
            continue;
        }
        let (origin, hit_threshold, hit_type, image_left, image_top, image) = {
            let Some(layer) = runtime.host().layer_tree().layer(layer_id) else {
                continue;
            };
            let Some(origin) = runtime.host().layer_tree().absolute_position(layer_id) else {
                continue;
            };
            (
                origin,
                layer.hit_threshold,
                layer.hit_type,
                layer.image_left,
                layer.image_top,
                layer.image.clone(),
            )
        };
        let local_x = (point_x - origin.x).floor() as i64;
        let local_y = (point_y - origin.y).floor() as i64;
        let pixel_hit = if hit_type == 1 {
            // htProvince is unsupported; province layers never hit.
            false
        } else if let Some(image) = &image {
            let px = local_x - image_left as i64;
            let py = local_y - image_top as i64;
            px >= 0
                && py >= 0
                && px < image.upload.width as i64
                && py < image.upload.height as i64
                && {
                    let index = ((py as u32 * image.upload.width + px as u32) * 4 + 3) as usize;
                    i32::from(image.upload.rgba[index]) >= hit_threshold
                }
        } else {
            // KRKR2's htMask requires MainImage; a transparent/no-image
            // control is not a hit merely because its threshold is zero.
            false
        };
        if !pixel_hit {
            continue;
        }
        let Some(object) = runtime.host().native_object_for_layer(layer_id) else {
            continue;
        };
        // Script veto via onHitTest, same protocol as the input dispatcher.
        runtime.set_object_member(object, "__nativeHitTestWork", Variant::Integer(1));
        if !matches!(runtime.object_member(object, "onHitTest"), Variant::Void) {
            runtime.call_object_method(
                object,
                "onHitTest",
                vec![
                    Variant::Integer(local_x),
                    Variant::Integer(local_y),
                    Variant::Integer(1),
                ],
            )?;
        }
        if !runtime
            .object_member(object, "__nativeHitTestWork")
            .is_truthy()
        {
            continue;
        }
        if !get_disabled && !render_node_enabled(runtime.host().layer_tree(), layer_id) {
            // Disabled front layer blocks events to everything below it.
            return Ok(Variant::Null);
        }
        return Ok(Variant::Object(object));
    }
    Ok(Variant::Null)
}

fn render_root_for_layer(
    tree: &krkr_core::LayerTree,
    mut id: krkr_core::LayerId,
) -> Option<krkr_core::LayerId> {
    loop {
        let layer = tree.layer(id)?;
        match layer.parent {
            Some(parent) => id = parent,
            None => return Some(id),
        }
    }
}

// `nodeEnabled` in KRKR2 is derived from this layer's enabled state, every
// ancestor, and the current modal layer. The host has no layer-modal state,
// but deriving the ancestor portion here avoids treating a child of a disabled
// parent as clickable when its cached render-node flag has not yet been synced.
fn render_node_enabled(tree: &krkr_core::LayerTree, mut id: krkr_core::LayerId) -> bool {
    loop {
        let Some(layer) = tree.layer(id) else {
            return false;
        };
        if !layer.enabled || !layer.node_enabled {
            return false;
        }
        match layer.parent {
            Some(parent) => id = parent,
            None => return true,
        }
    }
}

fn collect_front_child_candidates(
    tree: &krkr_core::LayerTree,
    id: krkr_core::LayerId,
    origin_x: f32,
    origin_y: f32,
    point_x: f32,
    point_y: f32,
    out: &mut Vec<krkr_core::LayerId>,
) {
    let Some(layer) = tree.layer(id) else {
        return;
    };
    if !layer.visible || !layer.renderable {
        return;
    }
    let local_x = point_x - origin_x - layer.left;
    let local_y = point_y - origin_y - layer.top;
    if local_x < 0.0 || local_y < 0.0 || local_x >= layer.width || local_y >= layer.height {
        return;
    }
    let child_origin_x = origin_x + layer.left;
    let child_origin_y = origin_y + layer.top;
    let mut children: Vec<_> = tree
        .layers()
        .filter(|child| child.parent == Some(id))
        .map(|child| (child.z_order, child.id))
        .collect();
    children.sort();
    for (_, child) in children.into_iter().rev() {
        collect_front_child_candidates(
            tree,
            child,
            child_origin_x,
            child_origin_y,
            point_x,
            point_y,
            out,
        );
    }
    out.push(id);
}

fn layer_zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn font_get_text_width(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let font = this_font_spec(runtime, this_obj)?;
    ensure_font_file_loaded(runtime, &font)?;
    let text = first_text_arg(&args)?;
    let width = runtime
        .host()
        .font_system()
        .text_metrics(&font, &text)
        .width;
    Ok(Variant::Integer(width.ceil() as i64))
}

fn font_get_text_height(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let font = this_font_spec(runtime, this_obj)?;
    ensure_font_file_loaded(runtime, &font)?;
    let text = first_text_arg(&args)?;
    let height = runtime
        .host()
        .font_system()
        .text_metrics(&font, &text)
        .height;
    Ok(Variant::Integer(height.ceil() as i64))
}

fn font_get_esc_width_x(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let font = this_font_spec(runtime, this_obj)?;
    ensure_font_file_loaded(runtime, &font)?;
    let text = first_text_arg(&args)?;
    let (x, _) = runtime.host().font_system().esc_width(&font, &text);
    Ok(Variant::Integer(x.round() as i64))
}

fn font_get_esc_width_y(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let font = this_font_spec(runtime, this_obj)?;
    ensure_font_file_loaded(runtime, &font)?;
    let text = first_text_arg(&args)?;
    let (_, y) = runtime.host().font_system().esc_width(&font, &text);
    Ok(Variant::Integer(y.round() as i64))
}

fn font_get_esc_height_x(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let font = this_font_spec(runtime, this_obj)?;
    ensure_font_file_loaded(runtime, &font)?;
    let text = first_text_arg(&args)?;
    let (x, _) = runtime.host().font_system().esc_height(&font, &text);
    Ok(Variant::Integer(x.round() as i64))
}

fn font_get_esc_height_y(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let font = this_font_spec(runtime, this_obj)?;
    ensure_font_file_loaded(runtime, &font)?;
    let text = first_text_arg(&args)?;
    let (_, y) = runtime.host().font_system().esc_height(&font, &text);
    Ok(Variant::Integer(y.round() as i64))
}

fn font_get_glyph_draw_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let font = this_font_spec(runtime, this_obj)?;
    ensure_font_file_loaded(runtime, &font)?;
    let text = first_text_arg(&args)?;
    let Some(ch) = text.chars().next() else {
        return construct_native_instance(runtime, &RECT_CLASS, None, Vec::new());
    };
    let rect = runtime
        .host()
        .font_system()
        .glyph_draw_rect(&font, ch)
        .unwrap_or_default();
    construct_native_instance(
        runtime,
        &RECT_CLASS,
        None,
        vec![
            Variant::Integer(rect.left as i64),
            Variant::Integer(rect.top as i64),
            Variant::Integer(rect.left as i64 + rect.width as i64),
            Variant::Integer(rect.top as i64 + rect.height as i64),
        ],
    )
}

fn font_get_list(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let values = runtime
        .host()
        .font_system()
        .families()
        .into_iter()
        .map(Variant::String)
        .collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

fn font_map_prerendered_font(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let storage = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .ok_or_else(|| TjsError::runtime("Font.mapPrerenderedFont requires a font name"))?;
    let font = this_font_spec(runtime, this_obj)?;
    let data = runtime.host().read_resource_storage(&storage)?;
    let bytes = data.to_arc_bytes().map_err(crate::storage::io_error)?;
    runtime
        .host_mut()
        .font_system_mut()
        .map_prerendered_font_for_spec_arc(&font, bytes)
        .map_err(TjsError::runtime)?;
    Ok(Variant::Void)
}

fn font_unmap_prerendered_font(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let font = this_font_spec(runtime, this_obj)?;
    let unmapped = runtime
        .host_mut()
        .font_system_mut()
        .unmap_prerendered_font_for_spec(&font);
    Ok(Variant::Integer(i64::from(unmapped)))
}

fn layer_bring_to_front(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let (_this, layer_id) = this_layer_id(runtime, this_obj)?;
    let next = runtime
        .host()
        .layer_tree()
        .layer(layer_id)
        .map(|layer| layer.z_order)
        .unwrap_or(20_000)
        + 1_000;
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.z_order = next;
    }
    Ok(Variant::Void)
}

fn layer_bring_to_back(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let (_this, layer_id) = this_layer_id(runtime, this_obj)?;
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.z_order = 0;
    }
    Ok(Variant::Void)
}

fn copy_layer_images(
    runtime: &mut Runtime<KrkrHost>,
    dest_object: ObjectHandle,
    dest_target: &LayerRenderTarget,
    source_object: ObjectHandle,
) -> Result<()> {
    complete_layer_before_draw(runtime, source_object)?;
    let Some(source_target) = render_layer_target(runtime, source_object)? else {
        return Ok(());
    };
    let Some(source) = render_layer_snapshot(runtime, &source_target) else {
        return Ok(());
    };

    let mut resized_to_source = false;
    mutate_render_layer(runtime, dest_target, |dest| {
        dest.image = source.image.clone();
        dest.image_left = source.image_left;
        dest.image_top = source.image_top;
        dest.image_width = source.image_width;
        dest.image_height = source.image_height;
        if dest.width <= 0.0 || dest.height <= 0.0 {
            dest.width = source.width;
            dest.height = source.height;
            resized_to_source = true;
        }
    });

    set_layer_property_storage(
        runtime,
        dest_object,
        "imageLeft",
        Variant::Integer(source.image_left as i64),
    );
    set_layer_property_storage(
        runtime,
        dest_object,
        "imageTop",
        Variant::Integer(source.image_top as i64),
    );
    set_layer_property_storage(
        runtime,
        dest_object,
        "imageWidth",
        Variant::Integer(source.image_width as i64),
    );
    set_layer_property_storage(
        runtime,
        dest_object,
        "imageHeight",
        Variant::Integer(source.image_height as i64),
    );
    if resized_to_source {
        set_layer_property_storage(
            runtime,
            dest_object,
            "width",
            Variant::Integer(source.width as i64),
        );
        set_layer_property_storage(
            runtime,
            dest_object,
            "height",
            Variant::Integer(source.height as i64),
        );
    }
    mark_image_modified(runtime, dest_object);
    Ok(())
}

fn native_layer_id(runtime: &Runtime<KrkrHost>, handle: ObjectHandle) -> Result<Option<u64>> {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    match runtime.object_member(handle, "__nativeLayerId") {
        Variant::Void => Ok(None),
        value => {
            let id = value.to_integer()? as u64;
            Ok((id != 0).then_some(id))
        }
    }
}

fn variant_object(value: &Variant) -> Option<ObjectHandle> {
    match value {
        Variant::Object(handle) => Some(*handle),
        _ => None,
    }
}

fn finish_immediate_transition(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    source: Option<ObjectHandle>,
) -> Result<()> {
    let Some(window) = variant_object(&layer_property_value(runtime, layer, "window"))
        .map(|window| runtime.bound_this(window).unwrap_or(window))
    else {
        return Ok(());
    };
    let trans_count_before = runtime
        .object_member(window, "transCount")
        .to_integer()
        .ok();
    notify_transition_completed(runtime, layer, source)?;
    finish_kag_window_transition_if_pending(runtime, layer)?;
    let callback_consumed_transition = trans_count_before.is_some_and(|before| {
        runtime
            .object_member(window, "transCount")
            .to_integer()
            .is_ok_and(|after| after != before)
    });
    if !callback_consumed_transition {
        runtime.set_object_member(layer, "inTransition", Variant::Integer(0));
        if let Some(trans_count) = trans_count_before {
            runtime.set_object_member(
                window,
                "transCount",
                Variant::Integer(trans_count.saturating_sub(1).max(0)),
            );
        }
    }
    Ok(())
}

fn finish_current_transition(runtime: &mut Runtime<KrkrHost>) -> Result<()> {
    runtime.host_mut().complete_active_transition();
    finish_completed_native_transitions(runtime)
}

pub(crate) fn finish_completed_native_transitions(runtime: &mut Runtime<KrkrHost>) -> Result<()> {
    let completions = runtime.host_mut().take_completed_native_transitions();
    for completion in completions {
        finish_native_transition(runtime, completion)?;
    }
    Ok(())
}

pub(crate) fn finish_native_transition(
    runtime: &mut Runtime<KrkrHost>,
    completion: NativeTransitionCompletion,
) -> Result<()> {
    if !runtime.object_valid(completion.dest) {
        return Ok(());
    }
    if completion.paired_comp
        && let Some(source) = completion.source
        && runtime.object_valid(source)
    {
        exchange_native_layer_info(runtime, completion.dest, source)?;
    }

    let window = variant_object(&layer_property_value(runtime, completion.dest, "window"))
        .map(|window| runtime.bound_this(window).unwrap_or(window));
    let trans_count_before = window.and_then(|window| {
        runtime
            .object_member(window, "transCount")
            .to_integer()
            .ok()
    });

    notify_transition_completed(runtime, completion.dest, completion.source)?;
    finish_kag_window_transition_if_pending(runtime, completion.dest)?;
    if completion.paired_comp
        && let Some(source) = completion.source
        && runtime.object_valid(source)
    {
        let visible = Variant::Integer(1);
        set_layer_property_storage(runtime, source, "visible", visible.clone());
        apply_layer_property_to_render(runtime, source, "visible", &visible)?;
    }

    let callback_consumed_transition =
        window
            .zip(trans_count_before)
            .is_some_and(|(window, before)| {
                runtime
                    .object_member(window, "transCount")
                    .to_integer()
                    .is_ok_and(|after| after != before)
            });
    if !callback_consumed_transition {
        runtime.set_object_member(completion.dest, "inTransition", Variant::Integer(0));
        if let Some(window) = window
            && let Ok(trans_count) = runtime.object_member(window, "transCount").to_integer()
        {
            runtime.set_object_member(
                window,
                "transCount",
                Variant::Integer(trans_count.saturating_sub(1).max(0)),
            );
        }
    }
    Ok(())
}

fn notify_transition_completed(
    runtime: &mut Runtime<KrkrHost>,
    dest: ObjectHandle,
    source: Option<ObjectHandle>,
) -> Result<()> {
    let source = source
        .filter(|source| runtime.object_valid(*source))
        .map(Variant::Object)
        .unwrap_or_default();
    let callback_args = vec![Variant::Object(dest), source];
    if !matches!(
        runtime.object_member(dest, "onTransitionCompleted"),
        Variant::Void
    ) {
        runtime.call_object_method(dest, "onTransitionCompleted", callback_args)?;
    } else if !runtime.call_secondary_class_method(
        dest,
        "onTransitionCompleted",
        callback_args.clone(),
    )? {
        // A plain Layer still exposes the native no-op through its primary
        // class chain, preserving the event's optional nature.
        runtime.call_object_method(dest, "onTransitionCompleted", callback_args)?;
    }
    Ok(())
}

fn finish_kag_window_transition_if_pending(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
) -> Result<()> {
    let Some(window) = variant_object(&layer_property_value(runtime, layer, "window"))
        .map(|window| runtime.bound_this(window).unwrap_or(window))
    else {
        return Ok(());
    };
    if !runtime.object_member(window, "inTransition").is_truthy()
        || !kag_window_transition_base(runtime, window, layer)
        || matches!(
            runtime.object_member(window, "onTransitionEnd"),
            Variant::Void
        )
    {
        return Ok(());
    }

    // KAG's BaseLayerBase receives Layer.onTransitionCompleted and normally
    // relays it to KAGWindow.onTransitionEnd.  A secondary TJS class extender
    // can make that relay unavailable to the instance lookup, so preserve the
    // native event contract at the KAG boundary rather than leaving its
    // conductor suspended forever.
    runtime
        .call_object_method(window, "onTransitionEnd", Vec::new())
        .map(|_| ())
}

fn kag_window_transition_base(
    runtime: &Runtime<KrkrHost>,
    window: ObjectHandle,
    layer: ObjectHandle,
) -> bool {
    ["_forebase", "_backbase"]
        .into_iter()
        .any(|name| variant_object(&runtime.object_member(window, name)) == Some(layer))
        || ["fore", "back"].into_iter().any(|name| {
            let Some(page) = variant_object(&runtime.object_member(window, name)) else {
                return false;
            };
            variant_object(&runtime.object_member(page, "base")) == Some(layer)
        })
}

fn optional_integer(args: &[Variant], index: usize) -> Result<Option<i64>> {
    args.get(index)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_integer)
        .transpose()
}

fn object_member_i64(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Result<Option<i64>> {
    match runtime.object_member(object, name) {
        Variant::Void | Variant::Null => Ok(None),
        value => value.to_integer().map(Some),
    }
}

fn first_text_arg(args: &[Variant]) -> Result<String> {
    args.first()
        .map(Variant::to_tjs_string)
        .transpose()
        .map(|text| text.unwrap_or_default())
}

fn this_font_spec(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<FontSpec> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(FontSpec::default());
    };
    font_spec_from_object(runtime, this)
}

fn layer_font_spec(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Result<FontSpec> {
    match layer_property_value(runtime, layer, "font") {
        Variant::Object(font) => font_spec_from_object(runtime, font),
        _ => Ok(FontSpec::default()),
    }
}

/// Read a font attribute through the TJS dispatch path.  GINKA wraps the
/// layer font in a script `FontHook` whose `face`/`height`/style members are
/// TJS properties forwarding to the real native font; a raw member read would
/// see the property object itself and fall back to defaults (GINKA's ruby was
/// drawn at the base font height because of this).
fn resolve_font_member(
    runtime: &mut Runtime<KrkrHost>,
    font: ObjectHandle,
    name: &str,
) -> Option<Variant> {
    runtime
        .resolve_object_member(font, name)
        .ok()
        .filter(|value| !matches!(value, Variant::Void | Variant::Null))
}

fn font_spec_from_object(runtime: &mut Runtime<KrkrHost>, font: ObjectHandle) -> Result<FontSpec> {
    let face = match resolve_font_member(runtime, font, "face") {
        None => String::new(),
        Some(value) => value.to_tjs_string()?,
    };
    let raw_height = resolve_font_member(runtime, font, "height")
        .map(|value| value.to_integer())
        .transpose()?
        .unwrap_or(FontSpec::default().height as i64);
    let height = if raw_height == 0 {
        FontSpec::default().height
    } else {
        raw_height.unsigned_abs().max(1) as f32
    };
    let rasterizer = match resolve_font_member(runtime, font, "rasterizer") {
        None => String::new(),
        Some(value) => value.to_tjs_string()?,
    };
    let flag = |runtime: &mut Runtime<KrkrHost>, name: &str| {
        resolve_font_member(runtime, font, name)
            .map(|value| value.to_integer().unwrap_or(0) != 0)
            .unwrap_or(false)
    };
    Ok(FontSpec {
        face,
        height,
        bold: flag(runtime, "bold"),
        italic: flag(runtime, "italic"),
        strikeout: flag(runtime, "strikeout"),
        underline: flag(runtime, "underline"),
        angle: resolve_font_member(runtime, font, "angle")
            .map(|value| value.to_integer())
            .transpose()?
            .unwrap_or(0) as i32,
        face_is_file_name: flag(runtime, "faceIsFileName"),
        rasterizer,
    })
}

fn ensure_font_file_loaded(runtime: &mut Runtime<KrkrHost>, spec: &FontSpec) -> Result<()> {
    if !spec.face_is_file_name || spec.face.is_empty() {
        return Ok(());
    }
    let data = runtime.host().read_resource_storage(&spec.face)?;
    let bytes = data
        .as_bytes()
        .map(|bytes| bytes.into_owned())
        .map_err(crate::storage::io_error)?;
    runtime
        .host_mut()
        .font_system_mut()
        .load_font_data(spec.face.clone(), bytes)
        .map_err(TjsError::runtime)
}

fn required_integer(args: &[Variant], index: usize, context: &str) -> Result<i64> {
    optional_integer(args, index)?
        .ok_or_else(|| TjsError::runtime(format!("{context} is required")))
}

fn rect_args(args: &[Variant]) -> Result<Option<(i64, i64, i64, i64)>> {
    let x = optional_integer(args, 0)?.unwrap_or(0);
    let y = optional_integer(args, 1)?.unwrap_or(0);
    let width = optional_integer(args, 2)?.unwrap_or(0);
    let height = optional_integer(args, 3)?.unwrap_or(0);
    Ok((width > 0 && height > 0).then_some((x, y, width, height)))
}

fn is_province_face(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> bool {
    layer_property_value(runtime, layer, "face")
        .to_integer()
        .is_ok_and(|face| face == 3)
}

fn mark_image_modified(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) {
    let layer = runtime.bound_this(layer).unwrap_or(layer);
    runtime.set_object_member(layer, "imageModified", Variant::Integer(1));
}

fn color_to_rgba(color: i64, opacity: Option<i64>) -> [u8; 4] {
    let color = color.max(0) as u32;
    let r = ((color >> 16) & 0xff) as u8;
    let g = ((color >> 8) & 0xff) as u8;
    let b = (color & 0xff) as u8;
    let a = match opacity {
        Some(opacity) if opacity < 0 => 0,
        Some(opacity) => opacity.clamp(0, 255) as u8,
        None if color <= 0x00ff_ffff && color != 0 => 255,
        None => ((color >> 24) & 0xff) as u8,
    };
    [r, g, b, a]
}

fn packed_color_to_rgba(color: i64) -> [u8; 4] {
    let color = color.max(0) as u32;
    [
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
        ((color >> 24) & 0xff) as u8,
    ]
}

fn mutate_layer_pixels<F>(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
    mutate: F,
) -> Result<()>
where
    F: FnOnce(&mut [u8], u32, u32),
{
    mutate_layer_pixels_min(runtime, target, 1, 1, mutate)
}

fn mutate_layer_pixels_min<F>(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
    min_width: u32,
    min_height: u32,
    mutate: F,
) -> Result<()>
where
    F: FnOnce(&mut [u8], u32, u32),
{
    mutate_layer_pixels_min_with_host(
        runtime,
        target,
        min_width,
        min_height,
        |_, pixels, width, height| {
            mutate(pixels, width, height);
        },
    )
}

fn mutate_layer_pixels_min_with_host<F>(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
    min_width: u32,
    min_height: u32,
    mutate: F,
) -> Result<()>
where
    F: FnOnce(&KrkrHost, &mut [u8], u32, u32),
{
    let Some(layer) = render_layer_snapshot(runtime, target) else {
        return Ok(());
    };
    let width = layer
        .image
        .as_ref()
        .map(|image| image.upload.width)
        .unwrap_or_else(|| layer.image_width.max(layer.width).max(1.0) as u32)
        .max(min_width)
        .max(1);
    let height = layer
        .image
        .as_ref()
        .map(|image| image.upload.height)
        .unwrap_or_else(|| layer.image_height.max(layer.height).max(1.0) as u32)
        .max(min_height)
        .max(1);
    let mut pixels = layer
        .image
        .as_ref()
        .filter(|image| image.upload.width == width && image.upload.height == height)
        .map(|image| image.upload.rgba.as_ref().to_vec())
        .unwrap_or_else(|| vec![0; width as usize * height as usize * 4]);

    mutate(runtime.host(), &mut pixels, width, height);

    let image = runtime.host_mut().create_layer_image(width, height, pixels);
    mutate_render_layer(runtime, target, |layer| {
        layer.image = Some(image);
        layer.image_width = width as f32;
        layer.image_height = height as f32;
        if layer.width <= 0.0 {
            layer.width = width as f32;
        }
        if layer.height <= 0.0 {
            layer.height = height as f32;
        }
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn fill_layer_pixels(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    rgba: [u8; 4],
) -> Result<()> {
    let Some(layer) = render_layer_snapshot(runtime, target) else {
        return Ok(());
    };
    let image_width = layer
        .image
        .as_ref()
        .map(|image| image.upload.width)
        .unwrap_or_else(|| layer.image_width.max(layer.width).max(1.0) as u32)
        .max(1);
    let image_height = layer
        .image
        .as_ref()
        .map(|image| image.upload.height)
        .unwrap_or_else(|| layer.image_height.max(layer.height).max(1.0) as u32)
        .max(1);

    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = (x + width).clamp(0, image_width as i64) as u32;
    let y1 = (y + height).clamp(0, image_height as i64) as u32;
    if x1 <= x0 || y1 <= y0 {
        return Ok(());
    }

    if x0 == 0 && y0 == 0 && x1 == image_width && y1 == image_height {
        let mut pixels = vec![0; image_width as usize * image_height as usize * 4];
        if rgba != [0, 0, 0, 0] {
            fill_pixel_buffer(&mut pixels, rgba);
        }
        let image = runtime
            .host_mut()
            .create_layer_image(image_width, image_height, pixels);
        mutate_render_layer(runtime, target, |layer| {
            layer.image = Some(image);
            layer.image_width = image_width as f32;
            layer.image_height = image_height as f32;
            if layer.width <= 0.0 {
                layer.width = image_width as f32;
            }
            if layer.height <= 0.0 {
                layer.height = image_height as f32;
            }
        });
        return Ok(());
    }

    mutate_layer_pixels_min_with_host(
        runtime,
        target,
        image_width,
        image_height,
        |_, pixels, _, _| {
            fill_pixels(pixels, image_width, image_height, x, y, width, height, rgba);
        },
    )
}

fn fill_pixel_buffer(pixels: &mut [u8], rgba: [u8; 4]) {
    let pixel = u32::from_ne_bytes(rgba);
    let mut offset = 0usize;
    while offset + 4 <= pixels.len() {
        unsafe {
            pixels
                .as_mut_ptr()
                .add(offset)
                .cast::<u32>()
                .write_unaligned(pixel);
        }
        offset += 4;
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_pixels(
    pixels: &mut [u8],
    image_width: u32,
    image_height: u32,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    rgba: [u8; 4],
) {
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = (x + width).clamp(0, image_width as i64) as u32;
    let y1 = (y + height).clamp(0, image_height as i64) as u32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let row_width = image_width as usize * 4;
    let x0 = x0 as usize;
    let x1 = x1 as usize;
    let pixel = u32::from_ne_bytes(rgba);
    for py in y0..y1 {
        let row_start = py as usize * row_width;
        let mut offset = row_start + x0 * 4;
        let row_end = row_start + x1 * 4;
        if row_end > pixels.len() {
            return;
        }
        while offset < row_end {
            // The write is within the bounds checked above, but the byte buffer
            // is not guaranteed to have u32 alignment.
            unsafe {
                pixels
                    .as_mut_ptr()
                    .add(offset)
                    .cast::<u32>()
                    .write_unaligned(pixel);
            }
            offset += 4;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn affine_copy_pixels(
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    source: &[u8],
    texture_width: u32,
    texture_height: u32,
    sx: i64,
    sy: i64,
    source_width: i64,
    source_height: i64,
    points: [(f64, f64); 3],
) {
    let [(x0, y0), (x1, y1), (x2, y2)] = points;
    let ux = x1 - x0;
    let uy = y1 - y0;
    let vx = x2 - x0;
    let vy = y2 - y0;
    let determinant = ux * vy - uy * vx;
    if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
        return;
    }
    let x3 = x1 + x2 - x0;
    let y3 = y1 + y2 - y0;
    let min_x = x0.min(x1).min(x2).min(x3).floor().max(0.0) as i64;
    let max_x = x0.max(x1).max(x2).max(x3).ceil().min(dest_width as f64) as i64;
    let min_y = y0.min(y1).min(y2).min(y3).floor().max(0.0) as i64;
    let max_y = y0.max(y1).max(y2).max(y3).ceil().min(dest_height as f64) as i64;
    for dy in min_y..max_y {
        for dx in min_x..max_x {
            let px = dx as f64 + 0.5 - x0;
            let py = dy as f64 + 0.5 - y0;
            let u = (px * vy - py * vx) / determinant;
            let v = (ux * py - uy * px) / determinant;
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            let src_x = sx + (u * source_width as f64).floor() as i64;
            let src_y = sy + (v * source_height as f64).floor() as i64;
            if src_x < 0
                || src_y < 0
                || src_x >= i64::from(texture_width)
                || src_y >= i64::from(texture_height)
            {
                continue;
            }
            let source_offset = ((src_y as u32 * texture_width + src_x as u32) * 4) as usize;
            let dest_offset = ((dy as u32 * dest_width + dx as u32) * 4) as usize;
            dest[dest_offset..dest_offset + 4]
                .copy_from_slice(&source[source_offset..source_offset + 4]);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_pixels(
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    source: &[u8],
    source_width: u32,
    source_height: u32,
    dx: i64,
    dy: i64,
    sx: i64,
    sy: i64,
    width: i64,
    height: i64,
    alpha_blend: bool,
) {
    let Some((dx, dy, sx, sy, width, height)) = clipped_copy_rect(
        dx,
        dy,
        sx,
        sy,
        width,
        height,
        dest_width as i64,
        dest_height as i64,
        source_width as i64,
        source_height as i64,
    ) else {
        return;
    };

    let dest_stride = dest_width as usize * 4;
    let source_stride = source_width as usize * 4;
    let bytes = width as usize * 4;
    let dx = dx as usize;
    let dy = dy as usize;
    let sx = sx as usize;
    let sy = sy as usize;
    let height = height as usize;

    for row in 0..height {
        let src_start = (sy + row) * source_stride + sx * 4;
        let dest_start = (dy + row) * dest_stride + dx * 4;
        let src_end = src_start + bytes;
        let dest_end = dest_start + bytes;
        if src_end > source.len() || dest_end > dest.len() {
            return;
        }
        let src_row = &source[src_start..src_end];
        let dest_row = &mut dest[dest_start..dest_end];
        if alpha_blend {
            for (dest_pixel, src_pixel) in dest_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4))
            {
                blend_pixel(dest_pixel, src_pixel);
            }
        } else {
            dest_row.copy_from_slice(src_row);
        }
    }
}

#[derive(Clone, Copy)]
struct PiledClip {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl PiledClip {
    fn new(x: f32, y: f32, width: f32, height: f32) -> Option<Self> {
        (width > 0.0 && height > 0.0).then_some(Self {
            x0: x,
            y0: y,
            x1: x + width,
            y1: y + height,
        })
    }

    fn intersect(self, other: Self) -> Option<Self> {
        let x0 = self.x0.max(other.x0);
        let y0 = self.y0.max(other.y0);
        let x1 = self.x1.min(other.x1);
        let y1 = self.y1.min(other.y1);
        (x1 > x0 && y1 > y0).then_some(Self { x0, y0, x1, y1 })
    }
}

#[derive(Clone)]
struct PiledRenderLayer {
    layer: LayerNode,
    origin_x: f32,
    origin_y: f32,
    clip: PiledClip,
    opacity: f32,
}

#[allow(clippy::too_many_arguments)]
fn collect_piled_render_layers(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
    parent_origin_x: f32,
    parent_origin_y: f32,
    parent_clip: Option<PiledClip>,
    parent_opacity: f32,
    include_position: bool,
    visited: &mut BTreeSet<ObjectHandle>,
    output: &mut Vec<PiledRenderLayer>,
) {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    if !visited.insert(handle) {
        return;
    }
    let Some(target) = registered_render_layer_target(runtime, handle) else {
        return;
    };
    let Some(layer) = render_layer_snapshot(runtime, &target) else {
        return;
    };
    if !layer.renderable || !layer.visible || layer.opacity == 0 {
        return;
    }

    let origin_x = if include_position {
        parent_origin_x + layer.left
    } else {
        parent_origin_x
    };
    let origin_y = if include_position {
        parent_origin_y + layer.top
    } else {
        parent_origin_y
    };
    let Some(layer_clip) = PiledClip::new(
        origin_x,
        origin_y,
        layer_effective_width(&layer),
        layer_effective_height(&layer),
    ) else {
        return;
    };
    let clip = match parent_clip {
        Some(parent_clip) => match parent_clip.intersect(layer_clip) {
            Some(clip) => clip,
            None => return,
        },
        None => layer_clip,
    };
    let opacity = parent_opacity * layer.opacity as f32 / 255.0;
    if opacity <= 0.0 {
        return;
    }
    output.push(PiledRenderLayer {
        layer,
        origin_x,
        origin_y,
        clip,
        opacity,
    });

    let mut children = layer_children(runtime, handle)
        .into_iter()
        .enumerate()
        .collect::<Vec<_>>();
    children.sort_by_key(|(index, child)| {
        let key = registered_render_layer_target(runtime, *child)
            .and_then(|target| render_layer_snapshot(runtime, &target))
            .map(|layer| (layer.z_order, layer.id))
            .unwrap_or((0, 0));
        (key.0, key.1, *index)
    });
    for (_, child) in children {
        collect_piled_render_layers(
            runtime,
            child,
            origin_x,
            origin_y,
            Some(clip),
            opacity,
            true,
            visited,
            output,
        );
    }
}

fn layer_effective_width(layer: &LayerNode) -> f32 {
    let image_width = layer
        .image
        .as_ref()
        .map(|image| image.upload.width as f32)
        .unwrap_or(0.0);
    layer.width.max(layer.image_width).max(image_width)
}

fn layer_effective_height(layer: &LayerNode) -> f32 {
    let image_height = layer
        .image
        .as_ref()
        .map(|image| image.upload.height as f32)
        .unwrap_or(0.0);
    layer.height.max(layer.image_height).max(image_height)
}

#[allow(clippy::too_many_arguments)]
fn composite_piled_layer(
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    layer: &PiledRenderLayer,
    dx: i64,
    dy: i64,
    sx: i64,
    sy: i64,
    width: i64,
    height: i64,
) {
    let Some(source_image) = layer.layer.image.as_ref() else {
        return;
    };
    let source = source_image.upload.rgba.as_ref();
    let source_width = source_image.upload.width;
    let source_height = source_image.upload.height;
    let image_x0 = layer.origin_x + layer.layer.image_left;
    let image_y0 = layer.origin_y + layer.layer.image_top;
    let image_x1 = image_x0 + source_width as f32;
    let image_y1 = image_y0 + source_height as f32;
    let source_rect_x1 = sx.saturating_add(width) as f32;
    let source_rect_y1 = sy.saturating_add(height) as f32;

    let copy_x0 = image_x0.max(layer.clip.x0).max(sx as f32).ceil() as i64;
    let copy_y0 = image_y0.max(layer.clip.y0).max(sy as f32).ceil() as i64;
    let copy_x1 = image_x1.min(layer.clip.x1).min(source_rect_x1).floor() as i64;
    let copy_y1 = image_y1.min(layer.clip.y1).min(source_rect_y1).floor() as i64;
    if copy_x1 <= copy_x0 || copy_y1 <= copy_y0 {
        return;
    }

    let dest_stride = dest_width as usize * 4;
    let source_stride = source_width as usize * 4;
    for root_y in copy_y0..copy_y1 {
        let dest_y = dy + root_y - sy;
        if dest_y < 0 || dest_y >= dest_height as i64 {
            continue;
        }
        let source_y = (root_y as f32 - image_y0).floor() as i64;
        if source_y < 0 || source_y >= source_height as i64 {
            continue;
        }
        for root_x in copy_x0..copy_x1 {
            let dest_x = dx + root_x - sx;
            if dest_x < 0 || dest_x >= dest_width as i64 {
                continue;
            }
            let source_x = (root_x as f32 - image_x0).floor() as i64;
            if source_x < 0 || source_x >= source_width as i64 {
                continue;
            }
            let source_index = source_y as usize * source_stride + source_x as usize * 4;
            let dest_index = dest_y as usize * dest_stride + dest_x as usize * 4;
            if source_index + 4 > source.len() || dest_index + 4 > dest.len() {
                continue;
            }
            let source_pixel = &source[source_index..source_index + 4];
            if source_pixel[3] == 0 {
                continue;
            }
            let dest_pixel = &mut dest[dest_index..dest_index + 4];
            if layer.opacity >= 0.999 {
                blend_pixel(dest_pixel, source_pixel);
            } else {
                let pixel = [
                    source_pixel[0],
                    source_pixel[1],
                    source_pixel[2],
                    ((source_pixel[3] as f32 * layer.opacity).round()).clamp(0.0, 255.0) as u8,
                ];
                blend_pixel(dest_pixel, &pixel);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stretch_copy_pixels(
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    source: &[u8],
    source_texture_width: u32,
    source_texture_height: u32,
    dx: i64,
    dy: i64,
    dest_rect_width: i64,
    dest_rect_height: i64,
    sx: i64,
    sy: i64,
    source_rect_width: i64,
    source_rect_height: i64,
) {
    if dest_rect_width <= 0
        || dest_rect_height <= 0
        || source_rect_width <= 0
        || source_rect_height <= 0
    {
        return;
    }
    let dest_x0 = dx.max(0);
    let dest_y0 = dy.max(0);
    let dest_x1 = dx
        .saturating_add(dest_rect_width)
        .clamp(0, dest_width as i64);
    let dest_y1 = dy
        .saturating_add(dest_rect_height)
        .clamp(0, dest_height as i64);
    if dest_x1 <= dest_x0 || dest_y1 <= dest_y0 {
        return;
    }

    let dest_stride = dest_width as usize * 4;
    let source_stride = source_texture_width as usize * 4;
    for dest_y in dest_y0..dest_y1 {
        let rel_y = dest_y - dy;
        let source_y = sx_scaled_coordinate(sy, rel_y, source_rect_height, dest_rect_height);
        if source_y < 0 || source_y >= source_texture_height as i64 {
            continue;
        }
        for dest_x in dest_x0..dest_x1 {
            let rel_x = dest_x - dx;
            let source_x = sx_scaled_coordinate(sx, rel_x, source_rect_width, dest_rect_width);
            if source_x < 0 || source_x >= source_texture_width as i64 {
                continue;
            }
            let source_index = source_y as usize * source_stride + source_x as usize * 4;
            let dest_index = dest_y as usize * dest_stride + dest_x as usize * 4;
            if source_index + 4 > source.len() || dest_index + 4 > dest.len() {
                continue;
            }
            dest[dest_index..dest_index + 4]
                .copy_from_slice(&source[source_index..source_index + 4]);
        }
    }
}

fn sx_scaled_coordinate(
    source_origin: i64,
    dest_offset: i64,
    source_len: i64,
    dest_len: i64,
) -> i64 {
    let scaled = (dest_offset as i128 * source_len as i128) / dest_len as i128;
    source_origin.saturating_add(scaled as i64)
}

fn dest_min_extent(offset: i64, length: i64) -> u32 {
    offset
        .saturating_add(length)
        .clamp(1, u32::MAX as i64)
        .try_into()
        .unwrap_or(u32::MAX)
}

#[allow(clippy::too_many_arguments)]
fn clipped_copy_rect(
    mut dx: i64,
    mut dy: i64,
    mut sx: i64,
    mut sy: i64,
    mut width: i64,
    mut height: i64,
    dest_width: i64,
    dest_height: i64,
    source_width: i64,
    source_height: i64,
) -> Option<(i64, i64, i64, i64, i64, i64)> {
    if width <= 0 || height <= 0 {
        return None;
    }
    if sx < 0 {
        let delta = -sx;
        sx = 0;
        dx += delta;
        width -= delta;
    }
    if sy < 0 {
        let delta = -sy;
        sy = 0;
        dy += delta;
        height -= delta;
    }
    if dx < 0 {
        let delta = -dx;
        dx = 0;
        sx += delta;
        width -= delta;
    }
    if dy < 0 {
        let delta = -dy;
        dy = 0;
        sy += delta;
        height -= delta;
    }
    width = width
        .min(source_width.saturating_sub(sx))
        .min(dest_width.saturating_sub(dx));
    height = height
        .min(source_height.saturating_sub(sy))
        .min(dest_height.saturating_sub(dy));
    (width > 0 && height > 0).then_some((dx, dy, sx, sy, width, height))
}

fn blend_pixel(dest: &mut [u8], src: &[u8]) {
    let src_a = src[3] as f32 / 255.0;
    let dest_a = dest[3] as f32 / 255.0;
    let out_a = src_a + dest_a * (1.0 - src_a);
    if out_a <= f32::EPSILON {
        dest.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    for channel in 0..3 {
        let src_c = src[channel] as f32 / 255.0;
        let dest_c = dest[channel] as f32 / 255.0;
        let out_c = (src_c * src_a + dest_c * dest_a * (1.0 - src_a)) / out_a;
        dest[channel] = (out_c * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dest[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn sync_layer_image_members(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    width: i64,
    height: i64,
) {
    set_layer_property_storage(runtime, this, "imageLeft", Variant::Integer(0));
    set_layer_property_storage(runtime, this, "imageTop", Variant::Integer(0));
    set_layer_property_storage(runtime, this, "imageWidth", Variant::Integer(width));
    set_layer_property_storage(runtime, this, "imageHeight", Variant::Integer(height));
    if layer_property_value(runtime, this, "width")
        .to_integer()
        .unwrap_or(0)
        <= 0
    {
        set_layer_property_storage(runtime, this, "width", Variant::Integer(width));
    }
    if layer_property_value(runtime, this, "height")
        .to_integer()
        .unwrap_or(0)
        <= 0
    {
        set_layer_property_storage(runtime, this, "height", Variant::Integer(height));
    }
}

pub(crate) struct NativeClassSpec {
    name: &'static str,
    methods: &'static [&'static str],
    properties: &'static [&'static str],
    static_methods: &'static [&'static str],
    static_properties: &'static [&'static str],
}

pub(crate) static TIMER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Timer",
    methods: &[],
    properties: &["interval", "enabled", "capacity", "mode"],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static ASYNC_TRIGGER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "AsyncTrigger",
    methods: &["trigger", "cancel"],
    properties: &["cached", "mode"],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static RECT_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Rect",
    methods: &[
        "isEmpty",
        "setSize",
        "setOffset",
        "addOffset",
        "clear",
        "set",
        "clip",
        "union",
        "intersects",
        "included",
        "includedPos",
        "equal",
    ],
    properties: &[
        "width",
        "height",
        "left",
        "top",
        "right",
        "bottom",
        "nativeArray",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static BITMAP_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Bitmap",
    methods: &[
        "getPixel",
        "setPixel",
        "getMaskPixel",
        "setMaskPixel",
        "independ",
        "setSize",
        "copyFrom",
        "save",
        "load",
        "loadAsync",
        "loadHeader",
        "getSaveOption",
        "onLoaded",
    ],
    properties: &[
        "width",
        "height",
        "buffer",
        "bufferForWrite",
        "bufferPitch",
        "loading",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static IMAGE_FUNCTION_CLASS: NativeClassSpec = NativeClassSpec {
    name: "ImageFunction",
    methods: &[
        "operateAffine",
        "operateRect",
        "operateStretch",
        "flipLR",
        "flipUD",
        "adjustGamma",
        "doBoxBlur",
        "doGrayScale",
        "fillRect",
        "colorRect",
        "drawText",
        "drawGlyph",
    ],
    properties: &[],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static BITMAP_LAYER_TREE_OWNER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "BitmapLayerTreeOwner",
    methods: &[
        "fireClick",
        "fireDoubleClick",
        "fireMouseDown",
        "fireMouseUp",
        "fireMouseMove",
        "fireMouseWheel",
        "fireReleaseCapture",
        "fireMouseOutOfWindow",
        "fireTouchDown",
        "fireTouchUp",
        "fireTouchMove",
        "fireTouchScaling",
        "fireTouchRotate",
        "fireMultiTouch",
        "fireKeyDown",
        "fireKeyUp",
        "fireKeyPress",
        "fireDisplayRotate",
        "fireRecheckInputState",
        "onSetMouseCursor",
        "onGetCursorPos",
        "onSetCursorPos",
        "onReleaseMouseCapture",
        "onSetHintText",
        "onResizeLayer",
        "onChangeLayerImage",
        "onSetAttentionPoint",
        "onDisableAttentionPoint",
        "onSetImeMode",
        "onResetImeMode",
    ],
    properties: &[
        "width",
        "height",
        "bitmap",
        "layerTreeOwnerInterface",
        "focusedLayer",
        "primaryLayer",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static MENU_ITEM_CLASS: NativeClassSpec = NativeClassSpec {
    name: "MenuItem",
    methods: &[
        "add", "insert", "remove", "clear", "click", "onClick", "popup",
    ],
    properties: &[
        "owner", "caption", "shortcut", "checked", "enabled", "visible", "radio", "group",
        "children",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static WINDOW_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Window",
    methods: &[
        "close",
        "beginMove",
        "bringToFront",
        "update",
        "showModal",
        "setMaskRegion",
        "removeMaskRegion",
        "add",
        "remove",
        "setSize",
        "setMinSize",
        "setMaxSize",
        "setPos",
        "setLayerPos",
        "setInnerSize",
        "setZoom",
        "hideMouseCursor",
        "postInputEvent",
        "onResize",
        "onMouseEnter",
        "onMouseLeave",
        "onClick",
        "onDoubleClick",
        "onMouseDown",
        "onMouseUp",
        "onMouseMove",
        "onMouseWheel",
        "onTouchDown",
        "onTouchUp",
        "onTouchMove",
        "onTouchScaling",
        "onTouchRotate",
        "onMultiTouch",
        "onKeyDown",
        "onKeyUp",
        "onKeyPress",
        "onFileDrop",
        "onCloseQuery",
        "onPopupHide",
        "onActivate",
        "onDeactivate",
        "onDisplayRotate",
        "findFullScreenCandidates",
        "registerMessageReceiver",
        "getTouchPoint",
        "getTouchVelocity",
        "getMouseVelocity",
        "resetMouseVelocity",
    ],
    properties: &[
        "visible",
        "caption",
        "width",
        "height",
        "minWidth",
        "minHeight",
        "maxWidth",
        "maxHeight",
        "left",
        "top",
        "focusable",
        "layerLeft",
        "layerTop",
        "innerSunken",
        "innerWidth",
        "innerHeight",
        "zoomNumer",
        "zoomDenom",
        "borderStyle",
        "stayOnTop",
        "showScrollBars",
        "useMouseKey",
        "trapKey",
        "imeMode",
        "mouseCursorState",
        "fullScreen",
        "menu",
        "mainWindow",
        "focusedLayer",
        "primaryLayer",
        "waitVSync",
        "layerTreeOwnerInterface",
        "HWND",
        "drawDevice",
        "touchScaleThreshold",
        "touchRotateThreshold",
        "touchPointCount",
        "hintDelay",
        "enableTouch",
        "displayOrientation",
        "displayRotate",
    ],
    static_methods: &[],
    static_properties: &["mainWindow"],
};

pub(crate) static LAYER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Layer",
    methods: &[
        "asLayer",
        "moveBefore",
        "moveBehind",
        "bringToBack",
        "bringToFront",
        "saveLayerImage",
        "loadImages",
        "freeImage",
        "loadProvinceImage",
        "getMainPixel",
        "setMainPixel",
        "getMaskPixel",
        "setMaskPixel",
        "getProvincePixel",
        "setProvincePixel",
        "getLayerAt",
        "setPos",
        "setSize",
        "setSizeToImageSize",
        "setImagePos",
        "setImageSize",
        "setDefaultCursor",
        "independMainImage",
        "independProvinceImage",
        "setClip",
        "fillRect",
        "colorRect",
        "drawText",
        "drawGlyph",
        "piledCopy",
        "copyRect",
        "copy9Patch",
        "operateRect",
        "stretchCopy",
        "operateStretch",
        "affineCopy",
        "operateAffine",
        "doBoxBlur",
        "adjustGamma",
        "doGrayScale",
        "flipLR",
        "flipUD",
        "convertType",
        "update",
        "setCursorPos",
        "releaseCapture",
        "releaseTouchCapture",
        "focus",
        "focusPrev",
        "focusNext",
        "setMode",
        "removeMode",
        "setAttentionPos",
        "beginTransition",
        "stopTransition",
        "assignImages",
        "exchangeInfo",
        "dump",
        "copyToBitmapFromMainImage",
        "copyFromBitmapToMainImage",
        "onHitTest",
        "onClick",
        "onDoubleClick",
        "onMouseDown",
        "onMouseUp",
        "onMouseMove",
        "onMouseEnter",
        "onMouseLeave",
        "onTouchDown",
        "onTouchUp",
        "onTouchMove",
        "onTouchScaling",
        "onTouchRotate",
        "onMultiTouch",
        "onBlur",
        "onFocus",
        "onNodeEnabled",
        "onNodeDisabled",
        "onKeyDown",
        "onKeyUp",
        "onKeyPress",
        "onMouseWheel",
        "onSearchPrevFocusable",
        "onSearchNextFocusable",
        "onBeforeFocus",
        "onPaint",
        "onTransitionCompleted",
    ],
    properties: &[
        "parent",
        "children",
        "order",
        "absolute",
        "absoluteOrderMode",
        "visible",
        "cached",
        "nodeVisible",
        "opacity",
        "window",
        "isPrimary",
        "left",
        "top",
        "width",
        "height",
        "imageLeft",
        "imageTop",
        "imageWidth",
        "imageHeight",
        "type",
        "face",
        "holdAlpha",
        "clipLeft",
        "clipTop",
        "clipWidth",
        "clipHeight",
        "imageModified",
        "hitType",
        "hitThreshold",
        "cursor",
        "cursorX",
        "cursorY",
        "hint",
        "showParentHint",
        "ignoreHintSensing",
        "focusable",
        "prevFocusable",
        "nextFocusable",
        "joinFocusChain",
        "nodeFocusable",
        "focused",
        "enabled",
        "nodeEnabled",
        "attentionLeft",
        "attentionTop",
        "useAttention",
        "imeMode",
        "callOnPaint",
        "font",
        "name",
        "neutralColor",
        "hasImage",
        "mainImageBuffer",
        "mainImageBufferForWrite",
        "mainImageBufferPitch",
        "provinceImageBuffer",
        "provinceImageBufferForWrite",
        "provinceImageBufferPitch",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static FONT_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Font",
    methods: &[
        "getTextWidth",
        "getTextHeight",
        "getEscWidthX",
        "getEscWidthY",
        "getEscHeightX",
        "getEscHeightY",
        "getGlyphDrawRect",
        "getList",
        "mapPrerenderedFont",
        "unmapPrerenderedFont",
    ],
    properties: &[
        "face",
        "height",
        "bold",
        "italic",
        "strikeout",
        "underline",
        "angle",
        "faceIsFileName",
        "rasterizer",
    ],
    static_methods: &[],
    static_properties: &[],
};

const WAVE_SOUND_BUFFER_METHODS: &[&str] = &[
    "open",
    "play",
    "stop",
    "fade",
    "stopFade",
    "setPos",
    "onStatusChanged",
    "onFadeCompleted",
    "onLabel",
    "freeDirectSound",
    "getVisBuffer",
    "setDefaultCounts",
    "setDefaultAheads",
];

pub(crate) static WAVE_SOUND_BUFFER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "WaveSoundBuffer",
    methods: WAVE_SOUND_BUFFER_METHODS,
    properties: &[
        "position",
        "samplePosition",
        "paused",
        "totalTime",
        "looping",
        "volume",
        "volume2",
        "pan",
        "sampleValue",
        "sampleCount",
        "sampleAhead",
        "posX",
        "posY",
        "posZ",
        "status",
        "frequency",
        "bits",
        "channels",
        "flags",
        "labels",
        "filters",
        "globalVolume",
        "globalFocusMode",
        "useVisBuffer",
    ],
    static_methods: WAVE_SOUND_BUFFER_METHODS,
    static_properties: &["globalVolume", "globalFocusMode", "useVisBuffer"],
};

pub(crate) static PHASE_VOCODER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "PhaseVocoder",
    methods: &[],
    properties: &["interface", "window", "overlap", "pitch", "time"],
    static_methods: &[],
    static_properties: &[],
};

/// Property names of the VideoOverlay native class, shared with the video
/// module (which installs the actual native property handlers).
pub(crate) fn video_overlay_property_names() -> &'static [&'static str] {
    VIDEO_OVERLAY_CLASS.properties
}

pub(crate) static VIDEO_OVERLAY_CLASS: NativeClassSpec = NativeClassSpec {
    name: "VideoOverlay",
    methods: &[
        "open",
        "play",
        "stop",
        "close",
        "setPos",
        "setSize",
        "setBounds",
        "pause",
        "rewind",
        "prepare",
        "setSegmentLoop",
        "cancelSegmentLoop",
        "setPeriodEvent",
        "cancelPeriodEvent",
        "selectAudioStream",
        "setMixingLayer",
        "resetMixingLayer",
        "onStatusChanged",
        "onCallbackCommand",
        "onPeriod",
        "onFrameUpdate",
    ],
    properties: &[
        "position",
        "left",
        "top",
        "width",
        "height",
        "originalWidth",
        "originalHeight",
        "visible",
        "loop",
        "frame",
        "fps",
        "numberOfFrame",
        "totalTime",
        "layer1",
        "layer2",
        "mode",
        "playRate",
        "segmentLoopStartFrame",
        "segmentLoopEndFrame",
        "periodEventFrame",
        "audioBalance",
        "audioVolume",
        "numberOfAudioStream",
        "enabledAudioStream",
        "numberOfVideoStream",
        "enabledVideoStream",
        "mixingMovieAlpha",
        "mixingMovieBGColor",
        "contrastRangeMin",
        "contrastRangeMax",
        "contrastDefaultValue",
        "contrastStepSize",
        "contrast",
        "brightnessRangeMin",
        "brightnessRangeMax",
        "brightnessDefaultValue",
        "brightnessStepSize",
        "brightness",
        "hueRangeMin",
        "hueRangeMax",
        "hueDefaultValue",
        "hueStepSize",
        "hue",
        "saturationRangeMin",
        "saturationRangeMax",
        "saturationDefaultValue",
        "saturationStepSize",
        "saturation",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static BASIC_DRAW_DEVICE_CLASS: NativeClassSpec = NativeClassSpec {
    name: "BasicDrawDevice",
    methods: &["recreate"],
    properties: &["interface", "enableD3D", "preferredDrawer"],
    static_methods: &[],
    static_properties: &["dtNone", "dtDrawDib", "dtDBGDI", "dtDBDD", "dtDBD3D"],
};
