//! Compatibility implementation for wamsoft `textrender.dll`
//! (`TextRenderBase` class).
//!
//! Games normally subclass this object in TJS
//! (`class TextRender extends TextRenderBase`), supply the layout callbacks and
//! paint the glyph records `getCharacters()` hands back into Layers. Returning
//! an empty list here is therefore not a harmless stub: it advances the
//! scenario while drawing no dialogue at all.
//!
//! # Surface
//!
//! The DLL registers 55 members on `TextRenderBase`. [`SURFACE`] lists them in
//! the DLL's registration order with the kind (ncbind *method* command vs
//! ncbind *property* command) and the reference signature recovered from the
//! command objects' mangled RTTI names
//! (`ncbNativeClassMethod<InvokeCommand<TextRenderBase, ...>>` /
//! `ncbNativeClassProperty<PropertyCommand<TextRenderBase, ...>>`). The module
//! registers every one of them, on the class object so script subclasses
//! inherit them, and the tests check the live class against the list.
//!
//! Where the M27 dossier (`.tower/worktrees/wt-27/docs/plugins/textrender.md`)
//! grouped members by guesswork, the binary's registration function decides:
//! `renderOver` is a get-only *bool* property, `renderText` a get-only *string*
//! property, `renderBottom` a get-only float property, `maxScrollOffset` a
//! get-only float property, `maxScrollLine` a get-only int property, and
//! `contains`/`getLinkOfPosition`/`isLinkContains`/`getLink*` are methods (their
//! commands are `ncbNativeClassMethod` instantiations). The 55 names, their
//! order and their count are the dossier's.
//!
//! # Reference defaults
//!
//! Every property starts at the value the DLL's constructor writes
//! (`FUN_1000d5e0`, `.rdata` constants `0x1002c700`-`0x1002c728`): `face`
//! "normal", font size 24, big 48, small 12, line size 24, line spacing 6,
//! pitch 0, ruby size 10, ruby offset -2, text color `0xffffffff`, shadow on
//! with color `0xff000000` and diff 1, edge off with color `0xff0080ff`,
//! align/valign -1, `timeScale`/`fontScale` 1.0. `setDefault` derives
//! `bigfontsize` (2x), `smallfontsize` (0.5x), `rubysize` (/2.4) and `linesize`
//! from `fontsize` when the caller leaves them out, exactly as `FUN_100022f0`
//! does.
//!
//! The same constructor also seeds the line-breaking character sets the
//! `setOption` keys replace (`following` 68 characters, `leading` 19,
//! `begin`/trailing 10 each) and `kinsoku_max` 1 with `word_break` on; those are
//! stored when a script passes them but not applied yet — see below.
//!
//! # What the engine cannot do yet
//!
//! These members are registered with reference kinds and honest values, but
//! their full behaviour needs engine work and is reported in the mission
//! summary instead of being faked:
//!
//! - vertical layout: `vertical` is stored and the scroll getters switch axis
//!   like the DLL, but glyphs are still laid out horizontally because the text
//!   drawing path performs no glyph rotation.
//! - the link model: `getLinkNames`/`getLinkRects`/`getLinkCharacters` return
//!   empty arrays, `isLinkContains` false and `getLinkOfPosition` -1 because no
//!   engine object tracks link spans or `linkName`s.
//! - inline evaluation: `onEval(text)` returns its argument; the DLL evaluates
//!   the expression through `TVPExecuteExpression`, which a plugin cannot reach
//!   in this runtime.
//! - `calcLineOffset`/`calcShowCount` follow their reference signatures; the
//!   DLL's own definitions were not decompiled, so the values are the natural
//!   ones over this module's line records (a line's origin; the characters in a
//!   line range).
//! - line-breaking options: `vertical`, `width_time_scale` and the booleans are
//!   stored and the layout acts on those it can (axis, per-glyph delay), but
//!   `following`/`leading`/`begin`/`kinsoku_max`/`word_break` and the
//!   `ignore_*` gates need the DLL's kazari line-breaking rules, which were not
//!   decompiled — text is broken at the render box width only.
//! - `done()` returns 1 where the DLL returns void: this renderer finishes
//!   synchronously, and the in-repo game conductors treat the truthy answer as
//!   "the characters are materialized".
//!
//! # Deliberate, documented deviations
//!
//! - Per-instance state lives in one hidden member (`__krkr_text_render`,
//!   a Dictionary) because TJS native integrations keep no side table; scripts
//!   can see it in a member enumeration, unlike the DLL's C++ members.
//! - `onGetTextWidth`/`onGetTextHeight`/`onGetGraphSize`/`onFontChange`/
//!   `onLabel` are not `TextRenderBase` members in the DLL (its character
//!   objects carry them, and the object is built dynamically per character).
//!   They stay here as void, script-assignable members because glyph
//!   measurement goes through `onGetTextWidth` and game scripts assign them.
//! - `setRenderSize` also seeds the result properties with the box, so a script
//!   that sizes a window before rendering sees the box rather than a stale 0.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "TextRenderBase",
    notes: "All 55 reference members in registration order: 22 methods with the DLL's signatures and 33 properties with the DLL's constructor defaults and get-only access. setOption's 18 keys and setDefault's 18 style keys follow the DLL, unknown keys are ignored as there. Glyphs measure through the game's onGetTextWidth or the engine Font (getEscWidthX/getTextWidth); vertical layout, the link model and inline onEval evaluation still need engine work.",
    install: |engine| engine.register_plugin(TextRenderPlugin),
};

pub struct TextRenderPlugin;

