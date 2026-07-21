//! Compatibility implementation for wamsoft `textrender.dll`
//! (`TextRenderBase` class).
//!
//! Games normally subclass this object in TJS.  The subclass supplies font
//! effects and calls `getCharacters()` to paint each laid-out glyph into a
//! Layer.  Returning an empty list here is therefore not a harmless stub: it
//! advances the scenario while drawing no dialogue at all.

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
        "resetFont",
        "resetStyle",
        "setFont",
        "setStyle",
        "newline",
    ] {
        runtime.register_object_native(handle, method, native_void);
    }
    runtime.register_object_native(handle, "setRenderSize", set_render_size);
    runtime.register_object_native(handle, "clear", clear);
    runtime.register_object_native(handle, "render", render);
    runtime.register_object_native(handle, "getCharacters", get_characters);
    runtime.register_object_native(handle, "getLinkOfPosition", native_void);
    // These are result properties in the real plugin, not methods.  Making
    // them native functions turns ordinary TJS property reads into objects,
    // which in turn breaks message-window positioning and delay handling.
    for (name, value) in [
        ("renderCount", 0),
        ("renderDelay", 0),
        ("renderLeft", 0),
        ("renderTop", 0),
        ("renderRight", 0),
        ("renderBottom", 0),
        ("maxScrollOffset", 0),
        ("maxScrollLine", 0),
        ("renderText", 0),
    ] {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, Variant::Integer(value));
        }
    }
    for method in [
        "renderOver",
        "renderLines",
        "contains",
        "calcLineOffset",
        "calcShowCount",
        "isLinkContains",
    ] {
        runtime.register_object_native(handle, method, zero);
    }
    // `getKeyWait()` is consumed as an Array by `TextRender` (it reads
    // `.count` immediately).  A numeric stub makes the first rendered
    // message fail inside the game's getter for `hasAnyKeyWait`.
    runtime.register_object_native(handle, "getKeyWait", empty_array);
    // Rendering is currently immediate, so scripts may proceed once the full
    // character list has been materialized.
    runtime.register_object_native(handle, "done", one);
    for method in ["getLinkNames", "getLinkRects", "getLinkCharacters"] {
        runtime.register_object_native(handle, method, empty_array);
    }
}

const CHARACTERS_MEMBER: &str = "__krkr_text_render_characters";
const RENDER_WIDTH_MEMBER: &str = "__krkr_text_render_width";
const RENDER_HEIGHT_MEMBER: &str = "__krkr_text_render_height";

fn bound_this(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Option<ObjectHandle> {
    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

fn set_render_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let width = args
        .first()
        .cloned()
        .unwrap_or(Variant::Integer(0))
        .to_integer()?;
    let height = args
        .get(1)
        .cloned()
        .unwrap_or(Variant::Integer(0))
        .to_integer()?;
    runtime.set_object_member(this, RENDER_WIDTH_MEMBER, Variant::Integer(width.max(0)));
    runtime.set_object_member(this, RENDER_HEIGHT_MEMBER, Variant::Integer(height.max(0)));
    runtime.set_object_member(this, "renderLeft", Variant::Integer(0));
    runtime.set_object_member(this, "renderTop", Variant::Integer(0));
    runtime.set_object_member(this, "renderRight", Variant::Integer(width.max(0)));
    runtime.set_object_member(this, "renderBottom", Variant::Integer(height.max(0)));
    Ok(Variant::Void)
}

fn clear(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let characters = runtime.alloc_array_object(Vec::new());
    runtime.set_object_member(this, CHARACTERS_MEMBER, Variant::Object(characters));
    Ok(Variant::Void)
}

/// Materialize the minimal character records consumed by GINKA's
/// `system/textrender.tjs`.  That script owns effect selection and delegates
/// actual glyph painting to Layer.drawText; the native plugin's responsibility
/// here is the line layout and character geometry.
fn render(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Integer(0));
    };
    let text = args
        .first()
        .cloned()
        .unwrap_or(Variant::String(String::new()))
        .to_string();
    let font = args.iter().find_map(|value| match value {
        Variant::Object(object)
            if !matches!(runtime.object_member(*object, "size"), Variant::Void) =>
        {
            Some(*object)
        }
        _ => None,
    });
    let font_size = font
        .map(|font| runtime.object_member(font, "size").to_integer())
        .transpose()?
        .unwrap_or_else(|| {
            runtime
                .object_member(this, "defaultFontSize")
                .to_integer()
                .unwrap_or(24)
        })
        .max(1);
    let color = font
        .map(|font| runtime.object_member(font, "color").to_integer())
        .transpose()?
        .unwrap_or_else(|| {
            runtime
                .object_member(this, "defaultChColor")
                .to_integer()
                .unwrap_or(0xffffff)
        });
    let width = runtime
        .object_member(this, RENDER_WIDTH_MEMBER)
        .to_integer()
        .unwrap_or(0)
        .max(0);
    let line_spacing = runtime
        .object_member(this, "defaultLineSpacing")
        .to_integer()
        .unwrap_or(0)
        .max(0);
    let pitch = runtime
        .object_member(this, "defaultPitch")
        .to_integer()
        .unwrap_or(0);
    let char_width = (font_size / 2 + pitch).max(1);
    let line_height = (font_size + line_spacing).max(1);
    let mut x = 0_i64;
    let mut y = 0_i64;
    let mut records = Vec::new();
    for character in text.chars() {
        if character == '\r' {
            continue;
        }
        if character == '\n' {
            x = 0;
            y = y.saturating_add(line_height);
            continue;
        }
        if width > 0 && x > 0 && x.saturating_add(char_width) > width {
            x = 0;
            y = y.saturating_add(line_height);
        }
        let record = runtime.alloc_dictionary_object();
        runtime.set_object_member(record, "x", Variant::Integer(x));
        runtime.set_object_member(record, "y", Variant::Integer(y));
        runtime.set_object_member(record, "cw", Variant::Integer(char_width));
        runtime.set_object_member(record, "size", Variant::Integer(font_size));
        runtime.set_object_member(record, "text", Variant::String(character.to_string()));
        runtime.set_object_member(record, "color", Variant::Integer(color));
        runtime.set_object_member(record, "edge", Variant::Integer(0));
        runtime.set_object_member(record, "edgeColor", Variant::Integer(0));
        runtime.set_object_member(record, "shadow", Variant::Integer(0));
        runtime.set_object_member(record, "shadowColor", Variant::Integer(0));
        runtime.set_object_member(record, "italic", Variant::Integer(0));
        runtime.set_object_member(record, "vertical", Variant::Integer(0));
        records.push(Variant::Object(record));
        x = x.saturating_add(char_width);
    }
    let count = records.len() as i64;
    let characters = runtime.alloc_array_object(records);
    runtime.set_object_member(this, CHARACTERS_MEMBER, Variant::Object(characters));
    runtime.set_object_member(this, "renderLeft", Variant::Integer(0));
    runtime.set_object_member(this, "renderTop", Variant::Integer(0));
    runtime.set_object_member(this, "renderRight", Variant::Integer(x.max(width)));
    runtime.set_object_member(
        this,
        "renderBottom",
        Variant::Integer(y.saturating_add(line_height)),
    );
    Ok(Variant::Integer(count))
}

fn get_characters(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Object(runtime.alloc_array_object(Vec::new())));
    };
    let characters = runtime.object_member(this, CHARACTERS_MEMBER);
    if matches!(characters, Variant::Void) {
        return Ok(Variant::Object(runtime.alloc_array_object(Vec::new())));
    }
    Ok(characters)
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
