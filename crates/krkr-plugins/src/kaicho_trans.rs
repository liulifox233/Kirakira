//! `KaichoTrans.dll` — the `blur` and `dim` transition providers.
//!
//! `KaichoTrans` adds two transitions and nothing else: both are
//! `iTVPTransHandlerProvider`s registered from `V2Link` and removed from
//! `V2Unlink`. There is no TJS class, no global member and no alias, so this
//! module's whole surface is the two provider names `blur` and `dim`.
//!
//! # Source
//!
//! Version 0251 (2015-06-20); the upstream site is dead and the archived zip
//! is the survey's `/tmp/m55/src/kaicho/KaichoTrans-0251/`:
//!
//! | file | what it holds |
//! |---|---|
//! | `src/Main.cpp` | `V2Link` (`:20-34`): `TVPInitImportStub`, then `RegisterBlurTransHandlerProvider()` and `RegisterDimTransHandlerProvider()`; `V2Unlink` (`:37-50`) unregisters both and calls `TVPUninitImportStub` |
//! | `src/blur.cpp` | the `blur` provider (`:578-693`) and handler (`:41-225`): one integral image per source, a box blur whose radius ramps with the clock |
//! | `src/dim.cpp` | the `dim` provider (`:325-443`) and handler (`:37-152`): a box-blurred mono rule graphic through the universal-transition blend |
//! | `KaichoTrans.txt` | the manual (Shift-JIS): the option tables quoted below |
//!
//! The sources are Shift-JIS with CRLF; every line number below is the
//! original file's (`iconv -f CP932 -t UTF-8` preserves numbering).
//!
//! # The provider surface
//!
//! `GetName` returns the exact name (`blur.cpp:604-610`, `dim.cpp:356-362`) and
//! the lookup is case-sensitive (`plugin_api::transition`); `StartTransition`
//! answers `ttExchange` + `tutDivisible` and builds one
//! `iTVPDivisibleTransHandler` per playback (`blur.cpp:613-675`,
//! `dim.cpp:365-441`). Both providers refuse a source pair whose sizes differ
//! (`blur.cpp:630-631`, `dim.cpp:382-383` — the reference fails the call
//! instead of degrading it) and clamp `time` to at least 2 ms (`blur.cpp:646`,
//! `dim.cpp:393`). Every other option has a default; the tables are in
//! [`BlurOptions::parse`] and [`DimOptions::parse`], which follow the
//! reference's read order (it decides which of `blur1`/`blur1x` wins).
//!
//! Registration goes through [`plugin_api::transition`](krkr_engine::plugin_api::transition)
//! (M79), exactly as the reference goes through `TVPAddTransHandlerProvider`:
//! [`KaichoTransPlugin::register`] is the plugin's `V2Link` and
//! [`KaichoTransPlugin::unregister`] its `V2Unlink`. The engine runs `register`
//! twice (boot and the first `Plugins.link`), so both providers live in a
//! `OnceLock` and the same `Arc` is handed back every time.
//!
//! One seam note: the KAG `[trans]` tag is the engine's own whole-tree
//! projection and never reaches a provider (`kag_transition_spec` answers a
//! registered provider's name with `TransitionMethod::Crossfade`), so
//! `blur`/`dim` fire from `Layer.beginTransition("blur", …)` — the script path
//! — not from a tag.
//!
//! # The pass model
//!
//! The channel hands a handler the destination layer's bitmap (`Src1`,
//! `frame.dest_before`), the source layer's bitmap (`Src2`, `frame.source`,
//! re-read every pass) and a `dest` buffer that starts as a copy of `Src1`.
//! The reference works one `tTVPDivisibleData` rectangle at a time because
//! `tutDivisible` splits the screen; this channel has no region, so every pass
//! covers the whole bitmap — the same result, since both kernels are per-pixel
//! and a partition is just a coarser pass.
//!
//! Faces in this channel are R, G, B, A per pixel (`plugin_api::transition`),
//! while the reference's `tjs_uint32` pixel is `0xAARRGGBB`, i.e. B, G, R, A in
//! memory. Every blur kernel here is per channel with the same box and the
//! same divisor, so the lane order cancels; only the composites name channels,
//! and they name them directly.
//!
//! # `blur`
//!
//! Two integral images are built from the first pass's faces
//! (`blur.cpp:486-514`) and re-used for the whole transition; `dynamic=1`
//! rebuilds them every pass (`:239-240`, `:527-536`). Per output line the
//! current radius is a fraction of the maximum: `src1` grows from 0 to its
//! maximum and `src2` shrinks from its maximum to 0, both by integer division
//! of the clock (`:251-254`). The two blurred lines are then composited by the
//! constant-opacity blend that matches the layer type (`:559-568`):
//! `TVPConstAlphaBlend_SD_d` for `ltAlpha`, `_SD_a` for `ltAddAlpha`, `_SD`
//! otherwise.
//!
//! Reproduced quirks, each with a test:
//!
//! * `accel` is parsed and stored but **never read**: `setCurrentRatio`
//!   (`blur.cpp:87-101`), the only reader of `Accel`, has no call site in
//!   `blur.cpp`, so the ramp is linear whatever the option says (the manual at
//!   `KaichoTrans.txt:32-33` promises otherwise).
//! * Edge pixels are repeated (`addALineToIntegralImage32`, `:274-360`), so a
//!   box that hangs over the border still has `(2x+1)(2y+1)` samples.
//! * The integral image is `height + 2*yblur + 2` rows tall (`:74`) while the
//!   build writes `height + 2*yblur + 1` of them (`:500-513`), so its last row
//!   is never written. A draw at the *bottom* row with the blur at its maximum
//!   reads that row (`:549`): the reference reads whatever `_aligned_malloc`
//!   left there — a fresh block, i.e. zero — and its scalar path would mask the
//!   wrapped quotient (`:377`) while the shipped SSE2 path saturates it to 255
//!   (`:400-405`, `cvtps2dq` → `packssdw` → `packuswb`). The port keeps the row
//!   zero and saturates, which is the DLL's behaviour; see
//!   [`IntegralImage::draw_line`]. Nothing visible rides on it: the frame
//!   where a source's radius is at its maximum is the frame where that
//!   source's blend ratio is 0 (`:247`, `:251-254`) or 1/256.
//! * The divide is an integer divide (`:371-377`). The shipped SSE2 build
//!   multiplies by `(float)(1<<16)/sq` and shifts (`:381-383`), which can
//!   differ by one where `sq` does not divide the sum; the port keeps the
//!   scalar algorithm, which is the one the source documents.
//! * At `BlendRatio == 255` the blend is `src1 + (src2-src1)*255 >> 8`, i.e.
//!   *almost* `src2` (`:247-248`, `:264`); the reference relies on `Exchange`
//!   at the stop for the exact final image, and so does this port (the seam
//!   moves the layers' content when the clock reaches `time`).
//!
//! # `dim`
//!
//! `rule` names a graphic the reference loads through the engine's image
//! provider (`iTVPSimpleImageProvider::LoadImage(rulename, 8, 0x02ffffff,
//! src1w, src1h, &ruleimg)`, `dim.cpp:421`) — 8bpp grayscale, scaled to the
//! transition's size (`TransIntf.cpp:139-162`, `TVPLoadGraphic(...,
//! glmGrayscale)`). The plugin box-blurs that mono plane (`DoBoxBlur`,
//! `:585-636`), negates it when `neg` is present (`:427-429`, `:639-652`; the
//! reference tests the option, never its value), and then runs the
//! engine's universal transition: `Phase = CurRatio * (255 + Vague)` (`:175`),
//! the opacity table (`TVPInitUnivTransBlendTable`, `visual/tvpgl.c:2847-2866`)
//! and the per-pixel rule blend (`:267-320` → `TVPUnivTransBlend[_switch][_d|_a]`,
//! `visual/tvpgl.c:2881-3400`). `Vague >= 512` takes the non-switch blend
//! (`:268`), everything else the switch one with `src1lv = Phase` and
//! `src2lv = Phase - Vague` (`:292-293`).
//!
//! **Gap**: this build's provider channel has no image provider — a provider's
//! `start_transition` receives options, layer type and sizes only, a running
//! handler only the two bitmap faces, and neither can reach the project
//! storage (`KrkrHost::load_image_storage` is `pub(crate)`; a provider is a
//! `'static Arc` and cannot borrow the host). So [`load_rule_image`] cannot
//! fetch the graphic and the provider fails the way the reference fails a rule
//! it cannot load (`dim.cpp:422-423`). Everything *after* the load — the mono
//! box blur, the negation order, the phase table and both blend families — is
//! ported and unit-tested against hand-built bitmaps; wiring the load up is
//! one call once the channel carries an image provider. Reading the rule out
//! of a script object instead would invent surface the reference does not
//! have, so the failure is reported rather than substituted.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::transition::{
        TransitionFrame, TransitionHandler, TransitionHandlerError, TransitionHandlerProvider,
        TransitionOptions, TransitionRequest, register_transition_provider,
        unregister_transition_provider,
    },
};
use krkr_tjs2::{
    Result,
    runtime::{Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

/// Canonical catalog name ([`crate::catalog`]); the alias `kaichotrans.dll`
/// resolves to this module.
const PLUGIN_NAME: &str = "KaichoTrans.dll";

/// `GetName` (`blur.cpp:604-610`).
const BLUR_NAME: &str = "blur";
/// `GetName` (`dim.cpp:356-362`).
const DIM_NAME: &str = "dim";

/// `ltAlpha` (`drawable.h:20-51`) — the exact `LayerType ==` test that picks
/// the destination-alpha composite in both kernels.
const LT_ALPHA: i32 = 2;
/// `ltAddAlpha` (`drawable.h:20-51`, additive alpha / premultiplied).
const LT_ADD_ALPHA: i32 = 12;

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "blur / dim transitions",
    notes: "Real providers for both names: `blur` is complete (option surface, integral-image box blur, layer-type composites). `dim`'s option surface, mono rule box blur, negation, phase table and universal-transition blends are ported, but its `rule` graphic cannot be fetched — the provider channel carries no image provider — so starting `dim` reports the reference's rule-load failure.",
    install: |engine| engine.register_plugin(KaichoTransPlugin),
};

/// The plugin itself: `V2Link`'s two registrations (`Main.cpp:30-31`).
pub struct KaichoTransPlugin;

impl KrkrPlugin for KaichoTransPlugin {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        register_transition_provider(runtime, Arc::clone(blur_provider()))?;
        register_transition_provider(runtime, Arc::clone(dim_provider()))
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        unregister_transition_provider(runtime, BLUR_NAME);
        unregister_transition_provider(runtime, DIM_NAME);
        Ok(())
    }
}

