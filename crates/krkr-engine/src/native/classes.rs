use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use super::blend;

use krkr_core::{
    AudioBus, AudioCommand, AudioLoadPolicy, Color, ImageUpload, LayerId, LayerImage, LayerNode,
    ProvinceImage, Size, TransitionMethod, TransitionParams, TransitionScrollFrom,
    TransitionScrollStay, UnknownTransitionName,
};
use krkr_font::{FontSpec, FontSystem, TextLayout, TextStyle};
use krkr_tjs2::{
    Result, TjsError, TjsErrorKind,
    runtime::{
        Closure, NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, TjsHost, Variant,
    },
};

use crate::host::{
    CompletedImageLoad, ImageLoadRequest, ImageLoadTarget, KagLayerSlot, KrkrHost,
    LayerRenderTarget, NativeTransitionCompletion, NativeTransitionStart, ProviderTransition,
    TraceCategory, TransitionFaceLists, TransitionFaces,
};
use crate::plugin_api::transition::{
    TransitionContext, TransitionHandlerProvider, TransitionOptions, TransitionRequest,
    TransitionScriptCallQueue,
};
use crate::resource_manager::decode_province_image;
use crate::scheduler::AsyncTriggerMode;

use super::{
    native_void, register_stub_method_with_arg_count,
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
    install_class_name_constructor(runtime, handle, spec.name, handle, None);
    if global {
        runtime.set_global_member(spec.name, Variant::Object(handle));
    }
    handle
}

/// krkrz publishes every native class's constructor as a member named after
/// the class: `TJS_END_NATIVE_CONSTRUCTOR_DECL(Layer)` ends in
/// `RegisterNCM("Layer", new NCM_Layer())` (tjsNative.h:233), which stores the
/// NCM on the class object at `val = dsp` — an object with no ObjThis
/// (tjsNative.cpp:246).  Scripts lean on that member to run a native
/// initializer on an object of their own: `_Layer.Layer(win, this)` from a
/// class whose first parent is not `Layer` (KAGEX's
/// `GUIAnimButtonObjectBase`, k2compat's layer wrappers) reaches it through
/// the class object, and TJS2 resolves the call's objthis as
/// `clo.ObjThis ? clo.ObjThis : ra[-1]` (tjsInterCodeExec.cpp:2015), so the
/// initializer runs on the *caller's* `this` rather than on the class object.
///
/// When a native class is initialized on an object, `tTJSNativeClass::FuncCall`
/// copies every registered member onto it as `tTJSVariant(val, objthis)`
/// (tjsNative.cpp:295), binding this constructor to that object; pass it as
/// `bound_to`.
fn install_class_name_constructor(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &str,
    class_handle: ObjectHandle,
    bound_to: Option<ObjectHandle>,
) {
    runtime.set_object_member(
        handle,
        class_name,
        Variant::Closure(Closure::new(class_handle, bound_to)),
    );
}

pub(crate) fn construct_native_instance(
    runtime: &mut Runtime<KrkrHost>,
    spec: &'static NativeClassSpec,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // KRKR's BaseTimer/AsyncTrigger native constructors require an action
    // owner as their first argument.  Apart from matching the native
    // BADPARAMCOUNT contract, rejecting an omitted owner prevents a timer
    // that can never dispatch its callback from being registered in the
    // scheduler.
    // `new Timer()` / `new AsyncTrigger()` require an owner. Subclass
    // construction (`new ConductorTimer()`) first invokes the native parent
    // initializer with the instance bound and no args; `super.Timer(owner)`
    // then supplies the action. Reject only the bare `new` path.
    if matches!(spec.name, "Timer" | "AsyncTrigger") && args.is_empty() && this_obj.is_none() {
        return Err(TjsError::runtime(format!(
            "{} requires an action owner",
            spec.name
        )));
    }

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
    if let Variant::Object(class_handle) = runtime.global_member(spec.name) {
        if runtime.object_super_class(handle).is_none() {
            runtime.set_object_super_class(handle, class_handle);
        }
        if class_handle != handle {
            install_class_name_constructor(runtime, handle, spec.name, class_handle, Some(handle));
        }
    }
    // WaveSoundBuffer and VideoOverlay keep their native methods on the class
    // object only: script subclasses override `open`/`play`/`stop` (a game's
    // Movie extends VideoOverlay) and forward with `SUPER.*()`, so installing
    // the natives directly on each instance would shadow those overrides.
    if !matches!(spec.name, "WaveSoundBuffer" | "VideoOverlay") {
        install_methods(runtime, handle, spec.name, spec.methods);
        install_special_methods(runtime, handle, spec.name);
    }
    // These are native events, not instance method implementations. Keeping
    // Layer's placeholders on the instance shadows overrides supplied by
    // script base classes (notably MessageLayer.onPaint, which drives a game's
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
    methods: &'static [NativeMethodSpec],
) {
    for &(method, min_args) in methods {
        if member_visible_in_chain(runtime, handle, method) {
            continue;
        }
        register_stub_method_with_arg_count(runtime, handle, class_name, method, min_args);
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
        "Bitmap" => install_bitmap_native_properties(runtime, handle, false),
        "WaveSoundBuffer" => install_wave_native_properties(runtime, handle, false),
        "VideoOverlay" => install_video_native_properties(runtime, handle, false),
        "AsyncTrigger" => install_async_trigger_native_properties(runtime, handle, false),
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
        "Bitmap" => install_bitmap_native_properties(runtime, handle, true),
        "WaveSoundBuffer" => install_wave_native_properties(runtime, handle, true),
        "VideoOverlay" => install_video_native_properties(runtime, handle, true),
        "AsyncTrigger" => install_async_trigger_native_properties(runtime, handle, true),
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
        let property_handle = runtime.register_object_native_property_with_access(
            handle,
            property,
            property_access(LAYER_READ_ONLY_PROPERTIES, property),
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
        if preserve_script_properties
            && (runtime.object_member_is_property(handle, property)
                || script_property_declares(runtime, handle, property))
        {
            continue;
        }
        let property_handle = runtime.register_object_native_property_with_access(
            handle,
            property,
            property_access(WINDOW_READ_ONLY_PROPERTIES, property),
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

/// `tTJSNI_BaseBitmap`'s `TJS_DENY_NATIVE_PROP_SETTER` members
/// (`BitmapIntf.cpp:452,467,482,496`). Kirakira has no bitmap buffer to hand
/// out yet, so the getters answer `void`; the point of registering them as
/// native properties is the denied write.
fn install_bitmap_native_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    preserve_script_properties: bool,
) {
    for &property in BITMAP_READ_ONLY_PROPERTIES {
        if preserve_script_properties && runtime.object_member_is_property(handle, property) {
            continue;
        }
        // The official getters report the pixel buffer address, its pitch and
        // the async-load flag (`BitmapIntf.cpp:446-500`). Kirakira's Bitmap
        // class has no native implementation behind it yet, so the engine
        // answers `void` exactly as the spec placeholders did -- no script can
        // write them, and reads stay the M2 §1/§2 gap rather than a fabricated
        // pointer.
        let property_handle = runtime.register_object_native_property_with_access(
            handle,
            property,
            NativePropertyAccess::ReadOnly,
            move |_runtime: &mut Runtime<KrkrHost>, _this_obj: Option<ObjectHandle>| {
                Ok(Variant::Void)
            },
            move |_runtime: &mut Runtime<KrkrHost>,
                  _this_obj: Option<ObjectHandle>,
                  _value: Variant| { Ok(()) },
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

fn property_access(read_only: &[&str], name: &str) -> NativePropertyAccess {
    if read_only.contains(&name) {
        NativePropertyAccess::ReadOnly
    } else {
        NativePropertyAccess::ReadWrite
    }
}

fn install_async_trigger_native_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    preserve_script_properties: bool,
) {
    for property in ["cached", "mode"] {
        if preserve_script_properties && runtime.object_member_is_property(handle, property) {
            continue;
        }
        let existing = runtime.object_member(handle, property);
        let property_handle = runtime.register_object_native_property(
            handle,
            property,
            move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                let this = this_obj.unwrap_or(handle);
                let backing =
                    runtime.object_member(this, &async_trigger_property_backing_key(property));
                Ok(if matches!(backing, Variant::Void) {
                    runtime.object_member(this, property)
                } else {
                    backing
                })
            },
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  value: Variant| {
                let this = this_obj.unwrap_or(handle);
                let previous =
                    runtime.object_member(this, &async_trigger_property_backing_key(property));
                let changed = match property {
                    // SetCached receives a bool in KRKR, so compare the
                    // converted truth value rather than the raw Variant
                    // representation (e.g. integer 1 vs string "1").
                    "cached" => {
                        let previous = if matches!(previous, Variant::Void) {
                            Variant::Integer(1)
                        } else {
                            previous
                        };
                        previous.is_truthy() != value.is_truthy()
                    }
                    // SetMode casts to an integer enum before comparing.
                    "mode" => {
                        let previous = if matches!(previous, Variant::Void) {
                            Variant::Integer(0)
                        } else {
                            previous
                        };
                        previous.to_integer().unwrap_or(0) != value.to_integer().unwrap_or(0)
                    }
                    _ => true,
                };
                runtime.set_object_member(
                    this,
                    async_trigger_property_backing_key(property),
                    value,
                );
                if changed {
                    runtime.host_mut().cancel_async(this);
                }
                Ok(())
            },
        );
        if !matches!(existing, Variant::Void) {
            runtime.set_object_member(
                handle,
                async_trigger_property_backing_key(property),
                existing,
            );
        }
        if preserve_script_properties {
            runtime.set_object_member(
                handle,
                property,
                Variant::Closure(Closure::new(property_handle, Some(handle))),
            );
        }
    }
}

fn async_trigger_property_backing_key(name: &str) -> String {
    format!("__nativeAsyncTriggerProperty${name}")
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
        // `filters` is `TJS_DENY_NATIVE_PROP_SETTER` in the reference
        // (`WaveIntf.cpp:1552`): script may read the instance's array but
        // a write fails with `TJS_E_ACCESSDENYED` (-1007) before any accessor
        // runs.  Every other member of the list is a normal read/write
        // property.
        let access = if property == "filters" {
            NativePropertyAccess::ReadOnly
        } else {
            NativePropertyAccess::ReadWrite
        };
        let property_handle = runtime.register_object_native_property_with_access(
            handle,
            property,
            access,
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
            // KRKR's default timer queue capacity is six events per timer.
            runtime.set_object_member(handle, "capacity", Variant::Integer(6));
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
            // The window's child registry is engine-internal: official `Window`
            // has no `children` member (M2 §5a, M7 §7), so the array lives only
            // behind `sync_children_array` and the host's window instance.
            let children = runtime.alloc_array_object(Vec::new());
            runtime
                .host_mut()
                .register_native_window(handle, Some(children));
            let menu = alloc_menu_item_object(runtime, Some(handle), String::new());
            runtime.set_object_member(handle, "menu", Variant::Object(menu));
            let draw_device =
                construct_native_instance(runtime, &BASIC_DRAW_DEVICE_CLASS, None, Vec::new())?;
            runtime.set_object_member(handle, "drawDevice", draw_device);
            // `Window.mainWindow` is a native property on the class and on every
            // instance (`WindowIntf.cpp:1796`, "static" in the reference);
            // `register_native_window` above already recorded the first
            // constructed window as the host's main window, which is what the
            // getters report.
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
            // Runs on an already initialized script subclass instance
            // (`super.MenuItem(...)`), so keep a script override of `index`.
            install_menu_item_index_property(runtime, handle, true);
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
                // A script subclass may run this constructor twice (an
                // intermediate `super.Layer()`); the official instance keeps
                // one `ChildrenArray` across `Construct` calls
                // (`LayerIntf.h:237`, created in the C++ ctor only), so reuse
                // whatever the instance already registered.
                let children = runtime
                    .host()
                    .native_layer_children_array(handle)
                    .unwrap_or_else(|| runtime.alloc_array_object(Vec::new()));
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
                set_layer_property_storage(
                    runtime,
                    handle,
                    "width",
                    Variant::Integer(DEFAULT_LAYER_SIZE as i64),
                );
                set_layer_property_storage(
                    runtime,
                    handle,
                    "height",
                    Variant::Integer(DEFAULT_LAYER_SIZE as i64),
                );
                set_layer_property_storage(runtime, handle, "imageLeft", Variant::Integer(0));
                set_layer_property_storage(runtime, handle, "imageTop", Variant::Integer(0));
                set_layer_property_storage(
                    runtime,
                    handle,
                    "imageWidth",
                    Variant::Integer(DEFAULT_LAYER_SIZE as i64),
                );
                set_layer_property_storage(
                    runtime,
                    handle,
                    "imageHeight",
                    Variant::Integer(DEFAULT_LAYER_SIZE as i64),
                );
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
                // `tTJSNI_BaseLayer` ctor: `HoldAlpha = TVPDefaultHoldAlpha` (false).
                set_layer_property_storage(runtime, handle, "holdAlpha", Variant::Integer(0));
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
                // The layer joins its parent inside `register_native_layer`;
                // the script visible `children` array follows the tree list on
                // its next read (`GetChildrenArrayObjectNoAddRef`,
                // `LayerIntf.cpp:630`), so nothing is pushed here.
                //
                // `tTJSNI_BaseLayer` ctor (`LayerIntf.cpp:342` / `:404`): Rect is
                // 32×32 and `AllocateDefaultImage` copies the 32×32 transparent
                // white holder. Drawable layers never start with no bitmap.
                // `hasImage` records that intent for re-attachment.
                set_layer_property_storage(runtime, handle, "hasImage", Variant::Integer(1));
                allocate_default_layer_image(runtime, handle);
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
            set_wave_status(runtime, handle, "unload");
            runtime.set_object_member(handle, "volume", Variant::Integer(100000));
            runtime.set_object_member(handle, "pan", Variant::Integer(0));
            runtime.set_object_member(handle, "looping", Variant::Integer(0));
            set_wave_property_storage(runtime, handle, "paused", Variant::Integer(0));
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

/// Official `tTJSNI_BaseLayer::GetChildrenArrayObjectNoAddRef`
/// (`LayerIntf.cpp:630`): every read hands out the *same* array object, and
/// its contents are rebuilt in place from the live tree whenever a tree
/// change dirtied it.  Script writes into that array never reach the tree and
/// are dropped by the next rebuild.
fn sync_children_array(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
) -> Option<ObjectHandle> {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    let array = match runtime
        .host()
        .native_layer_children_array(handle)
        .or_else(|| runtime.host().native_window_children_array(handle))
    {
        Some(array) => array,
        None => {
            let array = runtime.alloc_array_object(Vec::new());
            if !runtime.host_mut().register_children_array(handle, array) {
                return None;
            }
            array
        }
    };
    if !runtime.host_mut().take_native_children_array_dirty(handle) {
        return Some(array);
    }
    // Ask again which list this instance keeps: a layer registered without a
    // cached array got one just above, and reading the window list for it
    // would leave the array permanently empty.
    let children = if runtime.host().native_layer_children_array(handle).is_some() {
        runtime.host().native_layer_children(handle)
    } else {
        runtime.host().native_window_children(handle)
    };
    runtime.array_clear(array);
    for child in children {
        runtime.array_push(array, children_array_entry(child));
    }
    Some(array)
}

/// The value a children array stores for one child.
///
/// Official inserts `tTJSVariant(dsp, dsp)` (`LayerIntf.cpp:665`), so
/// `parent.children[0].focus()` runs with the child as its own objthis. The
/// engine's own array writes (`Window.add`/`Window.remove`) must use the same
/// shape or they cannot find an entry the rebuild created.
fn children_array_entry(child: ObjectHandle) -> Variant {
    self_bound(Variant::Object(child))
}

/// The engine-internal "the array a window/layer keeps its children in"
/// accessor. Official `Layer.children` is the one script-visible view of this
/// list; `Window` has no such member at all (M2 §5a), so `Window.add`/
/// `Window.remove` and layer invalidation reach it through here.
fn ensure_child_array(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) -> ObjectHandle {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    if let Some(children) = sync_children_array(runtime, handle) {
        return children;
    }
    let children = runtime.alloc_array_object(Vec::new());
    runtime.host_mut().register_children_array(handle, children);
    children
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
    } else if matches!(
        class_name,
        "Rect" | "Bitmap" | "BitmapLayerTreeOwner" | "PhaseVocoder" | "BasicDrawDevice"
    ) {
        // Classes whose only special method is the empty native `finalize`:
        // Rect (`RectItf.cpp:44`), Bitmap (`BitmapIntf.cpp:195`),
        // BitmapLayerTreeOwner (`BitmapLayerTreeOwner.cpp:200`), PhaseVocoder
        // (`PhaseVocoderFilter.cpp:29`) and BasicDrawDevice
        // (`win32/BasicDrawDevice.cpp:855`), all declaring
        // `TJS_DECL_EMPTY_FINALIZE_METHOD` (`tjsNative.h:380-383`).
        install_empty_finalize_method(runtime, handle);
    }
}

/// The no-op `finalize` krkrz declares on a native class with
/// `TJS_DECL_EMPTY_FINALIZE_METHOD` (`tjsNative.h:380-383`). The native
/// instance's real teardown lives in its destructor, so the member only has to
/// exist: scripts reach it on the class object (`global.Rect.finalize(...)`)
/// and through a script subclass's `super.finalize(...)`, and a missing member
/// aborts the caller with `Member "finalize" does not exist`. The
/// script-preserving shape keeps a subclass's own `finalize` when the native
/// constructor runs on its instance, the same rule the class's other methods
/// follow.
fn install_empty_finalize_method(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_native_method_preserving_script(runtime, handle, "finalize", native_void);
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
    install_menu_item_index_property(runtime, handle, false);
    handle
}

/// `MenuItem.index` (`plugins/win32/menu/manual.tjs:134`) is a native property
/// in the reference menu plugin; `preserve_script_properties` keeps a script
/// subclass's own `index` member when the native constructor runs on an
/// already initialized instance.
fn install_menu_item_index_property(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    preserve_script_properties: bool,
) {
    if preserve_script_properties && runtime.object_member_is_property(handle, "index") {
        return;
    }
    let property_handle = runtime.register_object_native_property(
        handle,
        "index",
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            menu_item_index(runtime, this_obj)
        },
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            let index = value.to_integer().unwrap_or(0);
            set_menu_item_index(runtime, this_obj, index)
        },
    );
    if preserve_script_properties {
        runtime.set_object_member(
            handle,
            "index",
            Variant::Closure(Closure::new(property_handle, Some(handle))),
        );
    }
}

fn install_menu_item_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // Native constructors can be invoked on an already initialized script
    // subclass instance (`super.MenuItem(...)`).  KRKR keeps native methods
    // on the native class, so that constructor must not replace overrides
    // such as KAGMenuItem.click/onClick on the leaf instance.
    //
    // The menu plugin's `tTJSNC_MenuItem` declares an empty `finalize` with
    // `TJS_DECL_EMPTY_FINALIZE_METHOD` (`MenuItemIntf.cpp:249`; the krkrz tree
    // carries the menu plugin out of tree, so the anchor is its
    // kirikiroid2/plugin copy); KAGEX's `KAGMenuItem` subclasses it and a
    // class-qualified or `super.*` finalize call must resolve to the no-op.
    register_native_method_preserving_script(runtime, handle, "finalize", native_void);
    // krkr2's `tTJSNC_MenuItem` floors (`plugins/win32/menu/MenuItemIntf.cpp`):
    // `add` :263, `insert` :273, `remove` :284 and `popup` :294, all plain
    // `if(numparams < N) return TJS_E_BADPARAMCOUNT;` tests.  `popup` stays
    // the no-op stub behind its floor: the engine has no window menu to track.
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "add",
        NativeArgCount::AtLeast(1),
        menu_item_add,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "insert",
        NativeArgCount::AtLeast(2),
        menu_item_insert,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "remove",
        NativeArgCount::AtLeast(1),
        menu_item_remove,
    );
    register_native_method_preserving_script(runtime, handle, "clear", menu_item_clear);
    register_native_method_preserving_script(runtime, handle, "click", menu_item_noop);
    register_native_method_preserving_script(runtime, handle, "onClick", menu_item_noop);
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "popup",
        NativeArgCount::AtLeast(3),
        menu_item_noop,
    );
}

fn menu_item_children(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    method: &str,
) -> Result<ObjectHandle> {
    let this =
        this_obj.ok_or_else(|| krkr_tjs2::TjsError::runtime(format!("{method} requires this")))?;
    match runtime.object_member(this, "children").object_handle() {
        Some(children) => Ok(children),
        None => {
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
    if let Some(child) = item.object_handle() {
        runtime.set_object_member(child, "parent", Variant::Void);
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
        if let Some(child) = item.object_handle() {
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
    let (Some(this), Some(child)) = (this_obj, item.object_handle()) else {
        return;
    };
    runtime.set_object_member(child, "parent", Variant::Object(this));
}

fn variant_object_handle(value: &Variant) -> Option<ObjectHandle> {
    match value {
        Variant::Object(handle) => Some(*handle),
        Variant::Closure(closure) => Some(closure.object),
        _ => None,
    }
}

/// The array a menu item lives in, or `None` when it has no parent
/// (`MenuItem.parent` is void for a root item).
fn menu_item_siblings(runtime: &Runtime<KrkrHost>, this: ObjectHandle) -> Option<ObjectHandle> {
    let parent = runtime.object_member(this, "parent").object_handle()?;
    runtime.object_member(parent, "children").object_handle()
}

/// `MenuItem.index` (`plugins/win32/menu/manual.tjs:134`): the item's position
/// among the children of its parent, 0 based.
fn menu_item_index(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    let Some(this) = this_obj else {
        return Ok(Variant::Integer(0));
    };
    let Some(children) = menu_item_siblings(runtime, this) else {
        return Ok(Variant::Integer(0));
    };
    let position = runtime
        .array_elements(children)
        .and_then(|elements| {
            elements
                .iter()
                .position(|element| variant_object_handle(element) == Some(this))
        })
        .unwrap_or(0);
    Ok(Variant::Integer(position as i64))
}

/// Assigning `MenuItem.index` moves the item to that position among its
/// siblings, the same way the reference menu handle does.
fn set_menu_item_index(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    index: i64,
) -> Result<()> {
    let Some(this) = this_obj else {
        return Ok(());
    };
    let Some(children) = menu_item_siblings(runtime, this) else {
        return Ok(());
    };
    let elements = runtime
        .array_elements(children)
        .map(<[Variant]>::to_vec)
        .unwrap_or_default();
    let Some(position) = elements
        .iter()
        .position(|element| variant_object_handle(element) == Some(this))
    else {
        return Ok(());
    };
    let value = elements[position].clone();
    runtime.array_remove_value(children, &value);
    let target = index.clamp(0, (elements.len() - 1) as i64) as usize;
    runtime.array_insert(children, target, value);
    Ok(())
}

fn menu_item_noop(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

/// `tTJSNC_Window`'s floors (`visual/WindowIntf.cpp`: `add` :832,
/// `remove` :842, `setPos` :879, `setSize` :852, `setInnerSize` :899,
/// `setZoom` :908) at the registration sites.  `onCloseQuery` (:1204) and the
/// event methods are unguarded in the reference.
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
    runtime.register_object_native_with_arg_count(
        handle,
        "add",
        NativeArgCount::AtLeast(1),
        window_add,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "remove",
        NativeArgCount::AtLeast(1),
        window_remove,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setPos",
        NativeArgCount::AtLeast(2),
        window_set_pos,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setSize",
        NativeArgCount::AtLeast(2),
        window_set_size,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setInnerSize",
        NativeArgCount::AtLeast(2),
        window_set_inner_size,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setZoom",
        NativeArgCount::AtLeast(2),
        window_set_zoom,
    );
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
    #[cfg(target_arch = "wasm32")]
    {
        // The Web shell has no separate native dialog surface. Suspending on
        // showModal would leave startup scripts blocked forever, so expose
        // the window state but continue synchronously until a DOM dialog
        // bridge is available.
        runtime
            .host_mut()
            .log(&format!("Window.showModal skipped on Web handle={this:?}"));
        Ok(Variant::Void)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        runtime.host_mut().push_modal_window(this);
        runtime
            .host_mut()
            .log(&format!("Window.showModal handle={this:?}"));
        runtime.request_suspend();
        Ok(Variant::Void)
    }
}

fn window_close(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = require_window_this(runtime, this_obj, "Window.close")?;
    set_window_property_storage(runtime, this, "visible", Variant::Integer(0));
    set_window_property_storage(runtime, this, "__nativeClosed", Variant::Integer(1));
    runtime
        .host_mut()
        .log(&format!("Window.close handle={this:?}"));
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
    runtime.host().main_window() == Some(window)
}

fn window_add(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Window.add requires this"))?;
    let item = args.first().cloned().unwrap_or_default();
    // The reference takes the argument through `AsObjectClosureNoAddRef()`
    // (`WindowIntf.cpp:829-847`), so a self-bound value -- `layer.parent`,
    // `parent.children[0]` -- names the object it binds.
    let Some(item_handle) = variant_object(&item) else {
        return Ok(Variant::Void);
    };
    let children = ensure_child_array(runtime, this);
    runtime.array_remove_value(children, &children_array_entry(item_handle));
    runtime.array_push(children, children_array_entry(item_handle));
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
    // See `window_add`: the argument is an object closure, and a self-bound
    // value names the layer it binds.
    let Some(item_handle) = variant_object(&item) else {
        return Ok(Variant::Void);
    };
    let children = ensure_child_array(runtime, this);
    runtime.array_remove_value(children, &children_array_entry(item_handle));
    runtime
        .host_mut()
        .remove_native_window_child(this, item_handle);
    // `primaryLayer`/`focusedLayer` are native properties whose value lives in
    // the host instance, so the comparison runs against that storage rather
    // than against the accessor the object member holds.
    if runtime.host().native_window_primary_layer(this) == Some(item_handle) {
        set_window_property_storage(runtime, this, "primaryLayer", Variant::Void);
    }
    if runtime.host().native_window_focused_layer(this) == Some(item_handle) {
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
    set_window_inner_size_members(runtime, this, width, height);
    Ok(Variant::Void)
}

/// Official `SetInnerSize` (`WindowFormUnit.cpp`) stores the logical size and
/// recomputes the paint box, which is `inner * zoomNumer / zoomDenom`; the
/// window's `width`/`height` report the paint box.
fn set_window_inner_size_members(
    runtime: &mut Runtime<KrkrHost>,
    window: ObjectHandle,
    width: i64,
    height: i64,
) {
    set_window_property_storage(runtime, window, "innerWidth", Variant::Integer(width));
    set_window_property_storage(runtime, window, "innerHeight", Variant::Integer(height));
    let (paint_width, paint_height) = window_paint_box_size(runtime, window, width, height);
    set_window_property_storage(runtime, window, "width", Variant::Integer(paint_width));
    set_window_property_storage(runtime, window, "height", Variant::Integer(paint_height));
}

fn window_property_i64(
    runtime: &Runtime<KrkrHost>,
    window: ObjectHandle,
    name: &str,
    fallback: i64,
) -> i64 {
    runtime
        .host()
        .native_window_property(window, name)
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(fallback)
}

fn window_paint_box_size(
    runtime: &Runtime<KrkrHost>,
    window: ObjectHandle,
    inner_width: i64,
    inner_height: i64,
) -> (i64, i64) {
    let numer = window_property_i64(runtime, window, "zoomNumer", 100).max(0);
    let denom = window_property_i64(runtime, window, "zoomDenom", 100).max(1);
    (
        inner_width.saturating_mul(numer) / denom,
        inner_height.saturating_mul(numer) / denom,
    )
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

/// The `numparams` floor every `tTJSNI_BaseLayer` method declares in
/// `visual/LayerIntf.cpp` is registered as `NativeArgCount::AtLeast(N)`, so a
/// short call reports `TJS_E_BADPARAMCOUNT` (-1004) before the handler runs --
/// the order the reference validates in (`if(numparams < N) return
/// TJS_E_BADPARAMCOUNT;`).  `setClip` and `update` are the exceptions: their
/// accepted set is *0 arguments, or at least N* (`LayerIntf.cpp:7001/7010` and
/// `:7645/7652`), which `AtLeast` cannot express, so their handlers keep the
/// count checks.
fn install_layer_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_native_method_preserving_script(runtime, handle, "finalize", layer_void);
    register_native_method_preserving_script(runtime, handle, "asLayer", layer_as_layer);
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "loadImages",
        NativeArgCount::AtLeast(1),
        layer_load_images,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "saveLayerImage",
        NativeArgCount::AtLeast(1),
        layer_save_layer_image,
    );
    register_native_method_preserving_script(runtime, handle, "freeImage", layer_free_image);
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "setPos",
        NativeArgCount::AtLeast(2),
        layer_set_pos,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "setSize",
        NativeArgCount::AtLeast(2),
        layer_set_size,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "setImagePos",
        NativeArgCount::AtLeast(2),
        layer_set_image_pos,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "setImageSize",
        NativeArgCount::AtLeast(2),
        layer_set_image_size,
    );
    // `if(numparams == 0)` resets the clip rect and only a count between 1 and
    // 3 is an error (`LayerIntf.cpp:7001/7010`), so the handler keeps the
    // count rule.
    register_native_method_preserving_script(runtime, handle, "setClip", layer_set_clip);
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getMainPixel",
        NativeArgCount::AtLeast(2),
        layer_get_main_pixel,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "setMainPixel",
        NativeArgCount::AtLeast(3),
        layer_set_main_pixel,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getMaskPixel",
        NativeArgCount::AtLeast(2),
        layer_get_mask_pixel,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "setMaskPixel",
        NativeArgCount::AtLeast(3),
        layer_set_mask_pixel,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "setCursorPos",
        NativeArgCount::AtLeast(2),
        layer_set_cursor_pos,
    );
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
    // `if(numparams < 1) return TJS_E_BADPARAMCOUNT;` (`LayerIntf.cpp:7835`):
    // the reference validates the argument *count* before it looks at the
    // argument, so `assignImages()` reports "Invalid argument count" (-1004)
    // and only a present non-Layer value reports `TVPSpecifyLayer`.
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "assignImages",
        NativeArgCount::AtLeast(1),
        layer_assign_images,
    );
    register_native_method_preserving_script(runtime, handle, "exchangeInfo", layer_exchange_info);
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "beginTransition",
        NativeArgCount::AtLeast(1),
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
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "fillRect",
        NativeArgCount::AtLeast(5),
        layer_fill_rect,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "colorRect",
        NativeArgCount::AtLeast(5),
        layer_color_rect,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "copyRect",
        NativeArgCount::AtLeast(7),
        layer_copy_rect,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "operateRect",
        NativeArgCount::AtLeast(7),
        layer_operate_rect,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "piledCopy",
        NativeArgCount::AtLeast(7),
        layer_piled_copy,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "stretchCopy",
        NativeArgCount::AtLeast(9),
        layer_stretch_copy,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "affineCopy",
        NativeArgCount::AtLeast(12),
        layer_affine_copy,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "operateStretch",
        NativeArgCount::AtLeast(9),
        layer_operate_stretch,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "operateAffine",
        NativeArgCount::AtLeast(12),
        layer_operate_affine,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "drawText",
        NativeArgCount::AtLeast(4),
        layer_draw_text,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "drawGlyph",
        NativeArgCount::AtLeast(4),
        layer_draw_glyph,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "loadProvinceImage",
        NativeArgCount::AtLeast(1),
        layer_load_province_image,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getProvincePixel",
        NativeArgCount::AtLeast(2),
        layer_get_province_pixel,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "setProvincePixel",
        NativeArgCount::AtLeast(3),
        layer_set_province_pixel,
    );
    register_native_method_preserving_script(
        runtime,
        handle,
        "independProvinceImage",
        layer_independ_province_image,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getLayerAt",
        NativeArgCount::AtLeast(2),
        layer_get_layer_at,
    );
    // `if(numparams < 1)` selects the whole-layer update path and does not
    // error; only a partial update below four arguments is rejected
    // (`LayerIntf.cpp:7645/7652`), so the handler keeps the count rule.
    register_native_method_preserving_script(runtime, handle, "update", layer_update);
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "convertType",
        NativeArgCount::AtLeast(1),
        layer_convert_type,
    );
    register_native_method_preserving_script(runtime, handle, "focus", layer_focus);
    register_native_method_preserving_script(runtime, handle, "focusPrev", layer_focus_prev);
    register_native_method_preserving_script(runtime, handle, "focusNext", layer_focus_next);
    register_native_method_preserving_script(runtime, handle, "setMode", layer_set_mode);
    register_native_method_preserving_script(runtime, handle, "removeMode", layer_remove_mode);
    register_native_method_preserving_script(runtime, handle, "releaseCapture", layer_void);
    register_native_method_preserving_script(runtime, handle, "onClick", layer_on_click);
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "onHitTest",
        NativeArgCount::AtLeast(3),
        layer_on_hit_test,
    );
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

/// `tTJSNC_Font`'s methods floor at one argument in the reference
/// (`LayerIntf.cpp:9919-10032`, one `if(numparams < 1) return
/// TJS_E_BADPARAMCOUNT;` each); `unmapPrerenderedFont`'s `< 0` test
/// (`:10043`) is dead, so it stays unguarded.
fn install_font_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // `tTJSNC_Font` declares an empty `finalize` with
    // `TJS_DECL_EMPTY_FINALIZE_METHOD` (`LayerIntf.cpp:9903`); the font
    // handle's real teardown lives in the native instance. Scripts reach the
    // member on the *class object* (`global.Font.finalize(...)`) and through a
    // script subclass's `super.finalize(...)`; without it the member call
    // aborts the caller with `Member "finalize" does not exist`.
    register_native_method_preserving_script(runtime, handle, "finalize", native_void);
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getTextWidth",
        NativeArgCount::AtLeast(1),
        font_get_text_width,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getTextHeight",
        NativeArgCount::AtLeast(1),
        font_get_text_height,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getEscWidthX",
        NativeArgCount::AtLeast(1),
        font_get_esc_width_x,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getEscWidthY",
        NativeArgCount::AtLeast(1),
        font_get_esc_width_y,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getEscHeightX",
        NativeArgCount::AtLeast(1),
        font_get_esc_height_x,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getEscHeightY",
        NativeArgCount::AtLeast(1),
        font_get_esc_height_y,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getGlyphDrawRect",
        NativeArgCount::AtLeast(1),
        font_get_glyph_draw_rect,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "getList",
        NativeArgCount::AtLeast(1),
        font_get_list,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "mapPrerenderedFont",
        NativeArgCount::AtLeast(1),
        font_map_prerendered_font,
    );
    register_native_method_preserving_script(
        runtime,
        handle,
        "unmapPrerenderedFont",
        font_unmap_prerendered_font,
    );
}

/// `tTJSNC_ImageFunction`'s two real methods floor at the reference counts
/// (`ImageFunction.cpp:829` for `drawText`, `:915` for `drawGlyph`).
fn install_image_function_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // `tTJSNC_ImageFunction` declares an empty `finalize` with
    // `TJS_DECL_EMPTY_FINALIZE_METHOD` (`ImageFunction.cpp:114`); the bitmap
    // work happens in this instance's methods, so its `finalize` is the
    // reference's no-op. Scripts reach it on the class object
    // (`global.ImageFunction.finalize(...)`) and through a script subclass's
    // `super.finalize(...)`.
    register_native_method_preserving_script(runtime, handle, "finalize", native_void);
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "drawText",
        NativeArgCount::AtLeast(6),
        image_function_draw_text,
    );
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        "drawGlyph",
        NativeArgCount::AtLeast(5),
        image_function_draw_glyph,
    );
}

type NativeMethod =
    fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>;

/// Official `tTJSNI_BaseLayer` ctor Rect / `TVPTempBitmapHolder` size (`LayerIntf.cpp:78`).
const DEFAULT_LAYER_SIZE: u32 = 32;
/// `TVP_RGBA2COLOR(255, 255, 255, 0)` — the default holder fill.
const DEFAULT_LAYER_IMAGE_RGBA: [u8; 4] = [255, 255, 255, 0];

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
    "holdAlpha",
    "clipLeft",
    "clipTop",
    "clipWidth",
    "clipHeight",
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
    "prevFocusable",
    "nextFocusable",
    "nodeFocusable",
    "focused",
    "mainImageBuffer",
    "mainImageBufferForWrite",
    "mainImageBufferPitch",
    "provinceImageBuffer",
    "provinceImageBufferForWrite",
    "provinceImageBufferPitch",
];

/// Official `TJS_DENY_NATIVE_PROP_SETTER` members of `tTJSNI_BaseLayer`
/// (`LayerIntf.cpp:8532 children`, `:8646 nodeVisible`, `:8689 window`,
/// `:8703 isPrimary`, `:9229 prevFocusable`, `:9252 nextFocusable`,
/// `:9286 nodeFocusable`, `:9300 focused`, `:9334 nodeEnabled`,
/// `:9523/:9537/:9551 mainImageBuffer{,ForWrite,Pitch}`,
/// `:9565/:9579/:9593 provinceImageBuffer{,ForWrite,Pitch}`). A script write
/// fails with `TJS_E_ACCESSDENYED` (-1007) before any engine state is touched;
/// the engine's own writes go through the host storage and the backing keys,
/// which the access policy does not consult.
///
/// `font` (`:9449`) is denied as well. The two store shapes behave
/// differently, exactly as the reference has them: `layer.font = x` meets the
/// denied setter and fails with -1007, while the `TJS_IGNOREPROP` store
/// `&layer.font = x` skips the property object entirely and overwrites the
/// member (`tTJSCustomObject::PropSet`, `tjsObject.cpp:1519-1552`). KAGEX
/// replaces a layer's font with a `FontHook` through that second shape
/// (`sysscn/prerenderfontex.tjs`, `&a2.font = this` compiled to `spds`), and
/// the engine's own font resolution follows it: it reads the font through TJS
/// dispatch because a game may wrap it in a hook (`resolve_font_member`).
const LAYER_READ_ONLY_PROPERTIES: &[&str] = &[
    "children",
    "nodeVisible",
    "window",
    "isPrimary",
    "prevFocusable",
    "nextFocusable",
    "nodeFocusable",
    "focused",
    "nodeEnabled",
    "mainImageBuffer",
    "mainImageBufferForWrite",
    "mainImageBufferPitch",
    "provinceImageBuffer",
    "provinceImageBufferForWrite",
    "provinceImageBufferPitch",
    "font",
];

/// `WindowIntf.cpp:1813 mainWindow`, `:1872 primaryLayer`,
/// `:1906 layerTreeOwnerInterface`.
const WINDOW_READ_ONLY_PROPERTIES: &[&str] =
    &["mainWindow", "primaryLayer", "layerTreeOwnerInterface"];

/// `BitmapIntf.cpp:452 buffer`, `:467 bufferForWrite`, `:482 bufferPitch`,
/// `:496 loading`.
const BITMAP_READ_ONLY_PROPERTIES: &[&str] =
    &["buffer", "bufferForWrite", "bufferPitch", "loading"];

const WINDOW_NATIVE_PROPERTIES: &[&str] = &[
    "visible",
    "left",
    "top",
    "width",
    "height",
    "innerWidth",
    "innerHeight",
    "primaryLayer",
    "focusedLayer",
    "mainWindow",
    "layerTreeOwnerInterface",
];

fn window_native_property_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    match name {
        // `tTJSNI_BaseWindow::GetPrimaryLayer()` throws `TVPWindowHasNoLayer`
        // when the draw device has no primary layer (`WindowIntf.cpp:1856-1868`,
        // `IDS_TVP_WINDOW_HAS_NO_LAYER` = "Window has no layer"), otherwise the
        // layer is handed out self-bound.
        "primaryLayer" => {
            let Some(layer) = runtime.host().native_window_primary_layer(this) else {
                return Err(TjsError::runtime("Window has no layer"));
            };
            return Ok(self_bound(Variant::Object(layer)));
        }
        // `tTJSVariant(dsp, dsp)`, NULL when nothing is focused (`:1824`).
        "focusedLayer" => {
            return Ok(runtime
                .host()
                .native_window_focused_layer(this)
                .map(|layer| self_bound(Variant::Object(layer)))
                .unwrap_or(Variant::Null));
        }
        // `Window.mainWindow` is a class property in the reference
        // (`WindowIntf.cpp:1796`, declared static); the TJS class-chain lookup
        // makes the same getter reachable from an instance, which is why every
        // `tTJSNI_Window` reports the process's main window here.
        "mainWindow" => {
            return Ok(runtime
                .host()
                .main_window()
                .map(|window| self_bound(Variant::Object(window)))
                .unwrap_or(Variant::Null));
        }
        // `reinterpret_cast<tjs_int64>(static_cast<iTVPLayerTreeOwner*>(_this))`
        // (`:1896-1904`). The handle is the engine's identity for the object,
        // which is what a script can compare or pass on.
        "layerTreeOwnerInterface" => return Ok(Variant::Integer(this.0 as i64)),
        _ => {}
    }
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
    // A denied accessor must survive the engine's own writes: replacing it with
    // a plain member would re-open the property to script writes. Every other
    // member is stored directly, which is what the engine's raw-member readers
    // (and `Window.left`-style assertions) have always seen -- except for the
    // two members a script class may override (`primaryLayer`, `focusedLayer`),
    // where the script declaration has to keep winning. Everything else keeps
    // the store: KAGEX's zoom menu reads the mirrored `zoomNumer`/`zoomDenom`
    // values, and the host mirror carries the engine's value for the getters
    // either way.
    let script_owns_member = matches!(name, "primaryLayer" | "focusedLayer")
        && script_property_declares(runtime, handle, name);
    if !member_is_denied_property(runtime, handle, name) && !script_owns_member {
        runtime.set_object_member(handle, name, value);
    }
}

/// Whether `name` is declared as a *script* property by the object itself or by
/// a class in its chain.
///
/// The reference keeps Window's native members on the class object
/// (`tTJSNC_Window`'s `TJS_BEGIN_NATIVE_PROP_DECL(primaryLayer)`,
/// `WindowIntf.cpp:1856-1874`), so a script subclass's declaration wins a
/// script read. Kirakira installs the natives per instance and mirrors engine
/// state into the window's script members (`set_window_property_storage`), and
/// such a store shadows the declaration: KAGEX's `KAGWindow.primaryLayer`
/// (`MainWindow.tjs:4839`, `sysbase` or the image-less `_primaryLayer`) stopped
/// being consulted once `Window.add`/`Layer.add` stored the first layer over
/// it, so `kag.primaryLayer` answered `_primaryLayer` and
/// `temp.piledCopy(0, 0, kag.primaryLayer, ...)` (`custom.tjs:196`) threw
/// `Source layer has no image` (`scnchart.ks:28`).
///
/// The twin of `video::chain_has_script_property`, which keeps a game `Movie`'s
/// `left`/`top`/... declarations from being shadowed by `VideoOverlay`'s
/// per-instance native members.
fn script_property_declares(runtime: &Runtime<KrkrHost>, handle: ObjectHandle, name: &str) -> bool {
    let own = runtime.object_member(handle, name);
    if runtime.variant_is_property(&own) && !runtime.variant_is_native_property(&own) {
        return true;
    }
    let mut current = runtime.object_super_class(handle);
    while let Some(class_handle) = current {
        if runtime.object_member_is_property(class_handle, name) {
            let member = runtime.object_member(class_handle, name);
            if !runtime.variant_is_native_property(&member) {
                return true;
            }
        }
        current = runtime.object_super_class(class_handle);
    }
    false
}

/// Whether `name` is a native property of `object` whose access policy refuses
/// script writes (`TJS_DENY_NATIVE_PROP_SETTER`). The engine's internal stores
/// use this to keep such an accessor in place.
fn member_is_denied_property(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> bool {
    let property = match runtime.object_member(object, name) {
        Variant::Object(handle) => Some(handle),
        Variant::Closure(closure) => Some(closure.object),
        _ => None,
    };
    property.is_some_and(|property| {
        matches!(
            runtime.native_property_access(property),
            Some(access) if !access.allows_set()
        )
    })
}

fn layer_native_property_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    match name {
        // `GetChildrenArrayObjectNoAddRef()` (`LayerIntf.cpp:8527`): the cached
        // array *is* the value's ObjThis, so `layer.children.clear()` runs
        // against the array rather than against the layer.
        "children" => {
            return Ok(sync_children_array(runtime, this)
                .map(|array| self_bound(Variant::Object(array)))
                .unwrap_or(Variant::Void));
        }
        "cursorX" | "cursorY" => return Ok(layer_cursor_position_value(runtime, this, name)),
        // `GetClipLeft`/`GetClipTop`/`GetClipWidth`/`GetClipHeight`
        // (`LayerIntf.h:711-717`) are plain reads of the live `ClipRect`;
        // `ResetClip` makes it the whole image, and a layer without an image
        // reports zeros rather than throwing.
        "clipLeft" | "clipTop" | "clipWidth" | "clipHeight" => {
            let (left, top, width, height) = layer_clip_rect(runtime, this);
            return Ok(Variant::Integer(match name {
                "clipLeft" => left,
                "clipTop" => top,
                "clipWidth" => width,
                _ => height,
            }));
        }
        "hasImage" => {
            return Ok(Variant::Integer(i64::from(layer_has_main_image(
                runtime, this,
            )?)));
        }
        // `GetNodeEnabled()` is computed on every read (`LayerIntf.h:651`) and
        // the official property is read-only (`TJS_DENY_NATIVE_PROP_SETTER`,
        // `LayerIntf.cpp:9334`); KAGEX reads it while drawing to pick between
        // full and halved text alpha, so it has to follow an ancestor's
        // `enabled` immediately.
        "nodeEnabled" => {
            if let Some(layer_id) = runtime.host().native_layer(this) {
                let enabled = runtime.host().layer_tree().node_enabled(layer_id);
                return Ok(Variant::Integer(i64::from(enabled)));
            }
        }
        // `GetNodeVisible()` is `GetParentVisible() && Visible`
        // (`LayerIntf.h:308`), computed on every read like `nodeEnabled`;
        // hiding an ancestor makes every descendant report 0.
        "nodeVisible" => {
            if let Some(layer_id) = runtime.host().native_layer(this) {
                let visible = runtime.host().layer_tree().node_visible(layer_id);
                return Ok(Variant::Integer(i64::from(visible)));
            }
        }
        // `GetNodeFocusable()` (`LayerIntf.h:622`): focusable, visible,
        // enabled, every ancestor visible/enabled and not disabled by mode.
        "nodeFocusable" => {
            return Ok(Variant::Integer(i64::from(layer_is_node_focusable(
                runtime, this,
            ))));
        }
        // `GetFocused()` is `Manager->GetFocusedLayer() == this`
        // (`LayerIntf.cpp:3218`); the window's focused layer is the live state.
        "focused" => {
            let focused = layer_window_object(runtime, this)
                .and_then(|window| focused_layer(runtime, window));
            return Ok(Variant::Integer(i64::from(focused == Some(this))));
        }
        // `GetPrevFocusable()`/`GetNextFocusable()` (`LayerIntf.cpp:3261`,
        // `:3300`): the nearest focus-chain member before/after this layer in
        // the window's overall order, handed out self-bound, with the
        // `onSearch*Focusable` event the reference posts to let a script
        // redirect the search.
        "prevFocusable" | "nextFocusable" => {
            return layer_relative_focusable(runtime, this, name == "nextFocusable");
        }
        // `GetOrderIndex()` (`LayerIntf.cpp:8541`): the live sibling position,
        // not a stored number.
        "order" => {
            return Ok(Variant::Integer(
                runtime.host().native_layer_order_index(this).unwrap_or(0) as i64,
            ));
        }
        // `GetAbsoluteOrderIndex()` (`:8561`): with no parent 0; in absolute
        // order mode the index the script set, otherwise the sibling order.
        "absolute" => {
            return Ok(Variant::Integer(layer_absolute_order_index(runtime, this)));
        }
        // The official getters report the image's pixel buffer address and
        // pitch (`LayerIntf.cpp:9517-9590`). The engine has no addressable
        // bitmap behind those, so they answer `void`; registering them is what
        // gives the read-only policy a member to protect.
        "mainImageBuffer"
        | "mainImageBufferForWrite"
        | "mainImageBufferPitch"
        | "provinceImageBuffer"
        | "provinceImageBufferForWrite"
        | "provinceImageBufferPitch" => {
            return Ok(Variant::Void);
        }
        _ => {}
    }
    if matches!(
        name,
        "imageWidth" | "imageHeight" | "imageLeft" | "imageTop"
    ) {
        let (width, height) = layer_main_image_size(runtime, this)?;
        return Ok(Variant::Integer(match name {
            "imageWidth" => width as i64,
            "imageHeight" => height as i64,
            "imageLeft" => layer_property_i64(runtime, this, "imageLeft", 0)?,
            _ => layer_property_i64(runtime, this, "imageTop", 0)?,
        }));
    }
    let value = layer_property_value(runtime, this, name);
    Ok(if LAYER_SELF_BOUND_PROPERTIES.contains(&name) {
        self_bound(value)
    } else {
        value
    })
}

/// `tTJSNI_BaseLayer`'s `tTJSVariant(dsp, dsp)` members: `parent`
/// (`LayerIntf.cpp:8490`), `children` (`:8527`) and each element it holds
/// (`:665`), `window` (`:8683`), `prevFocusable` (`:9219`), `nextFocusable`
/// (`:9242`) and `font` (`:9444`); `Window`'s `mainWindow` (`:1803`),
/// `focusedLayer` (`:1824`) and `primaryLayer` (`:1865`) use the same form.
/// The same shape comes back from the layer methods that hand out a layer --
/// `getLayerAt` (`:6909`), `focusPrev`/`focusNext` (`:7729`, `:7747`) -- and
/// from the event objects the action dispatcher builds: the target is
/// `tTJSVariant(targthis, targ)` (`EventIntf.cpp:889`) and the event itself is
/// handed to the handler as `tTJSVariant(evobj, evobj)`
/// (`EventIntf.h:201`).
///
/// The value carries its own object as ObjThis, which this engine models as a
/// bound closure over the same object: TJS2 picks a call's or a write's
/// objthis with `Object.ObjThis ? Object.ObjThis : ra[-1]`, so a member read
/// through such a value acts on the object it came from. KAGEX relies on that
/// when `objectHookInjection` replaces `layer.font`'s `face` property and the
/// injected setter has to run against the font rather than against the
/// writer's `this`.
///
/// One visibility rule follows from the same official line: the engine stores
/// these properties' *values* (host storage and the `__nativeLayerProperty$*`
/// keys) as plain objects, and only the script-facing getter binds them.
/// Readers that need the object itself unwrap with
/// [`variant_object_handle`]/[`variant_object`].
fn self_bound(value: Variant) -> Variant {
    match value {
        Variant::Object(handle) => Variant::Closure(Closure::new(handle, Some(handle))),
        value => value,
    }
}

/// The literal `TJS_BEGIN_NATIVE_PROP_DECL(...)` names whose getters hand out
/// `tTJSVariant(dsp, dsp)`.
const LAYER_SELF_BOUND_PROPERTIES: &[&str] = &["parent", "window", "font"];

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
    if name == "children" {
        // `TJS_DENY_NATIVE_PROP_SETTER` (`LayerIntf.cpp:8532`): the array is
        // the tree view and has no setter. A script write never gets this far
        // (`NativePropertyAccess::ReadOnly` refuses it in dispatch, with the
        // official `TJS_E_ACCESSDENYED`); storing one would also corrupt the
        // backing key `ensure_native_layer_attached` reads.
        return Err(TjsError::access_denied());
    }
    let previous_type = (name == "type")
        .then(|| {
            layer_property_value(runtime, this, "type")
                .to_integer()
                .ok()
        })
        .flatten();
    let value = normalize_layer_property_value(name, value)?;
    if name == "parent" {
        // `TVPSpecifyLayer` (`LayerIntf.cpp:451`): the property takes a layer or
        // nothing at all.  Kirakira used to accept any object and then
        // materialize the child as a render root at the coordinates its missing
        // parent used to clip.
        let parent =
            variant_object(&value).map(|parent| runtime.bound_this(parent).unwrap_or(parent));
        if let Some(parent) = parent
            && runtime.host().native_layer(parent).is_none()
        {
            return Err(TjsError::runtime("Specify Layer class object"));
        }
    }
    if name == "hasImage" {
        // `SetHasImage` (`LayerIntf.cpp:2228-2235`): a layer whose type cannot
        // carry an image (`ltBinder`/`ltEffect`/`ltFilter`) refuses one.
        if value.is_truthy()
            && !layer_type_can_have_image(layer_property_i64(runtime, this, "type", 1)?)
        {
            return Err(TjsError::runtime("This layer cannot have image"));
        }
        set_layer_has_image(runtime, this, value.is_truthy())?;
        return Ok(());
    }
    // `SetClipLeft`/`SetClipTop`/`SetClipWidth`/`SetClipHeight`
    // (`LayerIntf.cpp:3734-3752`) re-run `SetClip` with the other three live
    // values, so every one of them clamps and resets the rectangle the same
    // way; the read-only clip members are not stored separately.
    if matches!(name, "clipLeft" | "clipTop" | "clipWidth" | "clipHeight") {
        let (left, top, width, height) = layer_clip_rect(runtime, this);
        let value = value.to_integer()?;
        let (left, top, width, height) = match name {
            "clipLeft" => (value, top, width, height),
            "clipTop" => (left, value, width, height),
            "clipWidth" => (left, top, value, height),
            _ => (left, top, width, value),
        };
        set_layer_clip_rect(runtime, this, left, top, width, height)?;
        return Ok(());
    }
    if name == "imageWidth" {
        set_layer_image_width(runtime, this, value.to_integer()?)?;
        return Ok(());
    }
    if name == "imageHeight" {
        set_layer_image_height(runtime, this, value.to_integer()?)?;
        return Ok(());
    }
    if name == "width" {
        set_layer_geographical_width(runtime, this, value.to_integer()?)?;
        return Ok(());
    }
    if name == "height" {
        set_layer_geographical_height(runtime, this, value.to_integer()?)?;
        return Ok(());
    }
    if name == "order" {
        // `SetOrderIndex` (`LayerIntf.cpp:1218-1231`): the layer must have a
        // parent, the parent drops absolute order mode, and the layer moves to
        // the requested sibling position, clamped to the child count.
        // `Layer.order` reports the live position, so the write has to move the
        // tree rather than only store a number.
        let requested = value.to_integer()?;
        let Some(index) = set_layer_sibling_order(runtime, this, requested) else {
            return Err(cannot_move_primary_or_siblingless());
        };
        set_layer_property_storage(runtime, this, "order", Variant::Integer(index));
        return Ok(());
    }
    if name == "absolute" {
        // `SetAbsoluteOrderIndex` (`:1262-1272`): the layer must have a parent,
        // which switches to absolute order mode; this layer's index is stored
        // verbatim -- KAG picks `absolute` values far above the child count on
        // purpose.
        let index = value.to_integer()?;
        let Some(parent) = layer_parent_object(runtime, this) else {
            return Err(cannot_move_primary_or_siblingless());
        };
        set_layer_absolute_order_mode(runtime, parent, true);
        let index = Variant::Integer(index);
        set_layer_property_storage(runtime, this, "absolute", index.clone());
        return apply_layer_property_to_render(runtime, this, name, &index);
    }
    if name == "absoluteOrderMode" {
        set_layer_absolute_order_mode(runtime, this, value.is_truthy());
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
            // `SetType` (`LayerIntf.cpp:1446-1466`) allocates or frees the
            // main image with the new type and marks the layer as able or
            // unable to carry one.
            set_layer_has_image(runtime, this, layer_type_can_have_image(layer_type))?;
        }
    }
    apply_layer_property_to_render(runtime, this, name, &value)
}

/// `tTJSNI_BaseLayer::CanHaveImage` after `SetType` (`LayerIntf.cpp:1454-1508`):
/// `ltBinder` (0), `ltEffect` (6) and `ltFilter` (7) deallocate the image and
/// refuse a new one; every other type allocates.
fn layer_type_can_have_image(layer_type: i64) -> bool {
    !matches!(layer_type, 0 | 6 | 7)
}

/// `tTVPLayerType` (`visual/drawable.h:20-53`) values this module names.
const LT_BINDER: i64 = 0;
const LT_OPAQUE: i64 = 1;

fn layer_parent_object(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> Option<ObjectHandle> {
    variant_object(&layer_property_value(runtime, layer, "parent"))
        .map(|parent| runtime.bound_this(parent).unwrap_or(parent))
}

/// Official `GetAbsoluteOrderIndex()` (`LayerIntf.cpp:1253-1260`): no parent
/// reports 0, a parent in absolute order mode reports the child's stored
/// absolute index, and anywhere else `absolute` *is* the sibling order.
fn layer_absolute_order_index(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> i64 {
    let Some(parent) = layer_parent_object(runtime, layer) else {
        return 0;
    };
    let live_order = || runtime.host().native_layer_order_index(layer).unwrap_or(0) as i64;
    if !layer_property_value(runtime, parent, "absoluteOrderMode").is_truthy() {
        return live_order();
    }
    match layer_property_value(runtime, layer, "absolute") {
        Variant::Void => live_order(),
        value => value.to_integer().unwrap_or_else(|_| live_order()),
    }
}

/// `TVPCannotMovePrimaryOrSiblingless` (`LayerIntf.cpp:1221`, `:1264`;
/// `IDS_TVP_CANNOT_MOVE_PRIMARY_OR_SIBLINGLESS`, `string_table_en.rc:128`): a
/// primary or otherwise parentless layer has no sibling list to move through.
fn cannot_move_primary_or_siblingless() -> TjsError {
    TjsError::runtime("Cannot move primary or siblingless")
}

/// Moves a layer to `requested` among its siblings and reports the index it
/// landed on, or `None` when it has no parent (the caller reports
/// `TVPCannotMovePrimaryOrSiblingless`).
///
/// Official `ChildChangeOrder` (`LayerIntf.cpp:1120-1167`) rotates the children
/// vector; the render tree orders siblings by `(z_order, id)`, so the new
/// arrangement is written back as dense `z_order` values.
///
/// Every sibling's stored `order` *and* `absolute` follow, because
/// `apply_layer_properties_to_node` reads those keys back into the render node
/// on the next apply and prefers `absolute`. With absolute order mode off the
/// two are the same index (`GetAbsoluteOrderIndex` returns `GetOrderIndex()`,
/// `:1253-1260`), so updating both keeps a layer that was once placed with an
/// absolute index from snapping back to it after an `order` write.
fn set_layer_sibling_order(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    requested: i64,
) -> Option<i64> {
    let parent = layer_parent_object(runtime, layer)?;
    // `SetOrderIndex` leaves absolute order mode (`LayerIntf.cpp:1223`).
    set_layer_absolute_order_mode(runtime, parent, false);
    let index = runtime
        .host_mut()
        .reorder_native_layer(layer, requested)
        .unwrap_or(0);
    for child in runtime.host().native_layer_children(parent) {
        let order =
            Variant::Integer(runtime.host().native_layer_order_index(child).unwrap_or(0) as i64);
        set_layer_property_storage(runtime, child, "order", order.clone());
        set_layer_property_storage(runtime, child, "absolute", order);
    }
    Some(index)
}

/// Official `SetAbsoluteOrderMode` (`LayerIntf.cpp:1274-1294`): entering the
/// mode snapshots every child's current sibling order as its absolute index;
/// leaving it changes nothing else.
fn set_layer_absolute_order_mode(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    enabled: bool,
) {
    let was_enabled = layer_property_value(runtime, layer, "absoluteOrderMode").is_truthy();
    set_layer_property_storage(
        runtime,
        layer,
        "absoluteOrderMode",
        Variant::Integer(i64::from(enabled)),
    );
    if !enabled || was_enabled {
        return;
    }
    for child in runtime.host().native_layer_children(layer) {
        let order = runtime.host().native_layer_order_index(child).unwrap_or(0) as i64;
        set_layer_property_storage(runtime, child, "absolute", Variant::Integer(order));
    }
}

fn layer_property_value(runtime: &Runtime<KrkrHost>, handle: ObjectHandle, name: &str) -> Variant {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    let direct = runtime.object_member(handle, name);
    // A plain member is the live value.  The reference keeps a property's
    // state in the accessor's native instance and skips the accessor for a
    // store that carries `TJS_IGNOREPROP` (`tTJSCustomObject::PropSet`,
    // `tjsObject.cpp:1519-1541`), replacing the member -- KAGEX's
    // `&layer.font = hook`.  The engine's host storage still holds whatever
    // the property setter wrote last, so the member has to win here.
    if !runtime.variant_is_property(&direct) && !matches!(direct, Variant::Void) {
        return direct;
    }
    if let Some(value) = runtime.host().native_layer_property(handle, name) {
        return value;
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
    // The backing key is how the engine reads a property without going through
    // the accessor (`layer_property_value`), so it is always written.
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

    // `Layer.onPaint` is a native no-op placeholder kept on the native class
    // so `SUPER.onPaint(...)` remains callable.  Event delivery itself must
    // give the most-derived script class first refusal: KRKR's
    // TVP_ACTION_INVOKE sends the event to Owner, so a multiple-inheritance
    // extender such as EnvGraphicLayer must run before the base fallback.
    let callback = runtime.object_member(handle, "onPaint");
    let callback_is_native = runtime.variant_is_native_function(&callback);
    let primary_called = if matches!(callback, Variant::Void) || callback_is_native {
        runtime.call_primary_class_method(handle, "onPaint", Vec::new())?
    } else {
        false
    };

    if primary_called {
        Ok(())
    } else {
        runtime
            .call_object_method(handle, "onPaint", Vec::new())
            .map(|_| ())
    }
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
        "hasImage" | "holdAlpha" => Ok(Variant::Integer(i64::from(value.is_truthy()))),
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

/// `tTJSNI_BaseLayer`'s `TVPNotDrawableLayerType` text, shared by the native
/// pixel accessors and the plugin-facing bitmap views
/// (`plugin_api::layer`, Part B §B.3.1 of the design doc): a layer whose image
/// was freed must not be resurrected by a write.
pub(crate) fn not_drawable_layer_type() -> TjsError {
    TjsError::runtime("Not drawable layer type")
}

/// The reference's `TVPNotDrawableLayerType` check: every blit family refuses a
/// destination without a main image instead of allocating one
/// (`LayerIntf.cpp:4111` `PiledCopy`, `:4159` `CopyRect`, `:4245`/`:4253`
/// `StretchCopy`, `:4287`/`:4296` `AffineCopy`, `:4376` `OperateRect`,
/// `:4417` `OperateStretch`, `:4453` `OperateAffine`).
fn require_drawable_layer_image(
    runtime: &Runtime<KrkrHost>,
    target: &LayerRenderTarget,
) -> Result<()> {
    if render_layer_snapshot(runtime, target).is_some_and(|layer| layer.image.is_some()) {
        Ok(())
    } else {
        Err(not_drawable_layer_type())
    }
}

/// `TVPNotDrawableFaceType` (`string_table_en.rc:135`, "Not drawble face
/// type %1"): the requested blit has no method for the layer's draw face.
fn not_drawable_face_type() -> TjsError {
    TjsError::runtime("Not drawable face type")
}

fn cannot_create_empty_layer_image() -> TjsError {
    TjsError::runtime("Cannot create empty layer image")
}

/// `tTJSNI_BaseLayer::MainImage` belongs to the native instance
/// (`LayerIntf.cpp:2018` `AllocateImage`, `:2242` `GetImageWidth`). Kirakira
/// additionally *projects* an instance into the render tree, and a back-page
/// KAG layer is projected onto a shared `kag:<name>` node
/// (`Host::replace_kag_layer_slots`) that starts with no bitmap. Reading the
/// projected node first keeps real drawing visible; falling back to the
/// instance's own node keeps the ctor's `AllocateDefaultImage` visible, so
/// `setSizeToImageSize` does not throw on a normally constructed layer.
fn layer_main_image(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) -> Option<LayerImage> {
    // A dropped tree node must be rebuilt before the read, or a layer the
    // script still owns reports `hasImage = 0` and throws from `imageWidth`.
    ensure_native_layer_attached(runtime, handle);
    let projected = render_layer_target(runtime, handle)
        .ok()
        .flatten()
        .and_then(|target| render_layer_snapshot(runtime, &target))
        .and_then(|layer| layer.image);
    if projected.is_some() {
        return projected;
    }
    let instance_id = runtime.host().native_layer(handle)?;
    runtime
        .host()
        .layer_tree()
        .layer(instance_id)
        .and_then(|layer| layer.image.clone())
}

fn layer_has_main_image(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) -> Result<bool> {
    Ok(layer_main_image(runtime, handle).is_some())
}

fn layer_main_image_size(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
) -> Result<(u32, u32)> {
    let Some(image) = layer_main_image(runtime, handle) else {
        return Err(not_drawable_layer_type());
    };
    Ok((image.upload.width, image.upload.height))
}

fn layer_neutral_fill(runtime: &Runtime<KrkrHost>, handle: ObjectHandle) -> [u8; 4] {
    packed_color_to_rgba(
        layer_property_value(runtime, handle, "neutralColor")
            .to_integer()
            .unwrap_or(0x00ff_ffff),
    )
}

fn filled_layer_pixels(width: u32, height: u32, fill: [u8; 4]) -> Vec<u8> {
    let mut pixels = vec![0; width as usize * height as usize * 4];
    if fill != [0, 0, 0, 0] {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&fill);
        }
    }
    pixels
}

/// A fresh layer image at `width`×`height` filled with `fill`.
///
/// `create_layer_image` still copies the zeroed `Vec` into the image's `Arc`
/// (`Arc::<[u8]>::from` copies), so this does not remove a pass from the
/// allocate paths — the fill just writes the image's own allocation instead of
/// the temporary `Vec`.  Colour 0 skips the fill because the plane the image
/// was built from is already zeroed.
fn create_filled_layer_image(
    runtime: &mut Runtime<KrkrHost>,
    width: u32,
    height: u32,
    fill: [u8; 4],
) -> LayerImage {
    let mut image = runtime.host_mut().create_layer_image(
        width,
        height,
        vec![0; width as usize * height as usize * 4],
    );
    if fill != [0, 0, 0, 0]
        && let Some(pixels) = Arc::get_mut(&mut image.upload.rgba)
    {
        fill_pixel_buffer(pixels, fill);
    }
    image
}

/// Installs a plane the layer now owns: the image, the image size it reports,
/// and the layer Rect only when it never had one.
fn install_layer_plane(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
    image: LayerImage,
    width: u32,
    height: u32,
) {
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
}

fn allocate_default_layer_image(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // `AllocateDefaultImage` copies the 32×32 transparent-white holder. A
    // second `super.Layer(window, parent)` must not wipe a bitmap already
    // attached to this native instance.
    let Some(target) = runtime.host().layer_render_target(handle) else {
        return;
    };
    if render_layer_snapshot(runtime, &target).is_some_and(|layer| layer.image.is_some()) {
        return;
    }
    let image = create_filled_layer_image(
        runtime,
        DEFAULT_LAYER_SIZE,
        DEFAULT_LAYER_SIZE,
        DEFAULT_LAYER_IMAGE_RGBA,
    );
    mutate_render_layer(runtime, &target, |layer| {
        layer.width = DEFAULT_LAYER_SIZE as f32;
        layer.height = DEFAULT_LAYER_SIZE as f32;
        layer.image_left = 0.0;
        layer.image_top = 0.0;
        layer.set_image(image);
    });
}

/// Official `tTJSNI_BaseLayer` keeps a `MainImage` for the object's whole life
/// unless the script frees it (`SetHasImage(false)` → `DeallocateImage`,
/// `LayerIntf.cpp:2228`). Kirakira projects the layer into the render tree, and
/// `Invalidate` / KAG retargeting can drop or recreate that node, taking the
/// bitmap with it. When the script still expects an image, rebuild it at the
/// layer Rect the way `AllocateImage` (`LayerIntf.cpp:2056`) does, so
/// `imageWidth` / `imageHeight` stay readable for scripts that read them
/// unconditionally (GINKA `BaseLayer.freeImage`, `baselayer.tjs:285`, reads
/// `imageWidth` → `fillRect`).
fn restore_default_layer_image(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    if !layer_property_value(runtime, handle, "hasImage").is_truthy() {
        return;
    }
    let Some(target) = runtime.host().layer_render_target(handle) else {
        return;
    };
    let Some(layer) = render_layer_snapshot(runtime, &target) else {
        return;
    };
    if layer.image.is_some() {
        return;
    }
    let width = layer.width.round().max(1.0) as u32;
    let height = layer.height.round().max(1.0) as u32;
    let image =
        create_filled_layer_image(runtime, width, height, layer_neutral_fill(runtime, handle));
    mutate_render_layer(runtime, &target, |layer| {
        layer.image_left = 0.0;
        layer.image_top = 0.0;
        layer.set_image(image);
        // `AllocateImage` ends with `ResetClip()` (`LayerIntf.cpp:2067`), so a
        // rebuilt bitmap starts with the whole-image clip rather than the
        // rectangle a previous image left behind.
        layer.clip = None;
    });
}

fn deallocate_layer_image(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    target: &LayerRenderTarget,
) {
    // `tTJSNI_BaseLayer::DeallocateImage` (`LayerIntf.cpp:2079`) frees the
    // province plane together with the main image. `ClipRect` survives it
    // (`:2079-2085` only deletes the bitmaps), so a `ResetClip` state is
    // materialized at the image size it had: the accessors keep reporting the
    // last rectangle instead of falling back to the (now absent) image.
    mutate_render_layer(runtime, target, |layer| {
        if layer.clip.is_none()
            && let Some(image) = layer.image.as_ref()
        {
            layer.clip = Some(krkr_core::Rect::new(
                0.0,
                0.0,
                image.upload.width as f32,
                image.upload.height as f32,
            ));
        }
        layer.clear_image();
        layer.province = None;
    });
    // Mirror `GetHasImage() == false` (`LayerIntf.cpp:2237`) so a later
    // re-attachment does not resurrect the freed bitmap.
    set_layer_property_storage(runtime, handle, "hasImage", Variant::Integer(0));
    runtime
        .host_mut()
        .clear_layer_image_storage_for_target(target);
    mark_image_modified(runtime, handle);
}

fn allocate_layer_image(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) -> Result<()> {
    let Some(target) = render_layer_target(runtime, handle)? else {
        return Ok(());
    };
    if render_layer_snapshot(runtime, &target).is_some_and(|layer| layer.image.is_some()) {
        return Ok(());
    }
    // `AllocateImage` (`LayerIntf.cpp:2056`) builds a bitmap at Rect, filled
    // with NeutralColor. Empty Rect is illegal for a drawable image.
    let width = layer_property_i64(runtime, handle, "width", DEFAULT_LAYER_SIZE as i64)?.max(0);
    let height = layer_property_i64(runtime, handle, "height", DEFAULT_LAYER_SIZE as i64)?.max(0);
    if width == 0 || height == 0 {
        return Err(cannot_create_empty_layer_image());
    }
    let width = width as u32;
    let height = height as u32;
    let image =
        create_filled_layer_image(runtime, width, height, layer_neutral_fill(runtime, handle));
    mutate_render_layer(runtime, &target, |layer| {
        layer.image_left = 0.0;
        layer.image_top = 0.0;
        layer.set_image(image);
        // `AllocateImage` resets the clip (`LayerIntf.cpp:2056`).
        layer.clip = None;
    });
    set_layer_property_storage(runtime, handle, "imageLeft", Variant::Integer(0));
    set_layer_property_storage(runtime, handle, "imageTop", Variant::Integer(0));
    sync_layer_image_members(runtime, handle, width as i64, height as i64);
    mark_image_modified(runtime, handle);
    Ok(())
}

fn change_layer_image_size(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    width: i64,
    height: i64,
) -> Result<()> {
    if width <= 0 || height <= 0 {
        return Err(cannot_create_empty_layer_image());
    }
    let Some(target) = render_layer_target(runtime, handle)? else {
        return Ok(());
    };
    let existing = render_layer_snapshot(runtime, &target).and_then(|layer| layer.image);
    let fill = layer_neutral_fill(runtime, handle);
    let image = resize_layer_image(runtime, existing, width as u32, height as u32, fill)
        .ok_or_else(cannot_create_empty_layer_image)?;
    mutate_render_layer(runtime, &target, |layer| {
        layer.image_width = width as f32;
        layer.image_height = height as f32;
        layer.image = Some(image);
        // `ChangeImageSize` resizes both planes -- the province plane keeps its
        // overlapping values and zero-fills the rest (`LayerIntf.cpp:2043-2047`).
        if let Some(province) = layer.province.as_ref() {
            layer.province = Some(province.resized(width as u32, height as u32));
        }
        // `ChangeImageSize` ends with `ResetClip()` (`LayerIntf.cpp:2040`).
        layer.clip = None;
    });
    set_layer_property_storage(runtime, handle, "imageWidth", Variant::Integer(width));
    set_layer_property_storage(runtime, handle, "imageHeight", Variant::Integer(height));
    mark_image_modified(runtime, handle);
    Ok(())
}

fn image_layer_size_changed(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) -> Result<()> {
    // `ImageLayerSizeChanged` (`LayerIntf.cpp:2388`): grow the bitmap to cover
    // Rect; never shrink it when the layer shrinks.
    if !layer_has_main_image(runtime, handle)? {
        return Ok(());
    }
    let layer_width = layer_property_i64(runtime, handle, "width", 0)?.max(0);
    let layer_height = layer_property_i64(runtime, handle, "height", 0)?.max(0);
    let (mut image_width, mut image_height) = layer_main_image_size(runtime, handle)?;
    if (image_width as i64) < layer_width {
        change_layer_image_size(runtime, handle, layer_width, image_height as i64)?;
        image_width = layer_width as u32;
    }
    let image_left = layer_property_i64(runtime, handle, "imageLeft", 0)?;
    if image_width as i64 + image_left < layer_width {
        let image_left = layer_width - image_width as i64;
        set_layer_property_storage(runtime, handle, "imageLeft", Variant::Integer(image_left));
        if let Some(target) = render_layer_target(runtime, handle)? {
            mutate_render_layer(runtime, &target, |layer| {
                layer.image_left = image_left as f32;
            });
        }
    }
    if (image_height as i64) < layer_height {
        change_layer_image_size(runtime, handle, image_width as i64, layer_height)?;
        image_height = layer_height as u32;
    }
    let image_top = layer_property_i64(runtime, handle, "imageTop", 0)?;
    if image_height as i64 + image_top < layer_height {
        let image_top = layer_height - image_height as i64;
        set_layer_property_storage(runtime, handle, "imageTop", Variant::Integer(image_top));
        if let Some(target) = render_layer_target(runtime, handle)? {
            mutate_render_layer(runtime, &target, |layer| {
                layer.image_top = image_top as f32;
            });
        }
    }
    Ok(())
}

fn set_layer_geographical_width(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    width: i64,
) -> Result<()> {
    let width = width.max(0);
    if layer_property_i64(runtime, handle, "width", 0)? == width {
        return Ok(());
    }
    set_layer_property_storage(runtime, handle, "width", Variant::Integer(width));
    if let Some(target) = render_layer_target(runtime, handle)? {
        mutate_render_layer(runtime, &target, |layer| {
            layer.width = width as f32;
        });
    }
    image_layer_size_changed(runtime, handle)
}

fn set_layer_geographical_height(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    height: i64,
) -> Result<()> {
    let height = height.max(0);
    if layer_property_i64(runtime, handle, "height", 0)? == height {
        return Ok(());
    }
    set_layer_property_storage(runtime, handle, "height", Variant::Integer(height));
    if let Some(target) = render_layer_target(runtime, handle)? {
        mutate_render_layer(runtime, &target, |layer| {
            layer.height = height as f32;
        });
    }
    image_layer_size_changed(runtime, handle)
}

pub(crate) fn set_layer_geographical_size(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    width: i64,
    height: i64,
) -> Result<()> {
    let width = width.max(0);
    let height = height.max(0);
    let same = layer_property_i64(runtime, handle, "width", 0)? == width
        && layer_property_i64(runtime, handle, "height", 0)? == height;
    if same {
        return Ok(());
    }
    set_layer_property_storage(runtime, handle, "width", Variant::Integer(width));
    set_layer_property_storage(runtime, handle, "height", Variant::Integer(height));
    if let Some(target) = render_layer_target(runtime, handle)? {
        mutate_render_layer(runtime, &target, |layer| {
            layer.width = width as f32;
            layer.height = height as f32;
        });
    }
    image_layer_size_changed(runtime, handle)
}

pub(crate) fn internal_set_layer_image_size(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    width: i64,
    height: i64,
) -> Result<()> {
    // `InternalSetImageSize` (`LayerIntf.cpp:2353`): shrinking the image
    // below Rect also shrinks the layer.
    let layer_width = layer_property_i64(runtime, handle, "width", 0)?;
    let layer_height = layer_property_i64(runtime, handle, "height", 0)?;
    if width < layer_width {
        set_layer_property_storage(runtime, handle, "imageLeft", Variant::Integer(0));
        if let Some(target) = render_layer_target(runtime, handle)? {
            mutate_render_layer(runtime, &target, |layer| {
                layer.image_left = 0.0;
            });
        }
        set_layer_geographical_width(runtime, handle, width)?;
    }
    let layer_width = layer_property_i64(runtime, handle, "width", 0)?;
    let image_left = layer_property_i64(runtime, handle, "imageLeft", 0)?;
    if width + image_left < layer_width {
        let image_left = layer_width - width;
        set_layer_property_storage(runtime, handle, "imageLeft", Variant::Integer(image_left));
        if let Some(target) = render_layer_target(runtime, handle)? {
            mutate_render_layer(runtime, &target, |layer| {
                layer.image_left = image_left as f32;
            });
        }
    }
    if height < layer_height {
        set_layer_property_storage(runtime, handle, "imageTop", Variant::Integer(0));
        if let Some(target) = render_layer_target(runtime, handle)? {
            mutate_render_layer(runtime, &target, |layer| {
                layer.image_top = 0.0;
            });
        }
        set_layer_geographical_height(runtime, handle, height)?;
    }
    let layer_height = layer_property_i64(runtime, handle, "height", 0)?;
    let image_top = layer_property_i64(runtime, handle, "imageTop", 0)?;
    if height + image_top < layer_height {
        let image_top = layer_height - height;
        set_layer_property_storage(runtime, handle, "imageTop", Variant::Integer(image_top));
        if let Some(target) = render_layer_target(runtime, handle)? {
            mutate_render_layer(runtime, &target, |layer| {
                layer.image_top = image_top as f32;
            });
        }
    }
    change_layer_image_size(runtime, handle, width, height)
}

fn set_layer_image_width(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    width: i64,
) -> Result<()> {
    let (current_width, current_height) = layer_main_image_size(runtime, handle)?;
    if width == current_width as i64 {
        return Ok(());
    }
    internal_set_layer_image_size(runtime, handle, width, current_height as i64)
}

fn set_layer_image_height(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    height: i64,
) -> Result<()> {
    let (current_width, current_height) = layer_main_image_size(runtime, handle)?;
    if height == current_height as i64 {
        return Ok(());
    }
    internal_set_layer_image_size(runtime, handle, current_width as i64, height)
}

fn set_layer_has_image(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    has_image: bool,
) -> Result<()> {
    // `tTJSNI_BaseLayer::SetHasImage` → `AllocateImage` / `DeallocateImage`
    // (`LayerIntf.cpp:2228`). The native instance exists for the TJS object's
    // life; restore a dropped tree node (or a stripped native instance) so
    // GINKA `PSDLayer.updateDisp` (`hasImage = 1`) can actually get a dest.
    ensure_native_layer_attached(runtime, handle);
    set_layer_property_storage(
        runtime,
        handle,
        "hasImage",
        Variant::Integer(i64::from(has_image)),
    );
    let Some(target) = render_layer_target(runtime, handle)? else {
        return Ok(());
    };
    if !has_image {
        deallocate_layer_image(runtime, handle, &target);
        return Ok(());
    }
    allocate_layer_image(runtime, handle)
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
        // `tTJSNI_BaseLayer::Join()` parts the old parent first
        // (`LayerIntf.cpp:576`), which is what tells the manager about a layer
        // leaving the tree.  The parent's `children` array follows the tree on
        // its next read (`GetChildrenArrayObjectNoAddRef`,
        // `LayerIntf.cpp:630`), so only the tree edges are touched here.
        notify_part_if_attached(runtime, handle, parent)?;
        runtime
            .host_mut()
            .set_native_layer_parent(handle, parent, value.clone());
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
    "status",
    "looping",
    "volume",
    "volume2",
    "pan",
    "paused",
    "sampleValue",
    "sampleCount",
    "sampleAhead",
    "filters",
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
    if name == "filters" {
        // `tTJSNC_WaveSoundBuffer`'s NI constructor creates the filter array
        // (`WaveIntf.cpp:815`) and the getter (`:1540-1554`) hands the same
        // instance back on every read; `RebuildFilterChain` (`:865-905`) reads
        // it to build the DSP chain.  Lazily created per instance and cached
        // under the instance's backing key, so two buffers never share one.
        let key = wave_property_backing_key(name);
        let existing = runtime.object_member(this, &key);
        if !matches!(existing, Variant::Void) {
            return Ok(existing);
        }
        let array = Variant::Object(runtime.alloc_array_object(Vec::new()));
        runtime.set_object_member(this, &key, array.clone());
        return Ok(array);
    }
    if name == "status" {
        return Ok(runtime.object_member(this, &wave_property_backing_key("status")));
    }
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
            "paused" => Variant::Integer(i64::from(buffer.paused)),
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
    if name == "filters" {
        // The reference registers the getter only — `TJS_DENY_NATIVE_PROP_SETTER`
        // (`WaveIntf.cpp:1540-1554`) — `TJS_DENY_NATIVE_PROP_SETTER`: script
        // writes are refused by the property's access before this runs, and the
        // engine itself never writes the array through TJS dispatch.
        return Ok(());
    }
    if name == "status" {
        // Native status is read-only; transitions update it internally.
        return Ok(());
    }
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
        "paused" => {
            let paused = value.is_truthy();
            runtime.host_mut().set_native_audio_paused(this, paused);
            set_wave_paused(runtime, this, paused);
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

pub(crate) fn set_wave_status(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    status: &str,
) -> bool {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    let current = runtime.object_member(handle, &wave_property_backing_key("status"));
    if matches!(&current, Variant::String(value) if value == status) {
        return false;
    }
    set_wave_property_storage(
        runtime,
        handle,
        "status",
        Variant::String(status.to_string()),
    );
    true
}

pub(crate) fn set_wave_paused(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    paused: bool,
) -> bool {
    let handle = runtime.bound_this(handle).unwrap_or(handle);
    let current = runtime.object_member(handle, &wave_property_backing_key("paused"));
    if current.is_truthy() == paused && !matches!(current, Variant::Void) {
        return false;
    }
    set_wave_property_storage(
        runtime,
        handle,
        "paused",
        Variant::Integer(i64::from(paused)),
    );
    true
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
    register_native_method_preserving_script_with_arg_count(
        runtime,
        handle,
        name,
        NativeArgCount::Any,
        function,
    );
}

/// [`register_native_method_preserving_script`] with the method's declared
/// argument-count contract: a call that breaks it fails with
/// `TJS_E_BADPARAMCOUNT` (-1004) before the handler sees any argument, which is
/// the order the official native declarations validate in (`numparams` first).
fn register_native_method_preserving_script_with_arg_count(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
    arg_count: NativeArgCount,
    function: NativeMethod,
) {
    if matches!(runtime.object_member(handle, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native_with_arg_count(handle, name, arg_count, function);
}

fn install_async_trigger_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    runtime.register_object_native(handle, "trigger", async_trigger_trigger);
    runtime.register_object_native(handle, "cancel", async_trigger_cancel);
    register_native_method_preserving_script(runtime, handle, "onFire", async_trigger_on_fire);
}

fn install_timer_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // Timer subclasses in the stock KAG scripts call `super.finalize(...)`.
    // KRKR's native Timer has a no-op finalizer; leaving the member absent
    // turns that compatibility call into `void is not callable` and aborts
    // layer cleanup halfway through a scene transition.
    register_native_method_preserving_script(runtime, handle, "finalize", native_void);
    register_native_method_preserving_script(runtime, handle, "onTimer", timer_on_timer);
}

fn install_wave_sound_buffer_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // `tTJSNC_WaveSoundBuffer` declares an empty `finalize` with
    // `TJS_DECL_EMPTY_FINALIZE_METHOD` (`WaveIntf.cpp:1017`); the stream's real
    // teardown lives in the native instance. Script wrappers call it on the
    // *class object* -- KAGEX's voice-filter buffer does
    // `global.EnvWaveSoundBuffer.finalize(...)` while its sessions are
    // destroyed (`voiceeffect.tjs`) -- so the member must resolve through the
    // script class's native parent, or the member call aborts the caller.
    runtime.register_object_native(handle, "finalize", native_void);
    // `tTJSNI_BaseWaveSoundBuffer`'s floors: `open` (`WaveIntf.cpp:1036`),
    // `fade` (`:1069`), `setPos` (`:1101`) and `getVisBuffer`
    // (`sound/win32/WaveImpl.cpp:3376`); `play`/`stop`/`stopFade`/
    // `setDefaultCounts`/`setDefaultAheads` carry no arity test.
    runtime.register_object_native_with_arg_count(
        handle,
        "open",
        NativeArgCount::AtLeast(1),
        wave_sound_buffer_open,
    );
    runtime.register_object_native(handle, "play", wave_sound_buffer_play);
    runtime.register_object_native(handle, "stop", wave_sound_buffer_stop);
    runtime.register_object_native_with_arg_count(
        handle,
        "fade",
        NativeArgCount::AtLeast(2),
        wave_sound_buffer_fade,
    );
    runtime.register_object_native(handle, "stopFade", wave_sound_buffer_stop_fade);
    runtime.register_object_native_with_arg_count(
        handle,
        "setPos",
        NativeArgCount::AtLeast(3),
        wave_sound_buffer_set_pos,
    );
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
    runtime.register_object_native_with_arg_count(
        handle,
        "getVisBuffer",
        NativeArgCount::AtLeast(3),
        native_wave_noop,
    );
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
    set_wave_status(runtime, this, "stop");
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
    if runtime
        .host()
        .native_audio_buffer(this)
        .is_some_and(|buffer| buffer.playing)
    {
        // BaseSoundBuffer::Play is idempotent while already playing; do not
        // enqueue a second backend voice or emit a duplicate status event.
        return Ok(Variant::Void);
    }
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
    let status_changed = set_wave_status(runtime, this, "play");
    let paused_changed = set_wave_paused(runtime, this, false);
    if status_changed || paused_changed {
        call_wave_status_changed(runtime, this)?;
    }
    Ok(Variant::Void)
}

fn wave_sound_buffer_stop(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = native_audio_this(runtime, this_obj, "WaveSoundBuffer.stop")?;
    let previous_status = runtime.object_member(this, &wave_property_backing_key("status"));
    let was_playing = runtime
        .host()
        .native_audio_buffer(this)
        .is_some_and(|buffer| buffer.playing);
    if !was_playing
        && matches!(&previous_status, Variant::String(status) if status == "unload" || status == "stop")
    {
        return Ok(Variant::Void);
    }
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
    let status_changed = set_wave_status(runtime, this, "stop");
    let paused_changed = set_wave_paused(runtime, this, false);
    if status_changed || paused_changed {
        call_wave_status_changed(runtime, this)?;
    }
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
    if runtime.host().native_audio_buffer(this).is_none() {
        return Ok(Variant::Void);
    }
    let delay_millis = match args.get(2) {
        Some(Variant::Void) | None => 0,
        Some(value) => value.to_integer()?,
    };
    if delay_millis < 0 {
        return Err(TjsError::runtime(
            "WaveSoundBuffer.fade delay must be non-negative",
        ));
    }
    let (target, millis) = if let Some(options) = args.first().and_then(variant_object) {
        let target = object_member_i64(runtime, options, "volume")?.unwrap_or(0);
        let millis = match object_member_i64(runtime, options, "time")? {
            Some(value) => value,
            None => object_member_i64(runtime, options, "period")?.unwrap_or(0),
        };
        (target, millis)
    } else {
        if args.len() < 2 {
            return Err(TjsError::runtime(
                "WaveSoundBuffer.fade requires target and time",
            ));
        }
        (
            optional_integer(&args, 0)?.unwrap_or(0),
            optional_integer(&args, 1)?.unwrap_or(0),
        )
    };
    if millis <= 0 {
        return Err(TjsError::runtime(
            "WaveSoundBuffer.fade time must be positive",
        ));
    }
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
    runtime
        .host_mut()
        .schedule_audio_fade_completion(this, millis.saturating_add(delay_millis));
    Ok(Variant::Void)
}

fn wave_sound_buffer_set_pos(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = native_audio_this(runtime, this_obj, "WaveSoundBuffer.setPos")?;
    if args.len() < 3 {
        return Err(TjsError::runtime(
            "WaveSoundBuffer.setPos requires x, y and z coordinates",
        ));
    }
    for (name, index) in [("posX", 0), ("posY", 1), ("posZ", 2)] {
        runtime.set_object_member(this, name, Variant::Real(args[index].to_real()?));
    }
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
    let mode = match runtime.resolve_object_member(this, "mode")?.to_integer()? {
        1 => AsyncTriggerMode::Exclusive,
        2 => AsyncTriggerMode::AtIdle,
        _ => AsyncTriggerMode::Normal,
    };
    let cached = runtime.resolve_object_member(this, "cached")?.is_truthy();
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

pub(crate) fn this_render_layer_target(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<(ObjectHandle, Option<LayerRenderTarget>)> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    let this = runtime.bound_this(this).unwrap_or(this);
    ensure_native_layer_attached(runtime, this);
    Ok((this, render_layer_target(runtime, this)?))
}

/// Official `tTJSNI_BaseLayer` is constructed in `Layer.Construct` and lives
/// until `Invalidate` (`LayerIntf.cpp`). GINKA keeps `StandLayer` dests in
/// `_standpoollayer.children` and still calls `hasImage` / `fillRect` /
/// `copyRect` on them after a parent `invalidate` or after the script
/// `invalidate`s an older pool entry. Rebuild the native instance so those
/// drawing calls have a dest bitmap instead of silently no-op'ing.
fn ensure_native_layer_attached(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    if runtime.host().native_layer(handle).is_some() {
        runtime.host_mut().ensure_native_layer_node(handle);
        restore_default_layer_image(runtime, handle);
        return;
    }
    let had_id = matches!(
        runtime.object_member(handle, "__nativeLayerId"),
        Variant::Integer(_)
    );
    let window = variant_object(&runtime.object_member(handle, "__nativeLayerProperty$window"));
    let parent = variant_object(&runtime.object_member(handle, "__nativeLayerProperty$parent"));
    if !had_id && window.is_none() && parent.is_none() {
        return;
    }
    let children = variant_object(&runtime.object_member(handle, "__nativeLayerProperty$children"));
    let is_primary = runtime
        .object_member(handle, "__nativeLayerProperty$isPrimary")
        .is_truthy();
    let snapshot = snapshot_layer_tjs_properties(runtime, handle);
    let layer_id = runtime.host_mut().register_native_layer(
        handle,
        format!("native:{}", handle.0),
        window,
        parent,
        children,
        is_primary,
    );
    runtime.set_object_member(handle, "__nativeLayerId", Variant::Integer(layer_id as i64));
    for (name, value) in snapshot {
        set_layer_property_storage(runtime, handle, &name, value);
    }
    runtime.host_mut().apply_layer_instance_to_render(handle);
    restore_default_layer_image(runtime, handle);
}

fn snapshot_layer_tjs_properties(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
) -> Vec<(String, Variant)> {
    const NAMES: &[&str] = &[
        "left",
        "top",
        "width",
        "height",
        "imageLeft",
        "imageTop",
        "imageWidth",
        "imageHeight",
        "hasImage",
        "visible",
        "opacity",
        "type",
        "face",
        "holdAlpha",
        "neutralColor",
        "name",
        "order",
        "absolute",
        "absoluteOrderMode",
        "enabled",
        "hitType",
        "hitThreshold",
    ];
    NAMES
        .iter()
        .filter_map(|name| {
            let value = runtime.object_member(handle, layer_property_backing_key(name).as_ref());
            (!matches!(value, Variant::Void)).then(|| ((*name).to_string(), value))
        })
        .collect()
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
    let Some(kag) = runtime.global_member("kag").object_handle() else {
        runtime.host_mut().replace_kag_layer_slots(slots);
        return;
    };

    for page in ["fore", "back"] {
        let Some(page_object) = runtime.object_member(kag, page).object_handle() else {
            continue;
        };
        if let Some(base) = runtime.object_member(page_object, "base").object_handle() {
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
    let Some(array) = runtime.object_member(page_object, member).object_handle() else {
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
    let Some(candidate) = value.object_handle() else {
        return;
    };
    let handle = runtime.bound_this(candidate).unwrap_or(candidate);
    let layer = if message_layer {
        format!("message{index}")
    } else {
        index.to_string()
    };
    slots.insert(handle, KagLayerSlot::new(page, &layer));
}

pub(crate) fn render_layer_snapshot(
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

pub(crate) fn mutate_render_layer<R>(
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
    // `LoadImages` resets the clip (`LayerIntf.cpp:2494`).
    layer.clip = None;
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

    // Official `tTJSNI_BaseLayer::LoadImages` (`LayerIntf.cpp:2494`) decodes
    // into `MainImage` and then calls
    // `InternalSetImageSize(MainImage->GetWidth(), MainImage->GetHeight())`,
    // so the layer Rect follows the decoded bitmap. Kirakira's KAG target form
    // loads into a different layer, so only the plain form adjusts `this`.
    let mut loaded_into_this = false;
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
                loaded_into_this = true;
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
    if loaded_into_this {
        internal_set_layer_image_size(runtime, this, size.width as i64, size.height as i64)?;
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
    let (completions, script_image_completions) = runtime.host_mut().take_completed_image_loads();
    for completion in completions {
        apply_completed_image_load(runtime, completion)?;
    }
    // `Layer.loadImages` carries an explicit target continuation, but a script
    // helper (for example the packed quick-menu loader) used to suspend the TJS
    // VM while the decode worker ran.  Script image loads now complete inside
    // their own call, the way the reference's synchronous `TVPLoadGraphic`
    // (`GraphicsLoaderIntf.cpp:1672`) does, so a script-image completion is not
    // expected here; the resume stays as the safety net for a platform path
    // that cannot wait inside the call and parks a frame on one.
    if script_image_completions > 0 && runtime.is_suspended() {
        runtime.resume_suspended()?;
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
    if let Some(z_order) = request.z_order {
        layer.z_order = z_order;
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

/// The shortest clock a reference transition provider runs on.
///
/// Every provider clamps its own `time` option before it constructs its
/// handler -- `if(time < 2) time = 2; // too small time may cause problem` --
/// in the three built-ins (`TransIntf.cpp:530`, `:768`, `:1044`) and in every
/// extrans provider (`wave.cpp:336`, `mosaic.cpp:443`, `turn.cpp:561`,
/// `rotatetrans.cpp:192/291/460`, `ripple.cpp:1591`).  A handler therefore
/// never sees a duration below 2 ms, which is what keeps `HalfTime = time / 2`
/// (`wave.cpp:55`) away from zero.  The engine clamps at the call site so the
/// millisecond clock the kernels read (`TransitionParams::duration_millis`)
/// carries the same floor.
pub(crate) const TRANSITION_MIN_MILLIS: u64 = 2;

/// Transition names a linked plugin shim would answer for, keyed by the plugin
/// (`KrkrPlugin::name`, the name the host records) and the exact names its
/// provider registers through `TVPAddTransHandlerProvider` (`TransIntf.h:112`).
///
/// **Interim projection.**  The real path is
/// [`crate::plugin_api::transition`]: the reference resolves a name only while
/// some registered provider answers to it (`TVPFindTransHandlerProvider`,
/// `TransIntf.cpp:341-359`), and a plugin registers its providers from
/// `V2Link`.  This engine reaches that registry *first* — a registered
/// provider's name resolves to its handler — and this table is the fallback
/// for the two plugin families whose modules are still marker shims:
/// extNagano's twelve (`docs/plugins/transitions.md` §3, the DLL's string
/// table) and GlitchEffect's three (§4.2) degrade to the shim's own
/// `crossfade` while their plugin is linked, which is the closest projection
/// of "official resolves only while a provider is registered".
/// KaichoTrans registers extra `beginTransition` methods too, but neither its
/// source nor a binary on disk names them, so a kaicho-only name stays unknown
/// -- which is what the reference does while no provider has registered it.
///
/// The table goes away once `crates/krkr-plugins`' extnagano/glitch_effect
/// modules register their providers through `plugin_api::transition`: a
/// registered provider is consulted first, so its names never reach this
/// fallback, and their tests then pin the registry instead of the projection.
const PLUGIN_TRANSITION_NAMES: &[(&str, &[&str])] = &[
    (
        "extNagano.dll",
        &[
            "3duniversal",
            "blurfade",
            "book",
            "flutter",
            "honeyturn",
            "imagewipe",
            "morphing",
            "multiripple",
            "rgbfade",
            "scanline",
            "spin",
            "zoomfade",
        ],
    ),
    ("GlitchEffect.dll", &["fadeglitch", "glitch", "loopglitch"]),
];

/// What one transition name resolved to.
pub(crate) enum ResolvedTransition {
    /// A name this build has a kernel for (`krkr_core::TRANSITION_PROVIDER_NAMES`:
    /// the reference's three built-ins and extrans' seven), or the interim
    /// projection of a linked plugin shim (`PLUGIN_TRANSITION_NAMES`).
    Kernel(TransitionMethod),
    /// A provider a plugin registered through
    /// [`crate::plugin_api::transition`] — the reference's
    /// `iTVPTransHandlerProvider`.
    Provider(Arc<dyn TransitionHandlerProvider>),
}

/// The reference's provider lookup for one script- or tag-supplied name
/// (`TVPFindTransHandlerProvider`, `TransIntf.cpp:341-359`).
///
/// The find is exact and case-sensitive over the registered providers, and it
/// throws `TVPCannotFindTransHander` (`:354`) on a miss, before any option is
/// read (`LayerIntf.cpp:6206`).  `"Wave"` is therefore *not* `"wave"`, and an
/// unknown name is a script error rather than a silent crossfade.  The miss is
/// returned as `UnknownTransitionName` and the call sites report it as the
/// message-only `eTJSError` the reference raises (no numeric code, no trace).
///
/// Resolution order: the engine's own kernels first (the built-ins are
/// registered at the reference's first lookup, `TransIntf.cpp:343-349`, and
/// extrans registers at load, so they are always present here — which is also
/// why [`KrkrHost::register_transition_provider`] refuses those names), then a
/// plugin provider registered through `plugin_api::transition`, then the
/// interim linked-shim projection, then the official miss.
pub(crate) fn resolve_transition(
    runtime: &Runtime<KrkrHost>,
    name: &str,
) -> std::result::Result<ResolvedTransition, UnknownTransitionName> {
    let unknown = match TransitionMethod::try_from_name(name) {
        Ok(method) => return Ok(ResolvedTransition::Kernel(method)),
        Err(unknown) => unknown,
    };
    if let Some(provider) = runtime.host().transition_provider(name) {
        return Ok(ResolvedTransition::Provider(provider));
    }
    // A linked plugin shim answers for its own names (`PLUGIN_TRANSITION_NAMES`);
    // an unlinked one registered no provider, so its names stay unknown.
    let answers_for_name = PLUGIN_TRANSITION_NAMES
        .iter()
        .any(|(plugin, names)| names.contains(&name) && plugin_is_linked(runtime, plugin));
    if answers_for_name {
        return Ok(ResolvedTransition::Kernel(TransitionMethod::Crossfade));
    }
    Err(unknown)
}

fn plugin_is_linked(runtime: &Runtime<KrkrHost>, plugin: &str) -> bool {
    runtime
        .host()
        .linked_plugins()
        .any(|linked| linked.eq_ignore_ascii_case(plugin))
}

fn transition_params_from_options(
    runtime: &mut Runtime<KrkrHost>,
    method: TransitionMethod,
    options: Option<ObjectHandle>,
) -> Result<(TransitionParams, Option<ImageUpload>)> {
    let mut params = TransitionParams {
        method,
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

    // `tTVPUniversalTransHandlerProvider::GetTransitionObject`
    // (`TransIntf.cpp:777`): the rule graphic is required and its load failure
    // is reported as `TVPCannotLoadRuleGraphic` (`:784`).
    let rule_image_upload = if params.method == TransitionMethod::Universal {
        let Some(rule) =
            object_optional_string(runtime, options, "rule")?.filter(|rule| !rule.is_empty())
        else {
            return Err(TjsError::runtime("Specify option rule"));
        };
        match runtime.host_mut().load_image_storage(&rule) {
            Ok(image) => Some(image.upload),
            Err(error) if error.kind == TjsErrorKind::ResourcePending => return Err(error),
            Err(_) => {
                return Err(TjsError::runtime(format!(
                    "Cannot load rule graphics {rule}"
                )));
            }
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

/// One transition colour option (`bgcolor`, `bgcolor1`, `bgcolor2`): a
/// `tjs_uint32` **ARGB** value, exactly the option the reference stores in its
/// handler (`tTVPWaveTransHandlerProvider::StartTransition`: `bgcolor1` is read
/// at `wave.cpp:345` and `bgcolor2` at `:348`, both as `(tjs_int)tmp`) and
/// fills with (`TVPFillARGB`, `wave.cpp:219` and `:228`, after
/// `CurBGColor = Blend(BGColor1, BGColor2, BlendRatio)` at `:159`).  The top
/// byte is alpha, so the reference's `0` default is *transparent* black and a
/// vacated region lets the scene beneath show through; games write `0xff000000`
/// for opaque black.  The kernels take the four channels as floats
/// (`krkr-render`'s `color_uniform`).
///
/// The option is read the way TJS converts it: `(tjs_int)` parses a numeric
/// string (`TJSParseNumber`, `tjsLex.cpp:731-758`, so `"0xff0000"` is a number)
/// and yields `0` for anything else -- a name like `"red"` or a `"#rrggbb"`
/// spelling is not a number and becomes transparent black.
fn object_optional_color(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Result<Option<Color>> {
    let value = runtime.object_member(object, name);
    if matches!(value, Variant::Void) {
        return Ok(None);
    }
    Ok(Some(argb_color(value.to_integer()?)))
}

/// Decodes one `tjs_uint32` ARGB value into the render model's `Color`.
///
/// The reference keeps these values as `tjs_uint32` and every kernel reads the
/// same channel layout (`r` in bits 16-23, `g` 8-15, `b` 0-7, `a` 24-31); a
/// negative `i64` from TJS wraps into the same 32 bits a `tjs_uint32` cast
/// keeps.
pub(crate) fn argb_color(value: i64) -> Color {
    let value = value as u32;
    Color::new(
        ((value >> 16) & 0xff) as f32 / 255.0,
        ((value >> 8) & 0xff) as f32 / 255.0,
        (value & 0xff) as f32 / 255.0,
        ((value >> 24) & 0xff) as f32 / 255.0,
    )
}

fn source_object(value: &Variant) -> Option<ObjectHandle> {
    value.object_handle()
}

fn layer_free_image(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    if let Some(target) = runtime.host().layer_render_target(this) {
        deallocate_layer_image(runtime, this, &target);
    } else if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        // `DeallocateImage` keeps `ClipRect` (`LayerIntf.cpp:2079-2085`).
        if layer.clip.is_none()
            && let Some(image) = layer.image.as_ref()
        {
            layer.clip = Some(krkr_core::Rect::new(
                0.0,
                0.0,
                image.upload.width as f32,
                image.upload.height as f32,
            ));
        }
        layer.clear_image();
        layer.province = None;
        runtime.host_mut().clear_layer_image_storage(layer_id);
        mark_image_modified(runtime, this);
    }
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
    if width.is_some() || height.is_some() {
        image_layer_size_changed(runtime, this)?;
    }
    Ok(Variant::Void)
}

fn layer_set_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, _target) = this_render_layer_target(runtime, this_obj)?;
    let width = optional_integer(&args, 0)?.unwrap_or(0).max(0);
    let height = optional_integer(&args, 1)?.unwrap_or(0).max(0);
    set_layer_geographical_size(runtime, this, width, height)?;
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
    let (this, _target) = this_render_layer_target(runtime, this_obj)?;
    let (current_width, current_height) = layer_main_image_size(runtime, this)?;
    let width = optional_integer(&args, 0)?.unwrap_or(0);
    let height = optional_integer(&args, 1)?.unwrap_or(0);
    if width == current_width as i64 && height == current_height as i64 {
        return Ok(Variant::Void);
    }
    internal_set_layer_image_size(runtime, this, width, height)?;
    Ok(Variant::Void)
}

fn resize_layer_image(
    runtime: &mut Runtime<KrkrHost>,
    existing: Option<LayerImage>,
    width: u32,
    height: u32,
    fill: [u8; 4],
) -> Option<LayerImage> {
    if width == 0 || height == 0 {
        return None;
    }

    let Some(existing) = existing else {
        return Some(runtime.host_mut().create_layer_image(
            width,
            height,
            filled_layer_pixels(width, height, fill),
        ));
    };

    if existing.upload.width == width && existing.upload.height == height {
        return Some(existing);
    }

    let mut rgba = filled_layer_pixels(width, height, fill);
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
    let (this, _target) = this_render_layer_target(runtime, this_obj)?;
    let (width, height) = layer_main_image_size(runtime, this)?;
    set_layer_geographical_size(runtime, this, width as i64, height as i64)?;
    Ok(Variant::Void)
}

fn layer_assign_images(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    // The argument must be a Layer (`LayerIntf.cpp:7840-7844`,
    // `TVPSpecifyLayer`).
    let Some(source) = args
        .first()
        .and_then(variant_object)
        .filter(|source| native_layer_id(runtime, *source).ok().flatten().is_some())
    else {
        return Err(TjsError::runtime("Specify Layer class object"));
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
    // The swap moves render state between two live page layers; neither may be
    // handed the other's visibility while it is parted from the tree
    // (`Part()`, `LayerIntf.cpp:589`).
    let this_draws = runtime.host().render_layer_draws(this_layer_id);
    let comp_draws = runtime.host().render_layer_draws(comp_layer_id);
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(this_layer_id) {
        copy_render_state(layer, &comp_layer);
        layer.renderable &= this_draws;
    }
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(comp_layer_id) {
        copy_render_state(layer, &this_layer);
        layer.renderable &= comp_draws;
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

/// The optional `selfupdate` / `callback` members of a `beginTransition`
/// option object (`tTJSNI_BaseLayer::StartTransition`, `LayerIntf.cpp:6209-6234`).
///
/// `selfupdate` hands the update pass to the script (`TransSelfUpdate`), and
/// `callback` replaces the idle hook's tick with a value the script returns
/// (`GetTransTick`, `:6683`).  Both are only read when the member exists and is
/// not void.
fn transition_driver_options(
    runtime: &mut Runtime<KrkrHost>,
    options: Option<ObjectHandle>,
) -> (bool, Option<Variant>) {
    let Some(options) = options else {
        return (false, None);
    };
    let self_update = match runtime.object_member(options, "selfupdate") {
        Variant::Void => false,
        value => value.is_truthy(),
    };
    let tick_callback = match runtime.object_member(options, "callback") {
        Variant::Void => None,
        value => Some(value),
    };
    (self_update, tick_callback)
}

/// The size `tTJSNI_BaseLayer::StartTransition` hands to the transition
/// provider for one layer (`LayerIntf.cpp:6243`): the layer Rect when children
/// are included, the main image's own size otherwise.
fn transition_layer_size(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    with_children: bool,
) -> Result<(i64, i64)> {
    if with_children {
        Ok((
            layer_property_i64(runtime, handle, "width", 0)?,
            layer_property_i64(runtime, handle, "height", 0)?,
        ))
    } else {
        // A layer without a bitmap has no size to compare; the explicit
        // image check reports it (`LayerIntf.cpp:6271`).
        Ok(layer_main_image(runtime, handle)
            .map(|image| (image.upload.width as i64, image.upload.height as i64))
            .unwrap_or((0, 0)))
    }
}

fn layer_begin_transition(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    // The third argument must be a Layer (`LayerIntf.cpp:7798-7809`): the
    // official wrapper throws `TVPSpecifyLayer` for a missing, void or
    // non-Layer value and never falls back to the destination's `comp`.
    let source = args.get(2).and_then(variant_object);
    let Some(source) =
        source.filter(|source| native_layer_id(runtime, *source).ok().flatten().is_some())
    else {
        return Err(TjsError::runtime("Specify Layer class object"));
    };
    // `tTJSNI_BaseLayer::StartTransition` (`LayerIntf.cpp:6188-6196`): a
    // transition already running on this layer, or one whose source is this
    // layer, is a script error rather than an implicit replacement.
    if runtime.host().layer_in_transition(this) {
        return Err(TjsError::runtime("Current transition must be stopping"));
    }
    if runtime.host().layer_transition_source(source) == Some(this) {
        return Err(TjsError::runtime("Transition mutual source"));
    };
    let with_children = args
        .get(1)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(1)
        != 0;
    let name = args
        .first()
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    // `TVPFindTransHandlerProvider` runs as soon as the two guards passed
    // (`LayerIntf.cpp:6206`) -- before the option reads and before the family's
    // size check, which lives inside `pro->StartTransition` (`:6241`).  The
    // lookup is exact and case-sensitive, and a miss is a script error, so a
    // `void` name is the empty name the reference cannot find too.
    let resolved = resolve_transition(runtime, &name)
        .map_err(|unknown| TjsError::runtime(unknown.message()))?;
    // A registered plugin provider owns the transition: its handler composites
    // the two layer bitmaps itself (`plugin_api::transition`), so none of the
    // kernel machinery below runs for it.
    let method = match resolved {
        ResolvedTransition::Kernel(method) => method,
        ResolvedTransition::Provider(provider) => {
            let options = args.get(3).and_then(variant_object);
            begin_provider_transition(runtime, provider, this, source, with_children, options)?;
            return Ok(Variant::Void);
        }
    };
    // The three providers the reference registers (`TVPRegisterDefaultTransHandlerProvider`,
    // `TransIntf.cpp:1196`) and the rules they impose on their options.
    let crossfade_family = matches!(
        method,
        TransitionMethod::Crossfade | TransitionMethod::Universal | TransitionMethod::Scroll
    );
    // `tTVPCrossFadeTransHandlerProvider::StartTransition` (`TransIntf.cpp:508`,
    // inherited by `universal` and `scroll`): both faces must have the same
    // size, reported as the source size against the destination size.
    if crossfade_family {
        let dest_size = transition_layer_size(runtime, this, with_children)?;
        let source_size = transition_layer_size(runtime, source, with_children)?;
        if dest_size != source_size {
            return Err(TjsError::runtime(format!(
                "Transition layer size mismatch {}x{} and {}x{}",
                source_size.0, source_size.1, dest_size.0, dest_size.1
            )));
        }
    }
    let options = args.get(3).and_then(variant_object);
    let time = match options {
        Some(options) => object_optional_integer(runtime, options, "time").transpose()?,
        None => None,
    };
    // Every provider clamps its own `time` option to the reference's 2 ms floor
    // (`TRANSITION_MIN_MILLIS`), not only the crossfade family, so no handler
    // ever runs on a shorter clock.
    let duration = match (crossfade_family, time) {
        // `GetTransitionObject` (`TransIntf.cpp:529` throws `TVPSpecifyOption`
        // for the missing member; the clamp is the next line): the family
        // requires `time`.
        (true, None) => return Err(TjsError::runtime("Specify option time")),
        (_, Some(time)) => time.max(TRANSITION_MIN_MILLIS as i64) as u64,
        // An extrans method without `time` has no clock to run on.  Official
        // fails the provider call there (`wave.cpp:332-333`); the engine's own
        // projection keeps its immediate completion for that case.
        (false, None) => 0,
    };
    let (mut transition_params, rule_image_upload) =
        transition_params_from_options(runtime, method, options)?;
    // The clock the extrans kernels run on (`TransitionParams::duration_millis`):
    // exactly the clamped `time` in whole milliseconds, `0` when the caller
    // supplied none.
    transition_params.duration_millis = duration as f32;
    // `options.selfupdate` / `options.callback` (`LayerIntf.cpp:6209-6234`).
    let (self_update, tick_callback) = transition_driver_options(runtime, options);
    // `StartTransition` (`LayerIntf.cpp:6271`): without children the handler
    // blends the two main images, so both layers must have one.
    if !with_children
        && (!layer_has_main_image(runtime, this)? || !layer_has_main_image(runtime, source)?)
    {
        return Err(TjsError::runtime(
            "Transition source and destination must have image",
        ));
    }
    let dest_layer_id = native_layer_id(runtime, this)?;
    let source_layer_id = native_layer_id(runtime, source)?;
    let mut suppressed_images = BTreeSet::new();
    if let Some(source_layer_id) = source_layer_id {
        suppressed_images.insert(source_layer_id);
    }
    // `Src1` (`tTVPDivisibleData::Src1`, `LayerIntf.cpp:6592`) is the
    // destination's own composite and `Src2` (`:6611`) the source layer's own
    // cached bitmap
    // (`TransSrc->Complete(destrect)`, `:6604`) aligned with the destination's.
    // Neither layer is written while the transition runs: the destination keeps
    // its own image, rect and pixels, and only the stop's `Exchange`/`Swap`
    // moves content between the two objects
    // (`tTJSNI_BaseLayer::InternalStopTransition`, `:6364`).
    //
    // The face is the source's own subtree: a script layer's bitmap is its own
    // node, so the source page needs no extra roots -- KAGEX keeps the incoming
    // page in the source object's own layers and hands that subtree to this
    // transition (`MainWindow.tjs` `beginTransition`).
    let faces = match source_layer_id {
        Some(source) => TransitionFaces::Layers {
            dest: dest_layer_id.unwrap_or(source),
            source,
            extra_roots: Vec::new(),
            with_children,
        },
        None => TransitionFaces::Frozen(TransitionFaceLists::default()),
    };
    let comp = variant_object(&runtime.object_member(this, "comp"))
        .map(|comp| runtime.bound_this(comp).unwrap_or(comp));
    let paired_comp = Some(runtime.bound_this(source).unwrap_or(source)) == comp;
    // The engine's own `TransitionPolicy::Immediate` (debuggers and headless
    // hosts set it) behaves like a zero-duration transition.
    let immediate =
        duration == 0 || runtime.host().transition_policy() == crate::TransitionPolicy::Immediate;
    if immediate {
        if !paired_comp
            && let Some(source_layer_id) = source_layer_id
            && let Some(source_layer) = runtime
                .host_mut()
                .layer_tree_mut()
                .layer_mut(source_layer_id)
        {
            source_layer.renderable = false;
        }
        finish_immediate_transition(runtime, this, Some(source), with_children)?;
    } else {
        // `tTransDrawable::DrawCompleted` (`LayerIntf.cpp:6575`) composites the
        // two faces inside the destination's own draw rectangle, which is what
        // lets unrelated layers keep drawing while this one transitions.
        let dest_rect = dest_layer_id.and_then(|layer_id| {
            runtime
                .host()
                .transition_destination_rect(layer_id, with_children)
        });
        runtime
            .host_mut()
            .begin_native_transition(NativeTransitionStart {
                duration: Duration::from_millis(duration),
                params: transition_params,
                rule_image_upload,
                faces,
                suppressed_live_images: suppressed_images,
                dest_rect,
                self_update,
                tick_callback,
                completion: NativeTransitionCompletion {
                    dest: this,
                    source: Some(source),
                    paired_comp,
                    with_children,
                },
                provider: None,
            });
    }
    Ok(Variant::Void)
}

/// Starts the transition of a name a registered plugin provider answered --
/// the reference's `pro->StartTransition` + `Update(true)` after it
/// (`LayerIntf.cpp:6236-6344`).
///
/// The provider's handler owns the pixels: the engine hands it the two layers'
/// own bitmaps (`Src1`/`Src2`) and replaces the destination layer's image with
/// each pass's output (`process_provider_transition`), so none of the kernel
/// machinery -- `TransitionParams`, the rule graphic, the frozen faces -- is
/// involved.  The guards `StartTransition` applies have already run, so the
/// call only fails the way the reference's provider call does: a factory
/// failure is `TVPTransHandlerError` (`:6246`), and a layer without its own
/// bitmap is `TVPTransitionSourceAndDestinationMustHaveImage` (`:6271-6278`);
/// this channel needs the two images whatever `withchildren` says, because it
/// does not walk child draws the way `tTransDrawable::DrawCompleted` does
/// (`plugin_api::transition`, "What this channel does not model").
///
/// The factory runs through
/// [`TransitionHandlerProvider::start_transition_with`] with a
/// [`TransitionContext`] over this host, so a provider that needs a rule image
/// (`iTVPSimpleImageProvider::LoadImage`) or a script closure can copy both out
/// of the call; the drains after the factory and after the first pass deliver
/// whatever either queued (`TransitionScriptCallback`), on this script thread.
fn begin_provider_transition(
    runtime: &mut Runtime<KrkrHost>,
    provider: Arc<dyn TransitionHandlerProvider>,
    dest: ObjectHandle,
    source: ObjectHandle,
    with_children: bool,
    options: Option<ObjectHandle>,
) -> Result<()> {
    let (Some(dest_layer_id), Some(source_layer_id)) = (
        native_layer_id(runtime, dest)?,
        native_layer_id(runtime, source)?,
    ) else {
        return Err(TjsError::runtime(
            "Transition source and destination must have image",
        ));
    };
    let has_image = |image: &LayerImage| image.upload.width > 0 && image.upload.height > 0;
    let Some(dest_snapshot) = layer_main_image(runtime, dest).filter(has_image) else {
        return Err(TjsError::runtime(
            "Transition source and destination must have image",
        ));
    };
    let Some(source_size) = layer_main_image(runtime, source)
        .filter(has_image)
        .map(|image| (image.upload.width, image.upload.height))
    else {
        return Err(TjsError::runtime(
            "Transition source and destination must have image",
        ));
    };
    let dest_layer_type = runtime
        .host()
        .layer_tree()
        .layer(dest_layer_id)
        .map(|layer| layer.layer_type)
        .unwrap_or(0);
    let request = TransitionRequest {
        options: TransitionOptions::snapshot(runtime, options),
        dest_layer_type,
        dest_size: (dest_snapshot.upload.width, dest_snapshot.upload.height),
        source_size: Some(source_size),
    };
    // The script-closure channel of `plugin_api::transition`: one queue per
    // destination layer, kept in the layer's extension slot so the per-tick
    // drain finds it from the running transition's destination alone.
    let scripts = runtime
        .host_mut()
        .layer_extension_or_insert_with(dest, TransitionScriptCallQueue::default);
    let started = {
        let mut context = TransitionContext::new(runtime.host_mut(), Arc::clone(&scripts));
        provider.start_transition_with(&request, &mut context)
    };
    let handler = started.map_err(|error| {
        // `TVPTransHandlerError` + the detail text `LayerIntf.cpp:6246` passes
        // (`IDS_TVP_TRANS_HANDLER_ERROR`): the script sees the official
        // message, the provider's own reason goes to the host log the way the
        // reference discards the returned `tjs_error`.
        runtime.host_mut().log(&format!(
            "transition provider `{}` failed to start: {error}",
            provider.name()
        ));
        TjsError::runtime(
            "Transition handler error iTVPTransHandlerProvider::StartTransition failed",
        )
    })?;
    // A closure the reference's factory would have called synchronously (it
    // has the script dispatch) runs here, on the script thread, before the
    // first pass.
    drain_transition_script_calls_for(runtime, dest)?;
    let (self_update, tick_callback) = transition_driver_options(runtime, options);
    // Every provider clamps its own `time` option to the reference's 2 ms floor
    // (`TRANSITION_MIN_MILLIS`); an absent `time` leaves no clock to run the
    // handler on, so the call keeps the engine's immediate projection.
    let duration = request
        .options
        .integer("time")
        .map(|time| time.max(TRANSITION_MIN_MILLIS as i64) as u64)
        .unwrap_or(0);
    let comp = variant_object(&runtime.object_member(dest, "comp"))
        .map(|comp| runtime.bound_this(comp).unwrap_or(comp));
    let paired_comp = Some(runtime.bound_this(source).unwrap_or(source)) == comp;
    let immediate =
        duration == 0 || runtime.host().transition_policy() == crate::TransitionPolicy::Immediate;
    if immediate {
        if !paired_comp
            && let Some(source_layer) = runtime
                .host_mut()
                .layer_tree_mut()
                .layer_mut(source_layer_id)
        {
            source_layer.renderable = false;
        }
        return finish_immediate_transition(runtime, dest, Some(source), with_children);
    }
    // `Src2` must not draw over the composite the handler writes into the
    // destination layer's bitmap; the destination itself keeps drawing live,
    // because that bitmap is the composite.
    let mut suppressed_images = BTreeSet::new();
    suppressed_images.insert(source_layer_id);
    runtime
        .host_mut()
        .begin_native_transition(NativeTransitionStart {
            duration: Duration::from_millis(duration),
            // A provider transition runs no kernel, so `frame_transitions`
            // never reports this; the field still carries the clock the
            // kernels would read, exactly the clamped `time`.
            params: TransitionParams {
                method: TransitionMethod::Crossfade,
                duration_millis: duration as f32,
                ..TransitionParams::default()
            },
            rule_image_upload: None,
            faces: TransitionFaces::Frozen(TransitionFaceLists::default()),
            suppressed_live_images: suppressed_images,
            completion: NativeTransitionCompletion {
                dest,
                source: Some(source),
                paired_comp,
                with_children,
            },
            dest_rect: None,
            self_update,
            tick_callback,
            provider: Some(ProviderTransition::new(
                handler,
                request.options,
                dest_snapshot,
                source_layer_id,
            )),
        });
    // `StartTransition` ends with `Update(true)` (`LayerIntf.cpp:6344`): the
    // first pass composes the frame at tick zero, and a callback that pass
    // queued runs right after it.
    runtime.host_mut().process_provider_transition(dest);
    drain_transition_script_calls_for(runtime, dest)?;
    Ok(())
}

fn copy_render_state(dest: &mut LayerNode, source: &LayerNode) {
    dest.copy_render_state_from(source);
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

/// Whether a KAG layer name is the page base (`[backlay]` stages the whole page
/// for it, `MainWindow.tjs:346-355`).
fn is_kag_page_base(name: &str) -> bool {
    name == "base" || name == "background"
}

/// The TJS layer object for a KAG page layer (`kag.<page>.<name>`), when the
/// script has one.
///
/// The KAG projection works on the engine's own layer slots, but KAG's page
/// base is also a script `Layer`: `[trans]` is `kag.fore.base.beginTransition`
/// in KAG itself, so the projection has to own that object's `InTransition`
/// and take its rectangle from the object the script actually sized.
pub(crate) fn kag_layer_object(
    runtime: &Runtime<KrkrHost>,
    page: &str,
    layer: &str,
) -> Option<ObjectHandle> {
    kag_page_layer_handle(runtime, page, layer)
}

fn kag_page_layer_handle(
    runtime: &Runtime<KrkrHost>,
    page: &str,
    layer: &str,
) -> Option<ObjectHandle> {
    let kag = runtime.global_member("kag").object_handle()?;
    let page_object = runtime.object_member(kag, page).object_handle()?;
    if is_kag_page_base(layer) {
        return variant_object(&runtime.object_member(page_object, "base"))
            .map(|handle| runtime.bound_this(handle).unwrap_or(handle));
    }

    let (array_name, index) = if let Some(index) = layer.strip_prefix("message") {
        ("messages", index)
    } else {
        ("layers", layer)
    };
    let array = runtime
        .object_member(page_object, array_name)
        .object_handle()?;
    variant_object(&runtime.object_member(array, index))
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}
fn layer_stop_transition(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    runtime.host_mut().stop_transition_for(this);
    finish_completed_native_transitions(runtime)?;
    Ok(Variant::Void)
}

/// `StretchCopy` / `AffineCopy` / `PiledCopy` use `bmCopy` at full opacity;
/// on a dfOpaque destination `HoldAlpha` switches it to MAIN only
/// (`LayerIntf.cpp:4289`).
fn copy_blt_for_layer(runtime: &Runtime<KrkrHost>, this: ObjectHandle) -> blend::Blt {
    if effective_draw_face(runtime, this) == DF_MAIN && layer_holds_alpha(runtime, this) {
        blend::Blt::CopyColor
    } else {
        blend::Blt::CopyMask
    }
}

/// `tTJSNI_BaseLayer::SetCursorPos` (`LayerIntf.cpp`): converts the layer
/// point to window coordinates and asks the layer tree owner to move the
/// cursor. Kirakira records it as the host cursor position, which is what
/// `cursorX`/`cursorY` and the next input event read.
fn layer_set_cursor_pos(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, _target) = this_render_layer_target(runtime, this_obj)?;
    let x = required_integer(&args, 0, "Layer.setCursorPos x")?;
    let y = required_integer(&args, 1, "Layer.setCursorPos y")?;
    let origin = runtime
        .host()
        .native_layer(this)
        .and_then(|layer_id| runtime.host().layer_tree().absolute_position(layer_id))
        .unwrap_or(krkr_core::Point::new(0.0, 0.0));
    runtime
        .host_mut()
        .set_cursor_position(krkr_core::Point::new(
            origin.x + x as f32,
            origin.y + y as f32,
        ));
    Ok(Variant::Void)
}

/// `tTJSNI_BaseWindow::SetZoom` (`WindowImpl.cpp:1819` → `WindowFormUnit.cpp:681`):
/// reduces the fraction and stores `zoomNumer`/`zoomDenom`, which the window's
/// paint-box sizing and dialogs read.
fn window_set_zoom(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("Window.setZoom requires this"))?;
    let mut numer = optional_integer(&args, 0)?.unwrap_or(1);
    let mut denom = optional_integer(&args, 1)?.unwrap_or(1);
    if denom == 0 {
        return Err(TjsError::runtime("Divide by zero"));
    }
    let divisor = gcd_i64(numer.abs(), denom.abs()).max(1);
    numer /= divisor;
    denom /= divisor;
    set_window_property_storage(runtime, this, "zoomNumer", Variant::Integer(numer));
    set_window_property_storage(runtime, this, "zoomDenom", Variant::Integer(denom));
    // `InternalSetPaintBoxSize` recomputes `width`/`height` from the stored
    // logical size (`WindowFormUnit.cpp:681`).
    let inner_width = window_property_i64(runtime, this, "innerWidth", 0);
    let inner_height = window_property_i64(runtime, this, "innerHeight", 0);
    if inner_width > 0 && inner_height > 0 {
        let (paint_width, paint_height) =
            window_paint_box_size(runtime, this, inner_width, inner_height);
        set_window_property_storage(runtime, this, "width", Variant::Integer(paint_width));
        set_window_property_storage(runtime, this, "height", Variant::Integer(paint_height));
    }
    Ok(Variant::Void)
}

fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

/// `tTJSNI_BaseLayer::GetMainPixel` (`LayerIntf.cpp:2587`) returns
/// `TVPFromActualColor(GetPoint & 0xffffff)` — the 24-bit colour, no alpha.
fn layer_get_main_pixel(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (_this, target) = this_render_layer_target(runtime, this_obj)?;
    let x = required_integer(&args, 0, "Layer.getMainPixel x")?;
    let y = required_integer(&args, 1, "Layer.getMainPixel y")?;
    let Some(target) = target else {
        return Ok(Variant::Void);
    };
    let Some(image) = render_layer_snapshot(runtime, &target).and_then(|layer| layer.image) else {
        return Err(not_drawable_layer_type());
    };
    let (width, height) = (image.upload.width as i64, image.upload.height as i64);
    if x < 0 || y < 0 || x >= width || y >= height {
        return Err(TjsError::runtime("Out of rectangle"));
    }
    let index = ((y as usize * width as usize) + x as usize) * 4;
    let pixel = &image.upload.rgba[index..index + 4];
    Ok(Variant::Integer(
        (i64::from(pixel[0]) << 16) | (i64::from(pixel[1]) << 8) | i64::from(pixel[2]),
    ))
}

/// `tTJSNI_BaseLayer::SetMainPixel` (`LayerIntf.cpp:2593`): writes the 24-bit
/// colour resolved through `TVPToActualColor`, holding the destination alpha.
fn layer_set_main_pixel(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let x = required_integer(&args, 0, "Layer.setMainPixel x")?;
    let y = required_integer(&args, 1, "Layer.setMainPixel y")?;
    let color = required_integer(&args, 2, "Layer.setMainPixel color")?;
    let rgb = packed_color_to_rgba(to_actual_color(color));
    set_layer_pixel(
        runtime,
        this,
        target,
        x,
        y,
        Some([rgb[0], rgb[1], rgb[2]]),
        None,
    )
}

/// `tTJSNI_BaseLayer::GetMaskPixel`: the alpha channel of the main image.
fn layer_get_mask_pixel(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (_this, target) = this_render_layer_target(runtime, this_obj)?;
    let x = required_integer(&args, 0, "Layer.getMaskPixel x")?;
    let y = required_integer(&args, 1, "Layer.getMaskPixel y")?;
    let Some(target) = target else {
        return Ok(Variant::Void);
    };
    let Some(image) = render_layer_snapshot(runtime, &target).and_then(|layer| layer.image) else {
        return Err(not_drawable_layer_type());
    };
    let (width, height) = (image.upload.width as i64, image.upload.height as i64);
    if x < 0 || y < 0 || x >= width || y >= height {
        return Err(TjsError::runtime("Out of rectangle"));
    }
    let index = ((y as usize * width as usize) + x as usize) * 4;
    Ok(Variant::Integer(i64::from(image.upload.rgba[index + 3])))
}

/// `tTJSNI_BaseLayer::SetMaskPixel`: writes the alpha channel only.
fn layer_set_mask_pixel(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let x = required_integer(&args, 0, "Layer.setMaskPixel x")?;
    let y = required_integer(&args, 1, "Layer.setMaskPixel y")?;
    let mask = required_integer(&args, 2, "Layer.setMaskPixel mask")?;
    set_layer_pixel(runtime, this, target, x, y, None, Some((mask & 0xff) as u8))
}

/// `tTJSNI_BaseLayer::LoadProvinceImage` (`LayerIntf.cpp:2561`): loads an
/// 8-bit palettized/grayscale graphic as the province plane. The plane must
/// match the main image's size (`TVPProvinceSizeMismatch`), and the layer
/// must own a main image (`Not drawable layer type`).
fn layer_load_province_image(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let name = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    if name.is_empty() {
        return Err(TjsError::runtime(
            "Layer.loadProvinceImage requires storage",
        ));
    }
    let Some(target) = target else {
        return Err(not_drawable_layer_type());
    };
    let Some((main_width, main_height)) =
        render_layer_snapshot(runtime, &target).and_then(|layer| {
            layer
                .image
                .as_ref()
                .map(|image| (image.upload.width, image.upload.height))
        })
    else {
        return Err(not_drawable_layer_type());
    };
    let bytes = runtime
        .host_mut()
        .read_binary_storage_for_kind(&name, krkr_core::AssetKind::Image)?;
    // A deferred Web asset leaves the current plane untouched (the retry
    // replaces it), but a real decode/size failure deallocates it like
    // `LoadProvinceImage`'s catch block (`LayerIntf.cpp:2578`).
    let province = match decode_province_image(&bytes, &name) {
        Ok(province) => province,
        Err(error) => {
            mutate_render_layer(runtime, &target, |layer| layer.province = None);
            return Err(TjsError::runtime(error));
        }
    };
    if province.width != main_width || province.height != main_height {
        mutate_render_layer(runtime, &target, |layer| layer.province = None);
        return Err(TjsError::runtime(format!(
            "Province image {name} size mismatch"
        )));
    }
    mutate_render_layer(runtime, &target, |layer| layer.province = Some(province));
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

/// `tTJSNI_BaseLayer::GetProvincePixel` (`LayerIntf.cpp:2637`): reads outside
/// the plane and layers without a province plane return 0.
fn layer_get_province_pixel(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (_this, target) = this_render_layer_target(runtime, this_obj)?;
    let x = required_integer(&args, 0, "Layer.getProvincePixel x")?;
    let y = required_integer(&args, 1, "Layer.getProvincePixel y")?;
    let Some(target) = target else {
        return Ok(Variant::Integer(0));
    };
    let value = render_layer_snapshot(runtime, &target)
        .and_then(|layer| layer.province.map(|province| province.pixel(x, y)))
        .unwrap_or(0);
    Ok(Variant::Integer(i64::from(value)))
}

/// `tTJSNI_BaseLayer::SetProvincePixel` (`LayerIntf.cpp:2647`): allocates the
/// plane from the main image (or the layer Rect without one), honours
/// `ClipRect`, and ignores writes outside the plane.
fn layer_set_province_pixel(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let x = required_integer(&args, 0, "Layer.setProvincePixel x")?;
    let y = required_integer(&args, 1, "Layer.setProvincePixel y")?;
    let value = required_integer(&args, 2, "Layer.setProvincePixel value")?;
    let Some(target) = target else {
        return Ok(Variant::Void);
    };
    if let Some((cx0, cy0, cx1, cy1)) = layer_clip_bounds(runtime, &target)
        && (x < cx0 || y < cy0 || x >= cx1 || y >= cy1)
    {
        return Ok(Variant::Void);
    }
    mutate_render_layer(runtime, &target, |layer| {
        ensure_layer_province_plane(layer);
        if let Some(province) = layer.province.as_mut() {
            province.set_pixel(x, y, (value & 0xff) as u8);
        }
    });
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

/// `tTJSNI_BaseLayer::AllocateProvinceImage` (`LayerIntf.cpp:2647-2663`): the
/// plane starts at the main image's size, or the layer Rect when the layer has
/// no bitmap, and is at least one pixel wide/high.
///
/// Shared by `setProvincePixel`, `FillRect`'s `dfProvince` branch and the
/// plugin-facing province view (`plugin_api::layer`), so all three allocate
/// the same plane.
pub(crate) fn allocate_layer_province_plane(layer: &LayerNode) -> ProvinceImage {
    let (width, height) = layer
        .image
        .as_ref()
        .map(|image| (image.upload.width, image.upload.height))
        .unwrap_or((layer.width.max(0.0) as u32, layer.height.max(0.0) as u32));
    let (width, height) = (width.max(1), height.max(1));
    ProvinceImage::new(width, height, vec![0; (width as usize) * (height as usize)])
}

fn ensure_layer_province_plane(layer: &mut LayerNode) {
    if layer.province.is_none() {
        layer.province = Some(allocate_layer_province_plane(layer));
    }
}

/// `tTJSNI_BaseLayer::IndependProvinceImage` (`LayerIntf.cpp:2425`): `copy`
/// defaults to true; kirakira's copy-on-write makes the plane independent on
/// the next write either way.
fn layer_independ_province_image(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let (_this, target) = this_render_layer_target(runtime, this_obj)?;
    let copy = _args.first().map(Variant::is_truthy).unwrap_or(true);
    if let Some(target) = target {
        mutate_render_layer(runtime, &target, |layer| {
            if let Some(province) = layer.province.as_mut() {
                province.make_independent(copy);
            }
        });
    }
    Ok(Variant::Void)
}

/// `tTJSNI_BaseLayer::FillRect`/`ColorRect` `dfProvince`
/// (`LayerIntf.cpp:3883`): the colour's low byte is the province value,
/// opacity is ignored, and filling the whole plane with 0 deallocates it.
fn fill_layer_province(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    value: u8,
) {
    mutate_render_layer(runtime, target, |layer| {
        let (clip_x, clip_y, clip_width, clip_height) = match layer.clip {
            Some(clip) => (
                clip.x.round() as i64,
                clip.y.round() as i64,
                clip.width.round() as i64,
                clip.height.round() as i64,
            ),
            None => (
                0,
                0,
                layer.width.round() as i64,
                layer.height.round() as i64,
            ),
        };
        let x0 = x.max(clip_x);
        let y0 = y.max(clip_y);
        let x1 = x
            .saturating_add(width)
            .min(clip_x.saturating_add(clip_width));
        let y1 = y
            .saturating_add(height)
            .min(clip_y.saturating_add(clip_height));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        if layer.province.is_none() {
            if value == 0 {
                return;
            }
            ensure_layer_province_plane(layer);
        }
        let Some(province) = layer.province.as_mut() else {
            return;
        };
        if value == 0
            && x0 == 0
            && y0 == 0
            && x1 == province.width as i64
            && y1 == province.height as i64
        {
            layer.province = None;
            return;
        }
        province.fill_rect(x0, y0, x1 - x0, y1 - y0, value);
    });
}

/// Shared body of the pixel setters: both honour `ClipRect` and both write
/// into the destination bitmap in place (`SetPointMain` / `SetPointMask`,
/// `LayerBitmapIntf.cpp:186`).
fn set_layer_pixel(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    target: Option<LayerRenderTarget>,
    x: i64,
    y: i64,
    rgb: Option<[u8; 3]>,
    mask: Option<u8>,
) -> Result<Variant> {
    let Some(target) = target else {
        return Ok(Variant::Void);
    };
    // `SetMainPixel`/`SetMaskPixel` throw `TVPNotDrawableLayerType` without a
    // main image (`LayerIntf.cpp:2594-2596`, `:2621`); a freed bitmap must not
    // be resurrected by the write.
    if render_layer_snapshot(runtime, &target).is_none_or(|layer| layer.image.is_none()) {
        return Err(not_drawable_layer_type());
    }
    if let Some((cx0, cy0, cx1, cy1)) = layer_clip_bounds(runtime, &target)
        && (x < cx0 || y < cy0 || x >= cx1 || y >= cy1)
    {
        return Ok(Variant::Void);
    }
    let mut out_of_rectangle = false;
    mutate_layer_pixels(runtime, &target, |pixels, width, height| {
        if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
            out_of_rectangle = true;
            return;
        }
        let index = ((y as usize * width as usize) + x as usize) * 4;
        if let Some(rgb) = rgb {
            pixels[index..index + 3].copy_from_slice(&rgb);
        }
        if let Some(mask) = mask {
            pixels[index + 3] = mask;
        }
    })?;
    if out_of_rectangle {
        return Err(TjsError::runtime("Out of rectangle"));
    }
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

/// `tTJSNI_BaseLayer::SetClip` / `ResetClip` (`LayerIntf.cpp:3709-3732`). Four
/// arguments set a layer-local clip rectangle; no arguments reset it to the
/// layer rectangle. Every blit and fill is clipped to it. Both paths need a
/// main image; a count in between is the reference's `TJS_E_BADPARAMCOUNT`.
fn layer_set_clip(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    if args.is_empty() {
        if !layer_has_main_image(runtime, this)? {
            return Err(not_drawable_layer_type());
        }
        if let Some(target) = target {
            mutate_render_layer(runtime, &target, |layer| layer.clip = None);
        }
        return Ok(Variant::Void);
    }
    if args.len() < 4 {
        return Err(TjsError::bad_param_count());
    }
    let x = optional_integer(&args, 0)?.unwrap_or(0);
    let y = optional_integer(&args, 1)?.unwrap_or(0);
    let width = optional_integer(&args, 2)?.unwrap_or(0);
    let height = optional_integer(&args, 3)?.unwrap_or(0);
    set_layer_clip_rect(runtime, this, x, y, width, height)?;
    Ok(Variant::Void)
}

/// The live `ClipRect` in image coordinates. A node without a stored clip is
/// the `ResetClip` state, where the rectangle covers the whole image
/// (`LayerIntf.cpp:3709-3716`).
fn layer_clip_rect(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) -> (i64, i64, i64, i64) {
    ensure_native_layer_attached(runtime, handle);
    let Ok(Some(target)) = render_layer_target(runtime, handle) else {
        return (0, 0, 0, 0);
    };
    let Some(layer) = render_layer_snapshot(runtime, &target) else {
        return (0, 0, 0, 0);
    };
    match layer.clip {
        Some(clip) => (
            clip.x.round() as i64,
            clip.y.round() as i64,
            clip.width.round() as i64,
            clip.height.round() as i64,
        ),
        None => {
            let (width, height) = layer
                .image
                .as_ref()
                .map(|image| (image.upload.width as i64, image.upload.height as i64))
                .unwrap_or((0, 0));
            (0, 0, width, height)
        }
    }
}

/// `tTJSNI_BaseLayer::SetClip` (`LayerIntf.cpp:3718-3732`): the left/top edges
/// are clamped at zero, the right/bottom edges at the image size, and each
/// edge is forced past its counterpart. The right/bottom bound is computed
/// from the *unclamped* left/top, as the reference does.
fn set_layer_clip_rect(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    left: i64,
    top: i64,
    width: i64,
    height: i64,
) -> Result<()> {
    ensure_native_layer_attached(runtime, handle);
    let Some(target) = render_layer_target(runtime, handle)? else {
        return Err(not_drawable_layer_type());
    };
    let Some((image_width, image_height)) =
        render_layer_snapshot(runtime, &target).and_then(|layer| {
            layer
                .image
                .map(|image| (image.upload.width as i64, image.upload.height as i64))
        })
    else {
        return Err(not_drawable_layer_type());
    };
    let clip_left = left.max(0);
    let clip_top = top.max(0);
    let mut right = left.saturating_add(width).min(image_width);
    let mut bottom = top.saturating_add(height).min(image_height);
    if right < clip_left {
        right = clip_left;
    }
    if bottom < clip_top {
        bottom = clip_top;
    }
    mutate_render_layer(runtime, &target, |layer| {
        layer.clip = Some(krkr_core::Rect::new(
            clip_left as f32,
            clip_top as f32,
            (right - clip_left) as f32,
            (bottom - clip_top) as f32,
        ));
    });
    Ok(())
}

/// The active `ClipRect` in image coordinates, or `None` for `ResetClip`.
fn layer_clip_bounds(
    runtime: &Runtime<KrkrHost>,
    target: &LayerRenderTarget,
) -> Option<(i64, i64, i64, i64)> {
    let clip = render_layer_snapshot(runtime, target)?.clip?;
    Some((
        clip.x.round() as i64,
        clip.y.round() as i64,
        (clip.x + clip.width).round() as i64,
        (clip.y + clip.height).round() as i64,
    ))
}

fn layer_fill_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let face = effective_draw_face(runtime, this);
    let Some((x, y, width, height)) = rect_args(&args)? else {
        return Ok(Variant::Void);
    };
    let color = required_integer(&args, 4, "Layer.fillRect color")?;
    let Some(target) = target else {
        return Ok(Variant::Void);
    };

    // `tTJSNI_BaseLayer::FillRect` (krkrz/src/core/visual/LayerIntf.cpp) writes
    // the 32-bit colour as-is on alpha faces — it does not force opaque RGB.
    // A 24-bit value such as 0x5b2e2e therefore keeps alpha 0 on dfAlpha, which
    // is how KAGEX stand compositing (`PSDLayer.redrawRect` /
    // `AffineSourceBMPBase.redrawImage`) clears a canvas without painting a
    // solid plate over the background.
    // Every face but `dfProvince` needs the destination bitmap
    // (`LayerIntf.cpp:3866`/`:3873`/`:3880`).
    if face != DF_PROVINCE {
        require_drawable_layer_image(runtime, &target)?;
    }
    match face {
        DF_PROVINCE => {
            fill_layer_province(runtime, &target, x, y, width, height, (color & 0xff) as u8);
        }
        DF_MASK => {
            let mask = (color.max(0) & 0xff) as u8;
            blend_layer_pixels(runtime, &target, x, y, width, height, |pixel| {
                pixel[3] = mask;
            })?;
        }
        DF_MAIN => {
            if layer_holds_alpha(runtime, this) {
                // `FillColor(..., 255)`: replace RGB, keep destination alpha.
                // `FillRect`'s dfOpaque branch resolves system colours
                // (`LayerIntf.cpp:3873`).
                let rgb = packed_color_to_rgba(to_actual_color(color));
                let rgb = [rgb[0], rgb[1], rgb[2]];
                blend_layer_pixels(runtime, &target, x, y, width, height, |pixel| {
                    pixel[..3].copy_from_slice(&rgb);
                })?;
            } else {
                fill_layer_pixels(
                    runtime,
                    &target,
                    x,
                    y,
                    width,
                    height,
                    packed_color_to_rgba(color),
                )?;
            }
        }
        // dfAlpha / dfAddAlpha / (dfOpaque && !HoldAlpha): `MainImage->Fill`.
        _ => {
            fill_layer_pixels(
                runtime,
                &target,
                x,
                y,
                width,
                height,
                packed_color_to_rgba(color),
            )?;
        }
    }
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

fn layer_color_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let face = effective_draw_face(runtime, this);
    let Some((x, y, width, height)) = rect_args(&args)? else {
        return Ok(Variant::Void);
    };
    let raw_color = required_integer(&args, 4, "Layer.colorRect color")?;
    // `tTJSNI_BaseLayer::ColorRect` (`LayerIntf.cpp:3922`) resolves the colour
    // through `TVPToActualColor` before `FillColorOnAlpha` / `FillColor`; the
    // mask and province faces use the raw low byte instead.
    let color = to_actual_color(raw_color);
    let opacity = optional_integer(&args, 5)?.unwrap_or(255);
    let Some(target) = target else {
        return Ok(Variant::Void);
    };
    let rgb = packed_color_to_rgba(color);
    let rgb = [rgb[0], rgb[1], rgb[2]];

    // `tTJSNI_BaseLayer::ColorRect` dispatches on the resolved draw face; only
    // the alpha faces ever touch the destination alpha, and a negative opacity
    // erases opacity instead of painting. Every face but `dfProvince` needs the
    // destination bitmap (`LayerIntf.cpp:3936`/`:3949`/`:3962`/`:3969`).
    if face != DF_PROVINCE {
        require_drawable_layer_image(runtime, &target)?;
    }
    match face {
        DF_PROVINCE => {
            fill_layer_province(
                runtime,
                &target,
                x,
                y,
                width,
                height,
                (raw_color & 0xff) as u8,
            );
        }
        DF_MASK => {
            let mask = (raw_color.max(0) & 0xff) as u8;
            blend_layer_pixels(runtime, &target, x, y, width, height, |pixel| {
                pixel[3] = mask;
            })?;
        }
        DF_MAIN => {
            let opacity = opacity.clamp(0, 255);
            if opacity == 0 {
                return Ok(Variant::Void);
            }
            blend_layer_pixels(runtime, &target, x, y, width, height, |pixel| {
                if opacity == 255 {
                    pixel[..3].copy_from_slice(&rgb);
                } else {
                    blend_const_color_keep_alpha(pixel, rgb, opacity);
                }
            })?;
        }
        DF_ADD_ALPHA => {
            // `FillColorOnAddAlpha` → `TVPConstColorAlphaBlend_a`
            // (`LayerIntf.cpp:3948-3958`): a negative opacity is refused on
            // this face.
            if opacity < 0 {
                return Err(TjsError::runtime(
                    "Negative opacity not supported on this face",
                ));
            }
            let opacity = opacity.min(255);
            if opacity == 0 {
                return Ok(Variant::Void);
            }
            blend_layer_pixels(runtime, &target, x, y, width, height, |pixel| {
                let d = u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
                let out = blend::const_alpha_fill_blend_a(d, color as u32, opacity as u32);
                pixel.copy_from_slice(&out.to_le_bytes());
            })?;
        }
        // dfAlpha / dfBoth, and any face value the engine does not model.
        _ => {
            if opacity > 0 {
                let opacity = opacity.min(255);
                blend_layer_pixels(runtime, &target, x, y, width, height, |pixel| {
                    if opacity == 255 {
                        pixel[..3].copy_from_slice(&rgb);
                        pixel[3] = 255;
                    } else {
                        blend_const_color_on_alpha(pixel, rgb, opacity);
                    }
                })?;
            } else {
                let level = (-opacity).clamp(0, 255);
                if level == 0 {
                    return Ok(Variant::Void);
                }
                blend_layer_pixels(runtime, &target, x, y, width, height, |pixel| {
                    if level == 255 {
                        pixel[3] = 0;
                    } else {
                        remove_const_opacity(pixel, level);
                    }
                })?;
            }
        }
    }
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

/// `TVPSpecifyLayerOrBitmap` (`string_table_en.rc:121`, "Specify Layer or
/// Bitmap class object"): the official TJS blit wrappers resolve their source
/// argument themselves and report this when it holds no usable bitmap
/// (`LayerIntf.cpp:7150` `copyRect`, `:7234` `operateRect`, `:7289`
/// `stretchCopy`, `:7343` `operateStretch`, `:7410` `affineCopy`, `:7484`
/// `operateAffine`).
fn specify_layer_or_bitmap() -> TjsError {
    TjsError::runtime("Specify Layer or Bitmap class object")
}

/// `TVPSourceLayerHasNoImage` (`string_table_en.rc:133`, "Source layer has no
/// image"): the blit methods report a NULL source bitmap
/// (`LayerIntf.cpp:4160` `CopyRect`, `:4377` `OperateRect`, `:4246`/`:4418`
/// `StretchCopy`/`OperateStretch`, `:4288`/`:4454` the affine pair). A script
/// reaches it only through `copyRect`: its wrapper alone accepts a
/// province-only source (`:7136-7137`), which the copy then cannot use on a
/// main-image face. The other five wrappers reject that source themselves
/// (`:7234`, `:7289`, `:7343`, `:7410`, `:7484`).
fn source_layer_has_no_image() -> TjsError {
    TjsError::runtime("Source layer has no image")
}

/// The source resolution the official TJS blit wrappers perform *before* the
/// blit method runs: the argument is read as a Layer's main image
/// (`LayerIntf.cpp:7136` `copyRect`, `:7222` `operateRect`, `:7277`
/// `stretchCopy`, `:7331` `operateStretch`, `:7398` `affineCopy`, `:7472`
/// `operateAffine`), `copyRect`'s additionally accepts a Layer that only has
/// its province plane (`:7137`), and then the `tTJSNC_Bitmap` interface is
/// tried (`:7140-7148`). `None` is everything that resolves to nothing -- a
/// void, a non-object, another native class, or a Layer with neither plane --
/// which the wrapper turns into `TVPSpecifyLayerOrBitmap` (`:7150`); that
/// happens before the blit method runs, so the error precedes every check the
/// method itself makes.
///
/// What the wrapper resolves is a Layer's *bitmap object*, and the reference's
/// is not a function of whether the image has been decoded yet: `MainImage`
/// exists for the object's whole life (`AllocateDefaultImage`,
/// `LayerIntf.cpp:404`/`:2111`), a load decodes *into* it
/// (`tTJSNI_BaseLayer::LoadImages`, `:2509-2514`), and only `freeImage`
/// (`DeallocateImage`, `:2079`) clears it. Kirakira instead materializes that
/// bitmap in a render node, and the node can transiently be absent while the
/// layer itself is fine -- `Invalidate`, KAG page retargeting, or an
/// `assignImages` from an image-less source. Resolving through
/// `layer_main_image` keeps the wrapper's question about the *object*: it is
/// the same read `hasImage`/`imageWidth` use, it rebuilds a dropped node and
/// restores the ctor bitmap, and it answers `None` only for a Layer whose
/// bitmap really is gone (freed) or for a non-Layer -- so a still-loading
/// source blits its (not yet filled) bitmap the way the reference does instead
/// of reporting `TVPSpecifyLayerOrBitmap`.
fn blit_source_object(
    runtime: &mut Runtime<KrkrHost>,
    source: Option<&Variant>,
    allow_province: bool,
) -> Option<ObjectHandle> {
    let handle = source.and_then(variant_object)?;
    // The wrapper asks the *object* whether it is a Layer
    // (`NativeInstanceSupport(TJS_NIS_GETINSTANCE, tTJSNC_Layer::ClassID)`,
    // `LayerIntf.cpp:7218`), and in the reference that instance belongs to the
    // TJS object for its whole life: `Invalidate` (`:482`) frees the *image*
    // (`DeallocateImage`, `:2079`) and detaches it from the tree, but never
    // takes the native instance away from the object. Kirakira models
    // invalidation by dropping the instance from the host table and rebuilds
    // it on use (`ensure_native_layer_attached`, the call
    // `this_render_layer_target` already makes for the destination and
    // `set_layer_has_image` for `hasImage = 1`), so a source that a script
    // invalidated and still draws with has to be re-attached before this
    // question is asked. Without it the object looked like "another native
    // class" and the wrapper reported `TVPSpecifyLayerOrBitmap`: PARQUET's
    // option page dies exactly there when a tab switch repaints a widget
    // whose own `tabImage` layer was invalidated with the page it belonged to
    // (compiled `SliderLayer.tjs`, `onPaint` code object, `operateRect` at
    // word offset 1000 with `this.tabImage` as the source), and the exception
    // aborts every later draw of that event.
    // A freed layer (`freeImage` → `hasImage` 0) still resolves to no bitmap
    // and keeps M184's error, and a non-Layer object stays unresolved because
    // `ensure_native_layer_attached` only rebuilds an instance the Layer ctor
    // registered (`__nativeLayerId`/layer property storage).
    ensure_native_layer_attached(runtime, handle);
    let target = render_layer_target(runtime, handle).ok().flatten()?;
    let has_image = layer_main_image(runtime, handle).is_some();
    let has_province = allow_province
        && render_layer_snapshot(runtime, &target).is_some_and(|layer| layer.province.is_some());
    (has_image || has_province).then_some(handle)
}

/// `piledCopy`'s wrapper takes a Layer and nothing else
/// (`LayerIntf.cpp:7095-7108`): a void, a non-object or any other native class
/// is not a Layer here, and the wrapper reports `TVPSpecifyLayer` ("Specify
/// Layer class object", `string_table_en.rc:120`) before the method's bitmap
/// checks (`:4111-4112`) run.
fn piled_copy_source_object(
    runtime: &mut Runtime<KrkrHost>,
    source: Option<&Variant>,
) -> Option<ObjectHandle> {
    let handle = source.and_then(variant_object)?;
    // Same object-owned instance as the other wrappers (`LayerIntf.cpp:7104`):
    // `piledCopy`'s wrapper asks for a Layer and `PiledCopy` itself is what
    // reports a missing bitmap (`:4112` `TVPSourceLayerHasNoImage`), so a
    // layer a script invalidated and still piles has to be re-attached here
    // too, or the wrapper misreports it as a non-Layer object.
    ensure_native_layer_attached(runtime, handle);
    render_layer_target(runtime, handle)
        .ok()
        .flatten()
        .map(|_| handle)
}

fn layer_copy_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    copy_rect_impl(runtime, this_obj, args, LayerCopyKind::Copy)
}

fn layer_operate_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    copy_rect_impl(runtime, this_obj, args, LayerCopyKind::Operate)
}

fn layer_piled_copy(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, dest_target) = this_render_layer_target(runtime, this_obj)?;
    // `PiledCopy` is not face-dispatched in the reference: it requires a main
    // image on both layers and always copies `MAIN|MASK`
    // (`LayerIntf.cpp:4102-4142`), so a `dfProvince` layer composes normally.
    let dx = optional_integer(&args, 0)?.unwrap_or(0);
    let dy = optional_integer(&args, 1)?.unwrap_or(0);
    // The source must be a Layer: its wrapper reports `TVPSpecifyLayer`
    // before this method's bitmap checks (`LayerIntf.cpp:7095-7108`).
    let Some(source_object) = piled_copy_source_object(runtime, args.get(2)) else {
        return Err(TjsError::runtime("Specify Layer class object"));
    };
    let sx = optional_integer(&args, 3)?.unwrap_or(0);
    let sy = optional_integer(&args, 4)?.unwrap_or(0);
    let width = optional_integer(&args, 5)?.unwrap_or(0);
    let height = optional_integer(&args, 6)?.unwrap_or(0);
    let Some(dest_target) = dest_target else {
        return Ok(Variant::Void);
    };
    // `PiledCopy` checks both images before it touches the rectangles
    // (`LayerIntf.cpp:4111-4112`), so an empty source rectangle still reports a
    // missing bitmap.
    // `if(!MainImage) TVPThrowExceptionMessage(TVPNotDrawableLayerType);`
    // (`LayerIntf.cpp:4111`).
    require_drawable_layer_image(runtime, &dest_target)?;
    complete_layer_subtree_before_draw(runtime, source_object, &mut BTreeSet::new())?;
    let Some(_source_target) = render_layer_target(runtime, source_object)? else {
        return Err(TjsError::runtime("Specify Layer class object"));
    };
    // `if(!src->MainImage) TVPThrowExceptionMessage(TVPSourceLayerHasNoImage);`
    // (`LayerIntf.cpp:4112`). Read through `layer_main_image` for the same
    // reason the wrapper does: the reference's surrogate for "has no image" is
    // `freeImage`, not a bitmap the engine has not materialized yet.
    if layer_main_image(runtime, source_object).is_none() {
        return Err(source_layer_has_no_image());
    }
    if width <= 0 || height <= 0 {
        return Ok(Variant::Void);
    }
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
        piled_layer_target_type(runtime, source_object),
        false,
        true,
        &mut visited,
        &mut layers,
    );
    if layers.is_empty() {
        return Ok(Variant::Void);
    }

    // `PiledCopy` clips to `ClipRect` like every other blit
    // (`LayerIntf.cpp:4115`).
    let clip = layer_clip_bounds(runtime, &dest_target);
    let (x0, y0, x1, y1) = clip_rect_to_layer_clip(
        clip,
        dx.max(0) as u32,
        dy.max(0) as u32,
        (dx + width).max(0) as u32,
        (dy + height).max(0) as u32,
    );
    if x1 <= x0 || y1 <= y0 {
        return Ok(Variant::Void);
    }
    let (copy_x, copy_y) = (x0 as i64, y0 as i64);
    let width = (x1 - x0) as i64;
    let height = (y1 - y0) as i64;
    // `ClipDestPointAndSrcRect` moves the clipped destination point *and* trims
    // the source rectangle by the same amount (`LayerIntf.cpp:3762-3779`), so
    // the two stay aligned: the source pixel that lands on `copy_x` is the one
    // the original `dx` would have taken.
    let sx = sx.saturating_add(copy_x - dx);
    let sy = sy.saturating_add(copy_y - dy);

    // `src->Complete(rect)` (like every layer completion) builds the pile in an
    // offscreen bitmap: `CopySelf` puts the source layer's own image in first
    // and the children are then blitted over it with `BltImage`
    // (`LayerIntf.cpp:5164-5364` via `DrawCompleted` `:5920-5934`), which is a
    // composition over *transparency* rather than over the destination. The
    // finished pile is then copied plane-for-plane into this layer
    // (`MainImage->CopyRect(..., TVP_BB_COPY_MAIN|TVP_BB_COPY_MASK)`, `:4121`),
    // so a pile pixel that is transparent clears the destination pixel instead
    // of leaving it be.
    let mut pile = vec![0u8; width as usize * height as usize * 4];
    for layer in &layers {
        composite_piled_layer(
            &mut pile,
            width as u32,
            height as u32,
            layer,
            sx,
            sy,
            width,
            height,
        );
    }
    // The pile lands in the layer's *existing* `MainImage`; a copy whose
    // rectangle reaches past it is clipped by `tTVPBaseBitmap::CopyRect`'s
    // bound check and the pixels outside the rectangle keep their content
    // (`LayerBitmapIntf.cpp:689-745`). The bitmap never grows here: it only
    // changes size through `setImageSize`, `setSize` (`ImageLayerSizeChanged`,
    // `LayerIntf.cpp:2388`) and image loads.
    mutate_layer_pixels(
        runtime,
        &dest_target,
        |pixels, image_width, image_height| {
            let dest_stride = image_width as usize * 4;
            let pile_stride = width as usize * 4;
            for row in 0..height {
                let dest_y = copy_y + row;
                if dest_y < 0 || dest_y >= image_height as i64 {
                    continue;
                }
                let dest_x = copy_x.max(0);
                let span_width = (width - (dest_x - copy_x)).min(image_width as i64 - dest_x);
                if span_width <= 0 {
                    continue;
                }
                let pile_x = (dest_x - copy_x) as usize * 4;
                let dest_start = dest_y as usize * dest_stride + dest_x as usize * 4;
                let pile_start = row as usize * pile_stride + pile_x;
                let span = span_width as usize * 4;
                if dest_start + span > pixels.len() || pile_start + span > pile.len() {
                    continue;
                }
                pixels[dest_start..dest_start + span]
                    .copy_from_slice(&pile[pile_start..pile_start + span]);
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
    stretch_copy_impl(runtime, this_obj, args, false)
}

/// Official `operateStretch` (`LayerIntf.cpp:7315`):
/// `dx, dy, dw, dh, src, sx, sy, sw, sh, mode=omAuto, opa=255, type=0`.
fn layer_operate_stretch(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    stretch_copy_impl(runtime, this_obj, args, true)
}

/// `InternalAffineBlt`'s source rectangle check
/// (`LayerBitmapIntf.cpp:2704-2709`): an empty rectangle is simply not
/// drawable, but one that reaches outside the source bitmap throws
/// `TVPOutOfRectangle`. Both affine blits run this, and so does a stretch
/// whose type takes the affine path.
fn check_affine_source_rect(
    sx: i64,
    sy: i64,
    source_width: i64,
    source_height: i64,
    texture_width: u32,
    texture_height: u32,
) -> Result<()> {
    if sx < 0
        || sy < 0
        || sx.saturating_add(source_width) > i64::from(texture_width)
        || sy.saturating_add(source_height) > i64::from(texture_height)
    {
        return Err(TjsError::runtime("Out of rectangle"));
    }
    Ok(())
}

fn stretch_copy_impl(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
    operate: bool,
) -> Result<Variant> {
    let (this, dest_target) = this_render_layer_target(runtime, this_obj)?;
    // The wrapper resolves the source first and reports `TVPSpecifyLayerOrBitmap`
    // when nothing resolves, so that precedes even the method's face switch
    // (`LayerIntf.cpp:7269-7289` `stretchCopy`, `:7322-7343` `operateStretch`).
    let Some(source_object) = blit_source_object(runtime, args.get(4), false) else {
        return Err(specify_layer_or_bitmap());
    };
    // `StretchCopy`'s `switch(DrawFace)` throws on every face it does not
    // handle (`LayerIntf.cpp:4260-4262`). `OperateStretch` has no face switch:
    // it throws only when `GetBltMethodFromOperationModeAndDrawFace` fails,
    // which is what the blt resolution below reports (`:4411-4415`), so the
    // universal `om*` modes still blend into the main image on `dfProvince`.
    if is_province_face(runtime, this) && !operate {
        return Err(not_drawable_face_type());
    }
    let dx = optional_integer(&args, 0)?.unwrap_or(0);
    let dy = optional_integer(&args, 1)?.unwrap_or(0);
    let dest_width = optional_integer(&args, 2)?.unwrap_or(0);
    let dest_height = optional_integer(&args, 3)?.unwrap_or(0);
    let sx = optional_integer(&args, 5)?.unwrap_or(0);
    let sy = optional_integer(&args, 6)?.unwrap_or(0);
    let source_width = optional_integer(&args, 7)?.unwrap_or(0);
    let source_height = optional_integer(&args, 8)?.unwrap_or(0);
    let Some(dest_target) = dest_target else {
        return Ok(Variant::Void);
    };
    // Official TJS `operateStretch` (`LayerIntf.cpp:7315`): `omAuto` becomes the
    // source layer's `GetOperationModeFromType()`; `stretchCopy` has no mode and
    // always uses the destination face's copy method. The blt lookup and the
    // `MainImage` check both run before `StretchBlt`'s extent checks
    // (`:4245`/`:4417` precede `LayerBitmapIntf.cpp:1824`), so an empty stretch
    // still reports a missing bitmap.
    let blt = if operate {
        let mut mode = optional_integer(&args, 9)?.unwrap_or(OM_AUTO);
        if mode == OM_AUTO {
            let source_type = layer_property_value(runtime, source_object, "type")
                .to_integer()
                .unwrap_or(2);
            mode = operation_mode_from_layer_type(source_type);
        }
        blend::operation_mode_to_blt(mode, effective_draw_face(runtime, this))
            .ok_or_else(|| TjsError::runtime("Not drawable face type"))?
    } else {
        copy_blt_for_layer(runtime, this)
    };
    // `StretchCopy`/`OperateStretch` require the destination bitmap
    // (`LayerIntf.cpp:4245`/`:4417`).
    require_drawable_layer_image(runtime, &dest_target)?;
    // `StretchBlt` only rejects *zero* extents (`LayerBitmapIntf.cpp:1824`); a
    // negative destination extent mirrors through the affine path below.
    if dest_width == 0 || dest_height == 0 || source_width <= 0 || source_height <= 0 {
        return Ok(Variant::Void);
    }
    // Stretch types below `stLinear` are handed to `AffineBlt`, everything
    // else to `TVPResampleImage` (`LayerBitmapIntf.cpp:1857-1875`); the
    // reference's negative-extent and out-of-rectangle behaviour belongs to
    // the affine routine.
    let raw_stretch_type = if operate {
        optional_integer(&args, 11)?.unwrap_or(0)
    } else {
        optional_integer(&args, 9)?.unwrap_or(0)
    };
    let affine_path = raw_stretch_type & 0xffff < 2;
    if !affine_path && (dest_width < 0 || dest_height < 0) {
        // `TVPResampleImage`'s clipping yields a non-positive size and returns.
        return Ok(Variant::Void);
    }
    complete_layer_before_draw(runtime, source_object)?;
    let Some(source_target) = render_layer_target(runtime, source_object)? else {
        return Err(source_layer_has_no_image());
    };
    let Some(source_image) =
        render_layer_snapshot(runtime, &source_target).and_then(|layer| layer.image)
    else {
        return Err(source_layer_has_no_image());
    };
    let source_pixels = source_image.upload.rgba.as_ref().to_vec();
    let source_texture_width = source_image.upload.width;
    let source_texture_height = source_image.upload.height;
    if affine_path {
        // `InternalAffineBlt`'s source rectangle check
        // (`LayerBitmapIntf.cpp:2704-2709`).
        check_affine_source_rect(
            sx,
            sy,
            source_width,
            source_height,
            source_texture_width,
            source_texture_height,
        )?;
    }

    let opacity = if operate {
        optional_integer(&args, 10)?.unwrap_or(255).clamp(0, 255)
    } else {
        255
    };
    let hold_alpha = layer_holds_alpha(runtime, this);
    let clip = layer_clip_bounds(runtime, &dest_target);
    let stretch_type = blend::stretch_type_from_i64(raw_stretch_type);
    // `MainImage->StretchBlt(ClipRect, destrect, src, srcrect, ...)`
    // (`LayerIntf.cpp:4248`/`:4256`) resamples into the layer's *existing*
    // bitmap: `tTVPBaseBitmap::StretchBlt` folds the clip rectangle onto the
    // bitmap and hands it to the resampler (`LayerBitmapIntf.cpp:1849-1875`),
    // and the bitmap only changes size through `setImageSize`, `setSize`
    // (`ImageLayerSizeChanged`, `LayerIntf.cpp:2388`) and image loads. A
    // stretch that reaches past the bitmap is clipped; nothing outside the
    // destination rectangle is touched. A mirrored request (`dw < 0`) is
    // clipped the same way (`TVPIntersectRect` with the swapped rectangle,
    // `:4236-4239`).
    mutate_layer_pixels(
        runtime,
        &dest_target,
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
                blt,
                opacity,
                hold_alpha,
                clip,
                stretch_type,
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
    affine_copy_impl(runtime, this_obj, args, false)
}

/// Official `operateAffine` (`LayerIntf.cpp:7455`):
/// `src, sx, sy, sw, sh, affine, a, b, c, d, tx, ty, mode=omAuto, opa=255, type=0`.
fn layer_operate_affine(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    affine_copy_impl(runtime, this_obj, args, true)
}

fn affine_copy_impl(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
    operate: bool,
) -> Result<Variant> {
    if args.len() < 12 {
        return Err(TjsError::runtime("Layer.affineCopy requires 12 arguments"));
    }
    let (this, dest_target) = this_render_layer_target(runtime, this_obj)?;
    // The wrapper resolves the source first and reports `TVPSpecifyLayerOrBitmap`
    // when nothing resolves, so that precedes even the method's face switch
    // (`LayerIntf.cpp:7390-7410` `affineCopy`, `:7463-7484` `operateAffine`).
    let Some(source_object) = blit_source_object(runtime, args.first(), false) else {
        return Err(specify_layer_or_bitmap());
    };
    // `AffineCopy`'s `switch(DrawFace)` throws on every face it does not handle
    // (`LayerIntf.cpp:4303-4305`); `OperateAffine` has no face switch and
    // throws only when the blt lookup fails (`:4447-4452`), so the universal
    // `om*` modes still blend into the main image on `dfProvince`.
    if is_province_face(runtime, this) && !operate {
        return Err(not_drawable_face_type());
    }
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
    // `affineCopy` takes `type = param[12]`, `clear = param[13]`
    // (`LayerIntf.cpp:7416-7424`); `operateAffine` has no clear parameter at
    // all -- `param[12]` is the mode, `param[13]` the opacity, `param[14]` the
    // stretch type and `param[15]` a deprecated `hda` (`:7490-7507`).
    let clear = !operate && args.get(13).is_some_and(Variant::is_truthy);
    let clear_color = layer_property_value(runtime, this, "neutralColor")
        .to_integer()
        .map(packed_color_to_rgba)
        .unwrap_or([0, 0, 0, 0]);
    let Some(dest_target) = dest_target else {
        return Ok(Variant::Void);
    };
    // `tTVPBaseBitmap::AffineBlt`'s matrix entry point builds the three points
    // as the images of the source rectangle's *corners* — `(-0.5,-0.5)`,
    // `(rp-0.5,-0.5)` and `(-0.5,bp-0.5)` in the source rectangle's own frame
    // (`LayerBitmapIntf.cpp:3494-3513`); `InternalAffineBlt` then reads the
    // source rectangle as `refrect.*.65536 - 32768` (`:2711-2718`). The two
    // forms take the same points, so the matrix form has to subtract the half
    // pixel the point form's callers already do (a decompiled copy of KAG's
    // `AffineSourceBMPBase.drawAffine` from a game's data subtracts 0.5 from
    // each transformed corner, which is a data point for, not the source of,
    // the convention).
    let points = if affine {
        let (a, b, c, d, tx, ty) = (
            values[0], values[1], values[2], values[3], values[4], values[5],
        );
        let corner = |x: f64, y: f64| (a * x + c * y + tx, b * x + d * y + ty);
        [
            corner(-0.5, -0.5),
            corner(source_width as f64 - 0.5, -0.5),
            corner(-0.5, source_height as f64 - 0.5),
        ]
    } else {
        [
            (values[0], values[1]),
            (values[2], values[3]),
            (values[4], values[5]),
        ]
    };
    let blt = if operate {
        let mut mode = optional_integer(&args, 12)?.unwrap_or(OM_AUTO);
        if mode == OM_AUTO {
            let source_type = layer_property_value(runtime, source_object, "type")
                .to_integer()
                .unwrap_or(2);
            mode = operation_mode_from_layer_type(source_type);
        }
        blend::operation_mode_to_blt(mode, effective_draw_face(runtime, this))
            .ok_or_else(|| TjsError::runtime("Not drawable face type"))?
    } else {
        copy_blt_for_layer(runtime, this)
    };
    // `AffineCopy`/`OperateAffine` require the destination bitmap
    // (`LayerIntf.cpp:4287`/`:4453`).
    require_drawable_layer_image(runtime, &dest_target)?;
    complete_layer_before_draw(runtime, source_object)?;
    let Some(source_target) = render_layer_target(runtime, source_object)? else {
        return Err(source_layer_has_no_image());
    };
    let Some(source_image) =
        render_layer_snapshot(runtime, &source_target).and_then(|layer| layer.image)
    else {
        return Err(source_layer_has_no_image());
    };
    let source_pixels = source_image.upload.rgba.as_ref().to_vec();
    let texture_width = source_image.upload.width;
    let texture_height = source_image.upload.height;
    // `InternalAffineBlt` requires the source rectangle to lie inside the
    // source bitmap (`LayerBitmapIntf.cpp:2704-2709`).
    check_affine_source_rect(
        sx,
        sy,
        source_width,
        source_height,
        texture_width,
        texture_height,
    )?;
    let opacity = if operate {
        optional_integer(&args, 13)?.unwrap_or(255).clamp(0, 255)
    } else {
        255
    };
    let hold_alpha = layer_holds_alpha(runtime, this);
    let clip = layer_clip_bounds(runtime, &dest_target);
    let stretch_type = blend::stretch_type_from_i64(if operate {
        optional_integer(&args, 14)?.unwrap_or(0)
    } else {
        optional_integer(&args, 12)?.unwrap_or(0)
    });
    mutate_layer_pixels(runtime, &dest_target, |pixels, dest_width, dest_height| {
        if clear {
            clear_affine_destination(pixels, dest_width, dest_height, points, clear_color);
        }
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
            blt,
            opacity,
            hold_alpha,
            clip,
            stretch_type,
        );
    })?;
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

/// `tTJSNI_BaseLayer::convertType` (`LayerIntf.cpp:7624`) →
/// `ConvertLayerType` (`:1703`): rewrite the stored `MainImage` between the
/// straight-alpha and premultiplied ("additive alpha") pixel representations.
/// `fromtype` names the representation the pixels are in *now* and the layer's
/// own `DrawFace` is the one they are converted *to*, so only
/// `dfAlpha -> dfAddAlpha` (premultiply) and `dfAddAlpha -> dfAlpha`
/// (unpremultiply, "this may loose additive stuff", `:1714`) exist; every other
/// pairing throws `TVPCannotConvertLayerTypeUsingGivenDirection` (`:1721`).
/// The missing-argument case is the declaration's floor
/// (`if(numparams < 1)`, `:7628`), checked before this handler runs.
fn layer_convert_type(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let from_face = args[0].to_integer()?;
    let (this, target) = this_render_layer_target(runtime, this_obj)?;
    let convert: fn(u32) -> u32 = match (effective_draw_face(runtime, this), from_face) {
        (DF_ADD_ALPHA, DF_ALPHA) => blend::alpha_pixel_to_additive_alpha,
        (DF_ALPHA, DF_ADD_ALPHA) => blend::alpha_pixel_to_alpha,
        _ => {
            return Err(TjsError::runtime(
                "Cannot convert layer type using given direction",
            ));
        }
    };
    // `ConvertLayerType` only touches an existing `MainImage` (`:1710`/`:1716`)
    // but flags the layer modified and updates it either way (`:1724-1726`);
    // an image-less layer must not have one fabricated for it.
    let has_image = target.as_ref().is_some_and(|target| {
        render_layer_snapshot(runtime, target).is_some_and(|layer| layer.image.is_some())
    });
    if has_image && let Some(target) = target {
        mutate_layer_pixels(runtime, &target, |pixels, _width, _height| {
            for pixel in pixels.chunks_exact_mut(4) {
                let packed = u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
                pixel.copy_from_slice(&convert(packed).to_le_bytes());
            }
        })?;
    }
    mark_image_modified(runtime, this);
    Ok(Variant::Void)
}

#[derive(Clone, Copy)]
enum LayerCopyKind {
    Copy,
    Operate,
}

const OM_OPAQUE: i64 = 1;
const OM_ALPHA: i64 = 2;
const OM_AUTO: i64 = 128;

/// `tTJSNI_BaseLayer::GetOperationModeFromType` (`LayerIntf.cpp:1404`).
fn operation_mode_from_layer_type(layer_type: i64) -> i64 {
    match layer_type {
        1 => OM_OPAQUE, // ltOpaque
        2 => OM_ALPHA,  // ltAlpha
        12 => 12,       // ltAddAlpha / omAddAlpha
        3..=11 | 13..=28 => layer_type,
        _ => OM_OPAQUE,
    }
}

/// `tTJSNI_BaseLayer::CopyRect` (`LayerIntf.cpp:4144`) picks its copy flags
/// from the destination face and `HoldAlpha`: alpha faces copy MAIN|MASK,
/// dfOpaque copies MAIN only when `HoldAlpha` is set, and dfMask copies MASK.
fn copy_rect_blt(dest_face: i64, hold_alpha: bool) -> blend::Blt {
    match dest_face {
        DF_MASK => blend::Blt::CopyAlpha,
        DF_MAIN if hold_alpha => blend::Blt::CopyColor,
        _ => blend::Blt::CopyMask,
    }
}

/// `tTJSNI_BaseLayer::CopyRect`'s `dfProvince` branch
/// (`LayerIntf.cpp:4179-4195`): copy the source layer's province plane, or
/// zero-fill the destination plane when the source has none.
///
/// The reference hands `ClipDestPointAndSrcRect`'s rectangle to
/// `ProvinceImage->Fill` / `ProvinceImage->CopyRect`; the zero-fill therefore
/// lands on the clipped *source* rectangle in the destination plane, which is
/// what the port reproduces.
fn copy_province_rect(
    runtime: &mut Runtime<KrkrHost>,
    dest_target: &LayerRenderTarget,
    source_province: Option<ProvinceImage>,
    dx: i64,
    dy: i64,
    sx: i64,
    sy: i64,
    width: i64,
    height: i64,
) {
    // `ClipDestPointAndSrcRect` (`LayerIntf.cpp:3754-3789`): trim the source
    // rectangle to the destination's `ClipRect`, moving the destination point
    // with it (the right/bottom edges are computed from the *untrimmed*
    // origin, exactly as the reference does). A layer without a stored clip is
    // the `ResetClip` state, whose rectangle is the whole image; a layer that
    // never had an image keeps `ClipRect = (0,0,0,0)` (`LayerIntf.cpp:379-382`),
    // which cancels the copy.
    let clip = layer_clip_bounds(runtime, dest_target)
        .or_else(|| {
            render_layer_snapshot(runtime, dest_target).and_then(|layer| {
                layer
                    .image
                    .map(|image| (0, 0, image.upload.width as i64, image.upload.height as i64))
            })
        })
        .or(Some((0, 0, 0, 0)));
    let (mut dx, mut dy) = (dx, dy);
    let (mut left, mut top) = (sx, sy);
    let (mut right, mut bottom) = (sx.saturating_add(width), sy.saturating_add(height));
    if let Some((cx0, cy0, cx1, cy1)) = clip {
        let right_limit = dx.saturating_add(right - left);
        let bottom_limit = dy.saturating_add(bottom - top);
        if dx < cx0 {
            left += cx0 - dx;
            dx = cx0;
        }
        if right_limit > cx1 {
            right -= right_limit - cx1;
        }
        if right <= left {
            return;
        }
        if dy < cy0 {
            top += cy0 - dy;
            dy = cy0;
        }
        if bottom_limit > cy1 {
            bottom -= bottom_limit - cy1;
        }
        if bottom <= top {
            return;
        }
    }
    let (width, height) = (right - left, bottom - top);
    mutate_render_layer(runtime, dest_target, |layer| {
        let Some(source) = source_province else {
            if let Some(province) = layer.province.as_mut() {
                province.fill_rect(left, top, width, height, 0);
            }
            return;
        };
        if layer.province.is_none() {
            // `AllocateProvinceImage` sizes the plane from the main image (or
            // the layer Rect without one).
            let (plane_width, plane_height) = layer
                .image
                .as_ref()
                .map(|image| (image.upload.width, image.upload.height))
                .unwrap_or((layer.width.max(0.0) as u32, layer.height.max(0.0) as u32));
            let (plane_width, plane_height) = (plane_width.max(1), plane_height.max(1));
            layer.province = Some(ProvinceImage::new(
                plane_width,
                plane_height,
                vec![0; (plane_width as usize) * (plane_height as usize)],
            ));
        }
        let Some(province) = layer.province.as_mut() else {
            return;
        };
        for row in 0..height {
            for column in 0..width {
                let source_x = left + column;
                let source_y = top + row;
                if source_x < 0
                    || source_y < 0
                    || source_x >= source.width as i64
                    || source_y >= source.height as i64
                {
                    continue;
                }
                province.set_pixel(dx + column, dy + row, source.pixel(source_x, source_y));
            }
        }
    });
}

fn copy_rect_impl(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
    kind: LayerCopyKind,
) -> Result<Variant> {
    let (this, dest_target) = this_render_layer_target(runtime, this_obj)?;
    let dx = optional_integer(&args, 0)?.unwrap_or(0);
    let dy = optional_integer(&args, 1)?.unwrap_or(0);
    // Both wrappers resolve the source before the copy runs
    // (`LayerIntf.cpp:7127-7150` `copyRect`, `:7213-7234` `operateRect`);
    // `copyRect`'s also accepts a Layer that only has its province plane,
    // which the copy then uses on a `dfProvince` destination (`:4179-4195`).
    let Some(source_object) =
        blit_source_object(runtime, args.get(2), matches!(kind, LayerCopyKind::Copy))
    else {
        return Err(specify_layer_or_bitmap());
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
        return Err(source_layer_has_no_image());
    };

    // `CopyRect`'s dfProvince branch copies the source's province plane (or
    // zero-fills the destination plane when the source has none,
    // `LayerIntf.cpp:4179-4195`). The copy flavours differ on that face:
    // `copyRect` uses it, while `stretchCopy` (`:4260`), `affineCopy`
    // (`:4303`) and `piledCopy` (no face dispatch, `:4102-4142`) never reach
    // it, and `operateRect` (`:4357-4393`) has no face switch at all — it
    // throws only when `GetBltMethodFromOperationModeAndDrawFace` fails, so
    // the universal `om*` modes blend into the main image.
    if is_province_face(runtime, this) && matches!(kind, LayerCopyKind::Copy) {
        copy_province_rect(
            runtime,
            &dest_target,
            render_layer_snapshot(runtime, &source_target).and_then(|layer| layer.province),
            dx,
            dy,
            sx,
            sy,
            width,
            height,
        );
        mark_image_modified(runtime, this);
        return Ok(Variant::Void);
    }

    // Official TJS `operateRect` (`LayerIntf.cpp:7207`): `omAuto` becomes
    // the source layer's `GetOperationModeFromType()`. `copyRect` does not
    // take a mode; its flags come from the destination face and `HoldAlpha`.
    let dest_face = effective_draw_face(runtime, this);
    let hold_alpha = layer_holds_alpha(runtime, this);
    let blt = match kind {
        LayerCopyKind::Copy => copy_rect_blt(dest_face, hold_alpha),
        LayerCopyKind::Operate => {
            let mut mode = optional_integer(&args, 7)?.unwrap_or(OM_AUTO);
            if mode == OM_AUTO {
                let source_type = layer_property_value(runtime, source_object, "type")
                    .to_integer()
                    .unwrap_or(2);
                mode = operation_mode_from_layer_type(source_type);
            }
            blend::operation_mode_to_blt(mode, dest_face)
                .ok_or_else(|| TjsError::runtime("Not drawable face type"))?
        }
    };
    // `CopyRect` and `OperateRect` both require the destination bitmap
    // (`LayerIntf.cpp:4159`/`:4376`); the face dispatch above runs first.
    require_drawable_layer_image(runtime, &dest_target)?;

    // `copyRect`'s wrapper accepted a province-only source; on a face other
    // than `dfProvince` the method itself still has no source bitmap
    // (`LayerIntf.cpp:4160`).
    let Some(source_image) =
        render_layer_snapshot(runtime, &source_target).and_then(|layer| layer.image)
    else {
        return Err(source_layer_has_no_image());
    };
    let source_pixels = source_image.upload.rgba.as_ref().to_vec();
    let source_width = source_image.upload.width;
    let source_height = source_image.upload.height;

    let opacity = match kind {
        LayerCopyKind::Operate => optional_integer(&args, 8)?.unwrap_or(255).clamp(0, 255),
        LayerCopyKind::Copy => 255,
    };
    let clip = layer_clip_bounds(runtime, &dest_target);

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
                blt,
                opacity,
                hold_alpha,
                clip,
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
    if !draw_into_layer_image(runtime, &target, |host, pixels, width, height| {
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
    }) {
        return Err(not_drawable_layer_type());
    }
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
    if !draw_into_layer_image(runtime, &target, |host, pixels, width, height| {
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
    }) {
        return Err(not_drawable_layer_type());
    }
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
    layer_update_by_script(runtime, this)?;
    Ok(Variant::Void)
}

/// `tTJSNI_BaseLayer::UpdateByScript` (`LayerIntf.cpp:7638`), the body the
/// `Layer.update` native and `plugin_api::layer::layer_update` share.
///
/// It runs the layer's completion pass, which is what drives a `selfupdate`
/// transition (`BeforeCompletion`, `LayerIntf.cpp:5056`).  The engine's pass is
/// the frame itself, so the script-driven step is applied to the phase here.
/// `UpdateTransDestinationOnSelfUpdate` (`:4828`) drives the *destination*
/// when the source layer is the one being updated, so both ends are checked.
///
/// The commit path of the plugin-facing bitmap views deliberately does *not*
/// call this: the family contract is "mutate, then `Layer.update()`", and the
/// plugin decides when the repaint is due.
///
/// A self-updated provider pass runs here rather than from the host clock, so
/// the script calls it queued (`plugin_api::transition`'s
/// `TransitionScriptCallback`) are delivered right after the pass.
pub(crate) fn layer_update_by_script(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
) -> Result<()> {
    let mut destinations = Vec::new();
    if runtime.host().transition_self_update(this) {
        destinations.push(this);
    } else if let Some(dest) = runtime.host().layer_transition_destination(this)
        && runtime.host().transition_self_update(dest)
    {
        destinations.push(dest);
    }
    for dest in destinations {
        // A non-self-updating transition already reads its tick once per frame
        // in `refresh_transition_ticks`; only the self-updating ones are the
        // script's to move (`BeforeCompletion` calls `GetTransTick` under
        // `TransSelfUpdate` alone, `LayerIntf.cpp:5056`).
        if let Some(callback) = runtime.host().transition_tick_callback(dest) {
            let tick = runtime.call_function(callback, Vec::new())?.to_integer()?;
            runtime.host_mut().set_transition_tick(dest, tick);
        } else {
            runtime.host_mut().advance_self_updated_transition(dest);
        }
        drain_transition_script_calls_for(runtime, dest)?;
    }
    set_layer_property_storage(runtime, this, "callOnPaint", Variant::Integer(1));
    if !runtime.host_mut().request_layer_paint(this) {
        set_layer_property_storage(runtime, this, "callOnPaint", Variant::Integer(0));
    }
    Ok(())
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
        // `*result = tTJSVariant(lay->GetOwnerNoAddRef(),
        // lay->GetOwnerNoAddRef())` (`LayerIntf.cpp:7729` for `focusPrev`,
        // `:7747` for `focusNext`).
        Ok(self_bound(Variant::Object(target)))
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
    // `FireBeforeFocus` hands the candidate layer and the previously focused
    // one out as `tTJSVariant(Owner, Owner)` (`LayerIntf.cpp:3385-3401`).
    let target = layer_focus_work(
        runtime,
        target,
        "onBeforeFocus",
        Some(target),
        vec![
            previous.map(Variant::self_bound).unwrap_or(Variant::Null),
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
        // `Layer.focused` has no backing store any more: it reports
        // `Manager->GetFocusedLayer() == this` (`LayerIntf.cpp:3218`) from the
        // window's focused layer, which the write below moves.
        if !matches!(runtime.object_member(previous, "onBlur"), Variant::Void) {
            // `org->FireBlur(layer)`: the layer losing focus is told about the
            // new one as `tTJSVariant(Owner, Owner)` (`:3353-3365`, fired from
            // `tTVPLayerManager::SetFocusTo`, `LayerManager.cpp:786`).
            runtime.call_object_method(
                previous,
                "onBlur",
                vec![self_bound(Variant::Object(target))],
            )?;
        }
    }

    set_window_property_storage(runtime, window, "focusedLayer", Variant::Object(target));
    if !matches!(runtime.object_member(target, "onFocus"), Variant::Void) {
        // `FocusedLayer->FireFocus(org, direction)` hands the previously
        // focused layer out the same way (`:3369-3381`).
        runtime.call_object_method(
            target,
            "onFocus",
            vec![
                previous.map(Variant::self_bound).unwrap_or(Variant::Null),
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
    // `FocusWork = this` (`LayerIntf.cpp:3387`): the stored value and the
    // handler's first argument are the candidate as `tTJSVariant(dsp, dsp)`.
    let candidate_value = candidate.map(Variant::self_bound).unwrap_or(Variant::Null);
    runtime.set_object_member(layer, "__nativeFocusWork", candidate_value.clone());
    let mut args = vec![candidate_value];
    args.append(&mut extra_args);
    if !matches!(runtime.object_member(layer, method), Variant::Void) {
        runtime.call_object_method(layer, method, args)?;
    }
    // A handler override stores its answer through `SetFocusWork`
    // (`LayerIntf.h:608-609`), which a script calls with a layer value that is
    // self-bound: read the object back out of the binding.
    Ok(runtime
        .object_member(layer, "__nativeFocusWork")
        .object_handle()
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle)))
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
        .or_else(|| plain_member_object(runtime, window, "focusedLayer"))
}

/// The object a *data* member holds, for the engine's own readers.
///
/// Two shapes must not be mistaken for a value: a native property accessor
/// (its value lives behind the getter -- host storage, a backing key, or a
/// computed expression) and the bound `tTJSVariant(dsp, dsp)` closures the
/// script-facing getters hand out. The accessor is skipped, the closure is
/// unwrapped to the object it binds, so a reader never receives the property
/// object itself.
pub(crate) fn plain_member_object(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Option<ObjectHandle> {
    let value = runtime.object_member(object, name);
    if runtime.variant_is_property(&value) {
        return None;
    }
    variant_object_handle(&value).map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

fn layer_window_object(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> Option<ObjectHandle> {
    variant_object(&layer_property_value(runtime, layer, "window"))
        .map(|window| runtime.bound_this(window).unwrap_or(window))
}

/// The window's layers in official overall order (`tTVPLayerManager::AllNodes`,
/// `LayerManager.cpp:181-190`: the primary layer's subtree, depth first, each
/// node before its children).
fn window_layers_in_order(
    runtime: &Runtime<KrkrHost>,
    window: ObjectHandle,
    fallback_root: ObjectHandle,
) -> Vec<ObjectHandle> {
    let root = runtime
        .host()
        .native_window_primary_layer(window)
        .or_else(|| plain_member_object(runtime, window, "primaryLayer"))
        .unwrap_or(fallback_root);
    let mut layers = Vec::new();
    let mut visited = BTreeSet::new();
    let mut focusable = Vec::new();
    collect_layers(runtime, root, &mut visited, &mut layers, &mut focusable);
    layers
}

/// Depth-first walk of one layer subtree that records every node and, in the
/// same pass, the focus-chain members.
///
/// Children are visited in the reference's `Children` order, i.e. z-order:
/// `tTVPLayerManager::AllNodes` is rebuilt from that vector
/// (`LayerManager.cpp:181-190`) and `GetPrevFocusable`/`GetNextFocusable` walk
/// the result, so a reorder has to move the focus search with it.
fn collect_layers(
    runtime: &Runtime<KrkrHost>,
    layer: ObjectHandle,
    visited: &mut BTreeSet<ObjectHandle>,
    all: &mut Vec<ObjectHandle>,
    focusable: &mut Vec<ObjectHandle>,
) {
    let layer = runtime.bound_this(layer).unwrap_or(layer);
    if !visited.insert(layer) {
        return;
    }
    if layer_is_node_focusable(runtime, layer) && layer_joins_focus_chain(runtime, layer) {
        focusable.push(layer);
    }
    all.push(layer);
    for child in layer_children_in_draw_order(runtime, layer) {
        collect_layers(runtime, child, visited, all, focusable);
    }
}

/// [`layer_children`] sorted into the render tree's draw order --
/// `LayerTree::sorted_children`'s `(z_order, id)` key, the engine's counterpart
/// of the reference's `Children` vector.
fn layer_children_in_draw_order(
    runtime: &Runtime<KrkrHost>,
    layer: ObjectHandle,
) -> Vec<ObjectHandle> {
    let mut children = layer_children(runtime, layer);
    children.sort_by_key(|child| {
        runtime
            .host()
            .native_layer(*child)
            .and_then(|id| runtime.host().layer_tree().layer(id))
            .map(|node| (node.z_order, node.id))
            .unwrap_or((i32::MAX, 0))
    });
    children
}

fn focusable_layers_for_window(
    runtime: &Runtime<KrkrHost>,
    window: ObjectHandle,
    fallback_root: ObjectHandle,
) -> Vec<ObjectHandle> {
    let root = runtime
        .host()
        .native_window_primary_layer(window)
        .or_else(|| plain_member_object(runtime, window, "primaryLayer"))
        .unwrap_or(fallback_root);
    let mut all = Vec::new();
    let mut focusable = Vec::new();
    let mut visited = BTreeSet::new();
    collect_layers(runtime, root, &mut visited, &mut all, &mut focusable);
    focusable
}

/// `tTJSNI_BaseLayer::GetPrevFocusable`/`GetNextFocusable`
/// (`LayerIntf.cpp:3243-3319`): starting from the neighbouring node in the
/// window's overall order, walk forward (or backward) until a node that is
/// node-focusable and joins the focus chain turns up -- wrapping around the
/// order, never answering `this` twice. The reference then posts
/// `onSearchPrevFocusable`/`onSearchNextFocusable` with the candidate, which a
/// script may replace through `onSearchWork`; the getter reports the result.
fn layer_relative_focusable(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    forward: bool,
) -> Result<Variant> {
    let Some(window) = layer_window_object(runtime, layer) else {
        return Ok(Variant::Null);
    };
    let order = window_layers_in_order(runtime, window, layer);
    let candidate = order
        .iter()
        .position(|entry| *entry == layer)
        .and_then(|index| {
            let count = order.len();
            (1..count).find_map(|step| {
                let position = if forward {
                    (index + step) % count
                } else {
                    (index + count - step) % count
                };
                let entry = order[position];
                (entry != layer
                    && layer_is_node_focusable(runtime, entry)
                    && layer_joins_focus_chain(runtime, entry))
                .then_some(entry)
            })
        });
    let method = if forward {
        "onSearchNextFocusable"
    } else {
        "onSearchPrevFocusable"
    };
    let target = layer_focus_work(runtime, layer, method, candidate, Vec::new())?;
    Ok(target
        .map(|target| self_bound(Variant::Object(target)))
        .unwrap_or(Variant::Null))
}

fn layer_children(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> Vec<ObjectHandle> {
    let children = runtime.host().native_layer_children(layer);
    if !children.is_empty() {
        return children;
    }
    // A native instance's child list is authoritative -- an empty one means no
    // children -- and the script array may still be dirty (empty right after a
    // detach); only a handle without an instance falls back to the array.
    if runtime.host().native_layer(layer).is_some() {
        return children;
    }
    let Some(children) = layer_property_value(runtime, layer, "children").object_handle() else {
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

fn layer_set_mode(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this_obj) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) else {
        return Ok(Variant::Void);
    };
    let Some(layer_id) = runtime.host().native_layer(this_obj) else {
        return Ok(Variant::Void);
    };
    runtime.host_mut().set_modal_layer(this_obj, layer_id);
    Ok(Variant::Void)
}

fn layer_remove_mode(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this_obj) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) else {
        return Ok(Variant::Void);
    };
    let Some(layer_id) = runtime.host().native_layer(this_obj) else {
        return Ok(Variant::Void);
    };
    runtime.host_mut().remove_modal_layer(layer_id);
    Ok(Variant::Void)
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
    // `TVPCreateEventObject` stores the target as `tTJSVariant(targthis, targ)`
    // (`EventIntf.cpp:889`) and `TVP_ACTION_INVOKE_BEGIN` hands the event
    // itself as `tTJSVariant(evobj, evobj)` (`EventIntf.h:201`), so a handler
    // receives both self-bound.
    runtime.set_object_member(event, "target", self_bound(Variant::Object(this)));
    runtime.set_object_member(event, "type", Variant::String("onClick".to_string()));
    runtime.call_object_method(window, "action", vec![self_bound(Variant::Object(event))])
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
        let (origin, hit_threshold, hit_type, image_left, image_top, image, province) = {
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
                layer.province.clone(),
            )
        };
        let local_x = (point_x - origin.x).floor() as i64;
        let local_y = (point_y - origin.y).floor() as i64;
        let pixel_hit = if hit_type == 1 {
            // htProvince: hit where the province index is non-zero
            // (`LayerIntf.cpp:2911`).
            province.as_ref().is_some_and(|province| {
                province.pixel(local_x - image_left as i64, local_y - image_top as i64) != 0
            })
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
        // `*result = tTJSVariant(lay->GetOwnerNoAddRef(),
        // lay->GetOwnerNoAddRef())` (`LayerIntf.cpp:6909`).
        return Ok(self_bound(Variant::Object(object)));
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
// ancestor, and the current modal layer (`GetNodeEnabled()` is
// `GetEnabled() && ParentEnabled() && !IsDisabledByMode()`, `LayerIntf.h:651`).
// It is recomputed on every query, never cached.
fn render_node_enabled(tree: &krkr_core::LayerTree, id: krkr_core::LayerId) -> bool {
    tree.node_enabled(id)
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
    let bytes = runtime.host_mut().read_binary_storage_for_tjs(&storage)?;
    runtime
        .host_mut()
        .font_system_mut()
        .map_prerendered_font_for_spec_arc(&font, std::sync::Arc::from(bytes))
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

    // `tTJSNI_BaseLayer::AssignImages` (`LayerIntf.cpp:2124`) re-points the
    // destination at the source's bitmap (`MainImage->Assign`,
    // `LayerBitmapImpl.cpp:617`) rather than copying pixels; a destination that
    // already holds that bitmap reports "unchanged" and skips the geometry tail
    // below (`main_changed`, `LayerIntf.cpp:2154`).  This engine's layers own an
    // immutable `LayerImage`, so sharing it is the same copy-on-write contract:
    // every pixel write allocates a fresh image and never touches the other
    // layer's bitmap.  Identity is the pixel buffer, not the texture id -- a
    // running layer replaces its pixels under a stable id.
    let dest_image = render_layer_snapshot(runtime, dest_target).and_then(|layer| layer.image);
    let main_changed = match (dest_image.as_ref(), source.image.as_ref()) {
        (Some(dest), Some(source)) => !Arc::ptr_eq(&dest.upload.rgba, &source.upload.rgba),
        _ => true,
    };
    let copied_image = source.image.clone();
    let copied_province = source.province.clone();
    mutate_render_layer(runtime, dest_target, |dest| {
        match copied_image {
            Some(image) => {
                dest.image_width = image.upload.width as f32;
                dest.image_height = image.upload.height as f32;
                dest.image = Some(image);
                // `AssignImages` resets the clip after the assign
                // (`LayerIntf.cpp:2162`); a deallocated image leaves it alone.
                dest.clip = None;
            }
            None => dest.clear_image(),
        }
        // `ProvinceImage` is assigned alongside the main image, and dropped
        // when the source has none (`LayerIntf.cpp:2142`).
        dest.province = copied_province;
    });

    // `AssignImages` copies only the main image; the image offsets follow from
    // `InternalSetImageSize` (`LayerIntf.cpp:2154`), never from the source --
    // KAGEX copies `ImageLeft`/`ImageTop` itself (`MessageLayer.assignImages`)
    // when it wants them.  The old shape of this function handed over the
    // source offsets and sizes verbatim, which let a page-sized snapshot leak
    // past the destination rect.
    if main_changed && let Some(image) = source.image.as_ref() {
        internal_set_layer_image_size(
            runtime,
            dest_object,
            image.upload.width as i64,
            image.upload.height as i64,
        )?;
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
        // A value stored from `this` or from `new` carries the instance as its
        // own ObjThis (`tTJSVariant(objthis, objthis)`), which this engine
        // models as a bound closure over the same object.
        Variant::Closure(closure) => Some(closure.object),
        _ => None,
    }
}

fn complete_sourced_transition_exchange(
    runtime: &mut Runtime<KrkrHost>,
    dest: ObjectHandle,
    source: ObjectHandle,
    with_children: bool,
) -> Result<()> {
    let dest = runtime.bound_this(dest).unwrap_or(dest);
    let source = runtime.bound_this(source).unwrap_or(source);
    if dest == source {
        return Ok(());
    }

    let dest_left = layer_property_i64(runtime, dest, "left", 0)?;
    let dest_top = layer_property_i64(runtime, dest, "top", 0)?;
    let dest_visible = layer_property_value(runtime, dest, "visible").is_truthy();
    let source_left = layer_property_i64(runtime, source, "left", 0)?;
    let source_top = layer_property_i64(runtime, source, "top", 0)?;
    let source_visible = layer_property_value(runtime, source, "visible").is_truthy();

    exchange_layer_tree(runtime, dest, source, !with_children)?;

    set_layer_int_property(runtime, dest, "left", source_left)?;
    set_layer_int_property(runtime, dest, "top", source_top)?;
    set_layer_int_property(runtime, dest, "visible", i64::from(source_visible))?;
    set_layer_int_property(runtime, source, "left", dest_left)?;
    set_layer_int_property(runtime, source, "top", dest_top)?;
    set_layer_int_property(runtime, source, "visible", i64::from(dest_visible))?;
    Ok(())
}

fn exchange_layer_tree(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    target: ObjectHandle,
    keep_children: bool,
) -> Result<()> {
    let this_parent = runtime.host().native_layer_parent(this);
    let target_parent = runtime.host().native_layer_parent(target);
    let this_primary = layer_property_value(runtime, this, "isPrimary").is_truthy();
    let target_primary = layer_property_value(runtime, target, "isPrimary").is_truthy();
    let this_z = native_layer_z_order(runtime, this);
    let target_z = native_layer_z_order(runtime, target);
    let this_under_target = ancestor_child_layer(runtime, this, target);
    let target_under_this = ancestor_child_layer(runtime, target, this);

    join_native_layer(runtime, this, None)?;
    join_native_layer(runtime, target, None)?;

    if let Some(this_ancestor_child) = this_under_target {
        if this_ancestor_child != this {
            join_native_layer(runtime, this_ancestor_child, None)?;
        }
        let this_children = keep_children
            .then(|| take_native_layer_children(runtime, this))
            .unwrap_or_default();
        let target_children = keep_children
            .then(|| take_native_layer_children(runtime, target))
            .unwrap_or_default();
        join_native_layer(runtime, this, target_parent)?;
        if Some(target) == this_parent {
            join_native_layer(runtime, target, Some(this))?;
        } else {
            join_native_layer(runtime, target, this_parent)?;
        }
        if keep_children {
            for child in this_children {
                join_native_layer(runtime, child, Some(target))?;
            }
            for child in target_children {
                join_native_layer(runtime, child, Some(this))?;
            }
        }
        if this_ancestor_child != this {
            join_native_layer(runtime, this_ancestor_child, Some(this))?;
        }
    } else if let Some(target_ancestor_child) = target_under_this {
        if target_ancestor_child != target {
            join_native_layer(runtime, target_ancestor_child, None)?;
        }
        let this_children = keep_children
            .then(|| take_native_layer_children(runtime, this))
            .unwrap_or_default();
        let target_children = keep_children
            .then(|| take_native_layer_children(runtime, target))
            .unwrap_or_default();
        if Some(this) == target_parent {
            join_native_layer(runtime, this, Some(target))?;
        } else {
            join_native_layer(runtime, this, target_parent)?;
        }
        join_native_layer(runtime, target, this_parent)?;
        if keep_children {
            for child in this_children {
                join_native_layer(runtime, child, Some(target))?;
            }
            for child in target_children {
                join_native_layer(runtime, child, Some(this))?;
            }
        }
        if target_ancestor_child != target {
            join_native_layer(runtime, target_ancestor_child, Some(target))?;
        }
    } else {
        let this_children = keep_children
            .then(|| take_native_layer_children(runtime, this))
            .unwrap_or_default();
        let target_children = keep_children
            .then(|| take_native_layer_children(runtime, target))
            .unwrap_or_default();
        join_native_layer(runtime, this, target_parent)?;
        join_native_layer(runtime, target, this_parent)?;
        if keep_children {
            for child in this_children {
                join_native_layer(runtime, child, Some(target))?;
            }
            for child in target_children {
                join_native_layer(runtime, child, Some(this))?;
            }
        }
    }

    set_layer_property_storage(
        runtime,
        this,
        "isPrimary",
        Variant::Integer(i64::from(target_primary)),
    );
    set_layer_property_storage(
        runtime,
        target,
        "isPrimary",
        Variant::Integer(i64::from(this_primary)),
    );
    // `Exchange` parts both layers before it hands the primary role to the
    // swapped-in one (`LayerIntf.cpp:927`), so their render state has to be
    // recomputed for the new primary to draw as the page root.
    runtime.host_mut().apply_layer_instance_to_render(this);
    runtime.host_mut().apply_layer_instance_to_render(target);
    if this_primary || target_primary {
        let window = variant_object(&layer_property_value(runtime, this, "window"))
            .or_else(|| variant_object(&layer_property_value(runtime, target, "window")))
            .map(|window| runtime.bound_this(window).unwrap_or(window));
        if let Some(window) = window {
            // DetachPrimary: official LayerManager clears keyboard focus and
            // capture before the new primary is attached.  KAGEX only routes
            // Ctrl-skip through Window.processKeys when focusedLayer is null.
            blur_window_focus(runtime, window)?;
            let primary = if this_primary { target } else { this };
            set_window_property_storage(runtime, window, "primaryLayer", Variant::Object(primary));
            set_layer_int_property(runtime, primary, "visible", 1)?;
            set_layer_int_property(runtime, primary, "opacity", 255)?;
        }
    }

    let same_parent = this_parent == target_parent;
    if same_parent {
        set_native_layer_z_order(runtime, this, target_z);
        set_native_layer_z_order(runtime, target, this_z);
    } else {
        set_native_layer_z_order(runtime, this, this_z);
        set_native_layer_z_order(runtime, target, target_z);
    }
    Ok(())
}

fn ancestor_child_layer(
    runtime: &Runtime<KrkrHost>,
    descendant: ObjectHandle,
    ancestor: ObjectHandle,
) -> Option<ObjectHandle> {
    let mut previous = descendant;
    let mut current = runtime.host().native_layer_parent(descendant);
    while let Some(parent) = current {
        if parent == ancestor {
            return Some(previous);
        }
        previous = parent;
        current = runtime.host().native_layer_parent(parent);
    }
    None
}

fn take_native_layer_children(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
) -> Vec<ObjectHandle> {
    let children = runtime.host().native_layer_children(handle);
    for child in &children {
        let _ = join_native_layer(runtime, *child, None);
    }
    children
}

fn join_native_layer(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    parent: Option<ObjectHandle>,
) -> Result<()> {
    notify_part_if_attached(runtime, handle, parent)?;
    let value = parent.map(Variant::Object).unwrap_or(Variant::Void);
    set_layer_property_storage(runtime, handle, "parent", value.clone());
    apply_layer_property_to_render(runtime, handle, "parent", &value)
}

/// Joins `child` under `parent` the way a script's `child.parent = parent`
/// does, makes the child visible, and gives it the hit threshold the reference
/// gives its adaptor children.
///
/// `motionplayer.dll`'s `Motion.SeparateLayerAdaptor` is such a canvas: the
/// game constructs it with the owner layer (`new Motion.SeparateLayerAdaptor(
/// owner incontextof global.Layer)`) and switches that owner to `ltBinder`
/// right afterwards (`AffineSourceMotion.tjs` `entryOwner`), so the adaptor is
/// the only thing left drawing for the owner — which is exactly a visible
/// child the binder passes through to the screen.  The reference creates each
/// of the adaptor's host layers with `targetLayer` as its parent argument,
/// visible as soon as it has pixels, and sets `hitThreshold = 0x100` on them
/// (`motionplayer_nod3d.dll` `FUN_1000d280`, disassembly `0x1000d93d`/`0x1000d96c`):
/// 256 is above every 8-bit alpha, so the canvas never swallows a mouse hit
/// that belongs to the layers under or around it.
pub(crate) fn join_layer_under_parent(
    runtime: &mut Runtime<KrkrHost>,
    child: ObjectHandle,
    parent: ObjectHandle,
) -> Result<()> {
    join_native_layer(runtime, child, Some(parent))?;
    set_layer_int_property(runtime, child, "visible", 1)?;
    set_layer_int_property(runtime, child, "hitThreshold", 0x100)
}

/// `tTJSNI_BaseLayer::Join()` (`LayerIntf.cpp:576`): adopting a different
/// parent parts the old one first, so the manager hears about the layer
/// leaving the tree before it re-enters.
fn notify_part_if_attached(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    next_parent: Option<ObjectHandle>,
) -> Result<()> {
    let current = runtime.host().native_layer_parent(handle);
    if current.is_some() && current != next_parent {
        notify_part(runtime, handle)?;
    }
    Ok(())
}

/// `tTVPLayerManager::NotifyPart` (`LayerManager.cpp:168`): a subtree that
/// parts from the tree loses its modal state, the mouse leaves it, its capture
/// is released, and focus moves off it.  Without this a parted dialog keeps
/// blocking the layers it disabled for good, and a parted page keeps the
/// hover/focus state of controls that are no longer on screen.
fn notify_part(runtime: &mut Runtime<KrkrHost>, root: ObjectHandle) -> Result<()> {
    let Some(root_id) = runtime.host().native_layer(root) else {
        return blur_tree(runtime, root);
    };
    runtime.host_mut().remove_modal_layers_under(root_id);
    leave_mouse_from_tree(runtime, root_id)?;
    release_capture_from_tree(runtime, root_id);
    blur_tree(runtime, root)
}

/// `tTVPLayerManager::LeaveMouseFromTree` (`LayerManager.cpp:590`): only the
/// layer that actually received the last `onMouseMove` is notified, and the
/// manager forgets it so the next move enters whatever is under the cursor
/// afresh.
fn leave_mouse_from_tree(runtime: &mut Runtime<KrkrHost>, root: LayerId) -> Result<()> {
    let Some(hovered) = runtime.host().hovered_layer() else {
        return Ok(());
    };
    if !runtime
        .host()
        .layer_tree()
        .is_ancestor_or_self(root, hovered)
    {
        return Ok(());
    }
    runtime.host_mut().set_hovered_layer(None);
    let Some(handle) = runtime.host().native_object_for_layer(hovered) else {
        return Ok(());
    };
    if !matches!(runtime.object_member(handle, "onMouseLeave"), Variant::Void) {
        runtime.call_object_method(handle, "onMouseLeave", Vec::new())?;
    }
    Ok(())
}

/// `tTVPLayerManager::ReleaseCaptureFromTree` (`LayerManager.cpp:620`): a drag
/// that parts with its layer must not keep delivering moves to it.
fn release_capture_from_tree(runtime: &mut Runtime<KrkrHost>, root: LayerId) {
    let captured = runtime.host().captured_layer();
    if captured.is_some_and(|layer| runtime.host().layer_tree().is_ancestor_or_self(root, layer)) {
        runtime.host_mut().set_captured_layer(None);
    }
}

fn blur_tree(runtime: &mut Runtime<KrkrHost>, root: ObjectHandle) -> Result<()> {
    let Some(window) = layer_window_object(runtime, root) else {
        return Ok(());
    };
    let Some(focused) = focused_layer(runtime, window) else {
        return Ok(());
    };
    if !layer_is_ancestor_or_self(runtime, root, focused) {
        return Ok(());
    }
    // `BlurTree` (`LayerManager.cpp:692`) hands focus to the parted tree's next
    // focusable layer instead of dropping it: a focused button that leaves
    // with its page must not take the window's keyboard focus out of the
    // screen that is still up.
    if let Some(next) = next_focusable_outside(runtime, window, root) {
        return layer_set_focus_to(runtime, next, true).map(|_| ());
    }
    blur_window_focus(runtime, window)
}

/// `tTJSNI_BaseLayer::GetNextFocusable` (`LayerIntf.cpp:3300`) searches
/// forward through the overall layer order from the parted root.  This engine
/// keeps focusables in tree order instead, so the answer is the first
/// focusable that does not belong to the tree being blurred.
fn next_focusable_outside(
    runtime: &Runtime<KrkrHost>,
    window: ObjectHandle,
    root: ObjectHandle,
) -> Option<ObjectHandle> {
    focusable_layers_for_window(runtime, window, root)
        .into_iter()
        .find(|layer| !layer_is_ancestor_or_self(runtime, root, *layer))
}

fn blur_window_focus(runtime: &mut Runtime<KrkrHost>, window: ObjectHandle) -> Result<()> {
    if let Some(previous) = focused_layer(runtime, window) {
        // `Layer.focused` reports the window's focused layer
        // (`LayerIntf.cpp:3218`), so clearing the window's is what blurs it.
        if !matches!(runtime.object_member(previous, "onBlur"), Variant::Void) {
            runtime.call_object_method(previous, "onBlur", vec![Variant::Null])?;
        }
    }
    set_window_property_storage(runtime, window, "focusedLayer", Variant::Null);
    Ok(())
}

fn layer_is_ancestor_or_self(
    runtime: &Runtime<KrkrHost>,
    ancestor: ObjectHandle,
    node: ObjectHandle,
) -> bool {
    if ancestor == node {
        return true;
    }
    let mut current = runtime.host().native_layer_parent(node);
    while let Some(parent) = current {
        if parent == ancestor {
            return true;
        }
        current = runtime.host().native_layer_parent(parent);
    }
    false
}

fn native_layer_z_order(runtime: &Runtime<KrkrHost>, handle: ObjectHandle) -> i32 {
    runtime
        .host()
        .native_layer(handle)
        .and_then(|layer_id| runtime.host().layer_tree().layer(layer_id))
        .map(|layer| layer.z_order)
        .unwrap_or(0)
}

fn set_native_layer_z_order(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle, z_order: i32) {
    if let Some(layer_id) = runtime.host().native_layer(handle)
        && let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id)
    {
        layer.z_order = z_order;
    }
    set_layer_property_storage(
        runtime,
        handle,
        "absolute",
        Variant::Integer(i64::from(z_order)),
    );
}

fn set_layer_int_property(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &str,
    value: i64,
) -> Result<()> {
    if name == "visible" && value == 0 {
        blur_tree(runtime, handle)?;
    }
    let variant = Variant::Integer(value);
    set_layer_property_storage(runtime, handle, name, variant.clone());
    apply_layer_property_to_render(runtime, handle, name, &variant)
}

fn finish_immediate_transition(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    source: Option<ObjectHandle>,
    with_children: bool,
) -> Result<()> {
    if let Some(source) = source.filter(|source| runtime.object_valid(*source)) {
        complete_sourced_transition_exchange(runtime, layer, source, with_children)?;
    }
    let Some(window) = variant_object(&layer_property_value(runtime, layer, "window"))
        .map(|window| runtime.bound_this(window).unwrap_or(window))
    else {
        return Ok(());
    };
    let trans_count_before = runtime
        .object_member(window, "transCount")
        .to_integer()
        .ok();
    // `InternalStopTransition` posts the event with `TVP_EPT_IMMEDIATE`
    // (`LayerIntf.cpp:6420`), and `TVPPostEvent` drops it while event
    // dispatching is disabled (`EventIntf.cpp:230-258`).
    let deliver_event = !runtime.host().scheduler().event_disabled();
    if deliver_event {
        notify_transition_completed(runtime, layer, source)?;
    }
    finish_kag_window_transition_if_pending(runtime, layer)?;
    if deliver_event && !script_owns_transition_completion(runtime, window, trans_count_before) {
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

/// Whether the script's own `onTransitionCompleted` relay took ownership of a
/// completion.
///
/// KAG's relay decrements `window.transCount` (`KAGLayer.tjs`), so a plain
/// counter comparison cannot tell "the callback consumed this completion" from
/// "the callback consumed it and started a chained transition, restoring the
/// counter" -- the net-zero case, where the fallback used to decrement a
/// counter the script had already balanced and `waitTransition`
/// (`MainWindow.tjs:3231`) then resumed early.  Ownership is therefore decided
/// by a counter change *or* by another transition still running on the same
/// window, which is what a chained `beginTransition` leaves behind.  Without
/// ownership the engine keeps KAG's bookkeeping balanced on the script's
/// behalf, which is the only way a transition no TJS class observes can finish
/// a `[wt]`.
fn script_owns_transition_completion(
    runtime: &Runtime<KrkrHost>,
    window: ObjectHandle,
    trans_count_before: Option<i64>,
) -> bool {
    let window_of = |dest: ObjectHandle| {
        variant_object(&layer_property_value(runtime, dest, "window"))
            .map(|window| runtime.bound_this(window).unwrap_or(window))
    };
    if runtime
        .host()
        .transition_destinations()
        .into_iter()
        .any(|dest| window_of(dest) == Some(window))
    {
        return true;
    }
    trans_count_before.is_some_and(|before| {
        runtime
            .object_member(window, "transCount")
            .to_integer()
            .is_ok_and(|after| after != before)
    })
}

/// Runs the script calls provider handlers queued during this tick's passes —
/// [`TransitionScriptCallback`](crate::plugin_api::transition::TransitionScriptCallback)
/// (`plugin_api::transition`).
///
/// A handler's pass runs from the host clock, where the TJS runtime is not in
/// scope, so the call is queued and delivered here — on the script thread, in
/// the same tick the pass ran (`engine.rs` calls this right after
/// `advance_transition`; `Layer.update()` drains its own destination
/// directly).  A callback that throws stops its own destination's queue; every
/// other destination still drains, and the first error is what the caller
/// reports like any other transition callback error.
pub(crate) fn drain_transition_script_calls(runtime: &mut Runtime<KrkrHost>) -> Result<()> {
    let destinations = runtime.host().transition_destinations();
    let mut first_error = None;
    for dest in destinations {
        if let Err(error) = drain_transition_script_calls_for(runtime, dest)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// [`drain_transition_script_calls`] for one destination layer, whatever
/// transitions are active elsewhere.  The queue lives in the layer's extension
/// slot (`TransitionScriptCallQueue`); a destination without one has nothing
/// queued.
///
/// A call that throws stops *this* destination's queue and is returned: the
/// remaining calls are more invocations of the same hook, so running them
/// after one failure would only pile up exceptions the engine discards — the
/// reported first error is the useful one.
fn drain_transition_script_calls_for(
    runtime: &mut Runtime<KrkrHost>,
    dest: ObjectHandle,
) -> Result<()> {
    let Some(queue) = runtime
        .host()
        .layer_extension::<TransitionScriptCallQueue>(dest)
    else {
        return Ok(());
    };
    for call in queue.take() {
        runtime.call_function(call.callee, call.args)?;
    }
    Ok(())
}

pub(crate) fn finish_completed_native_transitions(runtime: &mut Runtime<KrkrHost>) -> Result<()> {
    let completions = runtime.host_mut().take_completed_native_transitions();
    for completion in completions {
        finish_native_transition(runtime, completion)?;
    }
    // Provider passes ran on the host clock before this call (`engine.rs`
    // advances the transitions first), so the script calls they queued are
    // delivered here, with the runtime in hand.
    drain_transition_script_calls(runtime)?;
    Ok(())
}

pub(crate) fn finish_native_transition(
    runtime: &mut Runtime<KrkrHost>,
    completion: NativeTransitionCompletion,
) -> Result<()> {
    if !runtime.object_valid(completion.dest) {
        return Ok(());
    }
    if let Some(source) = completion
        .source
        .filter(|source| runtime.object_valid(*source))
    {
        complete_sourced_transition_exchange(
            runtime,
            completion.dest,
            source,
            completion.with_children,
        )?;
    }

    let window = variant_object(&layer_property_value(runtime, completion.dest, "window"))
        .map(|window| runtime.bound_this(window).unwrap_or(window));
    let trans_count_before = window.and_then(|window| {
        runtime
            .object_member(window, "transCount")
            .to_integer()
            .ok()
    });

    // `InternalStopTransition` posts the event with `TVP_EPT_IMMEDIATE`
    // (`LayerIntf.cpp:6420`), and `TVPPostEvent` drops it while event
    // dispatching is disabled (`EventIntf.cpp:230-258`).
    let deliver_event = !runtime.host().scheduler().event_disabled();
    if deliver_event {
        notify_transition_completed(runtime, completion.dest, completion.source)?;
    }
    finish_kag_window_transition_if_pending(runtime, completion.dest)?;

    let owned = window.is_some_and(|window| {
        script_owns_transition_completion(runtime, window, trans_count_before)
    });
    if deliver_event && !owned {
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
    // The transition-completed event hands the destination and source layers
    // out as `tTJSVariant(TransDestObj, TransDestObj)` /
    // `(TransSrcObj, TransSrcObj)` (`LayerIntf.cpp:6411-6418`).
    let source = source
        .filter(|source| runtime.object_valid(*source))
        .map(Variant::self_bound)
        .unwrap_or_default();
    let callback_args = vec![self_bound(Variant::Object(dest)), source];
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
    // KAG's window exposes `inTransition` as a script property, so the raw
    // member is the accessor closure and only TJS dispatch yields the flag.
    // BaseLayerBase.onTransitionCompleted relays the event to
    // KAGWindow.onTransitionEnd, which clears the flag; a truthy value here
    // means that relay did not run.
    let in_transition = runtime
        .resolve_object_member(window, "inTransition")?
        .is_truthy();
    if !in_transition
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
    // KAGEX replaces the layer font with a script `FontHook` through the
    // `TJS_IGNOREPROP` store, so the value here is a self-bound object.
    match layer_property_value(runtime, layer, "font").object_handle() {
        Some(font) => font_spec_from_object(runtime, font),
        None => Ok(FontSpec::default()),
    }
}

/// Read a font attribute through the TJS dispatch path. A game may wrap the
/// layer font in a script `FontHook` whose `face`/`height`/style members are
/// TJS properties forwarding to the real native font; a raw member read would
/// see the property object itself and fall back to defaults (script ruby was
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
    let bytes = runtime.host_mut().read_binary_storage_for_tjs(&spec.face)?;
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

/// `tTVPDrawFace` (`LayerIntf.h`): `dfBoth`/`dfAlpha` share value 0 and
/// `dfMain`/`dfOpaque` share value 1.
const DF_ALPHA: i64 = 0;
const DF_MAIN: i64 = 1;
const DF_MASK: i64 = 2;
const DF_PROVINCE: i64 = 3;
const DF_ADD_ALPHA: i64 = 4;
const DF_AUTO: i64 = 128;

/// `tTJSNI_BaseLayer::HoldAlpha`. Unset members follow `TVPDefaultHoldAlpha`
/// (false) in `LayerIntf.cpp`.
fn layer_holds_alpha(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> bool {
    layer_property_value(runtime, layer, "holdAlpha").is_truthy()
}

/// `tTJSNI_BaseLayer::UpdateDrawFace`: `dfAuto` resolves to a concrete face
/// from the layer type, everything else is used verbatim.
fn effective_draw_face(runtime: &Runtime<KrkrHost>, layer: ObjectHandle) -> i64 {
    let face = layer_property_value(runtime, layer, "face")
        .to_integer()
        .unwrap_or(DF_AUTO);
    if face != DF_AUTO {
        return face;
    }
    let layer_type = layer_property_value(runtime, layer, "type")
        .to_integer()
        .unwrap_or(1);
    match layer_type {
        // ltAlpha and the ltPs* Photoshop blend modes draw onto both planes.
        2 | 13..=28 => DF_ALPHA,
        12 => DF_ADD_ALPHA, // ltAddAlpha
        _ => DF_MAIN,
    }
}

/// `TVPOpacityOnOpacityTable`: the weight the source colour gets when a source
/// with opacity `opa` is composited over a destination with opacity `dopa`.
fn opacity_on_opacity(dopa: i64, opa: i64) -> i64 {
    if dopa == 0 {
        return 255;
    }
    let at = dopa as f32 / 255.0;
    let bt = opa as f32 / 255.0;
    let mut c = bt / at;
    c /= 1.0 - bt + c;
    ((c * 255.0) as i64).clamp(0, 255)
}

/// `TVPConstColorAlphaBlend_d`: composite a constant colour onto a pixel while
/// honouring the destination alpha.
fn blend_const_color_on_alpha(pixel: &mut [u8], rgb: [u8; 3], opa: i64) {
    let dopa = pixel[3] as i64;
    let alpha = opacity_on_opacity(dopa, opa);
    for channel in 0..3 {
        let d = pixel[channel] as i64;
        pixel[channel] = (d + (((rgb[channel] as i64) - d) * alpha >> 8)).clamp(0, 255) as u8;
    }
    pixel[3] = (255 - ((255 - dopa) * (255 - opa) >> 8)).clamp(0, 255) as u8;
}

/// `TVPConstColorAlphaBlend`: composite a constant colour onto the colour plane
/// only, leaving the destination alpha untouched.
fn blend_const_color_keep_alpha(pixel: &mut [u8], rgb: [u8; 3], opa: i64) {
    let inv = 255 - opa;
    for channel in 0..3 {
        let d = pixel[channel] as i64;
        pixel[channel] = ((d * inv + (rgb[channel] as i64) * opa) >> 8).clamp(0, 255) as u8;
    }
}

/// `TVPRemoveConstOpacity`: scale the destination alpha down, keeping colour.
fn remove_const_opacity(pixel: &mut [u8], level: i64) {
    let strength = 255 - level;
    pixel[3] = (((pixel[3] as i64) * strength) >> 8).clamp(0, 255) as u8;
}

/// `ImageModified` for the layer's `imageModified` member.  Shared with the
/// plugin-facing commit path (`plugin_api::layer`), which marks the layer the
/// same way the native pixel setters do.
pub(crate) fn mark_image_modified(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) {
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

/// Win32 system-colour palette behind `TVPToActualColor`
/// (`visual/win32/LayerImpl.cpp:21`, `visual/win32/TVPColor.h:37`). The
/// official `GetSysColor` reads platform state; this is the classic desktop
/// palette, already swapped from `0xBBGGRR` to `0xRRGGBB`.
pub(crate) const SYSTEM_COLORS: [u32; 25] = [
    0x00c0_c0c0, // clScrollBar
    0x00ff_ffff, // clBackground
    0x0080_8080, // clActiveCaption
    0x0000_0080, // clInactiveCaption
    0x00c0_c0c0, // clMenu
    0x00ff_ffff, // clWindow
    0x0000_0000, // clWindowFrame
    0x0000_0000, // clMenuText
    0x00ff_ffff, // clWindowText
    0x0000_0000, // clCaptionText
    0x00c0_c0c0, // clActiveBorder
    0x00c0_c0c0, // clInactiveBorder
    0x00e0_e0e0, // clAppWorkSpace
    0x0000_007f, // clHighlight
    0x00ff_ffff, // clHighlightText
    0x00f0_f0f0, // clBtnFace
    0x0080_8080, // clBtnShadow
    0x0080_8080, // clGrayText
    0x0000_0000, // clBtnText
    0x00c0_c0c0, // clInactiveCaptionText
    0x00ff_ffff, // clBtnHighlight
    0x00ff_ffff, // cl3DDkShadow
    0x0000_0000, // cl3DLight
    0x00ff_ffff, // clInfoText
    0x00ff_ffff, // clInfoBk
];

/// `TVPToActualColor` (`visual/win32/LayerImpl.cpp:21`): a colour whose top
/// byte is set is a `GetSysColor` identifier (`cl*`, `TVPColor.h:6`), not a
/// raw RGB value. Shared with `System.toActualColor`.
pub(crate) fn to_actual_color(color: i64) -> i64 {
    let raw = color as u32;
    if raw & 0xff00_0000 == 0 {
        return color;
    }
    if raw & 0x8000_0000 != 0 {
        return i64::from(*SYSTEM_COLORS.get((raw & 0xff) as usize).unwrap_or(&0));
    }
    // `ColorToRGB` only consults the palette for identifiers with the sign bit
    // set; any other colour with a high byte passes through as `0xBBGGRR`, and
    // `TVPToActualColor` swaps it back to `0xRRGGBB`.
    let bgr = raw & 0x00ff_ffff;
    i64::from(((bgr & 0xff) << 16) | (bgr & 0xff00) | ((bgr & 0xff0000) >> 16))
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

/// Draw into the layer's *existing* main image, clipped to that image and to
/// the layer's `ClipRect`, without ever changing the image size.
///
/// The reference draws text through `MainImage->DrawText(ClipRect, x, y, ...)`
/// (`LayerIntf.cpp:4046`, `:4089`): `MainImage` only changes size through
/// `setImageSize`/`setSizeToImageSize`/`LoadImages`, and a draw that reaches
/// past the bitmap is clipped. Growing the plane here is what turned PARQUET's
/// 792x53 name layer into a 792x57 one between `transCapture` and
/// `assignImages`, so the `crossfade` in `CustomNameLayer.beginTrans`
/// (`sysscn/msghack.tjs:844`) threw `Transition layer size mismatch`.
///
/// Returns false when the layer is not drawable, the caller's
/// `TVPNotDrawableLayerType` (`LayerIntf.cpp:4020`, `:4064`): a freed bitmap is
/// not resurrected by a draw.
fn draw_into_layer_image<F>(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
    draw: F,
) -> bool
where
    F: FnOnce(&KrkrHost, &mut [u8], u32, u32),
{
    let Some(layer) = render_layer_snapshot(runtime, target) else {
        return true;
    };
    let Some(image) = layer.image else {
        return false;
    };
    let clip = layer_clip_bounds(runtime, target);
    let (width, height) = (image.upload.width, image.upload.height);
    let mut pixels = image.upload.rgba.as_ref().to_vec();
    // The glyph blitter already clips at the image edges; the layer's
    // `ClipRect` is applied by putting back whatever the draw changed outside
    // it.
    let unclipped = clip.map(|_| pixels.clone());
    draw(runtime.host(), &mut pixels, width, height);
    if let (Some(clip), Some(unclipped)) = (clip, unclipped) {
        restore_pixels_outside_clip(&mut pixels, width, height, clip, &unclipped);
    }
    let image = runtime.host_mut().create_layer_image(width, height, pixels);
    mutate_render_layer(runtime, target, |layer| layer.set_image(image));
    true
}

/// Put back the pixels a draw changed outside `clip`'s rectangle, the
/// `ClipRect` the reference hands to `MainImage->DrawText`
/// (`LayerIntf.cpp:4046`).
fn restore_pixels_outside_clip(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    clip: (i64, i64, i64, i64),
    original: &[u8],
) {
    let stride = width as usize * 4;
    let rows = height as usize;
    if stride == 0 || pixels.len() < stride * rows {
        return;
    }
    let x0 = clip.0.clamp(0, i64::from(width)) as usize;
    let y0 = clip.1.clamp(0, i64::from(height)) as usize;
    let x1 = clip.2.clamp(0, i64::from(width)) as usize;
    let y1 = clip.3.clamp(0, i64::from(height)) as usize;
    for row in 0..rows {
        let start = row * stride;
        let end = start + stride;
        if row < y0 || row >= y1 || x1 <= x0 {
            pixels[start..end].copy_from_slice(&original[start..end]);
            continue;
        }
        let left = start + x0 * 4;
        let right = start + x1 * 4;
        if left > start {
            pixels[start..left].copy_from_slice(&original[start..left]);
        }
        if right < end {
            pixels[right..end].copy_from_slice(&original[right..end]);
        }
    }
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

    let clip = layer_clip_bounds(runtime, target);
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = (x + width).clamp(0, image_width as i64) as u32;
    let y1 = (y + height).clamp(0, image_height as i64) as u32;
    let (x0, y0, x1, y1) = clip_rect_to_layer_clip(clip, x0, y0, x1, y1);
    if x1 <= x0 || y1 <= y0 {
        return Ok(());
    }
    // The snapshot above shares the layer's pixel buffer, so it has to go
    // before the whole-plane branch below asks whether this call owns that
    // buffer: an outstanding clone always reads as a second holder.
    drop(layer);

    if x0 == 0 && y0 == 0 && x1 == image_width && y1 == image_height && clip.is_none() {
        // The whole plane is replaced, so a buffer this call owns exclusively
        // is filled where it lies and published under a fresh texture id
        // (`plugin_api::layer`'s ownership rule); anything shared keeps its
        // bytes and gets a fresh image, exactly like before — cloning the old
        // plane first would copy every byte only to overwrite it.
        let Some(mut image) = crate::plugin_api::layer::take_unique_plane(runtime, target) else {
            let image = create_filled_layer_image(runtime, image_width, image_height, rgba);
            install_layer_plane(runtime, target, image, image_width, image_height);
            return Ok(());
        };
        // `fillRect(0, 0, w, h, 0)` is a clear, not a no-op: the owned plane is
        // zeroed before it is published, exactly like the fresh-plane path
        // (`create_layer_image` starts zeroed).  `make_mut` writes in place
        // because the take proved this call is the buffer's only holder.
        let pixels = Arc::make_mut(&mut image.upload.rgba);
        if rgba == [0, 0, 0, 0] {
            pixels.fill(0);
        } else {
            fill_pixel_buffer(pixels, rgba);
        }
        image.upload.texture_id = runtime.host_mut().allocate_video_texture_id();
        install_layer_plane(runtime, target, image, image_width, image_height);
        return Ok(());
    }

    let (x, y, width, height) = (x0 as i64, y0 as i64, (x1 - x0) as i64, (y1 - y0) as i64);
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

/// Trim an image-space rectangle to the layer's active `ClipRect`.
fn clip_rect_to_layer_clip(
    clip: Option<(i64, i64, i64, i64)>,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
) -> (u32, u32, u32, u32) {
    let Some((cx0, cy0, cx1, cy1)) = clip else {
        return (x0, y0, x1, y1);
    };
    (
        x0.max(cx0.max(0) as u32),
        y0.max(cy0.max(0) as u32),
        x1.min(cx1.max(0) as u32),
        y1.min(cy1.max(0) as u32),
    )
}

/// Apply a per-pixel operation over a rectangle of a layer's image, keeping the
/// pixels outside the rectangle (and the parts of the pixel the operation does
/// not touch) intact.
#[allow(clippy::too_many_arguments)]
fn blend_layer_pixels<F>(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    blend: F,
) -> Result<()>
where
    F: Fn(&mut [u8]),
{
    let clip = layer_clip_bounds(runtime, target);
    mutate_layer_pixels(runtime, target, |pixels, image_width, image_height| {
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x + width).clamp(0, image_width as i64) as u32;
        let y1 = (y + height).clamp(0, image_height as i64) as u32;
        let (x0, y0, x1, y1) = clip_rect_to_layer_clip(clip, x0, y0, x1, y1);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let row_width = image_width as usize * 4;
        for py in y0..y1 {
            let row_start = py as usize * row_width;
            let start = row_start + x0 as usize * 4;
            let end = row_start + x1 as usize * 4;
            if end > pixels.len() {
                return;
            }
            for pixel in pixels[start..end].chunks_exact_mut(4) {
                blend(pixel);
            }
        }
    })
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
    blt: blend::Blt,
    opacity: i64,
    hold_alpha: bool,
    clip: Option<(i64, i64, i64, i64)>,
    stretch_type: blend::StretchType,
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
    let (min_x, min_y, max_x, max_y) = clip_rect_to_layer_clip(
        clip,
        min_x.max(0) as u32,
        min_y.max(0) as u32,
        max_x.max(0) as u32,
        max_y.max(0) as u32,
    );
    let (min_x, min_y, max_x, max_y) = (min_x as i64, min_y as i64, max_x as i64, max_y as i64);
    for dy in min_y..max_y {
        for dx in min_x..max_x {
            // The affine points are the images of the source rectangle's
            // *corners*: `InternalAffineBlt` shifts the rectangle to
            // `refrect.*.65536 - 32768` (`LayerBitmapIntf.cpp:2711-2718`) while
            // `AffineBlt`'s matrix entry point builds them from `(-0.5,-0.5)`,
            // `(rp-0.5,-0.5)` and `(-0.5,bp-0.5)` (`:3494-3513`). Solving
            // `dest = p0 + u*(p1-p0) + v*(p2-p0)` therefore
            // puts `u`/`v` in the rectangle's corner frame (`u = 0` is the
            // left edge), and the nearest sample for a destination pixel is
            // `src + floor(u * len)`: the reference reads
            // `floor(σ + 0.5)` with `σ = src - 0.5 + u * len`
            // (`TVPDoAffineLoop`'s `+0.5`, `:2398-2399`), which is the same
            // index. `u, v ∈ [0,1)` is the reference's drawn set as well: a
            // sample outside `[src, src + len)` is dropped there
            // (`:2407-2438`), and `u < 0` or `u >= 1` always lands outside.
            let px = dx as f64 - x0;
            let py = dy as f64 - y0;
            let u = (px * vy - py * vx) / determinant;
            let v = (ux * py - uy * px) / determinant;
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            let sample = if stretch_type == blend::StretchType::Nearest {
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
                [
                    source[source_offset],
                    source[source_offset + 1],
                    source[source_offset + 2],
                    source[source_offset + 3],
                ]
            } else {
                let Some(sample) = blend::sample_rgba(
                    source,
                    texture_width,
                    texture_height,
                    sx as f64 + u * source_width as f64 - 0.5,
                    sy as f64 + v * source_height as f64 - 0.5,
                    stretch_type,
                ) else {
                    continue;
                };
                sample
            };
            let dest_offset = ((dy as u32 * dest_width + dx as u32) * 4) as usize;
            let d = u32::from_le_bytes([
                dest[dest_offset],
                dest[dest_offset + 1],
                dest[dest_offset + 2],
                dest[dest_offset + 3],
            ]);
            let s = u32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
            let out = blend::blt_pixel(d, s, blt, opacity as u32, hold_alpha);
            dest[dest_offset..dest_offset + 4].copy_from_slice(&out.to_le_bytes());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn clear_affine_destination(
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    points: [(f64, f64); 3],
    clear_color: [u8; 4],
) {
    let [(x0, y0), (x1, y1), (x2, y2)] = points;
    let ux = x1 - x0;
    let uy = y1 - y0;
    let vx = x2 - x0;
    let vy = y2 - y0;
    let determinant = ux * vy - uy * vx;
    if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
        fill_pixel_buffer(dest, clear_color);
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
            let px = dx as f64 - x0;
            let py = dy as f64 - y0;
            let u = (px * vy - py * vx) / determinant;
            let v = (ux * py - uy * px) / determinant;
            if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                continue;
            }
            let dest_offset = ((dy as u32 * dest_width + dx as u32) * 4) as usize;
            dest[dest_offset..dest_offset + 4].copy_from_slice(&clear_color);
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
    method: blend::Blt,
    opacity: i64,
    hold_alpha: bool,
    clip: Option<(i64, i64, i64, i64)>,
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
        clip,
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
        blend::blt_row(dest_row, src_row, method, opacity as u32, hold_alpha);
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
    /// The layer type of the bitmap this entry is composited into
    /// (`BltImage`'s `destlayertype`), i.e. the nearest non-binder ancestor's
    /// type (`LayerIntf.cpp:5164-5197`, `:5834`).
    dest_type: i64,
    /// The source layer of the `piledCopy` itself: its own image is copied into
    /// the pile (`CopySelf`, `LayerIntf.cpp:5561-5583`) rather than blitted.
    root: bool,
}

#[allow(clippy::too_many_arguments)]
fn collect_piled_render_layers(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
    parent_origin_x: f32,
    parent_origin_y: f32,
    parent_clip: Option<PiledClip>,
    parent_opacity: f32,
    dest_type: i64,
    include_position: bool,
    root: bool,
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
    // The gate is the reference's `IsSeen()` (`Visible && Opacity != 0`,
    // `LayerIntf.h:304`) as every *child* sees it: `Draw(..., true)` returns for
    // an unseen layer (`LayerIntf.cpp:5537`) and every child loop passes `true`
    // (`:5600`, `:5729`, `:5800`), so a zero-opacity binder hides its whole
    // subtree even though it draws no bitmap of its own.
    //
    // The *source layer of the `piledCopy` itself* is a known difference here:
    // the reference consults neither `Visible` nor `Opacity` for it — `PiledCopy`
    // only needs `MainImage` (`:4111-4112`) and `Complete()` renders the
    // source's own cache without an `IsSeen()` test (`:6104-6160`, the image
    // itself through `DrawSelf` `:5366`), which is why `PiledCopy` bypasses the
    // ordinary child draw path. This engine applies the child gate to the root
    // as well; filed as a follow-up rather than changed in place.
    let binder = i64::from(layer.layer_type) == LT_BINDER;
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
    // `PiledCopy` copies the source layer's completed bitmap plane for plane
    // (`LayerIntf.cpp:4120-4122`) and `tCompleteDrawable::DrawCompleted`
    // (`:6119-6128`) ignores the `type`/`opacity` it is handed, so the *source*
    // layer's own `Opacity` never reaches the pile. Each child is blitted with
    // its own `Opacity` (child `DrawSelf` → parent `DrawCompleted` → `BltImage`,
    // `:5385`, `:5920-5923`). A non-binder ancestor's opacity still multiplies
    // in as this engine's one-step approximation of the reference's two-step
    // composite (the ancestor's bitmap is blitted into its own parent with the
    // ancestor's opacity).
    let opacity = if root || binder {
        parent_opacity
    } else {
        parent_opacity * layer.opacity as f32 / 255.0
    };
    if opacity <= 0.0 {
        return;
    }
    output.push(PiledRenderLayer {
        layer: layer.clone(),
        origin_x,
        origin_y,
        clip,
        opacity,
        dest_type,
        root,
    });

    // `GetTargetLayerType` (`LayerIntf.cpp:5834`): a child of an `ltBinder`
    // layer draws into the bitmap of the nearest non-binder ancestor.
    let child_dest_type = if binder {
        dest_type
    } else {
        i64::from(layer.layer_type)
    };
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
            child_dest_type,
            true,
            false,
            visited,
            output,
        );
    }
}

/// `tTJSNI_BaseLayer::GetTargetLayerType` (`LayerIntf.cpp:5834`): the layer
/// type of the bitmap a layer's children are composited into is the layer's own
/// `DisplayType`, or its parent's for an `ltBinder` layer (walked up for nested
/// binders; `ltOpaque` when the chain runs out, `:5837`).
fn piled_layer_target_type(runtime: &Runtime<KrkrHost>, handle: ObjectHandle) -> i64 {
    let mut current = handle;
    for _ in 0..64 {
        let Some(target) = registered_render_layer_target(runtime, current) else {
            break;
        };
        let Some(layer) = render_layer_snapshot(runtime, &target) else {
            break;
        };
        if i64::from(layer.layer_type) != LT_BINDER {
            return i64::from(layer.layer_type);
        }
        let Some(parent) = layer_parent_object(runtime, current) else {
            break;
        };
        current = parent;
    }
    LT_OPAQUE
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

/// Composite one entry of a `piledCopy` source pile into the pile bitmap. The
/// pile is in *source-rectangle* coordinates: the pixel at `sx, sy` lands at
/// `(0, 0)`, which is how the reference's `Complete()` builds its offscreen
/// bitmap before `CopyRect` writes it into the destination (`LayerIntf.cpp`
/// `:4120-4122`).
#[allow(clippy::too_many_arguments)]
fn composite_piled_layer(
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    layer: &PiledRenderLayer,
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

    // `BltImage` (`LayerIntf.cpp:5164-5364`): the method comes from this
    // layer's own `DisplayType` and the `OnAlpha`/`OnAddAlpha` selection and
    // the blend families' `hda` flag come from the type of the bitmap the blit
    // lands in. `ltBinder` children draw nothing (`:5185-5187`).
    let blt = if layer.root {
        // The source layer's own image is *copied* into the pile
        // (`CopySelfForRect` → `dest->CopyRect(destx, desty, MainImage, cr)`,
        // `LayerIntf.cpp:5445-5448`), not blended.
        None
    } else {
        match blend::blt_image_for_layer_type(i64::from(layer.layer.layer_type), layer.dest_type) {
            Some(blt) => Some(blt),
            None => return,
        }
    };

    let dest_stride = dest_width as usize * 4;
    let source_stride = source_width as usize * 4;
    for root_y in copy_y0..copy_y1 {
        let dest_y = root_y - sy;
        if dest_y < 0 || dest_y >= dest_height as i64 {
            continue;
        }
        let source_y = (root_y as f32 - image_y0).floor() as i64;
        if source_y < 0 || source_y >= source_height as i64 {
            continue;
        }
        for root_x in copy_x0..copy_x1 {
            let dest_x = root_x - sx;
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
            if blt.is_none() {
                dest[dest_index..dest_index + 4].copy_from_slice(source_pixel);
                continue;
            }
            let (blt, hda) = blt.expect("checked above");
            let opacity = (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
            let d = u32::from_le_bytes([
                dest[dest_index],
                dest[dest_index + 1],
                dest[dest_index + 2],
                dest[dest_index + 3],
            ]);
            let s = u32::from_le_bytes([
                source_pixel[0],
                source_pixel[1],
                source_pixel[2],
                source_pixel[3],
            ]);
            let out = blend::blt_pixel(d, s, blt, opacity, hda);
            dest[dest_index..dest_index + 4].copy_from_slice(&out.to_le_bytes());
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
    blt: blend::Blt,
    opacity: i64,
    hold_alpha: bool,
    clip: Option<(i64, i64, i64, i64)>,
    stretch_type: blend::StretchType,
) {
    // A negative destination extent is the reference's mirrored blit: it
    // reaches `AffineBlt` with the rectangle's right/bottom edge left of its
    // left/top one (`LayerBitmapIntf.cpp:1867-1875`). Each destination pixel
    // takes the source sample of its mirror-image position.
    let mirror_x = dest_rect_width < 0;
    let mirror_y = dest_rect_height < 0;
    let dest_rect_width = dest_rect_width.abs();
    let dest_rect_height = dest_rect_height.abs();
    if dest_rect_width == 0
        || dest_rect_height == 0
        || source_rect_width <= 0
        || source_rect_height <= 0
    {
        return;
    }
    let dest_x0 = if mirror_x {
        dx.saturating_sub(dest_rect_width)
    } else {
        dx
    };
    let dest_y0 = if mirror_y {
        dy.saturating_sub(dest_rect_height)
    } else {
        dy
    };
    let dest_x1 = if mirror_x {
        dx
    } else {
        dx.saturating_add(dest_rect_width)
    };
    let dest_y1 = if mirror_y {
        dy
    } else {
        dy.saturating_add(dest_rect_height)
    };
    let dest_x0 = dest_x0.max(0);
    let dest_y0 = dest_y0.max(0);
    let dest_x1 = dest_x1.clamp(0, dest_width as i64);
    let dest_y1 = dest_y1.clamp(0, dest_height as i64);
    let (dest_x0, dest_y0, dest_x1, dest_y1) = clip_rect_to_layer_clip(
        clip,
        dest_x0 as u32,
        dest_y0 as u32,
        dest_x1 as u32,
        dest_y1 as u32,
    );
    let (dest_x0, dest_y0, dest_x1, dest_y1) = (
        dest_x0 as i64,
        dest_y0 as i64,
        dest_x1 as i64,
        dest_y1 as i64,
    );
    if dest_x1 <= dest_x0 || dest_y1 <= dest_y0 {
        return;
    }

    let dest_stride = dest_width as usize * 4;
    let source_stride = source_texture_width as usize * 4;
    for dest_y in dest_y0..dest_y1 {
        let rel_y = if mirror_y {
            dy - 1 - dest_y
        } else {
            dest_y - dy
        };
        let nearest = stretch_type == blend::StretchType::Nearest;
        let source_y = sy + ((2 * rel_y + 1) * source_rect_height) / (2 * dest_rect_height);
        if nearest && (source_y < 0 || source_y >= source_texture_height as i64) {
            continue;
        }
        for dest_x in dest_x0..dest_x1 {
            let rel_x = if mirror_x {
                dx - 1 - dest_x
            } else {
                dest_x - dx
            };
            let dest_index = dest_y as usize * dest_stride + dest_x as usize * 4;
            if dest_index + 4 > dest.len() {
                continue;
            }
            let sample = if nearest {
                // A `type < stLinear` stretch is `AffineBlt` with the corners
                // `destrect.* - 0.5` (`LayerBitmapIntf.cpp:1866-1875`), whose
                // nearest sample for a destination pixel is
                // `src + floor((rel + 0.5) * src_len / dst_len)`
                // (`InternalAffineBlt` + the `+0.5` rounding of
                // `TVPDoAffineLoop`, `:2398-2399`).
                let source_x = sx + ((2 * rel_x + 1) * source_rect_width) / (2 * dest_rect_width);
                if source_x < 0 || source_x >= source_texture_width as i64 {
                    continue;
                }
                let source_index = source_y as usize * source_stride + source_x as usize * 4;
                if source_index + 4 > source.len() {
                    continue;
                }
                [
                    source[source_index],
                    source[source_index + 1],
                    source[source_index + 2],
                    source[source_index + 3],
                ]
            } else {
                // Filtered sampling: the reference's resampler positions
                // destination pixel `d` at the source coordinate
                // `cx = (d + 0.5) * srclength / dstlength + srcstart`
                // (`gl/ResampleImage.cpp:302`), folds its kernel at the source
                // rectangle and widens the kernel for a shrink.
                let Some(sample) = blend::sample_resample_rgba(
                    source,
                    source_texture_width,
                    source_texture_height,
                    (sx, sy, sx + source_rect_width, sy + source_rect_height),
                    (dest_rect_width, dest_rect_height),
                    (rel_x, rel_y),
                    stretch_type,
                ) else {
                    continue;
                };
                sample
            };
            let d = u32::from_le_bytes([
                dest[dest_index],
                dest[dest_index + 1],
                dest[dest_index + 2],
                dest[dest_index + 3],
            ]);
            let s = u32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
            let out = blend::blt_pixel(d, s, blt, opacity as u32, hold_alpha);
            dest[dest_index..dest_index + 4].copy_from_slice(&out.to_le_bytes());
        }
    }
}

#[allow(clippy::too_many_arguments)]
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
    clip: Option<(i64, i64, i64, i64)>,
) -> Option<(i64, i64, i64, i64, i64, i64)> {
    if width <= 0 || height <= 0 {
        return None;
    }
    // `ClipDestPointAndSrcRect` (`LayerIntf.cpp:3755`) trims the destination
    // rectangle to `ClipRect` and shifts the source accordingly.
    if let Some((cx0, cy0, cx1, cy1)) = clip {
        if dx < cx0 {
            let delta = cx0 - dx;
            dx = cx0;
            sx += delta;
            width -= delta;
        }
        if dy < cy0 {
            let delta = cy0 - dy;
            dy = cy0;
            sy += delta;
            height -= delta;
        }
        width = width.min(cx1 - dx).max(0);
        height = height.min(cy1 - dy).max(0);
        if width <= 0 || height <= 0 {
            return None;
        }
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

/// A member declared on a native class spec: its name and the `numparams`
/// floor the reference declares (`if(numparams < N) return TJS_E_BADPARAMCOUNT;`),
/// `0` when the member is unguarded.  The floor reaches the stub registration,
/// so a short call fails with -1004 before the stub records it.  Members whose
/// only registration is a hand-written handler carry their floor at that
/// registration site instead.
type NativeMethodSpec = (&'static str, usize);

pub(crate) struct NativeClassSpec {
    name: &'static str,
    methods: &'static [NativeMethodSpec],
    properties: &'static [&'static str],
    static_methods: &'static [NativeMethodSpec],
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
    methods: &[("trigger", 0), ("cancel", 0)],
    properties: &["cached", "mode"],
    static_methods: &[],
    static_properties: &[],
};

/// `tTJSNI_BaseRect`'s floors (`visual/RectItf.cpp`): `setSize` :68,
/// `setOffset` :77, `addOffset` :86, `set` :103, `clip` :112, `union` :129,
/// `intersects` :146, `included` :163, `includedPos` :180, `equal` :190.
pub(crate) static RECT_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Rect",
    methods: &[
        ("isEmpty", 0),
        ("setSize", 2),
        ("setOffset", 2),
        ("addOffset", 2),
        ("clear", 0),
        ("set", 4),
        ("clip", 1),
        ("union", 1),
        ("intersects", 1),
        ("included", 1),
        ("includedPos", 2),
        ("equal", 1),
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

/// `tTJSNI_BaseBitmap`'s floors (`visual/BitmapIntf.cpp`): `getPixel` :211,
/// `setPixel` :220, `getMaskPixel` :229, `setMaskPixel` :238, `setSize` :258,
/// `copyFrom` :268, `save` :285, `load` :300, `loadAsync` :320, `loadHeader`
/// :329, `getSaveOption` :352.
pub(crate) static BITMAP_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Bitmap",
    methods: &[
        ("getPixel", 2),
        ("setPixel", 3),
        ("getMaskPixel", 2),
        ("setMaskPixel", 3),
        ("independ", 0),
        ("setSize", 2),
        ("copyFrom", 1),
        ("save", 1),
        ("load", 1),
        ("loadAsync", 1),
        ("loadHeader", 1),
        ("getSaveOption", 1),
        ("onLoaded", 0),
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

/// `tTJSNI_ImageFunction`'s floors (`visual/ImageFunction.cpp`):
/// `operateAffine` :132, `operateRect` :240, `operateStretch` :331, `flipLR`
/// :438, `flipUD` :474, `adjustGamma` :511, `doBoxBlur` :578, `doGrayScale`
/// :634, `fillRect` :670, `colorRect` :738.  `drawText`/`drawGlyph` are real
/// handlers here and carry their floors (`:829`/`:915`) at the registration
/// site.
pub(crate) static IMAGE_FUNCTION_CLASS: NativeClassSpec = NativeClassSpec {
    name: "ImageFunction",
    methods: &[
        ("operateAffine", 11),
        ("operateRect", 6),
        ("operateStretch", 2),
        ("flipLR", 1),
        ("flipUD", 1),
        ("adjustGamma", 1),
        ("doBoxBlur", 1),
        ("doGrayScale", 1),
        ("fillRect", 2),
        ("colorRect", 2),
        ("drawText", 0),
        ("drawGlyph", 0),
    ],
    properties: &[],
    static_methods: &[],
    static_properties: &[],
};

/// `tTJSNI_BitmapLayerTreeOwner`'s floors (`visual/BitmapLayerTreeOwner.cpp`):
/// `fireClick` :216, `fireDoubleClick` :225, `fireMouseDown` :234,
/// `fireMouseUp` :243, `fireMouseMove` :252, `fireMouseWheel` :261,
/// `fireTouchDown` :286, `fireTouchUp` :295, `fireTouchMove` :304,
/// `fireTouchScaling` :313, `fireTouchRotate` :322, `fireKeyDown` :339,
/// `fireKeyUp` :348, `fireKeyPress` :357, `fireDisplayRotate` :366.
pub(crate) static BITMAP_LAYER_TREE_OWNER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "BitmapLayerTreeOwner",
    methods: &[
        ("fireClick", 2),
        ("fireDoubleClick", 2),
        ("fireMouseDown", 4),
        ("fireMouseUp", 4),
        ("fireMouseMove", 3),
        ("fireMouseWheel", 4),
        ("fireReleaseCapture", 0),
        ("fireMouseOutOfWindow", 0),
        ("fireTouchDown", 5),
        ("fireTouchUp", 5),
        ("fireTouchMove", 5),
        ("fireTouchScaling", 5),
        ("fireTouchRotate", 6),
        ("fireMultiTouch", 0),
        ("fireKeyDown", 2),
        ("fireKeyUp", 2),
        ("fireKeyPress", 1),
        ("fireDisplayRotate", 5),
        ("fireRecheckInputState", 0),
        ("onSetMouseCursor", 0),
        ("onGetCursorPos", 0),
        ("onSetCursorPos", 0),
        ("onReleaseMouseCapture", 0),
        ("onSetHintText", 0),
        ("onResizeLayer", 0),
        ("onChangeLayerImage", 0),
        ("onSetAttentionPoint", 0),
        ("onDisableAttentionPoint", 0),
        ("onSetImeMode", 0),
        ("onResetImeMode", 0),
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
        ("add", 0),
        ("insert", 0),
        ("remove", 0),
        ("clear", 0),
        ("click", 0),
        ("onClick", 0),
        ("popup", 0),
    ],
    properties: &[
        "owner", "caption", "shortcut", "checked", "enabled", "visible", "radio", "group",
        "children",
    ],
    static_methods: &[],
    static_properties: &[],
};

/// `tTJSNC_Window`'s floor-free members and the ones only a stub serves; the
/// stub floors are `visual/WindowIntf.cpp`: `setMinSize` :861, `setMaxSize`
/// :870, `setLayerPos` :889, `postInputEvent` :926, plus
/// `visual/win32/WindowImpl.cpp`: `findFullScreenCandidates` :2082,
/// `registerMessageReceiver` :2104, `getTouchPoint` :2117, `getTouchVelocity`
/// :2168, `getMouseVelocity` :2193.  `add`/`remove`/`setSize`/`setPos`/
/// `setInnerSize`/`setZoom` are real handlers and carry their floors there.
pub(crate) static WINDOW_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Window",
    methods: &[
        ("close", 0),
        ("beginMove", 0),
        ("bringToFront", 0),
        ("update", 0),
        ("showModal", 0),
        ("setMaskRegion", 0),
        ("removeMaskRegion", 0),
        ("add", 0),
        ("remove", 0),
        ("setSize", 0),
        ("setMinSize", 2),
        ("setMaxSize", 2),
        ("setPos", 0),
        ("setLayerPos", 2),
        ("setInnerSize", 0),
        ("setZoom", 0),
        ("hideMouseCursor", 0),
        ("postInputEvent", 1),
        ("onResize", 0),
        ("onMouseEnter", 0),
        ("onMouseLeave", 0),
        ("onClick", 0),
        ("onDoubleClick", 0),
        ("onMouseDown", 0),
        ("onMouseUp", 0),
        ("onMouseMove", 0),
        ("onMouseWheel", 0),
        ("onTouchDown", 0),
        ("onTouchUp", 0),
        ("onTouchMove", 0),
        ("onTouchScaling", 0),
        ("onTouchRotate", 0),
        ("onMultiTouch", 0),
        ("onKeyDown", 0),
        ("onKeyUp", 0),
        ("onKeyPress", 0),
        ("onFileDrop", 0),
        ("onCloseQuery", 0),
        ("onPopupHide", 0),
        ("onActivate", 0),
        ("onDeactivate", 0),
        ("onDisplayRotate", 0),
        ("findFullScreenCandidates", 5),
        ("registerMessageReceiver", 3),
        ("getTouchPoint", 1),
        ("getTouchVelocity", 4),
        ("getMouseVelocity", 3),
        ("resetMouseVelocity", 0),
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

/// `tTJSNC_Layer`'s surface.  Only the members whose sole registration is the
/// stub carry a floor here: `moveBefore` (`LayerIntf.cpp:6731`), `moveBehind`
/// (:6752), `copy9Patch` (:7165), `setAttentionPos` (:7780),
/// `copyToBitmapFromMainImage` (:7865) and `copyFromBitmapToMainImage`
/// (:7884).  The rest are real handlers (floor at the registration site) or
/// have no reference arity test, and `onKeyDown`/`onKeyUp`/`onKeyPress`'s
/// `< N || <flag>` tests only pick the default handler -- they never reject.
pub(crate) static LAYER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Layer",
    methods: &[
        ("asLayer", 0),
        ("moveBefore", 1),
        ("moveBehind", 1),
        ("bringToBack", 0),
        ("bringToFront", 0),
        ("saveLayerImage", 0),
        ("loadImages", 0),
        ("freeImage", 0),
        ("loadProvinceImage", 0),
        ("getMainPixel", 0),
        ("setMainPixel", 0),
        ("getMaskPixel", 0),
        ("setMaskPixel", 0),
        ("getProvincePixel", 0),
        ("setProvincePixel", 0),
        ("getLayerAt", 0),
        ("setPos", 0),
        ("setSize", 0),
        ("setSizeToImageSize", 0),
        ("setImagePos", 0),
        ("setImageSize", 0),
        ("setDefaultCursor", 0),
        ("independMainImage", 0),
        ("independProvinceImage", 0),
        ("setClip", 0),
        ("fillRect", 0),
        ("colorRect", 0),
        ("drawText", 0),
        ("drawGlyph", 0),
        ("piledCopy", 0),
        ("copyRect", 0),
        ("copy9Patch", 1),
        ("operateRect", 0),
        ("stretchCopy", 0),
        ("operateStretch", 0),
        ("affineCopy", 0),
        ("operateAffine", 0),
        ("doBoxBlur", 0),
        ("adjustGamma", 0),
        ("doGrayScale", 0),
        ("flipLR", 0),
        ("flipUD", 0),
        ("convertType", 0),
        ("update", 0),
        ("setCursorPos", 0),
        ("releaseCapture", 0),
        ("releaseTouchCapture", 0),
        ("focus", 0),
        ("focusPrev", 0),
        ("focusNext", 0),
        ("setMode", 0),
        ("removeMode", 0),
        ("setAttentionPos", 2),
        ("beginTransition", 0),
        ("stopTransition", 0),
        ("assignImages", 0),
        ("exchangeInfo", 0),
        ("dump", 0),
        ("copyToBitmapFromMainImage", 1),
        ("copyFromBitmapToMainImage", 1),
        ("onHitTest", 0),
        ("onClick", 0),
        ("onDoubleClick", 0),
        ("onMouseDown", 0),
        ("onMouseUp", 0),
        ("onMouseMove", 0),
        ("onMouseEnter", 0),
        ("onMouseLeave", 0),
        ("onTouchDown", 0),
        ("onTouchUp", 0),
        ("onTouchMove", 0),
        ("onTouchScaling", 0),
        ("onTouchRotate", 0),
        ("onMultiTouch", 0),
        ("onBlur", 0),
        ("onFocus", 0),
        ("onNodeEnabled", 0),
        ("onNodeDisabled", 0),
        ("onKeyDown", 0),
        ("onKeyUp", 0),
        ("onKeyPress", 0),
        ("onMouseWheel", 0),
        ("onSearchPrevFocusable", 0),
        ("onSearchNextFocusable", 0),
        ("onBeforeFocus", 0),
        ("onPaint", 0),
        ("onTransitionCompleted", 0),
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

/// The Font method floors are the registration sites' (`LayerIntf.cpp`); the
/// stub list only has to declare the members, so every entry stays unguarded.
pub(crate) static FONT_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Font",
    methods: &[
        ("getTextWidth", 0),
        ("getTextHeight", 0),
        ("getEscWidthX", 0),
        ("getEscWidthY", 0),
        ("getEscHeightX", 0),
        ("getEscHeightY", 0),
        ("getGlyphDrawRect", 0),
        ("getList", 0),
        ("mapPrerenderedFont", 0),
        ("unmapPrerenderedFont", 0),
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

const WAVE_SOUND_BUFFER_METHODS: &[NativeMethodSpec] = &[
    ("open", 0),
    ("play", 0),
    ("stop", 0),
    ("fade", 0),
    ("stopFade", 0),
    ("setPos", 0),
    ("onStatusChanged", 0),
    ("onFadeCompleted", 0),
    ("onLabel", 0),
    ("freeDirectSound", 0),
    ("getVisBuffer", 0),
    ("setDefaultCounts", 0),
    ("setDefaultAheads", 0),
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

/// VideoOverlay's method floors belong to `native/video.rs` (the module that
/// registers the real handlers and the class surface); the spec only declares
/// the names, so every entry stays unguarded here.
pub(crate) static VIDEO_OVERLAY_CLASS: NativeClassSpec = NativeClassSpec {
    name: "VideoOverlay",
    methods: &[
        ("open", 0),
        ("play", 0),
        ("stop", 0),
        ("close", 0),
        ("setPos", 0),
        ("setSize", 0),
        ("setBounds", 0),
        ("pause", 0),
        ("rewind", 0),
        ("prepare", 0),
        ("setSegmentLoop", 0),
        ("cancelSegmentLoop", 0),
        ("setPeriodEvent", 0),
        ("cancelPeriodEvent", 0),
        ("selectAudioStream", 0),
        ("setMixingLayer", 0),
        ("resetMixingLayer", 0),
        ("onStatusChanged", 0),
        ("onCallbackCommand", 0),
        ("onPeriod", 0),
        ("onFrameUpdate", 0),
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
    methods: &[("recreate", 0)],
    properties: &["interface", "enableD3D", "preferredDrawer"],
    static_methods: &[],
    static_properties: &["dtNone", "dtDrawDib", "dtDBGDI", "dtDBDD", "dtDBD3D"],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(byte: u32) -> f32 {
        byte as f32 / 255.0
    }

    #[test]
    fn argb_color_decodes_the_reference_channel_layout() {
        // `bgcolor`/`bgcolor1`/`bgcolor2` are `tjs_uint32` ARGB values
        // (`wave.cpp:345`/`:348`, filled by `TVPFillARGB` at `:219`): the top
        // byte is alpha, so the reference default `0` is see-through black.
        assert_eq!(
            argb_color(0x7fff8000),
            Color::new(channel(0xff), channel(0x80), channel(0x00), channel(0x7f))
        );
        assert_eq!(argb_color(0), Color::new(0.0, 0.0, 0.0, 0.0));
        assert_eq!(argb_color(0xff000000), Color::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(argb_color(0xffffffff), Color::new(1.0, 1.0, 1.0, 1.0));
        // A negative TJS integer wraps into the same 32 bits a `tjs_uint32`
        // cast keeps.
        assert_eq!(argb_color(-1), Color::new(1.0, 1.0, 1.0, 1.0));
    }

    #[test]
    fn plugin_transition_names_are_unknown_to_the_core_registry() {
        // `resolve_transition_method` tries the core registry first, so a name
        // listed in both tables would never reach its plugin branch.
        for (plugin, names) in PLUGIN_TRANSITION_NAMES {
            assert!(!names.is_empty(), "{plugin} lists no transition names");
            for &name in names.iter() {
                assert!(
                    TransitionMethod::try_from_name(name).is_err(),
                    "`{name}` is in the core registry and in {plugin}'s list too"
                );
            }
        }
    }

    /// Every krkrz native class that declares an empty `finalize` with
    /// `TJS_DECL_EMPTY_FINALIZE_METHOD` (`tjsNative.h:380-383`) and that this
    /// engine installs must answer the member on its class object: scripts
    /// reach it as `global.<Class>.finalize(...)` or through a script
    /// subclass's `super.finalize(...)`, and a missing member aborts the
    /// caller with `Member "finalize" does not exist` -- the fatal shape M141
    /// hit on PARQUET's title screen (`title.ks:13`). A class added to the
    /// port without its empty finalizer fails here, in one place.
    ///
    /// The TJS2-side classes (`Exception` `tjsException.cpp:30`, `Math`
    /// `tjsMath.cpp:108`, `RegExp` `tjsRegExp.cpp:210`, `RandomGenerator`
    /// `tjsRandomGenerator.cpp:267`, `Date` `tjsDate.cpp:51`) are installed by
    /// `krkr-tjs2`; this engine-side list deliberately does not cover them.
    #[test]
    fn every_krkrz_empty_finalize_class_answers_a_finalize_member() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let cases: &[(&str, &str)] = &[
            ("global.Timer", "TimerIntf.cpp:142"),
            ("global.Layer", "LayerIntf.cpp:6715"),
            ("global.Window", "WindowIntf.cpp:751"),
            ("global.AsyncTrigger", "EventIntf.cpp:1287"),
            ("global.MenuItem", "MenuItemIntf.cpp:249 (menu plugin)"),
            ("global.KAGParser", "KAGParser.cpp:3306 (ExtKAGParser)"),
            ("global.WaveSoundBuffer", "WaveIntf.cpp:1017"),
            ("global.VideoOverlay", "VideoOvlIntf.cpp:205"),
            ("global.Font", "LayerIntf.cpp:9903"),
            ("global.Bitmap", "BitmapIntf.cpp:195"),
            ("global.Rect", "RectItf.cpp:44"),
            ("global.ImageFunction", "ImageFunction.cpp:114"),
            (
                "global.BitmapLayerTreeOwner",
                "BitmapLayerTreeOwner.cpp:200",
            ),
            (
                "global.WaveSoundBuffer.PhaseVocoder",
                "PhaseVocoderFilter.cpp:29",
            ),
            ("global.Window.BasicDrawDevice", "BasicDrawDevice.cpp:855"),
            (
                "global.Window.PassThroughDrawDevice",
                "BasicDrawDevice.cpp:855",
            ),
            ("global.Clipboard", "ClipboardIntf.cpp:41"),
            ("global.Debug", "DebugIntf.cpp:626"),
            ("global.Scripts", "ScriptMgnIntf.cpp:1218"),
            ("global.Storages", "StorageIntf.cpp:1357"),
            ("global.Plugins", "PluginIntf.cpp:27"),
            ("global.System", "SystemIntf.cpp:92"),
        ];
        for (path, anchor) in cases {
            let source = format!("return typeof {path}.finalize;");
            let value = engine
                .execute_script("empty_finalize_probe.tjs", &source)
                .unwrap_or_else(|error| panic!("{path} ({anchor}): probe failed: {error}"));
            assert_eq!(
                value,
                Variant::String("Object".to_string()),
                "{path} ({anchor})"
            );
        }
    }

    /// The classes this registration added a `finalize` member to, called the
    /// way a game calls it: on the class object. Every entry here aborted with
    /// `Member "finalize" does not exist` before the registration.
    #[test]
    fn krkrz_empty_finalize_classes_accept_a_class_object_call() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let cases: &[(&str, &str)] = &[
            ("global.Font.finalize()", "LayerIntf.cpp:9903"),
            ("global.Bitmap.finalize()", "BitmapIntf.cpp:195"),
            ("global.Rect.finalize()", "RectItf.cpp:44"),
            ("global.ImageFunction.finalize()", "ImageFunction.cpp:114"),
            (
                "global.BitmapLayerTreeOwner.finalize()",
                "BitmapLayerTreeOwner.cpp:200",
            ),
            (
                "global.MenuItem.finalize()",
                "MenuItemIntf.cpp:249 (menu plugin)",
            ),
            (
                "global.WaveSoundBuffer.PhaseVocoder.finalize()",
                "PhaseVocoderFilter.cpp:29",
            ),
            (
                "global.Window.BasicDrawDevice.finalize()",
                "BasicDrawDevice.cpp:855",
            ),
            (
                "global.Window.PassThroughDrawDevice.finalize()",
                "BasicDrawDevice.cpp:855",
            ),
            ("global.Clipboard.finalize()", "ClipboardIntf.cpp:41"),
            ("global.Debug.finalize()", "DebugIntf.cpp:626"),
            ("global.Scripts.finalize()", "ScriptMgnIntf.cpp:1218"),
            ("global.Storages.finalize()", "StorageIntf.cpp:1357"),
            ("global.Plugins.finalize()", "PluginIntf.cpp:27"),
            ("global.System.finalize()", "SystemIntf.cpp:92"),
        ];
        for (call, anchor) in cases {
            let source = format!("{call}; return 1;");
            let value = engine
                .execute_script("empty_finalize_call.tjs", &source)
                .unwrap_or_else(|error| panic!("{call} ({anchor}) failed: {error}"));
            assert_eq!(value, Variant::Integer(1), "{call} ({anchor})");
        }
    }

    /// The other reach path: a script subclass whose `finalize` forwards with
    /// `super.finalize(...)` (KAGEX's `KAGMenuItem` shape), invoked through
    /// `invalidate`. The subclass's own `finalize` must survive the native
    /// constructor running on the instance, and `super.finalize(...)` must
    /// land on the empty native member.
    #[test]
    fn empty_finalize_resolves_through_a_script_subclass_super_call() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "empty_finalize_super.tjs",
                r#"
                global.trace = "";
                class EnvFont extends Font {
                    function EnvFont() { super.Font(); }
                    function finalize() { global.trace += "F"; super.finalize(...); }
                }
                class EnvRect extends Rect {
                    function EnvRect() { super.Rect(); }
                    function finalize() { global.trace += "R"; super.finalize(...); }
                }
                class EnvMenuItem extends MenuItem {
                    function EnvMenuItem(owner, caption) { super.MenuItem(owner, caption); }
                    function finalize() { global.trace += "M"; super.finalize(...); }
                }
                var font = new EnvFont();
                var rect = new EnvRect();
                var item = new EnvMenuItem(global, "caption");
                invalidate font; invalidate rect; invalidate item;
                return global.trace + ":" + (isvalid font) + (isvalid rect) + (isvalid item);
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("FRM:000".to_string()));
    }

    /// PARQUET's voice-filter wrapper destroys its session buffers through the
    /// *class object*: `var t2 = global.EnvWaveSoundBuffer; t2.finalize(...)`
    /// (`sysscn/voiceeffect.tjs`, `FilterHackedEnvWaveSoundBuffer.finalize`).
    /// The lookup has to fall through the script class to the empty native
    /// finalizer krkrz declares on `WaveSoundBuffer`
    /// (`TJS_DECL_EMPTY_FINALIZE_METHOD`, `WaveIntf.cpp:1017`); without it the
    /// member call aborts the caller with `Member "finalize" does not exist`
    /// while `title.ks:13` env-initializes the title screen.
    #[test]
    fn wave_sound_buffer_finalize_is_reachable_from_a_script_class_object() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "wave_finalize.tjs",
                r#"
                global.trace = "";
                class EnvWaveSoundBuffer extends WaveSoundBuffer {
                    function EnvWaveSoundBuffer() { super.WaveSoundBuffer(); }
                }
                class FilterHackedEnvWaveSoundBuffer extends EnvWaveSoundBuffer {
                    function FilterHackedEnvWaveSoundBuffer() {
                        super.EnvWaveSoundBuffer();
                    }
                    function finalize() {
                        global.trace += "F";
                        var t2 = global.EnvWaveSoundBuffer;
                        t2.finalize(...);
                    }
                }
                var buffer = new FilterHackedEnvWaveSoundBuffer();
                invalidate buffer;
                return global.trace + ":" + (isvalid buffer);
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("F:0".to_string()));
    }

    /// PARQUET's voice-filter wrapper reads the member first:
    /// `FilterHackedEnvWaveSoundBuffer.searchPlayableStorage` starts with
    /// `filters.clear()` (`sysscn/voiceeffect.tjs`), so a native
    /// `WaveSoundBuffer` that answers `void` aborts the prologue with
    /// `Cannot convert the variable type ((void) to Object)`.
    /// `WaveIntf.cpp:815` creates the array in the NI constructor, the getter
    /// (`:1540-1554`) hands the same array back on every read, and the setter
    /// is denied (`TJS_DENY_NATIVE_PROP_SETTER`, `:1552`): a script write
    /// fails with `TJS_E_ACCESSDENYED` and leaves the instance's array alone.
    #[test]
    fn wave_sound_buffer_filters_is_a_per_instance_array() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "wave_filters.tjs",
                r#"
                class EnvWaveSoundBuffer extends WaveSoundBuffer {
                    function EnvWaveSoundBuffer() { super.WaveSoundBuffer(); }
                }
                class FilterHackedEnvWaveSoundBuffer extends EnvWaveSoundBuffer {
                    function FilterHackedEnvWaveSoundBuffer() {
                        super.EnvWaveSoundBuffer();
                    }
                }
                var first = new FilterHackedEnvWaveSoundBuffer();
                var second = new FilterHackedEnvWaveSoundBuffer();
                var same = first.filters === first.filters;
                var perInstance = !(first.filters === second.filters);
                first.filters.clear();
                first.filters.add("phase");
                var mutated = first.filters.count;
                var denied = (function () {
                    try {
                        first.filters = ["a", "b"];
                        return "no";
                    } catch (e) {
                        return e.message;
                    }
                })();
                return "typeof=" + (typeof second.filters)
                    + " mutated=" + mutated
                    + " denied=" + denied
                    + " kept=" + first.filters.count
                    + " second=" + second.filters.count
                    + " same=" + (same ? "y" : "n")
                    + " perInstance=" + (perInstance ? "y" : "n");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(
                "typeof=Object mutated=1 \
                 denied=Invalid operation for Read-only or Write-only property \
                 kept=1 second=0 same=y perInstance=y"
                    .to_string()
            )
        );
    }

    /// The address of the plane a plugin read view is handed.
    fn layer_plane_address(engine: &mut crate::KrkrEngine, layer: ObjectHandle) -> usize {
        crate::plugin_api::layer::layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            view.pixels.as_ptr() as usize
        })
        .expect("read pointer")
    }

    fn layer_main_pixel(engine: &mut crate::KrkrEngine, expression: &str) -> i64 {
        engine
            .execute_expression("read.tjs", expression)
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    /// A whole-plane `fillRect` writes the layer's own buffer (the plane keeps
    /// its address across the call) and colour 0 **clears** it: the
    /// fresh-image path always built a zeroed plane, so the in-place path must
    /// zero the owned one instead of skipping the fill.
    #[test]
    fn whole_plane_fills_edit_the_owned_buffer_in_place_and_clear_with_zero() {
        use crate::{EngineConfig, KrkrEngine};

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                "global.layer = new Layer();\n\
                 layer.setImageSize(8, 8);\n\
                 layer.fillRect(0, 0, 8, 8, 0xffff0000);\n",
            )
            .expect("script");
        let layer = engine
            .tjs_runtime()
            .global_member("layer")
            .object_handle()
            .expect("layer");
        assert_eq!(
            layer_main_pixel(&mut engine, "layer.getMainPixel(0, 0)"),
            0xff0000
        );
        let address = layer_plane_address(&mut engine, layer);

        engine
            .execute_script("fill.tjs", "layer.fillRect(0, 0, 8, 8, 0xff00ff00);")
            .expect("fill");
        assert_eq!(
            layer_main_pixel(&mut engine, "layer.getMainPixel(0, 0)"),
            0x00ff00
        );
        assert_eq!(
            layer_plane_address(&mut engine, layer),
            address,
            "an owned plane is filled where it lies, not replaced"
        );

        engine
            .execute_script("clear.tjs", "layer.fillRect(0, 0, 8, 8, 0);")
            .expect("clear");
        assert_eq!(
            layer_main_pixel(&mut engine, "layer.getMainPixel(0, 0)"),
            0,
            "colour 0 clears the plane"
        );
        assert_eq!(
            layer_main_pixel(&mut engine, "layer.getMaskPixel(0, 0)"),
            0,
            "colour 0 clears alpha too"
        );
        assert_eq!(
            layer_plane_address(&mut engine, layer),
            address,
            "the clear writes the same owned plane"
        );
    }

    /// A whole-plane fill over a buffer somebody else still holds
    /// (`Layer.assignImages` shares one image between two layers) must not
    /// touch the shared bytes: the destination gets a fresh plane, exactly like
    /// the old unconditional copy.
    #[test]
    fn a_shared_whole_plane_fill_leaves_the_other_holder_untouched() {
        use crate::{EngineConfig, KrkrEngine};

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                "global.src = new Layer();\n\
                 src.setImageSize(2, 1);\n\
                 src.fillRect(0, 0, 2, 1, 0xffff0000);\n\
                 global.dest = new Layer();\n\
                 dest.setImageSize(2, 1);\n\
                 dest.assignImages(src);\n",
            )
            .expect("script");
        let src_id = engine
            .execute_expression("id.tjs", "src.__nativeLayerId")
            .expect("id")
            .to_integer()
            .expect("integer") as u64;
        let dest_id = engine
            .execute_expression("id.tjs", "dest.__nativeLayerId")
            .expect("id")
            .to_integer()
            .expect("integer") as u64;
        let held = |engine: &crate::KrkrEngine, id: u64| {
            engine
                .host()
                .layer_tree()
                .layer(id)
                .expect("layer node")
                .image
                .clone()
                .expect("layer image")
        };
        assert!(
            Arc::ptr_eq(
                &held(&engine, src_id).upload.rgba,
                &held(&engine, dest_id).upload.rgba
            ),
            "assignImages shares one buffer between the two layers"
        );

        engine
            .execute_script("fill.tjs", "dest.fillRect(0, 0, 2, 1, 0xff00ff00);")
            .expect("fill");

        assert_eq!(
            layer_main_pixel(&mut engine, "src.getMainPixel(0, 0)"),
            0xff0000,
            "the holder keeps its bytes"
        );
        assert_eq!(
            layer_main_pixel(&mut engine, "dest.getMainPixel(0, 0)"),
            0x00ff00,
            "the destination committed the fill"
        );
        assert!(
            !Arc::ptr_eq(
                &held(&engine, src_id).upload.rgba,
                &held(&engine, dest_id).upload.rgba
            ),
            "the destination moved to a fresh plane"
        );
    }

    /// M169.  Every `tTJSNI_BaseLayer` method whose reference declares
    /// `if(numparams < N) return TJS_E_BADPARAMCOUNT;` carries that floor at its
    /// registration site (`visual/LayerIntf.cpp`, the per-row anchors are in
    /// the mission notes), so a short call reports `TJS_E_BADPARAMCOUNT`
    /// (-1004) before the handler runs -- while the arities the shipped games
    /// call (`setSize(10, 10)`, `fillRect(0, 0, 10, 10, color)`) still go
    /// through.  `setClip`/`update` accept 0 arguments and are pinned by
    /// `layer_clip_and_update_keep_the_zero_argument_forms`.
    #[test]
    fn layer_method_floors_reject_short_calls() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "layer_floors.tjs",
                r#"
                var layer = new Layer();
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var full = "";
                try {
                    layer.setSize(10, 10);
                    layer.setImageSize(10, 10);
                    layer.fillRect(0, 0, 10, 10, 0xffffffff);
                    layer.getLayerAt(0, 0);
                } catch (e) { full = e.message; }
                return full + "|" + [
                    message(function() { layer.setPos(1); }),
                    message(function() { layer.setSize(1); }),
                    message(function() { layer.setImagePos(1); }),
                    message(function() { layer.setImageSize(1); }),
                    message(function() { layer.getMainPixel(1); }),
                    message(function() { layer.setMainPixel(1, 0); }),
                    message(function() { layer.getMaskPixel(1); }),
                    message(function() { layer.setMaskPixel(1, 0); }),
                    message(function() { layer.getProvincePixel(1); }),
                    message(function() { layer.setProvincePixel(1, 0); }),
                    message(function() { layer.getLayerAt(1); }),
                    message(function() { layer.setCursorPos(1); }),
                    message(function() { layer.loadImages(); }),
                    message(function() { layer.saveLayerImage(); }),
                    message(function() { layer.loadProvinceImage(); }),
                    message(function() { layer.beginTransition(); }),
                    message(function() { layer.convertType(); }),
                    message(function() { layer.fillRect(0, 0, 1, 1); }),
                    message(function() { layer.colorRect(0, 0, 1, 1); }),
                    message(function() { layer.drawText(0, 0, "x"); }),
                    message(function() { layer.drawGlyph(0, 0, 1); }),
                    message(function() { layer.copyRect(0, 0, 1, 1, 1, 1); }),
                    message(function() { layer.operateRect(0, 0, 1, 1, 1, 1); }),
                    message(function() { layer.piledCopy(0, 0, 1, 1, 1, 1); }),
                    message(function() { layer.stretchCopy(0, 0, 1, 1, 1, 1, 1, 1); }),
                    message(function() { layer.operateStretch(0, 0, 1, 1, 1, 1, 1, 1); }),
                    message(function() { layer.affineCopy(0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1); }),
                    message(function() { layer.operateAffine(0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1); }),
                    message(function() { layer.onHitTest(1, 2); }),
                    message(function() { layer.moveBefore(); }),
                    message(function() { layer.moveBehind(); }),
                    message(function() { layer.copy9Patch(); }),
                    message(function() { layer.setAttentionPos(1); }),
                    message(function() { layer.copyToBitmapFromMainImage(); }),
                    message(function() { layer.copyFromBitmapToMainImage(); })
                ].join("|");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(format!("|{}", ["Invalid argument count"; 35].join("|")))
        );
        // The identity from Rust: the dispatch check answers
        // `TJS_E_BADPARAMCOUNT` (-1004) before the handler.
        let error = engine
            .execute_expression("layer_floors.tjs", "layer.setPos(1)")
            .expect_err("a short setPos call must fail");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
    }

    /// The `tTJSNC_Window` floors (`visual/WindowIntf.cpp` :832/:842/:852/:879/
    /// :899/:908 for the real handlers and :861/:870/:889/:926 plus
    /// `visual/win32/WindowImpl.cpp` :2082/:2104/:2117/:2168/:2193 for the
    /// members only a stub serves), with the window call shapes a game uses.
    #[test]
    fn window_method_floors_reject_short_calls() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "window_floors.tjs",
                r#"
                var window = new Window();
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var full = "";
                try {
                    window.setSize(100, 100);
                    window.setPos(20, 30);
                    window.setInnerSize(80, 60);
                    window.setZoom(200, 100);
                    window.add(new Layer(window, null));
                } catch (e) { full = e.message; }
                return full + "|" + [
                    message(function() { window.add(); }),
                    message(function() { window.remove(); }),
                    message(function() { window.setPos(1); }),
                    message(function() { window.setSize(1); }),
                    message(function() { window.setInnerSize(1); }),
                    message(function() { window.setZoom(1); }),
                    message(function() { window.setMinSize(1); }),
                    message(function() { window.setMaxSize(1); }),
                    message(function() { window.setLayerPos(1); }),
                    message(function() { window.postInputEvent(); }),
                    message(function() { window.getTouchPoint(); }),
                    message(function() { window.getTouchVelocity(1, 2, 3); }),
                    message(function() { window.getMouseVelocity(1, 2); }),
                    message(function() { window.registerMessageReceiver(1, 2); }),
                    message(function() { window.findFullScreenCandidates(1, 2, 3, 4); })
                ].join("|");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(format!("|{}", ["Invalid argument count"; 15].join("|")))
        );
    }

    /// The `tTJSNC_Font` floors: every method below `unmapPrerenderedFont` needs
    /// one argument (`LayerIntf.cpp:9919-10032`); the `unmapPrerenderedFont`
    /// test is `numparams < 0` (`:10043`) and therefore never rejects.
    #[test]
    fn font_method_floors_reject_short_calls() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "font_floors.tjs",
                r#"
                var font = new Font();
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var full = "";
                try {
                    font.getTextWidth("A");
                    font.getTextHeight("A");
                    font.unmapPrerenderedFont();
                } catch (e) { full = e.message; }
                return full + "|" + [
                    message(function() { font.getTextWidth(); }),
                    message(function() { font.getTextHeight(); }),
                    message(function() { font.getEscWidthX(); }),
                    message(function() { font.getEscWidthY(); }),
                    message(function() { font.getEscHeightX(); }),
                    message(function() { font.getEscHeightY(); }),
                    message(function() { font.getGlyphDrawRect(); }),
                    message(function() { font.getList(); }),
                    message(function() { font.mapPrerenderedFont(); })
                ].join("|");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(format!("|{}", ["Invalid argument count"; 9].join("|")))
        );
    }

    /// The `WaveSoundBuffer` floors (`WaveIntf.cpp:1036` `open`, `:1069`
    /// `fade`, `:1101` `setPos`; `win32/WaveImpl.cpp:3376` `getVisBuffer`); the
    /// engine plays a movie soundtrack through this surface and PARQUET's KAG
    /// wrappers forward the reference arity.
    #[test]
    fn wave_sound_buffer_method_floors_reject_short_calls() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "wave_floors.tjs",
                r#"
                var buffer = new WaveSoundBuffer();
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var full = "";
                try {
                    WaveSoundBuffer.setDefaultCounts(256);
                    buffer.stop();
                } catch (e) { full = e.message; }
                return full + "|" + [
                    message(function() { buffer.open(); }),
                    message(function() { buffer.fade(1); }),
                    message(function() { buffer.setPos(1, 2); }),
                    message(function() { buffer.getVisBuffer(1, 2); })
                ].join("|");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(format!("|{}", ["Invalid argument count"; 4].join("|")))
        );
    }

    /// The engine `MenuItem` floors M162 measured against krkr2's
    /// `tTJSNC_MenuItem` (`plugins/win32/menu/MenuItemIntf.cpp`): `add` :263,
    /// `insert` :273, `remove` :284 and `popup` :294.  `popup` answers void
    /// behind its floor -- the engine has no window menu to track, which is
    /// M114/M162's finding, not this floor.
    #[test]
    fn menu_item_method_floors_reject_short_calls() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "menu_item_floors.tjs",
                r#"
                var root = new MenuItem(null, "root");
                var child = new MenuItem(null, "child");
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var full = "";
                try {
                    root.add(child);
                } catch (e) { full = e.message; }
                var popup = root.popup(1, 2, 3);
                return full + "|" + (popup === void ? "true" : "false") + "|" + [
                    message(function() { root.add(); }),
                    message(function() { root.insert(child); }),
                    message(function() { root.remove(); }),
                    message(function() { root.popup(1, 2); })
                ].join("|");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(format!("|true|{}", ["Invalid argument count"; 4].join("|")))
        );
    }

    /// The spec-level floors of the classes that are stubs on this engine --
    /// `Rect` (11 rows), `Bitmap` (15) and `BitmapLayerTreeOwner` (29), all
    /// registered through `NativeClassSpec` (`visual/RectItf.cpp`,
    /// `visual/BitmapIntf.cpp`, `visual/BitmapLayerTreeOwner.cpp`).  A short
    /// call fails before the stub records it; a full-arity call still answers
    /// `void`.
    #[test]
    fn stub_class_method_floors_reject_short_calls() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "stub_floors.tjs",
                r#"
                var rect = new Rect(0, 0, 10, 10);
                var bitmap = new Bitmap();
                var owner = new BitmapLayerTreeOwner();
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var full = "";
                try {
                    rect.setSize(10, 10);
                    bitmap.getPixel(1, 1);
                    owner.fireClick(0, 0);
                } catch (e) { full = e.message; }
                return full + "|" + [
                    message(function() { rect.setSize(1); }),
                    message(function() { rect.setOffset(1); }),
                    message(function() { rect.addOffset(1); }),
                    message(function() { rect.set(1, 2, 3); }),
                    message(function() { rect.clip(); }),
                    message(function() { rect.union(); }),
                    message(function() { rect.intersects(); }),
                    message(function() { rect.included(); }),
                    message(function() { rect.includedPos(1); }),
                    message(function() { rect.equal(); }),
                    message(function() { bitmap.getPixel(1); }),
                    message(function() { bitmap.setPixel(1, 2); }),
                    message(function() { bitmap.getMaskPixel(1); }),
                    message(function() { bitmap.setMaskPixel(1, 2); }),
                    message(function() { bitmap.setSize(1); }),
                    message(function() { bitmap.copyFrom(); }),
                    message(function() { bitmap.save(); }),
                    message(function() { bitmap.load(); }),
                    message(function() { bitmap.loadAsync(); }),
                    message(function() { bitmap.loadHeader(); }),
                    message(function() { bitmap.getSaveOption(); }),
                    message(function() { owner.fireClick(1); }),
                    message(function() { owner.fireDoubleClick(1); }),
                    message(function() { owner.fireMouseDown(1, 2, 3); }),
                    message(function() { owner.fireMouseUp(1, 2, 3); }),
                    message(function() { owner.fireMouseMove(1, 2); }),
                    message(function() { owner.fireMouseWheel(1, 2, 3); }),
                    message(function() { owner.fireTouchDown(1, 2, 3, 4); }),
                    message(function() { owner.fireTouchUp(1, 2, 3, 4); }),
                    message(function() { owner.fireTouchMove(1, 2, 3, 4); }),
                    message(function() { owner.fireTouchScaling(1, 2, 3, 4); }),
                    message(function() { owner.fireTouchRotate(1, 2, 3, 4, 5); }),
                    message(function() { owner.fireKeyDown(1); }),
                    message(function() { owner.fireKeyUp(1); }),
                    message(function() { owner.fireKeyPress(); }),
                    message(function() { owner.fireDisplayRotate(1, 2, 3, 4); })
                ].join("|");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(format!("|{}", ["Invalid argument count"; 36].join("|")))
        );
    }

    /// `ImageFunction`'s two real handlers floor at six (`drawText`,
    /// `ImageFunction.cpp:829`) and five (`drawGlyph`, `:915`) arguments, and
    /// the stub members carry their own rows from the same file.  The
    /// layer-target form the engine forwards (`ImageFunction.drawText(layer,
    /// ...)`, six arguments) still reaches the layer handler.
    #[test]
    fn image_function_method_floors_reject_short_calls() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "image_function_floors.tjs",
                r#"
                var image = new ImageFunction();
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var full = "";
                try {
                    image.drawText(null, 0, 0, "x", 0, 0);
                    image.drawGlyph(null, 0, 0, 0, 0);
                } catch (e) { full = e.message; }
                return full + "|" + [
                    message(function() { image.drawText(null, 0, 0, "x", 0); }),
                    message(function() { image.drawGlyph(null, 0, 0, 0); }),
                    message(function() { image.operateAffine(0, 0, 1, 1, 0, 0, 1, 1, 0, 0); }),
                    message(function() { image.operateRect(0, 0, 1, 1, 1); }),
                    message(function() { image.operateStretch(0); }),
                    message(function() { image.flipLR(); }),
                    message(function() { image.flipUD(); }),
                    message(function() { image.adjustGamma(); }),
                    message(function() { image.doBoxBlur(); }),
                    message(function() { image.doGrayScale(); }),
                    message(function() { image.fillRect(0); }),
                    message(function() { image.colorRect(0); })
                ].join("|");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(format!("|{}", ["Invalid argument count"; 12].join("|")))
        );
    }

    /// The two members whose reference shape is *0 arguments, or at least N*:
    /// `Layer.setClip` (0 resets the clip rect, >= 4 sets it,
    /// `LayerIntf.cpp:7001/7010`) and `Layer.update` (0 updates the whole
    /// layer, `:7645/7652`).  They keep their handler checks instead of a
    /// registration floor, so the zero-argument calls the engine's own
    /// transition tests drive (`dest.update()`) and the clip reset stay valid
    /// while 1..3 arguments are still rejected.
    #[test]
    fn layer_clip_and_update_keep_the_zero_argument_forms() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "layer_exceptions.tjs",
                r#"
                var layer = new Layer();
                layer.setImageSize(4, 4);
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var zero = message(function() { layer.setClip(); layer.update(); });
                var four = message(function() { layer.setClip(0, 0, 4, 4); });
                return zero + "|" + four + "|" + [
                    message(function() { layer.setClip(1); }),
                    message(function() { layer.setClip(1, 2, 3); })
                ].join("|");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("||Invalid argument count|Invalid argument count".to_string())
        );
    }

    /// The other half of the floor contract: a call at exactly the reference's
    /// argument count must not be rejected as a short call.  A floor that is
    /// too high (a mistyped N) is the failure mode that would break working
    /// game scripts, so every floored member is called once here at the count
    /// its reference row declares and any `Invalid argument count` answer is
    /// reported with the member's name.  A handler that fails for any *other*
    /// reason (a detached layer, a missing storage, a stub's own limits)
    /// counts as accepted -- this test is about the declaration, not the
    /// implementation behind it.
    #[test]
    fn reference_arity_calls_are_not_rejected() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "floor_exactness.tjs",
                r#"
                var layer = new Layer();
                layer.setImageSize(10, 10);
                var source = new Layer();
                source.setImageSize(4, 4);
                var window = new Window();
                var added = new Layer(window, null);
                var font = new Font();
                var wave = new WaveSoundBuffer();
                var image = new ImageFunction();
                var rect = new Rect(0, 0, 10, 10);
                var bitmap = new Bitmap();
                var owner = new BitmapLayerTreeOwner();
                var root = new MenuItem(null, "root");
                var child = new MenuItem(null, "child");
                var problems = "";
                function check(name, body) {
                    try {
                        body();
                    } catch (e) {
                        if (e.message === "Invalid argument count") {
                            problems += name + "; ";
                        }
                    }
                }
                check("Layer.setPos", function() { layer.setPos(0, 0); });
                check("Layer.setSize", function() { layer.setSize(10, 10); });
                check("Layer.setImagePos", function() { layer.setImagePos(0, 0); });
                check("Layer.setImageSize", function() { layer.setImageSize(10, 10); });
                check("Layer.getMainPixel", function() { layer.getMainPixel(0, 0); });
                check("Layer.setMainPixel", function() { layer.setMainPixel(0, 0, 0xffffffff); });
                check("Layer.getMaskPixel", function() { layer.getMaskPixel(0, 0); });
                check("Layer.setMaskPixel", function() { layer.setMaskPixel(0, 0, 128); });
                check("Layer.getProvincePixel", function() { layer.getProvincePixel(0, 0); });
                check("Layer.setProvincePixel", function() { layer.setProvincePixel(0, 0, 0); });
                check("Layer.getLayerAt", function() { layer.getLayerAt(0, 0); });
                check("Layer.setCursorPos", function() { layer.setCursorPos(0, 0); });
                check("Layer.loadImages", function() { layer.loadImages("none"); });
                check("Layer.saveLayerImage", function() { layer.saveLayerImage("none"); });
                check("Layer.loadProvinceImage", function() { layer.loadProvinceImage("none"); });
                check("Layer.beginTransition", function() { layer.beginTransition("none"); });
                check("Layer.convertType", function() { layer.convertType(dfAlpha); });
                check("Layer.fillRect", function() { layer.fillRect(0, 0, 10, 10, 0xffffffff); });
                check("Layer.colorRect", function() { layer.colorRect(0, 0, 10, 10, 0xffffffff); });
                check("Layer.drawText", function() { layer.drawText(0, 0, "x", 0xffffff); });
                check("Layer.drawGlyph", function() { layer.drawGlyph(0, 0, "x", 0xffffff); });
                check("Layer.copyRect", function() { layer.copyRect(0, 0, source, 0, 0, 4, 4); });
                check("Layer.operateRect", function() { layer.operateRect(0, 0, source, 0, 0, 4, 4); });
                check("Layer.piledCopy", function() { layer.piledCopy(0, 0, source, 0, 0, 4, 4); });
                check("Layer.stretchCopy", function() { layer.stretchCopy(0, 0, 4, 4, source, 0, 0, 2, 2); });
                check("Layer.operateStretch", function() { layer.operateStretch(0, 0, 4, 4, source, 0, 0, 2, 2); });
                check("Layer.affineCopy", function() { layer.affineCopy(source, 1, 0, 1, 1, false, 0, 0, 1, 0, 0, 1); });
                check("Layer.operateAffine", function() { layer.operateAffine(source, 1, 0, 1, 1, false, 0, 0, 1, 0, 0, 1); });
                check("Layer.onHitTest", function() { layer.onHitTest(0, 0, 1); });
                check("Layer.moveBefore", function() { layer.moveBefore(0); });
                check("Layer.moveBehind", function() { layer.moveBehind(0); });
                check("Layer.copy9Patch", function() { layer.copy9Patch(0); });
                check("Layer.setAttentionPos", function() { layer.setAttentionPos(0, 0); });
                check("Layer.copyToBitmapFromMainImage", function() { layer.copyToBitmapFromMainImage(0); });
                check("Layer.copyFromBitmapToMainImage", function() { layer.copyFromBitmapToMainImage(0); });
                check("Layer.setClip", function() { layer.setClip(); });
                check("Layer.setClip4", function() { layer.setClip(0, 0, 4, 4); });
                check("Layer.update", function() { layer.update(); });
                check("Window.add", function() { window.add(added); });
                check("Window.remove", function() { window.remove(added); });
                check("Window.setPos", function() { window.setPos(0, 0); });
                check("Window.setSize", function() { window.setSize(100, 100); });
                check("Window.setInnerSize", function() { window.setInnerSize(80, 60); });
                check("Window.setZoom", function() { window.setZoom(100, 100); });
                check("Window.setMinSize", function() { window.setMinSize(10, 10); });
                check("Window.setMaxSize", function() { window.setMaxSize(100, 100); });
                check("Window.setLayerPos", function() { window.setLayerPos(0, 0); });
                check("Window.postInputEvent", function() { window.postInputEvent(0); });
                check("Window.getTouchPoint", function() { window.getTouchPoint(0); });
                check("Window.getTouchVelocity", function() { window.getTouchVelocity(0, 0, 0, 0); });
                check("Window.getMouseVelocity", function() { window.getMouseVelocity(0, 0, 0); });
                check("Window.registerMessageReceiver", function() { window.registerMessageReceiver(0, 0, 0); });
                check("Window.findFullScreenCandidates", function() { window.findFullScreenCandidates(0, 0, 0, 0, 0); });
                check("Font.getTextWidth", function() { font.getTextWidth("A"); });
                check("Font.getTextHeight", function() { font.getTextHeight("A"); });
                check("Font.getEscWidthX", function() { font.getEscWidthX("A"); });
                check("Font.getEscWidthY", function() { font.getEscWidthY("A"); });
                check("Font.getEscHeightX", function() { font.getEscHeightX("A"); });
                check("Font.getEscHeightY", function() { font.getEscHeightY("A"); });
                check("Font.getGlyphDrawRect", function() { font.getGlyphDrawRect("A"); });
                check("Font.getList", function() { font.getList("A"); });
                check("Font.mapPrerenderedFont", function() { font.mapPrerenderedFont(font); });
                check("WaveSoundBuffer.open", function() { wave.open("none"); });
                check("WaveSoundBuffer.fade", function() { wave.fade(0, 1); });
                check("WaveSoundBuffer.setPos", function() { wave.setPos(0, 0, 0); });
                check("WaveSoundBuffer.getVisBuffer", function() { wave.getVisBuffer(0, 0, 0); });
                check("ImageFunction.drawText", function() { image.drawText(null, 0, 0, "x", 0, 0); });
                check("ImageFunction.drawGlyph", function() { image.drawGlyph(null, 0, 0, 0, 0); });
                check("ImageFunction.operateAffine", function() { image.operateAffine(0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 0); });
                check("ImageFunction.operateRect", function() { image.operateRect(0, 0, 1, 1, 1, 1); });
                check("ImageFunction.operateStretch", function() { image.operateStretch(0, 0); });
                check("ImageFunction.flipLR", function() { image.flipLR(0); });
                check("ImageFunction.flipUD", function() { image.flipUD(0); });
                check("ImageFunction.adjustGamma", function() { image.adjustGamma(0); });
                check("ImageFunction.doBoxBlur", function() { image.doBoxBlur(0); });
                check("ImageFunction.doGrayScale", function() { image.doGrayScale(0); });
                check("ImageFunction.fillRect", function() { image.fillRect(0, 0); });
                check("ImageFunction.colorRect", function() { image.colorRect(0, 0); });
                check("Rect.setSize", function() { rect.setSize(10, 10); });
                check("Rect.setOffset", function() { rect.setOffset(0, 0); });
                check("Rect.addOffset", function() { rect.addOffset(0, 0); });
                check("Rect.set", function() { rect.set(0, 0, 10, 10); });
                check("Rect.clip", function() { rect.clip(rect); });
                check("Rect.union", function() { rect.union(rect); });
                check("Rect.intersects", function() { rect.intersects(rect); });
                check("Rect.included", function() { rect.included(rect); });
                check("Rect.includedPos", function() { rect.includedPos(0, 0); });
                check("Rect.equal", function() { rect.equal(rect); });
                check("Bitmap.getPixel", function() { bitmap.getPixel(0, 0); });
                check("Bitmap.setPixel", function() { bitmap.setPixel(0, 0, 0xffffffff); });
                check("Bitmap.getMaskPixel", function() { bitmap.getMaskPixel(0, 0); });
                check("Bitmap.setMaskPixel", function() { bitmap.setMaskPixel(0, 0, 128); });
                check("Bitmap.setSize", function() { bitmap.setSize(10, 10); });
                check("Bitmap.copyFrom", function() { bitmap.copyFrom(bitmap); });
                check("Bitmap.save", function() { bitmap.save("none"); });
                check("Bitmap.load", function() { bitmap.load("none"); });
                check("Bitmap.loadAsync", function() { bitmap.loadAsync("none"); });
                check("Bitmap.loadHeader", function() { bitmap.loadHeader("none"); });
                check("Bitmap.getSaveOption", function() { bitmap.getSaveOption("none"); });
                check("BitmapLayerTreeOwner.fireClick", function() { owner.fireClick(0, 0); });
                check("BitmapLayerTreeOwner.fireDoubleClick", function() { owner.fireDoubleClick(0, 0); });
                check("BitmapLayerTreeOwner.fireMouseDown", function() { owner.fireMouseDown(0, 0, 1, 0); });
                check("BitmapLayerTreeOwner.fireMouseUp", function() { owner.fireMouseUp(0, 0, 1, 0); });
                check("BitmapLayerTreeOwner.fireMouseMove", function() { owner.fireMouseMove(0, 0, 0); });
                check("BitmapLayerTreeOwner.fireMouseWheel", function() { owner.fireMouseWheel(0, 0, 1, 0); });
                check("BitmapLayerTreeOwner.fireTouchDown", function() { owner.fireTouchDown(0, 0, 1, 1, 0); });
                check("BitmapLayerTreeOwner.fireTouchUp", function() { owner.fireTouchUp(0, 0, 1, 1, 0); });
                check("BitmapLayerTreeOwner.fireTouchMove", function() { owner.fireTouchMove(0, 0, 1, 1, 0); });
                check("BitmapLayerTreeOwner.fireTouchScaling", function() { owner.fireTouchScaling(0, 0, 1, 1, 0); });
                check("BitmapLayerTreeOwner.fireTouchRotate", function() { owner.fireTouchRotate(0, 0, 1, 1, 0, 0); });
                check("BitmapLayerTreeOwner.fireKeyDown", function() { owner.fireKeyDown(0, 1); });
                check("BitmapLayerTreeOwner.fireKeyUp", function() { owner.fireKeyUp(0, 1); });
                check("BitmapLayerTreeOwner.fireKeyPress", function() { owner.fireKeyPress(0); });
                check("BitmapLayerTreeOwner.fireDisplayRotate", function() { owner.fireDisplayRotate(0, 1, 1, 0, 0); });
                check("MenuItem.add", function() { root.add(child); });
                check("MenuItem.insert", function() { root.insert(child, 0); });
                check("MenuItem.remove", function() { root.remove(child); });
                check("MenuItem.popup", function() { root.popup(0, 0, 0); });
                return problems === "" ? "ok" : problems;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("ok".to_string()));
    }

    /// PARQUET's `CustomNameLayer.beginTrans` (`sysscn/msghack.tjs:844-857`)
    /// captures `transFrom` before `processNameText` draws the name text and
    /// assigns `transTo` from the name layer right after, then runs a
    /// `crossfade` between the two. Our `drawText` used to grow the name
    /// layer's image to fit the text (792x53 -> 792x82 at font height 60), so
    /// the layers `beginTransition` compares had different `MainImage` sizes
    /// and the call threw `Transition layer size mismatch 792x82 and 792x53`.
    /// The reference draws through `MainImage->DrawText(ClipRect, ...)`
    /// (`LayerIntf.cpp:4014-4057`), so both stay 792x53 and the transition
    /// starts.
    #[test]
    fn layer_draw_text_keeps_the_image_size_and_starts_a_crossfade() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var name = new Layer();
                name.setImageSize(792, 53);
                name.font.height = 60;
                name.drawText(0, 0, "NAME", 0xffffff, 255);
                var from = new Layer();
                from.setImageSize(792, 53);
                var to = new Layer();
                to.assignImages(name);
                to.setSizeToImageSize();
                global.transition = "";
                try {
                    from.beginTransition("crossfade", 0, to, %[time: 500]);
                    global.transition = "started";
                } catch (e) {
                    global.transition = e.message;
                }
                return name.imageWidth + "x" + name.imageHeight + ":" +
                    from.imageWidth + "x" + from.imageHeight + ":" +
                    to.imageWidth + "x" + to.imageHeight + ":" + global.transition;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("792x53:792x53:792x53:started".to_string())
        );
    }

    /// A draw never changes the plane's size and never loses what it does not
    /// touch: the same 40x60 image is still 40x60 after a draw whose text is
    /// taller than the image, pixels outside the layer's `ClipRect` keep the
    /// white the earlier fill wrote, and pixels inside it took the draw.
    #[test]
    fn layer_draw_text_clips_to_the_clip_rect_without_losing_pixels() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                global.clipLayer = new Layer();
                clipLayer.setImageSize(40, 60);
                clipLayer.fillRect(0, 0, 40, 60, 0xffffffff);
                clipLayer.setClip(0, 0, 8, 60);
                clipLayer.font.height = 60;
                // The glyph's ink sits well below the origin at this size; the
                // negative y brings it into the bitmap so the clip is what
                // trims it.
                clipLayer.drawText(0, -30, "M", 0xff0000, 255);
                return clipLayer.__nativeLayerId + ":" +
                    clipLayer.imageWidth + "x" + clipLayer.imageHeight;
                "#,
            )
            .expect("script");
        let Variant::String(value) = value else {
            panic!("expected string result");
        };
        let (layer_id, size) = value.split_once(':').expect("layer id and size");
        assert_eq!(size, "40x60");
        let layer_id = layer_id.parse::<u64>().expect("layer id");
        let rgba = engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
            .map(|image| image.upload.rgba.as_ref().to_vec())
            .expect("drawText image");
        let pixel = |x: usize, y: usize| {
            let index = (y * 40 + x) * 4;
            [
                rgba[index],
                rgba[index + 1],
                rgba[index + 2],
                rgba[index + 3],
            ]
        };
        // Outside the clip rect: untouched by the draw.
        for y in 0..60 {
            for x in 8..40 {
                assert_eq!(pixel(x, y), [0xff, 0xff, 0xff, 0xff], "({x}, {y})");
            }
        }
        // Inside it, the glyph painted over the fill the earlier `fillRect`
        // wrote.
        assert!(
            (0..8).any(|x| (0..60).any(|y| pixel(x, y) != [0xff, 0xff, 0xff, 0xff])),
            "the clipped draw painted nothing"
        );
    }

    /// No blit changes the destination's image size: `PiledCopy` copies into
    /// the layer's *existing* `MainImage`
    /// (`MainImage->CopyRect(dx, dy, bmp, rect, TVP_BB_COPY_MAIN|TVP_BB_COPY_MASK)`,
    /// `LayerIntf.cpp:4120-4122`), whose size only moves through
    /// `setImageSize`/`setSize`/`LoadImages` (`ImageLayerSizeChanged`
    /// `LayerIntf.cpp:2388`, called from `InternalSetSize` `:1981`). A copy
    /// whose rectangle reaches past the bitmap is clipped by
    /// `tTVPBaseBitmap::CopyRect`'s bound check (`LayerBitmapIntf.cpp:689-745`),
    /// and everything outside it keeps its pixels. Our old `piledCopy` grew the
    /// 10x10 destination to 15x15 for the first copy and to 25x25 for the
    /// second, dropping every pixel the copies did not touch (the grown plane
    /// was zero-filled).
    #[test]
    fn layer_piled_copy_clips_into_the_existing_image() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.visible = true;
                source.setImageSize(20, 20);
                source.fillRect(0, 0, 20, 20, 0xff336699);

                var dest = new Layer();
                dest.setImageSize(10, 10);
                dest.fillRect(0, 0, 10, 10, 0xffff0000);

                var before = dest.imageWidth + "x" + dest.imageHeight + ":" +
                    dest.getMainPixel(0, 0) + ":" + dest.getMaskPixel(0, 0);
                // (5, 5) + 10x10 reaches 15x15 on a 10x10 bitmap.
                dest.piledCopy(5, 5, source, 0, 0, 10, 10);
                var after = dest.imageWidth + "x" + dest.imageHeight + ":" +
                    dest.getMainPixel(0, 0) + ":" + dest.getMaskPixel(0, 0) + ":" +
                    dest.getMainPixel(9, 9) + ":" + dest.getMaskPixel(9, 9);
                // (15, 15) + 10x10 reaches 25x25: entirely outside the bitmap.
                dest.piledCopy(15, 15, source, 0, 0, 10, 10);
                var outside = dest.imageWidth + "x" + dest.imageHeight + ":" +
                    dest.getMainPixel(0, 0) + ":" + dest.getMaskPixel(0, 0) + ":" +
                    dest.getMainPixel(9, 9) + ":" + dest.getMaskPixel(9, 9);
                return before + "|" + after + "|" + outside;
                "#,
            )
            .expect("script");
        // 0x336699 = 3368601: the pixels the first copy reaches took the
        // source, the ones outside its rectangle keep the fill the layer
        // started with, and the far copy changes nothing at all.
        assert_eq!(
            value,
            Variant::String(
                "10x10:16711680:255|10x10:16711680:255:3368601:255|10x10:16711680:255:3368601:255"
                    .to_string()
            )
        );
    }

    /// The `stretchCopy` half of the same rule: `StretchCopy` intersects the
    /// destination rectangle with the layer's `ClipRect` and then resamples
    /// into `MainImage` with `MainImage->StretchBlt(ClipRect, destrect, src,
    /// srcrect, bmCopy, 255, HoldAlpha, type, typeopt)`
    /// (`LayerIntf.cpp:4232-4273`); `tTVPBaseBitmap::StretchBlt` folds the clip
    /// rectangle onto the bitmap and never resizes it
    /// (`LayerBitmapIntf.cpp:1849-1875`), so a stretch that reaches past a
    /// 10x10 image stays 10x10 and loses nothing outside its rectangle.
    #[test]
    fn layer_stretch_copy_clips_into_the_existing_image() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.visible = true;
                source.setImageSize(20, 20);
                source.fillRect(0, 0, 20, 20, 0xff336699);

                var dest = new Layer();
                dest.setImageSize(10, 10);
                dest.fillRect(0, 0, 10, 10, 0xffff0000);

                var before = dest.imageWidth + "x" + dest.imageHeight + ":" +
                    dest.getMainPixel(0, 0) + ":" + dest.getMaskPixel(0, 0);
                dest.stretchCopy(5, 5, 10, 10, source, 0, 0, 20, 20, stNearest);
                var after = dest.imageWidth + "x" + dest.imageHeight + ":" +
                    dest.getMainPixel(0, 0) + ":" + dest.getMaskPixel(0, 0) + ":" +
                    dest.getMainPixel(9, 9) + ":" + dest.getMaskPixel(9, 9);
                return before + "|" + after;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("10x10:16711680:255|10x10:16711680:255:3368601:255".to_string())
        );
    }

    /// The one case these two blits *do* refuse: a destination whose
    /// `MainImage` is gone. `PiledCopy` (`LayerIntf.cpp:4111`), `StretchCopy`
    /// (`:4245`/`:4253`) and `OperateStretch` (`:4417`/`:4423`) all throw
    /// `TVPNotDrawableLayerType` before they look at any rectangle, so a freed
    /// bitmap is not resurrected by a blit that would have fit it.
    /// `OperateStretch` (`:4417`) takes the same check for the operation-mode
    /// entry points of the same resampler.
    #[test]
    fn layer_blits_without_a_destination_image_throw_not_drawable_layer_type() {
        use crate::{EngineConfig, KrkrEngine};

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.setImageSize(2, 2);
                global.dest = new Layer();
                dest.setImageSize(2, 2);
                dest.freeImage();
                "#,
            )
            .expect("script");
        for call in [
            "dest.piledCopy(0, 0, source, 0, 0, 8, 8);",
            "dest.stretchCopy(0, 0, 8, 8, source, 0, 0, 2, 2, stNearest);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("a freed destination image is not drawable");
            assert_eq!(error.message, "Not drawable layer type", "{call}");
        }
    }

    /// A Layer that still owns its bitmap is not a missing source just because
    /// the engine has not materialized that bitmap in the render node yet.
    /// The reference's `MainImage` belongs to the object for its whole life
    /// (`AllocateDefaultImage`, `LayerIntf.cpp:404`/`:2111`), a load decodes
    /// *into* it (`tTJSNI_BaseLayer::LoadImages`, `:2509-2514`) and only
    /// `freeImage` (`DeallocateImage`, `:2079`) clears it, so its wrapper's
    /// `TVPSpecifyLayerOrBitmap` (`:7150`) never fires for a layer that is
    /// merely still loading. Kirakira's bitmap lives in a render node, which
    /// `Invalidate`/KAG retargeting or an `assignImages` from an image-less
    /// source (`:2124-2140`; `copy_layer_images` clears the node) can take
    /// away while the layer keeps its `hasImage` intent. The guard used to
    /// read that node as the object and rejected the still-loading source --
    /// the save-return frame died with `Specify Layer or Bitmap class object`
    /// at `onPaint` while the decode completed a moment later. Resolving
    /// through `layer_main_image` (the `hasImage`/`imageWidth` read) rebuilds
    /// the ctor bitmap, so all seven members blit it, and a *freed* source
    /// keeps M184's error.
    #[test]
    fn layer_blits_accept_a_source_whose_bitmap_the_engine_rebuilds() {
        use crate::{EngineConfig, KrkrEngine};

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.blank = new Layer();
                blank.setImageSize(2, 2);
                blank.freeImage();

                global.source = new Layer();
                source.setImageSize(2, 2);
                source.assignImages(blank);

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                "#,
            )
            .expect("script");
        for call in [
            "dest.copyRect(0, 0, source, 0, 0, 2, 2);",
            "dest.operateRect(0, 0, source, 0, 0, 2, 2, omAlpha);",
            "dest.stretchCopy(0, 0, 2, 2, source, 0, 0, 2, 2, stNearest);",
            "dest.operateStretch(0, 0, 2, 2, source, 0, 0, 2, 2, omAlpha);",
            "dest.affineCopy(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2);",
            "dest.operateAffine(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2, omAlpha);",
            "dest.piledCopy(0, 0, source, 0, 0, 2, 2);",
        ] {
            if let Err(error) = engine.execute_script("inline.tjs", call) {
                panic!("{call}: {error}");
            }
        }
        // The rebuilt bitmap is the ctor's neutral fill (`AllocateDefaultImage`
        // copies the transparent-white holder), so the copy writes bytes: the
        // 4x4 destination starts black and its first pixel comes back white.
        assert_eq!(
            layer_main_pixel(&mut engine, "dest.getMainPixel(0, 0)"),
            0xffffff,
            "copyRect blitted the rebuilt bitmap"
        );

        engine
            .execute_script(
                "inline.tjs",
                "global.freed = new Layer(); freed.setImageSize(2, 2); freed.freeImage();",
            )
            .expect("script");
        let error = engine
            .execute_script(
                "inline.tjs",
                "dest.operateRect(0, 0, freed, 0, 0, 2, 2, omAlpha);",
            )
            .expect_err("a freed source still resolves to no bitmap");
        assert_eq!(error.message, "Specify Layer or Bitmap class object");
        let error = engine
            .execute_script("inline.tjs", "dest.piledCopy(0, 0, freed, 0, 0, 2, 2);")
            .expect_err("a freed source has no bitmap to pile");
        assert_eq!(error.message, "Source layer has no image");
    }

    /// A Layer a *script* invalidated is not a live blit source.  The reference
    /// keeps the class instance hosted (`Invalidate` never removes it), but
    /// `DeallocateImage` (`LayerIntf.cpp:2079`) frees its image and `_Finalize`
    /// deleted the object's members (`tjsObject.cpp:467`), so the wrapper that
    /// asks the source for `GetMainImage()` finds NULL: the six Layer-or-Bitmap
    /// wrappers report `TVPSpecifyLayerOrBitmap` (`LayerIntf.cpp:7150`
    /// `copyRect`, `:7234` `operateRect`, `:7289` `stretchCopy`, `:7343`
    /// `operateStretch`, `:7410` `affineCopy`, `:7484` `operateAffine`) and
    /// `PiledCopy` reports `TVPSourceLayerHasNoImage` (`:4112`).  Kirakira
    /// models invalidation by dropping the instance from the host table, so the
    /// same wrappers answer the same refusals through the dropped instance --
    /// and no member of the corpse can be read to revive it (the object is
    /// invalid, `tjsObject.cpp:1402`).
    ///
    /// Residual model difference (reported): `PiledCopy` cannot tell a corpse
    /// from an object that was never a Layer, because the instance that
    /// carried the class identity is gone, so it reports its *wrapper* error
    /// (`TVPSpecifyLayer`, `:7104`) where the reference reports the missing
    /// bitmap.
    #[test]
    fn layer_blits_reject_a_source_the_script_invalidated() {
        use crate::{EngineConfig, KrkrEngine};

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.fresh = function() {
                    var layer = new Layer();
                    layer.setImageSize(2, 2);
                    invalidate layer;
                    return layer;
                };

                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.fillRect(0, 0, 4, 4, 0xff000000);
                "#,
            )
            .expect("script");
        // Every wrapper resolves the source before its blit runs, so each call
        // gets an invalidated source of its own; none of them resolves to a
        // bitmap the reference would not have.
        for call in [
            "dest.copyRect(0, 0, detached, 0, 0, 2, 2);",
            "dest.operateRect(0, 0, detached, 0, 0, 2, 2, omAlpha);",
            "dest.stretchCopy(0, 0, 2, 2, detached, 0, 0, 2, 2, stNearest);",
            "dest.operateStretch(0, 0, 2, 2, detached, 0, 0, 2, 2, omAlpha);",
            "dest.affineCopy(detached, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2);",
            "dest.operateAffine(detached, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2, omAlpha);",
        ] {
            engine
                .execute_script("inline.tjs", "global.detached = fresh();")
                .expect("fresh invalidated source");
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("an invalidated source has no bitmap to hand the blit");
            assert_eq!(
                error.message, "Specify Layer or Bitmap class object",
                "{call}"
            );
        }
        // The corpse's own member protocol is what died with the invalidation:
        // the source cannot be read, written or called back to life.
        engine
            .execute_script("inline.tjs", "global.detached = fresh();")
            .expect("fresh invalidated source");
        for call in [
            "return detached.hasImage;",
            "detached.hasImage = 1;",
            "detached.fillRect(0, 0, 2, 2, 0xffffffff);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("an invalidated layer is inert");
            assert_eq!(error.message, "The object is already invalidated", "{call}");
        }
        engine
            .execute_script("inline.tjs", "global.detached = fresh();")
            .expect("fresh invalidated source");
        let error = engine
            .execute_script("inline.tjs", "dest.piledCopy(0, 0, detached, 0, 0, 2, 2);")
            .expect_err("an invalidated source has no bitmap to pile");
        assert_eq!(error.message, "Specify Layer class object");

        // A freed layer keeps M184's refusal: `freeImage` recorded
        // `hasImage == 0` (`LayerIntf.cpp:2237` reads `MainImage != NULL`), so
        // the blit finds no bitmap -- through the wrapper for the six
        // Layer-or-Bitmap calls and through `PiledCopy`'s own check for the
        // pile, exactly as the reference distinguishes them.
        engine
            .execute_script(
                "inline.tjs",
                "global.freed = new Layer(); freed.setImageSize(2, 2); freed.freeImage();",
            )
            .expect("script");
        let error = engine
            .execute_script(
                "inline.tjs",
                "dest.operateRect(0, 0, freed, 0, 0, 2, 2, omAlpha);",
            )
            .expect_err("a freed source still resolves to no bitmap");
        assert_eq!(error.message, "Specify Layer or Bitmap class object");
        let error = engine
            .execute_script("inline.tjs", "dest.piledCopy(0, 0, freed, 0, 0, 2, 2);")
            .expect_err("a freed source has no bitmap to pile");
        assert_eq!(error.message, "Source layer has no image");

        // `ensure_native_layer_attached` only rebuilds an instance the Layer
        // ctor registered, so nothing that is not a Layer starts resolving.
        for call in [
            "dest.operateRect(0, 0, void, 0, 0, 2, 2, omAlpha);",
            "dest.operateRect(0, 0, new Window(), 0, 0, 2, 2, omAlpha);",
            "dest.operateRect(0, 0, new Bitmap(), 0, 0, 2, 2, omAlpha);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("a non-Layer source cannot resolve to a bitmap");
            assert_eq!(
                error.message, "Specify Layer or Bitmap class object",
                "{call}"
            );
        }
        for call in [
            "dest.piledCopy(0, 0, void, 0, 0, 2, 2);",
            "dest.piledCopy(0, 0, new Bitmap(), 0, 0, 2, 2);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("piledCopy requires a Layer source");
            assert_eq!(error.message, "Specify Layer class object", "{call}");
        }
    }

    /// The official TJS blit wrappers resolve their source argument before the
    /// blit runs and report `TVPSpecifyLayerOrBitmap` ("Specify Layer or
    /// Bitmap class object", `string_table_en.rc:121`) when it yields no
    /// bitmap: `LayerIntf.cpp:7150` `copyRect`, `:7234` `operateRect`, `:7289`
    /// `stretchCopy`, `:7343` `operateStretch`, `:7410` `affineCopy`,
    /// `:7484` `operateAffine`. A Layer whose main image was freed and that
    /// carries no province plane resolves to nothing, so every one of the six
    /// reports the error where the port used to return `Ok(Void)` silently.
    /// `copyRect` alone accepts a province-only source in its wrapper
    /// (`:7136-7137`), which this test does not give it.
    #[test]
    fn layer_blits_reject_a_source_without_an_image() {
        use crate::{EngineConfig, KrkrEngine};

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.setImageSize(2, 2);
                source.freeImage();
                global.dest = new Layer();
                dest.setImageSize(4, 4);
                "#,
            )
            .expect("script");
        for call in [
            "dest.copyRect(0, 0, source, 0, 0, 2, 2);",
            "dest.operateRect(0, 0, source, 0, 0, 2, 2, omAlpha);",
            "dest.stretchCopy(0, 0, 2, 2, source, 0, 0, 2, 2, stNearest);",
            "dest.operateStretch(0, 0, 2, 2, source, 0, 0, 2, 2, omAlpha);",
            "dest.affineCopy(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2);",
            "dest.operateAffine(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2, omAlpha);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("an image-less source has no bitmap to blit");
            assert_eq!(
                error.message, "Specify Layer or Bitmap class object",
                "{call}"
            );
        }
        // `piledCopy`'s wrapper checks the *class* first and its method the
        // destination bitmap before the source one (`LayerIntf.cpp:7106`,
        // `:4111-4112`), so the image-less source still reports the source
        // error M26 pinned.
        let error = engine
            .execute_script("inline.tjs", "dest.piledCopy(0, 0, source, 0, 0, 2, 2);")
            .expect_err("an image-less source cannot be piled");
        assert_eq!(error.message, "Source layer has no image");
    }

    /// The province-only distinction: `copyRect`'s wrapper resolves the
    /// source's main image *or* its province plane (`LayerIntf.cpp:7136-7137`)
    /// and throws only when both are NULL (`:7150`), while the other five --
    /// `operateRect` included (`:7213-7222`, throw `:7234`) -- look at the
    /// main image alone. So a Layer with a province plane but no main image is
    /// accepted by `copyRect`: on a `dfProvince` destination it copies that
    /// plane (`:4179-4195`), and on a main-image face the method itself has no
    /// source bitmap and reports `TVPSourceLayerHasNoImage` (`:4160`). The
    /// other five report `TVPSpecifyLayerOrBitmap` before their method runs.
    /// The plane is built with `setProvincePixel`, which allocates it when
    /// absent (`AllocateProvinceImage`, `:2647-2663`), after `freeImage` has
    /// cleared both planes.
    #[test]
    fn layer_copy_rect_accepts_a_province_only_source() {
        use crate::{EngineConfig, KrkrEngine};

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(2, 2);
                source.freeImage();
                source.setProvincePixel(0, 0, 0x80);

                var dest = new Layer();
                dest.setImageSize(2, 2);

                var provinceDest = new Layer();
                provinceDest.setImageSize(2, 2);
                provinceDest.face = 3; // dfProvince
                provinceDest.copyRect(0, 0, source, 0, 0, 2, 2);
                return provinceDest.getProvincePixel(0, 0);
                "#,
            )
            .expect("a province-only source is accepted by copyRect");
        assert_eq!(value, krkr_tjs2::runtime::Variant::Integer(0x80));

        let error = engine
            .execute_script("inline.tjs", "dest.copyRect(0, 0, source, 0, 0, 2, 2);")
            .expect_err("a main-image face has no source bitmap");
        assert_eq!(error.message, "Source layer has no image");
        for call in [
            "dest.operateRect(0, 0, source, 0, 0, 2, 2, omAlpha);",
            "dest.stretchCopy(0, 0, 2, 2, source, 0, 0, 2, 2, stNearest);",
            "dest.operateStretch(0, 0, 2, 2, source, 0, 0, 2, 2, omAlpha);",
            "dest.affineCopy(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2);",
            "dest.operateAffine(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2, omAlpha);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("these wrappers never look at the province plane");
            assert_eq!(
                error.message, "Specify Layer or Bitmap class object",
                "{call}"
            );
        }
    }

    /// A source argument that is not a Layer at all: `copyRect` and the other
    /// five wrappers report `TVPSpecifyLayerOrBitmap` for a void argument
    /// (`clo.Object` is null, `LayerIntf.cpp:7127-7150`) and for any other
    /// native class. A `Bitmap` object lands here too: the reference's
    /// `tTJSNC_Bitmap` fallback (`:7140-7148`) has nothing to resolve in this
    /// engine -- `Bitmap` is a spec-only placeholder with no bitmap payload
    /// (`install_bitmap_native_properties`) -- so it reports the same error
    /// rather than silently dropping the blit. Accepting a Bitmap source is a
    /// documented deviation, owned by the open `20260912-layer-leftovers`
    /// finding (`...bitmap-arguments-unsupported-and-missing-source`), not a
    /// behaviour this test claims the reference has. `piledCopy` requires a
    /// Layer outright and reports `TVPSpecifyLayer` ("Specify Layer class
    /// object", `string_table_en.rc:120`; `:7095-7108`).
    #[test]
    fn layer_blits_reject_a_source_that_is_not_a_layer() {
        use crate::{EngineConfig, KrkrEngine};

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                "global.dest = new Layer(); dest.setImageSize(4, 4);",
            )
            .expect("script");
        for call in [
            "dest.copyRect(0, 0, void, 0, 0, 2, 2);",
            "dest.operateRect(0, 0, void, 0, 0, 2, 2, omAlpha);",
            "dest.stretchCopy(0, 0, 2, 2, void, 0, 0, 2, 2, stNearest);",
            "dest.operateStretch(0, 0, 2, 2, void, 0, 0, 2, 2, omAlpha);",
            "dest.affineCopy(void, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2);",
            "dest.operateAffine(void, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2, omAlpha);",
            "dest.copyRect(0, 0, new Window(), 0, 0, 2, 2);",
            "dest.copyRect(0, 0, new Bitmap(), 0, 0, 2, 2);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("a non-Layer source cannot resolve to a bitmap");
            assert_eq!(
                error.message, "Specify Layer or Bitmap class object",
                "{call}"
            );
        }
        for call in [
            "dest.piledCopy(0, 0, void, 0, 0, 2, 2);",
            "dest.piledCopy(0, 0, new Bitmap(), 0, 0, 2, 2);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("piledCopy requires a Layer source");
            assert_eq!(error.message, "Specify Layer class object", "{call}");
        }
    }

    /// The wrapper guard runs before every check the blit method makes, so a
    /// call whose source *and* destination have no image reports the source
    /// error: the source resolution (and its throw) happens in the TJS wrapper
    /// (`LayerIntf.cpp:7289` `stretchCopy`), while `Not drawable layer type` is
    /// raised inside the method (`:4245`) the wrapper only calls once a source
    /// resolved. `piledCopy` inverts the pair -- its wrapper only checks that
    /// the source is a Layer, so its method's destination check (`:4111`)
    /// reports first, as M182 pinned.
    #[test]
    fn layer_blits_reject_the_source_before_the_destination() {
        use crate::{EngineConfig, KrkrEngine};

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.source = new Layer();
                source.setImageSize(2, 2);
                source.freeImage();
                global.dest = new Layer();
                dest.setImageSize(2, 2);
                dest.freeImage();
                "#,
            )
            .expect("script");
        for call in [
            "dest.copyRect(0, 0, source, 0, 0, 2, 2);",
            "dest.operateRect(0, 0, source, 0, 0, 2, 2, omAlpha);",
            "dest.stretchCopy(0, 0, 2, 2, source, 0, 0, 2, 2, stNearest);",
            "dest.operateStretch(0, 0, 2, 2, source, 0, 0, 2, 2, omAlpha);",
            "dest.affineCopy(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2);",
            "dest.operateAffine(source, 0, 0, 2, 2, false, 0, 0, 2, 0, 0, 2, omAlpha);",
        ] {
            let error = engine
                .execute_script("inline.tjs", call)
                .expect_err("neither layer has a bitmap");
            assert_eq!(
                error.message, "Specify Layer or Bitmap class object",
                "{call}"
            );
        }
        let error = engine
            .execute_script("inline.tjs", "dest.piledCopy(0, 0, source, 0, 0, 2, 2);")
            .expect_err("neither layer has a bitmap");
        assert_eq!(error.message, "Not drawable layer type");
    }

    /// The guard leaves a resolvable source alone: all six members still copy
    /// and resample the source's pixels exactly as before it was added. Every
    /// destination starts blue, the 2x2 source is red, and only the blitted
    /// rectangles turn red (`stretchCopy` scales 2x2 to 4x4).
    #[test]
    fn layer_blits_with_a_valid_source_still_copy() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var source = new Layer();
                source.setImageSize(2, 2);
                source.fillRect(0, 0, 2, 2, 0xffff0000);

                var copied = new Layer();
                copied.setImageSize(4, 4);
                copied.fillRect(0, 0, 4, 4, 0xff0000ff);
                copied.copyRect(0, 0, source, 0, 0, 2, 2);

                var operated = new Layer();
                operated.setImageSize(4, 4);
                operated.fillRect(0, 0, 4, 4, 0xff0000ff);
                operated.operateRect(0, 0, source, 0, 0, 2, 2, omAlpha);

                var stretched = new Layer();
                stretched.setImageSize(4, 4);
                stretched.fillRect(0, 0, 4, 4, 0xff0000ff);
                stretched.stretchCopy(0, 0, 4, 4, source, 0, 0, 2, 2, stNearest);

                var operatedStretch = new Layer();
                operatedStretch.setImageSize(4, 4);
                operatedStretch.fillRect(0, 0, 4, 4, 0xff0000ff);
                operatedStretch.operateStretch(0, 0, 4, 4, source, 0, 0, 2, 2, omAlpha);

                var affined = new Layer();
                affined.setImageSize(4, 4);
                affined.fillRect(0, 0, 4, 4, 0xff0000ff);
                affined.affineCopy(source, 0, 0, 2, 2, false, 0, 0, 4, 0, 0, 4);

                var operatedAffine = new Layer();
                operatedAffine.setImageSize(4, 4);
                operatedAffine.fillRect(0, 0, 4, 4, 0xff0000ff);
                operatedAffine.operateAffine(source, 0, 0, 2, 2, false, 0, 0, 4, 0, 0, 4, omAlpha);

                return copied.getMainPixel(0, 0) + ":" + copied.getMaskPixel(0, 0) + ":" +
                    operated.getMainPixel(0, 0) + ":" +
                    stretched.getMainPixel(0, 0) + ":" + stretched.getMainPixel(3, 3) + ":" +
                    operatedStretch.getMainPixel(0, 0) + ":" +
                    affined.getMainPixel(0, 0) + ":" +
                    operatedAffine.getMainPixel(0, 0) + ":" +
                    copied.getMainPixel(3, 3);
                "#,
            )
            .expect("script");
        // 0xff0000 = 16711680 (the source), 255 (0x0000ff) is the blue fill
        // the copies did not cover and the full source alpha. The three
        // operate members go through the reference's `omAlpha` functor
        // `d + ((s - d) * a >> 8)` (`blend_functor_c.h:64`, ported in
        // `blend.rs`), so an opaque source over the blue destination lands one
        // step below 0xff0000 (16646144) instead of exactly on it.
        assert_eq!(
            value,
            Variant::String(
                "16711680:255:16646144:16711680:16711680:16646144:16711680:16646144:255"
                    .to_string()
            )
        );
    }

    fn global_object(engine: &crate::KrkrEngine, name: &str) -> krkr_tjs2::runtime::ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    /// The engine mirrors the primary layer into the window's script member
    /// (`Window.add` / `Layer.add` -> `set_window_property_storage`). A script
    /// class that declares that property must keep winning it: KAGEX's
    /// `KAGWindow.primaryLayer` (`MainWindow.tjs:4839`, `sysbase` or the
    /// image-less `_primaryLayer`) stopped being consulted once the first
    /// layer was added, so `kag.primaryLayer` answered `_primaryLayer` and
    /// `temp.piledCopy(0, 0, kag.primaryLayer, ...)` (`custom.tjs:196`) threw
    /// `Source layer has no image` (`scnchart.ks:28`). A plain window keeps
    /// the native value, and the host keeps the engine-side state either way.
    #[test]
    fn window_script_property_primary_layer_wins_over_the_engine_member() {
        use crate::{EngineConfig, KrkrEngine};
        use krkr_tjs2::runtime::Variant;

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                class GameWindow extends Window {
                    function GameWindow() { super.Window(...); }
                    property primaryLayer { getter() { return "SCRIPT"; } }
                }
                global.game = new GameWindow();
                global.gameLayer = new Layer(game, null);
                global.plain = new Window();
                global.plainLayer = new Layer(plain, null);
                return game.primaryLayer + ":" +
                    (game.primaryLayer == gameLayer) + ":" +
                    (plain.primaryLayer == plainLayer);
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("SCRIPT:0:1".to_string()));

        let game = global_object(&engine, "game");
        let game_layer = global_object(&engine, "gameLayer");
        assert_eq!(
            engine.host().native_window_primary_layer(game),
            Some(game_layer),
            "the engine's own view of the primary layer is unchanged"
        );
    }
}
