//! `extNagano.dll` — its twelve transition providers, registered through the
//! plugin-facing transition registry.
//!
//! The DLL ships without source. Everything below is read out of the shipped
//! image (`/Users/ruri/Downloads/PARQUET/plugin/extNagano.dll`, PE32 i386,
//! 241 664 B, MD5 `d684c05b7816ad59b1990a1effdcb2ff`); the addresses in the
//! comments are that image's virtual addresses and the decompilation is the
//! corpus at `/tmp/m27/out/extNagano.dll.dump.txt` plus the provider pass at
//! `/tmp/m27/out/extnagano_providers.decomp.txt` (the dossier's Ghidra recipe
//! with `DecompSome.java`). `docs/plugins/extnagano.md` is the surface survey
//! this module works from and `docs/plugins/transitions.md` §3 its engine side.
//!
//! # What the DLL does at link time
//!
//! `V2Link` (`0x10007940`) calls `TVPInitImportStub`, logs its banner and then
//! registers **twelve** providers through `TVPAddTransHandlerProvider`
//! (`TransIntf.cpp:307`), one call per provider; `V2Unlink` (`0x10007a90`)
//! removes the same twelve. Each provider implements
//! `iTVPTransHandlerProvider` (`transhandler.h:306-332`) — a `GetName` string
//! plus a `StartTransition` factory — and each factory builds an
//! `iTVPDivisibleTransHandler` (`:242-276`). The twelve `GetName` methods are
//! the exact KAG `transmethod` spellings:
//!
//! | provider `GetName` | `StartTransition` | handler | `Process` |
//! |---|---|---|---|
//! | `3duniversal` `0x10001ac0` | `0x100026e0` | `0x1002b324` | `0x100015f0` |
//! | `blurfade` `0x10003d00` | `0x10004160` | `0x1002b55c` | `0x10003800` |
//! | `book` `0x10004600` | `0x10005420` | `0x1002b614` (LR `0x1002b69c`, RL `0x1002b668`) | `0x10004580` |
//! | `flutter` `0x10006040` | `0x100061a0` | `0x1002b6f0` | `0x100058d0` |
//! | `honeyturn` `0x10006ca0` | `0x10006ec0` | `0x1002b7a4` | `0x10006af0` |
//! | `imagewipe` `0x10007620` | `0x10007780` | `0x1002b83c` | `0x100072b0` |
//! | `morphing` `0x10014ff0` | `0x100152d0` | `0x1002b944` | `0x10008010` |
//! | `multiripple` `0x100161d0` | `0x10016e70` | `0x1002ba94` | `0x10015640` |
//! | `rgbfade` `0x10017480` | `0x10017680` | `0x1002bb94` | `0x10017110` |
//! | `scanline` `0x10017bc0` | `0x10017cd0` | `0x1002bc48` | `0x100179d0` |
//! | `spin` `0x10018630` | `0x100188d0` | `0x1002bce4` | `0x10018070` |
//! | `zoomfade` `0x10018ff0` | `0x10019190` | `0x1002bda4` | `0x10018c80` |
//!
//! This module registers all twelve under those names from
//! [`KrkrPlugin::register`] and removes them from
//! [`KrkrPlugin::unregister`], which is the engine's `V2Link` / `V2Unlink`
//! ([`crate::catalog`] installs the plugin, and this engine runs `register` at
//! boot *and* at the first `Plugins.link`, so the module keeps one provider
//! object per name and hands the same `Arc` back both times —
//! [`register_transition_provider`] treats a repeat of the identical object as
//! a no-op). A transition already running keeps its handler across an unlink,
//! exactly as the reference does.
//!
//! # The option vocabulary, as recovered
//!
//! Every provider reads its options through `iTVPSimpleOptionProvider`
//! (`tTVPSimpleOptionProvider`, `TransIntf.cpp:26-114`) by *name*: the plugin's
//! readers are `FUN_100018b0` (a real with a caller-supplied default) and
//! `FUN_100019c0`/`FUN_10001bf0` (an integer with a caller-supplied default),
//! each of which keeps the passed-in value when the member is absent or not a
//! number. `FUN_10001ae0` is the saturating clamp; `FUN_10001810` reads the
//! clock as milliseconds and `FUN_100017b0` tests a variant for numberness.
//! All twelve also fail `StartTransition` when the destination and source
//! rectangles differ (`src1w != src2w || src1h != src2h`), report
//! `*type = 1` (`ttExchange`, `transhandler.h:27-31`), and report
//! `*updatetype = 1` (`tutDivisible`) — except `rgbfade` and `imagewipe`,
//! which report `0` (`tutDivisibleFade`, `:46-51`).
//!
//! | name | options (recovered names; defaults/clamps as the binary reads them) |
//! |---|---|
//! | `3duniversal` | `rule` (**required**: an image object or a filename), `type` (a string compared against `HSB`), `type2` (a string), `bound1`/`bound2` (ints), `accel1`/`speed1`/`accel2`/`speed2` (reals) with the aliases `a1`/`s1`/`a2`/`s2`, `time` |
//! | `blurfade` | `blur1`, `blur2` (ints, 0), `blur1x`/`blur1y` (default *`blur1`*), `blur2x`/`blur2y` (default *`blur2`*), `exponent` (real, 1.0), `type` (0/1/2), `prerender` (flag), `time` |
//! | `book` | `dir` (int, 0; `1` picks the LR handler and anything else the RL one, and the sentinel **`-1` draws `rand() & 1`**), `time` |
//! | `flutter` | `back` (an int read by `FUN_10005f40`, 0), `slip` (8, clamped to `0..min(width, height)`), `alpha` (255, kept as a byte), `time` |
//! | `honeyturn` | `size`, `twist`, `order` (ints, **all three required**), `time` |
//! | `imagewipe` | `rule` (**required**: a filename loaded through the image provider at 32 bpp), `dir` (int), `time` |
//! | `morphing` | `before`, `after` (objects carrying `Array` and `count`; six `tjs_int` per patch, at most 256), `time` |
//! | `multiripple` | `count` (6, clamped to `1..=20`), `wavecount` (2), `rwidth` (16), `maxdrift` (48), `roundness` (1.0), `delaylast` (1.0), `callback` (a script object), `time` |
//! | `rgbfade` | `delayR`, `delayG`, `delayB`, `delayA` (ints clamped to `0..=255`), `time` |
//! | `scanline` | `time` only |
//! | `spin` | `type1` (0), `type2` (1), `time` |
//! | `zoomfade` | `zoom1` (100), `zoom2` (200), `time` |
//!
//! `time` is the transition clock in milliseconds and is **required by every
//! one of the twelve**: each factory reads it first and returns
//! `TJS_E_FAIL` (`0xffffffff`) when it is missing or not a number, which the
//! layer reports as the reference's message-only `TVPTransHandlerError`
//! (`LayerIntf.cpp:6246`). This module keeps that rule.
//!
//! # What is real here and what is a documented degrade
//!
//! Following the mission's rule — never fake pixels — each provider ships
//! exactly one of two faces, and the split is stated in [`META`]:
//!
//! * **Real** — `rgbfade` and `scanline`. Both `StartTransition` *and*
//!   `Process` were recovered line by line, the handler state is the
//!   reference's own, and the pass writes the reference's pixels (below).
//! * **Mapped to a crossfade** — the other ten. Their factories still run and
//!   their recovered option vocabulary is still parsed and enforced (a call
//!   missing what the reference requires fails here too), but the pass
//!   composes a plain crossfade: the same composite the engine's interim
//!   projection produced while this module was a marker
//!   (`crate::catalog`'s projection table, `native/classes.rs`
//!   `PLUGIN_TRANSITION_NAMES`), written with the reference's own lerp
//!   arithmetic. **Not** the reference effect — see the per-provider reasons
//!   in [`META`] and the notes on each parser.
//!
//! The blockers are specific, not a shrug:
//!
//! * `3duniversal` and `imagewipe` need the **rule image** their factory loads
//!   through `iTVPSimpleImageProvider::LoadImage` (`transhandler.h:149-166`,
//!   `0x100022e0` and `0x10007780`). The plugin-facing registry has no image
//!   provider, so a `rule` handed in as a filename can never become pixels
//!   here; the reference's perspective/rule-driven rasteriser
//!   (`FUN_10002390`/`0x100015f0`) has no input.
//! * `morphing` needs `before.Array`/`after.Array`: **nested** members of the
//!   options object (`0x100152d0` walks `Array` and `count` through
//!   `PropGetByNum`, then keeps `min(256, count/6)` patches of six `tjs_int`
//!   each per side), and a `TransitionOptions` snapshot carries the options
//!   object's *own* members only. The mesh geometry is unreachable from the
//!   handler side — and its consumer is not pinned either: the handler's
//!   constructor `FUN_10015090` is not in the corpus and `Process` runs past
//!   the decompiler's per-function cap.
//! * `multiripple` drives its ripples through the `callback` script object
//!   (`0x10016e70` stores the variant for the handler, `FUN_10016350`).
//!   Calling TJS from the pass is impossible in this channel (the handler has
//!   no runtime), so the recovered wave field would silently lose the hook
//!   that schedules it.
//! * `blurfade`, `book`, `flutter`, `honeyturn`, `spin` and `zoomfade` own
//!   substantial precomputed state — `blurfade` an internal blur-buffer class
//!   (`FUN_10003da0`/`FUN_10002a80`, plus the `prerender` cache), `book` the
//!   LR/RL page-fold mapping (`0x100046f0`/`0x10004db0`, ~200 lines each),
//!   `flutter`'s three per-band alpha ramps (`0x100058d0`), `honeyturn`'s
//!   6-byte-per-pixel fold table (`0x10006af0` + the 0x148-byte constructor
//!   `FUN_10006d40`), `spin`'s five per-column tables built from the
//!   `sin`/`cos` wrappers `FUN_10019a00`/`FUN_10019be0`, `zoomfade`'s two
//!   per-column sample tables rounded out of x87 doubles by `FUN_10019af0`.
//!   Their factories, constructors and passes are mapped (addresses above) and
//!   the option vocabulary is recovered; the per-pixel math was not pinned far
//!   enough to write pixels a reviewer could diff. They stay crossfades until
//!   a follow-up finishes that reading.
//!
//! # The two real kernels, and the channel they run on
//!
//! [`RgbFadeHandler`] is `tTVPRGBFadeTransHandler` (`0x10017520` constructor,
//! `0x10017340` `StartProcess`, `0x10017110` `Process`):
//!
//! * The constructor turns each `delayX` into **milliseconds**,
//!   `delayX * time / 255`, and computes the per-channel fade span
//!   `max(1, (255 - max_delay) * time / 255)`.
//! * `StartProcess` sets `progressX = clamp(((elapsed - delayX_ms) * 255) /
//!   span, 0, 255)` — so the channel with the largest delay finishes exactly
//!   at `time` and the others finish earlier.
//! * `Process` fills the destination with `Src1` when `elapsed == 0`, hands
//!   back `Src2` when `elapsed == time`, and otherwise lerps per channel:
//!   `out = src1 + (((src2 - src1) * progress) >> 8)` — with the reference's
//!   two degenerate faces kept (`Src2` with zero alpha multiplies `Src1`'s
//!   channels by the progress byte; `Src1` with zero alpha multiplies them and
//!   takes the alpha from `Src2`). Note the reference's own quirk: the
//!   progress tops out at 255 while the lerp shifts by 8, so the last pass of a
//!   fade lands a step short of `Src2` (a 100-unit difference reaches 199, not
//!   200) — a quirk this port keeps, because matching the reference is the
//!   point.
//!
//! [`ScanlineHandler`] is `tTVPScanLineTransHandler` (`0x10017c60`,
//! `0x100178d0`, `0x100179d0`): the split starts at 0 and advances to the
//! image width as `width * elapsed / time` — one division, at full precision,
//! the value `StartProcess` stores in the handler and `Process`/`EndProcess`
//! read back (the factory's separate `elapsed * 255 / time` progress is never
//! consulted by the pass) — and each row is composed from the two faces with
//! the *wrap-around* the reference writes —
//!
//! * an even row takes its first `split` pixels from `Src2`'s **right** end
//!   (`Src2[x + width - split]`) and the rest from `Src1` (`Src1[x - split]`);
//! * an odd row takes its first `width - split` pixels from `Src1`'s **right**
//!   end (`Src1[x + split]`) and the rest from `Src2` (`Src2[x - (width -
//!   split)]`).
//!
//!   At `split = 0` every row is `Src1`, at `split = width` every row is
//!   `Src2`, and the rows alternate which face leads on the way — which is
//!   what the alternating-row pointer arithmetic in `0x100179d0` does. Both
//!   kernels process the whole image: the engine's channel hands the handler
//!   the destination's own bitmap, so the reference's processing-rect offsets
//!   (`data->Left/Top/DestLeft/...`) are all zero here.
//!
//! **Byte order**: the reference indexes its BGRA planes from byte 0; this
//! engine's faces are R, G, B, A per pixel, top-down and tightly packed
//! (`plugin_api::transition`, `docs/plugins/plugin-facing-engine-facilities.md`
//! §B.3.4). Each channel's own progress follows its channel (`delayR` drives
//! byte 0 here and byte 2 there), and the copies a scanline pass makes are
//! byte-for-byte, so no channel reordering is needed.