/// `RegisterBlurTransHandlerProvider` (`blur.cpp:679-685`): one provider
/// object per transition, handed back unchanged by every `register`.
fn blur_provider() -> &'static Arc<dyn TransitionHandlerProvider> {
    static PROVIDER: OnceLock<Arc<dyn TransitionHandlerProvider>> = OnceLock::new();
    PROVIDER.get_or_init(|| Arc::new(BlurProvider))
}

/// `RegisterDimTransHandlerProvider` (`dim.cpp:655-661`).
fn dim_provider() -> &'static Arc<dyn TransitionHandlerProvider> {
    static PROVIDER: OnceLock<Arc<dyn TransitionHandlerProvider>> = OnceLock::new();
    PROVIDER.get_or_init(|| Arc::new(DimProvider))
}

// ---------------------------------------------------------------- options

/// One option's integer value, with the reference's void guard.
///
/// `blur.cpp:648-666` (and `dim.cpp:396-412`) reads each option as
/// `TJS_SUCCEEDED(options->GetValue(name, &tmp)) && tmp.Type() != tvtVoid`: a
/// member that is absent, or present but `void`, keeps the caller's default. A
/// member that cannot be converted is the C++ `(tjs_int)tmp` throwing
/// `TJSConvertError` out of `StartTransition`, so it fails the call here.
fn option_integer(
    options: &TransitionOptions,
    name: &str,
) -> std::result::Result<Option<i64>, TransitionHandlerError> {
    let Some(value) = options.value(name) else {
        return Ok(None);
    };
    if matches!(value, Variant::Void) {
        return Ok(None);
    }
    value.to_integer().map(Some).map_err(|error| {
        TransitionHandlerError::new(format!("option `{name}` cannot be read: {error}"))
    })
}

/// [`option_integer`]'s real-valued twin — the reference's `(double)tmp`
/// (`blur.cpp:662-663`, `dim.cpp:410-412`).
fn option_real(
    options: &TransitionOptions,
    name: &str,
) -> std::result::Result<Option<f64>, TransitionHandlerError> {
    let Some(value) = options.value(name) else {
        return Ok(None);
    };
    if matches!(value, Variant::Void) {
        return Ok(None);
    }
    value.to_real().map(Some).map_err(|error| {
        TransitionHandlerError::new(format!("option `{name}` cannot be read: {error}"))
    })
}

/// A value-shaped boolean: the reference's `((tjs_int)tmp != 0)`
/// (`blur.cpp:665-666`, `blur`'s `dynamic`). `dim`'s `neg` is *not* one of
/// these — it negates on presence ([`option_present`]).
fn option_flag(
    options: &TransitionOptions,
    name: &str,
) -> std::result::Result<Option<bool>, TransitionHandlerError> {
    Ok(option_integer(options, name)?.map(|value| value != 0))
}

/// Whether the option is present and not `void` — the same
/// `TJS_SUCCEEDED(GetValue(name, &tmp)) && tmp.Type() != tvtVoid` guard
/// [`option_integer`] applies, for an option the reference reads *without*
/// looking at its value (`dim.cpp:427-429`'s `neg`).
fn option_present(options: &TransitionOptions, name: &str) -> bool {
    options
        .value(name)
        .is_some_and(|value| !matches!(value, Variant::Void))
}

/// `GetAsString` (`TransIntf.cpp:78-98`): `None` when the member is absent,
/// `void` (`TJS_E_MEMBERNOTFOUND`, `:85`) or not convertible (the `catch` at
/// `:94` answers `TJS_E_FAIL`); every one of those is a failed read for the
/// caller at `dim.cpp:417-419`. A number becomes its string form, as the
/// reference's variant-to-`ttstr` assignment does.
fn option_string(options: &TransitionOptions, name: &str) -> Option<String> {
    let value = options.value(name)?;
    if matches!(value, Variant::Void) {
        return None;
    }
    value.to_tjs_string().ok()
}

/// The `time` option: required, and clamped to the reference's 2 ms floor
/// (`blur.cpp:642-646`, `dim.cpp:388-393`).
///
/// An absent or void `time` is `TJS_E_FAIL` in the reference; the reference's
/// own wording for the same condition in the built-in providers is
/// `Specify option time` (`TransIntf.cpp:773`; this engine's `classes.rs` uses
/// the twin `Specify option rule` for `universal`), which is what this port
/// reports to the host log — the script only ever sees the seam's
/// message-only `Transition handler error …`.
fn required_time(options: &TransitionOptions) -> std::result::Result<i64, TransitionHandlerError> {
    let time = option_integer(options, "time")?
        .ok_or_else(|| TransitionHandlerError::new("Specify option time"))?;
    Ok(time.max(2))
}

/// A blur radius: the reference stores `(tjs_int)tmp` into a `tjs_uint32`
/// (`blur.cpp:649-660`, `dim.cpp:400-408`) and then indexes the integral image
/// with it, so a negative radius is out-of-bounds heap traffic there. The port
/// clamps to 0 instead of reproducing that; nothing else about the value
/// changes.
fn radius(value: i64) -> u32 {
    value.max(0) as u32
}

// ---------------------------------------------------------------- `blur`

/// The `blur` option object (`blur.cpp:634-672`), in the reference's read
/// order — which is what decides the values:
///
/// | option | default / rule | reference |
/// |---|---|---|
/// | `time` | required, clamped to >= 2 | `:642-646` |
/// | `blur1x` / `blur1y` | 32 / 32 | `:648-651` |
/// | `blur1` | sets **both** axes, read *after* them, so it wins | `:652-653` |
/// | `blur2x` / `blur2y` | 32 / 32 | `:655-658` |
/// | `blur2` | sets **both** axes, read after them | `:659-660` |
/// | `accel` | 1.0 | `:662-663` — parsed, never read (see the module docs) |
/// | `dynamic` | false | `:665-666` |
#[derive(Clone, Copy, Debug, PartialEq)]
struct BlurOptions {
    time: i64,
    x_blur1: u32,
    y_blur1: u32,
    x_blur2: u32,
    y_blur2: u32,
    accel: f64,
    dynamic: bool,
}

impl BlurOptions {
    fn parse(options: &TransitionOptions) -> std::result::Result<Self, TransitionHandlerError> {
        let time = required_time(options)?;

        let mut x_blur1 = 32;
        let mut y_blur1 = 32;
        if let Some(value) = option_integer(options, "blur1x")? {
            x_blur1 = radius(value);
        }
        if let Some(value) = option_integer(options, "blur1y")? {
            y_blur1 = radius(value);
        }
        if let Some(value) = option_integer(options, "blur1")? {
            x_blur1 = radius(value);
            y_blur1 = x_blur1;
        }

        let mut x_blur2 = 32;
        let mut y_blur2 = 32;
        if let Some(value) = option_integer(options, "blur2x")? {
            x_blur2 = radius(value);
        }
        if let Some(value) = option_integer(options, "blur2y")? {
            y_blur2 = radius(value);
        }
        if let Some(value) = option_integer(options, "blur2")? {
            x_blur2 = radius(value);
            y_blur2 = x_blur2;
        }

        let accel = option_real(options, "accel")?.unwrap_or(1.0);
        let dynamic = option_flag(options, "dynamic")?.unwrap_or(false);

        Ok(Self {
            time,
            x_blur1,
            y_blur1,
            x_blur2,
            y_blur2,
            accel,
            dynamic,
        })
    }
}

/// The `blur` provider (`tTVPBlurTransHandlerProvider`, `blur.cpp:578-677`).
struct BlurProvider;

impl TransitionHandlerProvider for BlurProvider {
    fn name(&self) -> &str {
        BLUR_NAME
    }

    fn start_transition(
        &self,
        request: &TransitionRequest,
    ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError> {
        let options = BlurOptions::parse(&request.options)?;
        // `if(src1w != src2w || src1h != src2h) return TJS_E_FAIL`
        // (`blur.cpp:630-631`): an unequal pair fails the call, it does not
        // degrade. A call without a source layer arrives as `None` here and is
        // the same mismatch (`src2w`/`src2h` would be 0).
        if request.source_size != Some(request.dest_size) {
            return Err(TransitionHandlerError::new(
                "blur: the transition source and destination must be the same size",
            ));
        }
        let (width, height) = request.dest_size;
        Ok(Box::new(BlurHandler::new(
            options,
            request.dest_layer_type,
            width,
            height,
        )?))
    }
}

/// The per-playback handler (`tTVPBlurTransHandler`, `blur.cpp:41-225`).
struct BlurHandler {
    options: BlurOptions,
    layer_type: i32,
    width: u32,
    height: u32,
    /// `bdst1`/`bdst2` (`blur.cpp:84`, `:168-169`): one blurred line each.
    line1: Vec<u8>,
    line2: Vec<u8>,
    /// `Iimg1`/`Iimg2` (`:70-82`): built at the first pass, rebuilt on every
    /// pass while `dynamic` is set (`:239-240`, `:527-536`).
    integral1: IntegralImage,
    integral2: IntegralImage,
    /// `First` (`:51`, `:237-240`).
    first: bool,
    /// `StartTick` (`:53`, `:238`): the tick of the first pass.
    start_tick: Option<Duration>,
}

impl BlurHandler {
    fn new(
        options: BlurOptions,
        layer_type: i32,
        width: u32,
        height: u32,
    ) -> std::result::Result<Self, TransitionHandlerError> {
        let line_length = width as usize * 4;
        let mut line1 = Vec::new();
        let mut line2 = Vec::new();
        line1.try_reserve_exact(line_length).map_err(blur_alloc)?;
        line2.try_reserve_exact(line_length).map_err(blur_alloc)?;
        line1.resize(line_length, 0);
        line2.resize(line_length, 0);
        Ok(Self {
            options,
            layer_type,
            width,
            height,
            line1,
            line2,
            integral1: IntegralImage::new(width, height, options.x_blur1, options.y_blur1)?,
            integral2: IntegralImage::new(width, height, options.x_blur2, options.y_blur2)?,
            first: true,
            start_tick: None,
        })
    }