impl KrkrPlugin for TextRenderPlugin {
    fn name(&self) -> &str {
        "textrender.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_text_render_compat(runtime);
        runtime.host_mut().log(
            "textrender.dll compat registered: TextRenderBase with the 55-member surface \
             (22 methods, 33 properties), setOption/setDefault key sets and reference \
             defaults; layout wraps through the game's onGetTextWidth or the engine Font",
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The reference surface

/// A member handler: the shape every native method in this module uses.
type NativeMethod =
    fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>;

/// A property getter that computes from layout state instead of reading the
/// stored member (the DLL computes `maxScrollOffset`/`maxScrollLine`).
type NativeGetter = fn(&mut Runtime<KrkrHost>, ObjectHandle) -> Result<Variant>;

/// The TJS value a property reads and writes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Bool,
    Int,
    Real,
    Text,
}

/// A property's value before a script ever writes it: the DLL constructor's
/// member initialisation.
#[derive(Clone, Copy)]
enum Default {
    Bool(bool),
    Int(i64),
    Real(f64),
    Text(&'static str),
}

/// How the DLL's registration function registers a member.
enum MemberKind {
    /// `ncbNativeClassMethod<InvokeCommand<...>>` — invocable.
    Method {
        handler: NativeMethod,
        /// Arguments the reference command needs to do anything; ncbind pads
        /// arguments a caller leaves out, so this is a lower bound, not an
        /// exact count (`PARQUET` calls the 5-argument `render` with 3).
        required_args: usize,
    },
    /// `ncbNativeClassProperty<PropertyCommand<...>>` — an accessor pair.
    /// `writable: false` mirrors the DLL's null setter: a script write fails
    /// with `TJS_E_ACCESSDENYED` before the setter runs.
    Property {
        value: ValueKind,
        writable: bool,
        default: Default,
        /// Computed getters; `None` reads the stored member.
        compute: Option<NativeGetter>,
    },
}

struct Member {
    name: &'static str,
    /// The DLL's command signature, written as `ret (TextRender::*)(args)`.
    signature: &'static str,
    kind: MemberKind,
}

/// The 55 members of the DLL's `TextRenderBase`, in registration order.
///
/// Order and names are the dossier's list; kinds and signatures come from the
/// registration function `FUN_10005a60` (each name is registered with the
/// command object built immediately before it) and the command vftables.
const SURFACE: &[Member] = &[
    member("setOption", "void (const tTJSVariant &)", set_option, 1),
    member("setDefault", "void (const tTJSVariant &)", set_default, 1),
    member("setRenderSize", "void (float, float)", set_render_size, 1),
    prop("vertical", "bool", ValueKind::Bool, Default::Bool(false)),
    prop(
        "timeScale",
        "float",
        ValueKind::Real,
        Default::Real(1.0),
    ),
    prop(
        "fontScale",
        "float",
        ValueKind::Real,
        Default::Real(1.0),
    ),
    member("clear", "void ()", clear, 0),
    member("resetFont", "void ()", reset_font, 0),
    member("resetStyle", "void ()", reset_style, 0),
    member("setFont", "void (const tTJSVariant &)", set_font, 1),
    member("setStyle", "void (const tTJSVariant &)", set_style, 1),
    member(
        "render",
        "bool (const tjs_char *, int, int, int, bool)",
        render,
        1,
    ),
    member("newline", "void ()", newline, 0),
    member("done", "void ()", done, 0),
    member(
        "onEval",
        "tTJSString (const tjs_char *)",
        on_eval,
        1,
    ),
    prop_read_only(
        "renderOver",
        "bool",
        ValueKind::Bool,
        Default::Bool(false),
    ),
    prop_read_only("renderLines", "int", ValueKind::Int, Default::Int(0)),
    prop_read_only("renderCount", "int", ValueKind::Int, Default::Int(0)),
    prop_read_only(
        "renderDelay",
        "float",
        ValueKind::Real,
        Default::Real(0.0),
    ),
    prop_read_only(
        "renderLeft",
        "float",
        ValueKind::Real,
        Default::Real(0.0),
    ),
    prop_read_only("renderTop", "float", ValueKind::Real, Default::Real(0.0)),
    prop_read_only(
        "renderRight",
        "float",
        ValueKind::Real,
        Default::Real(0.0),
    ),
    prop_read_only(
        "renderBottom",
        "float",
        ValueKind::Real,
        Default::Real(0.0),
    ),
    member("contains", "bool (float, float) const", contains, 2),
    prop_read_only(
        "renderText",
        "const tjs_char *",
        ValueKind::Text,
        Default::Text(""),
    ),
    prop_computed(
        "maxScrollOffset",
        "float",
        ValueKind::Real,
        Default::Real(0.0),
        max_scroll_offset,
    ),
    prop_computed(
        "maxScrollLine",
        "int",
        ValueKind::Int,
        Default::Int(0),
        max_scroll_line,
    ),
    member(
        "getKeyWait",
        "tTJSVariant () const",
        get_key_wait,
        0,
    ),
    member(
        "calcLineOffset",
        "float (int) const",
        calc_line_offset,
        1,
    ),
    member(
        "calcShowCount",
        "tTJSVariant (int, int) const",
        calc_show_count,
        1,
    ),
    member(
        "getCharacters",
        "tTJSVariant (int, int) const",
        get_characters,
        0,
    ),
    member(
        "getLinkNames",
        "tTJSVariant () const",
        get_link_names,
        0,
    ),
    member(
        "getLinkRects",
        "tTJSVariant (int) const",
        get_link_rects,
        0,
    ),
    member(
        "getLinkCharacters",
        "tTJSVariant (int) const",
        get_link_characters,
        0,
    ),
    member(
        "isLinkContains",
        "bool (int, float, float) const",
        is_link_contains,
        0,
    ),
    member(
        "getLinkOfPosition",
        "int (float, float)",
        get_link_of_position,
        2,
    ),
    prop(
        "defaultFace",
        "const tjs_char *",
        ValueKind::Text,
        Default::Text("normal"),
    ),
    prop(
        "defaultFontSize",
        "float",
        ValueKind::Real,
        Default::Real(24.0),
    ),
    prop(
        "defaultBigFontSize",
        "float",
        ValueKind::Real,
        Default::Real(48.0),
    ),
    prop(
        "defaultSmallFontSize",
        "float",
        ValueKind::Real,
        Default::Real(12.0),
    ),
    prop(
        "defaultLineSize",
        "float",
        ValueKind::Real,
        Default::Real(24.0),
    ),
    prop(
        "defaultLineSpacing",
        "float",
        ValueKind::Real,
        Default::Real(6.0),
    ),
    prop(
        "defaultPitch",
        "float",
        ValueKind::Real,
        Default::Real(0.0),
    ),
    prop("defaultAlign", "int", ValueKind::Int, Default::Int(-1)),
    prop("defaultValign", "int", ValueKind::Int, Default::Int(-1)),
    prop(
        "defaultRubySize",
        "float",
        ValueKind::Real,
        Default::Real(10.0),
    ),
    prop(
        "defaultRubyOffset",
        "float",
        ValueKind::Real,
        Default::Real(-2.0),
    ),
    prop(
        "defaultChColor",
        "unsigned int",
        ValueKind::Int,
        Default::Int(0xff_ff_ff_ff),
    ),
    prop(
        "defaultShadow",
        "bool",
        ValueKind::Bool,
        Default::Bool(true),
    ),
    prop(
        "defaultShadowColor",
        "unsigned int",
        ValueKind::Int,
        Default::Int(0xff00_0000),
    ),
    prop(
        "defaultShadowDiff",
        "int",
        ValueKind::Int,
        Default::Int(1),
    ),
    prop(
        "defaultEdge",
        "bool",
        ValueKind::Bool,
        Default::Bool(false),
    ),
    prop(
        "defaultEdgeColor",
        "unsigned int",
        ValueKind::Int,
        Default::Int(0xff00_80ff),
    ),
    prop("defaultBold", "bool", ValueKind::Bool, Default::Bool(false)),
    prop(
        "defaultItalic",
        "bool",
        ValueKind::Bool,
        Default::Bool(false),
    ),
];

const fn member(
    name: &'static str,
    signature: &'static str,
    handler: NativeMethod,
    required_args: usize,
) -> Member {
    Member {
        name,
        signature,
        kind: MemberKind::Method {
            handler,
            required_args,
        },
    }
}

const fn prop(
    name: &'static str,
    signature: &'static str,
    value: ValueKind,
    default: Default,
) -> Member {
    Member {
        name,
        signature,
        kind: MemberKind::Property {
            value,
            writable: true,
            default,
            compute: None,
        },
    }
}

/// A get-only property: the DLL registers these with a null setter, so a
/// script write fails with `TJS_E_ACCESSDENYED`.
const fn prop_read_only(
    name: &'static str,
    signature: &'static str,
    value: ValueKind,
    default: Default,
) -> Member {
    Member {
        name,
        signature,
        kind: MemberKind::Property {
            value,
            writable: false,
            default,
            compute: None,
        },
    }
}

const fn prop_computed(
    name: &'static str,
    signature: &'static str,
    value: ValueKind,
    default: Default,
    compute: NativeGetter,
) -> Member {
    Member {
        name,
        signature,
        kind: MemberKind::Property {
            value,
            writable: false,
            default,
            compute: Some(compute),
        },
    }
}

/// The DLL registers `finalize` through its auto-registration path, outside the
/// 55 members, and `tTJSNativeClass::FuncCall` copies registered members onto an
/// instance, so both are installed here too.
const ENGINE_COMPAT_MEMBERS: &[&str] = &[
    "onLabel",
    "onFontChange",
    "onGetTextWidth",
    "onGetTextHeight",
    "onGetGraphSize",
];

// ---------------------------------------------------------------------------
// Registration

fn install_text_render_compat(runtime: &mut Runtime<KrkrHost>) {
    debug_assert!(
        SURFACE
            .iter()
            .all(|item| !item.name.is_empty() && !item.signature.is_empty()),
        "every surface entry documents its reference signature"
    );
    let handle = text_render_base_constructor(runtime);
    runtime.set_global_member("TextRenderBase", Variant::Object(handle));
}

fn text_render_base_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            // `TextRenderBase.TextRenderBase()` from a script subclass runs on
            // the caller's object; a bare `new TextRenderBase()` gets a fresh
            // one. The DLL's class initialiser behaves the same way
            // (`tTJSNativeClass::FuncCall` copies members onto the object it is
            // invoked on).
            let instance = this_obj
                .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                .filter(|handle| *handle != runtime.global_handle())
                .unwrap_or_else(|| runtime.alloc_ordinary_object());
            runtime.add_object_class_info(instance, "TextRenderBase");
            // Members stay on the class object: a script subclass overrides
            // `render`/`clear`/... with its own methods, and an instance-level
            // copy would shadow the override (the same reason WaveSoundBuffer
            // and VideoOverlay keep their natives on the class).
            if runtime.object_super_class(instance).is_none()
                && let Variant::Object(class) = runtime.global_member("TextRenderBase")
            {
                runtime.set_object_super_class(instance, class);
            }
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "TextRenderBase");
    install_text_render_members(runtime, handle);
    handle
}

fn install_text_render_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for item in SURFACE {
        match &item.kind {
            MemberKind::Method {
                handler,
                required_args,
            } => {
                runtime.register_object_native_with_arg_count(
                    handle,
                    item.name,
                    NativeArgCount::AtLeast(*required_args),
                    *handler,
                );
            }
            MemberKind::Property {
                value,
                writable,
                default,
                compute,
            } => {
                let name = item.name;
                let (value, default, compute) = (*value, *default, *compute);
                let getter = move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                    let Some(instance) = bound_this(runtime, this_obj) else {
                        return Ok(default_variant(default));
                    };
                    if let Some(compute) = compute {
                        return compute(runtime, instance);
                    }
                    let stored = state_member(runtime, instance, name);
                    if matches!(stored, Variant::Void) {
                        return Ok(default_variant(default));
                    }
                    Ok(coerce_variant(stored, value))
                };
                let setter = move |runtime: &mut Runtime<KrkrHost>,
                                   this_obj: Option<ObjectHandle>,
                                   value_in: Variant| {
                    if let Some(instance) = bound_this(runtime, this_obj) {
                        state_store(runtime, instance, name, coerce_variant(value_in, value));
                    }
                    Ok(())
                };
                let access = if *writable {
                    NativePropertyAccess::ReadWrite
                } else {
                    NativePropertyAccess::ReadOnly
                };
                runtime.register_object_native_property_with_access(
                    handle, name, access, getter, setter,
                );
            }
        }
    }
    // Callback slots the DLL keeps on character objects; kept as inheritable
    // void members so a subclass can assign or read them (see module docs).
    for name in ENGINE_COMPAT_MEMBERS {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, *name, Variant::Void);
        }
    }
}

