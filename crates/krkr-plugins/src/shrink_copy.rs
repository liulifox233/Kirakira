//! `shrinkCopy.dll` — the area-average shrink blits on `Layer`.
//!
//! Reference: `shrinkCopy/main.cpp` (485 lines, byte-identical in the krkr2
//! trunk copy and the upstream `wtnbgo/shrinkCopy` repository the port was
//! verified against). Two functions attached to the global `Layer` class with
//! ncbind — `Layer.shrinkCopy(dleft, dtop, dwidth, dheight, src, sleft, stop,
//! swidth, sheight)` (`main.cpp:369`) and `Layer.shrinkCopyFast(src, shrink_x,
//! shrink_y=0)` (`main.cpp:485`).
//!
//! # `shrinkCopy`
//!
//! A destination rect in **reals** and a source rect in **pixels**
//! (`main.cpp:79-106`). `check()` (`:108-118`) refuses a non-positive
//! source/destination size and any enlargement the integer test
//! `sw < (long)dw || sh < (long)dh` catches (`:110-111`) — `TJS_E_INVALIDPARAM`
//! (`:93`), the "Invalid argument" exception. `clip()` (`:122-180`) first
//! drops the whole call when the source rect lies outside the source image
//! (`:130-131`, "fully clipped is a no-op"), then clips the source rect to its
//! image while shrinking the destination rect by the same ratio, turns the
//! destination rect into integer pixel bounds (`:148-154`: truncation toward
//! zero plus a carry when the real end passed it), clips those to the
//! destination image (`:157-164`) and derives the write range
//! `dsx/dsy/dex/dey` (`:161-164`). `copy()` (`:187-226`) block-averages:
//!
//! * Two tables (`makeAvgTable`, `:233-269`) describe each destination column
//!   and row as a source interval: `offset` is the first *whole* source pixel
//!   of the interval, `step` the number of whole pixels, `ta`/`ba` the
//!   fractional weights (1/256 px units) of the first and last pixel, `tc`/
//!   `bc` the same for the colour channels, and `total` the interval's weight
//!   sum. The **first and last** destination pixel of the range use
//!   `setAvgInfoEdge` (`:284-301`) and every pixel between them uses
//!   `setAvgInfo` (`:271-282`); a single-pixel range uses the edge form for
//!   it too (`:259-262`). The edge form clamps the interval to the source rect
//!   for the sampled range (`f1 = max(r1, 0)`, `f2 = min(r2, sw)`, `:289-290`)
//!   but keeps the *unclamped* extent in `total` and the colour weights —
//!   the "edge rule" that makes an interval overhanging the rect dilute and
//!   alpha-ramp the leading/trailing destination pixel while the colour
//!   channels are weighted by the unclamped extent.
//! * Each destination pixel is a 3x3 neighbourhood sum (`:209-217`): corner
//!   pixels weight alpha by `ta*ta` and colour by `tc*tc`, edge pixels by
//!   `unit*ta` vs `unit*tc`, the whole interior by `unit = hu*vu`, and the
//!   result is `sum / (hi.total * vi.total)` with per-channel integer
//!   division (`:218-222`). A zero alpha weight skips that element —
//!   including its colour contribution, exactly as the reference's `if
//!   (mul1)` gates do (`:313-351`).
//! * Channel bytes are the reference's `0xAARRGGBB` DWORD in memory order
//!   (`:318-321`): byte 0..2 are the three colour bytes and byte 3 is alpha.
//!   Every weight in the algorithm is positional (the colour weight applies
//!   to all three colour bytes, `ta`/`ba` to alpha), and the engine's R, G, B,
//!   A store has the same alpha position, so the average is
//!   layout-independent: the port reads and writes byte positions verbatim
//!   and the reference's `r`/`g`/`b` names stay memory positions, not colours.
//! * The unit is 256 (1/256 px) and drops only for shrink factors <= 1/16
//!   (`:253-258`), where the reference's 32-bit accumulators would otherwise
//!   overflow. Where they *still* can wrap — shrink factors just below 16,
//!   where a truncated edge weight can push `total` above the guard — the
//!   port accumulates in 64 bits and returns the unwrapped value.
//!
//! `face`, `holdAlpha`, opacity and the clip box are ignored, as in the
//! reference. Neither function calls `Layer.update()` (the reference writes
//! the bitmap and leaves the repaint to the layer's next draw), and the
//! engine re-uploads a committed image by texture id, so a script sees the
//! new pixels through `getMainPixel` and the next frame alike.
//!
//! # `shrinkCopyFast`
//!
//! `LimitedShrink` (`:372-484`): an integer-ratio block average, alpha
//! ignored (always 255, `:443`, `:462`). `stepy == 0` (argument omitted or
//! literal 0) means `stepy = stepx` (`:389`); `stepx`/`stepy` must be
//! positive and the source readable, else `TJS_E_INVALIDPARAM` (`:380`,
//! `:392-397`). The destination is **resized** to
//! `(ceil(siw/stepx), ceil(sih/stepy))` through `setImageSize` (`:398-404`);
//! a failure there is `TJS_E_FAIL` (`:381`). The copy walks `siw/stepx` whole
//! blocks per row plus one remainder block when `siw % stepx != 0`
//! (`:406-407`, `:467-470`), and for `stepy > 1` averages `stepy`
//! horizontally-shrunk rows from a scratch buffer, with a final remainder
//! group for `sih % stepy` (`:411-433`). `ShrinkLine` (`:435-465`) ignores
//! alpha: it copies or averages bytes 0..2 only.
//!
//! Two behaviours the engine seam cannot take verbatim, both deliberate:
//!
//! * With `src == dst` the reference resizes the destination — freeing the
//!   bitmap it is about to read — and then averages from the stale pointer
//!   (use-after-free). The port snapshots the source plane before the resize,
//!   so the call means what the manual describes (`manual.tjs`: "its own size
//!   is forcibly changed").
//! * The reference's `resize()` failure is the raw `TJS_E_FAIL` HRESULT; the
//!   engine has no such code, so the port reports a runtime error with the
//!   same meaning.
//!
//! Faithfulness of the arithmetic: every clipping rule, table entry and sum
//! below is a transcription of the lines cited above, and the tests derive
//! their expectations by hand from that arithmetic (an exact 2:1 box average,
//! a 2.5:1 interval ending on the source edge, a fractional destination
//! origin exercising both clamps, a 1:1 pass-through, the fast variant's
//! integer blocks, remaining row/column and forced alpha).

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{
        LayerBitmapView, LayerBitmapViewMut, layer_bitmap_read, layer_bitmap_read_write,
        layer_bitmap_write,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer.shrinkCopy / Layer.shrinkCopyFast area-average shrink blits",
    notes: "A port of shrinkCopy/main.cpp (485 lines, byte-identical upstream): \
            the fractional-rect area average with 1/256 px weights, the \
            first/last destination edge clamps and the alpha ramp they produce, \
            the reference's integer division and memory-positional channel \
            averaging, and the alpha-forced integer block average with its \
            remaining row/column and forced destination resize. Deliberate \
            deviations: the source is snapshotted before shrinkCopyFast resizes \
            the destination (the reference reads freed memory when src == dst), \
            64-bit accumulators where the reference's 32-bit sum can wrap just \
            below a 16x shrink, and a runtime error where the reference returns \
            the raw TJS_E_FAIL.",
    install: |engine| engine.register_plugin(ShrinkCopyPlugin),
};

pub struct ShrinkCopyPlugin;

impl KrkrPlugin for ShrinkCopyPlugin {
    fn name(&self) -> &str {
        "shrinkCopy.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `NCB_ATTACH_FUNCTION(shrinkCopy, Layer, …)` (`main.cpp:369`) and the
        // same for `shrinkCopyFast` (`:485`). `Plugins.link` re-runs this
        // after boot; a script that replaced a member keeps its own closure.
        register_unless_script(
            runtime,
            layer,
            "shrinkCopy",
            NativeArgCount::AtLeast(9),
            layer_shrink_copy,
        );
        register_unless_script(
            runtime,
            layer,
            "shrinkCopyFast",
            NativeArgCount::AtLeast(2),
            layer_shrink_copy_fast,
        );
        Ok(())
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // ncbind detaches the attached functions when the module unloads.
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        for name in ["shrinkCopy", "shrinkCopyFast"] {
            if matches!(runtime.object_member(layer, name), Variant::Closure(_)) {
                continue;
            }
            runtime.delete_object_member(layer, name);
        }
        Ok(())
    }
}