    /// `StartProcess`'s arithmetic (`blur.cpp:242-255`): the clock relative to
    /// the first pass, then the blend ratio and the two radii by integer
    /// division. `accel` takes no part — the reference never reads it here.
    fn phases(&self, cur_time: i64) -> BlurPhases {
        let time = self.options.time;
        let cur_time = cur_time.clamp(0, time);
        BlurPhases {
            blend_ratio: ((cur_time * 255 / time) as i32).min(255),
            x1: (self.options.x_blur1 as i64 * cur_time / time) as u32,
            y1: (self.options.y_blur1 as i64 * cur_time / time) as u32,
            x2: (self.options.x_blur2 as i64 * (time - cur_time) / time) as u32,
            y2: (self.options.y_blur2 as i64 * (time - cur_time) / time) as u32,
        }
    }
}

/// `CurXblur1`/`CurYblur1`/`CurXblur2`/`CurYblur2`/`BlendRatio` for one pass
/// (`blur.cpp:75-82`, `:247-254`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct BlurPhases {
    blend_ratio: i32,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
}

fn blur_alloc(_: std::collections::TryReserveError) -> TransitionHandlerError {
    TransitionHandlerError::new("blur: cannot allocate the transition buffers")
}

impl TransitionHandler for BlurHandler {
    fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]) {
        let (width, height) = (self.width, self.height);
        let row_bytes = width as usize * 4;
        let needed = row_bytes * height as usize;
        // The engine hands the destination layer's own bitmap; a source that
        // lost its image (or changed size) mid-transition is the reference's
        // null `Src2`, where the reference reads out of bounds. The port keeps
        // the frame the seam pre-filled (a copy of `Src1`) instead.
        let Some(source) = frame.source else { return };
        if source.width != width || source.height != height || source.pixels.len() < needed {
            return;
        }
        if dest.len() < needed || frame.dest_before.pixels.len() < needed {
            return;
        }

        let start = *self.start_tick.get_or_insert(frame.tick);
        let elapsed = frame.tick.saturating_sub(start).as_millis() as i64;
        let phases = self.phases(elapsed);

        // `if (First)` builds both integral images (`blur.cpp:527-536`);
        // `dynamic` re-arms the flag on every pass (`:239-240`).
        if self.first || self.options.dynamic {
            self.integral1.build(frame.dest_before.pixels);
            self.integral2.build(source.pixels);
            self.first = false;
        }

        for row in 0..height {
            let row_start = row as usize * row_bytes;
            self.integral1
                .draw_line(&mut self.line1, row as i64, phases.x1, phases.y1);
            self.integral2
                .draw_line(&mut self.line2, row as i64, phases.x2, phases.y2);
            composite_line(
                &mut dest[row_start..row_start + row_bytes],
                &self.line1,
                &self.line2,
                phases.blend_ratio,
                self.layer_type,
            );
        }
    }
}

/// One source's integral image (`blur.cpp:66-82`, `:138-170`, `:486-514`).
///
/// The reference stores four `tjs_uint32` lanes per cell; the port keeps the
/// same four-lane layout with the engine's channel order (R, G, B, A). Every
/// lane is summed against its own row and column headroom and divided by the
/// same `sq`, so the order cannot change a value.
struct IntegralImage {
    /// `Iimgwidth1` (`blur.cpp:73`): `width + 2*xblur + 1`.
    pitch: usize,
    /// `Iimgheight1` (`:74`): `height + 2*yblur + 2`.
    rows: usize,
    x_blur: u32,
    y_blur: u32,
    width: u32,
    height: u32,
    /// Cells in row-major order, four lanes each (`IPIXELCOLORNUM`, `:67`).
    lanes: Vec<u32>,
}

impl IntegralImage {
    fn new(
        width: u32,
        height: u32,
        x_blur: u32,
        y_blur: u32,
    ) -> std::result::Result<Self, TransitionHandlerError> {
        let pitch = width as usize + 2 * x_blur as usize + 1;
        let rows = height as usize + 2 * y_blur as usize + 2;
        let cells = pitch
            .checked_mul(rows)
            .and_then(|cells| cells.checked_mul(4))
            .ok_or_else(|| TransitionHandlerError::new("blur: the blur radius is too large"))?;
        let mut lanes = Vec::new();
        lanes.try_reserve_exact(cells).map_err(blur_alloc)?;
        lanes.resize(cells, 0);
        Ok(Self {
            pitch,
            rows,
            x_blur,
            y_blur,
            width,
            height,
            lanes,
        })
    }

    /// `buildIntegralImage32` (`blur.cpp:486-514`).
    ///
    /// Row 0 is zero (`:493-494`), then rows 1.. carry cumulative sums of the
    /// clamped scan lines in the reference's order: line 0 `yblur + 1` times
    /// (`:500-503`), lines 0..height-2 (`:504-508`), line height-1 `yblur`
    /// times (`:509-513`). That leaves the allocation's last row unwritten
    /// (`rows` is `height + 2*yblur + 2`); see [`Self::draw_line`].
    fn build(&mut self, pixels: &[u8]) {
        let width = self.width as usize;
        let height = self.height as i64;
        let row_bytes = width * 4;
        let line = |index: i64| -> &[u8] {
            let index = index.clamp(0, height - 1) as usize;
            &pixels[index * row_bytes..index * row_bytes + row_bytes]
        };
        let mut row = 1;
        for _ in -i64::from(self.y_blur) - 1..0 {
            self.add_line(row, line(0));
            row += 1;
        }
        for index in 0..height - 1 {
            self.add_line(row, line(index));
            row += 1;
        }
        for _ in height..height + i64::from(self.y_blur) {
            self.add_line(row, line(height - 1));
            row += 1;
        }
        // `Iimgheight1` (`blur.cpp:74`): the allocation is one row taller than
        // this build fills — the row `draw_line` reads at the peak radius.
        debug_assert_eq!(
            self.rows,
            self.height as usize + 2 * self.y_blur as usize + 2
        );
        debug_assert_eq!(row, self.rows - 1);
    }

    /// `addALineToIntegralImage32` (`blur.cpp:274-360`): one row of running
    /// sums, the row above added into every cell.
    ///
    /// The reference's three loops — the first pixel `xblur + 1` times, the
    /// line's own pixels, the last pixel `xblur + 1` times (`:280-308`) — are
    /// the increment `pixel[clamp(column - xblur - 1, 0, width - 1)]` for each
    /// of the `width + 2*xblur + 1` columns, which is what the SSE2 path
    /// (`:312-358`) accumulates as well. Four lanes, alpha included: the scalar
    /// variant's skip of the fourth lane (`:287`) would leave it uninitialised,
    /// and the shipped build is the SSE2 one.
    fn add_line(&mut self, row: usize, line: &[u8]) {
        let width = self.width as i64;
        let x_blur = i64::from(self.x_blur);
        let mut sums = [0u32; 4];
        for column in 0..width + 2 * x_blur + 1 {
            let source = (column - x_blur - 1).clamp(0, width - 1) as usize;
            let pixel = &line[source * 4..source * 4 + 4];
            let index = (row * self.pitch + column as usize) * 4;
            let previous = index - self.pitch * 4;
            for lane in 0..4 {
                sums[lane] = sums[lane].wrapping_add(u32::from(pixel[lane]));
                self.lanes[index + lane] = self.lanes[previous + lane].wrapping_add(sums[lane]);
            }
        }
    }

    /// `getIimg1Addr` (`blur.cpp:103-108`): image-relative coordinates, the
    /// buffer carrying `xblur + 1` columns and `yblur + 2` rows of headroom.
    fn cell(&self, x: i64, y: i64) -> usize {
        ((i64::from(self.y_blur) + 2 + y) as usize * self.pitch
            + (i64::from(self.x_blur) + 1 + x) as usize)
            * 4
    }

    /// `drawALineFromIntegralImageToImage32` (`blur.cpp:365-481`): one output
    /// line of the box blur for source row `source_y`.
    ///
    /// `src_u`/`src_d` (`:548-553`) are the rows above and at the bottom of the
    /// box; the per-lane sum is the reference's
    /// `src1[c] - src1[c + w] - src2[c] + src2[c + w]`, which is
    /// `Δ(src_d) - Δ(src_u)` — the box's `sq` samples. `w` is `(2 * xblur + 1)`
    /// cells (`:369`, `:380`).
    ///
    /// The quotient is saturated to 255 rather than masked: the reference's
    /// scalar path masks (`& 0xff`, `:377`) and its shipped SSE2 path saturates
    /// (`cvtps2dq` → `packssdw` → `packuswb`, `:400-405`). A legitimate sum is
    /// at most `255 * sq` over exactly `sq` samples, so 255 is never reached
    /// that way; the only sum that wraps is the one that reads the never
    /// written row above, and there the two paths disagree — the port follows
    /// the build the DLL ships. `draw_line` is also where that read happens
    /// (`:549`), which is why this comment is here and not in [`Self::build`].
    fn draw_line(&mut self, dest: &mut [u8], source_y: i64, x_blur: u32, y_blur: u32) {
        let sq = (2 * u64::from(x_blur) + 1) * (2 * u64::from(y_blur) + 1);
        let upper_y = source_y - i64::from(y_blur) - 1;
        let lower_y = source_y + i64::from(y_blur);
        let left_x = -i64::from(x_blur) - 1;
        let right = (2 * x_blur + 1) as usize * 4;
        for column in 0..self.width as i64 {
            let upper = self.cell(left_x + column, upper_y);
            let lower = self.cell(left_x + column, lower_y);
            for lane in 0..4 {
                let sum = self.lanes[upper + lane]
                    .wrapping_sub(self.lanes[upper + right + lane])
                    .wrapping_sub(self.lanes[lower + lane])
                    .wrapping_add(self.lanes[lower + right + lane]);
                dest[column as usize * 4 + lane] = (u64::from(sum) / sq).min(255) as u8;
            }
        }
    }
}

