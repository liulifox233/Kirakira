use std::{collections::BTreeSet, time::Duration};

use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::{KrkrHost, NativeTransitionCompletion};

use super::register_stub_method;

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
    install_methods(runtime, handle, spec.name, spec.methods);
    install_special_methods(runtime, handle, spec.name);
    install_properties(runtime, handle, spec.properties);
    apply_constructor_defaults(runtime, handle, spec.name, &args)?;
    Ok(Variant::Object(handle))
}

fn install_methods(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &'static str,
    methods: &'static [&'static str],
) {
    for method in methods {
        if !matches!(runtime.object_member(handle, method), Variant::Void) {
            continue;
        }
        register_stub_method(runtime, handle, class_name, method);
    }
}

fn install_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    properties: &'static [&'static str],
) {
    for property in properties {
        runtime.set_object_member(handle, *property, Variant::Void);
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
            if let Some(callback) = args.first().filter(|value| !matches!(value, Variant::Void)) {
                runtime.set_object_member(handle, "__callback", callback.clone());
            }
            runtime.host_mut().register_timer(handle);
        }
        "AsyncTrigger" => {
            runtime.set_object_member(handle, "cached", Variant::Integer(0));
            runtime.set_object_member(handle, "mode", Variant::Integer(0));
            if let Some(callback) = args.first().filter(|value| !matches!(value, Variant::Void)) {
                runtime.set_object_member(handle, "__callback", callback.clone());
            }
            runtime.host_mut().register_async_trigger(handle);
        }
        "Window" => {
            runtime.set_object_member(handle, "visible", Variant::Integer(0));
            runtime.set_object_member(handle, "caption", Variant::String(String::new()));
            runtime.set_object_member(handle, "width", Variant::Integer(0));
            runtime.set_object_member(handle, "height", Variant::Integer(0));
            let menu = alloc_menu_item_object(runtime, Some(handle), String::new());
            runtime.set_object_member(handle, "menu", Variant::Object(menu));
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
            let parent_layer = match parent_object {
                Some(parent) => runtime.host().native_layer(parent),
                None => None,
            };
            let is_primary =
                window_object.is_some() && matches!(parent, Variant::Void | Variant::Null);
            let stored_window = window_object
                .map(Variant::Object)
                .unwrap_or_else(|| window.clone());
            let stored_parent = parent_object
                .map(Variant::Object)
                .unwrap_or_else(|| parent.clone());
            let layer_id = runtime.host_mut().register_native_layer(
                handle,
                format!("native:{}", handle.0),
                parent_layer,
                is_primary,
            );
            runtime.set_object_member(handle, "window", stored_window.clone());
            runtime.set_object_member(handle, "parent", stored_parent.clone());
            runtime.set_object_member(handle, "__nativeLayerId", Variant::Integer(layer_id as i64));
            let children = runtime.alloc_array_object(Vec::new());
            runtime.set_object_member(handle, "children", Variant::Object(children));
            runtime.set_object_member(handle, "left", Variant::Integer(0));
            runtime.set_object_member(handle, "top", Variant::Integer(0));
            runtime.set_object_member(handle, "width", Variant::Integer(0));
            runtime.set_object_member(handle, "height", Variant::Integer(0));
            runtime.set_object_member(handle, "imageLeft", Variant::Integer(0));
            runtime.set_object_member(handle, "imageTop", Variant::Integer(0));
            runtime.set_object_member(handle, "imageWidth", Variant::Integer(0));
            runtime.set_object_member(handle, "imageHeight", Variant::Integer(0));
            runtime.set_object_member(handle, "order", Variant::Integer(0));
            runtime.set_object_member(handle, "absoluteOrderMode", Variant::Integer(0));
            runtime.set_object_member(handle, "visible", Variant::Integer(i64::from(is_primary)));
            runtime.set_object_member(handle, "enabled", Variant::Integer(1));
            runtime.set_object_member(handle, "nodeEnabled", Variant::Integer(1));
            runtime.set_object_member(handle, "nodeVisible", Variant::Integer(1));
            runtime.set_object_member(handle, "opacity", Variant::Integer(255));
            runtime.set_object_member(
                handle,
                "type",
                Variant::Integer(if is_primary { 1 } else { 2 }),
            );
            runtime.set_object_member(handle, "face", Variant::Integer(128));
            runtime.set_object_member(handle, "hitType", Variant::Integer(0));
            runtime.set_object_member(
                handle,
                "hitThreshold",
                Variant::Integer(if is_primary { 0 } else { 16 }),
            );
            runtime.set_object_member(handle, "isPrimary", Variant::Integer(i64::from(is_primary)));
            runtime.set_object_member(handle, "cursor", Variant::Integer(0));
            runtime.set_object_member(handle, "hint", Variant::String(String::new()));
            runtime.set_object_member(handle, "showParentHint", Variant::Integer(1));
            let font = construct_native_instance(runtime, &FONT_CLASS, None, Vec::new())?;
            runtime.set_object_member(handle, "font", font);
            if is_primary && let Some(window) = window_object {
                runtime.set_object_member(window, "primaryLayer", Variant::Object(handle));
                runtime.set_object_member(window, "focusedLayer", Variant::Object(handle));
            }
            if let Some(parent) = parent_object {
                let children = ensure_child_array(runtime, parent);
                runtime.array_push(children, Variant::Object(handle));
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
        "WaveSoundBuffer" => {
            runtime.set_object_member(handle, "status", Variant::String("unload".to_string()));
            runtime.set_object_member(handle, "volume", Variant::Integer(100000));
            runtime.set_object_member(handle, "pan", Variant::Integer(0));
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
    if class_name == "MenuItem" {
        install_menu_item_methods(runtime, handle);
    } else if class_name == "Layer" {
        install_layer_methods(runtime, handle);
    } else if class_name == "AsyncTrigger" {
        install_async_trigger_methods(runtime, handle);
    } else if class_name == "Window" {
        install_window_methods(runtime, handle);
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
    Ok(Variant::Void)
}

fn menu_item_clear(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let children = menu_item_children(runtime, this_obj, "MenuItem.clear")?;
    runtime.array_clear(children);
    Ok(Variant::Void)
}

fn menu_item_noop(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn install_window_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "setSize", window_set_size);
    runtime.register_object_native(handle, "setInnerSize", window_set_inner_size);
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
    runtime.set_object_member(window, "width", Variant::Integer(width));
    runtime.set_object_member(window, "height", Variant::Integer(height));
    runtime.set_object_member(window, "innerWidth", Variant::Integer(width));
    runtime.set_object_member(window, "innerHeight", Variant::Integer(height));
}

fn install_layer_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_native_method_preserving_script(runtime, handle, "loadImages", layer_load_images);
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
    register_native_method_preserving_script(runtime, handle, "bringToFront", layer_bring_to_front);
    register_native_method_preserving_script(runtime, handle, "bringToBack", layer_bring_to_back);
    register_native_method_preserving_script(runtime, handle, "assignImages", layer_assign_images);
    register_native_method_preserving_script(
        runtime,
        handle,
        "beginTransition",
        layer_begin_transition,
    );
    register_native_method_preserving_script(runtime, handle, "fillRect", layer_fill_rect);
    register_native_method_preserving_script(runtime, handle, "colorRect", layer_color_rect);
    register_native_method_preserving_script(runtime, handle, "copyRect", layer_copy_rect);
    register_native_method_preserving_script(runtime, handle, "operateRect", layer_operate_rect);
    register_native_method_preserving_script(runtime, handle, "piledCopy", layer_copy_rect);
    register_native_method_preserving_script(runtime, handle, "drawText", layer_draw_text);
    register_native_method_preserving_script(runtime, handle, "getProvincePixel", layer_zero);
    register_native_method_preserving_script(runtime, handle, "update", layer_update);
}

type NativeMethod =
    fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>;

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
    runtime.register_object_native(handle, "trigger", async_trigger_trigger);
    runtime.register_object_native(handle, "cancel", async_trigger_cancel);
}

fn async_trigger_trigger(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj.ok_or_else(|| TjsError::runtime("AsyncTrigger.trigger requires this"))?;
    runtime.host_mut().trigger_async(this);
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
    let visible = match options {
        Some(options) => object_optional_integer(runtime, options, "visible")
            .transpose()?
            .map(|value| value != 0)
            .unwrap_or(true),
        None => true,
    };
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

    let image = runtime.host_mut().load_image_storage(&storage)?;
    let size = image.size();

    match native_layer_id(runtime, this)? {
        Some(layer_id) => {
            if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
                layer.set_image(image);
                layer.visible = visible;
                if let Some(left) = left {
                    layer.left = left as f32;
                }
                if let Some(top) = top {
                    layer.top = top as f32;
                }
                if let Some(width) = width {
                    layer.width = width.max(0) as f32;
                }
                if let Some(height) = height {
                    layer.height = height.max(0) as f32;
                }
                if let Some(opacity) = opacity {
                    layer.opacity = opacity.clamp(0, 255) as u8;
                }
            }
        }
        None => {
            let page = match options {
                Some(options) => object_optional_string(runtime, options, "page")?
                    .unwrap_or_else(|| "back".to_string()),
                None => "back".to_string(),
            };
            let layer_name = match options {
                Some(options) => object_optional_string(runtime, options, "layer")?
                    .unwrap_or_else(|| "base".to_string()),
                None => "base".to_string(),
            };
            runtime
                .host_mut()
                .mutate_kag_layer(&page, &layer_name, |layer| {
                    layer.set_image(image);
                    layer.visible = visible;
                    if let Some(left) = left {
                        layer.left = left as f32;
                    }
                    if let Some(top) = top {
                        layer.top = top as f32;
                    }
                    layer.width = width.map_or(size.width, |width| width.max(0) as f32);
                    layer.height = height.map_or(size.height, |height| height.max(0) as f32);
                    if let Some(opacity) = opacity {
                        layer.opacity = opacity.clamp(0, 255) as u8;
                    }
                });
        }
    }
    sync_layer_image_members(runtime, this, size.width as i64, size.height as i64);
    runtime.set_object_member(this, "visible", Variant::Integer(i64::from(visible)));
    if let Some(left) = left {
        runtime.set_object_member(this, "left", Variant::Integer(left));
    }
    if let Some(top) = top {
        runtime.set_object_member(this, "top", Variant::Integer(top));
    }
    if let Some(width) = width {
        runtime.set_object_member(this, "width", Variant::Integer(width.max(0)));
    }
    if let Some(height) = height {
        runtime.set_object_member(this, "height", Variant::Integer(height.max(0)));
    }
    if let Some(opacity) = opacity {
        runtime.set_object_member(this, "opacity", Variant::Integer(opacity.clamp(0, 255)));
    }
    Ok(Variant::Void)
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

fn source_object(value: &Variant) -> Option<ObjectHandle> {
    match value {
        Variant::Object(handle) => Some(*handle),
        _ => None,
    }
}

fn layer_set_pos(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    let left = optional_integer(&args, 0)?.unwrap_or(0);
    let top = optional_integer(&args, 1)?.unwrap_or(0);
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.left = left as f32;
        layer.top = top as f32;
    }
    runtime.set_object_member(this, "left", Variant::Integer(left));
    runtime.set_object_member(this, "top", Variant::Integer(top));
    Ok(Variant::Void)
}