fn bound_this(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Option<ObjectHandle> {
    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

fn default_variant(default: Default) -> Variant {
    match default {
        Default::Bool(value) => Variant::Integer(i64::from(value)),
        Default::Int(value) => Variant::Integer(value),
        Default::Real(value) => Variant::Real(value),
        Default::Text(value) => Variant::String(value.to_string()),
    }
}

fn coerce_variant(value: Variant, kind: ValueKind) -> Variant {
    match kind {
        ValueKind::Bool => Variant::Integer(i64::from(value.to_integer().unwrap_or(0) != 0)),
        ValueKind::Int => Variant::Integer(value.to_integer().unwrap_or(0)),
        ValueKind::Real => Variant::Real(value.to_real().unwrap_or(0.0)),
        ValueKind::Text => Variant::String(value.to_tjs_string().unwrap_or_default()),
    }
}

// ---------------------------------------------------------------------------
// Per-instance state

/// The one hidden member holding every per-instance value: property values a
/// script wrote, the render box and the last layout. A Dictionary keeps the
/// TJS-visible surface to a single extra member (see the module docs).
const STATE_MEMBER: &str = "__krkr_text_render";

const NEXT_LINE_BREAK: &str = "pendingBreak";

fn state_handle(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle) -> ObjectHandle {
    if let Variant::Object(handle) = runtime.object_member(instance, STATE_MEMBER) {
        return handle;
    }
    let handle = runtime.alloc_dictionary_object();
    runtime.set_object_member(instance, STATE_MEMBER, Variant::Object(handle));
    handle
}

fn state_member(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, key: &str) -> Variant {
    let Variant::Object(state) = runtime.object_member(instance, STATE_MEMBER) else {
        return Variant::Void;
    };
    runtime.object_member(state, key)
}

fn state_store(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle, key: &str, value: Variant) {
    let state = state_handle(runtime, instance);
    runtime.set_object_member(state, key, value);
}

fn state_clear(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle, key: &str) {
    if let Variant::Object(state) = runtime.object_member(instance, STATE_MEMBER) {
        runtime.delete_object_member(state, key);
    }
}

fn state_int(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, key: &str) -> Option<i64> {
    match state_member(runtime, instance, key) {
        Variant::Void => None,
        value => value.to_integer().ok(),
    }
}

fn state_real(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, key: &str) -> Option<f64> {
    match state_member(runtime, instance, key) {
        Variant::Void => None,
        value => value.to_real().ok(),
    }
}

fn state_bool(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, key: &str) -> bool {
    state_int(runtime, instance, key).unwrap_or(0) != 0
}

fn state_text(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, key: &str) -> Option<String> {
    match state_member(runtime, instance, key) {
        Variant::Void => None,
        value => value.to_tjs_string().ok(),
    }
}

/// A property value with the DLL's fallback chain: the instance value a script
/// set, then the member's reference default.
fn value_real(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> f64 {
    state_real(runtime, instance, name)
        .or_else(|| surface_default(name).and_then(|default| match default {
            Default::Real(value) => Some(value),
            Default::Int(value) => Some(value as f64),
            _ => None,
        }))
        .unwrap_or(0.0)
}

fn value_int(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> i64 {
    state_int(runtime, instance, name)
        .or_else(|| surface_default(name).and_then(|default| match default {
            Default::Int(value) => Some(value),
            Default::Bool(value) => Some(i64::from(value)),
            Default::Real(value) => Some(value as i64),
            _ => None,
        }))
        .unwrap_or(0)
}

fn value_bool(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> bool {
    if let Some(value) = state_int(runtime, instance, name) {
        return value != 0;
    }
    matches!(surface_default(name), Some(Default::Bool(true)))
}

fn value_text(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> String {
    if let Some(value) = state_text(runtime, instance, name) {
        return value;
    }
    match surface_default(name) {
        Some(Default::Text(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn surface_default(name: &str) -> Option<Default> {
    SURFACE.iter().find(|item| item.name == name).and_then(|item| match &item.kind {
        MemberKind::Property { default, .. } => Some(*default),
        MemberKind::Method { .. } => None,
    })
}

/// A property's own member name is also its state key, so `setDefault` writes
/// the very values the `default*` properties read — exactly as the DLL's
/// `FUN_100022f0` writes the members its property getters return.
fn property_state_key(name: &str) -> &str {
    name
}

// ---------------------------------------------------------------------------
// setOption (FUN_10001a70) and setDefault (FUN_100022f0)

/// The 18 `setOption` keys the DLL compares, in its own order. Unknown keys are
/// never read: the DLL's accessor loop has no default branch, so an unknown key
/// is silently ignored, and so is a known key of the wrong type.
const OPTION_KEYS: &[&str] = &[
    "following",
    "leading",
    "begin",
    "vertical",
    "kinsoku_max",
    "word_break",
    "ignore_color",
    "ignore_size",
    "ignore_delay",
    "ignore_over",
    "ignore_overy",
    "ignore_overx",
    "width_time_scale",
    "ignore_ruby",
    "ignore_type",
    "ignore_face",
    "ignore_style",
    "ignore_xr",
];

/// The 18 style keys `setDefault`/`setStyle` read (`FUN_100022f0`), mapped onto
/// the properties whose members they write.
const STYLE_KEYS: &[(&str, &str)] = &[
    ("face", "defaultFace"),
    ("bold", "defaultBold"),
    ("fontsize", "defaultFontSize"),
    ("bigfontsize", "defaultBigFontSize"),
    ("smallfontsize", "defaultSmallFontSize"),
    ("rubysize", "defaultRubySize"),
    ("rubyoffset", "defaultRubyOffset"),
    ("color", "defaultChColor"),
    ("shadow", "defaultShadow"),
    ("shadowcolor", "defaultShadowColor"),
    ("shadowdiff", "defaultShadowDiff"),
    ("edge", "defaultEdge"),
    ("edgecolor", "defaultEdgeColor"),
    ("linespacing", "defaultLineSpacing"),
    ("pitch", "defaultPitch"),
    ("linesize", "defaultLineSize"),
    ("align", "defaultAlign"),
    ("valign", "defaultValign"),
];

/// Reference ratios the DLL derives when `fontsize` arrives without its
/// dependent keys: the constructor's 24/48/12/24 defaults give big = 2x,
/// small = 0.5x, line size = font size, and ruby size 10 for font size 24.
const BIG_FONT_RATIO: f64 = 2.0;
const SMALL_FONT_RATIO: f64 = 0.5;
const RUBY_FONT_DIVISOR: f64 = 2.4;

fn set_option(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let Some(options) = dictionary_argument(runtime, args.first()) else {
        return Ok(Variant::Void);
    };
    for key in OPTION_KEYS {
        let Some(value) = read_property(runtime, options, key) else {
            continue;
        };
        let coerced = match *key {
            "following" | "leading" | "begin" => Variant::String(value.to_tjs_string()?),
            "kinsoku_max" => Variant::Integer(value.to_integer().unwrap_or(1)),
            "vertical" => {
                let flag = Variant::Integer(i64::from(value.to_integer().unwrap_or(0) != 0));
                // `vertical` is a property, so the flag a script reads back has
                // to land in the store that property reads.
                state_store(runtime, this, property_state_key("vertical"), flag);
                continue;
            }
            _ => Variant::Integer(i64::from(value.to_integer().unwrap_or(0) != 0)),
        };
        state_store(runtime, this, key, coerced);
    }
    Ok(Variant::Void)
}

fn set_default(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let Some(styles) = dictionary_argument(runtime, args.first()) else {
        return Ok(Variant::Void);
    };
    apply_styles(runtime, this, styles)?;
    Ok(Variant::Void)
}

fn set_style(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    set_default(runtime, this_obj, args)
}

/// Read every known style key from `styles` and write it to the property the
/// DLL's member belongs to. `fontsize` also fills the dependent keys the caller
/// left out, exactly as `FUN_100022f0` does before its own reads.
fn apply_styles(
    runtime: &mut Runtime<KrkrHost>,
    instance: ObjectHandle,
    styles: ObjectHandle,
) -> Result<()> {
    let present = |runtime: &mut Runtime<KrkrHost>, key: &str| {
        read_property(runtime, styles, key).is_some()
    };
    let read = |runtime: &mut Runtime<KrkrHost>, key: &str, kind: ValueKind| {
        read_property(runtime, styles, key).map(|value| coerce_variant(value, kind))
    };
    for (key, property) in STYLE_KEYS {
        if let Some(value) = read(runtime, key, style_value_kind(property)) {
            state_store(runtime, instance, property_state_key(property), value);
        }
    }
    if present(runtime, "fontsize") {
        // The DLL derives the unset dependents from `fontsize` when it is the
        // only size key present (the nested checks in FUN_100022f0).
        let font_size = value_real(runtime, instance, "defaultFontSize");
        let derived = [
            ("bigfontsize", "defaultBigFontSize", font_size * BIG_FONT_RATIO),
            (
                "smallfontsize",
                "defaultSmallFontSize",
                font_size * SMALL_FONT_RATIO,
            ),
            ("linesize", "defaultLineSize", font_size),
            (
                "rubysize",
                "defaultRubySize",
                font_size / RUBY_FONT_DIVISOR,
            ),
        ];
        for (key, property, value) in derived {
            if !present(runtime, key) {
                state_store(
                    runtime,
                    instance,
                    property_state_key(property),
                    Variant::Real(value),
                );
            }
        }
    }
    Ok(())
}

fn style_value_kind(property: &str) -> ValueKind {
    match property {
        "defaultFace" => ValueKind::Text,
        "defaultBold" | "defaultShadow" | "defaultEdge" => ValueKind::Bool,
        "defaultChColor" | "defaultShadowColor" | "defaultShadowDiff" | "defaultEdgeColor"
        | "defaultAlign" | "defaultValign" => ValueKind::Int,
        _ => ValueKind::Real,
    }
}

/// The argument of `setOption`/`setDefault`/`setStyle`/`setFont`: a Dictionary,
/// a plain object, or a script closure bound to one (`new Dictionary()` results
/// arrive self-bound).
fn dictionary_argument(
    runtime: &Runtime<KrkrHost>,
    argument: Option<&Variant>,
) -> Option<ObjectHandle> {
    argument
        .and_then(Variant::object_handle)
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

/// Read one key through the TJS dispatch path. `None` when the key is absent or
/// holds void, which is how the DLL's accessor treats a missing member.
fn read_property(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    key: &str,
) -> Option<Variant> {
    let value = runtime
        .resolve_object_member(object, key)
        .unwrap_or(Variant::Void);
    if matches!(value, Variant::Void) {
        return None;
    }
    Some(value)
}

// ---------------------------------------------------------------------------
// Style resets

fn set_render_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let width = arguments(&args, 0).to_integer().unwrap_or(0).max(0);
    let height = arguments(&args, 1).to_integer().unwrap_or(0).max(0);
    state_store(runtime, this, RENDER_WIDTH, Variant::Integer(width));
    state_store(runtime, this, RENDER_HEIGHT, Variant::Integer(height));
    // The result properties describe the last layout in the DLL; seeding them
    // with the box keeps a script that sizes its window before rendering happy
    // (see the module docs).
    state_store(runtime, this, "renderLeft", Variant::Real(0.0));
    state_store(runtime, this, "renderTop", Variant::Real(0.0));
    state_store(runtime, this, "renderRight", Variant::Real(width as f64));
    state_store(runtime, this, "renderBottom", Variant::Real(height as f64));
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
    reset_layout(runtime, this);
    Ok(Variant::Void)
}

/// `resetFont` (`FUN_1000def0`) drops the attributes a font selection
/// invalidates: align, fontsize, linesize, linespacing, pitch and valign.
fn reset_font(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    runtime.delete_object_member(this, "font");
    for property in [
        "defaultAlign",
        "defaultFontSize",
        "defaultLineSize",
        "defaultLineSpacing",
        "defaultPitch",
        "defaultValign",
    ] {
        state_clear(runtime, this, property_state_key(property));
        state_clear(runtime, this, property);
    }
    Ok(Variant::Void)
}

/// `resetStyle` (`FUN_1000dff0`) clears bold, color, edge, edgecolor, face,
/// fontsize, rubyoffset, rubysize, shadow, shadowcolor and shadowdiff.
fn reset_style(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    for property in [
        "defaultBold",
        "defaultChColor",
        "defaultEdge",
        "defaultEdgeColor",
        "defaultFace",
        "defaultFontSize",
        "defaultRubyOffset",
        "defaultRubySize",
        "defaultShadow",
        "defaultShadowColor",
        "defaultShadowDiff",
    ] {
        state_clear(runtime, this, property_state_key(property));
    }
    Ok(Variant::Void)
}

fn set_font(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    if let Some(font) = args.first().and_then(Variant::object_handle) {
        // `render` reads the instance's `font` member, which is also where a
        // game script assigns the layer font directly.
        runtime.set_object_member(this, "font", Variant::Object(font));
    }
    for property in [
        "defaultAlign",
        "defaultFontSize",
        "defaultLineSize",
        "defaultLineSpacing",
        "defaultPitch",
        "defaultValign",
    ] {
        state_clear(runtime, this, property_state_key(property));
    }
    Ok(Variant::Void)
}

/// The layout results, which are also the get-only result properties: a `render`
/// pass fills them and `clear()` empties them. Their state key is the property
/// name, so the property getters read exactly what the layout wrote.
const LAYOUT_RESULT_KEYS: &[&str] = &[
    "renderCount",
    "renderLines",
    "renderOver",
    "renderDelay",
    "renderText",
    "renderLeft",
    "renderTop",
    "renderRight",
    "renderBottom",
];

fn reset_layout(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle) {
    for key in [
        LAYOUT_CHARACTERS,
        LAYOUT_TEXT,
        LAYOUT_LINE_ORIGINS,
        LAYOUT_EXTENT,
        LAYOUT_COUNT,
    ] {
        state_clear(runtime, instance, key);
    }
    for key in LAYOUT_RESULT_KEYS {
        state_clear(runtime, instance, key);
    }
}

// ---------------------------------------------------------------------------
// Layout

const RENDER_WIDTH: &str = "width";
const RENDER_HEIGHT: &str = "height";

const LAYOUT_CHARACTERS: &str = "characters";
const LAYOUT_TEXT: &str = "text";
const LAYOUT_LINE_ORIGINS: &str = "lineOrigins";
const LAYOUT_EXTENT: &str = "extent";
const LAYOUT_COUNT: &str = "count";
const LAYOUT_DELAY: &str = "delay";
const LAYOUT_WIDTH_TIME_SCALE: &str = "width_time_scale";

/// `newline()` forces the next `render` to start on a fresh line: the DLL's
/// layout is incremental, this module's is a single pass, so the break is
/// recorded and applied to the next text.
fn newline(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    state_store(runtime, this, NEXT_LINE_BREAK, Variant::Integer(1));
    Ok(Variant::Void)
}

/// `done()` reports that the whole character list exists. The DLL declares a
/// void method because it lays text out across frames; this renderer finishes
/// inside `render`, and the in-repo conductors read the truthy answer to know
/// the characters are materialized (see the module docs).
fn done(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(1))
}

/// `onEval(text)` evaluates an inline expression in the DLL (its implementation
/// runs through `TVPExecuteExpression`). No evaluator is reachable from a
/// plugin in this runtime, so the text is returned unchanged; a script subclass
/// that overrides `onEval` keeps its own definition.
fn on_eval(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let text = arguments(&args, 0).to_tjs_string().unwrap_or_default();
    let _ = runtime;
    Ok(Variant::String(text))
}

fn contains(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Integer(0));
    };
    let x = arguments(&args, 0).to_real().unwrap_or(0.0);
    let y = arguments(&args, 1).to_real().unwrap_or(0.0);
    let inside = x >= value_real(runtime, this, "renderLeft")
        && x <= value_real(runtime, this, "renderRight")
        && y >= value_real(runtime, this, "renderTop")
        && y <= value_real(runtime, this, "renderBottom");
    Ok(Variant::Integer(i64::from(inside)))
}

/// `getKeyWait()` answers the array of wait states the renderer is holding.
/// Rendering is immediate here, so the array is always empty — but it must be
/// an Array: the conductors read `.count` off it straight away.
fn get_key_wait(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

/// `calcLineOffset(line)` — the origin of a laid-out line, 0 when out of range.
fn calc_line_offset(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Real(0.0));
    };
    let line = arguments(&args, 0).to_integer().unwrap_or(0);
    let origins = line_origins(runtime, this);
    let offset = origins
        .get(usize::try_from(line).unwrap_or(usize::MAX))
        .copied()
        .unwrap_or(0.0);
    Ok(Variant::Real(offset))
}

/// `calcShowCount(from, to)` — how many characters sit on the lines in
/// `[from, to)`.
fn calc_show_count(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Integer(0));
    };
    let from = arguments(&args, 0).to_integer().unwrap_or(0).max(0);
    let to = arguments(&args, 1)
        .to_integer()
        .unwrap_or(i64::MAX / 2)
        .max(from);
    let mut count = 0_i64;
    for record in character_records(runtime, this) {
        let Some(record) = record.object_handle() else {
            continue;
        };
        let line = runtime.object_member(record, "line").to_integer().unwrap_or(0);
        if line >= from && line < to {
            count += 1;
        }
    }
    Ok(Variant::Integer(count))
}

/// `getCharacters(from?, to?)` — the character objects. The DLL's command takes
/// two ints; with no arguments every character is returned, which is how the
/// in-repo game scripts call it.
fn get_characters(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Object(runtime.alloc_array_object(Vec::new())));
    };
    let records = character_records(runtime, this);
    if args.is_empty() {
        return Ok(Variant::Object(runtime.alloc_array_object(records)));
    }
    let from = usize::try_from(arguments(&args, 0).to_integer().unwrap_or(0).max(0))
        .unwrap_or(usize::MAX);
    let to = match args.get(1) {
        Some(value) => usize::try_from(value.to_integer().unwrap_or(0).max(0)).unwrap_or(usize::MAX),
        None => records.len(),
    };
    let slice = records
        .into_iter()
        .skip(from)
        .take(to.saturating_sub(from))
        .collect();
    Ok(Variant::Object(runtime.alloc_array_object(slice)))
}

fn get_link_names(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // No engine object tracks link spans, so there are no link names to report.
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

fn get_link_rects(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

fn get_link_characters(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

fn is_link_contains(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn get_link_of_position(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // The DLL answers the link index under the point; with no link model the
    // honest answer is "no link here", which is the miss value scripts test for.
    Ok(Variant::Integer(-1))
}

/// Materialize the character records the subclass consumes. That script owns
/// effect selection and delegates glyph painting to `Layer.drawText`; the
/// native side owns line layout and character geometry.
///
/// Three call shapes are accepted, matching the observed `textrender.dll` usage:
/// - `render(textString, ...)`
/// - `render(msgObject, size, ...)` where the object carries a `text` member
///   (GINKA's scenario message model; it may contain `[ruby,count]` inline
///   annotations, where the ruby covers the following `count + 1` characters)
/// - `render(elm, diff, 0)` — PARQUET's KAGEX path, whose argument 1 is the
///   font size to lay the line out with.
///
/// The DLL's command returns bool; so does this one, with `renderCount`/the
/// character array carrying the numbers.
fn render(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Integer(0));
    };
    // The message model may be a `new` result (a self-bound closure).
    let text = match args.first().and_then(Variant::object_handle) {
        Some(message) => runtime
            .object_member(message, "text")
            .to_tjs_string()
            .unwrap_or_default(),
        None => arguments(&args, 0).to_tjs_string().unwrap_or_default(),
    };
    let text = if state_bool(runtime, this, NEXT_LINE_BREAK) {
        state_store(runtime, this, NEXT_LINE_BREAK, Variant::Integer(0));
        format!("\n{text}")
    } else {
        text
    };
    // textrender.dll measures every glyph through TextRender.onGetTextWidth:
    // the callback sets `this.font.height` and calls Font.getEscWidthX (or
    // Font.getTextWidth). `font` belongs to the TextRender instance, not the
    // render arguments.
    let font = runtime.object_member(this, "font").object_handle();
    // An explicit numeric size argument wins over the instance font and the
    // `defaultFontSize` property.
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
        .unwrap_or_else(|| value_real(runtime, this, "defaultFontSize") as i64)
        .max(1);
    let font_scale = value_real(runtime, this, "fontScale");
    let font_size = ((font_size as f64) * font_scale).round().max(1.0) as i64;
    let color = font
        .and_then(|font| resolve_font_int(runtime, font, &["color"]))
        .unwrap_or_else(|| value_int(runtime, this, "defaultChColor"));
    let width = state_int(runtime, this, RENDER_WIDTH).unwrap_or(0).max(0);
    let line_size = value_real(runtime, this, "defaultLineSize").max(font_size as f64);
    let line_spacing = value_real(runtime, this, "defaultLineSpacing").max(0.0);
    let pitch = value_real(runtime, this, "defaultPitch");
    let line_height = (line_size + line_spacing).max(1.0);
    let ruby_size = value_real(runtime, this, "defaultRubySize")
        .max(font_size as f64 / RUBY_FONT_DIVISOR)
        .max(1.0);
    let time_scale = value_real(runtime, this, "timeScale");
    let width_time_scale = state_bool(runtime, this, LAYOUT_WIDTH_TIME_SCALE);
    let bold = value_bool(runtime, this, "defaultBold");
    let italic = value_bool(runtime, this, "defaultItalic");
    let shadow = value_bool(runtime, this, "defaultShadow");
    let shadow_color = value_int(runtime, this, "defaultShadowColor");
    let shadow_diff = value_int(runtime, this, "defaultShadowDiff");
    let edge = value_bool(runtime, this, "defaultEdge");
    let edge_color = value_int(runtime, this, "defaultEdgeColor");
    let face = value_text(runtime, this, "defaultFace");
    let vertical = value_bool(runtime, this, "vertical");

    let mut x = 0_i64;
    let mut y = 0_i64;
    let mut line = 0_i64;
    let mut lines = 1_i64;
    let mut delay = 0.0_f64;
    let mut records = Vec::new();
    let mut line_origins = vec![0.0_f64];
    let mut min_left: Option<i64> = None;
    let mut max_right = 0_i64;
    let mut max_bottom = 0_i64;
    // Ruby group tracking: `[ruby,count]` covers the following count + 1 base
    // characters. The ruby record (a whole-string annotation dictionary, as
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
            y = y.saturating_add(line_height as i64);
            line += 1;
            lines += 1;
            line_origins.push(y as f64);
            continue;
        }
        let character = character.to_string();
        let char_width = measure_character_width(runtime, this, font, &character, font_size)
            .unwrap_or(font_size)
            .max(1);
        if width > 0 && x > 0 && x.saturating_add(char_width) > width {
            x = 0;
            y = y.saturating_add(line_height as i64);
            line += 1;
            lines += 1;
            line_origins.push(y as f64);
        }
        let in_ruby_group = ruby_remaining > 0;
        let record = runtime.alloc_dictionary_object();
        // Reference character-object names (the DLL's character objects carry
        // these; `x`/`y`/`cw`/`line` are the engine-facing aliases the in-repo
        // game scripts read).
        runtime.set_object_member(record, "text", Variant::String(character.clone()));
        runtime.set_object_member(record, "graph", Variant::Void);
        runtime.set_object_member(record, "face", Variant::String(face.clone()));
        runtime.set_object_member(record, "size", Variant::Integer(font_size));
        runtime.set_object_member(record, "italic", Variant::Integer(i64::from(italic)));
        runtime.set_object_member(record, "bold", Variant::Integer(i64::from(bold)));
        runtime.set_object_member(record, "color", Variant::Integer(color));
        runtime.set_object_member(record, "shadow", Variant::Integer(i64::from(shadow)));
        runtime.set_object_member(record, "shadowColor", Variant::Integer(shadow_color));
        runtime.set_object_member(record, "shadowDiff", Variant::Integer(shadow_diff));
        runtime.set_object_member(record, "edge", Variant::Integer(i64::from(edge)));
        runtime.set_object_member(record, "edgeColor", Variant::Integer(edge_color));
        // Character timing: `delay` is this glyph's own wait and `time` the wait
        // accumulated before it; `width_time_scale` charges the wait by advance
        // width instead of one tick per glyph, and `renderDelay` reports the
        // total times `timeScale`.
        runtime.set_object_member(
            record,
            "delay",
            Variant::Real(if width_time_scale {
                char_width as f64
            } else {
                1.0
            }),
        );
        runtime.set_object_member(record, "time", Variant::Real(delay));
        runtime.set_object_member(record, "link", Variant::Integer(0));
        runtime.set_object_member(record, "linkName", Variant::String(String::new()));
        runtime.set_object_member(record, "left", Variant::Integer(x));
        runtime.set_object_member(record, "width", Variant::Integer(char_width));
        runtime.set_object_member(record, "height", Variant::Integer(font_size));
        runtime.set_object_member(record, "vertical", Variant::Integer(i64::from(vertical)));
        runtime.set_object_member(record, "x", Variant::Integer(x));
        runtime.set_object_member(record, "y", Variant::Integer(y));
        runtime.set_object_member(record, "cw", Variant::Integer(char_width));
        runtime.set_object_member(record, "line", Variant::Integer(line));
        runtime.set_object_member(record, "index", Variant::Integer(records.len() as i64));
        records.push(Variant::Object(record));
        delay += if width_time_scale {
            char_width as f64
        } else {
            1.0
        };
        min_left = Some(min_left.map_or(x, |left| left.min(x)));
        max_right = max_right.max(x.saturating_add(char_width));
        max_bottom = max_bottom.max(y.saturating_add(line_height as i64));
        x = x.saturating_add(char_width);
        x = x.saturating_add(pitch as i64);
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
                attach_ruby(runtime, first, &ruby, group_base_width, ruby_size);
            }
        }
    }
    // A trailing annotation whose group never completed still gets its ruby
    // attached to the first covered character.
    if let (Some(first), Some(ruby)) = (group_first_record, pending_ruby)
        && ruby_remaining > 0
    {
        attach_ruby(runtime, first, &ruby, group_base_width, ruby_size);
    }
    let count = records.len() as i64;
    let characters = runtime.alloc_array_object(records);
    state_store(
        runtime,
        this,
        LAYOUT_CHARACTERS,
        Variant::Object(characters),
    );
    state_store(
        runtime,
        this,
        LAYOUT_TEXT,
        Variant::String(text.clone()),
    );
    state_store(runtime, this, LAYOUT_COUNT, Variant::Integer(count));
    state_store(runtime, this, LAYOUT_DELAY, Variant::Real(delay * time_scale));
    state_store(runtime, this, "renderCount", Variant::Integer(count));
    state_store(runtime, this, "renderLines", Variant::Integer(lines));
    let origins = line_origins
        .into_iter()
        .map(Variant::Real)
        .collect::<Vec<_>>();
    let origins = runtime.alloc_array_object(origins);
    state_store(
        runtime,
        this,
        LAYOUT_LINE_ORIGINS,
        Variant::Object(origins),
    );
    // The content extent is the axis the DLL scrolls: the laid-out width while
    // vertical, the laid-out height otherwise.
    let extent = if vertical {
        max_right as f64
    } else {
        max_bottom as f64
    };
    state_store(runtime, this, LAYOUT_EXTENT, Variant::Real(extent));
    // `renderOver`: the content did not fit the render box the script set.
    let height = state_int(runtime, this, RENDER_HEIGHT).unwrap_or(0).max(0);
    let over = (height > 0 && max_bottom > height) || (width > 0 && max_right > width);
    state_store(runtime, this, "renderOver", Variant::Integer(i64::from(over)));
    state_store(runtime, this, "renderDelay", Variant::Real(delay * time_scale));
    state_store(runtime, this, "renderText", Variant::String(text));
    state_store(
        runtime,
        this,
        "renderLeft",
        Variant::Real(min_left.unwrap_or(0) as f64),
    );
    state_store(runtime, this, "renderTop", Variant::Real(0.0));
    state_store(runtime, this, "renderRight", Variant::Real(max_right as f64));
    state_store(runtime, this, "renderBottom", Variant::Real(max_bottom as f64));
    Ok(Variant::Integer(1))
}

