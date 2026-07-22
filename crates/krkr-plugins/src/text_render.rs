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
///
/// Two call shapes are accepted, matching the observed `textrender.dll` usage:
/// - `render(textString, ...)`
/// - `render(msgObject, size, ...)` where the object carries a `text` member
///   (GINKA's scenario message model; it may contain `[ruby,count]` inline
///   annotations, where the ruby covers the following `count + 1` characters).
fn render(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Integer(0));
    };
    let text = match args.first() {
        Some(Variant::Object(message)) => runtime
            .object_member(*message, "text")
            .to_tjs_string()
            .unwrap_or_default(),
        Some(value) => value.to_tjs_string()?,
        None => String::new(),
    };
    // textrender.dll measures every glyph through TextRender.onGetTextWidth:
    // the callback sets `this.font.height` and calls Font.getEscWidthX (or
    // Font.getTextWidth).  `font` belongs to the TextRender instance, not the
    // render arguments.  Using `font_size / 2` here was especially wrong for
    // full-width Japanese glyphs and caused progressive line-wrap drift.
    let font = match runtime.object_member(this, "font") {
        Variant::Object(font) => Some(font),
        _ => None,
    };
    // GINKA passes the target layer's native Font to setFont(), whose glyph
    // size lives in the native `height` property; plain script font-info
    // objects carry `size` instead.  Resolve through the TJS dispatch path so
    // native property getters run — a raw member read would see the property
    // object itself or `void`.  An explicit numeric size argument wins over
    // both, then the TextRender default.
    let size_argument = args
        .get(1)
        .and_then(|value| match value {
            Variant::Integer(value) => Some(*value),
            Variant::Real(value) => Some(*value as i64),
            _ => None,
        })
        .filter(|value| *value > 0);
    let font_size = size_argument
        .or_else(|| font.and_then(|font| resolve_font_int(runtime, font, &["height", "size"])))
        .unwrap_or_else(|| {
            runtime
                .object_member(this, "defaultFontSize")
                .to_integer()
                .unwrap_or(24)
        })
        .max(1);
    let color = font
        .and_then(|font| resolve_font_int(runtime, font, &["color"]))
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
    let ruby_size = runtime
        .object_member(this, "defaultRubySize")
        .to_integer()
        .unwrap_or(0)
        .max(font_size / 2)
        .max(1);
    let line_height = (font_size + line_spacing).max(1);
    let mut x = 0_i64;
    let mut y = 0_i64;
    let mut records = Vec::new();
    // Ruby group tracking: `[ruby,count]` covers the following count + 1 base
    // characters.  The ruby record (a whole-string annotation dictionary, as
    // GINKA's drawRuby expects) is attached to the group's first character
    // once the group's advance is known.
    let mut pending_ruby: Option<String> = None;
    let mut ruby_remaining = 0_usize;
    let mut group_first_record: Option<ObjectHandle> = None;
    let mut group_base_width = 0_i64;
    for token in parse_ruby_annotations(&text) {
        let character = match token {
            RubyToken::Ruby { text: ruby, count } => {
                pending_ruby = Some(ruby);
                ruby_remaining = count + 1;
                continue;
            }
            RubyToken::Char(character) => character,
        };
        if character == '\r' {
            continue;
        }
        if character == '\n' {
            x = 0;
            y = y.saturating_add(line_height);
            continue;
        }
        let character = character.to_string();
        let char_width = measure_character_width(runtime, this, font, &character, font_size)
            .unwrap_or(font_size)
            .max(1);
        if width > 0 && x > 0 && x.saturating_add(char_width) > width {
            x = 0;
            y = y.saturating_add(line_height);
        }
        let in_ruby_group = ruby_remaining > 0;
        let record = runtime.alloc_dictionary_object();
        runtime.set_object_member(record, "x", Variant::Integer(x));
        runtime.set_object_member(record, "y", Variant::Integer(y));
        runtime.set_object_member(record, "cw", Variant::Integer(char_width));
        runtime.set_object_member(record, "size", Variant::Integer(font_size));
        runtime.set_object_member(record, "text", Variant::String(character));
        runtime.set_object_member(record, "color", Variant::Integer(color));
        runtime.set_object_member(record, "edge", Variant::Integer(0));
        runtime.set_object_member(record, "edgeColor", Variant::Integer(0));
        runtime.set_object_member(record, "shadow", Variant::Integer(0));
        runtime.set_object_member(record, "shadowColor", Variant::Integer(0));
        runtime.set_object_member(record, "italic", Variant::Integer(0));
        runtime.set_object_member(record, "vertical", Variant::Integer(0));
        records.push(Variant::Object(record));
        x = x.saturating_add(char_width).saturating_add(pitch);
        if in_ruby_group {
            if group_first_record.is_none() {
                group_first_record = Some(record);
                group_base_width = 0;
            }
            group_base_width = group_base_width.saturating_add(char_width);
            ruby_remaining -= 1;
            if ruby_remaining == 0
                && let (Some(first), Some(ruby)) = (group_first_record, pending_ruby.take())
            {
                let ruby_record = runtime.alloc_dictionary_object();
                let ruby_width = ruby.chars().count() as i64 * ruby_size;
                let ruby_x = (group_base_width.max(ruby_width) - ruby_width) / 2;
                runtime.set_object_member(ruby_record, "text", Variant::String(ruby));
                runtime.set_object_member(ruby_record, "x", Variant::Integer(ruby_x));
                runtime.set_object_member(ruby_record, "y", Variant::Integer(-ruby_size));
                runtime.set_object_member(ruby_record, "size", Variant::Integer(ruby_size));
                runtime.set_object_member(first, "ruby", Variant::Object(ruby_record));
            }
        }
    }
    // A trailing annotation whose group never completed still gets its ruby
    // attached to the first covered character.
    if let (Some(first), Some(ruby)) = (group_first_record, pending_ruby)
        && ruby_remaining > 0
    {
        let ruby_record = runtime.alloc_dictionary_object();
        let ruby_width = ruby.chars().count() as i64 * ruby_size;
        let ruby_x = (group_base_width.max(ruby_width) - ruby_width) / 2;
        runtime.set_object_member(ruby_record, "text", Variant::String(ruby));
        runtime.set_object_member(ruby_record, "x", Variant::Integer(ruby_x));
        runtime.set_object_member(ruby_record, "y", Variant::Integer(-ruby_size));
        runtime.set_object_member(ruby_record, "size", Variant::Integer(ruby_size));
        runtime.set_object_member(first, "ruby", Variant::Object(ruby_record));
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

enum RubyToken {
    Char(char),
    Ruby { text: String, count: usize },
}

/// Split raw message text into characters and `[ruby,count]` annotations.
/// Bracket runs without a comma are literal text (e.g. English asides).
fn parse_ruby_annotations(text: &str) -> Vec<RubyToken> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '[' {
            tokens.push(RubyToken::Char(character));
            continue;
        }
        let mut content = String::new();
        let mut closed = false;
        for next in chars.by_ref() {
            if next == ']' {
                closed = true;
                break;
            }
            content.push(next);
        }
        let annotation = closed.then(|| {
            let (ruby, count) = content.split_once(',')?;
            let count = count.trim().parse::<usize>().ok()?;
            (!ruby.is_empty()).then_some(RubyToken::Ruby {
                text: ruby.to_string(),
                count,
            })
        });
        match annotation.flatten() {
            Some(ruby) => tokens.push(ruby),
            None => {
                tokens.push(RubyToken::Char('['));
                tokens.extend(content.chars().map(RubyToken::Char));
                if closed {
                    tokens.push(RubyToken::Char(']'));
                }
            }
        }
    }
    tokens
}