fn layer_set_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    let width = optional_integer(&args, 0)?.unwrap_or(0).max(0);
    let height = optional_integer(&args, 1)?.unwrap_or(0).max(0);
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.width = width as f32;
        layer.height = height as f32;
    }
    runtime.set_object_member(this, "width", Variant::Integer(width));
    runtime.set_object_member(this, "height", Variant::Integer(height));
    Ok(Variant::Void)
}

fn layer_set_image_pos(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    let left = optional_integer(&args, 0)?.unwrap_or(0);
    let top = optional_integer(&args, 1)?.unwrap_or(0);
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.image_left = left as f32;
        layer.image_top = top as f32;
    }
    runtime.set_object_member(this, "imageLeft", Variant::Integer(left));
    runtime.set_object_member(this, "imageTop", Variant::Integer(top));
    Ok(Variant::Void)
}

fn layer_set_image_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    let width = optional_integer(&args, 0)?.unwrap_or(0).max(0);
    let height = optional_integer(&args, 1)?.unwrap_or(0).max(0);
    let image = (width > 0 && height > 0).then(|| {
        runtime.host_mut().create_layer_image(
            width as u32,
            height as u32,
            vec![0; width as usize * height as usize * 4],
        )
    });
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.image_width = width as f32;
        layer.image_height = height as f32;
        if let Some(image) = image {
            layer.image = Some(image);
        } else {
            layer.image = None;
        }
    }
    runtime.set_object_member(this, "imageWidth", Variant::Integer(width));
    runtime.set_object_member(this, "imageHeight", Variant::Integer(height));
    Ok(Variant::Void)
}