use std::sync::{Arc, OnceLock};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::transition::{
        TransitionFrame, TransitionHandler, TransitionHandlerError, TransitionHandlerProvider,
        TransitionOptions, TransitionRequest, register_transition_provider,
        unregister_transition_provider,
    },
};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "the twelve extNagano transition providers (3duniversal, blurfade, book, flutter, honeyturn, imagewipe, morphing, multiripple, rgbfade, scanline, spin, zoomfade)",
    notes: "All twelve names are registered through plugin_api::transition and their recovered option vocabulary is parsed and enforced (time required, equal source/destination sizes, honeyturn's size/twist/order, imagewipe's/3duniversal's rule). Two providers are real pixel work: rgbfade (the recovered per-channel delayed fade, delays as ms of the clock and the max(1, (255 - maxdelay) * time / 255) span, reference lerp) and scanline (the recovered moving split with its wrap-around alternating rows). The other ten compose a plain crossfade — the same composite the interim projection produced — and are documented as degraded: 3duniversal and imagewipe need a rule image the plugin API cannot load, morphing needs the nested before.Array/after.Array patch meshes a snapshot cannot reach, multiripple needs its callback script object, and blurfade/book/flutter/honeyturn/spin/zoomfade have their per-pixel math (blur buffers, page-fold tables, per-column sample tables) only partly pinned.",
    install: |engine| engine.register_plugin(ExtNaganoPlugin),
};

/// The canonical DLL name: what `Plugins.link` matches and what
/// [`KrkrPlugin::name`] reports.
const PLUGIN_NAME: &str = "extNagano.dll";

/// The twelve `GetName` strings — the exact spellings `TVPFindTransHandlerProvider`
/// hashes (`TransIntf.cpp:341-359`), alphabetically.
///
/// The DLL's own registration order is `V2Link`'s (`0x10007940`), which calls one
/// register helper per provider: `0x10019040` zoomfade, `0x10003d50` blurfade,
/// `0x10017c10` scanline, `0x10002290` 3duniversal, `0x100174d0` rgbfade,
/// `0x10018680` spin, `0x10007670` imagewipe, `0x10006090` flutter,
/// `0x10004650` book, `0x10006cf0` honeyturn, `0x10015040` morphing,
/// `0x10016220` multiripple (each helper sits inside its provider's own code
/// block, whose first function is that provider's `GetName`). Order is not part
/// of the registry's contract, so this module keeps them sorted.
const PROVIDER_NAMES: [&str; 12] = [
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
];

pub struct ExtNaganoPlugin;

impl KrkrPlugin for ExtNaganoPlugin {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        for provider in providers() {
            register_transition_provider(runtime, Arc::clone(provider))?;
        }
        Ok(())
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        for name in PROVIDER_NAMES {
            unregister_transition_provider(runtime, name);
        }
        Ok(())
    }
}

/// The twelve providers, built once and handed back on every `register` — the
/// registry's duplicate-name rule makes the identical `Arc` a no-op, which is
/// what this engine needs because it runs `register` twice (boot and the first
/// `Plugins.link`).
fn providers() -> &'static [Arc<dyn TransitionHandlerProvider>; 12] {
    static PROVIDERS: OnceLock<[Arc<dyn TransitionHandlerProvider>; 12]> = OnceLock::new();
    PROVIDERS.get_or_init(|| {
        [
            Arc::new(ExtNaganoProvider::new("3duniversal", Effect::Crossfade)),
            Arc::new(ExtNaganoProvider::new("blurfade", Effect::Crossfade)),
            Arc::new(ExtNaganoProvider::new("book", Effect::Crossfade)),
            Arc::new(ExtNaganoProvider::new("flutter", Effect::Crossfade)),
            Arc::new(ExtNaganoProvider::new("honeyturn", Effect::Crossfade)),
            Arc::new(ExtNaganoProvider::new("imagewipe", Effect::Crossfade)),
            Arc::new(ExtNaganoProvider::new("morphing", Effect::Crossfade)),
            Arc::new(ExtNaganoProvider::new("multiripple", Effect::Crossfade)),
            Arc::new(ExtNaganoProvider::new("rgbfade", Effect::RgbFade)),
            Arc::new(ExtNaganoProvider::new("scanline", Effect::Scanline)),
            Arc::new(ExtNaganoProvider::new("spin", Effect::Crossfade)),
            Arc::new(ExtNaganoProvider::new("zoomfade", Effect::Crossfade)),
        ]
    })
}