/// Query glyph advance through the same virtual callback as textrender.dll.
///
/// `TextRenderBase` deliberately exposes this hook because a game can select
/// a font or apply a scale in TJS.  GINKA's implementation writes the current
/// height to its Layer Font and uses `getEscWidthX`; the direct Font fallback
/// preserves that behaviour when a game does not supply the callback.
fn measure_character_width(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    font: Option<ObjectHandle>,
    character: &str,
    font_size: i64,
) -> Option<i64> {
    runtime
        .call_object_method(
            this,
            "onGetTextWidth",
            vec![
                Variant::String(character.to_string()),
                Variant::Integer(font_size),
            ],
        )
        .ok()
        .and_then(|width| width.to_integer().ok())
        .or_else(|| {
            font.and_then(|font| {
                runtime.set_object_member(font, "height", Variant::Integer(font_size));
                runtime
                    .call_object_method(
                        font,
                        "getEscWidthX",
                        vec![Variant::String(character.to_string())],
                    )
                    .or_else(|_| {
                        runtime.call_object_method(
                            font,
                            "getTextWidth",
                            vec![Variant::String(character.to_string())],
                        )
                    })
                    .ok()
                    .and_then(|width| width.to_integer().ok())
            })
        })
}

/// Read an integer font attribute through the TJS dispatch path (running any
/// property getter), trying each name in order.
fn resolve_font_int(
    runtime: &mut Runtime<KrkrHost>,
    font: ObjectHandle,
    names: &[&str],
) -> Option<i64> {
    for name in names {
        if let Ok(value) = runtime.resolve_object_member(font, name)
            && let Ok(value) = value.to_integer()
        {
            return Some(value);
        }
    }
    None
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
