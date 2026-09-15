//! Compatibility implementation for wamsoft `textrender.dll`
//! (`TextRenderBase` class).
//!
//! Games normally subclass this object in TJS
//! (`class TextRender extends TextRenderBase`), supply the layout callbacks and
//! paint the glyph records `getCharacters(from, count)` hands back into Layers.
//! Returning an empty list here is therefore not a harmless stub: it advances
//! the scenario while drawing no dialogue at all.
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
//! # Reference defaults and the active style
//!
//! Every property starts at the value the DLL's constructor writes
//! (`FUN_1000d5e0`, `.rdata` constants `0x1002c700`-`0x1002c728`): `face`
//! "normal", font size 24, big 48, small 12, line size 24, line spacing 6,
//! pitch 0, ruby size 10, ruby offset -2, text color `0xffffffff`, shadow on
//! with color `0xff000000` and diff 1, edge off with color `0xff0080ff`,
//! align/valign -1, `timeScale`/`fontScale` 1.0. `setDefault` derives
//! `bigfontsize` (2x), `smallfontsize` (0.5x), `rubysize` (`/3.0`) and
//! `linesize` from `fontsize` when the caller leaves them out, exactly as
//! `FUN_100022f0` does.
//!
//! The DLL keeps a second, *active* copy of the style members that the layout
//! and the character objects read, and the `set*`/`reset*` commands drive it:
//!
//! - `setFont(dict)` (`FUN_10002980`) reads the character attributes
//!   `face bold fontsize rubysize rubyoffset color shadow shadowcolor
//!   shadowdiff edge edgecolor` into the active members;
//! - `setStyle(dict)` (`FUN_10002e30`) reads the layout keys `linespacing pitch
//!   linesize align valign` into the active members;
//! - `resetFont()` (`FUN_1000def0`) and `resetStyle()` (`FUN_1000dff0`) **copy
//!   the stored defaults into the active members** — they are what a script
//!   pairs with `defaultFace = …` (`system/LangRender.tjs`:
//!   `defaultFace = kag.getLanguageFont(a5), resetFont()` and
//!   `defaultLineSpacing = …, resetStyle()`), and they never clear the
//!   `default*` properties.
//!
//! The module holds the active copy in a nested Dictionary under the state
//! member's `active` key; a value the layout has no active override for falls
//! back to the property's reference default.
//!
//! The two shipped builds differ by one style key, and the module accepts it:
//! build A (GINKA/少女世界, md5 `5aa3b6c8…`) reads `wordbreak` in `setDefault`
//! (into the stored word-break bool) and in `setStyle` (into the active one),
//! while build B (PARQUET, md5 `2213af66…`) has no `wordbreak` string in the
//! binary at all. No shipped script sends the key — M163's scan of both games'
//! archives and scenario files found zero hits — so this module takes the
//! union, exactly as it does for `render`'s five-versus-six arity: build A's
//! behaviour for a script that does send it, build B's for everything either
//! game runs. `word_break` is a `setOption` key in both builds and stays one.
//!
//! The same constructor also seeds the line-breaking character sets the
//! `setOption` keys replace (`following` 68 characters, `leading` 19,
//! `begin`/`end` 10 each) and `kinsoku_max` 1 with `word_break` on; those are
//! stored when a script passes them but not applied yet — see below.
//!
//! # The message text format
//!
//! `render`'s text is not plain text. The games' `TagTextConverter`
//! (`system\TagTextConverter.tjs`, an identical copy in both in-repo games)
//! turns KAG tags into escape sequences, `option.tjs` rewrites a raw newline
//! as the two characters `\n` (`split("\n").join(a1 + "\\n")`, GINKA
//! `sysscn\option.tjs` :4218), and `textrender.dll` interprets the result
//! while it lays text out. [`parse_message_text`] implements that grammar;
//! the anchors are the DLL's layout walker `FUN_1000b8c0` (one `switch` arm
//! per directive character; the addresses below are decimal member offsets in
//! its pseudocode) and the converter functions that emit each code:
//!
//! - `\n` — line break (`case 0x5c`/`n`, the same arm a raw 0x0A takes) —
//!   `parseR` :160 of the decompiled converter.
//! - `\k` — a click wait (`case 0x5c`/`k` records the position in the vector
//!   `getKeyWait` reports through `pos`/`time`) — `parseL` :163.
//! - `\w` — one blank character cell (the `k` arm's neighbour advances the pen
//!   by member 0x47, the line advance, and inserts nothing) — `parseSP` :176.
//! - `\x` — nothing at all (`case 0x5c`/`x` only clears the two walk flags) —
//!   `parseNUL` :173.
//! - `\i` / `\r` — the indent markers (`case 0x5c`/`i` calls `FUN_1000d8b0`,
//!   which sets the line-origin member 0x94 from the box's origin; `r` zeroes
//!   it) — `parseIndent` :179 / `parseEndIndent` :182.
//! - `\` + any other character — that character (`\[`, `\%`, `\#`, `\&`, `\$`
//!   when the games quote user text; `option.tjs` :4225 escapes exactly the
//!   set `[ \ % # & $`).
//! - `[ruby]` / `[ruby,count]` — a ruby annotation over the following
//!   `count + 1` characters (`case 0x5b`, `FUN_1000b180`) — `parseCh` :107-122.
//! - `$expr;` / `${expr}` — inline evaluation through `onEval`
//!   (`case 0x24`); the game overrides `onEval` with `Scripts.eval`
//!   (`system\TextRender.tjs` :23) — `parseEmb` :192-200.
//! - `&name;` — an inline image; the DLL calls `onGetGraphSize(name)` and
//!   reads `width`/`height` off its answer (`FUN_100154a0`, `case 0x26`) —
//!   `parseGraph` :185.
//! - `#rrggbb;` / `#;` — the text colour (member 0x4b; `case 0x23` ORs
//!   0xff000000 and falls back to the default colour for an empty value) —
//!   `addColor` :223.
//! - `%f<face>;` / `%f;` — the face, or the default face (`case 0x66`, which
//!   compares the parsed name against the empty string at 0x10027340) —
//!   `parseFont` :349-357.
//! - `%r` — reset the font attributes to the defaults (`case 0x72` →
//!   `FUN_1000d440`, the same copy `resetFont` performs) — `parseResetfont`
//!   :202.
//! - `%<percent>;` / `%;` / `%B` / `%S` — glyph size: the default size times
//!   percent/100 (the `.rdata` single 100.0 at 0x100277d0), the default size,
//!   the big size, the small size (`case 0x25`, the digit arm and B/S) —
//!   `parseFont` :288-323.
//! - `%b`/`%i`/`%s`/`%e` with `0`, `1` or `d` — bold/italic/shadow/edge on,
//!   off or the default (`case 0x25`; `addFont` :210-222 writes the three
//!   characters) — plus the `%e#…;` / `%s#…;` colour forms.
//! - `%p<n>;` / `%p;` — pitch or the default pitch (`case 0x70`, members
//!   0x32/0x31) — `parseStyle` :479-489.
//! - `%a<n>;` / `%d<n>;` / `%d;` — the per-character delay (member 0x5d,
//!   which `render`'s argument 2 seeds): absolute n, n/100 of the base
//!   delay, or the base delay (`case 0x25`, the `a`/`d` arms) —
//!   `parseDelay` :528-562.
//! - `%t<n>;` / `%w<n>;` / `%D<n>;` and their `$name;` forms — wait time
//!   added to the display clock (member 0x76); `%w` counts n/100 of the base
//!   delay and the others add their value directly; the `$` forms evaluate
//!   the name through `onEval` first (`case 0x25`, the `t`/`w`/`D` arms) —
//!   `parseWait` :502 / `parseWC` :514 / `parseTalkWait` :568. `%D` is the
//!   approximation: the DLL's arm (`0x1000c9a9`) calls the commit routine
//!   `FUN_100097c0` to retime the records of the current run instead of
//!   touching the clock, and that retiming is not modelled yet (see the wait
//!   handler's comment).
//! - `%n<n>;` — that many line breaks (`case 0x25`, the `n` arm calls the
//!   line-break routine `FUN_10007440` per unit) — `parseXR` :493.
//! - `%l<name>;` / `%l;` — link span start/end (`case 0x6c`) — `parseLink`
//!   :580-585.
//! - `%L` / `%C` / `%R` — align (`case 0x25`, member 0x34).
//! - `%k0`/`%k1`/`%kd` — a flag the DLL stores at member +0x49; no converter
//!   in the in-repo games emits it, so this module consumes it and does not
//!   guess what the flag does.
//!
//! The clock the timing codes move, and the base delay they scale against, are
//! the reference's timing members: `render`'s argument 2 arrives as the base
//! per-character delay (or 0.001 when it is 0 and argument 3 is positive;
//! `0x1000b8f4-0x1000b912` computes it, `0x1000ba47` stores it at member
//! `+0x174`), every character advances the display clock by the current
//! per-character delay (member `+0x1d8`, `FUN_1000a1f0`), and each character
//! object carries the clock value reached *before* that step as its `delay`
//! (the walker zeroes the clock at `0x1000ba31` and the record paths store it
//! before adding the step — `0x1000b097`/`0x1000a758`/`0x1000905c` — so the
//! first character shows at 0 ms and `renderDelay` is the time after the
//! last). All five timing codes are gated by the `ignore_delay` option
//! (member `+0x16a`), which suppresses them without touching the base.
//!
//! Every other `%X` is consumed up to its `;` and draws nothing — that is the
//! DLL's own `default` arm, and it is why the converters escape a literal `%`
//! as `\%`.
//!
//! # What the engine cannot do yet
//!
//! These members are registered with reference kinds and honest values, but
//! their full behaviour needs engine work and is reported in the mission
//! summary instead of being faked:
//!
//! - vertical layout: `vertical` is stored and the scroll getters switch axis
//!   like the DLL, but glyphs are still laid out horizontally because the text
//!   drawing path performs no glyph rotation. The game drives that rotation
//!   through the Font (`onFontChange` sets `font.angle = 2700`).
//! - the message format's directives move a *render-local* style (the shape
//!   the character records and `onFontChange` see) and start from the
//!   instance's effective style; the DLL writes its active members instead, so
//!   a directive survives the pass there.
//! - `%L`/`%C`/`%R` are parsed and stored in that local style but the layout
//!   does not shift lines: the DLL's line-shift formula (`FUN_10007440` reads
//!   member 0x34 and the half-width constant 0x100277c8) was only partially
//!   recovered, and no in-repo game text carries the codes.
//! - `\i`/`\r` are consumed without a layout effect: the DLL moves its line
//!   origin member, and this module lays every line out from x = 0 (the
//!   auto-indent `render` argument is not applied — see below).
//! - `&name;` records carry `text` (the name), a truthy `graph`,
//!   `cw`/`width` from `onGetGraphSize`'s `width` and `size`/`height` from its
//!   `height`, which is the field set `system\TextRender.tjs` `drawGraph`
//!   reads; the DLL's remaining graph-record fields were not recovered.
//! - the `%t`/`%w`/`%D` `$name;` forms evaluate the name through `onEval` and
//!   add the parsed number; the DLL routes the same three codes through its
//!   commit routine, so their pausing behaviour beyond that is not modelled.
//! - the link model: `getLinkNames`/`getLinkRects`/`getLinkCharacters` return
//!   empty arrays, `isLinkContains` false and `getLinkOfPosition` -1 because no
//!   engine object tracks link spans or `linkName`s.
//! - inline evaluation: `onEval(text)` returns its argument; the DLL evaluates
//!   the expression through `TVPExecuteExpression`, which a plugin cannot reach
//!   in this runtime. (The game's `TextRender.onEval` overrides it with
//!   `Scripts.eval`.)
//! - `calcLineOffset(line)` follows its reference signature; the DLL's own
//!   definition was not decompiled, so the value is the natural one over this
//!   module's line records (the line's origin).
//! - line-breaking options: `vertical`, `width_time_scale` and the booleans are
//!   stored and the layout acts on those it can (axis, per-glyph delay), but
//!   `following`/`leading`/`begin`/`end`/`kinsoku_max`/`word_break` need the
//!   DLL's kazari line-breaking rules — text is broken at the render box width
//!   only, and nothing applies the auto-indent the game passes as `render`'s
//!   argument 1 (`system/TextRender.tjs` calls
//!   `TextRenderBase.render(a3, a4, a7, a8, 0, 0)` with `a4` = 1 by default,
//!   and `system/LangRender.tjs` passes `kag.autoIndent`). Of the `ignore_*`
//!   gates `ignore_delay` is applied: the option lands at member `+0x16a`
//!   (`0x10018e5f`, key string 0x100274d4) and gates the five timing codes
//!   (`%d` `0x1000c54b`, `%a` `0x1000c5fb`, `%w` `0x1000c6fe`, `%t` `0x1000c83c`,
//!   `%D` `0x1000c977`), which then parse and consume their payload but leave
//!   the char delay and the clock alone. The style gates (`ignore_color`
//!   `+0x168`, `ignore_size` `+0x169`, `ignore_ruby` `+0x16d`, `ignore_type`
//!   `+0x16e`, `ignore_face` `+0x16f`, `ignore_style` `+0x170`, `ignore_xr`
//!   `+0x171`) are stored but not applied yet.
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
//!   measurement goes through `onGetTextWidth` and game scripts assign them;
//!   `onFontChange` is called with the active style dictionary when the active
//!   attributes change, as the DLL's notification does.
//! - `setRenderSize` also seeds the result properties with the box, so a script
//!   that sizes a window before rendering sees the box rather than a stale 0.
//! - `setFont` also accepts a non-Dictionary argument (a native `Font`) and
//!   stores it as the instance's `font` member, which the layout measures
//!   through when a game does not override `setFont` itself. The DLL's
//!   measurement is delegated to the game's `onGetTextWidth`; this fallback
//!   keeps the engine's own font path working for the in-repo games.