/// One `iTVPTransHandlerProvider`: its `GetName` and the handler its factory
/// builds.
enum Effect {
    /// The recovered per-channel delayed fade (`0x10017520`/`0x10017110`).
    RgbFade,
    /// The recovered moving split (`0x10017c60`/`0x100179d0`).
    Scanline,
    /// A documented degrade: the composite the engine's projection produced
    /// while this module was a marker (see the module docs for each
    /// provider's blocker).
    Crossfade,
}

struct ExtNaganoProvider {
    /// The exact `GetName` spelling (`TransIntf.cpp:313`).
    name: &'static str,
    effect: Effect,
}

impl ExtNaganoProvider {
    fn new(name: &'static str, effect: Effect) -> Self {
        Self { name, effect }
    }
}

impl TransitionHandlerProvider for ExtNaganoProvider {
    fn name(&self) -> &str {
        self.name
    }

    fn start_transition(
        &self,
        request: &TransitionRequest,
    ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError> {
        // Every one of the twelve opens with the same two tests
        // (`param_11`/`param_2` non-null, then `src1w == src2w &&
        // src1h == src2h`), and every one reads `time` first and fails when it
        // is missing.
        let source_size = request.source_size.ok_or_else(|| {
            TransitionHandlerError::new(format!(
                "extNagano {} needs a transition source layer (the reference passes a null Src2)",
                self.name
            ))
        })?;
        if source_size != request.dest_size {
            return Err(TransitionHandlerError::new(format!(
                "extNagano {} needs equal source and destination sizes: destination {}x{}, source {}x{}",
                self.name, request.dest_size.0, request.dest_size.1, source_size.0, source_size.1
            )));
        }
        let time = require_time(self.name, &request.options)?;
        let options = parse_options(self.name, request)?;
        Ok(match self.effect {
            Effect::RgbFade => {
                let ProviderOptions::RgbFade { delay } = options else {
                    return Err(TransitionHandlerError::new(
                        "extNagano rgbfade options did not parse".to_string(),
                    ));
                };
                Box::new(RgbFadeHandler::new(time, delay))
            }
            Effect::Scanline => Box::new(ScanlineHandler::new(time)),
            Effect::Crossfade => Box::new(CrossfadeHandler),
        })
    }
}

// ------------------------------------------------------------------ options

/// `time` in milliseconds, which all twelve factories read first and fail on:
/// an absent member, or one that is not a number, returns `TJS_E_FAIL`
/// (`0xffffffff`) and the layer reports the official handler error.
///
/// The value is bounded to the reference's own range. `FUN_10001810` reads the
/// option as a `tjs_int` and every factory keeps it in a 32-bit slot, widening
/// it only inside 64-bit multiplies; a port that took an unbounded `i64` would
/// overflow those products (the workspace's dev and test profiles check
/// arithmetic), so the clock is clamped the way the reference's storage clamps
/// it.
fn require_time(
    name: &str,
    options: &TransitionOptions,
) -> std::result::Result<u64, TransitionHandlerError> {
    options
        .integer("time")
        .filter(|time| *time >= 0)
        .map(|time| (time as u64).min(i32::MAX as u64))
        .ok_or_else(|| TransitionHandlerError::new(format!("extNagano {name} needs option `time`")))
}

/// One provider's recovered option vocabulary, parsed from
/// [`TransitionRequest::options`].
///
/// The names and defaults are the reference's own readers (see the module
/// table). Parsing is *not* decoration: `StartTransition` fails here for the
/// same cases the reference fails — a missing `time`, `honeyturn` without its
/// three sizes, a rule-driven provider without its `rule` — so a script that
/// worked against the DLL keeps the same error behaviour.
#[derive(Clone, Debug, PartialEq)]
enum ProviderOptions {
    /// `0x100026e0`: `rule` (required), `type`/`type2` (strings; `HSB` is the
    /// one comparison the binary makes), `bound1`/`bound2`, and the reals
    /// `accel1`/`speed1`/`accel2`/`speed2` with their short aliases
    /// `a1`/`s1`/`a2`/`s2`. The reference hands `FUN_100018b0` an
    /// uninitialised stack slot as those reals' default, so an absent option
    /// reads as indeterminate there; this port keeps `None` for "absent"
    /// instead of copying a bug.
    ThreeDUniversal {
        hsb: bool,
        type2: Option<String>,
        bound1: i64,
        bound2: i64,
        speed1: Option<f64>,
        speed2: Option<f64>,
        accel1: Option<f64>,
        accel2: Option<f64>,
    },
    /// `0x10004160`: `blur1`/`blur2` and their per-axis overrides, the
    /// `exponent` real (the reader's own default is 1.0), the `type` selector
    /// and the `prerender` flag.
    BlurFade {
        blur1: i64,
        blur1x: i64,
        blur1y: i64,
        blur2: i64,
        blur2x: i64,
        blur2y: i64,
        kind: i64,
        prerender: bool,
        exponent: f64,
    },
    /// `0x10005420`: `dir`, default 0. The factory picks the LR handler when the
    /// value is exactly 1 and the RL one otherwise, and only the sentinel `-1`
    /// takes the `rand() & 1` branch (`cmpl $-1`, `0x10005507`).
    Book { dir: i64 },
    /// `0x100061a0`: the `back` int (read through
    /// `tTJSVariant::operator tTVInteger` by `FUN_10005f40`, default 0),
    /// `slip` (default 8) clamped to `0..=min(width, height)`, and `alpha`
    /// (default 255) kept as a byte.
    Flutter { back: i64, slip: i64, alpha: i64 },
    /// `0x10006ec0`: `size`, `twist` and `order`, all three required — the
    /// factory's nested reads fail the whole call when one is missing.
    HoneyTurn { size: i64, twist: i64, order: i64 },
    /// `0x10007780`: the `rule` filename (required; the reference loads it
    /// itself) and `dir`.
    ImageWipe { rule: String, dir: i64 },
    /// `0x100152d0`: whether `before`/`after` are present as objects. The
    /// reference then reads `Array` and `count` *on those objects*; a snapshot
    /// holds no runtime to traverse with, so the counts are not reachable here.
    Morphing { before: bool, after: bool },
    /// `0x10016e70`: the ripple parameters plus whether a `callback` object was
    /// handed in. The defaults are its call sites' own immediates (`count` 6,
    /// clamped to `1..=20` by the handler's constructor; `wavecount` 2,
    /// `rwidth` 16, `maxdrift` 48, `roundness` and `delaylast` 1.0) and none of
    /// them is clamped by the reader.
    MultiRipple {
        count: i64,
        wavecount: i64,
        rwidth: i64,
        maxdrift: i64,
        roundness: f64,
        delaylast: f64,
        callback: bool,
    },
    /// `0x10017680`: the four per-channel delays, each clamped to `0..=255`.
    RgbFade { delay: [i64; 4] },
    /// `0x10017cd0`: `time` is the whole vocabulary.
    Scanline,
    /// `0x100188d0`: `type1` (0) and `type2` (1) pick between the handler's
    /// per-column table builders.
    Spin { type1: i64, type2: i64 },
    /// `0x10019190`: the two zoom percentages, `zoom1` 100 and `zoom2` 200
    /// (the factory pushes `$0x64` for the first read and `$0xc8` for the
    /// second).
    ZoomFade { zoom1: i64, zoom2: i64 },
}

/// Reads one provider's options by name, with the reference's defaults, and
/// enforces the two requirements its factory does: `rule` for the two
/// rule-driven providers and `size`/`twist`/`order` for `honeyturn`.
fn parse_options(
    name: &str,
    request: &TransitionRequest,
) -> std::result::Result<ProviderOptions, TransitionHandlerError> {
    let options = &request.options;
    let integer = |member: &str, default: i64| options.integer(member).unwrap_or(default);
    match name {
        "3duniversal" => {
            // `0x100026e0` reads `rule` and bails when neither an image object
            // nor a filename could be obtained.
            if !has_rule(options) {
                return Err(TransitionHandlerError::new(
                    "extNagano 3duniversal needs option `rule` (a rule image or a filename)",
                ));
            }
            Ok(ProviderOptions::ThreeDUniversal {
                hsb: options.string("type").as_deref() == Some("HSB"),
                type2: options.string("type2"),
                bound1: integer("bound1", 0),
                bound2: integer("bound2", 0),
                speed1: real_with_alias(options, "speed1", "s1"),
                speed2: real_with_alias(options, "speed2", "s2"),
                accel1: real_with_alias(options, "accel1", "a1"),
                accel2: real_with_alias(options, "accel2", "a2"),
            })
        }
        "blurfade" => {
            let blur1 = integer("blur1", 0);
            let blur2 = integer("blur2", 0);
            Ok(ProviderOptions::BlurFade {
                blur1,
                // `blur1x`/`blur1y` keep `blur1` and `blur2x`/`blur2y` keep
                // `blur2` when the option object omits them.
                blur1x: integer("blur1x", blur1),
                blur1y: integer("blur1y", blur1),
                blur2,
                blur2x: integer("blur2x", blur2),
                blur2y: integer("blur2y", blur2),
                kind: integer("type", 0),
                prerender: options.flag("prerender"),
                exponent: options.number("exponent").unwrap_or(1.0),
            })
        }
        // `dir` keeps the reader's default 0; only the value -1 reaches the
        // factory's random branch, and only 1 selects the LR handler.
        "book" => Ok(ProviderOptions::Book {
            dir: integer("dir", 0),
        }),
        "flutter" => {
            let cap = i64::from(request.dest_size.0.min(request.dest_size.1));
            Ok(ProviderOptions::Flutter {
                back: integer("back", 0),
                slip: integer("slip", 8).clamp(0, cap),
                alpha: integer("alpha", 255),
            })
        }
        "honeyturn" => {
            let required = |member: &str| {
                options.integer(member).ok_or_else(|| {
                    TransitionHandlerError::new(format!(
                        "extNagano honeyturn needs option `{member}`"
                    ))
                })
            };
            Ok(ProviderOptions::HoneyTurn {
                size: required("size")?,
                twist: required("twist")?,
                order: required("order")?,
            })
        }
        "imagewipe" => {
            let rule = options.string("rule").ok_or_else(|| {
                TransitionHandlerError::new(
                    "extNagano imagewipe needs option `rule` (the wipe's rule image filename)",
                )
            })?;
            Ok(ProviderOptions::ImageWipe {
                rule,
                dir: integer("dir", 0),
            })
        }
        "morphing" => Ok(ProviderOptions::Morphing {
            before: options.value("before").is_some(),
            after: options.value("after").is_some(),
        }),
        "multiripple" => Ok(ProviderOptions::MultiRipple {
            count: integer("count", 6).clamp(1, 20),
            wavecount: integer("wavecount", 2),
            rwidth: integer("rwidth", 16),
            maxdrift: integer("maxdrift", 48),
            roundness: options.number("roundness").unwrap_or(1.0),
            delaylast: options.number("delaylast").unwrap_or(1.0),
            callback: options.value("callback").is_some(),
        }),
        "rgbfade" => Ok(ProviderOptions::RgbFade {
            delay: ["delayR", "delayG", "delayB", "delayA"]
                .map(|member| integer(member, 0).clamp(0, 255)),
        }),
        "scanline" => Ok(ProviderOptions::Scanline),
        "spin" => Ok(ProviderOptions::Spin {
            type1: integer("type1", 0),
            type2: integer("type2", 1),
        }),
        "zoomfade" => Ok(ProviderOptions::ZoomFade {
            zoom1: integer("zoom1", 100),
            zoom2: integer("zoom2", 200),
        }),
        _ => Err(TransitionHandlerError::new(format!(
            "extNagano has no provider named {name}"
        ))),
    }
}

/// `0x100026e0`'s `rule` test: an object (loaded through the image provider) or
/// a string (a filename handed to `LoadImage`). Anything else fails the call.
fn has_rule(options: &TransitionOptions) -> bool {
    match options.value("rule") {
        Some(value) => value.object_handle().is_some() || value.to_tjs_string().is_ok(),
        None => false,
    }
}

/// `0x100026e0` reads `speed1`/`accel1` and then the aliases `s1`/`a1` into the
/// same slot (and likewise `speed2`/`accel2` with `s2`/`a2`), so either
/// spelling sets the value and the alias wins when both are present.
fn real_with_alias(options: &TransitionOptions, name: &str, alias: &str) -> Option<f64> {
    options.number(alias).or_else(|| options.number(name))
}

// ------------------------------------------------------------ rgbfade

/// `tTVPRGBFadeTransHandler` (`0x10017520`): the recovered per-channel delayed
/// fade.
struct RgbFadeHandler {
    /// `time` in milliseconds.
    time_ms: u64,
    /// `delayX * time / 255` — the delay of channel `X` in the clock's own
    /// milliseconds (constructor `0x10017520`).
    delay_ms: [i64; 4],
    /// `max(1, (255 - max_delay) * time / 255)`, the divisor `StartProcess`
    /// turns elapsed time into a `0..=255` progress with.
    span_ms: i64,
}

impl RgbFadeHandler {
    fn new(time_ms: u64, delay: [i64; 4]) -> Self {
        let time = time_ms as i64;
        let delay_ms = delay.map(|value| value * time / 255);
        let max_delay = delay.iter().copied().max().unwrap_or(0);
        let span_ms = ((255 - max_delay) * time / 255).max(1);
        Self {
            time_ms,
            delay_ms,
            span_ms,
        }
    }