fn attach_ruby(
    runtime: &mut Runtime<KrkrHost>,
    first: ObjectHandle,
    ruby: &str,
    group_base_width: i64,
    ruby_size: f64,
) {
    let ruby_record = runtime.alloc_dictionary_object();
    let ruby_width = ruby.chars().count() as i64 * ruby_size as i64;
    let ruby_x = (group_base_width.max(ruby_width) - ruby_width) / 2;
    runtime.set_object_member(ruby_record, "text", Variant::String(ruby.to_string()));
    runtime.set_object_member(ruby_record, "x", Variant::Integer(ruby_x));
    runtime.set_object_member(
        ruby_record,
        "left",
        Variant::Integer(ruby_x),
    );
    runtime.set_object_member(
        ruby_record,
        "y",
        Variant::Integer(-(ruby_size as i64)),
    );
    runtime.set_object_member(ruby_record, "size", Variant::Integer(ruby_size as i64));
    runtime.set_object_member(first, "ruby", Variant::Object(ruby_record));
}

fn character_records(runtime: &Runtime<KrkrHost>, instance: ObjectHandle) -> Vec<Variant> {
    match state_member(runtime, instance, LAYOUT_CHARACTERS) {
        Variant::Object(handle) => runtime
            .array_elements(handle)
            .map(<[Variant]>::to_vec)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn line_origins(runtime: &Runtime<KrkrHost>, instance: ObjectHandle) -> Vec<f64> {
    match state_member(runtime, instance, LAYOUT_LINE_ORIGINS) {
        Variant::Object(handle) => runtime
            .array_elements(handle)
            .map(|elements| {
                elements
                    .iter()
                    .map(|value| value.to_real().unwrap_or(0.0))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `maxScrollOffset` — the DLL subtracts the render origin from the content
/// extent on the scrolled axis (`FUN_10010eb0`).
fn max_scroll_offset(
    runtime: &mut Runtime<KrkrHost>,
    instance: ObjectHandle,
) -> Result<Variant> {
    let extent = state_real(runtime, instance, LAYOUT_EXTENT).unwrap_or(0.0);
    let origin = if value_bool(runtime, instance, "vertical") {
        value_real(runtime, instance, "renderLeft")
    } else {
        value_real(runtime, instance, "renderTop")
    };
    Ok(Variant::Real(extent - origin))
}

/// `maxScrollLine` — the DLL walks its line records backwards, consuming the
/// extent on the scrolled axis (`FUN_10010ee0`); the same walk over this
/// module's line origins.
fn max_scroll_line(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle) -> Result<Variant> {
    let origins = line_origins(runtime, instance);
    let extent = state_real(runtime, instance, LAYOUT_EXTENT).unwrap_or(0.0);
    let mut offset = extent;
    let mut index = origins.len() as i64 - 1;
    while index >= 0 {
        let origin = origins[index as usize];
        if origin > 0.0 && offset > origin {
            offset -= origin;
            index -= 1;
        } else {
            break;
        }
    }
    Ok(Variant::Integer(if index < 0 { 0 } else { index + 1 }))
}

/// The argument at `index`, or void when the caller left it out (ncbind pads a
/// short call with void, so this mirrors what the DLL sees).
fn arguments(args: &[Variant], index: usize) -> Variant {
    args.get(index).cloned().unwrap_or(Variant::Void)
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
/// a font or apply a scale in TJS. GINKA's implementation writes the current
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
        .and_then(|width| {
            if matches!(width, Variant::Void) {
                None
            } else {
                width.to_integer().ok()
            }
        })
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

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::{NativePropertyAccess, ObjectHandle, Variant};

    use super::{MemberKind, SURFACE, TextRenderPlugin};

    /// Run a script against an engine with this plugin registered and return the
    /// string form of its result.
    fn run(script: &str) -> String {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(TextRenderPlugin)
            .expect("register plugin");
        engine
            .execute_script("probe.tjs", script)
            .expect("script")
            .to_tjs_string()
            .expect("string result")
    }

    /// The checklist from the M27 dossier, independently typed: the 55 names in
    /// the DLL's registration order with `true` for methods and `false` for
    /// properties (the binary's command objects decide the kind).
    const REFERENCE_SURFACE: &[(&str, bool)] = &[
        ("setOption", true),
        ("setDefault", true),
        ("setRenderSize", true),
        ("vertical", false),
        ("timeScale", false),
        ("fontScale", false),
        ("clear", true),
        ("resetFont", true),
        ("resetStyle", true),
        ("setFont", true),
        ("setStyle", true),
        ("render", true),
        ("newline", true),
        ("done", true),
        ("onEval", true),
        ("renderOver", false),
        ("renderLines", false),
        ("renderCount", false),
        ("renderDelay", false),
        ("renderLeft", false),
        ("renderTop", false),
        ("renderRight", false),
        ("renderBottom", false),
        ("contains", true),
        ("renderText", false),
        ("maxScrollOffset", false),
        ("maxScrollLine", false),
        ("getKeyWait", true),
        ("calcLineOffset", true),
        ("calcShowCount", true),
        ("getCharacters", true),
        ("getLinkNames", true),
        ("getLinkRects", true),
        ("getLinkCharacters", true),
        ("isLinkContains", true),
        ("getLinkOfPosition", true),
        ("defaultFace", false),
        ("defaultFontSize", false),
        ("defaultBigFontSize", false),
        ("defaultSmallFontSize", false),
        ("defaultLineSize", false),
        ("defaultLineSpacing", false),
        ("defaultPitch", false),
        ("defaultAlign", false),
        ("defaultValign", false),
        ("defaultRubySize", false),
        ("defaultRubyOffset", false),
        ("defaultChColor", false),
        ("defaultShadow", false),
        ("defaultShadowColor", false),
        ("defaultShadowDiff", false),
        ("defaultEdge", false),
        ("defaultEdgeColor", false),
        ("defaultBold", false),
        ("defaultItalic", false),
    ];

    /// The get-only properties: the DLL registers them with a null setter, so a
    /// script write is denied.
    const GET_ONLY: &[&str] = &[
        "renderOver",
        "renderLines",
        "renderCount",
        "renderDelay",
        "renderLeft",
        "renderTop",
        "renderRight",
        "renderBottom",
        "renderText",
        "maxScrollOffset",
        "maxScrollLine",
    ];

    #[test]
    fn surface_is_the_reference_55_in_registration_order() {
        assert_eq!(SURFACE.len(), 55, "the DLL registers 55 members");
        assert_eq!(
            REFERENCE_SURFACE.len(),
            SURFACE.len(),
            "the checklist covers every member"
        );
        for (index, ((name, is_method), member)) in
            REFERENCE_SURFACE.iter().zip(SURFACE).enumerate()
        {
            assert_eq!(member.name, *name, "member {index} name");
            let method = matches!(member.kind, MemberKind::Method { .. });
            assert_eq!(
                method, *is_method,
                "member {name}: kind differs from the DLL's command object"
            );
            assert!(
                !member.signature.is_empty(),
                "member {name} carries its reference signature"
            );
        }
    }

    #[test]
    fn registered_members_match_the_checklist_with_reference_kinds() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(TextRenderPlugin)
            .expect("register plugin");
        let runtime = engine.tjs_runtime();
        let Variant::Object(class) = runtime.global_member("TextRenderBase") else {
            panic!("TextRenderBase is a class object");
        };
        let members = runtime.object_members(class);
        let mut names = members.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>();
        names.sort();
        let mut expected = REFERENCE_SURFACE
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect::<Vec<_>>();
        // `finalize` comes from the DLL's class auto-registration, and the five
        // callback slots are this module's documented engine-compat members.
        expected.push("finalize".to_string());
        expected.extend(
            super::ENGINE_COMPAT_MEMBERS
                .iter()
                .map(|name| (*name).to_string()),
        );
        expected.sort();
        assert_eq!(names, expected);
        for (name, is_method) in REFERENCE_SURFACE {
            let member = runtime
                .object_member(class, name)
                .object_handle()
                .unwrap_or_else(|| panic!("{name} is registered"));
            if *is_method {
                assert!(
                    runtime.variant_is_native_function(&Variant::Object(member)),
                    "{name} is registered as a method"
                );
            } else {
                assert!(
                    runtime.variant_is_native_property(&Variant::Object(member)),
                    "{name} is registered as a property"
                );
                let expected_access = if GET_ONLY.contains(name) {
                    NativePropertyAccess::ReadOnly
                } else {
                    NativePropertyAccess::ReadWrite
                };
                assert_eq!(
                    runtime.native_property_access(member),
                    Some(expected_access),
                    "{name} access policy"
                );
            }
        }
    }

    /// The constructor defaults the DLL writes (`FUN_1000d5e0` and the
    /// `.rdata` constants 0x1002c700-0x1002c728).
    #[test]
    fn properties_start_with_the_reference_defaults() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            // TJS prints a real zero as "+0.0" (`real_to_string`), so the float
            // members are compared as numbers and reported as 0/1.
            function zero(value) { return value == 0 ? 1 : 0; }
            return render.vertical + "/" + render.timeScale + "/" + render.fontScale + "/"
                + render.renderOver + "/" + render.renderLines + "/" + render.renderCount + "/"
                + zero(render.renderDelay) + "/" + zero(render.renderLeft) + "/" + zero(render.renderTop) + "/"
                + zero(render.renderRight) + "/" + zero(render.renderBottom) + "/" + render.renderText + "/"
                + zero(render.maxScrollOffset) + "/" + render.maxScrollLine + "/"
                + render.defaultFace + "/" + render.defaultFontSize + "/" + render.defaultBigFontSize + "/"
                + render.defaultSmallFontSize + "/" + render.defaultLineSize + "/" + render.defaultLineSpacing + "/"
                + zero(render.defaultPitch) + "/" + render.defaultAlign + "/" + render.defaultValign + "/"
                + render.defaultRubySize + "/" + render.defaultRubyOffset + "/" + render.defaultChColor + "/"
                + render.defaultShadow + "/" + render.defaultShadowColor + "/" + render.defaultShadowDiff + "/"
                + render.defaultEdge + "/" + render.defaultEdgeColor + "/" + render.defaultBold + "/"
                + render.defaultItalic;
            "#,
        );
        assert_eq!(
            value,
            "0/1/1/0/0/0/1/1/1/1/1//1/0/normal/24/48/12/24/6/1/-1/-1/10/-2/4294967295/1/4278190080/1/0/4278223103/0/0",
        );
    }

    /// Every `setOption` key the DLL compares (`FUN_10001a70`) reaches the
    /// render object's state, and a key the DLL does not know changes nothing.
    #[test]
    fn set_option_accepts_the_reference_keys_and_ignores_unknown_ones() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            render.setOption(%[
                "begin" => "b", "following" => "f", "leading" => "l",
                "vertical" => 1, "kinsoku_max" => 7, "word_break" => 0,
                "ignore_color" => 1, "ignore_size" => 1, "ignore_delay" => 1,
                "ignore_over" => 1, "ignore_overy" => 1, "ignore_overx" => 1,
                "ignore_ruby" => 1, "ignore_type" => 1, "ignore_face" => 1,
                "ignore_style" => 1, "ignore_xr" => 1, "width_time_scale" => 1,
                "end" => 1, "bogus" => 1
            ]);
            var state = render.__krkr_text_render;
            return render.vertical + "/" + state.begin + "/" + state.following + "/" + state.leading + "/"
                + state.kinsoku_max + "/" + state.word_break + "/" + state.ignore_color + "/"
                + state.ignore_size + "/" + state.ignore_delay + "/" + state.ignore_over + "/"
                + state.ignore_overy + "/" + state.ignore_overx + "/" + state.ignore_ruby + "/"
                + state.ignore_type + "/" + state.ignore_face + "/" + state.ignore_style + "/"
                + state.ignore_xr + "/" + state.width_time_scale + "/" + (state.end === void) + "/"
                + (state.bogus === void);
            "#,
        );
        assert_eq!(value, "1/b/f/l/7/0/1/1/1/1/1/1/1/1/1/1/1/1/1/1");
    }

    /// The style keys (`FUN_100022f0`) write the members the `default*`
    /// properties read, `fontsize` derives its dependents, and unknown keys are
    /// ignored.
    #[test]
    fn set_default_writes_the_reference_style_keys_and_derives_sizes() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            render.setDefault(%[
                "face" => "MS Gothic", "color" => 0x123456, "shadow" => 0, "shadowcolor" => 0xff00ff,
                "shadowdiff" => 4, "edge" => 1, "edgecolor" => 0x00ff00, "bold" => 1,
                "linespacing" => 9, "pitch" => 3, "align" => 1, "valign" => 2,
                "rubyoffset" => -3, "bogus" => 1
            ]);
            var before = render.defaultFontSize;
            render.setDefault(%["fontsize" => 30]);
            return render.defaultFace + "/" + render.defaultChColor + "/" + render.defaultShadow + "/"
                + render.defaultShadowColor + "/" + render.defaultShadowDiff + "/" + render.defaultEdge + "/"
                + render.defaultEdgeColor + "/" + render.defaultBold + "/" + render.defaultLineSpacing + "/"
                + render.defaultPitch + "/" + render.defaultAlign + "/" + render.defaultValign + "/"
                + render.defaultRubyOffset + "/" + before + "/" + render.defaultFontSize + "/"
                + render.defaultBigFontSize + "/" + render.defaultSmallFontSize + "/"
                + render.defaultLineSize + "/" + render.defaultRubySize;
            "#,
        );
        assert_eq!(
            value,
            "MS Gothic/1193046/0/16711935/4/1/65280/1/9/3/1/2/-3/24/30/60/15/30/12.5"
        );
    }

    /// A `setDefault` without `linesize`/`linespacing` keeps the DLL's
    /// constructor defaults for them; the derived ones follow `fontsize`.
    #[test]
    fn set_default_keeps_reference_defaults_for_unset_keys() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            render.setDefault(%["fontsize" => 20, "bigfontsize" => 50, "smallfontsize" => 5]);
            return render.defaultFontSize + "/" + render.defaultBigFontSize + "/"
                + render.defaultSmallFontSize + "/" + render.defaultLineSize + "/"
                + render.defaultLineSpacing + "/" + Math.floor(render.defaultRubySize * 1000) / 1000;
            "#,
        );
        assert_eq!(value, "20/50/5/20/6/8.333");
    }

    /// The DLL's get-only properties reject a script write the way a null
    /// setter does, and a writable property round-trips.
    #[test]
    fn result_properties_are_read_only_and_style_properties_round_trip() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            function denied(block) { try { block(); } catch (e) { return "denied"; } return "allowed"; }
            var reads = render.renderCount;
            render.setOption(%["vertical" => 1]);
            render.defaultFontSize = 30;
            var written = render.vertical + "/" + render.defaultFontSize;
            // A void store reaches the float setter as 0, exactly like ncbind's
            // argument conversion.
            render.defaultFontSize = void;
            return reads + "/" + denied(function() { render.renderCount = 5; }) + "/"
                + denied(function() { render.renderLeft = 5; }) + "/"
                + denied(function() { render.maxScrollOffset = 5; }) + "/"
                + denied(function() { render.renderText = "x"; }) + "/"
                + denied(function() { render.renderLines = 5; }) + "/" + written + "/"
                + (render.defaultFontSize == 0);
            "#,
        );
        assert_eq!(value, "0/denied/denied/denied/denied/denied/1/30/1");
    }

    /// Calling a method with fewer arguments than the reference command needs
    /// fails with the engine's parameter-count error, like ncbind's check.
    #[test]
    fn methods_enforce_their_reference_argument_contracts() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            function rejects(block) { try { block(); } catch (e) { return "rejected"; } return "accepted"; }
            render.setOption(%["vertical" => 0]);
            render.setDefault(%["face" => "normal"]);
            render.setRenderSize(100, 50);
            return rejects(function() { render.setOption(); }) + "/"
                + rejects(function() { render.setDefault(); }) + "/"
                + rejects(function() { render.setRenderSize(); }) + "/"
                + rejects(function() { render.render(); }) + "/"
                + rejects(function() { render.contains(1); }) + "/"
                + rejects(function() { render.getLinkOfPosition(1); }) + "/"
                + rejects(function() { render.onEval(); });
            "#,
        );
        assert_eq!(value, "rejected/rejected/rejected/rejected/rejected/rejected/rejected");
    }

    /// The layout end-to-end through the engine's font path: glyph advances come
    /// from the Font object a game hands to `setFont`, wrapping honours
    /// `setRenderSize`, and the result properties report the laid-out block.
    #[test]
    fn render_lays_out_characters_through_the_engine_font() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            var started = render.render("あいうえお");
            var glyph = font.getEscWidthX("あ");
            var chars = render.getCharacters();
            var first = chars[0];
            var last = chars[4];
            var waits = render.getKeyWait();
            var right = render.renderRight;
            var text = render.renderText;
            var inside = render.contains(1, 1);
            var outside = render.contains(10000, 10000);
            render.setRenderSize(60, 0);
            var wrapped = render.render("ああああああ");
            var lines = render.renderLines;
            render.resetFont();
            render.setRenderSize(400, 0);
            render.render("ab");
            var unwrappedLines = render.renderLines;
            return started + "/" + glyph + "/" + chars.count + "/" + first.text + "/" + first.size + "/"
                + first.width + "/" + first.left + "/" + first.y + "/" + last.left + "/" + right + "/"
                + text + "/" + waits.count + "/" + wrapped + "/" + lines + "/" + inside + "/"
                + outside + "/" + unwrappedLines;
            "#,
        );
        let parts = value.split('/').collect::<Vec<_>>();
        let glyph = parts[1].parse::<i64>().expect("glyph advance");
        assert!(glyph > 0, "the Font must measure the glyph: {value}");
        assert_eq!(parts[0], "1", "render reports success");
        assert_eq!(parts[2], "5", "one character object per glyph");
        assert_eq!(parts[3], "あ", "the character text is carried");
        assert_eq!(parts[4], "20", "the font height sizes the glyphs");
        assert_eq!(
            parts[5],
            glyph.to_string(),
            "the character advance is the Font's own advance"
        );
        assert_eq!(parts[6], "0", "the first glyph sits at the origin");
        assert_eq!(parts[7], "0", "on the first line");
        assert_eq!(
            parts[8],
            (glyph * 4).to_string(),
            "each glyph advances by its own width"
        );
        assert_eq!(
            parts[9],
            (glyph * 5).to_string(),
            "renderRight is the laid-out width"
        );
        assert_eq!(parts[10], "あいうえお", "renderText keeps the source text");
        assert_eq!(parts[11], "0", "no key waits the conductor must resolve");
        assert_eq!(parts[12], "1", "render returns the DLL's bool");
        let per_line = (60 / glyph).max(1);
        let expected_lines = 6_usize.div_ceil(usize::try_from(per_line).expect("per line"));
        assert_eq!(
            parts[13],
            expected_lines.to_string(),
            "six glyphs of {glyph} px wrap inside a 60 px box: {value}"
        );
        assert_eq!(parts[14], "1", "a point inside the block hits");
        assert_eq!(parts[15], "0", "a point outside the block misses");
        assert_eq!(parts[16], "1", "without a font the default size lays out one line");
    }

    /// The option keys the layout acts on: `width_time_scale` charges the delay
    /// by glyph width, and `timeScale` multiplies the reported total — the
    /// behaviour `renderDelay` exists for.
    #[test]
    fn render_delay_follows_width_time_scale_and_time_scale() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("ab");
            var per_char = render.renderDelay;
            render.setOption(%["width_time_scale" => 1]);
            render.render("ab");
            var per_width = render.renderDelay;
            render.timeScale = 2;
            render.render("ab");
            var scaled = render.renderDelay;
            return per_char + "/" + (per_width > per_char) + "/" + (scaled > per_width);
            "#,
        );
        assert_eq!(value, "2/1/1");
    }

    /// `clear` empties the character list, `newline` starts the next render on a
    /// fresh line and `resetStyle`/`resetFont` drop the style members the DLL's
    /// readers clear.
    #[test]
    fn clear_newline_and_resets_follow_the_reference_reset_sets() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("abc");
            var before = render.renderCount;
            render.clear();
            var cleared = render.renderCount;
            render.setFont(font);
            render.render("a");
            render.newline();
            render.render("b");
            var after_break = render.renderLines;
            var chars = render.getCharacters();
            // `newline` breaks the layout before the next text, so the second
            // render holds the one glyph that follows the break.
            var break_y = chars[0].y;
            render.setDefault(%["fontsize" => 30, "face" => "X", "color" => 0x111111]);
            render.resetStyle();
            var style = render.defaultFontSize + "/" + render.defaultFace + "/" + render.defaultChColor;
            render.setDefault(%["fontsize" => 30, "pitch" => 4]);
            render.resetFont();
            var font_reset = render.defaultFontSize + "/" + (render.defaultPitch == 0);
            var sliced = render.getCharacters(0, 1).count;
            return before + "/" + cleared + "/" + chars.count + "/" + after_break + "/"
                + (break_y > 0) + "/" + style + "/" + font_reset + "/" + sliced + "/"
                + render.getLinkNames().count + "/" + render.getLinkRects(0).count + "/"
                + render.getLinkCharacters(0).count + "/" + render.isLinkContains(0, 1, 1) + "/"
                + render.getLinkOfPosition(1, 1);
            "#,
        );
        assert_eq!(
            value,
            "3/0/1/2/1/24/normal/4294967295/24/1/1/0/0/0/0/-1"
        );
    }

    /// The script-facing members a subclass relies on: `vertical` read through
    /// the property (the bytecode reads it as a bare symbol), `onEval` on a
    /// subclass, and the character objects' callback slots.
    #[test]
    fn subclass_reads_vertical_and_keeps_its_own_callbacks() {
        let value = run(
            r#"
            class ProbeRender extends TextRenderBase {
                function ProbeRender() {
                    TextRenderBase.TextRenderBase();
                }
                function readVertical() {
                    return vertical;
                }
                function onEval(text) {
                    return "eval:" + text;
                }
            }
            var render = new ProbeRender();
            var before = render.readVertical();
            render.setOption(%["vertical" => 1]);
            var literal = render.readVertical();
            // The script-built dictionary arrives as a self-bound closure; the
            // property is a bool like the DLL's, so any truthy value reads 1.
            var options = new Dictionary();
            options.vertical = 2;
            render.setOption(options);
            var via_dictionary = render.readVertical();
            return before + "/" + literal + "/" + via_dictionary + "/" + render.onEval("1+1") + "/"
                + (render.onGetTextWidth === void);
            "#,
        );
        assert_eq!(value, "0/1/1/eval:1+1/1");
    }

    /// Members outside the 55 the dossier inventoried: the three a previous
    /// iteration invented as methods (`renderOver`, `renderText`,
    /// `getLinkOfPosition`) are properties or methods exactly as the DLL
    /// registers them, and no member the DLL lacks is registered.
    #[test]
    fn invented_members_from_the_earlier_stub_are_gone() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(TextRenderPlugin)
            .expect("register plugin");
        let runtime = engine.tjs_runtime();
        let Variant::Object(class) = runtime.global_member("TextRenderBase") else {
            panic!("TextRenderBase is a class object");
        };
        let member = |name: &str| -> Option<ObjectHandle> {
            runtime.object_member(class, name).object_handle()
        };
        assert!(runtime.variant_is_native_property(&Variant::Object(
            member("renderText").expect("renderText")
        )));
        assert!(runtime.variant_is_native_property(&Variant::Object(
            member("renderOver").expect("renderOver")
        )));
        assert!(runtime.variant_is_native_function(&Variant::Object(
            member("getLinkOfPosition").expect("getLinkOfPosition")
        )));
        assert!(runtime.variant_is_native_function(&Variant::Object(
            member("contains").expect("contains")
        )));
        // The earlier stub registered `setRender` as a method; the DLL has no
        // such member (M27 corrected the M24 census line).
        assert!(member("setRender").is_none());
    }
}
