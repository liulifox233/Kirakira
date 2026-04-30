use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

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
    let handle = this_obj
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object());
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
        if is_event_callback(method)
            && !matches!(runtime.object_member(handle, method), Variant::Void)
        {
            continue;
        }
        register_stub_method(runtime, handle, class_name, method);
    }
}

fn is_event_callback(method: &str) -> bool {
    method.starts_with("on")
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
            let parent_layer = match parent {
                Variant::Object(parent) => runtime.host().native_layer(parent),
                _ => None,
            };
            let layer_id = runtime.host_mut().register_native_layer(
                handle,
                format!("native:{}", handle.0),
                parent_layer,
            );
            runtime.set_object_member(handle, "window", window.clone());
            runtime.set_object_member(handle, "parent", parent.clone());
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
            runtime.set_object_member(handle, "visible", Variant::Integer(0));
            runtime.set_object_member(handle, "enabled", Variant::Integer(1));
            runtime.set_object_member(handle, "nodeEnabled", Variant::Integer(1));
            runtime.set_object_member(handle, "opacity", Variant::Integer(255));
            runtime.set_object_member(handle, "cursor", Variant::Integer(0));
            runtime.set_object_member(handle, "hint", Variant::String(String::new()));
            runtime.set_object_member(handle, "showParentHint", Variant::Integer(1));
            let font = construct_native_instance(runtime, &FONT_CLASS, None, Vec::new())?;
            runtime.set_object_member(handle, "font", font);
            if let Variant::Object(parent) = parent {
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
    runtime.register_object_native(handle, "loadImages", layer_load_images);
    runtime.register_object_native(handle, "setPos", layer_set_pos);
    runtime.register_object_native(handle, "setSize", layer_set_size);
    runtime.register_object_native(handle, "setImagePos", layer_set_image_pos);
    runtime.register_object_native(handle, "setImageSize", layer_set_image_size);
    runtime.register_object_native(handle, "setSizeToImageSize", layer_set_size_to_image_size);
    runtime.register_object_native(handle, "bringToFront", layer_bring_to_front);
    runtime.register_object_native(handle, "bringToBack", layer_bring_to_back);
    runtime.register_object_native(handle, "assignImages", layer_assign_images);
    runtime.register_object_native(handle, "beginTransition", layer_begin_transition);
    runtime.register_object_native(handle, "update", layer_noop);
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
            let layer_id = runtime.host_mut().ensure_kag_layer(&page, &layer_name);
            if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
                layer.set_image(image);
                layer.visible = visible;
                if let Some(left) = left {
                    layer.left = left as f32;
                }
                if let Some(top) = top {
                    layer.top = top as f32;
                }
                if let Some(opacity) = opacity {
                    layer.opacity = opacity.clamp(0, 255) as u8;
                }
            }
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
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.image_width = width as f32;
        layer.image_height = height as f32;
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
    let (width, height) = runtime
        .host()
        .layer_tree()
        .layer(layer_id)
        .map(|layer| (layer.image_width as i64, layer.image_height as i64))
        .unwrap_or((0, 0));
    if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
        layer.width = width as f32;
        layer.height = height as f32;
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
    if let Some(layer_id) = native_layer_id(runtime, this)? {
        if let Some(source) = source {
            copy_layer_images(runtime, this, layer_id, source)?;
        }
        if let Some(layer) = runtime.host_mut().layer_tree_mut().layer_mut(layer_id) {
            layer.visible = true;
        }
    }
    runtime.host_mut().apply_immediate_transition();
    runtime.set_object_member(this, "visible", Variant::Integer(1));
    finish_immediate_transition(runtime, this);
    Ok(Variant::Void)
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

fn layer_noop(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

fn optional_integer(args: &[Variant], index: usize) -> Result<Option<i64>> {
    args.get(index)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_integer)
        .transpose()
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
    runtime.set_object_member(this, "width", Variant::Integer(width));
    runtime.set_object_member(this, "height", Variant::Integer(height));
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