fn register_unless_script(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &'static str,
    arg_count: NativeArgCount,
    function: impl krkr_tjs2::runtime::NativeFunction<KrkrHost> + 'static,
) {
    if matches!(runtime.object_member(object, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native_with_arg_count(object, name, arg_count, function);
}

/// `ShrinkCopy::layerShrinkCopy` (`main.cpp:79-97`).
fn layer_shrink_copy(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `dst` is the receiver; a call with nothing behind it fails `check()`
    // like any other non-layer destination (`main.cpp:17`).
    let Some(dest) = this_obj else {
        return Err(TjsError::invalid_param());
    };
    // `param[4]->AsObjectNoAddRef()` (`main.cpp:88`); a non-object source is
    // `IsValidLayer(NULL)` failing (`:17`).
    let Some(src) = args.get(4).and_then(Variant::object_handle) else {
        return Err(TjsError::invalid_param());
    };

    let mut copy = ShrinkCopy {
        dx: arg_real(&args, 0)?,
        dy: arg_real(&args, 1)?,
        dw: arg_real(&args, 2)?,
        dh: arg_real(&args, 3)?,
        sx: arg_integer(&args, 5)?,
        sy: arg_integer(&args, 6)?,
        sw: arg_integer(&args, 7)?,
        sh: arg_integer(&args, 8)?,
        ..ShrinkCopy::default()
    };

    // `check()` (`main.cpp:108-118`): the size rules first, then both layer
    // images. `GetLayerBufferAndSize` reads `imageWidth`/`imageHeight`/
    // `mainImageBufferPitch` and requires `w > 0 && h > 0 && pitch != 0`; the
    // seam's metadata read is the same check.
    if !copy.check() {
        return Err(TjsError::invalid_param());
    }
    let source_meta = layer_bitmap_read(runtime, src, |view| SourceMeta {
        width: i64::from(view.bitmap.width),
        height: i64::from(view.bitmap.height),
        pitch: i64::from(view.bitmap.pitch),
    })
    .map_err(|_| TjsError::invalid_param())?;
    let dest_meta = layer_bitmap_read(runtime, dest, |view| SourceMeta {
        width: i64::from(view.bitmap.width),
        height: i64::from(view.bitmap.height),
        pitch: i64::from(view.bitmap.pitch),
    })
    .map_err(|_| TjsError::invalid_param())?;
    if source_meta.width <= 0
        || source_meta.height <= 0
        || source_meta.pitch <= 0
        || dest_meta.width <= 0
        || dest_meta.height <= 0
        || dest_meta.pitch <= 0
    {
        return Err(TjsError::invalid_param());
    }
    copy.siw = source_meta.width;
    copy.sih = source_meta.height;
    copy.spch = source_meta.pitch;
    copy.diw = dest_meta.width;
    copy.dih = dest_meta.height;

    // `if (!inst.clip()) return TJS_S_OK;` (`main.cpp:94`): a fully clipped
    // rectangle writes nothing — no image commit either.
    if !copy.clip() {
        return Ok(Variant::Void);
    }

    layer_bitmap_read_write(runtime, src, dest, |source, destination| {
        let source = SourceBorrowed {
            pitch: i64::from(source.bitmap.pitch),
            pixels: source.pixels,
        };
        let mut destination = DestPlane {
            pitch: i64::from(destination.bitmap.pitch),
            pixels: &mut destination.pixels[..],
        };
        copy.copy(&source, &mut destination);
    })
    .map_err(|_| TjsError::invalid_param())?;
    Ok(Variant::Void)
}

/// `LimitedShrink::layerShrinkCopy` (`main.cpp:375-384`).
fn layer_shrink_copy_fast(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(dest) = this_obj else {
        return Err(TjsError::invalid_param());
    };
    let Some(src) = args.first().and_then(Variant::object_handle) else {
        return Err(TjsError::invalid_param());
    };
    // `(numparams >= 3) ? (long)param[2]->AsInteger() : 0` (`main.cpp:378-379`).
    let stepx = arg_integer(&args, 1)?;
    let stepy = if args.len() >= 3 {
        arg_integer(&args, 2)?
    } else {
        0
    };
    // `if (!stepy) stepy = stepx;` (`main.cpp:389`), then `check()`
    // (`:392-397`): positive steps, a readable source and `IsValidLayer(dst)`.
    let stepy = if stepy == 0 { stepx } else { stepy };
    if stepx <= 0 || stepy <= 0 {
        return Err(TjsError::invalid_param());
    }
    // `GetLayerBufferAndSize(src, …)` (`main.cpp:395`): the source plane is
    // taken here — before `resize()` — because the reference's own source
    // pointer predates the destination's `setImageSize`.
    let source = layer_bitmap_read(runtime, src, SourcePlane::from)
        .map_err(|_| TjsError::invalid_param())?;
    if source.width <= 0 || source.height <= 0 || source.pitch <= 0 {
        return Err(TjsError::invalid_param());
    }
    // `IsValidLayer(dst)` (`main.cpp:396`).
    layer_bitmap_read(runtime, dest, |_| ()).map_err(|_| TjsError::invalid_param())?;

    // `resize()` (`main.cpp:398-404`): `setImageSize(ceil(siw/stepx),
    // ceil(sih/stepy))`, then the destination buffer is taken from the new
    // image. The reference's `(!resize()) → TJS_E_FAIL` (`:381`) has no
    // engine error kind; the runtime error carries the same meaning.
    let width = (source.width + stepx - 1) / stepx;
    let height = (source.height + stepy - 1) / stepy;
    runtime
        .call_object_method(
            dest,
            "setImageSize",
            vec![Variant::Integer(width), Variant::Integer(height)],
        )
        .map_err(|_| resize_failed())?;

    layer_bitmap_write(runtime, dest, |view| {
        copy_fast(&source, view, stepx, stepy);
    })
    .map_err(|_| resize_failed())?;
    Ok(Variant::Void)
}

/// The reference's raw `TJS_E_FAIL` (`main.cpp:381`), as the engine's closest
/// equivalent.
fn resize_failed() -> TjsError {
    TjsError::runtime("shrinkCopyFast could not resize the destination layer.")
}

fn arg_real(args: &[Variant], index: usize) -> Result<f64> {
    match args.get(index) {
        Some(value) => value.to_real(),
        None => Err(TjsError::bad_param_count()),
    }
}

fn arg_integer(args: &[Variant], index: usize) -> Result<i64> {
    match args.get(index) {
        Some(value) => value.to_integer(),
        None => Err(TjsError::bad_param_count()),
    }
}

// ---------------------------------------------------------------------------
// Planes and pixels

/// The image metrics `GetLayerBufferAndSize` reads (`main.cpp:29-49`).
struct SourceMeta {
    width: i64,
    height: i64,
    pitch: i64,
}

/// One layer bitmap as the reference sees it through `mainImageBuffer*`
/// (`main.cpp:52-73`): size, pitch and the bytes. The reference's `BufRefT` is
/// a raw byte pointer and its `PixelT` a `0xAARRGGBB` DWORD, i.e. memory order
/// B, G, R, A; the engine's plane is R, G, B, A per byte, top-down. Only
/// [`Pixel::from_bytes`] and [`Pixel::to_bytes`] translate between them.
struct SourcePlane {
    width: i64,
    height: i64,
    pitch: i64,
    pixels: Vec<u8>,
}

impl SourcePlane {
    fn from(view: &LayerBitmapView<'_>) -> Self {
        Self {
            width: i64::from(view.bitmap.width),
            height: i64::from(view.bitmap.height),
            pitch: i64::from(view.bitmap.pitch),
            pixels: view.pixels.to_vec(),
        }
    }
}

/// [`SourcePlane`] without the copy, for the read-write seam call.
struct SourceBorrowed<'a> {
    pitch: i64,
    pixels: &'a [u8],
}

/// The pixel read both plane flavours answer, addressed in layer pixels.
///
/// The reference dereferences its raw pointers blindly; with the tables'
/// clamps every read that carries a non-zero weight lands inside the source
/// rectangle, and the reads it performs outside it (an interval reaching
/// exactly one pixel past an edge) carry zero weight and are gated off by the
/// `if (mul1)` checks of the helpers. A checked read keeps the port
/// panic-free without changing any weighted result.
trait PixelSource {
    fn pixel(&self, x: i64, y: i64) -> Option<Pixel>;
}

fn read_pixel(pixels: &[u8], pitch: i64, x: i64, y: i64) -> Option<Pixel> {
    if x < 0 || y < 0 {
        return None;
    }
    let offset = y.checked_mul(pitch)?.checked_add(x.checked_mul(4)?)?;
    let offset = usize::try_from(offset).ok()?;
    let bytes = pixels.get(offset..offset + 4)?;
    Some(Pixel::from_bytes(bytes))
}

impl PixelSource for SourcePlane {
    fn pixel(&self, x: i64, y: i64) -> Option<Pixel> {
        read_pixel(&self.pixels, self.pitch, x, y)
    }
}

impl PixelSource for SourceBorrowed<'_> {
    fn pixel(&self, x: i64, y: i64) -> Option<Pixel> {
        read_pixel(self.pixels, self.pitch, x, y)
    }
}

