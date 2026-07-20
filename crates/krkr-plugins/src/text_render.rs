//! Stub for wamsoft `textrender.dll` (`TextRenderBase` class).
//!
//! Games subclass/wrap `TextRenderBase` for custom message text layout.
//! This stub keeps scripts running without laying out or drawing anything:
//! `done()` always reports finished so polling loops do not hang.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

pub struct TextRenderPlugin;

impl KrkrPlugin for TextRenderPlugin {
    fn name(&self) -> &str {
        "textrender.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_text_render_compat(runtime);
        Ok(())
    }
}

fn install_text_render_compat(runtime: &mut Runtime<KrkrHost>) {
    let handle = text_render_base_constructor(runtime);
    runtime.set_global_member("TextRenderBase", Variant::Object(handle));
}

fn text_render_base_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            // The real constructor takes render target info; tolerate any args.
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "TextRenderBase");
            install_text_render_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "TextRenderBase");
    install_text_render_members(runtime, handle);
    handle
}

fn install_text_render_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for (name, value) in [
        ("timeScale", Variant::Real(1.0)),
        ("fontScale", Variant::Real(1.0)),
        ("defaultFace", Variant::String(String::new())),
        ("defaultFontSize", Variant::Integer(24)),
        ("defaultBigFontSize", Variant::Integer(36)),
        ("defaultSmallFontSize", Variant::Integer(18)),
        ("defaultLineSize", Variant::Integer(0)),
        ("defaultLineSpacing", Variant::Integer(0)),
        ("defaultPitch", Variant::Integer(0)),
        ("defaultAlign", Variant::Integer(0)),
        ("defaultValign", Variant::Integer(0)),
        ("defaultRubySize", Variant::Integer(0)),
        ("defaultRubyOffset", Variant::Integer(0)),
        ("defaultChColor", Variant::Integer(0xffffff)),
        ("defaultShadow", Variant::Integer(1)),
        ("defaultShadowColor", Variant::Integer(0)),
        ("defaultShadowDiff", Variant::Integer(0)),
        ("defaultEdge", Variant::Integer(0)),
        ("defaultEdgeColor", Variant::Integer(0)),
        ("defaultBold", Variant::Integer(0)),
        ("defaultItalic", Variant::Integer(0)),
        // Script-assignable callbacks.
        ("onEval", Variant::Void),
        ("onLabel", Variant::Void),
        ("onFontChange", Variant::Void),
        ("onGetTextWidth", Variant::Void),
        ("onGetTextHeight", Variant::Void),
        ("onGetGraphSize", Variant::Void),
    ] {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, value);
        }
    }

    for method in [
        "setOption",
        "setDefault",
        "setRenderSize",
        "clear",
        "resetFont",
        "resetStyle",
        "setFont",
        "setStyle",
        "newline",
        "getLinkOfPosition",
    ] {
        runtime.register_object_native(handle, method, native_void);
    }
    for method in [
        "render",
        "renderOver",
        "renderLines",
        "renderCount",
        "renderDelay",
        "renderLeft",
        "renderTop",
        "renderRight",
        "renderBottom",
        "contains",
        "renderText",
        "maxScrollOffset",
        "maxScrollLine",
        "getKeyWait",
        "calcLineOffset",
        "calcShowCount",
        "isLinkContains",
    ] {
        runtime.register_object_native(handle, method, zero);
    }
    // Scripts poll done(); 1 = finished so they do not hang.
    runtime.register_object_native(handle, "done", one);
    for method in [
        "getCharacters",
        "getLinkNames",
        "getLinkRects",
        "getLinkCharacters",
    ] {
        runtime.register_object_native(handle, method, empty_array);
    }
}

fn empty_array(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

fn zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn one(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(1))
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}