    /// `StartProcess` (`0x10017340`): one `0..=255` progress per channel, from
    /// the clock and that channel's own delay.
    fn progress(&self, elapsed_ms: u64) -> [i64; 4] {
        let elapsed = elapsed_ms as i64;
        self.delay_ms
            .map(|delay| (((elapsed - delay) * 255) / self.span_ms).clamp(0, 255))
    }
}

impl TransitionHandler for RgbFadeHandler {
    fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]) {
        let elapsed = frame.tick.as_millis() as u64;
        let Some(source) = frame.source else {
            return;
        };
        // `Process` (`0x10017110`) hands the destination straight back to
        // `Src1` at the start and to `Src2` at the end; `dest` already holds
        // `Src1`, so the start only returns and the end copies.
        if elapsed == 0 {
            return;
        }
        if elapsed >= self.time_ms {
            if source.pixels.len() == dest.len() {
                dest.copy_from_slice(source.pixels);
            }
            return;
        }
        let progress = self.progress(elapsed);
        let before = frame.dest_before;
        let width = before.width.min(source.width) as usize;
        let height = before.height.min(source.height) as usize;
        for row in 0..height {
            let start = row * before.width as usize * 4;
            let source_start = row * source.width as usize * 4;
            for column in 0..width {
                let destination = start + column * 4;
                let origin = source_start + column * 4;
                let (Some(dest_pixel), Some(src_pixel)) = (
                    dest.get(destination..destination + 4),
                    source.pixels.get(origin..origin + 4),
                ) else {
                    continue;
                };
                let [r, g, b, a] = progress;
                let composed = fade_pixel(
                    [dest_pixel[0], dest_pixel[1], dest_pixel[2], dest_pixel[3]],
                    [src_pixel[0], src_pixel[1], src_pixel[2], src_pixel[3]],
                    [r, g, b, a],
                );
                if let Some(slot) = dest.get_mut(destination..destination + 4) {
                    slot.copy_from_slice(&composed);
                }
            }
        }
    }
}

/// `0x10017110`'s per-pixel arithmetic, byte for byte, in the reference's own
/// branch order:
///
/// * `Src2` is fully transparent (`cmpb $0x0, 0x3(...)` on the second face):
///   every colour channel is `src1 * progress` **truncated to a byte**
///   (`imulb`, which keeps only the low eight bits of the product) and the
///   alpha decays as `src1.A - ((src1.A * progress) >> 8)`;
/// * `Src1` is fully transparent: the same truncated multiply for the colour,
///   with the alpha taken from `Src2` as `(src2.A * progress) >> 8`;
/// * both faces carry alpha: `src1 + (((src2 - src1) * progress) >> 8)`, with
///   the difference signed and the add wrapping (`imull` / `sarl $0x8` /
///   `addb`).
///
/// The two degenerate faces are the reference's own lossy cases (a quantised
/// product, not a blend); a call whose source layer is an all-transparent image
/// takes them, so the port keeps them rather than "fixing" the pixels.
fn fade_pixel(src1: [u8; 4], src2: [u8; 4], progress: [i64; 4]) -> [u8; 4] {
    let scaled = [
        (i64::from(src1[0]) * progress[0]) as u8,
        (i64::from(src1[1]) * progress[1]) as u8,
        (i64::from(src1[2]) * progress[2]) as u8,
    ];
    if src2[3] == 0 {
        return [
            scaled[0],
            scaled[1],
            scaled[2],
            (i64::from(src1[3]) - ((i64::from(src1[3]) * progress[3]) >> 8)) as u8,
        ];
    }
    if src1[3] == 0 {
        return [
            scaled[0],
            scaled[1],
            scaled[2],
            ((i64::from(src2[3]) * progress[3]) >> 8) as u8,
        ];
    }
    [
        fade_channel(src1[0], src2[0], progress[0]),
        fade_channel(src1[1], src2[1], progress[1]),
        fade_channel(src1[2], src2[2], progress[2]),
        fade_channel(src1[3], src2[3], progress[3]),
    ]
}