/// A writable destination plane at the destination image's pitch.
struct DestPlane<'a> {
    pitch: i64,
    pixels: &'a mut [u8],
}

impl DestPlane<'_> {
    /// `p[0..4] = …` (`main.cpp:219-222`): the reference writes its summed
    /// channel bytes straight into the same memory positions it read them
    /// from, and so does the port.
    fn write(&mut self, x: i64, y: i64, pixel: Pixel) {
        let Some(offset) = y
            .checked_mul(self.pitch)
            .and_then(|row| x.checked_mul(4).and_then(|column| row.checked_add(column)))
        else {
            return;
        };
        let Ok(offset) = usize::try_from(offset) else {
            return;
        };
        if let Some(bytes) = self.pixels.get_mut(offset..offset + 4) {
            bytes.copy_from_slice(&pixel.to_bytes());
        }
    }
}

/// One pixel in the reference's channel naming (`main.cpp:6`): `r`, `g`, `b`,
/// `a` are the bytes of its `PixelT` DWORD in memory order.
///
/// The reference's buffer is `0xAARRGGBB` little-endian, so its `r` is the
/// DWORD's low byte and its three colour bytes are memory positions 0..2,
/// alpha position 3 (`docs/plugins/layer-ex-family.md` §1). The engine's
/// store is R, G, B, A — the same *positions* for a different colour naming.
/// Every weight below is positional (one weight for bytes 0..2, another for
/// byte 3), so the average is layout-independent and the port reads and
/// writes positions verbatim: a translation would change the bytes of a
/// pass-through copy without changing any weight's meaning.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Pixel {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Pixel {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            r: bytes[0],
            g: bytes[1],
            b: bytes[2],
            a: bytes[3],
        }
    }

    fn to_bytes(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

/// `ElementT` (`main.cpp:185`): the wrapped per-channel sums. The reference
/// accumulates in 32 bits; 64 bits here only changes results where the
/// reference's sums wrap (see the module docs).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Sum {
    r: u64,
    g: u64,
    b: u64,
    a: u64,
}

/// `addRect` (`main.cpp:313-324`): `w` x `h` whole pixels, one weight for all
/// four channels. Gated on `mul` like the reference's `if (mul)`.
fn add_rect(sum: &mut Sum, plane: &impl PixelSource, x: i64, y: i64, w: i64, h: i64, mul: u64) {
    if mul == 0 {
        return;
    }
    for row in 0..h {
        for column in 0..w {
            let Some(pixel) = plane.pixel(x + column, y + row) else {
                continue;
            };
            sum.r += u64::from(pixel.r) * mul;
            sum.g += u64::from(pixel.g) * mul;
            sum.b += u64::from(pixel.b) * mul;
            sum.a += u64::from(pixel.a) * mul;
        }
    }
}

/// `addVert` (`main.cpp:325-333`): `h` pixels down a column; `mul1` scales
/// alpha and is the gate, `mul2` scales the colour bytes.
fn add_vert(sum: &mut Sum, plane: &impl PixelSource, x: i64, y: i64, h: i64, mul1: u64, mul2: u64) {
    if mul1 == 0 {
        return;
    }
    for row in 0..h {
        let Some(pixel) = plane.pixel(x, y + row) else {
            continue;
        };
        sum.r += u64::from(pixel.r) * mul2;
        sum.g += u64::from(pixel.g) * mul2;
        sum.b += u64::from(pixel.b) * mul2;
        sum.a += u64::from(pixel.a) * mul1;
    }
}

/// `addHorz` (`main.cpp:334-342`): `w` pixels along a row, `mul1` for alpha
/// (and the gate), `mul2` for colour.
fn add_horz(sum: &mut Sum, plane: &impl PixelSource, x: i64, y: i64, w: i64, mul1: u64, mul2: u64) {
    if mul1 == 0 {
        return;
    }
    for column in 0..w {
        let Some(pixel) = plane.pixel(x + column, y) else {
            continue;
        };
        sum.r += u64::from(pixel.r) * mul2;
        sum.g += u64::from(pixel.g) * mul2;
        sum.b += u64::from(pixel.b) * mul2;
        sum.a += u64::from(pixel.a) * mul1;
    }
}

/// `addPoint` (`main.cpp:343-351`): one pixel, `mul1` for alpha (and the
/// gate), `mul2` for colour.
fn add_point(sum: &mut Sum, plane: &impl PixelSource, x: i64, y: i64, mul1: u64, mul2: u64) {
    if mul1 == 0 {
        return;
    }
    let Some(pixel) = plane.pixel(x, y) else {
        return;
    };
    sum.r += u64::from(pixel.r) * mul2;
    sum.g += u64::from(pixel.g) * mul2;
    sum.b += u64::from(pixel.b) * mul2;
    sum.a += u64::from(pixel.a) * mul1;
}

// ---------------------------------------------------------------------------
// The reference's ShrinkCopy

/// `RtoL` (`main.cpp:120`): the `(long)` cast, truncating toward zero.
fn rto_l(value: f64) -> i64 {
    value as i64
}

/// `AvgInfoT` (`main.cpp:184`), transcribed field for field. `offset` is the
/// byte offset of the first whole pixel of the interval — the reference's
/// `wk.stop + u1 + 1` times the axis' bytes per pixel — and `step` is `int`
/// there too, so it can be negative for an interval narrower than a pixel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AvgInfo {
    offset: i64,
    step: i64,
    ta: u64,
    tc: u64,
    ba: u64,
    bc: u64,
    total: u64,
}

/// `MakeAvgWorkT` (`main.cpp:228-232`) — one axis of a `makeAvgTable` call.
struct AvgWork {
    unit: u64,
    max: f64,
    ratio: f64,
    diff: f64,
    stop: i64,
    ofmul: i64,
}

/// `ShrinkCopy` (`main.cpp:99-106`, `:122-226`) as one state machine.
#[derive(Debug, Default)]
struct ShrinkCopy {
    // Arguments.
    dx: f64,
    dy: f64,
    dw: f64,
    dh: f64,
    sx: i64,
    sy: i64,
    sw: i64,
    sh: i64,
    // Source and destination image metrics.
    siw: i64,
    sih: i64,
    spch: i64,
    diw: i64,
    dih: i64,
    // clip() results.
    dtx: i64,
    dty: i64,
    dtw: i64,
    dth: i64,
    dsx: i64,
    dsy: i64,
    dex: i64,
    dey: i64,
}

impl ShrinkCopy {
    /// `check()`'s size rules (`main.cpp:110`), before the buffer reads the
    /// caller performs.
    fn check(&self) -> bool {
        self.sw > 0
            && self.sh > 0
            && self.dw > 0.0
            && self.dh > 0.0
            && self.sw >= rto_l(self.dw)
            && self.sh >= rto_l(self.dh)
    }