/// The composite `Process` finishes a line with (`blur.cpp:559-568`).
///
/// `TVPConstAlphaBlend_SD(_d|_a)` are `visual/gl/blend_function.cpp:290-294`
/// over the functors in `visual/gl/blend_functor_c.h`: the plain one lerps the
/// B/G/R lanes and leaves the alpha byte zero (`const_alpha_blend_functor`,
/// `:584-596` — its `0xff00ff`/`0xff00` masks name no alpha byte), `_d` lerps
/// the RGB lanes by the opacity-on-opacity table and the alpha byte by the
/// constant (`sd_const_alpha_blend_d_functor`, `:610-625`), and `_a` lerps all
/// four lanes (`blend_argb`, `blend_util_func.h:149-156`).
///
/// The branch is an exact `LayerType ==` test (`blur.cpp:559-568`), not the
/// `TVPIsTypeUsingAlpha` family `dim` uses (`drawable.h:55-80`).
fn composite_line(dest: &mut [u8], src1: &[u8], src2: &[u8], opa: i32, layer_type: i32) {
    match layer_type {
        LT_ALPHA => {
            // `opa_`/`iopa_` are the functor's adjusted constants
            // (`blend_functor_c.h:614`).
            let opa_ = if opa > 127 { opa + 1 } else { opa };
            let iopa = if opa > 127 { 255 - opa } else { 256 - opa };
            for pixel in 0..dest.len() / 4 {
                let base = pixel * 4;
                let a1 = i32::from(src1[base + 3]);
                let a2 = i32::from(src2[base + 3]);
                let addr = ((a2 * opa_) & 0xff00) + ((a1 * iopa) >> 8);
                let alpha = opacity_on_opacity_table(addr & 0xff, (addr >> 8) & 0xff) as i32;
                for channel in 0..3 {
                    dest[base + channel] =
                        lerp_byte(src1[base + channel], src2[base + channel], alpha);
                }
                dest[base + 3] = (a1 + (((a2 - a1) * opa_) >> 8)) as u8;
            }
        }
        LT_ADD_ALPHA => {
            for pixel in 0..dest.len() / 4 {
                let base = pixel * 4;
                for channel in 0..4 {
                    dest[base + channel] =
                        lerp_byte(src1[base + channel], src2[base + channel], opa);
                }
            }
        }
        _ => {
            for pixel in 0..dest.len() / 4 {
                let base = pixel * 4;
                for channel in 0..3 {
                    dest[base + channel] =
                        lerp_byte(src1[base + channel], src2[base + channel], opa);
                }
                dest[base + 3] = 0;
            }
        }
    }
}

/// One byte of `s1 + (s2 - s1) * opa >> 8` (`blend_functor_c.h:584-596`).
///
/// The reference computes it in 32-bit lanes with the wrapped-carry trick; the
/// `>> 8` is arithmetic (it floors a negative difference) and the byte addition
/// wraps, which is what the port's `as u8` reproduces. The result of a lerp
/// with `0 <= opa <= 256` lies between the two inputs, so no saturation
/// applies here.
fn lerp_byte(s1: u8, s2: u8, opa: i32) -> u8 {
    (i32::from(s1) + (((i32::from(s2) - i32::from(s1)) * opa) >> 8)) as u8
}

/// `TVPNegativeMulTable[source << 8 | destination]` (`visual/tvpgl.c:213-214`):
/// `255 - (255 - destination) * (255 - source) / 255`, the resulting opacity of
/// the same pair. `TVPUnivTransBlend_switch_d` reads it for the destination
/// alpha (`:3252-3253`); the non-switch `TVPUnivTransBlend_d` lerps instead
/// (`:3113`).
fn negative_mul_table(destination: i32, source: i32) -> u32 {
    let value = 255 - (255 - destination) * (255 - source) / 255;
    value.clamp(0, 255) as u32
}

/// `TVPOpacityOnOpacityTable[source << 8 | destination]`
/// (`visual/tvpgl.c:186-214`): the weight a source of opacity `source` gets
/// over a destination of opacity `destination`.
///
/// `sd_const_alpha_blend_d_functor` (`blend_functor_c.h:617`) and
/// `TVPUnivTransBlend_d` (`visual/tvpgl.c:3098`) build the index as
/// `(source_weight << 8) + destination_weight`, so the port takes the two
/// weights and evaluates the reference's float recipe on them. The reference
/// answers 255 whenever the destination side is 0 (the `if(a)` at
/// `visual/tvpgl.c:196-207`, whose `a` is the table index's low byte).
fn opacity_on_opacity_table(destination: i32, source: i32) -> u32 {
    if destination == 0 {
        return 255;
    }
    let at = destination as f32 / 255.0;
    let bt = source as f32 / 255.0;
    let mut c = bt / at;
    c /= 1.0 - bt + c;
    let ci = (c * 255.0) as i64;
    if ci >= 256 { 255 } else { ci.max(0) as u32 }
}

// ---------------------------------------------------------------- `dim`

/// The `dim` option object (`dim.cpp:385-429`), in the reference's read order:
///
/// | option | default / rule | reference |
/// |---|---|---|
/// | `time` | required, clamped to >= 2 | `:388-393` |
/// | `vague` | 64 | `:395-398` |
/// | `blur` | 16, **both axes**, read *before* them | `:399-402` |
/// | `xblur` / `yblur` | 16 / 16, each overrides `blur` | `:403-408` |
/// | `accel` | 1.0 | `:409-412` |
/// | `rule` | required — a failed `GetAsString` throws | `:414-419` |
/// | `neg` | **present ⇒ negate**, whatever the value | `:427-429` |
///
/// The `blur`/`xblur`/`blur1y` order is the mirror image of the `blur`
/// provider's `blur1`/`blur1x`/`blur1y`: there the whole-radius option is read
/// last and wins, here the per-axis options are. `neg` is a *presence* test:
/// `dim.cpp:427-429` guards only on `GetValue` succeeding and the value not
/// being `tvtVoid`, so `neg=0` and `neg="false"` negate too — unlike `blur`'s
/// `dynamic`, which reads `((tjs_int)tmp != 0)` (`blur.cpp:665-666`).
#[derive(Clone, Debug, PartialEq)]
struct DimOptions {
    time: i64,
    vague: i32,
    x_blur: u32,
    y_blur: u32,
    accel: f64,
    /// True when `neg` was present and not `void` — see the table above.
    neg: bool,
    rule: String,
}

impl DimOptions {
    fn parse(options: &TransitionOptions) -> std::result::Result<Self, TransitionHandlerError> {
        let time = required_time(options)?;

        let vague = option_integer(options, "vague")?.unwrap_or(64) as i32;

        let mut x_blur = 16;
        let mut y_blur = 16;
        if let Some(value) = option_integer(options, "blur")? {
            x_blur = radius(value);
            y_blur = x_blur;
        }
        if let Some(value) = option_integer(options, "xblur")? {
            x_blur = radius(value);
        }
        if let Some(value) = option_integer(options, "yblur")? {
            y_blur = radius(value);
        }

        let accel = option_real(options, "accel")?.unwrap_or(1.0);
        // `options->GetAsString(TJS_W("rule"), &rulename)` then
        // `TVPThrowExceptionMessage(TJS_W("オプション %1 を指定してください"),
        // TJS_W("rule"))` (`dim.cpp:417-419`) — the reference formats the
        // argument into the message, which is the text the port uses.
        let rule = option_string(options, "rule")
            .ok_or_else(|| TransitionHandlerError::new("オプション rule を指定してください"))?;
        let neg = option_present(options, "neg");

        Ok(Self {
            time,
            vague,
            x_blur,
            y_blur,
            accel,
            neg,
            rule,
        })
    }
}

/// The blurred, maybe-negated mono rule graphic (`Ruleimg`, `dim.cpp:57`,
/// `:421-429`).
///
/// The reference's rule is an 8bpp scanline provider scaled to the
/// transition's size; the port keeps the plane and its size.
#[derive(Clone, Debug, PartialEq)]
struct RuleImage {
    width: u32,
    height: u32,
    /// `width * height` grayscale samples, row-major, tightly packed.
    gray: Vec<u8>,
}

impl RuleImage {
    /// The value at `(x, y)`: the reference reads
    /// `Ruleimg->GetScanLine(data->Top + y)` and advances by `data->Left + x`
    /// (`dim.cpp:236`, `:265`), which for a whole-bitmap pass is the same
    /// coordinate in the rule.
    fn sample(&self, x: usize, y: usize) -> u8 {
        self.gray[y * self.width as usize + x]
    }
}

/// `imagepro->LoadImage(rulename, 8, 0x02ffffff, src1w, src1h, &ruleimg)`
/// (`dim.cpp:421`) — the storage named by `rule` as an 8bpp grayscale bitmap
/// scaled to the transition's size (`TransIntf.cpp:139-162`,
/// `TVPLoadGraphic(..., glmGrayscale)`).
///
/// **This build cannot perform it**: the M79 provider channel carries no image
/// provider (see the module docs), so the reference's failure path is the only
/// reachable one — `TVPThrowExceptionMessage(TJS_W("ルール画像 %1 を読み込む
/// ことができません"), rulename)` (`dim.cpp:422-423`). The message keeps the
/// reference's text and names the reason the port cannot do better.
fn load_rule_image(
    name: &str,
    _width: u32,
    _height: u32,
) -> std::result::Result<RuleImage, TransitionHandlerError> {
    Err(TransitionHandlerError::new(format!(
        "ルール画像 {name} を読み込むことができません (this engine's transition-provider channel carries no image provider)"
    )))
}