use std::collections::VecDeque;

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "TextRenderBase",
    notes: "All 55 reference members in registration order: 22 methods with the DLL's signatures and 33 properties with the DLL's constructor defaults and get-only access. setOption's 19 keys and setDefault's 18 style keys follow the DLL, unknown keys are ignored as there; build A's extra `wordbreak` style key (build B has no such string) is accepted in setDefault and setStyle as the union the module takes for render's arity; setFont/setStyle write the active style, resetFont/resetStyle copy the stored defaults into it. getCharacters(from, count) and calcShowCount(elapsed) match FUN_10003d90/FUN_10011080. render takes the DLL's six arguments, consumes argument 2 as the per-character base delay (the games' kag.actualChSpeed, with the reference's 0.001 fallback when it is 0 and argument 3 is positive), seeds every record's display time from it and scales the %d/%w codes by it while %a/%t stay absolute, and parses the message text format the games' TagTextConverter emits and the DLL's layout walker FUN_1000b8c0 interprets: \\n/raw newline/%n breaks, \\k key waits for getKeyWait(), \\w/\\x/\\i/\\r, $expr; through onEval, &name; through onGetGraphSize, %f/%r/%<n>;/%;/%B/%S/%b/%i/%s/%e/%p/%a/%d/%t/%w/%D/%l/#…; style and timing codes (%a/%d/%t/%w/%D honour ignore_delay; the last group's line-shift, indent and named-wait details are documented gaps), [ruby,count], and \\X as the literal X. Glyphs measure through the game's onGetTextWidth or the engine Font (getEscWidthX/getTextWidth); vertical layout, the link model and the auto-indent/kinsoku rules still need engine work. Every method's argument floor is its command's declared parameter count (ncbind's ArgsCount: fewer is TJS_E_BADPARAMCOUNT, extras are ignored), so setRenderSize needs 2, getCharacters 2, getLinkRects/getLinkCharacters 1 and isLinkContains 3, while the games' own call shapes (setRenderSize(w, h), getCharacters(0, 0)) pass the full lists.",
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
             defaults; the message text format's escapes are parsed (line breaks, key \
             waits, style codes, $…; evaluation, &…; graphs, [ruby,count]) and glyphs \
             measure through the game's onGetTextWidth or the engine Font",
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
        /// The minimum parameter count across the reference builds whose
        /// command declares the member (`InvokeCommand`'s `ArgsCount`,
        /// `ncbind.hpp:1231`). ncbind rejects a call that passes fewer —
        /// `if (_numparams < SelectorT::ArgsCount) return TJS_E_BADPARAMCOUNT`
        /// (`ncbind.hpp:1186`) — and ignores extra arguments, so a value below
        /// one build's count is the compatibility floor for that build's
        /// callers rather than an exact match (see `render`).
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
    member("setOption", "void (tTJSVariant)", set_option, 1),
    member("setDefault", "void (tTJSVariant)", set_default, 1),
    member("setRenderSize", "void (float, float)", set_render_size, 2),
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
    member("setFont", "void (tTJSVariant)", set_font, 1),
    member("setStyle", "void (tTJSVariant)", set_style, 1),
    member(
        "render",
        "bool (const tjs_char *, int, int, int, bool, bool)",
        render,
        // The two shipped builds of the DLL declare different arities: the
        // GINKA/少女世界 build (md5 5aa3b6c88c0c50c59dc218d895036712) takes six
        // parameters (`P8TextRender@@AE_NPB_WHHH_N1@Z`, its mangler `1` a
        // back-reference to the preceding `bool`) and the PARQUET build
        // (md5 2213af667928ddb8733e92b19f8421d2) five
        // (`P8TextRender@@AE_NPB_WHHH_N@Z`). ncbind rejects a call shorter
        // than the declared count (`ncbind.hpp:1186`), so the module requires
        // the smaller of the two: PARQUET's five-argument dialogue calls work
        // and the six-argument calls the other build's scripts make are
        // accepted too (ncbind ignores extra arguments). Requiring six broke
        // PARQUET's `system/TextRender.tjs`/`LangRender.tjs` calls.
        5,
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
    prop_computed(
        "renderDelay",
        "float",
        ValueKind::Real,
        Default::Real(0.0),
        render_delay,
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
        "int (int) const",
        calc_show_count,
        1,
    ),
    member(
        "getCharacters",
        "tTJSVariant (int, int) const",
        get_characters,
        // `tTJSVariant(TextRenderBase::*)(int,int) const`
        // (`P81@BE?AVtTJSVariant@@HH@Z`), the class's only two-int getter.
        2,
    ),
    member(
        "getLinkNames",
        "tTJSVariant ()",
        get_link_names,
        0,
    ),
    member(
        "getLinkRects",
        "tTJSVariant (int) const",
        get_link_rects,
        1,
    ),
    member(
        "getLinkCharacters",
        "tTJSVariant (int) const",
        get_link_characters,
        1,
    ),
    member(
        "isLinkContains",
        "bool (int, float, float) const",
        is_link_contains,
        3,
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

// ---------------------------------------------------------------------------
// The active style
//
// The DLL keeps a second, active copy of the style members (`FUN_10002980`
// writes it, `FUN_1000def0`/`FUN_1000dff0` refill it from the defaults) and the
// layout reads that copy. This module stores it in a nested Dictionary under
// the state member's `active` key; a property with no active override falls
// back to its reference default, so a game that never calls the resets still
// lays text out with the constructor defaults.

const ACTIVE_STYLE: &str = "active";

fn active_style_handle(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle) -> ObjectHandle {
    if let Variant::Object(handle) = state_member(runtime, instance, ACTIVE_STYLE) {
        return handle;
    }
    let handle = runtime.alloc_dictionary_object();
    state_store(runtime, instance, ACTIVE_STYLE, Variant::Object(handle));
    handle
}

fn active_store(
    runtime: &mut Runtime<KrkrHost>,
    instance: ObjectHandle,
    property: &str,
    value: Variant,
) {
    let active = active_style_handle(runtime, instance);
    runtime.set_object_member(active, property, value);
}

fn active_value(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, property: &str) -> Variant {
    let Variant::Object(active) = state_member(runtime, instance, ACTIVE_STYLE) else {
        return Variant::Void;
    };
    runtime.object_member(active, property)
}

/// Copy the stored default of `property` into the active style — the operation
/// `resetFont`/`resetStyle` perform (`FUN_1000def0`/`FUN_1000dff0` copy the
/// `default*` members into the active ones, so a `defaultFace = …` written just
/// before the reset is what lands there).
fn active_refresh(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle, property: &str) {
    let value = match state_member(runtime, instance, property) {
        Variant::Void => match surface_default(property) {
            Some(default) => default_variant(default),
            None => Variant::Void,
        },
        value => value,
    };
    active_store(runtime, instance, property, value);
}

/// The layout's view of a style attribute: the active copy when the game has
/// pushed one (through `setFont`/`setStyle`/`resetFont`/`resetStyle`), else the
/// property value, else the reference default.
fn effective_real(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> f64 {
    match active_value(runtime, instance, name) {
        Variant::Void => value_real(runtime, instance, name),
        value => value.to_real().unwrap_or(0.0),
    }
}

fn effective_int(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> i64 {
    match active_value(runtime, instance, name) {
        Variant::Void => value_int(runtime, instance, name),
        value => value.to_integer().unwrap_or(0),
    }
}

fn effective_bool(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> bool {
    match active_value(runtime, instance, name) {
        Variant::Void => value_bool(runtime, instance, name),
        value => value.to_integer().unwrap_or(0) != 0,
    }
}

fn effective_text(runtime: &Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> String {
    match active_value(runtime, instance, name) {
        Variant::Void => value_text(runtime, instance, name),
        value => value.to_tjs_string().unwrap_or_default(),
    }
}

/// A property's own member name is also its state key, so `setDefault` writes
/// the very values the `default*` properties read — exactly as the DLL's
/// `FUN_100022f0` writes the members its property getters return.
fn property_state_key(name: &str) -> &str {
    name
}

// ---------------------------------------------------------------------------
// setOption (FUN_10001a70) and setDefault (FUN_100022f0)

/// The 19 `setOption` keys the DLL compares, in its own order (`FUN_10001a70`
/// reads `following leading begin end vertical kinsoku_max word_break` and the
/// `ignore_*`/`width_time_scale` gates; `end` sits between `begin` and
/// `vertical`, wide string 0x1002aae4). Unknown keys are never read: the DLL's
/// accessor loop has no default branch, so an unknown key is silently ignored,
/// and so is a known key of the wrong type.
const OPTION_KEYS: &[&str] = &[
    "following",
    "leading",
    "begin",
    "end",
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

/// The 18 style keys `setDefault` reads (`FUN_100022f0`), mapped onto the
/// properties whose members they write.
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

/// The character attributes `setFont` reads (`FUN_10002980`), in its order:
/// face, bold, fontsize, rubysize, rubyoffset, color, shadow, shadowcolor,
/// shadowdiff, edge, edgecolor. The DLL writes them into the *active* members.
const FONT_KEYS: &[(&str, &str)] = &[
    ("face", "defaultFace"),
    ("bold", "defaultBold"),
    ("fontsize", "defaultFontSize"),
    ("rubysize", "defaultRubySize"),
    ("rubyoffset", "defaultRubyOffset"),
    ("color", "defaultChColor"),
    ("shadow", "defaultShadow"),
    ("shadowcolor", "defaultShadowColor"),
    ("shadowdiff", "defaultShadowDiff"),
    ("edge", "defaultEdge"),
    ("edgecolor", "defaultEdgeColor"),
];

/// The layout keys `setStyle` reads (`FUN_10002e30`): linespacing, pitch, then
/// the line size (which falls back to `fontsize`), align and valign.
const LAYOUT_KEYS: &[(&str, &str)] = &[
    ("linespacing", "defaultLineSpacing"),
    ("pitch", "defaultPitch"),
    ("linesize", "defaultLineSize"),
    ("align", "defaultAlign"),
    ("valign", "defaultValign"),
];

/// The extra style key build A reads and build B has no string for:
/// `wordbreak`. Build A's `setDefault` (`0x10016ff0`, decompiled line ~350)
/// reads it into the stored word-break bool at `this + 0x48` and its
/// `setStyle` (`0x100193a0`, `:1001974f-10019763`) into the active one at
/// `this + 0x49`; build B's `FUN_100022f0`/`FUN_10002e30` read their 18 and 6
/// keys and never look at it. No shipped script sends the key — M163's scan
/// found zero `wordbreak` hits across both games' archives and scenario files —
/// so accepting it is the same union this module already takes for `render`'s
/// arity: build A's behaviour for a script that sends the key, build B's for
/// everything either game runs.
const STYLE_WORD_BREAK_KEY: &str = "wordbreak";

/// The state key of the word-break flag: the one `setOption`'s `word_break`
/// writes, and the one build A's `resetStyle` copies from the stored bool into
/// the active one.
const WORD_BREAK_KEY: &str = "word_break";

/// The word-break flag's constructor value. Build A's constructor stores 1 into
/// the stored bool (`10004840: movb $0x1,0x48(%edi)`, right after `kinsoku_max`
/// 1 at `:1000486e`) and build B's into its single flag (`1000d6f9:
/// movb $0x1,0x50(%edi)`), so the reference's word break starts on.
const WORD_BREAK_DEFAULT: i64 = 1;

/// The active members `resetFont` copies the stored defaults into, per
/// `FUN_1000def0`: bold, italic, size, ruby size, ruby offset, face, color,
/// shadow, shadow diff, shadow color, edge color and edge.
const RESET_FONT_PROPERTIES: &[&str] = &[
    "defaultBold",
    "defaultItalic",
    "defaultFontSize",
    "defaultRubySize",
    "defaultRubyOffset",
    "defaultFace",
    "defaultChColor",
    "defaultShadow",
    "defaultShadowDiff",
    "defaultShadowColor",
    "defaultEdgeColor",
    "defaultEdge",
];

/// The active layout values `resetStyle` recomputes (`FUN_1000dff0`):
/// `lineSpacing`, `pitch`, `align`, `valign`, plus the line advance
/// `fontScale * lineSize` that the DLL stores at member 0xc8.
const RESET_STYLE_PROPERTIES: &[&str] = &[
    "defaultLineSpacing",
    "defaultPitch",
    "defaultAlign",
    "defaultValign",
];

/// Reference ratios the DLL derives when `fontsize` arrives without its
/// dependent keys: big = 2x, small = 0.5x, line size = font size, and the ruby
/// size divides by the double at `.rdata` 0x1002c720, which is 3.0.
const BIG_FONT_RATIO: f64 = 2.0;
const SMALL_FONT_RATIO: f64 = 0.5;
const RUBY_FONT_DIVISOR: f64 = 3.0;

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
            "following" | "leading" | "begin" | "end" => Variant::String(value.to_tjs_string()?),
            "kinsoku_max" => Variant::Integer(value.to_integer().unwrap_or(1)),
            "vertical" => {
                let flag = Variant::Integer(i64::from(value.to_integer().unwrap_or(0) != 0));
                // `vertical` is a property, so the flag a script reads back has
                // to land in the store that property reads.
                state_store(runtime, this, property_state_key("vertical"), flag);
                continue;
            }
            // Build A's `setOption` writes the word-break bool into *both*
            // style slots (`:10018cc1`, `:10018cc4`: `mov %al,0x49(%esi)`
            // then `mov %al,0x48(%esi)`), where build B has the single flag
            // the state store keeps below.
            "word_break" => {
                let flag = Variant::Integer(i64::from(value.to_integer().unwrap_or(0) != 0));
                active_store(runtime, this, WORD_BREAK_KEY, flag.clone());
                flag
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

/// `setStyle(dict)` (`FUN_10002e30`) writes the layout subset into the active
/// members; it is not a second `setDefault`. Its line-size step reads `linesize`
/// and falls back to `fontsize` when only that one is present, the two-source
/// block the DLL has there.
fn set_style(
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
    for (key, property) in LAYOUT_KEYS {
        if let Some(value) = read_property(runtime, styles, key) {
            active_store(
                runtime,
                this,
                property,
                coerce_variant(value, style_value_kind(property)),
            );
        }
    }
    if read_property(runtime, styles, "linesize").is_none()
        && let Some(value) = read_property(runtime, styles, "fontsize")
    {
        active_store(
            runtime,
            this,
            "defaultLineSize",
            coerce_variant(value, ValueKind::Real),
        );
    }
    // Build A's `setStyle` reads one key more than build B's: `wordbreak` into
    // the *active* word-break bool (`:1001975e-10019763`).
    if let Some(value) = read_property(runtime, styles, STYLE_WORD_BREAK_KEY) {
        active_store(
            runtime,
            this,
            WORD_BREAK_KEY,
            coerce_variant(value, ValueKind::Bool),
        );
    }
    Ok(Variant::Void)
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
    // Build A's `setDefault` reads one key more than build B's: `wordbreak`,
    // into the *stored* word-break bool the `resetStyle` copy below reads back
    // (`:10016ff0`, decompiled line ~350).
    if let Some(value) = read(runtime, STYLE_WORD_BREAK_KEY, ValueKind::Bool) {
        state_store(runtime, instance, WORD_BREAK_KEY, value);
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

/// `resetFont` (`FUN_1000def0`) copies the stored font/style defaults into the
/// active members — the DLL's companion to `defaultFace = …, resetFont()`.
/// bold, italic, size, ruby size, ruby offset, face, color, shadow, shadow
/// diff, shadow color, edge color, edge.
fn reset_font(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    for property in RESET_FONT_PROPERTIES {
        active_refresh(runtime, this, property);
    }
    notify_font_change(runtime, this);
    Ok(Variant::Void)
}

/// `resetStyle` (`FUN_1000dff0`) recomputes and applies the layout defaults:
/// line spacing, pitch, align, valign and the line advance `fontScale *
/// lineSize` (`FUN_1000dff0` stores that product at member 0xc8). The module's
/// layout derives the line step from the line size and spacing on the fly, so
/// the advance needs no separate member.
fn reset_style(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    for property in RESET_STYLE_PROPERTIES {
        active_refresh(runtime, this, property);
    }
    // The word-break flag is one of the members `resetStyle` copies: the
    // reference's own bytes are `mov 0x48(%ecx),%al; mov %al,0x49(%ecx)`
    // (`:1000d5ce-1000d5d9`), and `resetFont` (`:1000d440-1000d579`) never
    // touches either. A flag no script ever wrote keeps the constructor's
    // value, so the copy is on like the DLL's.
    let word_break = state_int(runtime, this, WORD_BREAK_KEY).unwrap_or(WORD_BREAK_DEFAULT);
    active_store(
        runtime,
        this,
        WORD_BREAK_KEY,
        Variant::Integer(i64::from(word_break != 0)),
    );
    Ok(Variant::Void)
}

/// `setFont(dict)` (`FUN_10002980`) reads the character attributes into the
/// active style and notifies `onFontChange`; the DLL calls that notification on
/// its own virtual when face/bold/size change, and the game's handler pushes
/// the values into its Font (`system/TextRender.tjs`:
/// `font.bold = a0.bold; font.italic = a0.italic; font.face = …`).
///
/// A non-Dictionary argument — a native `Font` a game hands to the base
/// `setFont` instead of overriding it — is stored as the instance's `font`
/// member, which the layout measures through. `system/TextRender.tjs` overrides
/// `setFont` for exactly that, so this keeps the engine font path working for
/// games that extend it rather than replace it.
fn set_font(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let Some(argument) = args.first().cloned() else {
        return Ok(Variant::Void);
    };
    let Some(source) = dictionary_argument(runtime, Some(&argument)) else {
        return Ok(Variant::Void);
    };
    if !runtime.is_dictionary_instance(source) {
        runtime.set_object_member(this, "font", Variant::Object(source));
        return Ok(Variant::Void);
    }
    for (key, property) in FONT_KEYS {
        if let Some(value) = read_property(runtime, source, key) {
            active_store(
                runtime,
                this,
                property,
                coerce_variant(value, style_value_kind(property)),
            );
        }
    }
    notify_font_change(runtime, this);
    Ok(Variant::Void)
}

/// Call the instance's `onFontChange` with a dictionary of the active style,
/// the shape the DLL's notification carries and the game's handler reads
/// (`a0.face`, `a0.bold`, `a0.italic`). A game that does not define it keeps
/// the module's void compat slot and nothing happens.
fn notify_font_change(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle) {
    let report = runtime.alloc_dictionary_object();
    for (property, key) in [
        ("defaultFace", "face"),
        ("defaultBold", "bold"),
        ("defaultItalic", "italic"),
        ("defaultFontSize", "size"),
        ("defaultRubySize", "rubysize"),
        ("defaultRubyOffset", "rubyoffset"),
        ("defaultChColor", "color"),
        ("defaultShadow", "shadow"),
        ("defaultShadowColor", "shadowColor"),
        ("defaultShadowDiff", "shadowDiff"),
        ("defaultEdge", "edge"),
        ("defaultEdgeColor", "edgeColor"),
    ] {
        let value = match property {
            "defaultFace" => Variant::String(effective_text(runtime, instance, property)),
            "defaultBold" | "defaultItalic" | "defaultShadow" | "defaultEdge" => {
                Variant::Integer(i64::from(effective_bool(runtime, instance, property)))
            }
            "defaultChColor" | "defaultShadowColor" | "defaultShadowDiff"
            | "defaultEdgeColor" => Variant::Integer(effective_int(runtime, instance, property)),
            _ => Variant::Real(effective_real(runtime, instance, property)),
        };
        runtime.set_object_member(report, key, value);
    }
    fire_font_change(runtime, instance, report);
}

/// Call the instance's `onFontChange` with an already-built report.
fn fire_font_change(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle, report: ObjectHandle) {
    let Ok(handler) = runtime.resolve_object_member(instance, "onFontChange") else {
        return;
    };
    let Some(handler) = handler.object_handle() else {
        return;
    };
    if runtime.object_is_callable(handler) {
        let _ = runtime.call_object_method(instance, "onFontChange", vec![Variant::Object(report)]);
    }
}

/// The `onFontChange` notification for a style a message-text directive moved
/// (`%f`, `%r`, the size and attribute codes). The DLL fires the same event
/// from every one of its style setters (`FUN_1000d6e0` and friends call the
/// vtable slot `FUN_10016bb0` builds the report from, and that report carries
/// `face`/`bold`/`italic` plus the attribute set); GINKA's handler pushes the
/// values into its Font, which is what makes a `%f…;` span measure with the
/// switched face.
fn call_font_change(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle, style: &GlyphStyle) {
    let report = runtime.alloc_dictionary_object();
    for (key, value) in [
        ("face", Variant::String(style.face.clone())),
        ("bold", Variant::Integer(i64::from(style.bold))),
        ("italic", Variant::Integer(i64::from(style.italic))),
        ("size", Variant::Real(style.size as f64)),
        (
            "rubysize",
            Variant::Real(effective_real(runtime, instance, "defaultRubySize")),
        ),
        (
            "rubyoffset",
            Variant::Real(effective_real(runtime, instance, "defaultRubyOffset")),
        ),
        ("color", Variant::Integer(style.color)),
        ("shadow", Variant::Integer(i64::from(style.shadow))),
        ("shadowColor", Variant::Integer(style.shadow_color)),
        (
            "shadowDiff",
            Variant::Integer(effective_int(runtime, instance, "defaultShadowDiff")),
        ),
        ("edge", Variant::Integer(i64::from(style.edge))),
        ("edgeColor", Variant::Integer(style.edge_color)),
    ] {
        runtime.set_object_member(report, key, value);
    }
    fire_font_change(runtime, instance, report);
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
        LAYOUT_KEY_WAITS,
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
const LAYOUT_KEY_WAITS: &str = "keyWaits";

/// The active glyph attributes the layout carries while it walks message text.
///
/// textrender.dll keeps the *active* style in the members its `set*` commands
/// and the text's own directives write (`%f` → `FUN_1000d7c0`, `%<n>;` →
/// `FUN_1000d830`, `#`/`%e#`/`%s#` → the colour members 0x4b/0x4e/0x53, `%p` →
/// 0x32, `%d`/`%a` → 0x5d, `%b`/`%i`/`%s`/`%e` → the flags at +4/+5/+0x131/
/// +0x145), and every character record is built from that copy. This struct is
/// the same idea for one render pass: it starts from the instance's effective
/// style and each directive moves it.
struct GlyphStyle {
    /// The face name the character records carry (`onFontChange` passes it on
    /// to the game's Font).
    face: String,
    /// The current glyph size in pixels.
    size: i64,
    /// `defaultFontSize` x `fontScale`: what `%;` restores and `%<n>;` takes
    /// its percentage of (the DLL's member 0x2a).
    base_size: i64,
    big_size: i64,
    small_size: i64,
    color: i64,
    edge_color: i64,
    shadow_color: i64,
    shadow_diff: i64,
    bold: bool,
    italic: bool,
    shadow: bool,
    edge: bool,
    /// The extra advance per character (`%p`; the DLL's member 0x32).
    pitch: f64,
    /// `%L`/`%C`/`%R`: stored, not applied (see the module docs).
    align: i64,
    /// The per-character delay `%d`/`%a` move (the DLL's member 0x5d).
    char_delay: f64,
    /// The link name the character records carry (`%l…;`), empty outside a
    /// link span.
    link: String,
}

impl GlyphStyle {
    /// `%r`: copy the stored defaults back into the active attributes — the
    /// same reset `resetFont` performs (`FUN_1000d440` reads the default
    /// members 0x48/0x58/0x161/0x2a/0x4a/0x4c/0x4d/0x51/0x52).
    fn reset_font(&mut self, runtime: &Runtime<KrkrHost>, instance: ObjectHandle) {
        self.face = effective_text(runtime, instance, "defaultFace");
        self.size = self.base_size;
        self.color = effective_int(runtime, instance, "defaultChColor");
        self.edge_color = effective_int(runtime, instance, "defaultEdgeColor");
        self.shadow_color = effective_int(runtime, instance, "defaultShadowColor");
        self.shadow_diff = effective_int(runtime, instance, "defaultShadowDiff");
        self.bold = effective_bool(runtime, instance, "defaultBold");
        self.italic = effective_bool(runtime, instance, "defaultItalic");
        self.shadow = effective_bool(runtime, instance, "defaultShadow");
        self.edge = effective_bool(runtime, instance, "defaultEdge");
    }
}

/// Where one character record sits and when it shows, for
/// [`build_character_record`].
struct RecordPlacement {
    x: i64,
    y: i64,
    line: i64,
    width: i64,
    height: i64,
    delay: f64,
}

/// Build one character object for the layout: the reference member names the
/// DLL's record builder registers (`text`, `graph`, `face`, `size`, `italic`,
/// `bold`, `color`, `shadow`, `shadowColor`, `shadowDiff`, `edge`,
/// `edgeColor`, `delay`, `link`, `linkName`, `left`, `width`, `height`,
/// `vertical`) plus the engine-facing aliases the in-repo game scripts read
/// (`x`, `y`, `cw`, `line`, `index`). A `graph` record's `text` is the image
/// name and its geometry comes from `onGetGraphSize`.
fn build_character_record(
    runtime: &mut Runtime<KrkrHost>,
    style: &GlyphStyle,
    placement: RecordPlacement,
    vertical: bool,
    text: &str,
    graph: Option<&str>,
    index: i64,
) -> ObjectHandle {
    let record = runtime.alloc_dictionary_object();
    runtime.set_object_member(record, "text", Variant::String(text.to_string()));
    runtime.set_object_member(
        record,
        "graph",
        match graph {
            Some(name) => Variant::String(name.to_string()),
            None => Variant::Void,
        },
    );
    runtime.set_object_member(record, "face", Variant::String(style.face.clone()));
    runtime.set_object_member(record, "size", Variant::Integer(placement.height));
    runtime.set_object_member(record, "italic", Variant::Integer(i64::from(style.italic)));
    runtime.set_object_member(record, "bold", Variant::Integer(i64::from(style.bold)));
    runtime.set_object_member(record, "color", Variant::Integer(style.color));
    runtime.set_object_member(record, "shadow", Variant::Integer(i64::from(style.shadow)));
    runtime.set_object_member(record, "shadowColor", Variant::Integer(style.shadow_color));
    runtime.set_object_member(record, "shadowDiff", Variant::Integer(style.shadow_diff));
    runtime.set_object_member(record, "edge", Variant::Integer(i64::from(style.edge)));
    runtime.set_object_member(record, "edgeColor", Variant::Integer(style.edge_color));
    runtime.set_object_member(record, "delay", Variant::Real(placement.delay));
    runtime.set_object_member(record, "link", Variant::String(style.link.clone()));
    runtime.set_object_member(record, "linkName", Variant::String(style.link.clone()));
    runtime.set_object_member(record, "left", Variant::Integer(placement.x));
    runtime.set_object_member(record, "width", Variant::Integer(placement.width));
    runtime.set_object_member(record, "height", Variant::Integer(placement.height));
    runtime.set_object_member(record, "vertical", Variant::Integer(i64::from(vertical)));
    runtime.set_object_member(record, "x", Variant::Integer(placement.x));
    runtime.set_object_member(record, "y", Variant::Integer(placement.y));
    runtime.set_object_member(record, "cw", Variant::Integer(placement.width));
    runtime.set_object_member(record, "line", Variant::Integer(placement.line));
    runtime.set_object_member(record, "index", Variant::Integer(index));
    record
}

/// Evaluate message text in the DLL's `$…;` code: the instance's `onEval`
/// (`case 0x24`). The game overrides it with `Scripts.eval`; this module's
/// base slot returns its argument, which is the honest answer for a plugin
/// with no evaluator, and a failing handler inserts nothing.
fn evaluate_expression(
    runtime: &mut Runtime<KrkrHost>,
    instance: ObjectHandle,
    expression: &str,
) -> String {
    match runtime.call_object_method(
        instance,
        "onEval",
        vec![Variant::String(expression.to_string())],
    ) {
        Ok(Variant::Void | Variant::Null) | Err(_) => String::new(),
        Ok(value) => value.to_tjs_string().unwrap_or_default(),
    }
}

/// Ask the instance's `onGetGraphSize` for an inline image's size
/// (`FUN_100154a0` calls the handler and reads `width`/`height` off the
/// Dictionary it answers with; a handler that answers nothing leaves a
/// zero-sized graph).
fn graph_size(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> (i64, i64) {
    let Some(answer) = runtime
        .call_object_method(
            instance,
            "onGetGraphSize",
            vec![Variant::String(name.to_string())],
        )
        .ok()
        .and_then(|value| value.object_handle())
    else {
        return (0, 0);
    };
    let width = resolve_font_member_int(runtime, answer, "width").unwrap_or(0);
    let height = resolve_font_member_int(runtime, answer, "height").unwrap_or(0);
    (width.max(0), height.max(0))
}

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

/// `getKeyWait()` answers the waits the last layout recorded — one entry per
/// `\k` (and `%D…;`) in the text, each a Dictionary `{pos, time}`:
///
/// - `pos` is the number of characters laid out up to the wait. The conductor
///   reveals exactly that many (`rendermsgwin.tjs`: `drawCount(keyWait[0].pos
///   - cpos)`) and treats the wait as due once `calcShowCount` reaches it
///   (`if (hasAnyKeyWait && !(keyWait[0].pos > l1)) waitClick()`).
/// - `time` is the display time (the `delay` accumulator) reached at the
///   wait, times the current `timeScale` — the field the conductor rewinds
///   its typewriter clock with (`startTime = System.getTickCount() -
///   keyWait[0].time`).
///
/// The DLL stores the same pair per wait (`FUN_10009bb0` reads the position,
/// `FUN_10009be0` the time and multiplies by `timeScale`) and builds a fresh
/// Dictionary per entry in `FUN_10015790`, which is why this getter builds a
/// fresh array: a `timeScale` write after the render must change the answer.
fn get_key_wait(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Object(runtime.alloc_array_object(Vec::new())));
    };
    let time_scale = value_real(runtime, this, "timeScale");
    let waits = match state_member(runtime, this, LAYOUT_KEY_WAITS) {
        Variant::Object(handle) => runtime
            .array_elements(handle)
            .map(<[Variant]>::to_vec)
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let entries = waits
        .into_iter()
        .filter_map(|wait| wait.object_handle())
        .map(|wait| {
            let entry = runtime.alloc_dictionary_object();
            let pos = runtime.object_member(wait, "pos").to_integer().unwrap_or(0);
            let delay = runtime
                .object_member(wait, "delay")
                .to_real()
                .unwrap_or(0.0);
            runtime.set_object_member(entry, "pos", Variant::Integer(pos));
            runtime.set_object_member(entry, "time", Variant::Real(delay * time_scale));
            Variant::Object(entry)
        })
        .collect::<Vec<_>>();
    Ok(Variant::Object(runtime.alloc_array_object(entries)))
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

/// `calcShowCount(elapsed)` (`FUN_10011080`) — how many characters are visible
/// after `elapsed` milliseconds: the DLL walks its records backwards and
/// answers `index + 1` for the first record whose display time
/// (`record delay x timeScale`) has come, and 0 before the first character. The
/// game drives its typewriter with this (`sysscn/rendermsgwin.tjs` `onUpdate`:
/// `calcShowCount(System.getTickCount() - startTime)`, `calcShowCount(0)` at
/// start).
fn calc_show_count(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Integer(0));
    };
    let elapsed = arguments(&args, 0).to_real().unwrap_or(0.0);
    let time_scale = value_real(runtime, this, "timeScale");
    let mut shown = 0_i64;
    for record in character_records(runtime, this) {
        let Some(record) = record.object_handle() else {
            continue;
        };
        let display_time = runtime
            .object_member(record, "delay")
            .to_real()
            .unwrap_or(0.0);
        if display_time * time_scale <= elapsed {
            shown += 1;
        } else {
            break;
        }
    }
    Ok(Variant::Integer(shown))
}

/// `getCharacters(from, count)` (`FUN_10003d90`) — the character objects. The
/// second argument is a **count**, and 0 means "everything from `from`":
/// `if (arg2 == 0) arg2 = renderCount - arg1`. The game calls
/// `getCharacters(0, 0)` in its message renderer and its redraw path, so
/// treating the second argument as an end index would return nothing and draw
/// no dialogue at all. Both arguments are mandatory: the command's `ArgsCount`
/// is two (`P81@BE?AVtTJSVariant@@HH@Z`), so ncbind rejects a shorter call
/// before the handler runs.
fn get_characters(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = bound_this(runtime, this_obj) else {
        return Ok(Variant::Object(runtime.alloc_array_object(Vec::new())));
    };
    let records = character_records(runtime, this);
    let from = usize::try_from(arguments(&args, 0).to_integer().unwrap_or(0).max(0))
        .unwrap_or(usize::MAX);
    let count = arguments(&args, 1).to_integer().unwrap_or(0);
    let take = if count <= 0 {
        records.len().saturating_sub(from)
    } else {
        usize::try_from(count).unwrap_or(usize::MAX)
    };
    let slice = records.into_iter().skip(from).take(take).collect();
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

/// The base per-character delay `render`'s argument index 2 carries — the value
/// the reference walker scales the `%a`/`%d`/`%w` codes against and seeds every
/// character with.
///
/// `0x1000b8f4-0x1000b912`: when argument 3 is positive and argument 2 is 0 the
/// walker substitutes the `.rdata` single at 0x100277b8, which is `0.001f`;
/// otherwise the delay is `(float)arg2`. `0x1000ba47` stores it at member
/// `+0x174` and `FUN_1000a1f0` starts every character's delay from it.
fn base_char_delay(arg2: i64, arg3: i64) -> f64 {
    if arg2 != 0 || arg3 <= 0 {
        // `cvtdq2ps` at 0x1000b90f and the `movss` store at 0x1000ba47 keep
        // the value in an f32.
        f64::from(arg2 as f32)
    } else {
        f64::from(0.001_f32)
    }
}

/// One character's step of the display clock: the current per-character delay,
/// scaled by the glyph's own extent when `width_time_scale` is on.
///
/// `FUN_1000a1f0` (`0x1000a76a-0x1000a791`) computes the option-on step as the
/// char delay times `extent / (fontScale * size)` — the glyph's own advance
/// over its pixel size — so a glyph as wide as its em advances the clock by
/// the base delay itself. (The `%w` handler applies a related but different
/// factor; see the wait codes' comment.)
fn character_step(width_time_scale: bool, char_delay: f64, size: i64, extent: i64) -> f64 {
    if width_time_scale {
        char_delay * (extent as f64) / (size.max(1) as f64)
    } else {
        char_delay
    }
}

/// Materialize the character records the subclass consumes. That script owns
/// effect selection and delegates glyph painting to `Layer.drawText`; the
/// native side owns line layout and character geometry.
///
/// The text is the first argument: a string, or an object carrying a `text`
/// member (the KAGEX message element; it may contain `[ruby,count]` inline
/// annotations, where the ruby covers the following `count + 1` characters).
/// The shipped builds declare different arities — GINKA/少女世界's
/// (md5 `5aa3b6c8…`) `bool (TextRender::*)(const tjs_char *, int, int, int,
/// bool, bool)` (`P8TextRender@@AE_NPB_WHHH_N1@`, the mangler's `1`
/// back-referencing the preceding `bool`) and PARQUET's (md5 `2213af66…`)
/// the same with five parameters — and ncbind rejects a call that passes
/// fewer than the declared count (`ncbind.hpp:1186`), so the registration
/// requires five (see the surface entry). GINKA and 少女世界 pass six
/// (`system/TextRender.tjs` → `TextRenderBase.render(a3, a4, a7, a8, 0, 0)`,
/// `system/LangRender.tjs` → `_TextRenderBase.render(text, indent, speed, …)`),
/// PARQUET five, with argument 2 the per-character speed in both.
///
/// Argument index 2 is the base delay and index 3 the gate for the `0.001`
/// fallback ([`base_char_delay`]). Argument 1 is *not* a font size: KAGEX puts
/// the auto-indent there (1 by default, which would lay every glyph out one
/// pixel wide), the size comes from the instance font, else `defaultFontSize`,
/// and the auto-indent itself is not applied yet (see the module docs). The
/// last two arguments drive the DLL's incremental redraw paths, which this
/// single-pass renderer has no equivalent for; they are accepted and unused.
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
    // Argument 1 is not a font size — KAGEX passes the auto-indent the layer
    // supplies (1 by default) and the layout does not apply it yet, so the size
    // comes from the instance font, else the active/default font size.
    let font_scale = value_real(runtime, this, "fontScale");
    // Argument 2 is the base per-character delay (`kag.actualChSpeed` on the
    // message path) and argument 3 the gate for the reference's 0.001 fallback.
    let base_delay = base_char_delay(
        arguments(&args, 2).to_integer().unwrap_or(0),
        arguments(&args, 3).to_integer().unwrap_or(0),
    );
    // The default glyph size the `%<n>;` code takes its percentage of (the
    // DLL's member 0x2a) and the size the first glyph starts out with: the
    // font a game handed to `setFont`, else the default.
    let base_size = (effective_real(runtime, this, "defaultFontSize") * font_scale)
        .round()
        .max(1.0) as i64;
    let start_size = font
        .and_then(|font| resolve_font_size(runtime, font))
        .map(|size| ((size as f64) * font_scale).round().max(1.0) as i64)
        .unwrap_or(base_size);
    let mut style = GlyphStyle {
        face: effective_text(runtime, this, "defaultFace"),
        size: start_size,
        base_size,
        big_size: (effective_real(runtime, this, "defaultBigFontSize") * font_scale)
            .round()
            .max(1.0) as i64,
        small_size: (effective_real(runtime, this, "defaultSmallFontSize") * font_scale)
            .round()
            .max(1.0) as i64,
        color: font
            .and_then(|font| resolve_font_int(runtime, font, &["color"]))
            .unwrap_or_else(|| effective_int(runtime, this, "defaultChColor")),
        edge_color: effective_int(runtime, this, "defaultEdgeColor"),
        shadow_color: effective_int(runtime, this, "defaultShadowColor"),
        shadow_diff: effective_int(runtime, this, "defaultShadowDiff"),
        bold: effective_bool(runtime, this, "defaultBold"),
        italic: effective_bool(runtime, this, "defaultItalic"),
        shadow: effective_bool(runtime, this, "defaultShadow"),
        edge: effective_bool(runtime, this, "defaultEdge"),
        pitch: effective_real(runtime, this, "defaultPitch"),
        align: effective_int(runtime, this, "defaultAlign"),
        char_delay: base_delay,
        link: String::new(),
    };
    let width = state_int(runtime, this, RENDER_WIDTH).unwrap_or(0).max(0);
    let line_size = effective_real(runtime, this, "defaultLineSize").max(start_size as f64);
    let line_spacing = effective_real(runtime, this, "defaultLineSpacing").max(0.0);
    let line_height = (line_size + line_spacing).max(1.0);
    let ruby_size = effective_real(runtime, this, "defaultRubySize")
        .max(start_size as f64 / RUBY_FONT_DIVISOR)
        .max(1.0);
    let ruby_offset = effective_real(runtime, this, "defaultRubyOffset");
    let width_time_scale = state_bool(runtime, this, LAYOUT_WIDTH_TIME_SCALE);
    let vertical = value_bool(runtime, this, "vertical");
    // `ignore_delay` (member `+0x16a`) suppresses the five timing codes without
    // touching the base delay; the DLL reads it at each of their handlers.
    let ignore_delay = state_bool(runtime, this, "ignore_delay");

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
    // The `\k` waits: the position (the number of characters laid out so far —
    // the `pos` the conductor reveals up to) and the display time reached
    // there (the `time` it resumes the typewriter clock from). The DLL keeps
    // the same pairs at +0x1c4 and hands them out through `FUN_10015790`.
    let mut key_waits: Vec<(i64, f64)> = Vec::new();
    // An evaluated `$…;` inserts the characters of its result where the code
    // stood, so the walk consumes a queue it can push them back onto.
    let mut queue: VecDeque<TextToken> = parse_message_text(&text).into();
    while let Some(token) = queue.pop_front() {
        let character = match token {
            TextToken::Ruby { text: ruby, count } => {
                pending_ruby = Some(ruby);
                // A count read out of message text must not overflow the group
                // counter; a group longer than the rest of the text just ends
                // with the text.
                ruby_remaining = count.saturating_add(1);
                continue;
            }
            TextToken::LineBreak => {
                x = 0;
                y = y.saturating_add(line_height as i64);
                line += 1;
                lines += 1;
                line_origins.push(y as f64);
                continue;
            }
            TextToken::LineBreaks(count) => {
                for _ in 0..count {
                    x = 0;
                    y = y.saturating_add(line_height as i64);
                    line += 1;
                    lines += 1;
                    line_origins.push(y as f64);
                }
                continue;
            }
            TextToken::KeyWait => {
                key_waits.push((records.len() as i64, delay));
                continue;
            }
            // `\w` is a blank character cell — it moves the pen by the line
            // advance and draws nothing; `\x` is nothing at all.
            TextToken::Space => {
                x = x.saturating_add(style.size);
                continue;
            }
            TextToken::Invisible | TextToken::Indent | TextToken::IndentEnd | TextToken::Flag => {
                continue;
            }
            TextToken::Eval { expression } => {
                // An empty expression inserts nothing: the DLL compares the
                // run against the empty string before it calls `onEval`.
                let evaluated = if expression.is_empty() {
                    String::new()
                } else {
                    evaluate_expression(runtime, this, &expression)
                };
                for character in evaluated.chars().rev() {
                    queue.push_front(TextToken::Char(character));
                }
                continue;
            }
            TextToken::Graph { name } => {
                let (graph_width, graph_height) = graph_size(runtime, this, &name);
                // Like a character, the graph's record carries the clock value
                // reached before its step (`0x1000905c`).
                let display_time = delay;
                delay +=
                    character_step(width_time_scale, style.char_delay, style.size, graph_width);
                let record = build_character_record(
                    runtime,
                    &style,
                    RecordPlacement {
                        x,
                        y,
                        line,
                        width: graph_width,
                        height: graph_height,
                        delay: display_time,
                    },
                    vertical,
                    &name,
                    // `drawGraph` truth-tests `graph` and loads the image by
                    // `text`; the record carries the image size in `cw` and
                    // `size`, the two fields `drawGraph` reads.
                    Some(&name),
                    records.len() as i64,
                );
                records.push(Variant::Object(record));
                min_left = Some(min_left.map_or(x, |left| left.min(x)));
                max_right = max_right.max(x.saturating_add(graph_width));
                max_bottom = max_bottom.max(y.saturating_add(graph_height));
                x = x.saturating_add(graph_width);
                continue;
            }
            TextToken::Face { face } => {
                let face = if face.is_empty() {
                    effective_text(runtime, this, "defaultFace")
                } else {
                    face
                };
                if face != style.face {
                    style.face = face;
                    call_font_change(runtime, this, &style);
                }
                continue;
            }
            TextToken::ResetFont => {
                style.reset_font(runtime, this);
                call_font_change(runtime, this, &style);
                continue;
            }
            TextToken::FontSize(code) => {
                style.size = match code {
                    FontSizeCode::Default => style.base_size,
                    FontSizeCode::Big => style.big_size,
                    FontSizeCode::Small => style.small_size,
                    FontSizeCode::Percent(percent) => ((style.base_size as f64) * percent / 100.0)
                        .round()
                        .max(1.0) as i64,
                };
                call_font_change(runtime, this, &style);
                continue;
            }
            TextToken::Attribute { attribute, value } => {
                let value = value.unwrap_or_else(|| match attribute {
                    FontAttribute::Bold => effective_bool(runtime, this, "defaultBold"),
                    FontAttribute::Italic => effective_bool(runtime, this, "defaultItalic"),
                    FontAttribute::Shadow => effective_bool(runtime, this, "defaultShadow"),
                    FontAttribute::Edge => effective_bool(runtime, this, "defaultEdge"),
                });
                match attribute {
                    FontAttribute::Bold => style.bold = value,
                    FontAttribute::Italic => style.italic = value,
                    FontAttribute::Shadow => style.shadow = value,
                    FontAttribute::Edge => style.edge = value,
                }
                call_font_change(runtime, this, &style);
                continue;
            }
            TextToken::Color { target, value } => {
                let value = value.unwrap_or_else(|| match target {
                    ColorTarget::Text => effective_int(runtime, this, "defaultChColor"),
                    ColorTarget::Edge => effective_int(runtime, this, "defaultEdgeColor"),
                    ColorTarget::Shadow => effective_int(runtime, this, "defaultShadowColor"),
                });
                match target {
                    ColorTarget::Text => style.color = value,
                    ColorTarget::Edge => style.edge_color = value,
                    ColorTarget::Shadow => style.shadow_color = value,
                }
                continue;
            }
            TextToken::Align(align) => {
                // Stored like the DLL's active alignment member; the layout
                // does not shift lines yet (see the module docs).
                style.align = align;
                continue;
            }
            TextToken::Pitch(pitch) => {
                style.pitch =
                    pitch.unwrap_or_else(|| effective_real(runtime, this, "defaultPitch"));
                continue;
            }
            TextToken::Delay { absolute, value } => {
                if !ignore_delay {
                    style.char_delay = match (absolute, value) {
                        // `%a<n>;` is absolute; `%a;` and `%d;` fall back to the
                        // base delay, and `%d<n>;` is n/100 of it (`0x1000c5b8`
                        // and `0x1000c545`: an empty run seeds the 1.0f at
                        // 0x100277c0 and the store multiplies by the base at
                        // `[ebp-0x134]`).
                        (true, Some(value)) => value,
                        (false, Some(percent)) => percent / 100.0 * base_delay,
                        (_, None) => base_delay,
                    };
                }
                continue;
            }
            TextToken::Wait { value, percent } => {
                if !ignore_delay {
                    let number = match value {
                        WaitValue::Time(number) => number,
                        WaitValue::Named(name) => {
                            let evaluated = evaluate_expression(runtime, this, &name);
                            let evaluated = evaluated.trim();
                            if evaluated.is_empty() {
                                None
                            } else {
                                Some(evaluated.parse::<f64>().unwrap_or(0.0))
                            }
                        }
                    };
                    // `%w` counts hundredths of the base delay — an empty run
                    // means 100, the 1.0f at 0x100277c0 (`0x1000c658-0x1000c777`
                    // multiplies it by the base and adds it to the clock) —
                    // while `%t` adds its value directly. Two documented
                    // divergences from the reference stay here:
                    // * `%D<n>;` (`0x1000c9a9`, gated at `0x1000c977`) calls the
                    //   commit/retiming routine `FUN_100097c0` on the records
                    //   laid out since the run's start (`+0x1e0`) instead of
                    //   adding to the clock; a faithful model needs that
                    //   retiming traced, so `%D` keeps `%t`'s absolute add.
                    // * with `width_time_scale` on, `%w` scales by the stored
                    //   member `[+0x11c]` over `fontScale x size`
                    //   (`0x1000c754-0x1000c764`) rather than by a glyph extent;
                    //   this layout has no `+0x11c` equivalent and adds the
                    //   unscaled value.
                    delay += if percent {
                        number.unwrap_or(100.0) / 100.0 * base_delay
                    } else {
                        number.unwrap_or(0.0)
                    };
                }
                continue;
            }
            TextToken::Link { name } => {
                style.link = name;
                continue;
            }
            TextToken::Char(character) => character,
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
        let char_width = measure_character_width(runtime, this, font, &character, style.size)
            .unwrap_or(style.size)
            .max(1);
        if width > 0 && x > 0 && x.saturating_add(char_width) > width {
            x = 0;
            y = y.saturating_add(line_height as i64);
            line += 1;
            lines += 1;
            line_origins.push(y as f64);
        }
        let in_ruby_group = ruby_remaining > 0;
        // Character timing: the record's `delay` is the display clock value
        // reached when the character is committed — the field the character
        // object's `delay` property binds (`record+0x5c`) and `FUN_10008590`
        // (calcShowCount) compares with the elapsed time; the game paces on it
        // (`rendermsgwin.tjs`: `updateTimerInterval(l1.delay - elapsed)`).
        // Every record-producing path in the reference writes that field
        // *before* adding this character's step — the batch loop stores
        // `[+0x1d8]` at `[esi+0x38]` and only then adds the step
        // (`0x1000b097`/`0x1000b0f5`), the single-character path stores it at
        // `[ebp-0x3c]` at `0x1000a758` before `0x1000a7b2` adds it, and the
        // graph path does the same at `0x1000905c` — so the first character
        // shows at 0 and the last record sits one step below `renderDelay`.
        // The step is the current per-character delay — `render`'s argument 2
        // until a `%d`/`%a` code moves it — times the `width_time_scale`
        // factor (see [`character_step`]); `renderDelay` reports the total
        // wait times `timeScale` (its getter `FUN_10009e60` returns
        // `[+0x1e8] x [+0x4c]`).
        let display_time = delay;
        delay += character_step(width_time_scale, style.char_delay, style.size, char_width);
        let record = build_character_record(
            runtime,
            &style,
            RecordPlacement {
                x,
                y,
                line,
                width: char_width,
                height: style.size,
                delay: display_time,
            },
            vertical,
            &character,
            None,
            records.len() as i64,
        );
        records.push(Variant::Object(record));
        min_left = Some(min_left.map_or(x, |left| left.min(x)));
        max_right = max_right.max(x.saturating_add(char_width));
        max_bottom = max_bottom.max(y.saturating_add(line_height as i64));
        x = x.saturating_add(char_width);
        x = x.saturating_add(style.pitch as i64);
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
                attach_ruby(runtime, first, &ruby, group_base_width, ruby_size, ruby_offset);
            }
        }
    }
    // A trailing annotation whose group never completed still gets its ruby
    // attached to the first covered character.
    if let (Some(first), Some(ruby)) = (group_first_record, pending_ruby)
        && ruby_remaining > 0
    {
        attach_ruby(runtime, first, &ruby, group_base_width, ruby_size, ruby_offset);
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
    // The accumulator is stored unscaled: `renderDelay` multiplies it by the
    // current `timeScale` on every read, like the DLL's getter.
    state_store(runtime, this, LAYOUT_DELAY, Variant::Real(delay));
    // The waits are stored unscaled too; `getKeyWait` builds its answer with
    // the `timeScale` of the moment, the way `FUN_10009be0` multiplies on
    // every read.
    let waits = key_waits
        .into_iter()
        .map(|(pos, time)| {
            let entry = runtime.alloc_dictionary_object();
            runtime.set_object_member(entry, "pos", Variant::Integer(pos));
            runtime.set_object_member(entry, "delay", Variant::Real(time));
            Variant::Object(entry)
        })
        .collect::<Vec<_>>();
    let waits = runtime.alloc_array_object(waits);
    state_store(runtime, this, LAYOUT_KEY_WAITS, Variant::Object(waits));
    // The alignment the text ended with, stored like the DLL's active member;
    // the line layout does not shift lines (see the module docs).
    state_store(runtime, this, "align", Variant::Integer(style.align));
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
    ruby_offset: f64,
) {
    let ruby_record = runtime.alloc_dictionary_object();
    let ruby_width = ruby.chars().count() as i64 * ruby_size as i64;
    let ruby_x = (group_base_width.max(ruby_width) - ruby_width) / 2;
    runtime.set_object_member(ruby_record, "text", Variant::String(ruby.to_string()));
    runtime.set_object_member(ruby_record, "x", Variant::Integer(ruby_x));
    runtime.set_object_member(ruby_record, "left", Variant::Integer(ruby_x));
    // The ruby sits above the base glyph; `defaultRubyOffset` (reference -2)
    // nudges it, as the DLL's ruby layout does.
    runtime.set_object_member(
        ruby_record,
        "y",
        Variant::Integer(-(ruby_size as i64) + ruby_offset as i64),
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

/// `renderDelay` — the total wait the layout accumulated, times the *current*
/// `timeScale`: the DLL's getter multiplies its accumulator by `[+0x4c]` on
/// every read (`FUN_10009e60` returns `[+0x1e8] x [+0x4c]`), so a `timeScale`
/// write after the render still changes the answer. The game only tests it
/// against 0, which is how it decides between drawing the text at once and
/// running the reveal timer (`sysscn/rendermsgwin.tjs`).
fn render_delay(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle) -> Result<Variant> {
    let delay = state_real(runtime, instance, LAYOUT_DELAY).unwrap_or(0.0);
    Ok(Variant::Real(delay * value_real(runtime, instance, "timeScale")))
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

/// One element of the message text format — the escape sequences the games'
/// `TagTextConverter` writes and `textrender.dll` reads back. The variant
/// names carry the DLL anchor for each code in the module docs.
enum TextToken {
    /// An ordinary character.
    Char(char),
    /// `[ruby]` / `[ruby,count]` over the following `count + 1` characters.
    Ruby { text: String, count: usize },
    /// `\n`, a raw 0x0A, or one unit of `%n…;`.
    LineBreak,
    /// `%n<count>;`: that many line breaks.
    LineBreaks(usize),
    /// `\k`: a click wait at the current position.
    KeyWait,
    /// `\w`: one blank character cell, no glyph.
    Space,
    /// `\x`: nothing at all.
    Invisible,
    /// `\i`: the indent marker.
    Indent,
    /// `\r`: the indent-end marker.
    IndentEnd,
    /// `$expr;` / `${expr}`: inline evaluation through `onEval`.
    Eval { expression: String },
    /// `&name;`: an inline image measured through `onGetGraphSize`.
    Graph { name: String },
    /// `%f<face>;` / `%f;` (empty = the default face).
    Face { face: String },
    /// `%r`: reset the font attributes to the defaults.
    ResetFont,
    /// `%;`, `%<percent>;`, `%B`, `%S`.
    FontSize(FontSizeCode),
    /// `%b`/`%i`/`%s`/`%e` with `0`, `1` or `d` (the `d` and anything else
    /// mean "the default", which is the `None` value).
    Attribute {
        attribute: FontAttribute,
        value: Option<bool>,
    },
    /// `#rrggbb;` / `#;` and the `%e#…;` / `%s#…;` forms; `None` is the
    /// default colour.
    Color {
        target: ColorTarget,
        value: Option<i64>,
    },
    /// `%L` / `%C` / `%R`.
    Align(i64),
    /// `%p<n>;` / `%p;` (`None` = the default pitch).
    Pitch(Option<f64>),
    /// `%a<n>;` (absolute) / `%d<n>;` (percent of the base delay) / `%d;`
    /// (the base delay).
    Delay { absolute: bool, value: Option<f64> },
    /// `%t…;` / `%w…;` / `%D…;`: wait time, literal or named; `percent` marks
    /// the `%w` spelling, whose value counts hundredths of the *base* delay
    /// rather than display time.
    Wait { value: WaitValue, percent: bool },
    /// `%l<name>;` / `%l;` (empty = end the link span).
    Link { name: String },
    /// `%k<0|1|d>`: a flag with no converter emitting it.
    Flag,
}

#[derive(Clone, Copy)]
enum FontSizeCode {
    Default,
    Big,
    Small,
    Percent(f64),
}

#[derive(Clone, Copy)]
enum FontAttribute {
    Bold,
    Italic,
    Shadow,
    Edge,
}

#[derive(Clone, Copy)]
enum ColorTarget {
    Text,
    Edge,
    Shadow,
}

/// The payload of a wait code: a literal count, or the name of an expression
/// the DLL evaluates through `onEval` before reading the number.
enum WaitValue {
    /// The digits of the payload. `None` is a run without digits, which the
    /// walker treats as 1.0f for `%w` (100 hundredths) and as 0 for `%t`/`%D`.
    Time(Option<f64>),
    Named(String),
}

/// Split message text into tokens — the DLL's `FUN_1000b8c0` walk (see the
/// module docs for the code-by-code anchors). `[ruby]` runs without a comma
/// are literal text (e.g. English asides).
fn parse_message_text(text: &str) -> Vec<TextToken> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\\' => match chars.next() {
                Some('n') => tokens.push(TextToken::LineBreak),
                Some('k') => tokens.push(TextToken::KeyWait),
                Some('w') => tokens.push(TextToken::Space),
                Some('x') => tokens.push(TextToken::Invisible),
                Some('i') => tokens.push(TextToken::Indent),
                Some('r') => tokens.push(TextToken::IndentEnd),
                // The DLL turns `\t` into an ordinary tab glyph.
                Some('t') => tokens.push(TextToken::Char('\t')),
                // Any other escape is the literal character: `\[` is "[",
                // `\\` is "\", and so on.
                Some(escaped) => tokens.push(TextToken::Char(escaped)),
                None => {}
            },
            '[' => {
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
                    (!ruby.is_empty()).then_some(TextToken::Ruby {
                        text: ruby.to_string(),
                        count,
                    })
                });
                match annotation.flatten() {
                    Some(ruby) => tokens.push(ruby),
                    None => {
                        tokens.push(TextToken::Char('['));
                        tokens.extend(content.chars().map(TextToken::Char));
                        if closed {
                            tokens.push(TextToken::Char(']'));
                        }
                    }
                }
            }
            '%' => parse_percent_code(&mut chars, &mut tokens),
            '#' => tokens.push(TextToken::Color {
                target: ColorTarget::Text,
                value: parse_color(&take_to_semicolon(&mut chars)),
            }),
            '$' => {
                let expression = if chars.peek() == Some(&'{') {
                    chars.next();
                    let mut expression = String::new();
                    for next in chars.by_ref() {
                        if next == '}' {
                            break;
                        }
                        expression.push(next);
                    }
                    expression
                } else {
                    take_to_semicolon(&mut chars)
                };
                tokens.push(TextToken::Eval { expression });
            }
            '&' => tokens.push(TextToken::Graph {
                name: take_to_semicolon(&mut chars),
            }),
            other => tokens.push(TextToken::Char(other)),
        }
    }
    tokens
}

/// The `%` codes (`FUN_1000b8c0` case 0x25). A `%` the walk cannot place is a
/// directive whose payload is consumed to the next `;` and dropped — the
/// DLL's own `default` arm.
fn parse_percent_code(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    tokens: &mut Vec<TextToken>,
) {
    let Some(code) = chars.peek().copied() else {
        return;
    };
    if code.is_ascii_digit() {
        if let Some(percent) = take_digits(chars, false) {
            tokens.push(TextToken::FontSize(FontSizeCode::Percent(percent)));
        }
        return;
    }
    chars.next();
    match code {
        ';' => tokens.push(TextToken::FontSize(FontSizeCode::Default)),
        'B' => tokens.push(TextToken::FontSize(FontSizeCode::Big)),
        'S' => tokens.push(TextToken::FontSize(FontSizeCode::Small)),
        'L' => tokens.push(TextToken::Align(-1)),
        'C' => tokens.push(TextToken::Align(0)),
        'R' => tokens.push(TextToken::Align(1)),
        'a' => tokens.push(TextToken::Delay {
            absolute: true,
            value: take_digits(chars, false),
        }),
        'd' => tokens.push(TextToken::Delay {
            absolute: false,
            value: take_digits(chars, false),
        }),
        'n' => {
            // The DLL breaks the line once when the code carries no number and
            // not at all for a count below 1.
            let count = match take_digits(chars, true) {
                Some(value) if value >= 1.0 => value as usize,
                Some(_) => 0,
                None => 1,
            };
            tokens.push(TextToken::LineBreaks(count));
        }
        'p' => tokens.push(TextToken::Pitch(take_digits(chars, true))),
        'r' => tokens.push(TextToken::ResetFont),
        'f' => tokens.push(TextToken::Face {
            face: take_to_semicolon(chars),
        }),
        'l' => tokens.push(TextToken::Link {
            name: take_to_semicolon(chars),
        }),
        't' | 'w' | 'D' => tokens.push(TextToken::Wait {
            value: parse_wait_value(chars),
            percent: code == 'w',
        }),
        'b' | 'i' => {
            let attribute = if code == 'b' {
                FontAttribute::Bold
            } else {
                FontAttribute::Italic
            };
            tokens.push(TextToken::Attribute {
                attribute,
                value: take_switch(chars),
            });
        }
        's' | 'e' => {
            let attribute = if code == 's' {
                FontAttribute::Shadow
            } else {
                FontAttribute::Edge
            };
            if chars.peek() == Some(&'#') {
                chars.next();
                let target = if code == 's' {
                    ColorTarget::Shadow
                } else {
                    ColorTarget::Edge
                };
                tokens.push(TextToken::Color {
                    target,
                    value: parse_color(&take_to_semicolon(chars)),
                });
            } else {
                tokens.push(TextToken::Attribute {
                    attribute,
                    value: take_switch(chars),
                });
            }
        }
        'k' => {
            take_switch(chars);
            tokens.push(TextToken::Flag);
        }
        _ => {
            // The DLL's default arm: the rest of the directive is swallowed.
            if code != ';' {
                take_to_semicolon(chars);
            }
        }
    }
}

/// The `%t`/`%w`/`%D` payload: digits, or `$name;` to be evaluated. A payload
/// without digits stays `None` so the `%w` default (1.0f, 100 hundredths) survives.
fn parse_wait_value(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> WaitValue {
    if chars.peek() == Some(&'$') {
        chars.next();
        WaitValue::Named(take_to_semicolon(chars))
    } else {
        WaitValue::Time(take_digits(chars, false))
    }
}

/// Read up to the next `;` (consuming it); at the end of the text the run
/// simply stops, which is what the DLL's loops do.
fn take_to_semicolon(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut content = String::new();
    for next in chars.by_ref() {
        if next == ';' {
            break;
        }
        content.push(next);
    }
    content
}

/// The decimal digits of a numeric code, with the optional leading `-` of
/// `%n`/`%p`. A `;` is consumed when it follows; any other character is left
/// for the main walk, as the DLL's `break` does.
fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, signed: bool) -> Option<f64> {
    let mut digits = String::new();
    if signed && chars.peek() == Some(&'-') {
        digits.push(chars.next()?);
    }
    while let Some(next) = chars.peek().copied() {
        if !next.is_ascii_digit() {
            break;
        }
        digits.push(chars.next()?);
    }
    if chars.peek() == Some(&';') {
        chars.next();
    }
    if digits.is_empty() || digits == "-" {
        return None;
    }
    digits.parse::<f64>().ok()
}

/// The `0`/`1`/`d` value of a boolean code: `d` (or anything else) means the
/// default.
fn take_switch(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<bool> {
    match chars.next() {
        Some('1') => Some(true),
        Some('0') => Some(false),
        _ => None,
    }
}

/// A `#…;` colour value: the hex digits OR the alpha byte the DLL adds
/// (`uVar9 | 0xff000000`). An empty run is the default colour.
fn parse_color(content: &str) -> Option<i64> {
    if content.is_empty() {
        return None;
    }
    let value = i64::from_str_radix(content.trim(), 16).unwrap_or(0);
    Some((value & 0xffff_ffff) | 0xff00_0000)
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
///
/// A member the font object does not have reads back as `Void`, and
/// `Variant::to_integer` maps `Void` to 0 — so a missing `color` would paint
/// every glyph black instead of falling back to `defaultChColor`. Skip absent
/// values the way the engine's `resolve_font_member` does before converting.
fn resolve_font_int(
    runtime: &mut Runtime<KrkrHost>,
    font: ObjectHandle,
    names: &[&str],
) -> Option<i64> {
    names
        .iter()
        .find_map(|name| resolve_font_member_int(runtime, font, name))
}

/// One font attribute, `None` when the member is absent rather than zero.
fn resolve_font_member_int(
    runtime: &mut Runtime<KrkrHost>,
    font: ObjectHandle,
    name: &str,
) -> Option<i64> {
    let value = runtime.resolve_object_member(font, name).ok()?;
    if matches!(value, Variant::Void | Variant::Null) {
        return None;
    }
    value.to_integer().ok()
}

/// The glyph size a font object asks for: `height` first, then `size` for plain
/// script font-info objects. A height of 0 means "unset" — a `new Font()` starts
/// at 0 — and a negative one is a pixel size, exactly the mapping the engine's
/// own font resolution applies (`font_spec_from_object`); a font that never had
/// its height set therefore falls back to `defaultFontSize` like one that lacks
/// the member, instead of laying the glyphs out one pixel wide.
fn resolve_font_size(runtime: &mut Runtime<KrkrHost>, font: ObjectHandle) -> Option<i64> {
    for name in ["height", "size"] {
        let Some(value) = resolve_font_member_int(runtime, font, name) else {
            continue;
        };
        if value == 0 {
            continue;
        }
        return Some(value.unsigned_abs().max(1) as i64);
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
    use krkr_tjs2::{
        TjsError, TjsErrorKind,
        runtime::{NativePropertyAccess, ObjectHandle, Variant},
    };

    use super::{MemberKind, SURFACE, TextRenderPlugin};

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(TextRenderPlugin)
            .expect("register plugin");
        engine
    }

    /// Run a script against an engine with this plugin registered and return the
    /// string form of its result.
    fn run(script: &str) -> String {
        engine()
            .execute_script("probe.tjs", script)
            .expect("script")
            .to_tjs_string()
            .expect("string result")
    }

    /// The error a failing script raises, for the reference-identity checks.
    fn script_error(script: &str) -> TjsError {
        engine()
            .execute_script("probe.tjs", script)
            .expect_err("script must fail")
    }

    /// The checklist from the M27 dossier and the DLL's command vftables,
    /// independently typed: the 55 names in the DLL's registration order, with
    /// `true` for methods and `false` for properties and the reference
    /// signature each command object's mangled RTTI name carries.
    #[rustfmt::skip]
    const REFERENCE_SURFACE: &[(&str, bool, &str)] = &[
        ("setOption", true, "void (tTJSVariant)"),
        ("setDefault", true, "void (tTJSVariant)"),
        ("setRenderSize", true, "void (float, float)"),
        ("vertical", false, "bool"),
        ("timeScale", false, "float"),
        ("fontScale", false, "float"),
        ("clear", true, "void ()"),
        ("resetFont", true, "void ()"),
        ("resetStyle", true, "void ()"),
        ("setFont", true, "void (tTJSVariant)"),
        ("setStyle", true, "void (tTJSVariant)"),
        ("render", true, "bool (const tjs_char *, int, int, int, bool, bool)"),
        ("newline", true, "void ()"),
        ("done", true, "void ()"),
        ("onEval", true, "tTJSString (const tjs_char *)"),
        ("renderOver", false, "bool"),
        ("renderLines", false, "int"),
        ("renderCount", false, "int"),
        ("renderDelay", false, "float"),
        ("renderLeft", false, "float"),
        ("renderTop", false, "float"),
        ("renderRight", false, "float"),
        ("renderBottom", false, "float"),
        ("contains", true, "bool (float, float) const"),
        ("renderText", false, "const tjs_char *"),
        ("maxScrollOffset", false, "float"),
        ("maxScrollLine", false, "int"),
        ("getKeyWait", true, "tTJSVariant () const"),
        ("calcLineOffset", true, "float (int) const"),
        ("calcShowCount", true, "int (int) const"),
        ("getCharacters", true, "tTJSVariant (int, int) const"),
        ("getLinkNames", true, "tTJSVariant ()"),
        ("getLinkRects", true, "tTJSVariant (int) const"),
        ("getLinkCharacters", true, "tTJSVariant (int) const"),
        ("isLinkContains", true, "bool (int, float, float) const"),
        ("getLinkOfPosition", true, "int (float, float)"),
        ("defaultFace", false, "const tjs_char *"),
        ("defaultFontSize", false, "float"),
        ("defaultBigFontSize", false, "float"),
        ("defaultSmallFontSize", false, "float"),
        ("defaultLineSize", false, "float"),
        ("defaultLineSpacing", false, "float"),
        ("defaultPitch", false, "float"),
        ("defaultAlign", false, "int"),
        ("defaultValign", false, "int"),
        ("defaultRubySize", false, "float"),
        ("defaultRubyOffset", false, "float"),
        ("defaultChColor", false, "unsigned int"),
        ("defaultShadow", false, "bool"),
        ("defaultShadowColor", false, "unsigned int"),
        ("defaultShadowDiff", false, "int"),
        ("defaultEdge", false, "bool"),
        ("defaultEdgeColor", false, "unsigned int"),
        ("defaultBold", false, "bool"),
        ("defaultItalic", false, "bool"),
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
        for (index, ((name, is_method, signature), member)) in
            REFERENCE_SURFACE.iter().zip(SURFACE).enumerate()
        {
            assert_eq!(member.name, *name, "member {index} name");
            let method = matches!(member.kind, MemberKind::Method { .. });
            assert_eq!(
                method, *is_method,
                "member {name}: kind differs from the DLL's command object"
            );
            assert_eq!(
                member.signature, *signature,
                "member {name}: signature differs from the command's RTTI name"
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
            .map(|(name, _, _)| (*name).to_string())
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
        for (name, is_method, _) in REFERENCE_SURFACE {
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
                "begin" => "b", "end" => "e", "following" => "f", "leading" => "l",
                "vertical" => 1, "kinsoku_max" => 7, "word_break" => 0,
                "ignore_color" => 1, "ignore_size" => 1, "ignore_delay" => 1,
                "ignore_over" => 1, "ignore_overy" => 1, "ignore_overx" => 1,
                "ignore_ruby" => 1, "ignore_type" => 1, "ignore_face" => 1,
                "ignore_style" => 1, "ignore_xr" => 1, "width_time_scale" => 1,
                "bogus" => 1
            ]);
            var state = render.__krkr_text_render;
            return render.vertical + "/" + state.begin + "/" + state.end + "/" + state.following + "/"
                + state.leading + "/" + state.kinsoku_max + "/" + state.word_break + "/"
                + state.ignore_color + "/" + state.ignore_size + "/" + state.ignore_delay + "/"
                + state.ignore_over + "/" + state.ignore_overy + "/" + state.ignore_overx + "/"
                + state.ignore_ruby + "/" + state.ignore_type + "/" + state.ignore_face + "/"
                + state.ignore_style + "/" + state.ignore_xr + "/" + state.width_time_scale + "/"
                + (state.bogus === void);
            "#,
        );
        // `end` sits between `begin` and `vertical` in the DLL's comparison order
        // (`FUN_10001a70`) and is the second 10-character bracket set the game
        // passes (`system/TextRender.tjs`: `t1["end"] = "」』）'"…"` for
        // `autoIndentEndCharacters`).
        assert_eq!(value, "1/b/e/f/l/7/0/1/1/1/1/1/1/1/1/1/1/1/1/1");
    }

    /// Build A's extra style key (`FUN_10016ff0`/`FUN_100193a0`): `wordbreak`
    /// reaches the word-break flag from both commands — `setDefault` into the
    /// stored bool, `setStyle` into the active one — and `resetStyle` carries
    /// the stored one across (`:1000d5ce-1000d5d9`). Build B has no such key,
    /// so this is the union the module takes for `render`'s arity; the key is
    /// not a `setOption` key in either build, and the flag starts on.
    #[test]
    fn wordbreak_is_build_as_extra_style_key() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var probe = function() {
                var state;
                try { state = render.__krkr_text_render; } catch (e) { return "none/none"; }
                var stored = state.word_break === void ? "none" : "" + state.word_break;
                var active = state.active === void || state.active.word_break === void
                    ? "none" : "" + state.active.word_break;
                return stored + "/" + active;
            };
            var before = probe();
            render.setDefault(%["wordbreak" => 1]);
            var stored = probe();
            render.setStyle(%["wordbreak" => 0]);
            var styled = probe();
            render.setOption(%["word_break" => 1]);
            var optioned = probe();
            render.setDefault(%["wordbreak" => 0]);
            render.resetStyle();
            var reset = probe();
            render.setOption(%["wordbreak" => 1]);
            var option_only = probe();
            render.setDefault(%["wordbreak" => 7]);
            var normalized = probe();
            return before + "|" + stored + "|" + styled + "|" + optioned + "|" + reset
                + "|" + option_only + "|" + normalized;
            "#,
        );
        assert_eq!(value, "none/none|1/none|1/0|1/1|0/0|0/0|1/0");
    }

    /// `resetStyle` copies the stored word-break flag into the active slot even
    /// when no `setDefault` ever wrote one: the DLL's constructor stores 1
    /// (`10004840: movb $0x1,0x48(%edi)`), so the copy is on.
    #[test]
    fn reset_style_copies_the_constructor_word_break_flag() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            render.resetStyle();
            var state = render.__krkr_text_render;
            return (state.word_break === void ? "none" : "" + state.word_break) + "/"
                + state.active.word_break;
            "#,
        );
        assert_eq!(value, "none/1");
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
            "MS Gothic/1193046/0/16711935/4/1/65280/1/9/3/1/2/-3/24/30/60/15/30/10"
        );
    }

    /// A `setDefault` without `linesize`/`linespacing` keeps the DLL's
    /// constructor defaults for them; the derived ones follow `fontsize`, and
    /// the ruby size divides by the `.rdata` double 3.0 at 0x1002c720.
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
        assert_eq!(value, "20/50/5/20/6/6.666");
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

    /// Calling a method with fewer arguments than the reference command
    /// declares fails with the engine's parameter-count error — ncbind's
    /// `doInvoke` (`ncbind.hpp:1186`: `if (_numparams < SelectorT::ArgsCount)
    /// return TJS_E_BADPARAMCOUNT`, its `ArgsCount` the parameter count of the
    /// member-function signature, `ncbind.hpp:1231`) — and extra arguments are
    /// ignored. `render`'s two shipped builds declare five (PARQUET,
    /// `P8TextRender@@AE_NPB_WHHH_N@Z`) and six (GINKA/少女世界,
    /// `P8TextRender@@AE_NPB_WHHH_N1@Z`) parameters, so the registration
    /// accepts both flavours and rejects four arguments or fewer. The rest of
    /// the surface takes its own command's count: `setRenderSize` and
    /// `getCharacters` two (`P8TextRender@@AEXMM@Z`,
    /// `P81@BE?AVtTJSVariant@@HH@Z`), `getLinkRects`/`getLinkCharacters` one
    /// (`tTJSVariant(int) const`) and `isLinkContains` three
    /// (`bool(int,float,float) const`). No shipped call site passes a shorter
    /// list — the games write `setRenderSize(w, h)` and `getCharacters(0, 0)`
    /// — so the floors only turn a silent short call into the reference's
    /// error.
    #[test]
    fn methods_enforce_their_reference_argument_contracts() {
        for (member, call) in [
            ("setOption", "render.setOption()"),
            ("setDefault", "render.setDefault()"),
            ("setRenderSize", "render.setRenderSize()"),
            ("setRenderSize", "render.setRenderSize(100)"),
            ("render", "render.render()"),
            ("render", "render.render(\"a\", 1, 25)"),
            ("render", "render.render(\"a\", 1, 25, 0)"),
            ("contains", "render.contains(1)"),
            ("getLinkOfPosition", "render.getLinkOfPosition(1)"),
            ("onEval", "render.onEval()"),
            ("getCharacters", "render.getCharacters()"),
            ("getCharacters", "render.getCharacters(1)"),
            ("getLinkRects", "render.getLinkRects()"),
            ("getLinkCharacters", "render.getLinkCharacters()"),
            ("isLinkContains", "render.isLinkContains(0, 1)"),
        ] {
            let error = script_error(&format!("var render = new TextRenderBase();\n{call};"));
            assert_eq!(
                error.kind,
                TjsErrorKind::BadParamCount,
                "{member} {call}: {}",
                error.message
            );
            assert_eq!(
                error.kind.tjs_error_code(),
                Some(-1004),
                "{member} {call} must be the reference's TJS_E_BADPARAMCOUNT"
            );
            assert_eq!(error.message, "Invalid argument count", "{member} {call}");
        }

        // The exact and longer shapes the reference accepts — including the
        // calls the games make — still run; extras are ignored, so a
        // six-argument `render` works against PARQUET's five-parameter
        // declaration too.
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.setRenderSize(400, 0, 1);
            render.render("abc", 1, 0, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var extra = render.getCharacters(1, 1, "ignored");
            var links = render.getLinkRects(0).count + render.getLinkCharacters(0).count
                + render.isLinkContains(0, 1, 1) + render.isLinkContains(0, 1, 1, 2);
            // Each render replaces the record list, so the five-argument call
            // comes after the reads; that it runs at all is the assertion.
            render.render("a", 1, 25, 0, void);
            return chars.count + "/" + extra.count + "/" + links;
            "#,
        );
        assert_eq!(value, "3/1/0");
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
            var started = render.render("あいうえお", 1, 0, 0, void, 0);
            var glyph = font.getEscWidthX("あ");
            var chars = render.getCharacters(0, 0);
            var first = chars[0];
            var last = chars[4];
            var waits = render.getKeyWait();
            var right = render.renderRight;
            var text = render.renderText;
            var inside = render.contains(1, 1);
            var outside = render.contains(10000, 10000);
            render.setRenderSize(60, 0);
            var wrapped = render.render("ああああああ", 1, 0, 0, void, 0);
            var lines = render.renderLines;
            render.resetFont();
            render.setRenderSize(400, 0);
            render.render("ab", 1, 1, 0, void, 0);
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

    /// The option keys the layout acts on: `width_time_scale` scales each
    /// character's step by the glyph's own extent over the glyph's pixel size
    /// (the factor `FUN_1000a1f0`'s `0x1000a76a-0x1000a791` applies to the
    /// current char delay, so a glyph as wide as its em charges the whole
    /// delay), and `timeScale` multiplies the reported total — the behaviour
    /// `renderDelay` exists for.
    #[test]
    fn render_delay_follows_width_time_scale_and_time_scale() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("ab", 1, 1, 0, void, 0);
            var per_char = render.renderDelay;
            var widths = font.getEscWidthX("a") + font.getEscWidthX("b");
            render.setOption(%["width_time_scale" => 1]);
            render.render("ab", 1, 1, 0, void, 0);
            var per_width = render.renderDelay;
            render.timeScale = 2;
            render.render("ab", 1, 1, 0, void, 0);
            var scaled = render.renderDelay;
            return per_char + "/" + per_width + "/" + widths + "/" + scaled;
            "#,
        );
        let parts = value.split('/').collect::<Vec<_>>();
        let per_char = parts[0].parse::<f64>().expect("per-character delay");
        let per_width = parts[1].parse::<f64>().expect("per-width delay");
        let widths = parts[2].parse::<f64>().expect("measured advances");
        let scaled = parts[3].parse::<f64>().expect("scaled delay");
        assert!(
            (per_char - 2.0).abs() < 0.0001,
            "one base tick per glyph with the option off: {value}"
        );
        let expected = widths / 20.0;
        assert!(
            (per_width - expected).abs() < 0.0001,
            "the option charges delay x extent / size ({expected}): {value}"
        );
        assert!(
            (scaled - per_width * 2.0).abs() < 0.0001,
            "timeScale scales the same total: {value}"
        );
    }

    /// The typewriter clock: `render`'s argument index 2 is the per-character
    /// base delay the games pass as `kag.actualChSpeed`. `0x1000b8f4-0x1000b912`
    /// computes it and `0x1000ba47` stores it at member `+0x174`; every
    /// character advances the display clock by it and the record carries the
    /// clock value reached *before* its own step — the walker zeroes `+0x1d8`
    /// at `0x1000ba31` and the record paths store it before adding the step
    /// (`0x1000b097`, `0x1000a758`, `0x1000905c`), so the first character is
    /// due at 0 ms and `renderDelay` is the time after the last
    /// (`0x1000bacc`'s commit hands `+0x1dc + base` to `+0x1e8`, the member
    /// `renderDelay` reads). Before the fix the port ignored the argument and
    /// charged one tick per glyph, so a whole message was revealed inside a
    /// single frame.
    #[test]
    fn render_uses_its_second_argument_as_the_base_delay() {
        let value = run(r#"
            // TJS prints a real zero as "+0.0"; the delay table reads better
            // with the reference's own "0".
            function d(value) { return value == 0 ? "0" : "" + value; }
            var render = new TextRenderBase();
            render.setRenderSize(400, 100);
            var started = render.render("abcd", 1, 25, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var delays = d(chars[0].delay) + "/" + d(chars[1].delay) + "/" + d(chars[2].delay) + "/"
                + d(chars[3].delay);
            return started + "/" + render.renderDelay + "/" + delays + "/"
                + render.calcShowCount(0) + "/" + render.calcShowCount(24) + "/"
                + render.calcShowCount(25) + "/" + render.calcShowCount(49) + "/"
                + render.calcShowCount(50) + "/" + render.calcShowCount(99) + "/"
                + render.calcShowCount(100);
            "#);
        // 25 ms per character: display times 0/25/50/75 and a total of 100. The
        // first character is due at 0, and each further multiple of 25 reveals
        // one more.
        assert_eq!(value, "1/100/0/25/50/75/1/1/2/2/3/4/4");
    }

    /// The timing codes, against that base: the walker's `a` arm stores the
    /// absolute value (`0x1000c5b8`), its `d` arm stores n/100 of the base with
    /// an empty run storing the base itself (`0x1000c545`, `DAT_100277c0` the
    /// 1.0f), its `w` arm *adds* n/100 of the base to the display clock
    /// (`0x1000c658-0x1000c777`), and `t` adds its value (`0x1000c83c`). The
    /// pre-fix port charged the raw numbers instead.
    #[test]
    fn render_scales_the_timing_codes_by_the_base_delay() {
        let value = run(r#"
            function d(value) { return value == 0 ? "0" : "" + value; }
            function probe(text) {
                var render = new TextRenderBase();
                render.setRenderSize(400, 100);
                render.render(text, 0, 25, 0, void, 0);
                var chars = render.getCharacters(0, 0);
                var delays = "";
                var i = 0;
                while (i < chars.count) { delays += (i > 0 ? "," : "") + d(chars[i].delay); i = i + 1; }
                return render.renderDelay + ":" + delays;
            }
            return probe("ab%d50;cd") + "/" + probe("ab%d;cd") + "/" + probe("ab%a50;cd") + "/"
                + probe("ab%w100;cd") + "/" + probe("ab%w;cd") + "/" + probe("ab%t50;cd");
            "#);
        // `%d50;`  -> 0,25,50,62.5   (half of the base from there on)
        // `%d;`    -> 0,25,50,75     (the base itself)
        // `%a50;`  -> 0,25,50,100    (absolute, 50 each)
        // `%w100;` -> 0,25,75,100    (the clock takes one base tick first)
        // `%w;`    -> 0,25,75,100    (an empty run counts 100 hundredths)
        // `%t50;`  -> 0,25,100,125   (absolute display time)
        assert_eq!(
            value,
            "75:0,25,50,62.5/100:0,25,50,75/150:0,25,50,100/125:0,25,75,100/\
             125:0,25,75,100/150:0,25,100,125"
        );
    }

    /// The `0.001` fallback (`0x1000b8f4-0x1000b912`): a zero argument 2
    /// together with a positive argument 3 uses the `.rdata` single at
    /// 0x100277b8, 0.001f, so a caller that asks for an already-running
    /// animation still gets a non-zero clock; zero or negative argument 3
    /// leaves the delay at 0 and the text appears at once (`renderDelay == 0`
    /// is what the game tests).
    #[test]
    fn render_falls_back_to_the_reference_point_zero_zero_one_delay() {
        let value = run(r#"
            function probe(text, arg2, arg3) {
                var render = new TextRenderBase();
                render.setRenderSize(400, 100);
                render.render(text, 0, arg2, arg3, void, 0);
                var chars = render.getCharacters(0, 0);
                return ((chars[0].delay * 1000 + 0.5) | 0) + "/"
                    + ((chars[1].delay * 1000 + 0.5) | 0) + "/"
                    + ((render.renderDelay * 1000 + 0.5) | 0);
            }
            return probe("ab", 0, 10) + "/" + probe("ab", 0, 0) + "/" + probe("ab", 0, -5) + "/"
                + (function() {
                    var render = new TextRenderBase();
                    render.setRenderSize(400, 100);
                    render.render("ab", 0, 0, 0, void, 0);
                    return render.renderDelay == 0;
                })();
            "#);
        // One and two milliseconds after rounding for the fallback pair (the
        // first character sits at 0), then plain zeros where the fallback does
        // not apply (two probes), and a 1 for the `renderDelay == 0` the game
        // tests.
        assert_eq!(value, "0/1/2/0/0/0/0/0/0/1");
    }

    /// `timeScale` still scales the reveal: `renderDelay` and `calcShowCount`
    /// multiply the stored display times on every read (the DLL's getters read
    /// `+0x4c` at each call), so a write after the render changes both.
    #[test]
    fn time_scale_still_scales_the_reveal() {
        let value = run(r#"
            var render = new TextRenderBase();
            render.setRenderSize(400, 100);
            render.render("abcd", 1, 25, 0, void, 0);
            var before = render.calcShowCount(99) + "/" + render.renderDelay;
            render.timeScale = 2;
            var after = render.calcShowCount(99) + "/" + render.renderDelay;
            return before + "/" + after;
            "#);
        // At 99 ms all four characters are due (display times 0/25/50/75); at
        // half speed only two of them are, and the reported total doubles.
        assert_eq!(value, "4/100/2/200");
    }

    /// `ignore_delay` (key string 0x100274d4, stored at member `+0x16a` by
    /// `0x10018e5f`) suppresses the timing codes themselves — the walker gates
    /// their five handlers on it (`%d` `0x1000c54b`, `%a` `0x1000c5fb`, `%w`
    /// `0x1000c6fe`, `%t` `0x1000c83c`, `%D` `0x1000c977`) — while the base
    /// delay from `render`'s argument 2 keeps its effect.
    #[test]
    fn ignore_delay_suppresses_the_timing_codes_but_not_the_base() {
        let value = run(r#"
            function d(value) { return value == 0 ? "0" : "" + value; }
            var render = new TextRenderBase();
            render.setRenderSize(400, 100);
            render.render("ab%d50;c%t1000;d", 0, 25, 0, void, 0);
            var active = render.getCharacters(0, 0);
            var with_codes = d(active[0].delay) + "/" + d(active[1].delay) + "/" + d(active[2].delay)
                + "/" + d(active[3].delay) + "/" + render.renderDelay;
            render.setOption(%["ignore_delay" => 1]);
            render.render("ab%d50;c%t1000;d", 0, 25, 0, void, 0);
            var ignored = render.getCharacters(0, 0);
            return with_codes + "/" + d(ignored[0].delay) + "/" + d(ignored[1].delay) + "/"
                + d(ignored[2].delay) + "/" + d(ignored[3].delay) + "/" + render.renderDelay;
            "#);
        // Live: `%d50;` halves the per-character delay for `c` (12.5) and
        // `%t1000;` adds a full second to the clock before `d`. Ignored: every
        // character keeps the 25 ms base and the wait is dropped.
        assert_eq!(value, "0/25/50/1062.5/1075/0/25/50/75/100");
    }

    /// `getCharacters`' second argument is a count, 0 meaning "to the end"
    /// (`FUN_10003d90`: `if (arg2 == 0) arg2 = renderCount - arg1`). PARQUET
    /// asks for `getCharacters(0, 0)` in its message renderer and its redraw
    /// path and draws every record it gets back, so an empty answer draws no
    /// dialogue at all.
    #[test]
    fn get_characters_second_argument_is_a_count() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("abcde", 1, 0, 0, void, 0);
            var all = render.getCharacters(0, 0);
            var tail = render.getCharacters(2, 0);
            var slice = render.getCharacters(1, 2);
            var none = render.getCharacters(4, 1);
            var from_three = render.getCharacters(3, 0);
            var texts = "";
            var i = 0;
            while (i < slice.count) { texts += slice[i].text, i = i + 1; }
            return all.count + "/" + all[4].text + "/" + tail.count + "/" + slice.count + "/"
                + texts + "/" + none.count + "/" + from_three.count;
            "#,
        );
        assert_eq!(value, "5/e/3/2/bc/1/2");
    }

    /// `calcShowCount(elapsed)` (`FUN_10008590`/`FUN_10011080`) is the
    /// typewriter clock: the number of characters whose display time — the
    /// record's `delay`, the clock value reached before the glyph — times
    /// `timeScale` has come at `elapsed` milliseconds. `delay` is that value
    /// as the DLL's layout writes it (`record+0x5c`, the field the character
    /// object's `delay` property binds), so the first character is due at 0.
    #[test]
    fn calc_show_count_reveals_characters_over_elapsed_time() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("abcd", 1, 1, 0, void, 0);
            var start = render.calcShowCount(0);
            var one = render.calcShowCount(1);
            var two = render.calcShowCount(2);
            var all = render.calcShowCount(100);
            var chars = render.getCharacters(0, 0);
            // TJS prints a real zero as "+0.0"; report the reference's "0".
            var display_times = (chars[0].delay == 0 ? "0" : chars[0].delay) + "/"
                + chars[1].delay + "/" + chars[3].delay;
            // The DLL's character objects have no `time` (that name belongs to
            // the keyWait entries), and a Dictionary miss reads as void.
            var stray_time_key = chars[0].time === void ? 1 : 0;
            render.timeScale = 2;
            var scaled = render.calcShowCount(2);
            return start + "/" + one + "/" + two + "/" + all + "/" + display_times + "/"
                + stray_time_key + "/" + scaled + "/" + render.renderDelay;
            "#,
        );
        // The first glyph is due at 0 and each further tick reveals one more, so
        // at 2 three have arrived; the records carry display times 0/1/3 with no
        // extra `time` key, and with `timeScale` 2 only two of them are due at
        // 2 (0 and 2) while the total doubles.
        assert_eq!(value, "1/2/3/4/0/1/3/1/2/8");
    }

    /// `render`'s remaining arguments are not a font size: PARQUET passes the
    /// auto-indent there (1 by default) and sets the size through the Font, so
    /// reading argument 1 as a size laid every glyph out one pixel wide.
    #[test]
    fn render_arguments_beyond_the_text_do_not_resize_the_glyphs() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("a", 1, 0, 0, void, 0);
            var char = render.getCharacters(0, 0)[0];
            return char.size + "/" + (char.width > 1);
            "#,
        );
        assert_eq!(value, "20/1");
    }

    /// A font object that lacks a member must fall back, not read 0: a missing
    /// `color` used to paint every glyph black (the game hands `layer.font`, a
    /// self-bound native Font with no `color`) because `Void.to_integer()` is 0,
    /// and a missing `height`/`size` would size the glyphs at 0. A *present*
    /// height of 0 counts as unset too — a `new Font()` starts at 0, and the
    /// engine's own font path maps that to the default height.
    #[test]
    fn missing_font_members_fall_back_instead_of_reading_zero() {
        let value = run(
            r#"
            class SizedFont {
                function SizedFont() { this.height = 20; }
            }
            class ZeroFont {
                function ZeroFont() { this.height = 0; }
            }
            class BareFont {
            }
            var render = new TextRenderBase();
            render.setFont(new SizedFont());
            render.setRenderSize(400, 0);
            render.render("a", 1, 0, 0, void, 0);
            var sized = render.getCharacters(0, 0)[0];
            render.setFont(new ZeroFont());
            render.render("b", 1, 0, 0, void, 0);
            var zero = render.getCharacters(0, 0)[0];
            render.setFont(new BareFont());
            render.render("c", 1, 0, 0, void, 0);
            var bare = render.getCharacters(0, 0)[0];
            render.setFont(new Font());
            render.render("d", 1, 0, 0, void, 0);
            var native = render.getCharacters(0, 0)[0];
            // A negative height is a pixel size in the engine's own font
            // resolution, so `-20` must lay out at 20, not fall back.
            render.font.height = -20;
            render.render("e", 1, 0, 0, void, 0);
            var negative = render.getCharacters(0, 0)[0];
            return sized.size + "/" + sized.color + "/" + zero.size + "/" + bare.size + "/"
                + native.size + "/" + negative.size + "/" + bare.color + "/" + render.defaultChColor;
            "#,
        );
        // The colour falls back to `defaultChColor` (0xffffffff) instead of 0,
        // and the size to `defaultFontSize` (24) whenever the font carries no
        // usable height — absent, 0, or a fresh `new Font()`.
        assert_eq!(value, "20/4294967295/24/24/24/20/4294967295/4294967295");
    }

    /// A ruby annotation count read out of message text is clamped instead of
    /// overflowing the group counter.
    #[test]
    fn ruby_annotation_count_cannot_overflow() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("[あ,18446744073709551615]ab", 1, 0, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var ruby = "";
            if (chars[0].ruby !== void) {
                ruby = chars[0].ruby.text;
            }
            return chars.count + "/" + chars[0].text + "/" + ruby;
            "#,
        );
        assert_eq!(value, "2/a/あ");
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
            render.render("abc", 1, 0, 0, void, 0);
            var before = render.renderCount;
            render.clear();
            var cleared = render.renderCount;
            render.setFont(font);
            render.render("a", 1, 0, 0, void, 0);
            render.newline();
            render.render("b", 1, 0, 0, void, 0);
            var after_break = render.renderLines;
            var chars = render.getCharacters(0, 0);
            // `newline` breaks the layout before the next text, so the second
            // render holds the one glyph that follows the break.
            var break_y = chars[0].y;
            // `system/LangRender.tjs` writes a default and applies it with the
            // matching reset: `defaultFace = kag.getLanguageFont(a5), resetFont()`
            // and `defaultLineSpacing = …, resetStyle()`. The resets copy the
            // stored defaults into the active style and never clear them.
            render.setDefault(%["face" => "Lang", "fontsize" => 30, "linespacing" => 20]);
            render.resetFont();
            render.resetStyle();
            render.render("a\nb", 1, 0, 0, void, 0);
            var styled = render.getCharacters(0, 0);
            var active = styled[0].face + "/" + styled[1].y;
            var defaults = render.defaultFace + "/" + render.defaultFontSize + "/"
                + render.defaultLineSpacing;
            // `setFont(dict)` writes the active style, not the default — and it
            // must not clobber the `font` member the game's own `onGetTextWidth`
            // measures through (`system/LangRender.tjs` calls
            // `TextRenderBase.setFont(%["face" => defaultFace])`).
            render.setFont(%["face" => "Pressed"]);
            render.render("c", 1, 0, 0, void, 0);
            var pressed = render.getCharacters(0, 0)[0].face + "/" + render.defaultFace + "/"
                + (render.font !== void);
            var sliced = render.getCharacters(0, 1).count;
            return before + "/" + cleared + "/" + chars.count + "/" + after_break + "/"
                + (break_y > 0) + "/" + active + "/" + defaults + "/" + pressed + "/" + sliced + "/"
                + render.getLinkNames().count + "/" + render.getLinkRects(0).count + "/"
                + render.getLinkCharacters(0).count + "/" + render.isLinkContains(0, 1, 1) + "/"
                + render.getLinkOfPosition(1, 1);
            "#,
        );
        // The active face reaches the character records, the reset style's line
        // spacing (20) steps the second line to 20 + the derived line size 30,
        // the defaults the script wrote survive the resets, and the `font`
        // member the game measures through is untouched by the dictionary call.
        assert_eq!(
            value,
            "3/0/1/2/1/Lang/50/Lang/30/20/Pressed/Lang/1/1/0/0/0/0/-1"
        );
    }

    /// The script-facing members a subclass relies on: `vertical` read through
    /// the property (the bytecode reads it as a bare symbol), `onEval` on a
    /// subclass, the character objects' callback slots, and the `onFontChange`
    /// notification the DLL fires when the active font attributes change.
    #[test]
    fn subclass_reads_vertical_and_keeps_its_own_callbacks() {
        let value = run(
            r#"
            class ProbeRender extends TextRenderBase {
                function ProbeRender() {
                    TextRenderBase.TextRenderBase();
                    this.fontChanges = 0;
                }
                function readVertical() {
                    return vertical;
                }
                function onEval(text) {
                    return "eval:" + text;
                }
                // The DLL passes the active style; the game's own handler reads
                // `a0.face` / `a0.bold` / `a0.italic` off it.
                function onFontChange(style) {
                    this.seenFace = style.face;
                    this.seenBold = style.bold;
                    this.fontChanges = fontChanges + 1;
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
            render.setDefault(%["face" => "First"]);
            render.resetFont();
            render.setFont(%["face" => "Second", "bold" => 1]);
            return before + "/" + literal + "/" + via_dictionary + "/" + render.onEval("1+1") + "/"
                + (render.onGetTextWidth === void) + "/" + render.fontChanges + "/"
                + render.seenFace + "/" + render.seenBold;
            "#,
        );
        assert_eq!(value, "0/1/1/eval:1+1/1/2/Second/1");
    }

    /// `setStyle` (`FUN_10002e30`) writes the *active* layout, so a later
    /// `setDefault` with the same key does not disturb it until a `resetStyle`
    /// copies the default in.
    #[test]
    fn set_style_writes_the_active_layout_not_the_defaults() {
        let value = run(
            r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.setStyle(%["pitch" => 5, "linespacing" => 12]);
            render.render("ab", 1, 1, 0, void, 0);
            var line = render.getCharacters(0, 0);
            var pitch = line[1].left - line[0].left - line[0].width;
            render.render("a\nb", 1, 0, 0, void, 0);
            var styled = render.getCharacters(0, 0);
            var line_step = styled[1].y;
            render.setDefault(%["linespacing" => 40]);
            render.render("a\nb", 1, 0, 0, void, 0);
            var after_default = render.getCharacters(0, 0)[1].y;
            render.resetStyle();
            render.render("a\nb", 1, 0, 0, void, 0);
            var after_reset = render.getCharacters(0, 0)[1].y;
            return pitch + "/" + line_step + "/" + after_default + "/" + after_reset + "/"
                + render.defaultLineSpacing;
            "#,
        );
        // pitch 5 reaches the advance, the active line spacing 12 steps the
        // second line to 24 + 12, the default 40 stays out of the layout until
        // `resetStyle` applies it (24 + 40).
        assert_eq!(value, "5/36/36/64/40");
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

    /// The message text format's line break: `\n` is the two characters the
    /// games' `TagTextConverter` writes (`parseR` → `addText("\\n")`) and
    /// `sysscn\option.tjs` rewrites a real newline into; `render` must break
    /// the line there instead of drawing a backslash and an `n`. M85's live
    /// probe on the game's own renderer saw four glyph records with `\` at
    /// x=24 and `n` at x=48 on line 0; this pins the two-record answer.
    #[test]
    fn render_breaks_lines_on_the_message_text_escape() {
        let value = run(r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("A\\nB", 1, 0, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var texts = "";
            var i = 0;
            while (i < chars.count) { texts += chars[i].text, i = i + 1; }
            return chars.count + "/" + texts + "/" + chars[0].line + "/" + chars[1].line + "/"
                + chars[1].y + "/" + render.renderLines;
            "#);
        // Two glyphs, `B` on the second line one line height (24 + 6) down.
        assert_eq!(value, "2/AB/0/1/30/2");
    }

    /// `\k` records a click wait for the conductor: `getKeyWait()[0].pos` is
    /// the number of characters laid out up to it (`rendermsgwin.tjs` draws
    /// `pos - cpos` more and stops showing when `calcShowCount` reaches it)
    /// and `.time` is the display time reached there times the current
    /// `timeScale` (the clock rewind in `continueClick`).
    #[test]
    fn render_key_wait_reports_position_and_time() {
        let value = run(r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("あ\\kい", 1, 1, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var waits = render.getKeyWait();
            var first = waits[0];
            render.timeScale = 2;
            var scaled = render.getKeyWait()[0].time;
            return chars.count + "/" + chars[0].text + "/" + chars[1].text + "/" + waits.count + "/"
                + first.pos + "/" + first.time + "/" + scaled;
            "#);
        // The `\k` sits between the two glyphs: one wait at pos 1, its time the
        // one displayed character. The getter scales on every read, so the
        // later `timeScale` write changes the answer.
        assert_eq!(value, "2/あ/い/1/1/1/2");
    }

    /// `%f<face>;` switches the face of the following characters and
    /// `$expr;` runs through `onEval` and lays out the result. M85's live
    /// probe saw `render("A%fuser;B$記号$;C")` draw all 15 characters
    /// literally; the reference answer draws only the real glyphs.
    #[test]
    fn render_switches_the_face_and_evaluates_inline_expressions() {
        let value = run(r#"
            class ProbeRender extends TextRenderBase {
                function ProbeRender() {
                    TextRenderBase.TextRenderBase();
                    this.evaluated = "";
                }
                function onEval(text) {
                    this.evaluated = text;
                    return "eval";
                }
            }
            var render = new ProbeRender();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("A%fuser;B$記号$;C", 1, 0, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var texts = "";
            var i = 0;
            while (i < chars.count) { texts += chars[i].text, i = i + 1; }
            return chars.count + "/" + texts + "/" + chars[0].face + "/" + chars[1].face + "/"
                + render.evaluated;
            "#);
        // `%fuser;` reaches the characters after it only, and the evaluated
        // text replaces the code ($記号$; evaluates to `記号$`).
        assert_eq!(value, "7/ABevalC/normal/user/記号$");
    }

    /// The attribute codes the converters emit: `%i1…%id` (italic on and back
    /// to the default), `%120;` (120% of the default size, the `.rdata` 100.0
    /// divisor), `#rrggbb;` (the colour, ORed with the DLL's 0xff000000).
    /// A directive moves the render's own style; the instance properties stay
    /// untouched.
    #[test]
    fn render_moves_the_active_style_with_the_attribute_codes() {
        let value = run(r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("a%i1b%idc%120;d#ff0000;e", 1, 0, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var texts = "";
            var i = 0;
            while (i < chars.count) { texts += chars[i].text, i = i + 1; }
            return chars.count + "/" + texts + "/" + chars[0].italic + "/" + chars[1].italic + "/"
                + chars[2].italic + "/" + chars[2].size + "/" + chars[3].size + "/" + chars[4].color + "/"
                + render.defaultChColor;
            "#);
        // The size is the default size (24) times the percentage, not the
        // Font's 20: 24 x 1.2 rounds to 29.
        assert_eq!(value, "5/abcde/0/1/0/20/29/4294901760/4294967295");
    }

    /// The pausing and blank codes: `%t<ms>;` adds its time to the display
    /// clock (the DLL's wait accumulator 0x76), `\w` advances the pen one
    /// character cell without a record and `\x` does nothing at all, and
    /// `%n<count>;` is that many line breaks.
    #[test]
    fn render_waits_advance_the_clock_and_blank_codes_draw_nothing() {
        let value = run(r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("a\\w\\x%t1000;b%n2;c", 1, 1, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var texts = "";
            var i = 0;
            while (i < chars.count) { texts += chars[i].text, i = i + 1; }
            var glyph = chars[0].width;
            return chars.count + "/" + texts + "/" + (chars[1].left - glyph) + "/"
                + chars[1].delay + "/" + chars[1].line + "/" + chars[2].line + "/"
                + render.renderLines + "/" + render.renderDelay;
            "#);
        // `b` sits one blank cell — the current glyph size, 20 — after `a`
        // (whose advance the Font measured), carries the 1000 the wait added on
        // top of `a`'s tick (its record's delay is the clock value reached
        // before its own tick), and `c` lands two lines below it.
        assert_eq!(value, "3/abc/20/1001/0/2/3/1003");
    }

    /// A `%…;` the walk cannot place is consumed up to its `;` and draws
    /// nothing — the DLL's own `default` arm, which is why the games quote a
    /// literal `%` as `\%`. Quoted punctuation comes back through the same
    /// backslash rule (`\[` is "[", `\\` is "\").
    #[test]
    fn render_consumes_directives_it_does_not_know() {
        let value = run(r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(400, 0);
            render.render("a%zz;b\\[c\\\\d", 1, 0, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var texts = "";
            var i = 0;
            while (i < chars.count) { texts += chars[i].text, i = i + 1; }
            return chars.count + "/" + texts;
            "#);
        assert_eq!(value, "6/ab[c\\d");
    }

    /// GINKA's `cr_data.xp3 > scn101.ks.scn`, scene `*start`, texts[137] (the
    /// same string is in scn114 texts[226]/[237]): the stored field is
    /// `「それはない」\n「ないない」` — 15 characters, the escape counted as the
    /// two characters it is. The reference must draw two lines holding the 13
    /// real glyphs.
    #[test]
    fn render_draws_the_ginka_escaped_game_string_on_two_lines() {
        let value = run(r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(800, 0);
            render.render("「それはない」\\n「ないない」", 1, 0, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var texts = "";
            var i = 0;
            while (i < chars.count) { texts += chars[i].text, i = i + 1; }
            return chars.count + "/" + texts + "/" + chars[7].text + "/" + chars[7].line + "/"
                + chars[7].y + "/" + render.renderLines;
            "#);
        // The seventh character closes the first quotation, and the eighth
        // opens the second line.
        assert_eq!(value, "13/「それはない」「ないない」/「/1/30/2");
    }

    /// 少女世界的生存之道's `data.xp3 > scn/a01_1.txt.scn` (scene `*start`)
    /// opens with `一连串的爆炸平息之后，\n我将枪管架在沙袋的凹槽上，悄悄探出头观察外面的情况。`
    /// — one of the 141 distinct strings in that scene carrying the escape.
    #[test]
    fn render_draws_the_qtsj_escaped_game_string_on_two_lines() {
        let value = run(r#"
            var render = new TextRenderBase();
            var font = new Font();
            font.height = 20;
            render.setFont(font);
            render.setRenderSize(800, 0);
            render.render("一连串的爆炸平息之后，\\n我将枪管架在沙袋的凹槽上，悄悄探出头观察外面的情况。", 1, 0, 0, void, 0);
            var chars = render.getCharacters(0, 0);
            var texts = "";
            var i = 0;
            while (i < chars.count) { texts += chars[i].text, i = i + 1; }
            return chars.count + "/" + texts + "/" + chars[11].text + "/" + chars[11].line + "/"
                + chars[11].y + "/" + render.renderLines;
            "#);
        assert_eq!(
            value,
            "37/一连串的爆炸平息之后，我将枪管架在沙袋的凹槽上，悄悄探出头观察外面的情况。/我/1/30/2"
        );
    }
}