    /// `clip()` (`main.cpp:122-180`): `false` means "fully clipped, do
    /// nothing".
    fn clip(&mut self) -> bool {
        let zx = self.dw / self.sw as f64;
        let zy = self.dh / self.sh as f64;

        // `srcクリッピング` (`main.cpp:130-131`).
        if self.sx + self.sw <= 0
            || self.sy + self.sh <= 0
            || self.sx >= self.siw
            || self.sy >= self.sih
        {
            return false;
        }
        if self.sx < 0 {
            self.sw += self.sx;
            let dcut = zx * (-self.sx) as f64;
            self.dw -= dcut;
            self.sx = 0;
            self.dx += dcut;
        }
        if self.sy < 0 {
            self.sh += self.sy;
            let dcut = zy * (-self.sy) as f64;
            self.dh -= dcut;
            self.sy = 0;
            self.dy += dcut;
        }
        let scut = self.sx + self.sw - self.siw;
        if scut > 0 {
            self.sw -= scut;
            self.dw -= zx * scut as f64;
        }
        let scut = self.sy + self.sh - self.sih;
        if scut > 0 {
            self.sh -= scut;
            self.dh -= zy * scut as f64;
        }

        // `dstの整数位置` (`main.cpp:148-154`): truncation plus the carry for
        // a real end that passed the truncated one.
        self.dtx = rto_l(self.dx);
        self.dty = rto_l(self.dy);
        self.dtw = rto_l(self.dx + self.dw) - self.dtx;
        self.dth = rto_l(self.dy + self.dh) - self.dty;
        if self.dx + self.dw > (self.dtx + self.dtw) as f64 {
            self.dtw += 1;
        }
        if self.dy + self.dh > (self.dty + self.dth) as f64 {
            self.dth += 1;
        }

        // `dstクリッピング` (`main.cpp:157-164`).
        if self.dtx + self.dtw <= 0
            || self.dty + self.dth <= 0
            || self.dtx >= self.diw
            || self.dty >= self.dih
        {
            return false;
        }
        self.dsx = if self.dtx < 0 { -self.dtx } else { 0 };
        self.dsy = if self.dty < 0 { -self.dty } else { 0 };
        self.dex = if self.dtx + self.dtw > self.diw {
            self.diw - self.dtx
        } else {
            self.dtw
        };
        self.dey = if self.dty + self.dth > self.dih {
            self.dih - self.dty
        } else {
            self.dth
        };
        true
    }

    /// `copy()` (`main.cpp:187-226`).
    fn copy(&self, source: &impl PixelSource, dest: &mut DestPlane<'_>) {
        // `allocAvgBuffer` (`main.cpp:303-308`): one table entry per
        // destination column and row. The reference mallocs `w * h` entries
        // and splits them at `w`, which under-allocates for a one-pixel axis;
        // each table is sized to its own length here.
        let mut horz = Vec::new();
        let mut vert = Vec::new();
        let mut horz_work = AvgWork {
            unit: 0,
            max: self.sw as f64,
            ratio: self.sw as f64 / self.dw,
            diff: self.dx - self.dtx as f64,
            stop: self.sx,
            ofmul: 4,
        };
        let hu = make_avg_table(&mut horz_work, &mut horz, self.dsx, self.dex - 1);
        let mut vert_work = AvgWork {
            unit: 0,
            max: self.sh as f64,
            ratio: self.sh as f64 / self.dh,
            diff: self.dy - self.dty as f64,
            stop: self.sy,
            ofmul: self.spch,
        };
        let vu = make_avg_table(&mut vert_work, &mut vert, self.dsy, self.dey - 1);
        let unit = hu * vu;

        for y in self.dsy..self.dey {
            let vi = vert[(y - self.dsy) as usize];
            for x in self.dsx..self.dex {
                let hi = horz[(x - self.dsx) as usize];
                // `r = rl + hi->offset`, `rl = ps + vi->offset` (`main.cpp:200-204`):
                // the first whole column/row of the clamped interval, in
                // pixels.
                let (r_x, r_y) = (hi.offset / 4, vi.offset / self.spch);
                let (w, h) = (hi.step, vi.step);
                let mut sum = Sum::default();
                add_point(
                    &mut sum,
                    source,
                    r_x - 1,
                    r_y - 1,
                    hi.ta * vi.ta,
                    hi.tc * vi.tc,
                );
                add_horz(&mut sum, source, r_x, r_y - 1, w, hu * vi.ta, hu * vi.tc);
                add_point(
                    &mut sum,
                    source,
                    r_x + w,
                    r_y - 1,
                    hi.ba * vi.ta,
                    hi.bc * vi.tc,
                );
                add_vert(&mut sum, source, r_x - 1, r_y, h, hi.ta * vu, hi.tc * vu);
                add_rect(&mut sum, source, r_x, r_y, w, h, unit);
                add_vert(&mut sum, source, r_x + w, r_y, h, hi.ba * vu, hi.bc * vu);
                add_point(
                    &mut sum,
                    source,
                    r_x - 1,
                    r_y + h,
                    hi.ta * vi.ba,
                    hi.tc * vi.bc,
                );
                add_horz(&mut sum, source, r_x, r_y + h, w, hu * vi.ba, hu * vi.bc);
                add_point(
                    &mut sum,
                    source,
                    r_x + w,
                    r_y + h,
                    hi.ba * vi.ba,
                    hi.bc * vi.bc,
                );

                let div = hi.total * vi.total;
                // The interval products are positive for every reachable
                // rectangle; guarded so a script's degenerate geometry cannot
                // panic the engine (the reference would divide by zero).
                if div == 0 {
                    continue;
                }
                let pixel = Pixel {
                    r: (sum.r / div) as u8,
                    g: (sum.g / div) as u8,
                    b: (sum.b / div) as u8,
                    a: (sum.a / div) as u8,
                };
                dest.write(self.dtx + x, self.dty + y, pixel);
            }
        }
    }
}

/// `makeAvgTable` (`main.cpp:233-269`), filling `table` and returning the
/// axis unit (`hu`/`vu`).
fn make_avg_table(work: &mut AvgWork, table: &mut Vec<AvgInfo>, pos: i64, end: i64) -> u64 {
    work.unit = 256; // 1dotの解像度 (`main.cpp:253`)
    if work.ratio <= 1.0 / 16.0 {
        // 1/16以下で桁あふれの可能性があるのでunitを小さくする (`main.cpp:254-258`)
        let divisor = ((2.0 / 16.0) / work.ratio) as u64;
        work.unit /= divisor.max(1);
        if work.unit == 0 {
            work.unit = 1;
        }
    }
    if pos == end {
        // 縮小幅が１ドットの場合 (`main.cpp:259-261`)
        table.push(avg_info_edge(work, pos));
    } else {
        // 端は edge、間は setAvgInfo (`main.cpp:262-267`).
        table.push(avg_info_edge(work, pos));
        let mut middle = pos + 1;
        while middle < end {
            table.push(avg_info(work, middle));
            middle += 1;
        }
        table.push(avg_info_edge(work, end));
    }
    work.unit
}

/// `total` (`main.cpp:278-281`, `:294-297`): `tc + bc + (t2-t1-1)*unit`. The
/// middle count is `int` and can be -1 for an interval narrower than a pixel;
/// the reference computes the sum in unsigned arithmetic, which is the same
/// value for every rectangle a script can reach through `check()`.
fn interval_total(tc: u64, bc: u64, t1: i64, t2: i64, unit: u64) -> u64 {
    (tc as i64 + bc as i64 + (t2 - t1 - 1) * unit as i64) as u64
}

/// `setAvgInfo` (`main.cpp:271-282`): a whole interval — `ta == tc`,
/// `ba == bc`.
fn avg_info(work: &AvgWork, pos: i64) -> AvgInfo {
    let unit = work.unit;
    let r1 = (pos as f64 - work.diff) * work.ratio;
    let r2 = r1 + work.ratio;
    let t1 = rto_l(r1);
    let t2 = rto_l(r2);
    let tc = (unit as i64 - rto_l((r1 - t1 as f64) * unit as f64)) as u64;
    let bc = rto_l((r2 - t2 as f64) * unit as f64).max(0) as u64;
    AvgInfo {
        offset: (work.stop + t1 + 1) * work.ofmul,
        total: interval_total(tc, bc, t1, t2, unit),
        ta: tc,
        tc,
        ba: bc,
        bc,
        step: t2 - t1 - 1,
    }
}