/// `tTVPDimTransHandlerProvider::DoBoxBlur` (`dim.cpp:585-636`): the rule's
/// box blur, in place.
///
/// The reference keeps a ring of `2*yblur + 2` cumulative rows of `u32`
/// (`iimgsiz`, `:592`) with a zero row at index 0 (`:597-599`), fills rows
/// 1..2yblur+1 from the clamped lines `-yblur..yblur` (`:600-610`), and slides
/// the window: draw the line from the row pair, overwrite the oldest row with
/// the newest line's running sum, advance the pointers around the ring
/// (`:617-634`). The port keeps that structure — including the reference's
/// unsigned subtraction and `& 0xff` (`:520`) — so a radius of 0 is the
/// identity and the edges repeat exactly as the colour kernel's do.
fn box_blur_gray(image: &mut RuleImage, x_blur: u32, y_blur: u32) {
    let (width, height) = (image.width as usize, image.height as usize);
    if width == 0 || height == 0 {
        return;
    }
    let pitch = width + 2 * x_blur as usize + 1;
    let rows = 2 * y_blur as usize + 2;
    let sq = (2 * u64::from(x_blur) + 1) * (2 * u64::from(y_blur) + 1);
    let mut integral = vec![0u32; pitch * rows];
    let mut out = vec![0u8; width * height];
    {
        let source = &image.gray;
        let line = |index: i64| -> &[u8] {
            let index = index.clamp(0, height as i64 - 1) as usize;
            &source[index * width..index * width + width]
        };
        for step in 0..2 * y_blur as i64 + 1 {
            add_gray_line(
                &mut integral,
                pitch,
                x_blur,
                1 + step as usize,
                step as usize,
                line(step - y_blur as i64),
            );
        }
        let mut first = 0usize;
        let mut second = 2 * y_blur as usize + 1;
        for y in 0..height {
            draw_gray_line(
                &mut out[y * width..y * width + width],
                &integral,
                pitch,
                first,
                second,
                x_blur,
                sq,
            );
            add_gray_line(
                &mut integral,
                pitch,
                x_blur,
                first,
                second,
                line((y + y_blur as usize + 1).min(height - 1) as i64),
            );
            second = first;
            first += 1;
            if first >= rows {
                first = 0;
            }
        }
    }
    image.gray = out;
}

/// `addALineToIntegralImage8` (`dim.cpp:447-511`): one row of running sums.
/// The same clamped-increment structure as [`IntegralImage::add_line`], one
/// lane wide.
fn add_gray_line(
    integral: &mut [u32],
    pitch: usize,
    x_blur: u32,
    row: usize,
    previous: usize,
    line: &[u8],
) {
    let width = line.len() as i64;
    let x_blur = i64::from(x_blur);
    let mut sum = 0u32;
    for column in 0..(width + 2 * x_blur + 1) as usize {
        let source = (column as i64 - x_blur - 1).clamp(0, width - 1) as usize;
        sum = sum.wrapping_add(u32::from(line[source]));
        integral[row * pitch + column] = integral[previous * pitch + column].wrapping_add(sum);
    }
}

/// `drawALineFromIntegralImageToImage8` (`dim.cpp:515-582`): one blurred line
/// out of the ring. The reference reads the pair as
/// `src1 -> iimgp1` and `src2 -> iimgp2` with the same
/// `src1[c] - src1[c + w] - src2[c] + src2[c + w]` shape as the colour kernel
/// and masks the quotient with `& 0xff` (`:520`).
fn draw_gray_line(
    dest: &mut [u8],
    integral: &[u32],
    pitch: usize,
    first: usize,
    second: usize,
    x_blur: u32,
    sq: u64,
) {
    let right = 2 * x_blur as usize + 1;
    for column in 0..dest.len() {
        let sum = integral[first * pitch + column]
            .wrapping_sub(integral[first * pitch + column + right])
            .wrapping_sub(integral[second * pitch + column])
            .wrapping_add(integral[second * pitch + column + right]);
        dest[column] = (u64::from(sum) / sq) as u8;
    }
}

/// `NegateGrayImage` (`dim.cpp:639-652`): `255 - v` in place, applied *after*
/// the blur (`dim.cpp:425-429`) — the two orders differ once the blur's integer
/// divide truncates.
fn negate_gray(image: &mut RuleImage) {
    for value in &mut image.gray {
        *value = 255 - *value;
    }
}

/// The `dim` provider (`tTVPDimTransHandlerProvider`, `dim.cpp:325-441`).
struct DimProvider;

impl TransitionHandlerProvider for DimProvider {
    fn name(&self) -> &str {
        DIM_NAME
    }

    fn start_transition(
        &self,
        request: &TransitionRequest,
    ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError> {
        let options = DimOptions::parse(&request.options)?;
        if request.source_size != Some(request.dest_size) {
            return Err(TransitionHandlerError::new(
                "dim: the transition source and destination must be the same size",
            ));
        }
        let (width, height) = request.dest_size;
        // `DoBoxBlur(ruleimg, xblur, yblur)` then, when `neg` is present,
        // `NegateGrayImage` (`dim.cpp:425-429`) — in that order.
        let mut rule = load_rule_image(&options.rule, width, height)?;
        box_blur_gray(&mut rule, options.x_blur, options.y_blur);
        if options.neg {
            negate_gray(&mut rule);
        }
        Ok(Box::new(DimHandler {
            options,
            layer_type: request.dest_layer_type,
            width,
            height,
            rule,
            table: [0; 256],
            start_tick: None,
        }))
    }
}

/// The per-playback handler (`tTVPDimTransHandler`, `dim.cpp:37-152`).
struct DimHandler {
    options: DimOptions,
    layer_type: i32,
    width: u32,
    height: u32,
    rule: RuleImage,
    /// `BlendTable[256]` (`dim.cpp:60`), refilled per pass by
    /// `TVPInitUnivTransBlendTable` (`:177-182`).
    table: [u32; 256],
    /// `StartTick` (`dim.cpp:48`, `:164-167`).
    start_tick: Option<Duration>,
}

impl DimHandler {
    /// `setCurrentRatio` (`dim.cpp:64-79`): the clock as a 0..1 ratio, bent by
    /// `accel` — above 1.0 the ratio is raised to that power (slow, then fast),
    /// below -1.0 it answers `1 - (1 - ratio)^|accel|` (fast, then slow), and
    /// anything in `-1.0..=1.0` is the identity, which is what the default
    /// does.
    fn current_ratio(&self, cur_time: i64) -> f64 {
        let time = self.options.time;
        let ratio = (cur_time.clamp(0, time) as f64 / time as f64).clamp(0.0, 1.0);
        let accel = self.options.accel;
        if accel > 1.0 {
            ratio.powf(accel)
        } else if accel < -1.0 {
            1.0 - (1.0 - ratio).powf(-accel)
        } else {
            ratio
        }
    }
}

impl TransitionHandler for DimHandler {
    fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]) {
        let (width, height) = (self.width, self.height);
        let row_bytes = width as usize * 4;
        let needed = row_bytes * height as usize;
        if dest.len() < needed || self.rule.width != width || self.rule.height != height {
            return;
        }
        let start = *self.start_tick.get_or_insert(frame.tick);
        let elapsed = frame.tick.saturating_sub(start).as_millis() as i64;
        let ratio = self.current_ratio(elapsed);

        // `Process` (`dim.cpp:199-221`): ratio 0 keeps `Src1` (the seam already
        // pre-filled `dest` with it), ratio 1 hands `Src2` through, anything
        // between runs the universal-transition blend.
        if ratio == 0.0 {
            return;
        }
        let Some(source) = frame.source else { return };
        if source.width != width || source.height != height || source.pixels.len() < needed {
            return;
        }
        if ratio >= 1.0 {
            dest[..needed].copy_from_slice(&source.pixels[..needed]);
            return;
        }

        // `StartProcess` (`dim.cpp:173-182`): `Phase`, then the table.
        let phase = (ratio * f64::from(255i32.saturating_add(self.options.vague))) as i32;
        self.table = univ_trans_table(phase, self.options.vague);

        // `Blend` (`dim.cpp:225-321`).
        let before = frame.dest_before.pixels;
        if before.len() < needed {
            return;
        }
        let vague = self.options.vague;
        let alpha_aware = uses_alpha(self.layer_type);
        let add_alpha = self.layer_type == LT_ADD_ALPHA;
        let family = if vague >= 512 {
            BlendFamily::Table
        } else {
            BlendFamily::Switch
        };
        for y in 0..height as usize {
            let row_start = y * row_bytes;
            for x in 0..width as usize {
                let base = row_start + x * 4;
                let rule = self.rule.sample(x, y);
                let src1 = [
                    before[base],
                    before[base + 1],
                    before[base + 2],
                    before[base + 3],
                ];
                let src2 = [
                    source.pixels[base],
                    source.pixels[base + 1],
                    source.pixels[base + 2],
                    source.pixels[base + 3],
                ];
                let blends = || {
                    blend_rule(
                        src1,
                        src2,
                        self.table[rule as usize] as i32,
                        alpha_aware,
                        add_alpha,
                        family,
                    )
                };
                let pixel = if family == BlendFamily::Table {
                    // `TVPUnivTransBlend[_d|_a]` (`dim.cpp:269-290`): the rule
                    // value only picks a table entry.
                    blends()
                } else if i32::from(rule) >= phase {
                    // `TVPUnivTransBlend_switch`: `>= src1lv` is `Src1`.
                    src1
                } else if i32::from(rule) < phase.saturating_sub(vague) {
                    // `< src2lv` is `Src2` (`dim.cpp:292-293`).
                    src2
                } else {
                    blends()
                };
                dest[base..base + 4].copy_from_slice(&pixel);
            }
        }
    }
}

/// `TVPInitUnivTransBlendTable` (`visual/tvpgl.c:2847-2866`): rule value `i`
/// maps to 255 below `phase - vague`, to 0 at and above `phase`, and to the
/// linear ramp `255 - ((i - (phase - vague)) * 255 / vague)` between them —
/// with the reference's truncating division and its two clamps.
///
/// `TVPInitUnivTransBlendTable_d`/`_a` are aliases of this function
/// (`:2867-2879`), so one table serves all three families. The `vague == 0`
/// case cannot reach the division (the two comparisons cover everything), and
/// the port keeps that shape instead of guarding it.
fn univ_trans_table(phase: i32, vague: i32) -> [u32; 256] {
    let mut table = [0u32; 256];
    let phasemax = phase;
    let phase = phase.saturating_sub(vague);
    for (index, entry) in table.iter_mut().enumerate() {
        let index = index as i32;
        let value = if index < phase {
            255
        } else if index >= phasemax {
            0
        } else {
            let ramp = (i64::from(index) - i64::from(phase)) * 255 / i64::from(vague);
            (255 - ramp).clamp(0, 255)
        };
        *entry = value as u32;
    }
    table
}

/// `TVPIsTypeUsingAlpha` (`drawable.h:55-75`): the layer types that carry a
/// straight-alpha channel — `ltAlpha` and the Photoshop blend family
/// (`ltPsNormal` = 13 ..= `ltPsExclusion` = 28, `drawable.h:20-51`).
fn uses_alpha(layer_type: i32) -> bool {
    layer_type == LT_ALPHA || (13..=28).contains(&layer_type)
}