fn layer_set_size_to_image_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    let width = runtime
        .object_member(this, "imageWidth")
        .to_integer()?
        .max(0);
    let height = runtime
        .object_member(this, "imageHeight")
        .to_integer()?
        .max(0);
    let replacement_image = if width > 0 && height > 0 {
        let needs_image = runtime
            .host()
            .layer_tree()
            .layer(layer_id)
            .and_then(|layer| layer.image.as_ref())
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
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.image_width = width as f32;
        layer.image_height = height as f32;
        layer.width = width as f32;
        layer.height = height as f32;
        if width == 0 || height == 0 {
            layer.image = None;
        } else if let Some(image) = replacement_image {
            layer.image = Some(image);
        }
    }
    runtime.set_object_member(this, "width", Variant::Integer(width));
    runtime.set_object_member(this, "height", Variant::Integer(height));
    Ok(Variant::Void)
}

fn layer_assign_images(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    let Some(source) = args.first().and_then(variant_object) else {
        return Ok(Variant::Void);
    };
    copy_layer_images(runtime, this, layer_id, source)?;
    Ok(Variant::Void)
}

fn layer_begin_transition(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = this_obj
        .map(|this| runtime.bound_this(this).unwrap_or(this))
        .ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    let source = args
        .get(2)
        .and_then(variant_object)
        .or_else(|| variant_object(&runtime.object_member(this, "comp")));
    let method = args
        .first()
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_else(|| "crossfade".to_string());
    let duration = match args.get(3).and_then(variant_object) {
        Some(options) => object_optional_integer(runtime, options, "time")
            .transpose()?
            .unwrap_or(0),
        None => 0,
    }
    .max(0) as u64;
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
    if let Some(layer_id) = native_layer_id(runtime, this)? {
        if let Some(source) = source {
            copy_layer_images(runtime, this, layer_id, source)?;
        }
        if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
            layer.visible = true;
        }
    }
    runtime.set_object_member(this, "visible", Variant::Integer(1));
    if duration == 0 {
        if let Some(source_layer_id) = source_layer_id
            && let Some(source_layer) = runtime
                .host_mut()
                .layer_tree_mut()
                .layer_mut(source_layer_id)
        {
            source_layer.renderable = false;
        }
        finish_immediate_transition(runtime, this);
    } else {
        runtime.host_mut().begin_native_transition(
            &method,
            Duration::from_millis(duration),
            frozen.0,
            frozen.1,
            suppressed_images,
            NativeTransitionCompletion {
                dest: this,
                source,
                paired_comp,
            },
        );
    }
    Ok(Variant::Void)
}