/// `setAvgInfoEdge` (`main.cpp:284-301`): the first and last interval of an
/// axis clamp to the source rect for the *sampled* range (`u1`/`u2`,
/// `ta`/`ba`) but keep the unclamped extent in `total` and `tc`/`bc`.
fn avg_info_edge(work: &AvgWork, pos: i64) -> AvgInfo {
    let unit = work.unit;
    let r1 = (pos as f64 - work.diff) * work.ratio;
    let r2 = r1 + work.ratio;
    let f1 = r1.max(0.0);
    let f2 = r2.min(work.max);
    let t1 = rto_l(r1);
    let t2 = rto_l(r2);
    let u1 = rto_l(f1);
    let u2 = rto_l(f2);
    let tc = (unit as i64 - rto_l((r1 - t1 as f64) * unit as f64)) as u64;
    let bc = rto_l((r2 - t2 as f64) * unit as f64).max(0) as u64;
    let ta = (unit as i64 - rto_l((f1 - u1 as f64) * unit as f64)) as u64;
    let ba = rto_l((f2 - u2 as f64) * unit as f64).max(0) as u64;
    AvgInfo {
        offset: (work.stop + u1 + 1) * work.ofmul,
        total: interval_total(tc, bc, t1, t2, unit),
        ta,
        tc,
        ba,
        bc,
        step: u2 - u1 - 1,
    }
}

// ---------------------------------------------------------------------------
// The reference's LimitedShrink

/// `LimitedShrink::copy()` (`main.cpp:405-434`).
fn copy_fast(src: &SourcePlane, view: &mut LayerBitmapViewMut<'_>, stepx: i64, stepy: i64) {
    let diw = i64::from(view.bitmap.width);
    let dpch = i64::from(view.bitmap.pitch);
    let dest: &mut [u8] = &mut view.pixels[..];
    let spch = src.pitch;

    // `xdiv = siw/stepx; xrem = siw - xdiv*stepx;` (`main.cpp:406-407`).
    let xdiv = src.width / stepx;
    let xrem = src.width - xdiv * stepx;

    let mut ps = 0usize;
    let mut pd = 0usize;
    if stepy <= 1 {
        // `for (y = 0; y < sih; y++, pd+=dpch, ps+=spch) shrinkLineX(pd, ps);`
        // (`main.cpp:408-410`).
        for _ in 0..src.height {
            shrink_line_x(dest, pd, &src.pixels, ps, xdiv, xrem, stepx);
            pd += dpch as usize;
            ps += spch as usize;
        }
        return;
    }

    // `long bpch = diw * 4; WrtRefT buf = new UnitT[bpch * stepy];`
    // (`main.cpp:412-413`).
    let bpch = diw * 4;
    let mut buf = vec![0u8; (bpch * stepy) as usize];
    let groups = src.height / stepy;
    for _ in 0..groups {
        // `for (sub = stepy, ofs = 0; sub > 0; sub--, ofs+=bpch, ps+=spch)`
        // (`main.cpp:417-418`).
        for sub in 0..stepy {
            shrink_line_x(
                &mut buf,
                (sub * bpch) as usize,
                &src.pixels,
                ps,
                xdiv,
                xrem,
                stepx,
            );
            ps += spch as usize;
        }
        // `shrinkLineY(pd, buf, bpch, stepy);` (`main.cpp:419`).
        shrink_line_y(dest, pd, &buf, bpch, stepy, diw);
        pd += dpch as usize;
    }
    // `div *= stepy; if (div < sih) { yrem = sih - div; … }`
    // (`main.cpp:421-427`).
    let consumed = groups * stepy;
    if consumed < src.height {
        let yrem = src.height - consumed;
        for sub in 0..yrem {
            shrink_line_x(
                &mut buf,
                (sub * bpch) as usize,
                &src.pixels,
                ps,
                xdiv,
                xrem,
                stepx,
            );
            ps += spch as usize;
        }
        shrink_line_y(dest, pd, &buf, bpch, yrem, diw);
    }
}

/// `shrinkLineX` (`main.cpp:467-470`): the whole blocks, then the row's
/// `xrem` leftover pixels as one destination pixel.
fn shrink_line_x(
    dest: &mut [u8],
    dest_pos: usize,
    src: &[u8],
    src_pos: usize,
    xdiv: i64,
    xrem: i64,
    stepx: i64,
) {
    let mut w = dest_pos;
    let mut r = src_pos;
    shrink_line(dest, &mut w, src, &mut r, xdiv, stepx, stepx * 4, 4);
    if xrem > 0 {
        shrink_line(dest, &mut w, src, &mut r, 1, xrem, 0, 4);
    }
}

/// `shrinkLineY` (`main.cpp:471-473`): `diw` destination pixels averaged from
/// `shrink` rows of the scratch buffer at `pch` bytes per row.
fn shrink_line_y(dest: &mut [u8], dest_pos: usize, buf: &[u8], pch: i64, shrink: i64, diw: i64) {
    let mut w = dest_pos;
    let mut r = 0usize;
    shrink_line(dest, &mut w, buf, &mut r, diw, shrink, 4, pch);
}