/// One channel of the reference's lerp: `src1 + (((src2 - src1) * progress)
/// >> 8)`, with the difference signed and the result truncated to a byte.
fn fade_channel(src1: u8, src2: u8, progress: i64) -> u8 {
    let difference = i64::from(src2) - i64::from(src1);
    (i64::from(src1) + ((difference * progress) >> 8)) as u8
}

// ----------------------------------------------------------- scanline

/// `tTVPScanLineTransHandler` (`0x10017c60`): the split sweeps across the
/// image, each row taking its leading part from one face and its tail from the
/// other.
struct ScanlineHandler {
    time_ms: u64,
}

impl ScanlineHandler {
    fn new(time_ms: u64) -> Self {
        Self { time_ms }
    }

    /// `StartProcess` (`0x100178d0`): the split is `width * elapsed / time` —
    /// **one** `__allmul`/`__aulldiv` pair, stored in the handler's own field
    /// and read as the split by `Process` (`0x100179d0`, both the even and the
    /// odd branch) and by `EndProcess` (`0x10017970`). The factory also keeps a
    /// separate `elapsed * 255 / time` progress, but `Process` never reads it,
    /// so the split must not be derived from it: truncating twice moves the
    /// boundary early for most of the clock. Past `time` the split is the whole
    /// width.
    fn split(&self, elapsed_ms: u64, width: usize) -> usize {
        if elapsed_ms >= self.time_ms {
            return width;
        }
        (width as u64 * elapsed_ms / self.time_ms) as usize
    }
}

impl TransitionHandler for ScanlineHandler {
    fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]) {
        let Some(source) = frame.source else {
            // A null `Src2` in the reference is a null scan-line provider; the
            // destination keeps the `Src1` copy it was seeded with.
            return;
        };
        let before = frame.dest_before;
        let width = before.width.min(source.width) as usize;
        let height = before.height.min(source.height) as usize;
        let split = self.split(frame.tick.as_millis() as u64, width);
        for row in 0..height {
            let destination = row * before.width as usize * 4;
            for column in 0..width {
                // Even rows lead with `Src2`'s right end, odd rows with
                // `Src1`'s; each row then continues with the other face from
                // its own left edge (`0x100179d0`).
                let (pixels, index) = if row % 2 == 0 {
                    if column < split {
                        (source, column + (width - split))
                    } else {
                        (before, column - split)
                    }
                } else if column < width - split {
                    (before, column + split)
                } else {
                    (source, column - (width - split))
                };
                let from = origin_of(index, row, pixels.width as usize);
                let Some(pixel) = pixels.pixels.get(from..from + 4) else {
                    continue;
                };
                if let Some(slot) =
                    dest.get_mut(destination + column * 4..destination + column * 4 + 4)
                {
                    slot.copy_from_slice(pixel);
                }
            }
        }
    }
}

/// The byte offset of pixel `(column, row)` in a tightly packed RGBA face.
fn origin_of(column: usize, row: usize, width: usize) -> usize {
    (row * width + column) * 4
}

// ----------------------------------------------------------- crossfade

/// The documented degrade: a plain crossfade of the two faces, which is the
/// composite the engine's interim projection produced for these names while
/// this module was a marker. The arithmetic is the reference's own fade lerp,
/// so a degraded provider rounds like the real ones do.
struct CrossfadeHandler;