/// Which `TVPUnivTransBlend` family a pass runs (`dim.cpp:268`): `Vague >= 512`
/// takes the non-switch functions, everything else the `_switch` ones — and the
/// two write *different* alpha lanes in the `_d` pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlendFamily {
    /// `TVPUnivTransBlend[_d|_a]` (`visual/tvpgl.c:2881-2954`, `:3087-3183`,
    /// `:3337-3361`).
    Table,
    /// `TVPUnivTransBlend_switch[_d|_a]` (`visual/tvpgl.c:2956-3335`).
    Switch,
}

/// One pixel of `TVPUnivTransBlend[_d|_a]`; `opa` is already `table[rule]`.
///
/// * `_d` weights the RGB lanes by the opacity-on-opacity table of the two
///   alphas. Its alpha lane is the one place the two families disagree: the
///   non-switch function writes the lerp
///   `a1 + ((a2 - a1)*opa >> 8)` (`visual/tvpgl.c:3113`), while the switch
///   function — the one `Vague < 512` uses, i.e. the default —
///   writes `TVPNegativeMulTable[addr]` over the same index
///   (`:3252-3253`, table at `:213-214`).
/// * `_a` (`:3337-3361`) is `TVPBlendARGB` — all four lanes lerped
///   (`blend_util_func.h:149-156`), the same in both families.
/// * the plain family (`:2881-2954`, `:2956-…`) lerps the B/G/R lanes and
///   leaves the alpha byte zero, the same mask shape as `blur`'s plain
///   composite.
fn blend_rule(
    before: [u8; 4],
    after: [u8; 4],
    opa: i32,
    alpha_aware: bool,
    add_alpha: bool,
    family: BlendFamily,
) -> [u8; 4] {
    if alpha_aware {
        let a1 = i32::from(before[3]);
        let a2 = i32::from(after[3]);
        let addr = ((a2 * opa) & 0xff00) + ((a1 * (256 - opa)) >> 8);
        let (destination, source) = (addr & 0xff, (addr >> 8) & 0xff);
        let alpha = opacity_on_opacity_table(destination, source) as i32;
        let alpha_lane = match family {
            BlendFamily::Switch => negative_mul_table(destination, source) as u8,
            BlendFamily::Table => (a1 + (((a2 - a1) * opa) >> 8)) as u8,
        };
        [
            lerp_byte(before[0], after[0], alpha),
            lerp_byte(before[1], after[1], alpha),
            lerp_byte(before[2], after[2], alpha),
            alpha_lane,
        ]
    } else if add_alpha {
        [
            lerp_byte(before[0], after[0], opa),
            lerp_byte(before[1], after[1], opa),
            lerp_byte(before[2], after[2], opa),
            lerp_byte(before[3], after[3], opa),
        ]
    } else {
        [
            lerp_byte(before[0], after[0], opa),
            lerp_byte(before[1], after[1], opa),
            lerp_byte(before[2], after[2], opa),
            0,
        ]
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use krkr_core::{FrameInput, Size};
    use krkr_engine::{
        EngineConfig, EngineInput, KrkrEngine,
        plugin_api::transition::{TransitionFace, transition_provider_names},
    };

    use super::*;

    fn options(entries: &[(&str, Variant)]) -> TransitionOptions {
        TransitionOptions::new(
            entries
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.clone())),
        )
    }

    fn face(pixels: &[u8], width: u32, height: u32) -> TransitionFace<'_> {
        TransitionFace {
            pixels,
            width,
            height,
        }
    }

    fn frame<'a>(
        tick: Duration,
        snapshot: &'a TransitionOptions,
        before: TransitionFace<'a>,
        source: TransitionFace<'a>,
    ) -> TransitionFrame<'a> {
        TransitionFrame {
            tick,
            progress: 0.0,
            options: snapshot,
            dest_before: before,
            source: Some(source),
        }
    }

    /// The reference's per-line blur over one face: the integral image is built
    /// at the *maximum* radius and drawn at the current one
    /// (`blur.cpp:527-554`).
    fn blur_face(pixels: &mut [u8], width: u32, height: u32, maximum: u32, current: u32) {
        let mut image =
            IntegralImage::new(width, height, maximum, maximum).expect("integral image");
        image.build(pixels);
        let mut line = vec![0u8; width as usize * 4];
        for row in 0..height as i64 {
            image.draw_line(&mut line, row, current, current);
            let start = row as usize * width as usize * 4;
            pixels[start..start + line.len()].copy_from_slice(&line);
        }
    }

    // ---- options -------------------------------------------------------

    #[test]
    fn the_blur_options_follow_the_reference_read_order() {
        // Only `time`: every radius defaults to 32 (`blur.cpp:637-639`).
        let parsed = BlurOptions::parse(&options(&[("time", Variant::Integer(200))]))
            .expect("defaults parse");
        assert_eq!(
            parsed,
            BlurOptions {
                time: 200,
                x_blur1: 32,
                y_blur1: 32,
                x_blur2: 32,
                y_blur2: 32,
                accel: 1.0,
                dynamic: false,
            }
        );

        // `blur1` is read *after* `blur1x`/`blur1y` (`blur.cpp:648-653`), so it
        // wins both axes; `blur2x` is then overridden by `blur2` (`:655-660`).
        let parsed = BlurOptions::parse(&options(&[
            ("time", Variant::Integer(200)),
            ("blur1x", Variant::Integer(5)),
            ("blur1y", Variant::Integer(6)),
            ("blur1", Variant::Integer(7)),
            ("blur2x", Variant::Integer(2)),
            ("blur2", Variant::Integer(4)),
            ("accel", Variant::Real(2.5)),
            ("dynamic", Variant::Integer(1)),
        ]))
        .expect("overrides parse");
        assert_eq!(
            parsed,
            BlurOptions {
                time: 200,
                x_blur1: 7,
                y_blur1: 7,
                x_blur2: 4,
                y_blur2: 4,
                accel: 2.5,
                dynamic: true,
            }
        );

        // A member that is present but `void` keeps the default
        // (`blur.cpp:648-651` behind `GetValue`'s `tvtVoid` guard).
        let parsed = BlurOptions::parse(&options(&[
            ("time", Variant::Integer(10)),
            ("blur1", Variant::Void),
            ("dynamic", Variant::Void),
        ]))
        .expect("void members parse");
        assert_eq!(
            (parsed.x_blur1, parsed.y_blur1, parsed.dynamic),
            (32, 32, false)
        );

        // `dynamic` is a *value* test here — `((tjs_int)tmp != 0)`
        // (`blur.cpp:665-666`) — unlike `dim`'s presence-driven `neg`.
        let parsed = BlurOptions::parse(&options(&[
            ("time", Variant::Integer(10)),
            ("dynamic", Variant::Integer(0)),
        ]))
        .expect("falsy dynamic parses");
        assert!(!parsed.dynamic);

        // `time` is required and clamped to the 2 ms floor (`:642-646`).
        assert_eq!(
            BlurOptions::parse(&options(&[]))
                .expect_err("time is required")
                .message(),
            "Specify option time"
        );
        assert_eq!(
            BlurOptions::parse(&options(&[("time", Variant::Integer(1))]))
                .expect("clamped")
                .time,
            2
        );
    }

    #[test]
    fn the_dim_options_follow_the_reference_read_order() {
        let rule = Variant::String("rule.png".to_string());
        let parsed = DimOptions::parse(&options(&[
            ("time", Variant::Integer(100)),
            ("rule", rule.clone()),
        ]))
        .expect("defaults parse");
        assert_eq!(
            parsed,
            DimOptions {
                time: 100,
                vague: 64,
                x_blur: 16,
                y_blur: 16,
                accel: 1.0,
                neg: false,
                rule: "rule.png".to_string(),
            }
        );

        // `xblur`/`yblur` are read *after* `blur` (`dim.cpp:399-408`), so they
        // override it — the opposite of `blur1`'s order in the other provider.
        let parsed = DimOptions::parse(&options(&[
            ("time", Variant::Integer(100)),
            ("rule", rule.clone()),
            ("blur", Variant::Integer(8)),
            ("xblur", Variant::Integer(3)),
            ("yblur", Variant::Integer(4)),
            ("neg", Variant::Integer(1)),
            ("vague", Variant::Integer(20)),
            ("accel", Variant::Real(-2.0)),
        ]))
        .expect("overrides parse");
        assert_eq!((parsed.x_blur, parsed.y_blur), (3, 4));
        assert_eq!(parsed.vague, 20);
        assert!(parsed.neg);
        assert_eq!(parsed.accel, -2.0);

        // `neg` negates on *presence*: `dim.cpp:427-429` guards only on the
        // read succeeding and the value not being `tvtVoid`, so `neg=0` and a
        // non-numeric string negate too.
        for value in [Variant::Integer(0), Variant::String("false".to_string())] {
            let parsed = DimOptions::parse(&options(&[
                ("time", Variant::Integer(10)),
                ("rule", rule.clone()),
                ("neg", value),
            ]))
            .expect("present neg parses");
            assert!(parsed.neg, "a present, non-void `neg` negates");
        }

        // `rule` is required (`dim.cpp:417-419`) — absent or void alike, and
        // the message formats the argument into the reference's format string.
        assert_eq!(
            DimOptions::parse(&options(&[("time", Variant::Integer(10))]))
                .expect_err("rule is required")
                .message(),
            "オプション rule を指定してください"
        );
        assert_eq!(
            DimOptions::parse(&options(&[
                ("time", Variant::Integer(10)),
                ("rule", Variant::Void),
            ]))
            .expect_err("a void rule is a failed GetAsString")
            .message(),
            "オプション rule を指定してください"
        );
        assert_eq!(
            DimOptions::parse(&options(&[("rule", rule)]))
                .expect_err("time is required")
                .message(),
            "Specify option time"
        );
    }

    // ---- blur kernel ---------------------------------------------------

    #[test]
    fn the_blur_box_repeats_edges_below_the_maximum_radius() {
        // R = 10, 20, 30 in one row, built at the maximum radius 2 and drawn at
        // 1: the box is the clamped 3x3 window, so the columns are
        // (10+10+20), (10+20+30), (20+30+30) and the vertical edge repeats the
        // only line three times (there is no line -1 or line 1). Divided by
        // sq = 9 with the reference's truncating divide (`blur.cpp:371-377`):
        // 40*3/9 = 13, 60*3/9 = 20, 80*3/9 = 26, and the alpha lane stays 255.
        let mut pixels = vec![10, 0, 0, 255, 20, 0, 0, 255, 30, 0, 0, 255];
        blur_face(&mut pixels, 3, 1, 2, 1);
        assert_eq!(pixels, vec![13, 0, 0, 255, 20, 0, 0, 255, 26, 0, 0, 255]);
    }

    #[test]
    fn a_peak_radius_draw_reads_the_never_written_row() {
        // One gray(100) pixel drawn at the maximum radius: the window's last
        // row is the allocation's extra row, which `buildIntegralImage32`
        // never writes (`blur.cpp:74` sizes `height + 2*yblur + 2` rows while
        // `:500-513` writes `height + 2*yblur + 1`). The subtraction wraps
        // (`:371`) and the shipped SSE2 divide chain — `cvtdq2ps`, `mulps`,
        // `cvtps2dq`, `psrld`, `packssdw`, `packuswb` (`:400-405`) — answers
        // 255 for it. Only this read can reach the saturation: a legitimate
        // sum is at most 255 * sq over sq samples.
        let mut pixels = vec![100, 100, 100, 255];
        blur_face(&mut pixels, 1, 1, 1, 1);
        assert_eq!(pixels, vec![255, 255, 255, 255]);
    }

    #[test]
    fn the_blur_ramp_is_the_reference_integer_division_of_the_clock() {
        // `CurXblur1 = MaxXblur1*CurTime/Time` and `CurXblur2 =
        // MaxXblur2*(Time-CurTime)/Time` (`blur.cpp:251-254`), `BlendRatio =
        // CurTime*255/Time` (`:247-248`) — all truncating, and the clock is
        // clamped to `Time` (`:243-244`).
        let options = BlurOptions {
            time: 7,
            x_blur1: 32,
            y_blur1: 5,
            x_blur2: 10,
            y_blur2: 64,
            accel: 1.0,
            dynamic: false,
        };
        let handler = BlurHandler::new(options, LT_ALPHA, 4, 4).expect("handler");
        assert_eq!(
            handler.phases(0),
            BlurPhases {
                blend_ratio: 0,
                x1: 0,
                y1: 0,
                x2: 10,
                y2: 64,
            }
        );
        assert_eq!(
            handler.phases(3),
            BlurPhases {
                blend_ratio: 109,
                x1: 13,
                y1: 2,
                x2: 5,
                y2: 36,
            }
        );
        assert_eq!(
            handler.phases(7),
            BlurPhases {
                blend_ratio: 255,
                x1: 32,
                y1: 5,
                x2: 0,
                y2: 0,
            }
        );
        assert_eq!(handler.phases(9), handler.phases(7));
    }

    #[test]
    fn accel_is_parsed_but_the_blur_kernel_never_reads_it() {
        // `blur.cpp` parses `accel` (`:662-663`) and stores it (`:151`), but
        // `setCurrentRatio` (`:87-101`) — the only reader — has no call site in
        // the file, so a script's `accel` changes nothing about the blur. Same
        // input, two handlers, byte-identical output — the manual at
        // `KaichoTrans.txt:32-33` promises otherwise.
        let run = |accel: f64| {
            let blur_options = BlurOptions {
                time: 4,
                x_blur1: 1,
                y_blur1: 1,
                x_blur2: 1,
                y_blur2: 1,
                accel,
                dynamic: false,
            };
            let mut handler = BlurHandler::new(blur_options, LT_ADD_ALPHA, 3, 1).expect("handler");
            let before = vec![10, 0, 0, 255, 20, 0, 0, 255, 30, 0, 0, 255];
            let source = vec![30, 0, 0, 255, 20, 0, 0, 255, 10, 0, 0, 255];
            let snapshot = options(&[("time", Variant::Integer(4))]);
            let mut dest = before.clone();
            handler.process(
                frame(
                    Duration::from_millis(2),
                    &snapshot,
                    face(&before, 3, 1),
                    face(&source, 3, 1),
                ),
                &mut dest,
            );
            dest
        };
        assert_eq!(run(1.0), run(8.0));
        assert_eq!(
            BlurOptions::parse(&options(&[
                ("time", Variant::Integer(4)),
                ("accel", Variant::Real(8.0)),
            ]))
            .expect("parse")
            .accel,
            8.0
        );
    }

    // ---- dim kernel ----------------------------------------------------

    #[test]
    fn the_rule_box_blur_repeats_edges_and_negates_after_it() {
        // The same clamped box as the colour kernel, one plane
        // (`dim.cpp:585-636`): radius 1 over [10, 20, 30] is the clamped
        // horizontal mean, 40/3 = 13, 60/3 = 20, 80/3 = 26.
        let mut rule = RuleImage {
            width: 3,
            height: 1,
            gray: vec![10, 20, 30],
        };
        box_blur_gray(&mut rule, 1, 1);
        assert_eq!(rule.gray, vec![13, 20, 26]);

        // `neg` negates *after* the blur (`dim.cpp:425-429`), and the two
        // orders differ once the divide truncates: blur([1, 0, 0]) with a
        // horizontal radius of 1 is [2/3, 1/3, 0/3] = [0, 0, 0], negated
        // [255, 255, 255]; negating first gives [254, 255, 255], which blurs to
        // [254, 254, 254].
        let mut rule = RuleImage {
            width: 3,
            height: 1,
            gray: vec![1, 0, 0],
        };
        box_blur_gray(&mut rule, 1, 0);
        negate_gray(&mut rule);
        assert_eq!(rule.gray, vec![255, 255, 255]);

        // A radius of 0 is the identity — `blur=0` / `xblur=0` in the manual
        // (`KaichoTrans.txt:82-88`).
        let mut rule = RuleImage {
            width: 2,
            height: 1,
            gray: vec![7, 9],
        };
        box_blur_gray(&mut rule, 0, 0);
        assert_eq!(rule.gray, vec![7, 9]);
    }

    #[test]
    fn the_universal_transition_table_ramps_between_vague_and_phase() {
        // `TVPInitUnivTransBlendTable` (`visual/tvpgl.c:2847-2866`) with
        // phase = 128 and vague = 128: 255 below `phase - vague` = 0, then
        // `255 - i*255/128` up to 128, then 0.
        let table = univ_trans_table(128, 128);
        assert_eq!(table[0], 255);
        assert_eq!(table[64], 128); // 255 - 64*255/128 = 255 - 127
        assert_eq!(table[127], 2); // 255 - 127*255/128 = 255 - 253
        assert_eq!(table[128], 0);
        assert_eq!(table[255], 0);

        // `vague == 0` is a step at `phase`; the reference's division sits in
        // the branch the two comparisons exclude there.
        let table = univ_trans_table(100, 0);
        assert_eq!(
            (table[0], table[99], table[100], table[255]),
            (255, 255, 0, 0)
        );
    }

    #[test]
    fn the_opacity_on_opacity_table_matches_the_reference_recipe() {
        // `TVPOpacityOnOpacityTable[source << 8 | destination]`
        // (`visual/tvpgl.c:186-214`): a zero destination answers 255, equal
        // 255s answer 255, the 127/127 pair answers
        // `c/(1 - c + c)` scaled by 255, and a zero source answers 0.
        assert_eq!(opacity_on_opacity_table(0, 128), 255);
        assert_eq!(opacity_on_opacity_table(255, 255), 255);
        assert_eq!(opacity_on_opacity_table(127, 127), 169);
        assert_eq!(opacity_on_opacity_table(255, 0), 0);
    }

    #[test]
    fn dim_switches_at_the_phase_edges_and_blends_the_midpoint() {
        // A 3-pixel rule row [255, 127, 0] with no blur and no negation; a
        // 1000 ms transition read at 500 ms gives `CurRatio = 0.5`, so
        // `Phase = 0.5 * (255 + 128) = 191` (`dim.cpp:175`) with
        // `Vague = 128` — `src1lv = 191`, `src2lv = 63` (`:292-293`). The
        // destination layer is `ltOpaque`, so the plain
        // `TVPUnivTransBlend_switch` (`:313-315`) runs: rule 255 is `Src1`,
        // rule 0 is `Src2`, and rule 127 lands on the table's ramp where
        // `phasemax = 191`, `phase = 63` and
        // `255 - (127 - 63)*255/128` = 255 - 127 = 128.
        let rule = RuleImage {
            width: 3,
            height: 1,
            gray: vec![255, 127, 0],
        };
        let snapshot = options(&[]);
        let before = vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
        let source = vec![255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255];
        let mut handler = DimHandler {
            options: DimOptions {
                time: 1000,
                vague: 128,
                x_blur: 0,
                y_blur: 0,
                accel: 1.0,
                neg: false,
                rule: "rule".to_string(),
            },
            layer_type: 1,
            width: 3,
            height: 1,
            rule,
            table: [0; 256],
            start_tick: None,
        };

        // Ratio 0 keeps `Src1` — the seam's pre-filled copy, so `dest` is left
        // alone (`dim.cpp:208-211`).
        let mut dest = before.clone();
        handler.process(
            frame(
                Duration::ZERO,
                &snapshot,
                face(&before, 3, 1),
                face(&source, 3, 1),
            ),
            &mut dest,
        );
        assert_eq!(dest, before);

        // Half way: `Src1`, the midpoint `0 + (255-0)*128 >> 8 = 127`, `Src2`.
        let mut dest = before.clone();
        handler.process(
            frame(
                Duration::from_millis(500),
                &snapshot,
                face(&before, 3, 1),
                face(&source, 3, 1),
            ),
            &mut dest,
        );
        assert_eq!(
            dest,
            vec![0, 0, 0, 255, 127, 127, 127, 0, 255, 255, 255, 255]
        );

        // Ratio 1 hands `Src2` through (`dim.cpp:212-215`).
        let mut dest = before.clone();
        handler.process(
            frame(
                Duration::from_millis(1000),
                &snapshot,
                face(&before, 3, 1),
                face(&source, 3, 1),
            ),
            &mut dest,
        );
        assert_eq!(dest, source);
    }

    /// One `dim` pass at half the clock over a single rule pixel of 127, on the
    /// default `ltAlpha` layer: `table[127]` is 128 in both families and the
    /// opacity-on-opacity weight is 169, so the RGB lanes answer 168 either way.
    /// Only the alpha lane differs, which is what the two tests below pin.
    fn alpha_lane_pass(vague: i32) -> Vec<u8> {
        let rule = RuleImage {
            width: 1,
            height: 1,
            gray: vec![127],
        };
        let snapshot = options(&[]);
        let before = vec![0, 0, 0, 255];
        let source = vec![255, 255, 255, 255];
        let mut handler = DimHandler {
            options: DimOptions {
                time: 1000,
                vague,
                x_blur: 0,
                y_blur: 0,
                accel: 1.0,
                neg: false,
                rule: "rule".to_string(),
            },
            layer_type: LT_ALPHA,
            width: 1,
            height: 1,
            rule,
            table: [0; 256],
            // The engine's start pass runs at tick zero, so the handler's
            // `StartTick` (`dim.cpp:48`) is set before a real tick arrives.
            start_tick: Some(Duration::ZERO),
        };
        let mut dest = before.clone();
        handler.process(
            frame(
                Duration::from_millis(500),
                &snapshot,
                face(&before, 1, 1),
                face(&source, 1, 1),
            ),
            &mut dest,
        );
        dest
    }

    #[test]
    fn the_switch_family_writes_the_negative_mul_alpha_lane() {
        // `Vague < 512` takes `TVPUnivTransBlend_switch_d` (`dim.cpp:295-299` —
        // the default `vague = 64`), whose alpha lane is
        // `TVPNegativeMulTable[addr] << 24` (`visual/tvpgl.c:3252-3253`), *not*
        // the non-switch family's lerp (`:3113`). With `Phase = 0.5 * 383 = 191`
        // and `table[127] = 128`, two alphas of 255 give the index `0x7f7f`, so
        // the lane is `255 - 128*128/255 = 191` while the RGB lanes take the
        // opacity-on-opacity weight 169 and answer `(255*169) >> 8 = 168`.
        assert_eq!(alpha_lane_pass(128), vec![168, 168, 168, 191]);
    }

    #[test]
    fn the_non_switch_family_lerps_the_alpha_lane() {
        // `Vague >= 512` takes the non-switch `TVPUnivTransBlend_d`
        // (`dim.cpp:268-290`), whose alpha lane is the lerp
        // `a1 + (a2 - a1) * opa >> 8` (`visual/tvpgl.c:3113`) — 255 for two
        // opaque faces. `Phase = 0.5 * (255 + 512) = 383` still leaves
        // `table[127] = 255 - (127 + 129) * 255 / 512 = 128`, so the RGB lanes
        // stay at 168 and only the alpha lane moves.
        assert_eq!(alpha_lane_pass(512), vec![168, 168, 168, 255]);
    }

    // ---- engine --------------------------------------------------------

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(KaichoTransPlugin).expect("plugin");
        engine
    }

    fn frame_input() -> EngineInput {
        EngineInput::new(FrameInput::new(Size::new(64.0, 64.0), 0.0), Vec::new())
    }

    fn pixel(engine: &mut KrkrEngine, expression: &str) -> Variant {
        engine
            .execute_expression("pixel.tjs", expression)
            .expect("expression")
    }

    /// A 4x4 red destination and a 4x4 blue source; `dest.type` selects the
    /// composite branch.
    fn layers(layer_type: i32) -> String {
        format!(
            r#"
            global.dest = new Layer();
            dest.setImageSize(4, 4);
            dest.fillRect(0, 0, 4, 4, 0xffff0000);
            dest.type = {layer_type};
            dest.visible = true;
            global.source = new Layer();
            source.setImageSize(4, 4);
            source.fillRect(0, 0, 4, 4, 0xff0000ff);
            source.visible = true;
            "#
        )
    }

    #[test]
    fn the_plugin_registers_both_names_and_unlinks_them() {
        let mut engine = engine();
        assert_eq!(
            transition_provider_names(engine.tjs_runtime()),
            vec![BLUR_NAME.to_string(), DIM_NAME.to_string()]
        );
        // `Plugins.link` runs `register` again (the same `Arc`, a no-op for the
        // registry) and `Plugins.unlink` is `V2Unlink`.
        engine
            .execute_script("link.tjs", r#"Plugins.link("KaichoTrans.dll");"#)
            .expect("link");
        assert_eq!(
            transition_provider_names(engine.tjs_runtime()),
            vec![BLUR_NAME.to_string(), DIM_NAME.to_string()]
        );
        engine
            .execute_script("link.tjs", r#"Plugins.unlink("KaichoTrans.dll");"#)
            .expect("unlink");
        assert!(transition_provider_names(engine.tjs_runtime()).is_empty());
    }

    #[test]
    fn a_script_blur_transition_runs_the_provider_handler() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    {}
                    global.completed = 0;
                    dest.onTransitionCompleted = function(d, s) {{ global.completed = 1; }};
                    dest.beginTransition("blur", true, source, %[time: 4, blur1: 1, blur2: 1]);
                    "#,
                    layers(1)
                ),
            )
            .expect("begin transition");

        // The first pass runs at tick 0, where `BlendRatio = 0` and the
        // destination still leads (`blur.cpp:247-254`).
        assert_eq!(
            pixel(&mut engine, "dest.getMainPixel(0, 0)"),
            Variant::Integer(0xff0000)
        );

        let frame = engine
            .update(frame_input(), Duration::from_millis(2))
            .expect("frame");
        assert!(
            frame.output.transitions.is_empty(),
            "a provider transition composes CPU-side, not through a kernel"
        );
        // At 2 of 4 ms the blend ratio is 2*255/4 = 127 (`blur.cpp:247`) and
        // both faces are one flat colour, so the radii (1*2/4 = 0) do not
        // matter and the plain `TVPConstAlphaBlend_SD` (the destination is
        // `ltOpaque`, `blur.cpp:565-567`) lerps per byte
        // (`blend_functor_c.h:584-596`): red 255 + (0-255)*127 >> 8 = 128, blue
        // (255-0)*127 >> 8 = 126.
        assert_eq!(
            pixel(&mut engine, "dest.getMainPixel(0, 0)"),
            Variant::Integer(0x80007e)
        );

        // The clock reaches `time` and the stop fires the completion event.
        // (The stop's `Exchange` does not move the bitmaps of two *unattached*
        // layers — the kernel path behaves the same — so the exchange is the
        // engine's own machinery and is covered by M1/M79, not here.)
        engine
            .update(frame_input(), Duration::from_millis(4))
            .expect("frame");
        assert_eq!(pixel(&mut engine, "completed"), Variant::Integer(1));
    }

    #[test]
    fn the_alpha_layer_composites_through_the_opacity_table() {
        // The default `Layer.type` is `ltAlpha`, which takes the `SD_d`
        // composite (`blur.cpp:559-562`). At the same 127 blend ratio the
        // functor's adjusted constants are `opa_ = 127`, `iopa_ = 129`
        // (`blend_functor_c.h:614`), so `addr` is
        // `(255*127 & 0xff00) + (255*129 >> 8)` = the 127/128 weight pair, the
        // table answers 168 (`visual/tvpgl.c:186-214`) and the RGB lanes lerp
        // by that: red 255 - 168 = 87, blue 168 - 1 = 167.
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    {}
                    dest.beginTransition("blur", true, source, %[time: 4, blur1: 1, blur2: 1]);
                    "#,
                    layers(2)
                ),
            )
            .expect("begin transition");
        engine
            .update(frame_input(), Duration::from_millis(2))
            .expect("frame");
        assert_eq!(
            pixel(&mut engine, "dest.getMainPixel(0, 0)"),
            Variant::Integer(0x5700a7)
        );
    }

    #[test]
    fn an_unequal_source_fails_the_blur_call_like_the_reference() {
        // `if(src1w != src2w || src1h != src2h) return TJS_E_FAIL`
        // (`blur.cpp:630-631`): the failure is the seam's message-only
        // `TVPTransHandlerError`, and the provider's own reason reaches the
        // host log.
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.dest = new Layer();
                dest.setImageSize(4, 4);
                dest.fillRect(0, 0, 4, 4, 0xffff0000);
                dest.visible = true;
                global.source = new Layer();
                source.setImageSize(8, 4);
                source.fillRect(0, 0, 8, 4, 0xff0000ff);
                source.visible = true;
                global.message = "";
                try { dest.beginTransition("blur", true, source, %[time: 10]); }
                catch (e) { message = e.message; }
                "#,
            )
            .expect("script");
        assert_eq!(
            pixel(&mut engine, "message"),
            Variant::String(
                "Transition handler error iTVPTransHandlerProvider::StartTransition failed"
                    .to_string()
            )
        );
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("must be the same size")),
            "the provider's own reason is logged"
        );
        assert_eq!(
            pixel(&mut engine, "dest.getMainPixel(0, 0)"),
            Variant::Integer(0xff0000)
        );
    }

    #[test]
    fn dim_reports_the_reference_rule_load_failure() {
        // The seam carries no image provider, so the rule graphic cannot be
        // fetched: the provider answers with the reference's own text
        // (`dim.cpp:422-423`) while the script sees the message-only handler
        // error. This is the documented gap, pinned so it cannot go quiet.
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", &layers(1))
            .expect("layers");
        let message = engine
            .execute_script(
                "inline.tjs",
                r#"
                var message = "";
                try { dest.beginTransition("dim", true, source, %[time: 10, rule: "rule.png"]); }
                catch (e) { message = e.message; }
                return message;
                "#,
            )
            .expect("script");
        assert_eq!(
            message,
            Variant::String(
                "Transition handler error iTVPTransHandlerProvider::StartTransition failed"
                    .to_string()
            )
        );
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("ルール画像 rule.png を読み込むことができません")),
            "the reference's rule-image failure text is logged"
        );

        // A missing `rule` is the reference's other message
        // (`dim.cpp:417-419`), reported the same way.
        engine
            .execute_script(
                "inline.tjs",
                r#"
                try { dest.beginTransition("dim", true, source, %[time: 10]); }
                catch (e) { }
                "#,
            )
            .expect("script");
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("オプション rule を指定してください")),
            "the reference's option failure text is logged"
        );
    }
}