/// `ShrinkLine` (`main.cpp:435-465`): `len` output pixels, each the average
/// of `shrink` source samples `tstep` bytes apart, advancing `step` bytes per
/// output pixel. Colour bytes 0..2 only; alpha is forced to 255.
///
/// The argument list splits the reference's two cursor references (`w`/`r`)
/// into `(slice, cursor)` pairs, which is what makes it one over the lint's
/// limit; the order is the reference's.
#[allow(clippy::too_many_arguments)]
fn shrink_line(
    dest: &mut [u8],
    dest_cursor: &mut usize,
    src: &[u8],
    src_cursor: &mut usize,
    len: i64,
    shrink: i64,
    step: i64,
    tstep: i64,
) {
    if shrink == 1 {
        // `case 1:` (`main.cpp:438-445`) — one sample, alpha forced.
        for _ in 0..len {
            if let (Some(bytes), Some(out)) = (
                src.get(*src_cursor..*src_cursor + 3),
                dest.get_mut(*dest_cursor..*dest_cursor + 4),
            ) {
                out[0] = bytes[0];
                out[1] = bytes[1];
                out[2] = bytes[2];
                out[3] = 255;
            }
            *src_cursor += step.max(0) as usize;
            *dest_cursor += 4;
        }
        return;
    }
    // `default:` (`main.cpp:450-463`) — `shrink` samples, three channels,
    // integer division, alpha forced.
    for _ in 0..len {
        let (mut sr, mut sg, mut sb) = (0u64, 0u64, 0u64);
        let mut tr = *src_cursor as i64;
        for _ in 0..shrink {
            if tr >= 0
                && let Some(bytes) = src.get(tr as usize..).and_then(|rest| rest.get(..3))
            {
                sr += u64::from(bytes[0]);
                sg += u64::from(bytes[1]);
                sb += u64::from(bytes[2]);
            }
            tr += tstep;
        }
        if let Some(out) = dest.get_mut(*dest_cursor..*dest_cursor + 4) {
            out[0] = (sr / shrink as u64) as u8;
            out[1] = (sg / shrink as u64) as u8;
            out[2] = (sb / shrink as u64) as u8;
            out[3] = 255;
        }
        *src_cursor += step.max(0) as usize;
        *dest_cursor += 4;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use krkr_core::{FrameInput, Size};
    use krkr_engine::{
        EngineConfig, EngineInput, KrkrEngine, plugin_api::layer::layer_bitmap_read,
    };

    use super::ShrinkCopyPlugin;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ShrinkCopyPlugin).expect("plugin");
        engine
    }

    fn pixel(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMainPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    fn mask(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMaskPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    /// One source row of a fresh layer, painted pixel by pixel. Every channel
    /// is distinct so a swapped channel read changes an expectation.
    fn paint_row(engine: &mut KrkrEngine, layer: &str, colors: &[i64]) {
        for (x, color) in colors.iter().enumerate() {
            engine
                .execute_script(
                    "paint.tjs",
                    &format!("{layer}.fillRect({x}, 0, 1, 1, {color});"),
                )
                .expect("paint");
        }
    }

    /// The exact 2:1 box average of `main.cpp:187-226` for a whole-pixel
    /// destination rectangle: with `ratio = 2` and `diff = 0` each
    /// destination pixel's interval is two whole source pixels
    /// (`ta = tc = unit`, `ba = bc = 0`, `total = 2*unit`), so the sum is
    /// `(p0 + p1) * unit^2` and the division by `total_h * total_v` is the
    /// truncating per-channel average.
    #[test]
    fn exact_two_to_one_box_average() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(4, 1);
                src.fillRect(0, 0, 1, 1, 0xff0a141e);
                src.fillRect(1, 0, 1, 1, 0xff1e2832);
                src.fillRect(2, 0, 1, 1, 0xff323c46);
                src.fillRect(3, 0, 1, 1, 0xff46505a);
                global.dst = new Layer();
                dst.setImageSize(2, 1);
                dst.fillRect(0, 0, 2, 1, 0xff000000);
                "#,
            )
            .expect("layers");
        engine
            .execute_script("shrink.tjs", "dst.shrinkCopy(0, 0, 2, 1, src, 0, 0, 4, 1);")
            .expect("shrinkCopy");

        // R: (0x0a + 0x1e)/2 = 0x14, (0x32 + 0x46)/2 = 0x3c.
        // G: (0x14 + 0x28)/2 = 0x1e, (0x3c + 0x50)/2 = 0x46.
        // B: (0x1e + 0x32)/2 = 0x28, (0x46 + 0x5a)/2 = 0x50.
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0x141e28);
        assert_eq!(pixel(&mut engine, "dst", 1, 0), 0x3c4650);
        assert_eq!(mask(&mut engine, "dst", 0, 0), 0xff);
        assert_eq!(mask(&mut engine, "dst", 1, 0), 0xff);
    }

    /// The same 2:1 arithmetic at a non-zero destination origin
    /// (`dleft = dtop = 1`, `main.cpp:197`'s
    /// `pd + ((dtx+dsx)<<2) + ((dty+dsy)*dpch)`): every destination pixel is
    /// the four-pixel average of its 2x2 source block — weights `ta*ta = unit²`
    /// per corner and `total_h * total_v = (2*unit)²`, i.e. `sum / 4` with the
    /// reference's truncating division (block (0,1) sums to 10, so its
    /// average is 2, not 2.5) — and the destination's first row/column are
    /// left untouched.
    #[test]
    fn a_destination_origin_offset_writes_the_mapped_blocks() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(4, 4);
                src.fillRect(0, 0, 1, 1, 0xff0a141e);
                src.fillRect(1, 0, 1, 1, 0xff1e2832);
                src.fillRect(2, 0, 1, 1, 0xff6400ff);
                src.fillRect(3, 0, 1, 1, 0xff7800ff);
                src.fillRect(0, 1, 1, 1, 0xff323c46);
                src.fillRect(1, 1, 1, 1, 0xff46505a);
                src.fillRect(2, 1, 1, 1, 0xff8c00ff);
                src.fillRect(3, 1, 1, 1, 0xffa000ff);
                src.fillRect(0, 2, 1, 1, 0xff0100ff);
                src.fillRect(1, 2, 1, 1, 0xff0200ff);
                src.fillRect(2, 2, 1, 1, 0xffc800ff);
                src.fillRect(3, 2, 1, 1, 0xffc800ff);
                src.fillRect(0, 3, 1, 1, 0xff0300ff);
                src.fillRect(1, 3, 1, 1, 0xff0400ff);
                src.fillRect(2, 3, 1, 1, 0xffc800ff);
                src.fillRect(3, 3, 1, 1, 0xffc900ff);
                global.dst = new Layer();
                dst.setImageSize(4, 4);
                dst.fillRect(0, 0, 4, 4, 0xff123456);
                "#,
            )
            .expect("layers");
        engine
            .execute_script("shrink.tjs", "dst.shrinkCopy(1, 1, 2, 2, src, 0, 0, 4, 4);")
            .expect("shrinkCopy");

        // Block (0,0): R (10+30+50+70)/4 = 40 = 0x28, G (20+40+60+80)/4 = 50 =
        // 0x32, B (30+50+70+90)/4 = 60 = 0x3c.
        assert_eq!(pixel(&mut engine, "dst", 1, 1), 0x28323c);
        assert_eq!(mask(&mut engine, "dst", 1, 1), 0xff);
        // Block (1,0): R (100+120+140+160)/4 = 130 = 0x82, B 0xff.
        assert_eq!(pixel(&mut engine, "dst", 2, 1), 0x8200ff);
        // Block (0,1): R (1+2+3+4)/4 = 2 (truncating).
        assert_eq!(pixel(&mut engine, "dst", 1, 2), 0x0200ff);
        // Block (1,1): R (200+200+200+201)/4 = 200 (truncating).
        assert_eq!(pixel(&mut engine, "dst", 2, 2), 0xc800ff);
        // The destination's untouched rows and columns keep their fill.
        for y in 0..4 {
            for x in 0..4 {
                if (1..3).contains(&x) && (1..3).contains(&y) {
                    continue;
                }
                assert_eq!(pixel(&mut engine, "dst", x, y), 0x123456, "({x}, {y})");
            }
        }
    }

    /// A 2.5:1 ratio, hand-evaluated from `makeAvgTable`/`setAvgInfo*`
    /// (`main.cpp:233-301`): for `sw = 5`, `dw = 2` the first interval is
    /// `[0, 2.5)` — `tc = 256`, `bc = RtoL(0.5*256) = 128`, `total = 640`,
    /// `offset` at pixel 1, `step = 1` — and the last is `[2.5, 5)` —
    /// `tc = 256 - 128`, `bc = 0`, `total = 128 + 2*256 = 640`, `offset` at
    /// pixel 3, `step = 2`. The source rect's end makes the last interval's
    /// `u2 = 5` a zero-weight read (`ba = 0`). With the painted round values
    /// the division is exact for the first pixel and truncating for the last:
    /// `(0x28*128 + 0xfa*256 + 0x80*256) / 640 = 101888/640 = 159.2 → 159`.
    #[test]
    fn non_integer_ratio_uses_the_reference_intervals() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                "global.src = new Layer(); src.setImageSize(5, 1);\n\
                 global.dst = new Layer(); dst.setImageSize(2, 1);",
            )
            .expect("layers");
        // R carries the values; G = 0 and B = 255 flag a swapped channel.
        paint_row(
            &mut engine,
            "src",
            &[0xff6400ff, 0xffc800ff, 0xff2800ff, 0xfffa00ff, 0xff8000ff],
        );
        engine
            .execute_script("shrink.tjs", "dst.shrinkCopy(0, 0, 2, 1, src, 0, 0, 5, 1);")
            .expect("shrinkCopy");

        // Position 0: (0x64*256 + 0xc8*256 + 0x28*128) / 640 = 128 = 0x80.
        // Position 1: (0x28*128 + 0xfa*256 + 0x80*256) / 640 = 159 = 0x9f.
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0x8000ff);
        assert_eq!(pixel(&mut engine, "dst", 1, 0), 0x9f00ff);
        assert_eq!(mask(&mut engine, "dst", 0, 0), 0xff);
        assert_eq!(mask(&mut engine, "dst", 1, 0), 0xff);
    }

    /// A fractional destination origin (`dleft = 0.5`) against a 2:1 source
    /// ratio, evaluated from `main.cpp:233-301` with `ratio = 2`,
    /// `diff = 0.5`, `sw = 4`:
    ///
    /// * `pos = 0` (edge): interval `[-1, 1)` clamps to `u1 = 0, u2 = 1`;
    ///   `tc = 256`, `bc = 0`, `total = tc + bc + (t2-t1-1)*unit = 512`,
    ///   `step = 0`, `ta = 256`, `ba = 0` — the only weighted element is the
    ///   clamped first pixel at full `ta`, so the result is
    ///   `p0 * 65536 / (512*256) = p0/2`, alpha included: the interval's
    ///   overhang dilutes the leading pixel and ramps its alpha (255 → 127).
    /// * `pos = 1` (whole interval): `[1, 3)` — `tc = 256`, `bc = 0`,
    ///   `total = 512`, `step = 1` — `(p1 + p2)/2` at full alpha.
    /// * `pos = 2` (edge): `[3, 5)` clamps to `u2 = 4 = sw` (`f2 = min(5, 4)`);
    ///   `tc = 256`, `ba = 0`, `total = 512` — `p3/2`, alpha 127.
    ///
    /// The destination rectangle is 3 pixels wide (the `:153` carry for
    /// `0.5 + 2 = 2.5`).
    #[test]
    fn fractional_origin_uses_the_reference_edge_clamps() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                "global.src = new Layer(); src.setImageSize(4, 1);\n\
                 global.dst = new Layer(); dst.setImageSize(3, 1);",
            )
            .expect("layers");
        paint_row(
            &mut engine,
            "src",
            &[0xff6400ff, 0xffc800ff, 0xff2800ff, 0xfffa00ff],
        );
        engine
            .execute_script(
                "shrink.tjs",
                "dst.shrinkCopy(0.5, 0, 2, 1, src, 0, 0, 4, 1);",
            )
            .expect("shrinkCopy");

        // Every colour byte is diluted by the same divisor: R is 0x64/2 = 50 =
        // 0x32, (0xc8 + 0x28)/2 = 120 = 0x78, 0xfa/2 = 125 = 0x7d; B (0xff in
        // the source) is 0x7f wherever the divisor halves the pixel.
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0x32007f);
        assert_eq!(pixel(&mut engine, "dst", 1, 0), 0x7800ff);
        assert_eq!(pixel(&mut engine, "dst", 2, 0), 0x7d007f);
        assert_eq!(mask(&mut engine, "dst", 0, 0), 127);
        assert_eq!(mask(&mut engine, "dst", 1, 0), 255);
        assert_eq!(mask(&mut engine, "dst", 2, 0), 127);
    }

    /// Ratio 1 (`dw == sw`, whole pixels): every interval is one whole pixel
    /// (`tc = unit`, `bc = 0`, `total = unit`, `step = 0`), so the copy is a
    /// pass-through — colour and alpha. The committed image also reaches the
    /// frame output without a `Layer.update()` call, which is what the
    /// reference leaves to the layer's next draw.
    #[test]
    fn same_size_copy_is_a_pass_through() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(4, 1);
                src.fillRect(0, 0, 1, 1, 0x80112233);
                src.fillRect(1, 0, 1, 1, 0x40224455);
                src.fillRect(2, 0, 1, 1, 0x20336677);
                src.fillRect(3, 0, 1, 1, 0xff448899);
                global.dst = new Layer();
                dst.setImageSize(4, 1);
                dst.fillRect(0, 0, 4, 1, 0x00000000);
                dst.visible = true;
                dst.setSize(4, 1);
                "#,
            )
            .expect("layers");
        engine
            .execute_script("shrink.tjs", "dst.shrinkCopy(0, 0, 4, 1, src, 0, 0, 4, 1);")
            .expect("shrinkCopy");

        for (x, color) in [0x112233, 0x224455, 0x336677, 0x448899].iter().enumerate() {
            assert_eq!(pixel(&mut engine, "dst", x as i64, 0), *color, "x = {x}");
        }
        for (x, alpha) in [0x80, 0x40, 0x20, 0xff].iter().enumerate() {
            assert_eq!(mask(&mut engine, "dst", x as i64, 0), *alpha, "x = {x}");
        }

        let generation = engine
            .tjs_runtime()
            .global_member("dst")
            .object_handle()
            .map(|layer| {
                layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
                    view.bitmap.generation
                })
                .expect("read generation")
            })
            .expect("dst handle");
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("frame");
        let upload = frame
            .output
            .image_uploads
            .iter()
            .find(|upload| upload.texture_id == generation)
            .expect("the committed image reaches the frame output");
        assert_eq!(&upload.rgba[..4], [0x11, 0x22, 0x33, 0x80]);
    }

    /// `clip()` (`main.cpp:122-180`): a destination rectangle entirely
    /// outside the destination image (`dtx >= diw`) and a source rectangle
    /// entirely outside the source image (`sx + sw <= 0`) are both
    /// `TJS_S_OK` no-ops — nothing is written and no image is committed.
    #[test]
    fn fully_clipped_rectangles_are_no_ops() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(4, 1);
                src.fillRect(0, 0, 1, 1, 0xff0a141e);
                src.fillRect(1, 0, 1, 1, 0xff1e2832);
                global.dst = new Layer();
                dst.setImageSize(4, 1);
                dst.fillRect(0, 0, 4, 1, 0xff123456);
                "#,
            )
            .expect("layers");
        let generation = |engine: &mut KrkrEngine| {
            let layer = engine
                .tjs_runtime()
                .global_member("dst")
                .object_handle()
                .expect("dst handle");
            layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
                view.bitmap.generation
            })
            .expect("generation")
        };
        let before = generation(&mut engine);

        // Destination origin at the image edge: `dtx >= diw` (`main.cpp:158`).
        engine
            .execute_script("clip.tjs", "dst.shrinkCopy(4, 0, 2, 1, src, 0, 0, 2, 1);")
            .expect("destination clip");
        // Source rect ending at or before zero: `sx + sw <= 0`
        // (`main.cpp:130`).
        engine
            .execute_script("clip.tjs", "dst.shrinkCopy(0, 0, 2, 1, src, -4, 0, 2, 1);")
            .expect("source clip");

        for x in 0..4 {
            assert_eq!(pixel(&mut engine, "dst", x, 0), 0x123456, "x = {x}");
        }
        assert_eq!(
            generation(&mut engine),
            before,
            "a no-op must not replace the destination image"
        );
    }

    /// `check()` (`main.cpp:108-118`) and the argument contract: an
    /// enlargement, a non-positive size, a non-layer source, a destination
    /// without an image and a short argument list all fail, each with the
    /// reference's own code.
    #[test]
    fn invalid_arguments_are_rejected() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(2, 1);
                src.fillRect(0, 0, 2, 1, 0xff0a141e);
                global.dst = new Layer();
                dst.setImageSize(4, 1);
                dst.fillRect(0, 0, 4, 1, 0xff000000);
                "#,
            )
            .expect("layers");

        // `sw < (long)dw` → TJS_E_INVALIDPARAM (`main.cpp:110-111`, `:93`).
        let error = engine
            .execute_expression("inline.tjs", "dst.shrinkCopy(0, 0, 4, 1, src, 0, 0, 2, 1)")
            .expect_err("enlargement");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::InvalidParam);
        assert_eq!(error.message, "Invalid argument");

        // A zero destination width is `dw <= 0` (`main.cpp:110`).
        let error = engine
            .execute_expression("inline.tjs", "dst.shrinkCopy(0, 0, 0, 1, src, 0, 0, 2, 1)")
            .expect_err("zero width");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::InvalidParam);

        // A non-object source is `IsValidLayer(NULL)` (`main.cpp:17`).
        let error = engine
            .execute_expression("inline.tjs", "dst.shrinkCopy(0, 0, 2, 1, 7, 0, 0, 2, 1)")
            .expect_err("non-layer source");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::InvalidParam);

        // A freed destination image fails `GetLayerBufferAndSize`
        // (`main.cpp:114-115`).
        engine
            .execute_script("free.tjs", "dst.freeImage();")
            .expect("freeImage");
        let error = engine
            .execute_expression("inline.tjs", "dst.shrinkCopy(0, 0, 2, 1, src, 0, 0, 2, 1)")
            .expect_err("freed destination");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::InvalidParam);

        // `numparams < 9` is TJS_E_BADPARAMCOUNT (`main.cpp:81`).
        let error = engine
            .execute_expression("inline.tjs", "dst.shrinkCopy(0, 0, 2, 1, src, 0, 0, 2)")
            .expect_err("short argument list");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        assert_eq!(error.message, "Invalid argument count");
    }

    /// `shrinkCopyFast` with `stepx = stepy = 2` (`main.cpp:411-434`): each
    /// 2x2 block collapses to the average of its colour bytes — two integer
    /// divisions per pixel, so the source values divide exactly — and the
    /// destination layer is resized to `(ceil(4/2), ceil(2/2)) = (2, 1)`.
    /// Alpha is forced to 255 even though the source is fully transparent
    /// (`:443`, `:462`).
    #[test]
    fn shrink_copy_fast_averages_blocks_and_forces_opaque() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(4, 2);
                src.fillRect(0, 0, 1, 1, 0x003c00ff);
                src.fillRect(1, 0, 1, 1, 0x005000ff);
                src.fillRect(2, 0, 1, 1, 0x006400ff);
                src.fillRect(3, 0, 1, 1, 0x007800ff);
                src.fillRect(0, 1, 1, 1, 0x006400ff);
                src.fillRect(1, 1, 1, 1, 0x007800ff);
                src.fillRect(2, 1, 1, 1, 0x008c00ff);
                src.fillRect(3, 1, 1, 1, 0x00a000ff);
                global.dst = new Layer();
                dst.setImageSize(8, 8);
                dst.fillRect(0, 0, 8, 8, 0xff000000);
                "#,
            )
            .expect("layers");
        engine
            .execute_script("shrink.tjs", "dst.shrinkCopyFast(src, 2);")
            .expect("shrinkCopyFast");

        // Block (0,0): rows 0x3c,0x50 and 0x64,0x78 average in x to 0x46 and
        // 0x6e, then in y to 0x5a = 90. Block (1,0): 0x64,0x78 → 0x6e and
        // 0x8c,0xa0 → 0x96, then 0x82 = 130. B is 0xff everywhere, alpha 255.
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0x5a00ff);
        assert_eq!(pixel(&mut engine, "dst", 1, 0), 0x8200ff);
        assert_eq!(mask(&mut engine, "dst", 0, 0), 255);
        assert_eq!(mask(&mut engine, "dst", 1, 0), 255);
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "dst.imageWidth * 100 + dst.imageHeight")
                .expect("size")
                .to_integer()
                .expect("integer"),
            201,
            "the destination was resized to 2x1"
        );
    }

    /// The remainder rules (`main.cpp:406-407`, `:421-427`): a 5x3 source
    /// with `stepx = stepy = 2` gives `xdiv = 2`/`xrem = 1` and
    /// `ceil(5/2) = 3` columns — the last one the average of the single
    /// leftover pixel — and the `sih % stepy = 1` group gives a second row
    /// averaged from one row. Destination row 0 is the y-average of source
    /// rows 0 and 1 (row 1 transparent, so its colour bytes contribute 0).
    #[test]
    fn shrink_copy_fast_handles_remainder_column_and_row() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(5, 3);
                src.fillRect(0, 0, 5, 3, 0x00000000);
                src.fillRect(0, 0, 1, 1, 0xff2800ff);
                src.fillRect(1, 0, 1, 1, 0xff5000ff);
                src.fillRect(2, 0, 1, 1, 0xff7800ff);
                src.fillRect(3, 0, 1, 1, 0xffa000ff);
                src.fillRect(4, 0, 1, 1, 0xffc800ff);
                src.fillRect(0, 2, 1, 1, 0xff0a00ff);
                src.fillRect(1, 2, 1, 1, 0xff1400ff);
                src.fillRect(2, 2, 1, 1, 0xff1e00ff);
                src.fillRect(3, 2, 1, 1, 0xff2800ff);
                src.fillRect(4, 2, 1, 1, 0xff3200ff);
                global.dst = new Layer();
                dst.setImageSize(1, 1);
                "#,
            )
            .expect("layers");
        engine
            .execute_script("shrink.tjs", "dst.shrinkCopyFast(src, 2);")
            .expect("shrinkCopyFast");

        // Row 0: x averages (0x28+0x50)/2 = 0x3c, (0x78+0xa0)/2 = 0x8c, and
        // the remainder 0xc8; row 1 is transparent black, so the y average
        // halves every byte — R to 0x1e, 0x46, 0x64 and the source's B = 0xff
        // to 0x7f.
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0x1e007f);
        assert_eq!(pixel(&mut engine, "dst", 1, 0), 0x46007f);
        assert_eq!(pixel(&mut engine, "dst", 2, 0), 0x64007f);
        // Row 1 is the single remaining source row (0x0a, 0x14, 0x1e, 0x28,
        // 0x32): column 0 pairs 0x0a/0x14 → 0x0f, column 1 pairs 0x1e/0x28 →
        // 0x23, and the remainder column takes 0x32 alone.
        assert_eq!(pixel(&mut engine, "dst", 0, 1), 0x0f00ff);
        assert_eq!(pixel(&mut engine, "dst", 1, 1), 0x2300ff);
        assert_eq!(pixel(&mut engine, "dst", 2, 1), 0x3200ff);
        for x in 0..3 {
            assert_eq!(mask(&mut engine, "dst", x, 1), 255, "x = {x}");
        }
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "dst.imageWidth * 100 + dst.imageHeight")
                .expect("size")
                .to_integer()
                .expect("integer"),
            302,
            "the destination was resized to 3x2"
        );
    }

    /// `stepx = stepy = 1` takes `ShrinkLine`'s `case 1` (`main.cpp:438-445`)
    /// for the whole row: the colour bytes are copied untouched and alpha is
    /// forced to 255 — the same-size pass-through with the fast variant's
    /// alpha rule. An explicit `shrink_y = 0` means `shrink_x` (`:389`).
    #[test]
    fn shrink_copy_fast_copies_at_ratio_one_and_forces_alpha() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(3, 1);
                src.fillRect(0, 0, 1, 1, 0x00112233);
                src.fillRect(1, 0, 1, 1, 0x40224455);
                src.fillRect(2, 0, 1, 1, 0xff336677);
                global.dst = new Layer();
                dst.setImageSize(1, 1);
                "#,
            )
            .expect("layers");
        engine
            .execute_script("shrink.tjs", "dst.shrinkCopyFast(src, 1, 0);")
            .expect("shrinkCopyFast");

        for (x, color) in [0x112233, 0x224455, 0x336677].iter().enumerate() {
            assert_eq!(pixel(&mut engine, "dst", x as i64, 0), *color, "x = {x}");
        }
        for x in 0..3 {
            assert_eq!(
                mask(&mut engine, "dst", x, 0),
                255,
                "the fast path ignores the source alpha, x = {x}"
            );
        }
    }

    /// `shrinkCopyFast`'s argument contract (`main.cpp:377-381`, `:392-397`):
    /// `numparams < 2` is `TJS_E_BADPARAMCOUNT`; a non-positive step or a
    /// non-layer source is `TJS_E_INVALIDPARAM`.
    #[test]
    fn shrink_copy_fast_rejects_invalid_arguments() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                "global.src = new Layer(); src.setImageSize(2, 2);\n\
                 global.dst = new Layer(); dst.setImageSize(2, 2);",
            )
            .expect("layers");

        let error = engine
            .execute_expression("inline.tjs", "dst.shrinkCopyFast(src)")
            .expect_err("missing step");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);

        let error = engine
            .execute_expression("inline.tjs", "dst.shrinkCopyFast(src, 0)")
            .expect_err("zero step");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::InvalidParam);

        let error = engine
            .execute_expression("inline.tjs", "dst.shrinkCopyFast(7, 2)")
            .expect_err("non-layer source");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::InvalidParam);
    }

    /// `Plugins.unlink` detaches the two members (ncbind's unload path) and a
    /// later link installs them again.
    #[test]
    fn unlink_detaches_the_members_and_relink_installs_them() {
        let mut engine = engine();
        engine
            .execute_script("unlink.tjs", "Plugins.unlink(\"shrinkCopy.dll\");")
            .expect("unlink");
        let error = engine
            .execute_expression("inline.tjs", "Layer.shrinkCopyFast(0, 1)")
            .expect_err("detached");
        assert!(matches!(
            error.kind,
            krkr_tjs2::TjsErrorKind::MemberNotFound | krkr_tjs2::TjsErrorKind::InvalidType
        ));

        engine
            .execute_script("relink.tjs", "Plugins.link(\"shrinkCopy.dll\");")
            .expect("relink");
        engine
            .execute_script(
                "inline.tjs",
                "global.src = new Layer(); src.setImageSize(2, 2);\n\
                 global.dst = new Layer(); dst.setImageSize(1, 1);\n\
                 dst.shrinkCopyFast(src, 2);",
            )
            .expect("shrinkCopyFast after relink");
    }
}