impl TransitionHandler for CrossfadeHandler {
    fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]) {
        let Some(source) = frame.source else {
            return;
        };
        if source.pixels.len() == dest.len() {
            let progress = (f64::from(frame.progress) * 255.0)
                .round()
                .clamp(0.0, 255.0) as i64;
            if progress == 255 {
                dest.copy_from_slice(source.pixels);
                return;
            }
            if progress == 0 {
                return;
            }
            for index in 0..dest.len() / 4 {
                let origin = &source.pixels[index * 4..index * 4 + 4];
                let destination = &mut dest[index * 4..index * 4 + 4];
                let composed = fade_pixel(
                    [
                        destination[0],
                        destination[1],
                        destination[2],
                        destination[3],
                    ],
                    [origin[0], origin[1], origin[2], origin[3]],
                    [progress; 4],
                );
                destination.copy_from_slice(&composed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use krkr_core::{FrameInput, Size};
    use krkr_engine::plugin_api::transition::transition_provider_names;
    use krkr_engine::{
        EngineConfig, EngineInput, KrkrEngine, plugin_api::layer::layer_bitmap_read,
    };
    use krkr_tjs2::runtime::Variant;

    use super::*;

    /// Two 8×8 layers whose pixels encode their own coordinates — red carries
    /// `x`, green carries `y`, blue carries `255 - x` — so a composed pixel
    /// says which face and which position each of its channels came from.
    const LAYER_PAIR: &str = r#"
        global.dest = new Layer();
        dest.setImageSize(8, 8);
        dest.fillRect(0, 0, 8, 8, 0xff000000);
        global.source = new Layer();
        source.setImageSize(8, 8);
        source.fillRect(0, 0, 8, 8, 0xff000000);
        for (var y = 0; y < 8; y = y + 1) {
            for (var x = 0; x < 8; x = x + 1) {
                dest.fillRect(x, y, 1, 1, 0xff000000 | (x << 16) | (y << 8) | (255 - x));
                source.fillRect(x, y, 1, 1, 0xff000000 | (x << 16) | (y << 8) | (255 - x));
            }
        }
        dest.visible = true;
        source.visible = true;
    "#;

    /// The same pair, but at one flat colour per side, for the fade tests:
    /// `(100, 100, 100, 255)` in the destination, `(200, 200, 200, 255)` in
    /// the source.
    const FADE_PAIR: &str = r#"
        global.completed = 0;
        global.dest = new Layer();
        dest.setImageSize(8, 8);
        dest.fillRect(0, 0, 8, 8, 0xff646464);
        dest.onTransitionCompleted = function(d, s) { global.completed = 1; };
        dest.visible = true;
        global.source = new Layer();
        source.setImageSize(8, 8);
        source.fillRect(0, 0, 8, 8, 0xffc8c8c8);
        source.visible = true;
    "#;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ExtNaganoPlugin).expect("plugin");
        engine
    }

    fn run(engine: &mut KrkrEngine, script: &str) -> Variant {
        engine.execute_script("inline.tjs", script).expect("script")
    }

    fn update(engine: &mut KrkrEngine, delta: Duration) {
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                delta,
            )
            .expect("update");
    }

    fn layer(engine: &KrkrEngine, name: &str) -> krkr_tjs2::runtime::ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    /// `dest.onTransitionCompleted`'s flag: the script-visible "the transition
    /// stopped" signal (the host's own running-set predicate is
    /// engine-private).
    fn completed(engine: &mut KrkrEngine) -> bool {
        engine
            .execute_expression("probe.tjs", "completed")
            .expect("completed")
            == Variant::Integer(1)
    }

    /// Every pixel of a layer, read back through the plugin-facing view.
    fn pixels(engine: &mut KrkrEngine, name: &str) -> Vec<u8> {
        let handle = layer(engine, name);
        layer_bitmap_read(engine.tjs_runtime_mut(), handle, |view| {
            view.pixels.to_vec()
        })
        .expect("read layer")
    }

    fn pixel(pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
        let index = (y * 8 + x) * 4;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    }

    /// The snapshot a script's options object would produce, for the parser
    /// tests.
    fn options(entries: &[(&str, Variant)]) -> TransitionOptions {
        TransitionOptions::new(
            entries
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.clone())),
        )
    }

    fn request(options: TransitionOptions) -> TransitionRequest {
        TransitionRequest {
            options,
            dest_layer_type: 2,
            dest_size: (8, 8),
            source_size: Some((8, 8)),
        }
    }

    fn parse(name: &str, entries: &[(&str, Variant)]) -> ProviderOptions {
        parse_options(name, &request(options(entries))).expect("parse")
    }

    fn parse_error(name: &str, entries: &[(&str, Variant)]) -> String {
        parse_options(name, &request(options(entries)))
            .expect_err("a required option is missing")
            .message()
            .to_string()
    }

    fn integer(value: i64) -> Variant {
        Variant::Integer(value)
    }

    /// `V2Link` registers exactly the twelve names the DLL's `GetName` methods
    /// answer to, and `V2Unlink` takes them away again — the reference's
    /// provider-gated resolution.
    #[test]
    fn the_plugin_links_and_unlinks_its_twelve_names() {
        let mut engine = engine();
        let mut expected = PROVIDER_NAMES.map(str::to_string).to_vec();
        expected.sort();
        assert_eq!(transition_provider_names(engine.tjs_runtime()), expected);

        // The engine runs `register` twice (boot and the first
        // `Plugins.link`); the second pass hands back the same providers.
        engine
            .register_plugin(ExtNaganoPlugin)
            .expect("re-register");
        assert_eq!(transition_provider_names(engine.tjs_runtime()), expected);

        run(&mut engine, r#"Plugins.link("extNagano.dll");"#);
        assert_eq!(transition_provider_names(engine.tjs_runtime()), expected);

        run(&mut engine, r#"Plugins.unlink("extNagano.dll");"#);
        assert!(transition_provider_names(engine.tjs_runtime()).is_empty());

        run(&mut engine, LAYER_PAIR);
        let message = run(
            &mut engine,
            r#"
            var message = "";
            try { dest.beginTransition("rgbfade", true, source, %[time: 10]); }
            catch (e) { message = e.message; }
            return message;
            "#,
        );
        assert_eq!(
            message,
            Variant::String("Cannot find transition handler rgbfade".to_string()),
            "an unlinked provider's name is the official unknown-name error"
        );

        run(&mut engine, r#"Plugins.link("extNagano.dll");"#);
        engine
            .execute_script(
                "inline.tjs",
                r#"dest.beginTransition("rgbfade", true, source, %[time: 10]);"#,
            )
            .expect("the name answers again");
    }

    /// A provider name no longer resolves to the engine's crossfade kernel:
    /// the registry is consulted first, so the frame carries no kernel
    /// transition and the destination's bitmap is the handler's own composite.
    #[test]
    fn a_provider_transition_runs_the_handler_instead_of_a_kernel() {
        let mut engine = engine();
        run(&mut engine, LAYER_PAIR);
        run(
            &mut engine,
            r#"dest.beginTransition("scanline", true, source, %[time: 100]);"#,
        );
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(50),
            )
            .expect("frame");
        assert!(
            frame.output.transitions.is_empty(),
            "a provider transition composes CPU-side, not through a kernel"
        );
        // The composite reaches the renderer as the destination layer's own
        // upload, which is what a running provider transition is.
        let composed = pixels(&mut engine, "dest");
        assert!(
            frame
                .output
                .image_uploads
                .iter()
                .any(|upload| upload.rgba.as_ref() == composed.as_slice()),
            "the handler's composite is uploaded as the destination's image"
        );
    }

    /// The recovered `rgbfade` math, frame by frame: `delayR` 0, `delayG` 51,
    /// `delayB` 102, `delayA` 153 over a 1000 ms clock put the delays at 0, 200,
    /// 400 and 600 ms and the span at `(255 - 153) * 1000 / 255` = 400 ms, so
    /// each channel leads the next by 200 ms — and the reference's `>> 8` lerp
    /// tops out at 199, not 200, for a 100-unit difference.
    #[test]
    fn rgbfade_delays_each_channel_by_its_own_option() {
        let mut engine = engine();
        run(&mut engine, FADE_PAIR);
        run(
            &mut engine,
            r#"
            global.completed = 0;
            dest.onTransitionCompleted = function(d, s) { global.completed = 1; };
            dest.beginTransition("rgbfade", true, source,
                %[time: 1000, delayR: 0, delayG: 51, delayB: 102, delayA: 153]);
            "#,
        );
        assert_eq!(
            pixel(&pixels(&mut engine, "dest"), 0, 0),
            [100, 100, 100, 255],
            "the first pass is `Src1`: every progress is still zero"
        );

        for (frame, expected) in [
            (200u64, [149u8, 100, 100, 255]),
            (200, [199, 149, 100, 255]),
            (200, [199, 199, 149, 255]),
            (200, [199, 199, 199, 255]),
        ] {
            update(&mut engine, Duration::from_millis(frame));
            assert_eq!(
                pixel(&pixels(&mut engine, "dest"), 0, 0),
                expected,
                "after {frame} more ms"
            );
        }

        update(&mut engine, Duration::from_millis(200));
        assert!(completed(&mut engine), "the clock reached `time`");
        assert_eq!(
            pixels(&mut engine, "dest")[0..4],
            [199, 199, 199, 255],
            "the stop keeps the last composed pass; the core never calls `MakeFinalImage`"
        );
    }

    /// The recovered per-channel arithmetic, including the two degenerate
    /// faces and the `>> 8` top-out.
    #[test]
    fn the_fade_lerp_matches_the_recovered_byte_arithmetic() {
        assert_eq!(fade_channel(100, 200, 0), 100);
        assert_eq!(fade_channel(100, 200, 127), 149);
        assert_eq!(fade_channel(100, 200, 255), 199, "255 * 100 >> 8 is 99");
        assert_eq!(
            fade_channel(200, 100, 255),
            100,
            "the difference is signed: (100 - 200) * 255 >> 8 is -100"
        );

        let handler = RgbFadeHandler::new(1000, [0, 51, 102, 153]);
        assert_eq!(handler.delay_ms, [0, 200, 400, 600]);
        assert_eq!(handler.span_ms, 400);
        assert_eq!(handler.progress(0), [0, 0, 0, 0]);
        assert_eq!(handler.progress(200), [127, 0, 0, 0]);
        assert_eq!(handler.progress(400), [255, 127, 0, 0]);
        assert_eq!(handler.progress(2000), [255, 255, 255, 255]);

        // The clamp keeps an all-255 delay set from dividing by zero: the
        // span floors at the reference's `max(1, ...)`.
        let zeroed = RgbFadeHandler::new(100, [255, 255, 255, 255]);
        assert_eq!(zeroed.span_ms, 1);

        // The two degenerate faces `0x10017110` branches into, kept because the
        // reference takes them rather than blending: `imulb` keeps only the low
        // eight bits, so a transparent `Src2` quantises `Src1`.
        assert_eq!(
            fade_pixel([100, 100, 100, 255], [0, 0, 0, 0], [255; 4]),
            [156, 156, 156, 1],
            "a transparent Src2 multiplies by the progress byte: 100 * 255 truncates to 156"
        );
        assert_eq!(
            fade_pixel([100, 100, 100, 0], [200, 200, 200, 128], [255; 4]),
            [156, 156, 156, 127],
            "a transparent Src1 keeps the multiply and takes Src2's alpha"
        );
        assert_eq!(
            fade_pixel([100, 100, 100, 255], [200, 200, 200, 255], [127; 4]),
            [149, 149, 149, 255],
            "two opaque faces take the lerp"
        );
    }

    /// The reference keeps `time` in a 32-bit `tjs_int` and widens it only
    /// inside 64-bit multiplies; a script value past that range is bounded the
    /// same way, so the handler's products cannot overflow (the workspace's dev
    /// and test profiles check arithmetic, and an unbounded clock panicked
    /// inside `start_transition`).
    #[test]
    fn an_absurd_clock_is_bounded_like_the_reference_int32() {
        assert_eq!(
            require_time("rgbfade", &options(&[("time", integer(i64::MAX))])).expect("time"),
            i32::MAX as u64
        );
        assert_eq!(
            require_time(
                "rgbfade",
                &options(&[("time", integer(i64::from(i32::MAX) + 1))])
            )
            .expect("time"),
            i32::MAX as u64,
            "one past the reference's range saturates instead of overflowing"
        );
        assert!(require_time("rgbfade", &options(&[("time", integer(-1))])).is_err());
        assert!(require_time("rgbfade", &options(&[])).is_err());

        // The constructor's own products stay inside `i64` at the bound.
        let handler = RgbFadeHandler::new(i32::MAX as u64, [255; 4]);
        assert_eq!(handler.delay_ms, [i64::from(i32::MAX); 4]);
        assert_eq!(handler.span_ms, 1);
        assert_eq!(handler.progress(0), [0, 0, 0, 0]);
        assert_eq!(
            handler.progress(i64::from(i32::MAX) as u64 + 1000),
            [255, 255, 255, 255],
            "past the last channel's delay the span of 1 takes every channel to 255"
        );
    }

    /// The recovered `scanline` split and its alternating rows: at tick 50 of a
    /// 100 ms clock on an 8-wide image the split is `8 * 50 / 100` = 4, so even
    /// rows lead with `Src2`'s right end and odd rows with `Src1`'s.
    #[test]
    fn scanline_splits_each_row_against_the_alternating_face() {
        let mut engine = engine();
        // Two faces that can be told apart per position: `dest` carries `x` in
        // red and `255 - x` in blue, `source` the other way round, so a
        // composed pixel names its column in whichever face it came from.
        run(
            &mut engine,
            r#"
            global.dest = new Layer();
            dest.setImageSize(8, 8);
            global.source = new Layer();
            source.setImageSize(8, 8);
            for (var y = 0; y < 8; y = y + 1) {
                for (var x = 0; x < 8; x = x + 1) {
                    dest.fillRect(x, y, 1, 1, 0xff000000 | (x << 16) | (y << 8) | (255 - x));
                    source.fillRect(x, y, 1, 1, 0xff000000 | ((255 - x) << 16) | (y << 8) | x);
                }
            }
            dest.visible = true;
            source.visible = true;
            "#,
        );
        run(
            &mut engine,
            r#"
            global.completed = 0;
            dest.onTransitionCompleted = function(d, s) { global.completed = 1; };
            dest.beginTransition("scanline", true, source, %[time: 100]);
            "#,
        );
        update(&mut engine, Duration::from_millis(50));
        let composed = pixels(&mut engine, "dest");

        // The split is `8 * 50 / 100` = 4 (`0x100178d0` divides once, at full
        // precision; the handler's `elapsed * 255 / time` progress is never the
        // split).
        let from_dest =
            |column: usize, row: usize| [column as u8, row as u8, 255 - column as u8, 255];
        let from_source =
            |column: usize, row: usize| [255 - column as u8, row as u8, column as u8, 255];

        // Row 0 is even: its first `split` columns come from `Src2`'s right end
        // (`Src2[column + width - split]`) and the rest from `Src1[column -
        // split]`.
        for (column, source_column) in [(0usize, 4usize), (3, 7)] {
            assert_eq!(
                pixel(&composed, column, 0),
                from_source(source_column, 0),
                "even row {column} should come from `Src2` at {source_column}"
            );
        }
        for (column, dest_column) in [(4usize, 0usize), (7, 3)] {
            assert_eq!(
                pixel(&composed, column, 0),
                from_dest(dest_column, 0),
                "even row {column} should come from `Src1` at {dest_column}"
            );
        }
        // Row 1 is odd: its first `width - split` columns come from `Src1`'s
        // right end (`Src1[column + split]`), the rest from `Src2`'s left edge.
        for (column, dest_column) in [(0usize, 4usize), (3, 7)] {
            assert_eq!(
                pixel(&composed, column, 1),
                from_dest(dest_column, 1),
                "odd row {column} should come from `Src1` at {dest_column}"
            );
        }
        for (column, source_column) in [(4usize, 0usize), (7, 3)] {
            assert_eq!(
                pixel(&composed, column, 1),
                from_source(source_column, 1),
                "odd row {column} should come from `Src2` at {source_column}"
            );
        }

        // The stop keeps the last pass on screen — the reference's core never
        // calls `MakeFinalImage` (see the `plugin_api::transition` docs), so
        // the destination is not exchanged for `Src2`.
        let last_pass = pixels(&mut engine, "dest");
        update(&mut engine, Duration::from_millis(50));
        assert!(completed(&mut engine), "the clock reached `time`");
        assert_eq!(
            pixels(&mut engine, "dest"),
            last_pass,
            "the last composed pass stays on the destination"
        );

        let handler = ScanlineHandler::new(100);
        assert_eq!(handler.split(0, 8), 0);
        assert_eq!(handler.split(50, 8), 4);
        assert_eq!(handler.split(99, 8), 8 - 1);
        assert_eq!(handler.split(100, 8), 8);
        // One division, not two: `width * (elapsed * 255 / time) / 255` would
        // give 637 here where the reference gives 640, and the deviation is
        // systematic for most of the clock (up to 5 columns at 1280 wide).
        assert_eq!(ScanlineHandler::new(1000).split(500, 1280), 640);
        assert_eq!(ScanlineHandler::new(1000).split(1, 1280), 1);
    }

    /// An option the reference requires aborts `StartTransition`, and the
    /// script sees the official message-only handler error.
    #[test]
    fn a_missing_required_option_reports_the_official_handler_error() {
        let mut engine = engine();
        run(&mut engine, FADE_PAIR);
        let message = run(
            &mut engine,
            r#"
            var message = "";
            try { dest.beginTransition("rgbfade", true, source, %[]); }
            catch (e) { message = e.message; }
            return message;
            "#,
        );
        assert_eq!(
            message,
            Variant::String(
                "Transition handler error iTVPTransHandlerProvider::StartTransition failed"
                    .to_string()
            )
        );
        // A refused start leaves the layer alone: the next frame's pass never
        // runs, so the destination keeps `Src1`'s pixels.
        let before = pixels(&mut engine, "dest");
        update(&mut engine, Duration::from_millis(5));
        assert_eq!(pixels(&mut engine, "dest"), before);
        assert!(!completed(&mut engine));

        let message = run(
            &mut engine,
            r#"
            var message = "";
            try { dest.beginTransition("honeyturn", true, source, %[time: 10]); }
            catch (e) { message = e.message; }
            return message;
            "#,
        );
        assert_eq!(
            message,
            Variant::String(
                "Transition handler error iTVPTransHandlerProvider::StartTransition failed"
                    .to_string()
            ),
            "honeyturn's size/twist/order are required, as the factory's nested reads are"
        );
    }

    /// The same guard end to end: a script clock past the reference's 32-bit
    /// range starts the transition and composes a pass instead of panicking
    /// inside `start_transition` or `process`.
    #[test]
    fn an_absurd_script_clock_starts_and_composes() {
        let mut engine = engine();
        run(&mut engine, FADE_PAIR);
        run(
            &mut engine,
            r#"dest.beginTransition("rgbfade", true, source,
                %[time: 9223372036854775807, delayR: 255, delayG: 255, delayB: 255, delayA: 255]);"#,
        );
        assert_eq!(
            pixel(&pixels(&mut engine, "dest"), 0, 0),
            [100, 100, 100, 255],
            "the bounded clock keeps the handler's own arithmetic finite"
        );
        update(&mut engine, Duration::from_millis(10));
        assert_eq!(
            pixel(&pixels(&mut engine, "dest"), 0, 0),
            [100, 100, 100, 255],
            "the delay is the whole clock, so the pass still hands back `Src1`"
        );
    }

    /// The reference's `src1w == src2w && src1h == src2h` rule, enforced by
    /// every one of the twelve factories.
    #[test]
    fn a_source_of_another_size_is_refused() {
        let mut engine = engine();
        run(&mut engine, FADE_PAIR);
        run(
            &mut engine,
            r#"
            global.small = new Layer();
            small.setImageSize(4, 4);
            small.visible = true;
            "#,
        );
        let message = run(
            &mut engine,
            r#"
            var message = "";
            try { dest.beginTransition("scanline", true, small, %[time: 10]); }
            catch (e) { message = e.message; }
            return message;
            "#,
        );
        assert_eq!(
            message,
            Variant::String(
                "Transition handler error iTVPTransHandlerProvider::StartTransition failed"
                    .to_string()
            )
        );
    }

    /// Every provider's recovered vocabulary, parsed from the names the binary
    /// reads: the defaults a missing member keeps and the clamps a value gets.
    #[test]
    fn the_recovered_option_vocabulary_is_parsed_by_name() {
        assert_eq!(
            parse(
                "rgbfade",
                &[
                    ("delayR", integer(0)),
                    ("delayG", integer(51)),
                    ("delayB", integer(102))
                ]
            ),
            ProviderOptions::RgbFade {
                delay: [0, 51, 102, 0]
            },
            "an absent delayA keeps the reference's 0"
        );
        assert_eq!(
            parse(
                "rgbfade",
                &[("delayR", integer(-4)), ("delayA", integer(999))]
            ),
            ProviderOptions::RgbFade {
                delay: [0, 0, 0, 255]
            },
            "each delay is clamped to 0..=255"
        );
        assert_eq!(parse("scanline", &[]), ProviderOptions::Scanline);
        assert_eq!(
            parse("zoomfade", &[]),
            ProviderOptions::ZoomFade {
                zoom1: 100,
                zoom2: 200
            },
            "the factory pushes `$0x64` for `zoom1` and `$0xc8` for `zoom2`"
        );
        assert_eq!(
            parse("zoomfade", &[("zoom1", integer(50))]),
            ProviderOptions::ZoomFade {
                zoom1: 50,
                zoom2: 200
            }
        );
        assert_eq!(
            parse("spin", &[]),
            ProviderOptions::Spin { type1: 0, type2: 1 }
        );
        assert_eq!(
            parse("spin", &[("type1", integer(6)), ("type2", integer(7))]),
            ProviderOptions::Spin { type1: 6, type2: 7 }
        );
        assert_eq!(
            parse("blurfade", &[("blur1", integer(3))]),
            ProviderOptions::BlurFade {
                blur1: 3,
                blur1x: 3,
                blur1y: 3,
                blur2: 0,
                blur2x: 0,
                blur2y: 0,
                kind: 0,
                prerender: false,
                exponent: 1.0,
            },
            "the per-axis overrides keep `blur1`/`blur2` when they are absent, and `exponent` keeps the reader's 1.0"
        );
        assert_eq!(
            parse(
                "blurfade",
                &[
                    ("blur1", integer(3)),
                    ("blur1x", integer(5)),
                    ("blur2", integer(7)),
                    ("blur2y", integer(9)),
                    ("prerender", integer(1)),
                    ("exponent", Variant::Real(1.5)),
                ]
            ),
            ProviderOptions::BlurFade {
                blur1: 3,
                blur1x: 5,
                blur1y: 3,
                blur2: 7,
                blur2x: 7,
                blur2y: 9,
                kind: 0,
                prerender: true,
                exponent: 1.5,
            }
        );
        assert_eq!(
            parse(
                "flutter",
                &[
                    ("back", integer(1)),
                    ("slip", integer(99)),
                    ("alpha", integer(200)),
                ]
            ),
            ProviderOptions::Flutter {
                back: 1,
                slip: 8,
                alpha: 200
            },
            "`slip` (reader default 8) saturates at `min(width, height)`, `alpha` (255) is kept"
        );
        assert_eq!(
            parse("flutter", &[]),
            ProviderOptions::Flutter {
                back: 0,
                slip: 8,
                alpha: 255
            },
            "an empty object keeps the reader's own defaults"
        );
        assert_eq!(
            parse(
                "honeyturn",
                &[
                    ("size", integer(32)),
                    ("twist", integer(2)),
                    ("order", integer(1))
                ]
            ),
            ProviderOptions::HoneyTurn {
                size: 32,
                twist: 2,
                order: 1
            }
        );
        assert_eq!(
            parse(
                "multiripple",
                &[
                    ("count", integer(4)),
                    ("wavecount", integer(2)),
                    ("rwidth", integer(64)),
                    ("maxdrift", integer(12)),
                    ("roundness", Variant::Real(1.5)),
                    ("delaylast", Variant::Real(0.25)),
                ]
            ),
            ProviderOptions::MultiRipple {
                count: 4,
                wavecount: 2,
                rwidth: 64,
                maxdrift: 12,
                roundness: 1.5,
                delaylast: 0.25,
                callback: false,
            }
        );
        assert_eq!(
            parse("multiripple", &[]),
            ProviderOptions::MultiRipple {
                count: 6,
                wavecount: 2,
                rwidth: 16,
                maxdrift: 48,
                roundness: 1.0,
                delaylast: 1.0,
                callback: false,
            },
            "an empty object keeps the reader's own defaults, and `count` is clamped to 1..=20"
        );
        assert_eq!(
            parse("multiripple", &[("count", integer(99))]),
            ProviderOptions::MultiRipple {
                count: 20,
                wavecount: 2,
                rwidth: 16,
                maxdrift: 48,
                roundness: 1.0,
                delaylast: 1.0,
                callback: false,
            }
        );
        assert_eq!(
            parse("book", &[]),
            ProviderOptions::Book { dir: 0 },
            "an absent `dir` keeps the reader's 0, which the factory turns into the RL handler"
        );
        assert_eq!(
            parse("book", &[("dir", integer(1))]),
            ProviderOptions::Book { dir: 1 },
            "1 selects the LR handler"
        );
        assert_eq!(
            parse("book", &[("dir", integer(-1))]),
            ProviderOptions::Book { dir: -1 },
            "-1 is the sentinel the factory replaces with `rand() & 1`"
        );
        assert_eq!(
            parse("morphing", &[]),
            ProviderOptions::Morphing {
                before: false,
                after: false
            }
        );
    }

    /// The aliases `0x100026e0` reads into the same slots as their long
    /// spellings (`a1`/`s1`/`a2`/`s2`), the `HSB` string comparison, and the
    /// `rule` requirement.
    #[test]
    fn three_d_universal_reads_its_short_aliases_and_requires_its_rule() {
        let rule = Variant::String("rule.png".to_string());
        assert_eq!(
            parse(
                "3duniversal",
                &[
                    ("rule", rule.clone()),
                    ("type", Variant::String("HSB".to_string())),
                    ("type2", Variant::String("box".to_string())),
                    ("bound1", integer(4)),
                    ("bound2", integer(5)),
                    ("s1", Variant::Real(1.5)),
                    ("accel2", Variant::Real(2.5)),
                ]
            ),
            ProviderOptions::ThreeDUniversal {
                hsb: true,
                type2: Some("box".to_string()),
                bound1: 4,
                bound2: 5,
                speed1: Some(1.5),
                speed2: None,
                accel1: None,
                accel2: Some(2.5),
            },
            "`s1` fills the `speed1` slot and an absent `speed2` stays absent"
        );

        // The alias wins when both spellings are present, because the
        // reference reads it second into the same variable.
        assert_eq!(
            parse(
                "3duniversal",
                &[
                    ("rule", rule.clone()),
                    ("speed1", Variant::Real(1.0)),
                    ("s1", Variant::Real(2.0)),
                ]
            ),
            ProviderOptions::ThreeDUniversal {
                hsb: false,
                type2: None,
                bound1: 0,
                bound2: 0,
                speed1: Some(2.0),
                speed2: None,
                accel1: None,
                accel2: None,
            },
            "an absent `type` is not the `HSB` variant"
        );

        assert!(parse_error("3duniversal", &[]).contains("`rule`"));
        assert!(parse_error("imagewipe", &[("time", integer(10))]).contains("`rule`"));
        assert_eq!(
            parse("imagewipe", &[("rule", rule), ("dir", integer(1))]),
            ProviderOptions::ImageWipe {
                rule: "rule.png".to_string(),
                dir: 1
            }
        );
        assert!(
            parse_error("honeyturn", &[("size", integer(8)), ("twist", integer(1))])
                .contains("`order`")
        );
        assert_eq!(
            parse_error("nonsense", &[]),
            "extNagano has no provider named nonsense"
        );
    }

    /// Each provider whose pixels are not recovered composes the documented
    /// crossfade — the composite the interim projection produced — and the
    /// name still resolves to this plugin's handler rather than to a kernel.
    #[test]
    fn the_degraded_providers_compose_the_documented_crossfade() {
        for name in [
            "3duniversal",
            "blurfade",
            "book",
            "flutter",
            "honeyturn",
            "imagewipe",
            "morphing",
            "multiripple",
            "spin",
            "zoomfade",
        ] {
            let mut engine = engine();
            run(&mut engine, FADE_PAIR);
            // The options each provider's factory requires, so the degrade is
            // reached at all: `rule` for the two rule-driven ones,
            // size/twist/order for honeyturn.
            let options = match name {
                "3duniversal" | "imagewipe" => "%[time: 100, rule: \"rule.png\"]".to_string(),
                "honeyturn" => "%[time: 100, size: 8, twist: 1, order: 1]".to_string(),
                _ => "%[time: 100]".to_string(),
            };
            run(
                &mut engine,
                &format!(r#"dest.beginTransition("{name}", true, source, {options});"#),
            );
            let frame = engine
                .update(
                    EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                    Duration::from_millis(50),
                )
                .expect("frame");
            assert!(
                frame.output.transitions.is_empty(),
                "{name} runs this module's handler, not a kernel"
            );
            assert_eq!(
                pixel(&pixels(&mut engine, "dest"), 0, 0),
                [150, 150, 150, 255],
                "{name} composes the crossfade at half the clock: `frame.progress` is 0.5, \
                 its 0..=255 form rounds to 128, and 100 + (100 * 128 >> 8) is 150"
            );
        }
    }
}