fn layer_fill_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let Some((x, y, width, height)) = rect_args(&args)? else {
        return Ok(Variant::Void);
    };
    let color = required_integer(&args, 4, "Layer.fillRect color")?;
    let rgba = color_to_rgba(color, None);
    mutate_layer_pixels(runtime, layer_id, |pixels, image_width, image_height| {
        fill_pixels(pixels, image_width, image_height, x, y, width, height, rgba);
    })?;
    Ok(Variant::Void)
}

fn layer_color_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let Some((x, y, width, height)) = rect_args(&args)? else {
        return Ok(Variant::Void);
    };
    let color = required_integer(&args, 4, "Layer.colorRect color")?;
    let opacity = optional_integer(&args, 5)?;
    let rgba = color_to_rgba(color, opacity);
    mutate_layer_pixels(runtime, layer_id, |pixels, image_width, image_height| {
        fill_pixels(pixels, image_width, image_height, x, y, width, height, rgba);
    })?;
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

fn copy_rect_impl(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
    alpha_blend: bool,
) -> Result<Variant> {
    let (this, dest_layer_id) = this_layer_id(runtime, this_obj)?;
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
    let Some(source_layer_id) = native_layer_id(runtime, source_object)? else {
        return Ok(Variant::Void);
    };
    let Some(source_image) = runtime
        .host()
        .layer_tree()
        .layer(source_layer_id)
        .and_then(|layer| layer.image.clone())
    else {
        return Ok(Variant::Void);
    };
    let source_pixels = source_image.upload.rgba.as_ref().to_vec();
    let source_width = source_image.upload.width;
    let source_height = source_image.upload.height;

    mutate_layer_pixels(
        runtime,
        dest_layer_id,
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
    Ok(Variant::Void)
}

fn layer_draw_text(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (this, layer_id) = this_layer_id(runtime, this_obj)?;
    if is_province_face(runtime, this) {
        return Ok(Variant::Void);
    }
    let x = optional_integer(&args, 0)?.unwrap_or(0);
    let y = optional_integer(&args, 1)?.unwrap_or(0);
    let text = args
        .get(2)
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let color = optional_integer(&args, 3)?.unwrap_or(0x00ff_ffff);
    let opacity = optional_integer(&args, 4)?;
    let rgba = color_to_rgba(color, opacity);
    mutate_layer_pixels(runtime, layer_id, |pixels, image_width, image_height| {
        let mut cursor_x = x;
        for ch in text.chars() {
            if ch != ' ' && ch != '\t' {
                fill_pixels(pixels, image_width, image_height, cursor_x, y, 7, 12, rgba);
            }
            cursor_x += 8;
        }
    })?;
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
    runtime.host_mut().request_layer_paint(this);
    Ok(Variant::Void)
}

fn layer_zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
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
    dest_layer_id: u64,
    source_object: ObjectHandle,
) -> Result<()> {
    let Some(source_layer_id) = native_layer_id(runtime, source_object)? else {
        return Ok(());
    };
    let Some(source) = runtime.host().layer_tree().layer(source_layer_id).cloned() else {
        return Ok(());
    };

    let mut resized_to_source = false;
    if let Some(dest) = runtime.host_mut().layer_tree_mut().layer_mut(dest_layer_id) {
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
    }

    runtime.set_object_member(
        dest_object,
        "imageLeft",
        Variant::Integer(source.image_left as i64),
    );
    runtime.set_object_member(
        dest_object,
        "imageTop",
        Variant::Integer(source.image_top as i64),
    );
    runtime.set_object_member(
        dest_object,
        "imageWidth",
        Variant::Integer(source.image_width as i64),
    );
    runtime.set_object_member(
        dest_object,
        "imageHeight",
        Variant::Integer(source.image_height as i64),
    );
    if resized_to_source {
        runtime.set_object_member(dest_object, "width", Variant::Integer(source.width as i64));
        runtime.set_object_member(
            dest_object,
            "height",
            Variant::Integer(source.height as i64),
        );
    }
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

fn finish_immediate_transition(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) {
    runtime.set_object_member(layer, "inTransition", Variant::Integer(0));
    let Variant::Object(window) = runtime.object_member(layer, "window") else {
        return;
    };
    let Ok(trans_count) = runtime.object_member(window, "transCount").to_integer() else {
        return;
    };
    runtime.set_object_member(
        window,
        "transCount",
        Variant::Integer(trans_count.saturating_sub(1).max(0)),
    );
}

fn optional_integer(args: &[Variant], index: usize) -> Result<Option<i64>> {
    args.get(index)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_integer)
        .transpose()
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
    runtime
        .object_member(layer, "face")
        .to_integer()
        .is_ok_and(|face| face == 3)
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

fn mutate_layer_pixels<F>(runtime: &mut Runtime<KrkrHost>, layer_id: u64, mutate: F) -> Result<()>
where
    F: FnOnce(&mut [u8], u32, u32),
{
    let Some(layer) = runtime.host().layer_tree().layer(layer_id).cloned() else {
        return Ok(());
    };
    let width = layer
        .image
        .as_ref()
        .map(|image| image.upload.width)
        .unwrap_or_else(|| layer.image_width.max(layer.width).max(1.0) as u32)
        .max(1);
    let height = layer
        .image
        .as_ref()
        .map(|image| image.upload.height)
        .unwrap_or_else(|| layer.image_height.max(layer.height).max(1.0) as u32)
        .max(1);
    let mut pixels = layer
        .image
        .as_ref()
        .filter(|image| image.upload.width == width && image.upload.height == height)
        .map(|image| image.upload.rgba.as_ref().to_vec())
        .unwrap_or_else(|| vec![0; width as usize * height as usize * 4]);

    mutate(&mut pixels, width, height);

    let image = runtime.host_mut().create_layer_image(width, height, pixels);
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.image = Some(image);
        layer.image_width = width as f32;
        layer.image_height = height as f32;
        if layer.width <= 0.0 {
            layer.width = width as f32;
        }
        if layer.height <= 0.0 {
            layer.height = height as f32;
        }
    }
    Ok(())
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
    for py in y0..y1 {
        for px in x0..x1 {
            let index = ((py * image_width + px) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&rgba);
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
    for row in 0..height {
        let src_y = sy + row;
        let dest_y = dy + row;
        if src_y < 0 || dest_y < 0 || src_y >= source_height as i64 || dest_y >= dest_height as i64
        {
            continue;
        }
        for col in 0..width {
            let src_x = sx + col;
            let dest_x = dx + col;
            if src_x < 0
                || dest_x < 0
                || src_x >= source_width as i64
                || dest_x >= dest_width as i64
            {
                continue;
            }
            let src_index = ((src_y as u32 * source_width + src_x as u32) * 4) as usize;
            let dest_index = ((dest_y as u32 * dest_width + dest_x as u32) * 4) as usize;
            if alpha_blend {
                blend_pixel(
                    &mut dest[dest_index..dest_index + 4],
                    &source[src_index..src_index + 4],
                );
            } else {
                dest[dest_index..dest_index + 4].copy_from_slice(&source[src_index..src_index + 4]);
            }
        }
    }
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
    runtime.set_object_member(this, "imageLeft", Variant::Integer(0));
    runtime.set_object_member(this, "imageTop", Variant::Integer(0));
    runtime.set_object_member(this, "imageWidth", Variant::Integer(width));
    runtime.set_object_member(this, "imageHeight", Variant::Integer(height));
    if runtime
        .object_member(this, "width")
        .to_integer()
        .unwrap_or(0)
        <= 0
    {
        runtime.set_object_member(this, "width", Variant::Integer(width));
    }
    if runtime
        .object_member(this, "height")
        .to_integer()
        .unwrap_or(0)
        <= 0
    {
        runtime.set_object_member(this, "height", Variant::Integer(height));
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
    methods: &["onTimer"],
    properties: &["interval", "enabled", "capacity", "mode"],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static ASYNC_TRIGGER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "AsyncTrigger",
    methods: &["trigger", "cancel", "onFire"],
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
        "moveBefore",
        "moveBehind",
        "bringToBack",
        "bringToFront",
        "saveLayerImage",
        "loadImages",
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
    properties: &["interface"],
    static_methods: &[],
    static_properties: &[],
};
