//! `layerExDraw.dll`: the GDI+ drawing surface for `Layer`, ported to the
//! engine's own software rasteriser.
//!
//! Real plugin: `krkr2/kirikiri2/trunk/kirikiri2/src/plugins/win32/layerExDraw/`
//! (upstream <https://github.com/wtnbgo/layerExDraw>; the family dossier is
//! `docs/plugins/layer-ex-family.md` §2.1). The reference wraps the layer's
//! raw bitmap in a GDI+ `Bitmap(width, height, pitch, PixelFormat32bppARGB,
//! buffer)` and draws through a `Graphics` on it (`LayerExDraw.cpp:975-1009`);
//! this port keeps the same surface and replaces the GDI+ calls with a
//! software rasteriser in this file.
//!
//! # The surface, and what is real
//!
//! `NCB_ATTACH_CLASS_WITH_HOOK(LayerExDraw, Layer)` attaches four properties
//! and forty-one methods to the global `Layer` class (`main.cpp:859-936`);
//! every one of them is registered here with the reference's own argument
//! count, which ncbind checks as `numparams < declared` (`ncbind.hpp:1186`),
//! i.e. `NativeArgCount::AtLeast`.
//!
//! | member | args | reference | this port |
//! |---|---|---|---|
//! | `updateWhenDraw` `smoothingMode` `textRenderingHint` `record` | props | `main.cpp:861-863, 908` | real, per-layer state (recording is a warned no-op) |
//! | `setViewTransform` `resetViewTransform` `rotateViewTransform` `scaleViewTransform` `translateViewTransform` | 1/0/1/2/2 | `LayerExDraw.cpp:1026-1061` | real |
//! | `setTransform` `resetTransform` `rotateTransform` `scaleTransform` `translateTransform` | 1/0/1/2/2 | `:1080-1115` | real |
//! | `clear` | 1 | `:1121-1130` | real (fills the clip box, SourceCopy) |
//! | `drawPath` | 2 | `:1266-1270` | real |
//! | `drawArc` `drawPie` | 7 | `:1282-1288, 1422-1428` | real |
//! | `drawBezier` `drawBeziers` | 9/2 | `:1303-1325` | real |
//! | `drawClosedCurve` `drawClosedCurve2` | 2/3 | `:1333-1358` | real (cardinal spline) |
//! | `drawCurve` `drawCurve2` `drawCurve3` | 2/3/5 | `:1366-1410` | real (cardinal spline) |
//! | `drawEllipse` | 5 | `:1439-1445` | real |
//! | `drawLine` `drawLines` `drawPolygon` `drawRectangle` `drawRectangles` | 5/2/2/5/2 | `:1456-1529` | real |
//! | `drawString` `drawPathString` | 5 | `:1540-1636` | **warned stub** (no text backend) |
//! | `measureString` `measureStringInternal` | 2 | `:1644-1681` | **warned stub** (empty RectF) |
//! | `drawImage` `drawImageRect` `drawImageStretch` `drawImageAffine` | 3/7/9/12 | `:1690-1800` | real for a `Layer` source (bilinear samples); a data-less `GdiPlus.Image` warns |
//! | `getRecordImage` `redrawRecord` `saveRecord` `loadRecord` | 0/0/1/1 | `:1884-1983` | **warned stub** (no metafile format) |
//! | `saveImage` | ≥1 | `:2280-2319` | **warned stub** (no encoder; returns false) |
//! | `getColorRegionRects` | 1 | `:2328-2373` | real row runs, doc'd divergence on merging |
//! | `GdiPlus.*` | — | `main.cpp:598-789` | 154 enum members (170 with the `Layer` block's 16 `EncoderValue`s), PointF/RectF/Matrix real geometry, Font/Image data-less |
//!
//! `main.cpp:916` and `:592` are the two members only the krkr2 variant of the
//! plugin has (`getColorRegionRects`, `Path.drawPath`); the mission points at
//! that tree, so both are kept (the dossier §4 asks for the choice to be
//! deliberate).
//!
//! # The drawing model
//!
//! The reference's drawing methods all build a GDI+ `GraphicsPath` and hand it
//! to `_drawPath` (`:1204-1261`), which walks the `Appearance`'s draw list:
//! every `DrawInfo` is either a pen (stroke, `type == 0`) or a brush (fill,
//! `type == 1`), each with its own `(ox, oy)` offset. For each entry the
//! graphics transform is `T(ox, oy) · calcTransform` — the offset matrix is
//! prepended to the context transform (`draw`/`fill`, `:1181-1199`) — and the
//! transform is `calcTransform = transform · viewTransform` (`:1064-1073`).
//! The port composes the same matrix and transforms the geometry itself,
//! which is why a pen's width is transformed by the same matrix: the stroke
//! outline is built from the *world-space* width and then mapped, exactly the
//! way GDI+ scales a pen under its transform for the segment parts.
//!
//! Coordinates. GDI+ addresses a 32bppARGB bitmap with pixel `(x, y)`
//! covering the square `[x, x+1) × [y, y+1)`; a one-pixel pen centred on
//! `y = 0.5` therefore covers exactly one pixel row, and a line ending at
//! `x = 0.5` covers half of the pixel column it ends in. The port measures
//! coverage in the same frame: a pixel's coverage is the fraction of its
//! square the shape covers, computed on `SUB_ROWS` horizontal sub-rows per
//! pixel row with exact horizontal extents. That is the same model GDI+ (its
//! own antialiaser is a low-resolution supersampler) approximates, and the
//! tests derive their expectations from the geometry, not from this
//! implementation. `smoothingMode` selects it: `SmoothingModeAntiAlias` and
//! `SmoothingModeHighQuality` antialias; `Default`, `HighSpeed`, `None` and
//! `Invalid` test the pixel centre and write hard edges. The reference's
//! default is `SmoothingModeAntiAlias` (`:954`).
//!
//! Fill rule. A `GraphicsPath` defaults to `FillModeAlternate` — the even-odd
//! rule — and the reference never calls `SetFillMode`, so fills (including a
//! pen-less `Appearance`) use even-odd: two overlapping figures in one path
//! leave their overlap unpainted. The port implements exactly that (winding
//! parity per sub-row).
//!
//! Strokes. A pen draws the segment rectangle of width `w` (GDI+ default
//! `LineCapFlat`, so butt caps), with a miter join at interior vertices
//! (`LineJoinMiter`, miter limit 10 — `Pen`'s defaults) that falls back to a
//! bevel past the limit. `Appearance.addPen`'s option dictionary may set
//! `width`, `lineJoin` and the `startCap`/`endCap` caps; dashes
//! (`dashStyle`/`dashOffset`/`dashPattern`), `dashCap`, `compoundArray` and
//! `PenAlignmentInset` are warned no-ops — dashes are not implemented in this
//! port, so the pen draws one solid stroke.
//!
//! Brushes. Only `BrushTypeSolidColor` is implemented. The hatch, texture,
//! path-gradient and linear-gradient types are recognised — and a type
//! outside 0..=4 throws `invalid brush type` exactly where `createBrush`
//! does (`:785-787`) — but their draw entries are skipped with a one-time
//! warning rather than painted with a wrong colour.
//!
//! Images. `drawImage*` map the source `(sleft, stop, swidth, sheight)`
//! rectangle onto a destination parallelogram through `calcTransform`
//! exactly as the reference's four members funnel into `drawImageAffine`
//! (`:1690-1800`), and the returned rect is the transformed corners' box. The
//! `src` argument is the reference's `Image*`, which its converter also
//! answers for a **`Layer`** (`main.cpp:424-445`); a layer's pixels are
//! reachable here, so those calls are real: every destination pixel whose
//! centre lies inside the quad takes a bilinear sample of the source (GDI+'s
//! default interpolation mode; taps outside the image clamp to its edge),
//! blended SourceOver. Destination edges are hard — GDI+ does not antialias a
//! `DrawImage` parallelogram either. A `GdiPlus.Image` carries no pixels
//! (the engine's decoder is not reachable from plugin code) and takes the
//! reference's null-image no-op with a warning (`:1694`).
//!
//! Colours. The reference's pixels are B, G, R, A in memory (`0xAARRGGBB`
//! DWORDs, dossier §1); the engine's plane is R, G, B, A (`plugin_api::layer`),
//! so every colour is decomposed to R/G/B/A and re-composed at the write, and
//! the `getColorRegionRects` comparison packs the engine pixel back into ARGB.
//! Blending is SourceOver in straight alpha — `reset()` sets
//! `CompositingModeSourceOver` (`:993`) and the layer is `PixelFormat32bppARGB`
//! — computed in `f64` and rounded half-up, where GDI+ blends in premultiplied
//! integer space; a covered pixel's colour is exact, and a partially covered
//! pixel can differ by one unit of alpha or colour from GDI+'s rounding. The
//! image sampler interpolates the straight channels, where GDI+ interpolates
//! premultiplied ones; the two agree wherever the alpha is uniform.
//!
//! The clip box (`bitmap.clip`) is applied per pixel, as the reference's
//! `Region(Rect(clipLeft, clipTop, clipWidth, clipHeight))` (`:1006-1007`)
//! does; `clear` fills only the clip box (`Graphics::Clear` fills the clipping
//! region), and `draw*` methods call `Layer.update()` through
//! [`layer_update`] when `updateWhenDraw` is set, mirroring `updateRect`
//! (`:937-946`). The engine's `update` posts a whole-layer repaint, so the
//! reference's four-argument call and its zero-rect no-op become one repaint
//! per draw.
//!
//! # Divergences from the reference
//!
//! * **`Layer.update` arguments.** The reference passes `updateRect`'s
//!   `(x, y, width, height)` and a draw with no appearance entries asks for a
//!   zero rect, which repaints nothing; [`layer_update`] posts the layer's
//!   whole repaint either way, so an empty appearance still marks the layer
//!   dirty.
//! * **Coverage.** GDI+'s antialiaser is a black box; the port computes the
//!   geometric area fraction, quantised to [`SUB_ROWS`] sub-rows per pixel
//!   row. Exact for sub-row-aligned geometry (all the tests), at most
//!   `1/(2·SUB_ROWS)` off on a slanted edge.
//! * **Curves.** Arcs, ellipses and cardinal splines are flattened to
//!   polylines (tolerance [`FLATTEN_TOLERANCE`]) instead of GDI+'s Bezier
//!   representation, so a path's `GetBounds` is the polyline's bounds and the
//!   curve outline can differ inside a pixel.
//! * **Stroke joins under a non-uniform transform.** The port builds the
//!   stroke in world space and maps it, where GDI+ maps the pen and builds the
//!   stroke in device space; the two differ only for the join/miter of a
//!   skewed transform.
//! * **Stroke pieces are unioned per sub-row.** A polyline's stroke is the
//!   union of its segment rectangles, join wedges and caps; the port merges
//!   their intervals on every sub-row, so overlapping pieces count once, the
//!   way GDI+ draws one continuous stroke outline. Only the sub-row
//!   quantisation above remains.
//! * **Text, metafiles, encoders, `GdiPlus.Image`.** Registered with the
//!   reference's member count, a one-time warning and a failure result: no
//!   text backend is reachable from plugin code (`drawString`/
//!   `drawPathString`/`measureString*` draw nothing and measure empty), no
//!   metafile format exists (`record` setter warns, `getRecordImage` answers
//!   void, `redrawRecord`/`saveRecord` answer false — `saveRecord`'s false is
//!   the reference's result without a metafile, `:1937-1964`; `loadRecord`
//!   answers false exactly as the reference always does, `:1972-1983`), no
//!   encoder is reachable (`saveImage` answers false instead of GDI+'s
//!   status), and the engine's image decoder is not reachable either, so a
//!   `GdiPlus.Image` has no pixels and `drawImage*` of one is the reference's
//!   null-image no-op (`:1694`). A **`Layer`** source *is* sampled (see
//!   Images above).
//! * **State carriers.** The reference holds `Path` figures and `Appearance`
//!   draw lists in ncbind native instances; this port stores each object's
//!   data in a `__figures`/`__drawInfos` member of the object itself (a flat
//!   array), so the state dies with the object and nothing else owns it. The
//!   members are script-visible, which no reference script reads or writes.
//! * **Read-only properties.** `RectF.left/top/right/bottom/location/bounds`
//!   and the `Font` metrics deny a script write with `TJS_E_ACCESSDENYED`,
//!   like the reference's `TJS_DENY_NATIVE_PROP_SETTER` properties, and the
//!   `Matrix` `*Order` arguments are required, as ncbind's declared-parameter
//!   count makes them.
//! * **Brushes and dashes.** Only solid brushes and solid strokes are
//!   rasterised; the other brush types, pen dashes and `compoundArray` are
//!   recognised, warned about once and skipped. This is scope, not a missing
//!   engine facility — gradients, hatch patterns and dashes are arithmetic
//!   over this rasteriser.
//! * **`getColorRegionRects`** returns the reference's scanline runs merged
//!   vertically where they are identical; GDI+ unions the runs into a `Region`
//!   first, whose scan conversion can merge differently. The covered area is
//!   the same.
//! * **Non-matrix transform arguments.** The reference's converter builds a
//!   `Matrix` from an array or a dictionary and passes `NULL` for anything
//!   else, which makes `setTransform` reset the transform; the port accepts
//!   the same two shapes and, for anything else, throws instead of silently
//!   resetting.
//! * **The returned update region** is the transformed geometry's bounds
//!   expanded by the pen width, like `GraphicsPath::GetBounds(..., pen)`
//!   (`:1229`), but measured on flattened points.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Mutex;

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{
        LayerBitmap, LayerBitmapView, LayerBitmapViewMut, layer_bitmap_read,
        layer_bitmap_read_write, layer_bitmap_write, layer_update,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{
        NativeArgCount, NativeFunction, NativePropertyAccess, ObjectHandle, Runtime, Variant,
    },
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer.draw* vector drawing / GdiPlus namespace",
    notes: "The path/line/curve/rectangle/ellipse surface, the Appearance pen+brush state, the per-layer transform stack, clear, the clip box and the four drawImage* members are real: geometry is built as polylines, rasterised by this module's own area-coverage rasteriser (8 sub-rows per pixel row, even-odd fills, butt/miter strokes, SourceOver in straight alpha) and committed through plugin_api::layer; drawImage* accept a Layer source — which the reference's converter also answers — and sample it bilinearly. drawString/drawPathString/measureString*, the metafile record API, saveImage and GdiPlus.Image's decoder are registered with the reference's argument counts and a one-time warning (no text backend or encoder reachable from plugin code, no metafile format, and the decoder is not reachable so a GdiPlus.Image takes the reference's null-image path). Hatch/texture/gradient brushes, dashes and PenAlignmentInset are recognised and skipped with a one-time warning rather than painted wrongly. getColorRegionRects is real row runs.",
    install: |engine| engine.register_plugin(LayerExDrawPlugin),
};

pub struct LayerExDrawPlugin;

impl KrkrPlugin for LayerExDrawPlugin {
    fn name(&self) -> &str {
        "layerExDraw.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // A fresh registration starts from a clean drawing state per object,
        // the way a reloaded DLL starts from its constructor defaults.
        reset_object_state(runtime);
        install_gdi_plus(runtime);
        install_layer_ex_draw(runtime);
        Ok(())
    }
}

// ---------------------------------------------------------------- plugin state

/// The `LayerExDraw` instance fields (`LayerExDraw.hpp:213-257`) that survive
/// between calls: the two transform matrices, the two rendering hints and
/// `updateWhenDraw`. Kept in the host's per-layer extension slot
/// (`KrkrHost::layer_extension_or_insert_with`), which layer invalidation
/// prunes with the layer object.
#[derive(Debug)]
struct LayerState {
    /// `updateRect` calls `Layer.update` after a draw (`:937-946`); default
    /// true (`:956`).
    update_when_draw: bool,
    /// `LayerExDraw.hpp:230`, default `SmoothingModeAntiAlias` (`:954`).
    smoothing_mode: i64,
    /// `LayerExDraw.hpp:232`, default `TextRenderingHintAntiAlias` (`:954`).
    text_rendering_hint: i64,
    /// `transform` (`LayerExDraw.hpp:224`), the matrix `setTransform` and the
    /// `*Transform` methods maintain.
    transform: [f64; 6],
    /// `viewTransform` (`:225`).
    view_transform: [f64; 6],
}

impl Default for LayerState {
    fn default() -> Self {
        Self {
            update_when_draw: true,
            smoothing_mode: SMOOTHING_MODE_ANTIALIAS,
            text_rendering_hint: TEXT_RENDERING_HINT_ANTIALIAS,
            transform: MATRIX_IDENTITY,
            view_transform: MATRIX_IDENTITY,
        }
    }
}

/// Runs `mutate` on the layer's drawing state, creating it with the
/// constructor's defaults the first time the layer is seen.
fn with_layer_state<R>(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    mutate: impl FnOnce(&mut LayerState) -> R,
) -> R {
    // A method read off an object arrives as a self-bound closure; the state
    // belongs to the object it was read from.
    let layer = runtime.bound_this(layer).unwrap_or(layer);
    let state = runtime
        .host_mut()
        .layer_extension_or_insert_with(layer, || Mutex::new(LayerState::default()));
    let mut state = state.lock().expect("layerExDraw state");
    mutate(&mut state)
}

/// Clears the one-time warning log, so a re-registration logs again.
fn reset_object_state(runtime: &mut Runtime<KrkrHost>) {
    WARNED.with(|warned| warned.borrow_mut().clear());
    let _ = runtime;
}

thread_local! {
    /// Message keys whose one-time "not implemented" warning has already been
    /// logged, so a per-frame script does not flood the log.
    static WARNED: RefCell<BTreeSet<&'static str>> = const { RefCell::new(BTreeSet::new()) };
}

/// Logs `message` once per registration under the plugin's name.
fn warn_once(runtime: &mut Runtime<KrkrHost>, key: &'static str, message: &str) {
    let first = WARNED.with(|warned| warned.borrow_mut().insert(key));
    if first {
        runtime
            .host_mut()
            .log(&format!("layerExDraw.dll: {message}"));
    }
}

// ---------------------------------------------------------------- constants

/// `SmoothingModeAntiAlias` (`main.cpp:766`), the constructor's default.
const SMOOTHING_MODE_ANTIALIAS: i64 = 4;
/// `SmoothingModeHighQuality` (`main.cpp:764`), the other smoothing mode.
const SMOOTHING_MODE_HIGH_QUALITY: i64 = 2;
/// `TextRenderingHintAntiAlias` (`main.cpp:772`), the constructor's default.
const TEXT_RENDERING_HINT_ANTIALIAS: i64 = 4;

/// Sub-rows per pixel row in the coverage rasteriser: a pixel's coverage is
/// quantised to `1 / SUB_ROWS` vertically and computed exactly horizontally.
/// GDI+'s own antialiaser is a low-resolution supersampler too; 8 keeps a
/// full-layer fill cheap while making half-integer geometry exact.
const SUB_ROWS: i64 = 8;

/// Flatness tolerance for flattening arcs, ellipses and Bezier segments into
/// polylines, in layer pixels.
const FLATTEN_TOLERANCE: f64 = 0.25;

/// GDI+ `Pen`'s default miter limit.
const DEFAULT_MITER_LIMIT: f64 = 10.0;

/// Row-vector affine matrix, `[m11, m12, m21, m22, dx, dy]`, the layout the
/// `GdiPlus.Matrix` class uses (`LayerExDraw.hpp:224-226`). `p · a · b` maps
/// `p` through `a` first (the existing stub's convention, kept).
const MATRIX_IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `GdiPlus` enum members, transcribed in order from the `ENUM(...)` block of
/// the reference main.cpp (`main.cpp:601-773`, GDI+ numeric values).
const GDIPLUS_ENUM_CONSTANTS: &[(&str, i64)] = &[
    // Status
    ("Ok", 0),
    ("GenericError", 1),
    ("InvalidParameter", 2),
    ("OutOfMemory", 3),
    ("ObjectBusy", 4),
    ("InsufficientBuffer", 5),
    ("NotImplemented", 6),
    ("Win32Error", 7),
    ("WrongState", 8),
    ("Aborted", 9),
    ("FileNotFound", 10),
    ("ValueOverflow", 11),
    ("AccessDenied", 12),
    ("UnknownImageFormat", 13),
    ("FontFamilyNotFound", 14),
    ("FontStyleNotFound", 15),
    ("NotTrueTypeFont", 16),
    ("UnsupportedGdiplusVersion", 17),
    ("GdiplusNotInitialized", 18),
    ("PropertyNotFound", 19),
    ("PropertyNotSupported", 20),
    // FontStyle
    ("FontStyleRegular", 0),
    ("FontStyleBold", 1),
    ("FontStyleItalic", 2),
    ("FontStyleBoldItalic", 3),
    ("FontStyleUnderline", 4),
    ("FontStyleStrikeout", 8),
    // BrushType
    ("BrushTypeSolidColor", 0),
    ("BrushTypeHatchFill", 1),
    ("BrushTypeTextureFill", 2),
    ("BrushTypePathGradient", 3),
    ("BrushTypeLinearGradient", 4),
    // DashCap
    ("DashCapFlat", 0),
    ("DashCapRound", 2),
    ("DashCapTriangle", 3),
    // DashStyle
    ("DashStyleSolid", 0),
    ("DashStyleDash", 1),
    ("DashStyleDot", 2),
    ("DashStyleDashDot", 3),
    ("DashStyleDashDotDot", 4),
    // HatchStyle
    ("HatchStyleHorizontal", 0),
    ("HatchStyleVertical", 1),
    ("HatchStyleForwardDiagonal", 2),
    ("HatchStyleBackwardDiagonal", 3),
    ("HatchStyleCross", 4),
    ("HatchStyleDiagonalCross", 5),
    ("HatchStyle05Percent", 6),
    ("HatchStyle10Percent", 7),
    ("HatchStyle20Percent", 8),
    ("HatchStyle25Percent", 9),
    ("HatchStyle30Percent", 10),
    ("HatchStyle40Percent", 11),
    ("HatchStyle50Percent", 12),
    ("HatchStyle60Percent", 13),
    ("HatchStyle70Percent", 14),
    ("HatchStyle75Percent", 15),
    ("HatchStyle80Percent", 16),
    ("HatchStyle90Percent", 17),
    ("HatchStyleLightDownwardDiagonal", 18),
    ("HatchStyleLightUpwardDiagonal", 19),
    ("HatchStyleDarkDownwardDiagonal", 20),
    ("HatchStyleDarkUpwardDiagonal", 21),
    ("HatchStyleWideDownwardDiagonal", 22),
    ("HatchStyleWideUpwardDiagonal", 23),
    ("HatchStyleLightVertical", 24),
    ("HatchStyleLightHorizontal", 25),
    ("HatchStyleNarrowVertical", 26),
    ("HatchStyleNarrowHorizontal", 27),
    ("HatchStyleDarkVertical", 28),
    ("HatchStyleDarkHorizontal", 29),
    ("HatchStyleDashedDownwardDiagonal", 30),
    ("HatchStyleDashedUpwardDiagonal", 31),
    ("HatchStyleDashedHorizontal", 32),
    ("HatchStyleDashedVertical", 33),
    ("HatchStyleSmallConfetti", 34),
    ("HatchStyleLargeConfetti", 35),
    ("HatchStyleZigZag", 36),
    ("HatchStyleWave", 37),
    ("HatchStyleDiagonalBrick", 38),
    ("HatchStyleHorizontalBrick", 39),
    ("HatchStyleWeave", 40),
    ("HatchStylePlaid", 41),
    ("HatchStyleDivot", 42),
    ("HatchStyleDottedGrid", 43),
    ("HatchStyleDottedDiamond", 44),
    ("HatchStyleShingle", 45),
    ("HatchStyleTrellis", 46),
    ("HatchStyleSphere", 47),
    ("HatchStyleSmallGrid", 48),
    ("HatchStyleSmallCheckerBoard", 49),
    ("HatchStyleLargeCheckerBoard", 50),
    ("HatchStyleOutlinedDiamond", 51),
    ("HatchStyleSolidDiamond", 52),
    ("HatchStyleTotal", 53),
    ("HatchStyleLargeGrid", 4), // HatchStyleCross
    ("HatchStyleMin", 0),       // HatchStyleHorizontal
    ("HatchStyleMax", 52),      // HatchStyleSolidDiamond
    // LinearGradientMode
    ("LinearGradientModeHorizontal", 0),
    ("LinearGradientModeVertical", 1),
    ("LinearGradientModeForwardDiagonal", 2),
    ("LinearGradientModeBackwardDiagonal", 3),
    // LineCap
    ("LineCapFlat", 0),
    ("LineCapSquare", 1),
    ("LineCapRound", 2),
    ("LineCapTriangle", 3),
    ("LineCapNoAnchor", 16),
    ("LineCapSquareAnchor", 17),
    ("LineCapRoundAnchor", 18),
    ("LineCapDiamondAnchor", 19),
    ("LineCapArrowAnchor", 20),
    // LineJoin
    ("LineJoinMiter", 0),
    ("LineJoinBevel", 1),
    ("LineJoinRound", 2),
    ("LineJoinMiterClipped", 3),
    // PenAlignment
    ("PenAlignmentCenter", 0),
    ("PenAlignmentInset", 1),
    // WrapMode
    ("WrapModeTile", 0),
    ("WrapModeTileFlipX", 1),
    ("WrapModeTileFlipY", 2),
    ("WrapModeTileFlipXY", 3),
    ("WrapModeClamp", 4),
    // MatrixOrder
    ("MatrixOrderPrepend", 0),
    ("MatrixOrderAppend", 1),
    // ImageType
    ("ImageTypeUnknown", 0),
    ("ImageTypeBitmap", 1),
    ("ImageTypeMetafile", 2),
    // RotateFlipType
    ("RotateNoneFlipNone", 0),
    ("Rotate90FlipNone", 1),
    ("Rotate180FlipNone", 2),
    ("Rotate270FlipNone", 3),
    ("RotateNoneFlipX", 4),
    ("Rotate90FlipX", 5),
    ("Rotate180FlipX", 6),
    ("Rotate270FlipX", 7),
    ("RotateNoneFlipY", 6),
    ("Rotate90FlipY", 7),
    ("Rotate180FlipY", 4),
    ("Rotate270FlipY", 5),
    ("RotateNoneFlipXY", 2),
    ("Rotate90FlipXY", 3),
    ("Rotate180FlipXY", 0),
    ("Rotate270FlipXY", 1),
    // SmoothingMode
    ("SmoothingModeInvalid", -1),
    ("SmoothingModeDefault", 0),
    ("SmoothingModeHighSpeed", 1),
    ("SmoothingModeHighQuality", 2),
    ("SmoothingModeNone", 3),
    ("SmoothingModeAntiAlias", 4),
    // TextRenderingHint
    ("TextRenderingHintSystemDefault", 0),
    ("TextRenderingHintSingleBitPerPixelGridFit", 1),
    ("TextRenderingHintSingleBitPerPixel", 2),
    ("TextRenderingHintAntiAliasGridFit", 3),
    ("TextRenderingHintAntiAlias", 4),
    ("TextRenderingHintClearTypeGridFit", 5),
];

/// GDI+ `EncoderValue` constants attached to `Layer` (the reference's second
/// `ENUM(...)` block, `main.cpp:918-934`).
const ENCODER_VALUE_CONSTANTS: &[(&str, i64)] = &[
    ("EncoderValueCompressionLZW", 2),
    ("EncoderValueCompressionCCITT3", 3),
    ("EncoderValueCompressionCCITT4", 4),
    ("EncoderValueCompressionRle", 5),
    ("EncoderValueCompressionNone", 6),
    ("EncoderValueScanMethodInterlaced", 7),
    ("EncoderValueScanMethodNonInterlaced", 8),
    ("EncoderValueVersionGif87", 9),
    ("EncoderValueVersionGif89", 10),
    ("EncoderValueRenderProgressive", 11),
    ("EncoderValueRenderNonProgressive", 12),
    ("EncoderValueTransformRotate90", 13),
    ("EncoderValueTransformRotate180", 14),
    ("EncoderValueTransformRotate270", 15),
    ("EncoderValueTransformFlipHorizontal", 16),
    ("EncoderValueTransformFlipVertical", 17),
];

// ---------------------------------------------------------------- GdiPlus

/// `NCB_REGISTER_CLASS(GdiPlus)` (`main.cpp:599-789`): the namespace object
/// with its enums, two statics, and the seven subclasses. The geometry
/// classes (PointF/RectF/Matrix) are the placeholder's functional pure math,
/// with the reference converter's array/dictionary argument forms and the
/// GDI+ `MatrixOrder` arguments restored; Font/Image/Appearance/Path are the
/// plugin's own classes.
fn install_gdi_plus(runtime: &mut Runtime<KrkrHost>) {
    let gdi_plus = match runtime.global_member("GdiPlus") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "GdiPlus");
            runtime.set_global_member("GdiPlus", Variant::Object(handle));
            handle
        }
    };

    for &(name, value) in GDIPLUS_ENUM_CONSTANTS {
        runtime.set_object_member(gdi_plus, name, Variant::Integer(value));
    }

    runtime.register_object_native_with_arg_count(
        gdi_plus,
        "addPrivateFont",
        NativeArgCount::AtLeast(1),
        add_private_font,
    );
    runtime.register_object_native_with_arg_count(
        gdi_plus,
        "getFontList",
        NativeArgCount::AtLeast(1),
        get_font_list,
    );
    // ncbind puts a `finalize` on every class it registers (its instance
    // adaptor's destructor); the geometry classes below carry one too.
    runtime.register_object_native_with_arg_count(
        gdi_plus,
        "finalize",
        NativeArgCount::AtLeast(0),
        native_void,
    );

    let point_f = point_f_constructor(runtime);
    let rect_f = rect_f_constructor(runtime);
    let matrix = matrix_constructor(runtime);
    let image = image_constructor(runtime);
    let font = font_constructor(runtime);
    let appearance = appearance_constructor(runtime);
    let path = path_constructor(runtime);
    runtime.set_object_member(gdi_plus, "PointF", Variant::Object(point_f));
    runtime.set_object_member(gdi_plus, "RectF", Variant::Object(rect_f));
    runtime.set_object_member(gdi_plus, "Matrix", Variant::Object(matrix));
    runtime.set_object_member(gdi_plus, "Image", Variant::Object(image));
    runtime.set_object_member(gdi_plus, "Font", Variant::Object(font));
    runtime.set_object_member(gdi_plus, "Appearance", Variant::Object(appearance));
    runtime.set_object_member(gdi_plus, "Path", Variant::Object(path));
}

/// `GdiPlus.addPrivateFont`: the reference loads the font into a private
/// collection (`LayerExDraw.cpp:154-188`); this engine has no font-registration
/// backend, so the file is only checked to be readable.
fn add_private_font(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = arg_string(&args, 0);
    if runtime.host().read_binary_storage(&name).is_err() {
        // The reference throws `cannot open:%1` when the storage is missing
        // (`LayerExDraw.cpp:187`).
        return Err(TjsError::runtime(format!("cannot open:{name}")));
    }
    warn_once(
        runtime,
        "add-private-font",
        "GdiPlus.addPrivateFont only checks the storage; private fonts are not registered \
         (no font-registration backend in this engine)",
    );
    Ok(Variant::Void)
}

/// `GdiPlus.getFontList`: nothing enumerates a private collection here, and
/// the installed-font query has no backend either, so the reference's array is
/// always empty (`LayerExDraw.cpp:216-230`).
fn get_font_list(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_once(
        runtime,
        "font-list",
        "GdiPlus.getFontList returns an empty array (font enumeration is not implemented)",
    );
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

// ---------------------------------------------------------------- PointF

fn point_f_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor_with_arg_count(
        NativeArgCount::AtLeast(2),
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "PointF");
            install_point_f_members(runtime, instance);
            runtime.set_object_member(instance, "x", Variant::Real(arg_real(&args, 0)));
            runtime.set_object_member(instance, "y", Variant::Real(arg_real(&args, 1)));
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "PointF");
    install_point_f_members(runtime, handle);
    handle
}

fn install_point_f_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for name in ["x", "y"] {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, Variant::Real(0.0));
        }
    }
    // `NCB_METHOD(Equals)` (`layerExDraw/main.cpp:129`) is
    // `BOOL PointF::Equals(const PointF&) const`: one parameter, so a call
    // with none is ncbind's `TJS_E_BADPARAMCOUNT` (`ncbind.hpp:1186`).
    runtime.register_object_native_with_arg_count(
        handle,
        "Equals",
        NativeArgCount::AtLeast(1),
        point_f_equals,
    );
}

fn new_point_f(runtime: &mut Runtime<KrkrHost>, x: f64, y: f64) -> ObjectHandle {
    let handle = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(handle, "PointF");
    install_point_f_members(runtime, handle);
    runtime.set_object_member(handle, "x", Variant::Real(x));
    runtime.set_object_member(handle, "y", Variant::Real(y));
    handle
}

fn point_f_equals(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let equals = this_real(runtime, this_obj, "x") == variant_real(runtime, args.first(), "x")
        && this_real(runtime, this_obj, "y") == variant_real(runtime, args.first(), "y");
    Ok(Variant::Integer(i64::from(equals)))
}

// ---------------------------------------------------------------- RectF

const RECT_MEMBERS: [&str; 4] = ["x", "y", "width", "height"];

fn rect_f_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor_with_arg_count(
        NativeArgCount::AtLeast(4),
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "RectF");
            install_rect_f_members(runtime, instance);
            for (index, name) in RECT_MEMBERS.iter().enumerate() {
                runtime.set_object_member(instance, *name, Variant::Real(arg_real(&args, index)));
            }
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "RectF");
    install_rect_f_members(runtime, handle);
    handle
}

/// Builds a RectF-shaped object exactly the way the RectF constructor does;
/// used for every `GdiPlus.RectF` value returned from Layer draw methods.
fn new_rect_f(
    runtime: &mut Runtime<KrkrHost>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> ObjectHandle {
    let handle = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(handle, "RectF");
    install_rect_f_members(runtime, handle);
    store_rect(runtime, handle, [x, y, width, height]);
    handle
}

fn install_rect_f_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for name in RECT_MEMBERS {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, Variant::Real(0.0));
        }
    }
    register_readonly_property(
        runtime,
        handle,
        "left",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Real(this_real(runtime, this_obj, "x")))
        },
    );
    register_readonly_property(
        runtime,
        handle,
        "top",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Real(this_real(runtime, this_obj, "y")))
        },
    );
    register_readonly_property(
        runtime,
        handle,
        "right",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Real(
                this_real(runtime, this_obj, "x") + this_real(runtime, this_obj, "width"),
            ))
        },
    );
    register_readonly_property(
        runtime,
        handle,
        "bottom",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Real(
                this_real(runtime, this_obj, "y") + this_real(runtime, this_obj, "height"),
            ))
        },
    );
    register_readonly_property(
        runtime,
        handle,
        "location",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let x = this_real(runtime, this_obj, "x");
            let y = this_real(runtime, this_obj, "y");
            Ok(Variant::Object(new_point_f(runtime, x, y)))
        },
    );
    register_readonly_property(
        runtime,
        handle,
        "bounds",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let rect = this_rect(runtime, this_obj);
            Ok(Variant::Object(new_rect_f(
                runtime, rect[0], rect[1], rect[2], rect[3],
            )))
        },
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Clone",
        NativeArgCount::AtLeast(0),
        rect_clone,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Equals",
        NativeArgCount::AtLeast(1),
        rect_equals,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Inflate",
        NativeArgCount::AtLeast(2),
        rect_inflate,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "InflatePoint",
        NativeArgCount::AtLeast(1),
        rect_inflate_point,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "IntersectsWith",
        NativeArgCount::AtLeast(1),
        rect_intersects_with,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "IsEmptyArea",
        NativeArgCount::AtLeast(0),
        rect_is_empty_area,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Offset",
        NativeArgCount::AtLeast(2),
        rect_offset,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Union",
        NativeArgCount::AtLeast(3),
        rect_union,
    );
}

fn store_rect(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle, rect: [f64; 4]) {
    let handle = resolved_this(runtime, Some(handle)).unwrap_or(handle);
    for (index, name) in RECT_MEMBERS.iter().enumerate() {
        runtime.set_object_member(handle, *name, Variant::Real(rect[index]));
    }
}

fn this_rect(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> [f64; 4] {
    let mut rect = [0.0; 4];
    if let Some(handle) = resolved_this(runtime, this_obj) {
        for (index, name) in RECT_MEMBERS.iter().enumerate() {
            rect[index] = runtime.object_member(handle, name).to_real().unwrap_or(0.0);
        }
    }
    rect
}

/// `getRect` (`main.cpp:201-207`): a RectF instance, an array
/// `[x, y, width, height]`, or a dictionary with those members; anything else
/// is a zero RectF, as the converter's `T()` fallback.
fn variant_rect(runtime: &Runtime<KrkrHost>, value: Option<&Variant>) -> [f64; 4] {
    let Some(handle) = value.and_then(Variant::object_handle) else {
        return [0.0; 4];
    };
    if let Some(elements) = runtime.array_elements(handle) {
        return [
            element_real(elements, 0),
            element_real(elements, 1),
            element_real(elements, 2),
            element_real(elements, 3),
        ];
    }
    this_rect(runtime, Some(handle))
}

fn rect_clone(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let rect = this_rect(runtime, this_obj);
    Ok(Variant::Object(new_rect_f(
        runtime, rect[0], rect[1], rect[2], rect[3],
    )))
}

fn rect_equals(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let rect = this_rect(runtime, this_obj);
    let other = variant_rect(runtime, args.first());
    Ok(Variant::Integer(i64::from(rect == other)))
}

fn rect_inflate(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    inflate_this(runtime, this_obj, arg_real(&args, 0), arg_real(&args, 1));
    Ok(Variant::Void)
}

fn rect_inflate_point(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dx = variant_real(runtime, args.first(), "x");
    let dy = variant_real(runtime, args.first(), "y");
    inflate_this(runtime, this_obj, dx, dy);
    Ok(Variant::Void)
}

fn inflate_this(runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, dx: f64, dy: f64) {
    if let Some(this) = this_obj {
        let rect = this_rect(runtime, Some(this));
        store_rect(
            runtime,
            this,
            [
                rect[0] - dx,
                rect[1] - dy,
                rect[2] + 2.0 * dx,
                rect[3] + 2.0 * dy,
            ],
        );
    }
}

fn rect_intersects_with(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let rect = this_rect(runtime, this_obj);
    let other = variant_rect(runtime, args.first());
    let intersects = rect[0] < other[0] + other[2]
        && rect[1] < other[1] + other[3]
        && rect[0] + rect[2] > other[0]
        && rect[1] + rect[3] > other[1];
    Ok(Variant::Integer(i64::from(intersects)))
}

fn rect_is_empty_area(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let rect = this_rect(runtime, this_obj);
    Ok(Variant::Integer(i64::from(
        rect[2] <= 0.0 || rect[3] <= 0.0,
    )))
}

fn rect_offset(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        let rect = this_rect(runtime, Some(this));
        store_rect(
            runtime,
            this,
            [
                rect[0] + arg_real(&args, 0),
                rect[1] + arg_real(&args, 1),
                rect[2],
                rect[3],
            ],
        );
    }
    Ok(Variant::Void)
}

/// GDI+'s `RectF` exposes `Union` as the static
/// `BOOL Union(RectF& c, const RectF& a, const RectF& b)` — the single
/// `Union` overload of `gdiplustypes.h`, which the reference's plain
/// `NCB_METHOD(Union)` binds (`main.cpp:198`). A TJS call therefore spells
/// out all three rectangles: `GdiPlus.RectF.Union(dst, a, b)` stores the
/// union of `a` and `b` in the *first* argument and answers whether the
/// result is non-empty. (The previous stub's 3-argument form had the same
/// shape; this diff's 2-argument instance form did not exist in the
/// reference.)
fn rect_union(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let a = variant_rect(runtime, args.get(1));
    let b = variant_rect(runtime, args.get(2));
    let left = a[0].min(b[0]);
    let top = a[1].min(b[1]);
    let right = (a[0] + a[2]).max(b[0] + b[2]);
    let bottom = (a[1] + a[3]).max(b[1] + b[3]);
    let (width, height) = (right - left, bottom - top);
    if let Some(destination) = args.first().and_then(Variant::object_handle) {
        store_rect(runtime, destination, [left, top, width, height]);
    }
    Ok(Variant::Integer(i64::from(width > 0.0 && height > 0.0)))
}

// ---------------------------------------------------------------- Matrix

const MATRIX_MEMBERS: [&str; 6] = ["m11", "m12", "m21", "m22", "dx", "dy"];

fn matrix_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let elements = match args.len() {
                0 => MATRIX_IDENTITY,
                // The factory's 2-argument form is `Matrix(RectF, PointF)`
                // (`main.cpp:371-394`); the port keeps rejecting it rather
                // than guess GDI+'s rect-to-point transform.
                6 => [
                    arg_real(&args, 0),
                    arg_real(&args, 1),
                    arg_real(&args, 2),
                    arg_real(&args, 3),
                    arg_real(&args, 4),
                    arg_real(&args, 5),
                ],
                _ => return Err(TjsError::runtime("invalid parameter")),
            };
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Matrix");
            install_matrix_members(runtime, instance);
            store_matrix(runtime, instance, elements);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Matrix");
    install_matrix_members(runtime, handle);
    handle
}

fn install_matrix_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for (index, name) in MATRIX_MEMBERS.iter().enumerate() {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, *name, Variant::Real(MATRIX_IDENTITY[index]));
        }
    }
    runtime.register_object_native_with_arg_count(
        handle,
        "OffsetX",
        NativeArgCount::AtLeast(0),
        matrix_offset_x,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "OffsetY",
        NativeArgCount::AtLeast(0),
        matrix_offset_y,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Equals",
        NativeArgCount::AtLeast(1),
        matrix_equals,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "SetElements",
        NativeArgCount::AtLeast(6),
        matrix_set_elements,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "GetLastStatus",
        NativeArgCount::AtLeast(0),
        zero,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Invert",
        NativeArgCount::AtLeast(0),
        matrix_invert,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "IsIdentity",
        NativeArgCount::AtLeast(0),
        matrix_is_identity,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "IsInvertible",
        NativeArgCount::AtLeast(0),
        matrix_is_invertible,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Multiply",
        NativeArgCount::AtLeast(2),
        matrix_multiply,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Reset",
        NativeArgCount::AtLeast(0),
        matrix_reset,
    );
    // GDI+'s `MatrixOrder` is a declared parameter of every one of these
    // (`Status Rotate(REAL, MatrixOrder = ...)`, mingw-w64/ReactOS
    // `gdiplustypes.h`), and ncbind counts declared parameters, defaults
    // included (`ncbind.hpp:1186`), so the reference requires them too.
    runtime.register_object_native_with_arg_count(
        handle,
        "Rotate",
        NativeArgCount::AtLeast(2),
        matrix_rotate,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "RotateAt",
        NativeArgCount::AtLeast(3),
        matrix_rotate_at,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Scale",
        NativeArgCount::AtLeast(3),
        matrix_scale,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Shear",
        NativeArgCount::AtLeast(3),
        matrix_shear,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Translate",
        NativeArgCount::AtLeast(3),
        matrix_translate,
    );
}

fn store_matrix(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle, matrix: [f64; 6]) {
    let handle = resolved_this(runtime, Some(handle)).unwrap_or(handle);
    for (index, name) in MATRIX_MEMBERS.iter().enumerate() {
        runtime.set_object_member(handle, *name, Variant::Real(matrix[index]));
    }
}

fn this_matrix(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> [f64; 6] {
    let mut matrix = [0.0; 6];
    if let Some(handle) = resolved_this(runtime, this_obj) {
        for (index, name) in MATRIX_MEMBERS.iter().enumerate() {
            matrix[index] = runtime.object_member(handle, name).to_real().unwrap_or(0.0);
        }
    }
    matrix
}

/// `getMatrix` (`main.cpp:338-368`): a Matrix instance, an array of six
/// reals, or a dictionary with the member names; anything else is `None` (the
/// converter's `NULL`, which the Layer transform setters treat as a reset).
fn variant_matrix(runtime: &Runtime<KrkrHost>, value: Option<&Variant>) -> Option<[f64; 6]> {
    let handle = value.and_then(Variant::object_handle)?;
    if let Some(elements) = runtime.array_elements(handle) {
        if elements.len() < 6 {
            return None;
        }
        let mut matrix = [0.0; 6];
        for (index, element) in matrix.iter_mut().enumerate() {
            *element = element_real(elements, index);
        }
        return Some(matrix);
    }
    if !matches!(runtime.object_member(handle, "m11"), Variant::Void) {
        return Some(this_matrix(runtime, Some(handle)));
    }
    None
}

/// Row-vector affine multiply (`p · a · b`): applies `a` first, then `b`.
/// Elements are laid out as [m11, m12, m21, m22, dx, dy], matching GDI+.
fn matrix_mul(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

/// GDI+ `MatrixOrder`: `Prepend` (the default) applies `transform` before the
/// current matrix, `Append` after it (`MatrixOrderAppend == 1`,
/// `main.cpp:738`).
fn matrix_order_is_append(args: &[Variant], index: usize) -> bool {
    arg_int(args, index) == 1
}

/// Applies `transform` to `this` under the GDI+ `MatrixOrder` argument.
fn apply_matrix_order(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    transform: [f64; 6],
    append: bool,
) {
    if let Some(this) = this_obj {
        let matrix = this_matrix(runtime, Some(this));
        let result = if append {
            matrix_mul(matrix, transform)
        } else {
            matrix_mul(transform, matrix)
        };
        store_matrix(runtime, this, result);
    }
}

fn matrix_offset_x(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Real(this_matrix(runtime, this_obj)[4]))
}

fn matrix_offset_y(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Real(this_matrix(runtime, this_obj)[5]))
}

fn matrix_equals(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let matrix = this_matrix(runtime, this_obj);
    let other = variant_matrix(runtime, args.first()).unwrap_or([f64::NAN; 6]);
    Ok(Variant::Integer(i64::from(matrix == other)))
}

fn matrix_set_elements(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        store_matrix(
            runtime,
            this,
            [
                arg_real(&args, 0),
                arg_real(&args, 1),
                arg_real(&args, 2),
                arg_real(&args, 3),
                arg_real(&args, 4),
                arg_real(&args, 5),
            ],
        );
    }
    Ok(Variant::Void)
}

fn matrix_invert(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        let m = this_matrix(runtime, Some(this));
        let det = m[0] * m[3] - m[1] * m[2];
        if det != 0.0 {
            store_matrix(
                runtime,
                this,
                [
                    m[3] / det,
                    -m[1] / det,
                    -m[2] / det,
                    m[0] / det,
                    (m[2] * m[5] - m[3] * m[4]) / det,
                    (m[1] * m[4] - m[0] * m[5]) / det,
                ],
            );
        }
    }
    Ok(Variant::Void)
}

fn matrix_is_identity(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(i64::from(
        this_matrix(runtime, this_obj) == MATRIX_IDENTITY,
    )))
}

fn matrix_is_invertible(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let m = this_matrix(runtime, this_obj);
    Ok(Variant::Integer(i64::from(
        m[0] * m[3] - m[1] * m[2] != 0.0,
    )))
}

fn matrix_multiply(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(other) = variant_matrix(runtime, args.first()) {
        apply_matrix_order(runtime, this_obj, other, matrix_order_is_append(&args, 1));
    }
    Ok(Variant::Void)
}

fn matrix_reset(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        store_matrix(runtime, this, MATRIX_IDENTITY);
    }
    Ok(Variant::Void)
}

fn matrix_rotate(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (sin, cos) = arg_real(&args, 0).to_radians().sin_cos();
    apply_matrix_order(
        runtime,
        this_obj,
        [cos, sin, -sin, cos, 0.0, 0.0],
        matrix_order_is_append(&args, 1),
    );
    Ok(Variant::Void)
}

fn matrix_rotate_at(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (sin, cos) = arg_real(&args, 0).to_radians().sin_cos();
    let cx = variant_real(runtime, args.get(1), "x");
    let cy = variant_real(runtime, args.get(1), "y");
    // Rotation about (cx, cy): translate to the origin, rotate, translate
    // back; the combined transform is then ordered like the reference.
    let rotate_at = matrix_mul(
        matrix_mul(
            [1.0, 0.0, 0.0, 1.0, -cx, -cy],
            [cos, sin, -sin, cos, 0.0, 0.0],
        ),
        [1.0, 0.0, 0.0, 1.0, cx, cy],
    );
    apply_matrix_order(
        runtime,
        this_obj,
        rotate_at,
        matrix_order_is_append(&args, 2),
    );
    Ok(Variant::Void)
}

fn matrix_scale(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let transform = [arg_real(&args, 0), 0.0, 0.0, arg_real(&args, 1), 0.0, 0.0];
    apply_matrix_order(
        runtime,
        this_obj,
        transform,
        matrix_order_is_append(&args, 2),
    );
    Ok(Variant::Void)
}

fn matrix_shear(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let transform = [1.0, arg_real(&args, 1), arg_real(&args, 0), 1.0, 0.0, 0.0];
    apply_matrix_order(
        runtime,
        this_obj,
        transform,
        matrix_order_is_append(&args, 2),
    );
    Ok(Variant::Void)
}

fn matrix_translate(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let transform = [1.0, 0.0, 0.0, 1.0, arg_real(&args, 0), arg_real(&args, 1)];
    apply_matrix_order(
        runtime,
        this_obj,
        transform,
        matrix_order_is_append(&args, 2),
    );
    Ok(Variant::Void)
}

// ---------------------------------------------------------------- Image

/// `NCB_REGISTER_GDIP_SUBCLASS2(Image, ImageConvertor)` (`main.cpp:513-547`):
/// the reference wraps a GDI+ `Image` decoded from a storage name
/// (`ImageFactory`, `main.cpp:448-464`). This engine exposes no decoder to
/// plugin code, so the object validates the storage and carries no pixels:
/// `drawImage*` of such an object draws nothing and warns, and `GetBounds`
/// answers an empty RectF.
fn image_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Image");
            install_image_members(runtime, instance);
            match args.first() {
                None => {}
                Some(Variant::String(name)) => load_image(runtime, instance, name)?,
                Some(_) => return Err(TjsError::runtime("invalid parameter")),
            }
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Image");
    install_image_members(runtime, handle);
    handle
}

/// The reference decodes through GDI+ (`loadImage`, `LayerExDraw.cpp:53-99`)
/// and throws `cannot open:%1` when the storage is missing
/// (`main.cpp:460`); the port reproduces the error and keeps a 0x0 image.
fn load_image(runtime: &mut Runtime<KrkrHost>, _instance: ObjectHandle, name: &str) -> Result<()> {
    if runtime.host().read_binary_storage(name).is_err() {
        return Err(TjsError::runtime(format!("cannot open:{name}")));
    }
    warn_once(
        runtime,
        "image-decode",
        "GdiPlus.Image carries no pixels: this engine's decoder is not reachable from plugin \
         code, so drawImage*/GetBounds have nothing to sample",
    );
    Ok(())
}

fn install_image_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    runtime.register_object_native_with_arg_count(
        handle,
        "load",
        NativeArgCount::AtLeast(1),
        image_load,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "Clone",
        NativeArgCount::AtLeast(0),
        image_clone,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "GetBounds",
        NativeArgCount::AtLeast(0),
        image_get_bounds,
    );
    for name in [
        "GetFlags",
        "GetHeight",
        "GetLastStatus",
        "GetPixelFormat",
        "GetType",
        "GetWidth",
    ] {
        runtime.register_object_native_with_arg_count(
            handle,
            name,
            NativeArgCount::AtLeast(0),
            zero,
        );
    }
    for name in ["GetHorizontalResolution", "GetVerticalResolution"] {
        runtime.register_object_native_with_arg_count(
            handle,
            name,
            NativeArgCount::AtLeast(0),
            real_zero,
        );
    }
    runtime.register_object_native_with_arg_count(
        handle,
        "RotateFlip",
        NativeArgCount::AtLeast(1),
        native_void,
    );
}

fn image_load(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = arg_string(&args, 0);
    if let Some(this) = this_obj {
        load_image(runtime, this, &name)?;
    }
    Ok(Variant::Void)
}

/// `ImageClone` (`main.cpp:476-493`) clones the GDI+ image; a data-less image
/// clones to another data-less image.
fn image_clone(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let instance = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(instance, "Image");
    install_image_members(runtime, instance);
    Ok(Variant::Object(instance))
}

/// `ImageBounds` (`main.cpp:495-511`) asks the GDI+ image for its bounds,
/// converted from its unit to pixels (`getBounds`, `LayerExDraw.cpp:101-144`).
fn image_get_bounds(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_once(
        runtime,
        "image-bounds",
        "GdiPlus.Image.GetBounds answers an empty RectF: images are not decoded",
    );
    Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)))
}

// ---------------------------------------------------------------- Font

/// `NCB_SUBCLASS(Font, FontInfo)` (`main.cpp:786`): the reference's
/// `FontInfo` carries a GDI+ `FontFamily` and Win32 outline metrics
/// (`LayerExDraw.cpp:236-442`). The port keeps the members and the
/// `forceSelfPathDraw` flag; every metric is zero (no font backend is
/// consulted) and the metric reads warn once.
fn font_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor_with_arg_count(
        NativeArgCount::AtLeast(3),
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Font");
            install_font_members(runtime, instance);
            runtime.set_object_member(
                instance,
                "familyName",
                Variant::String(arg_string(&args, 0)),
            );
            runtime.set_object_member(instance, "emSize", Variant::Real(arg_real(&args, 1)));
            runtime.set_object_member(instance, "style", Variant::Integer(arg_int(&args, 2)));
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Font");
    install_font_members(runtime, handle);
    handle
}

fn install_font_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for (name, value) in [
        ("familyName", Variant::String(String::new())),
        ("emSize", Variant::Real(12.0)),
        ("style", Variant::Integer(0)),
        ("forceSelfPathDraw", Variant::Integer(0)),
    ] {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, value);
        }
    }
    for name in [
        "ascent",
        "descent",
        "ascentLeading",
        "descentLeading",
        "lineSpacing",
    ] {
        register_readonly_property(runtime, handle, name, font_metric_getter);
    }
}

/// `FontInfo::getAscent` & co. (`LayerExDraw.cpp:407-442`) run Win32 outline
/// metrics; there is no equivalent here, so the members read 0 with a
/// one-time warning.
fn font_metric_getter(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    warn_once(
        runtime,
        "font-metrics",
        "Font metrics (ascent/descent/lineSpacing) are 0: no font backend is consulted",
    );
    Ok(Variant::Real(0.0))
}

// ---------------------------------------------------------------- Appearance

/// One entry of the reference's `Appearance::drawInfos` (`LayerExDraw.hpp:101-139`):
/// a solid brush (fill) or a pen (stroke) with the entry's offset. The port
/// keeps the reference's two paint kinds for the solid-colour case only;
/// hatch/texture/path-gradient/linear-gradient brushes are skipped with a
/// one-time warning instead of drawing a wrong colour.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Paint {
    Fill(u32),
    Stroke(Pen),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pen {
    colour: u32,
    width: f64,
    start_cap: Cap,
    end_cap: Cap,
    join: Join,
    miter_limit: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cap {
    Flat,
    Square,
    Round,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Join {
    Miter,
    Bevel,
    Round,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DrawInfo {
    paint: Paint,
    ox: f64,
    oy: f64,
}

/// `NCB_REGISTER_SUBCLASS(Appearance)` (`main.cpp:566-571`): `clear`,
/// `addBrush`, `addPen`. The draw list lives in the object's `__drawInfos`
/// member as a flat Real array, `[kind, colour, width, startCap, endCap,
/// join, miterLimit, ox, oy]` per entry — state that dies with the object
/// instead of a handle-keyed side table.
fn appearance_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Appearance");
            install_appearance_members(runtime, instance);
            store_draw_infos(runtime, instance, &[]);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Appearance");
    install_appearance_members(runtime, handle);
    handle
}

fn install_appearance_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    runtime.register_object_native_with_arg_count(
        handle,
        "clear",
        NativeArgCount::AtLeast(0),
        appearance_clear,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "addBrush",
        NativeArgCount::AtLeast(3),
        appearance_add_brush,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "addPen",
        NativeArgCount::AtLeast(4),
        appearance_add_pen,
    );
}

fn appearance_clear(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = appearance_this(this_obj) {
        store_draw_infos(runtime, this, &[]);
    }
    Ok(Variant::Void)
}

/// `Appearance::addBrush(colorOrBrush, ox, oy)` (`LayerExDraw.cpp:799-803`).
fn appearance_add_brush(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = appearance_this(this_obj) else {
        return Ok(Variant::Void);
    };
    let mut infos = read_draw_infos(runtime, this);
    if let Some(brush) = resolve_brush(runtime, args.first())? {
        infos.push(DrawInfo {
            paint: Paint::Fill(brush),
            ox: arg_real(&args, 1),
            oy: arg_real(&args, 2),
        });
    }
    store_draw_infos(runtime, this, &infos);
    Ok(Variant::Void)
}

/// `Appearance::addPen(colorOrBrush, widthOrOption, ox, oy)`
/// (`LayerExDraw.cpp:812-903`): the 1.0 default width, the option
/// dictionary's `width`, the caps, the join and the miter limit are honoured;
/// dashes, compound arrays, custom (dictionary) caps and `PenAlignmentInset`
/// are warned no-ops.
fn appearance_add_pen(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = appearance_this(this_obj) else {
        return Ok(Variant::Void);
    };
    let mut infos = read_draw_infos(runtime, this);

    let colour = match resolve_brush(runtime, args.first())? {
        Some(colour) => colour,
        None => {
            store_draw_infos(runtime, this, &infos);
            return Ok(Variant::Void);
        }
    };

    let mut pen = Pen {
        colour,
        width: 1.0,
        start_cap: Cap::Flat,
        end_cap: Cap::Flat,
        join: Join::Miter,
        miter_limit: DEFAULT_MITER_LIMIT,
    };

    let Some(option) = args.get(1) else {
        infos.push(DrawInfo {
            paint: Paint::Stroke(pen),
            ox: arg_real(&args, 2),
            oy: arg_real(&args, 3),
        });
        store_draw_infos(runtime, this, &infos);
        return Ok(Variant::Void);
    };

    if let Some(handle) = option.object_handle() {
        pen.width = object_real(runtime, handle, "width").unwrap_or(1.0);
        if let Some(alignment) = object_real(runtime, handle, "alignment")
            && alignment != 0.0
        {
            warn_once(
                runtime,
                "pen-alignment",
                "PenAlignmentInset is not implemented; the pen stays centred",
            );
        }
        let dash_style = runtime.object_member(handle, "dashStyle");
        if !matches!(dash_style, Variant::Void)
            && dash_style
                .to_integer()
                .map(|style| style != 0)
                .unwrap_or(true)
        {
            warn_once(
                runtime,
                "pen-dash",
                "pen dashes are not implemented in this port: dashStyle/dashOffset/\
                 dashPattern are ignored and the path is drawn solid",
            );
        }
        if object_has_member(runtime, handle, "dashPattern")
            || object_has_member(runtime, handle, "dashOffset")
        {
            warn_once(
                runtime,
                "pen-dash",
                "pen dashes are not implemented in this port: dashStyle/dashOffset/\
                 dashPattern are ignored and the path is drawn solid",
            );
        }
        // `dashCap` only shapes dash ends; with dashes unimplemented it has
        // nothing to affect, so say so instead of dropping it silently.
        if let Some(cap) = object_real(runtime, handle, "dashCap")
            && cap != 0.0
        {
            warn_once(
                runtime,
                "pen-dash-cap",
                "pen dashCap is ignored: dashes are not implemented in this port, and with a \
                 solid pen GDI+ has no dash ends for it to shape either",
            );
        }
        if object_has_member(runtime, handle, "compoundArray") {
            warn_once(
                runtime,
                "pen-compound",
                "Pen.SetCompoundArray is not implemented: the pen draws a single stroke",
            );
        }
        if let Some(cap) = line_cap(runtime, handle, "startCap") {
            pen.start_cap = cap;
        }
        if let Some(cap) = line_cap(runtime, handle, "endCap") {
            pen.end_cap = cap;
        }
        if let Some(join) = object_real(runtime, handle, "lineJoin") {
            pen.join = match join as i64 {
                1 => Join::Bevel,
                2 => Join::Round,
                _ => Join::Miter,
            };
        }
        if let Some(limit) = object_real(runtime, handle, "miterLimit") {
            pen.miter_limit = limit;
        }
    } else {
        pen.width = option.to_real().unwrap_or(0.0);
    }

    infos.push(DrawInfo {
        paint: Paint::Stroke(pen),
        ox: arg_real(&args, 2),
        oy: arg_real(&args, 3),
    });
    store_draw_infos(runtime, this, &infos);
    Ok(Variant::Void)
}

/// The reference's `getLineCap` (`LayerExDraw.cpp:905-930`): a numeric
/// `LineCap`, or a dictionary naming an `AdjustableArrowCap` — which has no
/// equivalent here and is warned about.
fn line_cap(runtime: &mut Runtime<KrkrHost>, options: ObjectHandle, name: &str) -> Option<Cap> {
    let value = runtime.object_member(options, name);
    match value {
        Variant::Integer(value) => match value {
            0 => Some(Cap::Flat),
            1 => Some(Cap::Square),
            2 => Some(Cap::Round),
            _ => {
                warn_once(
                    runtime,
                    "pen-cap",
                    "anchor/arrow line caps are not implemented; the cap is drawn flat",
                );
                None
            }
        },
        Variant::Real(value) if value.is_finite() => match value as i64 {
            0 => Some(Cap::Flat),
            1 => Some(Cap::Square),
            2 => Some(Cap::Round),
            _ => {
                warn_once(
                    runtime,
                    "pen-cap",
                    "anchor/arrow line caps are not implemented; the cap is drawn flat",
                );
                None
            }
        },
        Variant::Object(_) => {
            warn_once(
                runtime,
                "pen-custom-cap",
                "custom line caps (AdjustableArrowCap dictionaries) are not implemented; the \
                 cap is drawn flat",
            );
            None
        }
        _ => None,
    }
}

/// `createBrush` (`LayerExDraw.cpp:668-791`): a non-object argument is a
/// solid ARGB colour, a dictionary is dispatched on its `type` member
/// (default `BrushTypeSolidColor`), and an out-of-range type throws
/// `invalid brush type` exactly where the reference does (`:786`).
///
/// The four non-solid types (hatch, texture, path gradient, linear gradient)
/// are **not implemented in this port**, which carries the solid-colour
/// rasteriser only; the entry is skipped with a one-time warning rather than
/// painted with a wrong colour. (Their GDI+ construction needs hatch patterns
/// and gradient-path sampling, not an engine facility, so this is scope
/// rather than a missing capability.)
fn resolve_brush(runtime: &mut Runtime<KrkrHost>, value: Option<&Variant>) -> Result<Option<u32>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if let Some(handle) = value.object_handle() {
        let brush_type = object_real(runtime, handle, "type").unwrap_or(0.0) as i64;
        match brush_type {
            0 => Ok(Some(
                object_real(runtime, handle, "color")
                    .map(|colour| colour as i64 as u32)
                    .unwrap_or(0xffff_ffff),
            )),
            1..=4 => {
                warn_once(
                    runtime,
                    "brush-type",
                    "hatch/texture/path-gradient/linear-gradient brushes are not implemented in \
                     this port (solid colours only); their draw entries are skipped",
                );
                Ok(None)
            }
            _ => Err(TjsError::runtime("invalid brush type")),
        }
    } else {
        Ok(Some(value.to_integer().unwrap_or(0) as u32))
    }
}

/// The `this` an Appearance method acts on: the bound object, if it is one of
/// ours.
fn appearance_this(this_obj: Option<ObjectHandle>) -> Option<ObjectHandle> {
    this_obj
}

/// Reads the object's flat `__drawInfos` number array, empty when the member
/// is missing (in which case the object is not one of our Appearance
/// objects).
fn read_draw_infos(runtime: &Runtime<KrkrHost>, owner: ObjectHandle) -> Vec<DrawInfo> {
    appearance_infos_of(runtime, owner).unwrap_or_default()
}

/// Decodes the flat `[kind, colour, width, startCap, endCap, join,
/// miterLimit, ox, oy]` entries of `__drawInfos`.
fn decode_draw_infos(values: &[f64]) -> Vec<DrawInfo> {
    values
        .chunks_exact(APPEARANCE_STRIDE)
        .filter_map(|entry| match entry[0] as i64 {
            1 => Some(DrawInfo {
                paint: Paint::Stroke(Pen {
                    colour: entry[1] as i64 as u32,
                    width: entry[2],
                    start_cap: cap_from(entry[3]),
                    end_cap: cap_from(entry[4]),
                    join: match entry[5] as i64 {
                        1 => Join::Bevel,
                        2 => Join::Round,
                        _ => Join::Miter,
                    },
                    miter_limit: entry[6],
                }),
                ox: entry[7],
                oy: entry[8],
            }),
            0 => Some(DrawInfo {
                paint: Paint::Fill(entry[1] as i64 as u32),
                ox: entry[7],
                oy: entry[8],
            }),
            _ => None,
        })
        .collect()
}

fn store_draw_infos(runtime: &mut Runtime<KrkrHost>, owner: ObjectHandle, infos: &[DrawInfo]) {
    let owner = resolved_this(runtime, Some(owner)).unwrap_or(owner);
    let mut values = Vec::with_capacity(infos.len() * APPEARANCE_STRIDE);
    for info in infos {
        let (kind, colour, width, start_cap, end_cap, join, miter_limit) = match info.paint {
            Paint::Fill(colour) => (0.0, colour as f64, 0.0, 0.0, 0.0, 0.0, 0.0),
            Paint::Stroke(pen) => (
                1.0,
                pen.colour as f64,
                pen.width,
                cap_index(pen.start_cap),
                cap_index(pen.end_cap),
                match pen.join {
                    Join::Miter => 0.0,
                    Join::Bevel => 1.0,
                    Join::Round => 2.0,
                },
                pen.miter_limit,
            ),
        };
        values.extend_from_slice(&[
            kind,
            colour,
            width,
            start_cap,
            end_cap,
            join,
            miter_limit,
            info.ox,
            info.oy,
        ]);
    }
    store_number_member(runtime, owner, "__drawInfos", &values);
}

const APPEARANCE_STRIDE: usize = 9;

fn cap_index(cap: Cap) -> f64 {
    match cap {
        Cap::Flat => 0.0,
        Cap::Square => 1.0,
        Cap::Round => 2.0,
    }
}

fn cap_from(value: f64) -> Cap {
    match value as i64 {
        1 => Cap::Square,
        2 => Cap::Round,
        _ => Cap::Flat,
    }
}

// ---------------------------------------------------------------- Path

/// A point in layer (device) space.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// This point mapped through the row-vector matrix
    /// `[m11, m12, m21, m22, dx, dy]`.
    fn transform(self, m: [f64; 6]) -> Self {
        Self {
            x: self.x * m[0] + self.y * m[2] + m[4],
            y: self.x * m[1] + self.y * m[3] + m[5],
        }
    }

    fn lerp(self, other: Self, t: f64) -> Self {
        Self {
            x: self.x + (other.x - self.x) * t,
            y: self.y + (other.y - self.y) * t,
        }
    }
}

/// One `GraphicsPath` figure: its points, and whether `CloseFigure` closed it.
/// A figure of one or zero points contributes nothing to a stroke and is
/// invisible to a fill.
#[derive(Clone, Debug, PartialEq)]
struct Figure {
    points: Vec<Point>,
    closed: bool,
}

impl Figure {
    fn open(points: Vec<Point>) -> Self {
        Self {
            points,
            closed: false,
        }
    }

    fn closed(points: Vec<Point>) -> Self {
        Self {
            points,
            closed: true,
        }
    }

    fn transform(&self, m: [f64; 6]) -> Self {
        Self {
            points: self.points.iter().map(|point| point.transform(m)).collect(),
            closed: self.closed,
        }
    }
}

/// Appends a point to the path's current figure, starting one when the path
/// is empty or the last figure was closed. GDI+ appends *points* to the
/// current figure, so two consecutive `AddLine`s connect through the shared
/// current point (`LayerExDraw.cpp:1303-1410` and the Add* family).
fn figure_add_point(figures: &mut Vec<Figure>, point: Point) {
    match figures.last_mut() {
        Some(figure) if !figure.closed => figure.points.push(point),
        _ => figures.push(Figure::open(vec![point])),
    }
}

/// `Path::startFigure` (`Path.cpp:17-21`): starts a new figure without
/// closing the current one.
fn figure_start(figures: &mut Vec<Figure>) {
    figures.push(Figure::open(Vec::new()));
}

/// `Path::closeFigure` (`Path.cpp:27-31`): closes the current figure; a later
/// point starts a new one.
///
/// The `Vec` parameter is the `path_edit` closure signature the member table
/// shares with the figure-adding methods.
#[allow(clippy::ptr_arg)]
fn figure_close(figures: &mut Vec<Figure>) {
    if let Some(figure) = figures.last_mut()
        && !figure.closed
        && !figure.points.is_empty()
    {
        figure.closed = true;
    }
}

/// A rectangle as GDI+ `AddRectangle` builds it: a closed figure of the four
/// corners, in the order `(x, y) (x+w, y) (x+w, y+h) (x, y+h)`.
fn rectangle_figure(x: f64, y: f64, width: f64, height: f64) -> Figure {
    Figure::closed(vec![
        Point::new(x, y),
        Point::new(x + width, y),
        Point::new(x + width, y + height),
        Point::new(x, y + height),
    ])
}

/// Samples one ellipse arc into a polyline: `count + 1` points from
/// `start_radians` over `sweep_radians`, clockwise in the y-down screen frame
/// as GDI+ measures its angles (`AddArc`, `LayerExDraw.cpp:1286`).
fn arc_points(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    start_angle: f64,
    sweep_angle: f64,
) -> Vec<Point> {
    let cx = x + width / 2.0;
    let cy = y + height / 2.0;
    let rx = width / 2.0;
    let ry = height / 2.0;
    let sweep = sweep_angle.to_radians();
    let start = start_angle.to_radians();
    let count = arc_steps(rx, ry, sweep);
    let mut points = Vec::with_capacity(count + 1);
    for step in 0..=count {
        let t = start + sweep * (step as f64) / (count as f64);
        points.push(Point::new(cx + rx * t.cos(), cy + ry * t.sin()));
    }
    points
}

/// Segment count for an arc: one segment per [`FLATTEN_TOLERANCE`]-scale
/// chord on the larger radius, clamped so a full circle of a huge ellipse
/// stays bounded and a degenerate one still produces points.
fn arc_steps(rx: f64, ry: f64, sweep: f64) -> usize {
    let radius = rx.abs().max(ry.abs());
    let per_radian = if radius <= FLATTEN_TOLERANCE {
        4.0
    } else {
        (radius / FLATTEN_TOLERANCE).sqrt().max(1.0)
    };
    let steps = (sweep.abs() * per_radian).ceil();
    (steps as usize).clamp(1, 1024)
}

/// Flattens a cubic Bezier segment (the form GDI+ stores `AddBezier`,
/// `AddBeziers` and the cardinal splines as) by recursive midpoint
/// subdivision until the control polygon is within [`FLATTEN_TOLERANCE`] of
/// the chord, appending every point after `p0`.
fn flatten_cubic(out: &mut Vec<Point>, p0: Point, p1: Point, p2: Point, p3: Point, depth: u32) {
    if depth >= 16 || cubic_is_flat(p0, p1, p2, p3) {
        out.push(p3);
        return;
    }
    let p01 = p0.lerp(p1, 0.5);
    let p12 = p1.lerp(p2, 0.5);
    let p23 = p2.lerp(p3, 0.5);
    let p012 = p01.lerp(p12, 0.5);
    let p123 = p12.lerp(p23, 0.5);
    let mid = p012.lerp(p123, 0.5);
    flatten_cubic(out, p0, p01, p012, mid, depth + 1);
    flatten_cubic(out, mid, p123, p23, p3, depth + 1);
}

/// Whether the control points `p1`/`p2` are within [`FLATTEN_TOLERANCE`] of
/// the chord `p0`–`p3` (the standard flatness test, scaled by the chord
/// length so it stays a distance).
fn cubic_is_flat(p0: Point, p1: Point, p2: Point, p3: Point) -> bool {
    let dx = p3.x - p0.x;
    let dy = p3.y - p0.y;
    let length2 = dx * dx + dy * dy;
    let tolerance = FLATTEN_TOLERANCE;
    if length2 <= f64::EPSILON {
        // Degenerate chord: the segment is flat when both controls are within
        // tolerance of the start point.
        return distance2(p1, p0) <= tolerance * tolerance
            && distance2(p2, p0) <= tolerance * tolerance;
    }
    let d1 = ((p1.x - p0.x) * dy - (p1.y - p0.y) * dx).abs() / length2.sqrt();
    let d2 = ((p2.x - p0.x) * dy - (p2.y - p0.y) * dx).abs() / length2.sqrt();
    d1 <= tolerance && d2 <= tolerance
}

fn distance2(a: Point, b: Point) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

/// The `AddCurve` cardinal spline (`LayerExDraw.cpp:1366-1410`): a cubic
/// Bezier per segment between consecutive points, with the tangent at each
/// point the chord of its neighbours scaled by `tension / 3` — the conversion
/// GDI+ documents for `tension` (default 0.5).
fn curve_points(points: &[Point], tension: f64, closed: bool) -> Vec<Point> {
    let count = points.len();
    if count < 2 {
        return points.to_vec();
    }
    if count == 2 {
        return points.to_vec();
    }
    let mut out = vec![points[0]];
    let segments = if closed { count } else { count - 1 };
    for index in 0..segments {
        let p0 = points[(index + count - 1) % count];
        let p1 = points[index % count];
        let p2 = points[(index + 1) % count];
        let p3 = points[(index + 2) % count];
        let c1 = Point::new(
            p1.x + (p2.x - p0.x) * tension / 3.0,
            p1.y + (p2.y - p0.y) * tension / 3.0,
        );
        let c2 = Point::new(
            p2.x - (p3.x - p1.x) * tension / 3.0,
            p2.y - (p3.y - p1.y) * tension / 3.0,
        );
        flatten_cubic(&mut out, p1, c1, c2, p2, 0);
    }
    if closed && !out.is_empty() {
        let first = out[0];
        if let Some(last) = out.last_mut() {
            *last = first;
        }
    }
    out
}

/// `NCB_REGISTER_SUBCLASS(Path)` (`main.cpp:573-593`): the `GraphicsPath`
/// builder surface. The figures live in the object's `__figures` member as an
/// array of arrays, `[closed, x0, y0, x1, y1, ...]` per figure.
fn path_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Path");
            install_path_members(runtime, instance);
            store_figures(runtime, instance, &[]);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Path");
    install_path_members(runtime, handle);
    handle
}

/// The `Path` member table: name, the reference signature's parameter count
/// (`Path.cpp`; ncbind requires all of them) and the handler.
type PathHandler =
    fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>;

fn install_path_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    let members: [(&str, usize, PathHandler); 18] = [
        ("startFigure", 0, path_start_figure),
        ("closeFigure", 0, path_close_figure),
        ("drawArc", 6, path_draw_arc),
        ("drawPie", 6, path_draw_pie),
        ("drawBezier", 8, path_draw_bezier),
        ("drawBeziers", 1, path_draw_beziers),
        ("drawClosedCurve", 1, path_draw_closed_curve),
        ("drawClosedCurve2", 2, path_draw_closed_curve2),
        ("drawCurve", 1, path_draw_curve),
        ("drawCurve2", 2, path_draw_curve2),
        ("drawCurve3", 4, path_draw_curve3),
        ("drawEllipse", 4, path_draw_ellipse),
        ("drawLine", 4, path_draw_line),
        ("drawLines", 1, path_draw_lines),
        ("drawPolygon", 1, path_draw_polygon),
        ("drawRectangle", 4, path_draw_rectangle),
        ("drawRectangles", 1, path_draw_rectangles),
        ("drawPath", 2, path_draw_path),
    ];
    for (name, count, handler) in members {
        runtime.register_object_native_with_arg_count(
            handle,
            name,
            NativeArgCount::AtLeast(count),
            handler,
        );
    }
}

/// Reads the acting `Path`'s figures, applies `edit`, writes them back. A
/// `this` that is not one of our `Path` objects edits nothing (the reference
/// would have a null native instance and crash).
fn path_edit(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    edit: impl FnOnce(&mut Vec<Figure>),
) -> Result<Variant> {
    if let Some(this) = this_obj {
        let mut figures = read_figures(runtime, this).unwrap_or_default();
        edit(&mut figures);
        store_figures(runtime, this, &figures);
    }
    Ok(Variant::Void)
}

/// `Path::startFigure` (`Path.cpp:17-21`).
fn path_start_figure(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    path_edit(runtime, this_obj, figure_start)
}

/// `Path::closeFigure` (`Path.cpp:27-31`).
fn path_close_figure(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    path_edit(runtime, this_obj, figure_close)
}

/// `Path::drawArc` (`Path.cpp:42-46`).
fn path_draw_arc(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = arc_points(
        arg_real(&args, 0),
        arg_real(&args, 1),
        arg_real(&args, 2),
        arg_real(&args, 3),
        arg_real(&args, 4),
        arg_real(&args, 5),
    );
    path_edit(runtime, this_obj, |figures| {
        for point in points {
            figure_add_point(figures, point);
        }
    })
}

/// `Path::drawPie` (`Path.cpp:158-162`): the two radii plus the arc, as one
/// closed figure.
fn path_draw_pie(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let x = arg_real(&args, 0);
    let y = arg_real(&args, 1);
    let width = arg_real(&args, 2);
    let height = arg_real(&args, 3);
    let mut points = vec![Point::new(x + width / 2.0, y + height / 2.0)];
    points.extend(arc_points(
        x,
        y,
        width,
        height,
        arg_real(&args, 4),
        arg_real(&args, 5),
    ));
    path_edit(runtime, this_obj, |figures| {
        figures.push(Figure::closed(points))
    })
}

/// `Path::drawBezier` (`Path.cpp:60-64`): one cubic segment appended to the
/// current figure.
fn path_draw_bezier(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let control = [
        Point::new(arg_real(&args, 0), arg_real(&args, 1)),
        Point::new(arg_real(&args, 2), arg_real(&args, 3)),
        Point::new(arg_real(&args, 4), arg_real(&args, 5)),
        Point::new(arg_real(&args, 6), arg_real(&args, 7)),
    ];
    path_edit(runtime, this_obj, |figures| append_cubic(figures, control))
}

/// `Path::drawBeziers` (`Path.cpp:71-77`): `3n + 1` control points.
fn path_draw_beziers(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = points_from_variant(runtime, args.first());
    if points.len() < 4 || !(points.len() - 1).is_multiple_of(3) {
        warn_once(
            runtime,
            "beziers-count",
            "Path.drawBeziers needs 3n+1 control points; the call draws nothing",
        );
        return Ok(Variant::Void);
    }
    path_edit(runtime, this_obj, |figures| {
        let mut index = 0;
        while index + 3 < points.len() {
            append_cubic(
                figures,
                [
                    points[index],
                    points[index + 1],
                    points[index + 2],
                    points[index + 3],
                ],
            );
            index += 3;
        }
    })
}

/// `Path::drawClosedCurve` (`Path.cpp:84-90`), GDI+ default tension 0.5.
fn path_draw_closed_curve(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = points_from_variant(runtime, args.first());
    path_edit(runtime, this_obj, |figures| {
        figures.push(Figure::closed(curve_points(&points, 0.5, true)))
    })
}

/// `Path::drawClosedCurve2` (`Path.cpp:98-104`).
fn path_draw_closed_curve2(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = points_from_variant(runtime, args.first());
    let tension = arg_real(&args, 1);
    path_edit(runtime, this_obj, |figures| {
        figures.push(Figure::closed(curve_points(&points, tension, true)))
    })
}

/// `Path::drawCurve` (`Path.cpp:111-117`), GDI+ default tension 0.5.
fn path_draw_curve(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = points_from_variant(runtime, args.first());
    path_edit(runtime, this_obj, |figures| {
        for point in curve_points(&points, 0.5, false) {
            figure_add_point(figures, point);
        }
    })
}

/// `Path::drawCurve2` (`Path.cpp:125-131`).
fn path_draw_curve2(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = points_from_variant(runtime, args.first());
    let tension = arg_real(&args, 1);
    path_edit(runtime, this_obj, |figures| {
        for point in curve_points(&points, tension, false) {
            figure_add_point(figures, point);
        }
    })
}

/// `Path::drawCurve3` (`Path.cpp:141-147`): `offset` and
/// `numberOfSegments` select a slice of the point array; a slice outside the
/// array is GDI+'s `InvalidParameter` and draws nothing.
fn path_draw_curve3(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = points_from_variant(runtime, args.first());
    let offset = arg_int(&args, 1).max(0) as usize;
    let segments = arg_int(&args, 2).max(0) as usize;
    let tension = arg_real(&args, 3);
    let end = offset + segments;
    if segments == 0 || end >= points.len() {
        return Ok(Variant::Void);
    }
    let slice = points[offset..=end].to_vec();
    path_edit(runtime, this_obj, |figures| {
        for point in curve_points(&slice, tension, false) {
            figure_add_point(figures, point);
        }
    })
}

/// `Path::drawEllipse` (`Path.cpp:172-176`): a closed figure.
fn path_draw_ellipse(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = arc_points(
        arg_real(&args, 0),
        arg_real(&args, 1),
        arg_real(&args, 2),
        arg_real(&args, 3),
        0.0,
        360.0,
    );
    path_edit(runtime, this_obj, |figures| {
        figures.push(Figure::closed(points))
    })
}

/// `Path::drawLine` (`Path.cpp:186-190`): both endpoints appended to the
/// current figure, so consecutive calls connect.
fn path_draw_line(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let start = Point::new(arg_real(&args, 0), arg_real(&args, 1));
    let end = Point::new(arg_real(&args, 2), arg_real(&args, 3));
    path_edit(runtime, this_obj, |figures| {
        figure_add_point(figures, start);
        figure_add_point(figures, end);
    })
}

/// `Path::drawLines` (`Path.cpp:197-203`).
fn path_draw_lines(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = points_from_variant(runtime, args.first());
    path_edit(runtime, this_obj, |figures| {
        for point in points {
            figure_add_point(figures, point);
        }
    })
}

/// `Path::drawPolygon` (`Path.cpp:211-217`): a closed figure.
fn path_draw_polygon(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let points = points_from_variant(runtime, args.first());
    path_edit(runtime, this_obj, |figures| {
        figures.push(Figure::closed(points))
    })
}

/// `Path::drawRectangle` (`Path.cpp:228-233`).
fn path_draw_rectangle(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let figure = rectangle_figure(
        arg_real(&args, 0),
        arg_real(&args, 1),
        arg_real(&args, 2),
        arg_real(&args, 3),
    );
    path_edit(runtime, this_obj, |figures| figures.push(figure))
}

/// `Path::drawRectangles` (`Path.cpp:240-246`).
fn path_draw_rectangles(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let rects = rects_from_variant(runtime, args.first());
    path_edit(runtime, this_obj, |figures| {
        for rect in rects {
            figures.push(rectangle_figure(rect[0], rect[1], rect[2], rect[3]));
        }
    })
}

/// `Path::drawPath` (`Path.cpp:252-256`, the krkr2 variant's extra member,
/// `main.cpp:592`): appends another path's figures, optionally connecting the
/// first one to the current point.
fn path_draw_path(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(source) = args.first().and_then(Variant::object_handle) else {
        return Err(TjsError::runtime("drawPath: path must be GdiPlus.Path"));
    };
    let Some(source_figures) = read_figures(runtime, source) else {
        return Err(TjsError::runtime("drawPath: path must be GdiPlus.Path"));
    };
    let connect = arg_int(&args, 1) != 0;
    path_edit(runtime, this_obj, |figures| {
        if connect && let Some(first) = source_figures.first() {
            for point in &first.points {
                figure_add_point(figures, *point);
            }
            if first.closed {
                figure_close(figures);
            }
            figures.extend(source_figures[1..].iter().cloned());
        } else {
            figures.extend(source_figures.iter().cloned());
        }
    })
}

/// Appends a cubic segment (flattened) to the current figure.
fn append_cubic(figures: &mut Vec<Figure>, control: [Point; 4]) {
    figure_add_point(figures, control[0]);
    let mut points = Vec::new();
    flatten_cubic(
        &mut points,
        control[0],
        control[1],
        control[2],
        control[3],
        0,
    );
    for point in points {
        figure_add_point(figures, point);
    }
}

// ---------------------------------------------------------------- rasteriser

/// One drawing operation, with its geometry already mapped into layer
/// (device) coordinates.
struct Job {
    geometry: Geometry,
    /// ARGB, the reference's `Color` layout.
    colour: u32,
    /// `smoothingMode` selects antialiasing (`LayerExDraw.cpp:1186, 1196`).
    aa: bool,
}

enum Geometry {
    /// The path's figures, filled with GDI+'s default `FillModeAlternate`
    /// (even-odd) rule.
    Fill(Vec<Figure>),
    /// A stroke's convex pieces (segment rectangles, join wedges, caps); the
    /// union of their spans is the stroke.
    Stroke(Vec<Vec<Point>>),
}

/// Rasterises every job into the layer in one commit.
fn rasterize(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle, jobs: &[Job]) -> Result<()> {
    layer_bitmap_write(runtime, layer, |view| {
        for job in jobs {
            rasterize_job(view, job);
        }
    })?;
    Ok(())
}

fn rasterize_job(view: &mut LayerBitmapViewMut<'_>, job: &Job) {
    let bitmap = view.bitmap;
    let pitch = bitmap.pitch as usize;
    let (clip_left, clip_top, clip_width, clip_height) = clip_box(bitmap);
    let Some((min_x, min_y, max_x, max_y)) = geometry_bounds(&job.geometry) else {
        return;
    };
    // GDI+ pixel (x, y) is the square [x, x+1) x [y, y+1): a shape touching
    // a pixel's boundary covers it.
    let x0 = (min_x.floor() as i64).max(clip_left).max(0);
    let y0 = (min_y.floor() as i64).max(clip_top).max(0);
    let x1 = (max_x.ceil() as i64)
        .min(clip_left + clip_width)
        .min(i64::from(bitmap.width));
    let y1 = (max_y.ceil() as i64)
        .min(clip_top + clip_height)
        .min(i64::from(bitmap.height));
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let sub_rows = if job.aa { SUB_ROWS } else { 1 };
    let columns = (x1 - x0) as usize;
    let mut cover = vec![0.0f64; columns];
    let mut spans: Vec<(f64, f64)> = Vec::new();
    let mut merged: Vec<(f64, f64)> = Vec::new();
    let mut events: Vec<(f64, i32)> = Vec::new();

    for row in y0..y1 {
        cover.fill(0.0);
        for sub in 0..sub_rows {
            let y = row as f64 + (sub as f64 + 0.5) / sub_rows as f64;
            spans.clear();
            match &job.geometry {
                Geometry::Fill(figures) => fill_spans(figures, y, &mut events, &mut spans),
                Geometry::Stroke(pieces) => {
                    for piece in pieces {
                        convex_span(piece, y, &mut spans);
                    }
                }
            }
            merge_spans(&mut spans, &mut merged);
            for &(start, end) in &merged {
                add_span(&mut cover, x0, start, end, job.aa);
            }
        }
        let weight = 1.0 / sub_rows as f64;
        for (index, value) in cover.iter().enumerate() {
            let coverage = (value * weight).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let x = (x0 + index as i64) as usize;
            let offset = row as usize * pitch + x * 4;
            if let Some(pixel) = view.pixels.get_mut(offset..offset + 4) {
                blend_pixel(pixel, job.colour, coverage);
            }
        }
    }
}

/// One pixel row band's coverage of the region: the horizontal extent of a
/// horizontal slice through the geometry, at height `y`.
fn add_span(cover: &mut [f64], x0: i64, start: f64, end: f64, aa: bool) {
    let columns = cover.len() as i64;
    let low = (x0 as f64).max(start);
    let high = ((x0 + columns) as f64).min(end);
    if high <= low {
        return;
    }
    let first = (low.floor() as i64).max(x0);
    let last = ((high.ceil() as i64) - 1).min(x0 + columns - 1);
    for column in first..=last {
        if aa {
            let left = (column as f64).max(low);
            let right = ((column + 1) as f64).min(high);
            if right > left {
                cover[(column - x0) as usize] += right - left;
            }
        } else {
            // No antialiasing: the pixel is painted whole when the slice
            // contains its centre, GDI+'s grid-fit behaviour.
            let centre = column as f64 + 0.5;
            if centre >= low && centre < high {
                cover[(column - x0) as usize] += 1.0;
            }
        }
    }
}

/// Sorts and coalesces the slices of one sub-row, so overlapping stroke
/// pieces count once.
///
/// The sort is the point: `stroke_pieces` emits its pieces in path order
/// (all segment rectangles, then all join wedges and caps), so a later piece
/// can lie to the *left* of an earlier one at a sampled row. The coalesce
/// walks the intervals by ascending start; without the sort a disjoint
/// left-hand interval whose start falls before the running end would be
/// dropped instead of unioned, which loses whole stroke pieces (a stroked
/// rectangle lost its left edge on the interior rows).
fn merge_spans(spans: &mut [(f64, f64)], merged: &mut Vec<(f64, f64)>) {
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    merged.clear();
    for &(start, end) in spans.iter() {
        if end <= start {
            continue;
        }
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
}

/// The horizontal slice `y` of a convex polygon: the interval between its
/// extreme crossings. Horizontal edges contribute nothing to a one-line
/// slice.
fn convex_span(poly: &[Point], y: f64, out: &mut Vec<(f64, f64)>) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut crossings = 0;
    for index in 0..poly.len() {
        let a = poly[index];
        let b = poly[(index + 1) % poly.len()];
        if a.y == b.y {
            continue;
        }
        if (a.y <= y) != (b.y <= y) {
            let t = (y - a.y) / (b.y - a.y);
            let x = a.x + t * (b.x - a.x);
            min = min.min(x);
            max = max.max(x);
            crossings += 1;
        }
    }
    if crossings >= 2 && max > min {
        out.push((min, max));
    }
}

/// The horizontal slice `y` of the path's even-odd interior (GDI+'s
/// `FillModeAlternate`, the `GraphicsPath` default): walk the path's
/// crossings sorted by x and keep the spans between an odd and an even
/// crossing count. A figure is treated as closed, as GDI+ fills every
/// figure.
fn fill_spans(figures: &[Figure], y: f64, events: &mut Vec<(f64, i32)>, out: &mut Vec<(f64, f64)>) {
    events.clear();
    for figure in figures {
        let points = &figure.points;
        if points.len() < 3 {
            continue;
        }
        for index in 0..points.len() {
            let a = points[index];
            let b = points[(index + 1) % points.len()];
            if a.y == b.y {
                continue;
            }
            if (a.y <= y) != (b.y <= y) {
                let t = (y - a.y) / (b.y - a.y);
                let x = a.x + t * (b.x - a.x);
                events.push((x, if b.y > a.y { 1 } else { -1 }));
            }
        }
    }
    if events.is_empty() {
        return;
    }
    events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut winding = 0;
    let mut start = 0.0;
    for &(x, direction) in events.iter() {
        let was_odd = winding % 2 != 0;
        winding += direction;
        let is_odd = winding % 2 != 0;
        if !was_odd && is_odd {
            start = x;
        } else if was_odd && !is_odd {
            out.push((start, x));
        }
    }
}

/// The device-space bounding box of a job's geometry, `(min_x, min_y, max_x,
/// max_y)`.
fn geometry_bounds(geometry: &Geometry) -> Option<(f64, f64, f64, f64)> {
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    let mut add = |point: Point| {
        bounds = Some(match bounds {
            Some((min_x, min_y, max_x, max_y)) => (
                min_x.min(point.x),
                min_y.min(point.y),
                max_x.max(point.x),
                max_y.max(point.y),
            ),
            None => (point.x, point.y, point.x, point.y),
        });
    };
    match geometry {
        Geometry::Fill(figures) => {
            for figure in figures {
                for point in &figure.points {
                    add(*point);
                }
            }
        }
        Geometry::Stroke(pieces) => {
            for piece in pieces {
                for point in piece {
                    add(*point);
                }
            }
        }
    }
    bounds
}

/// The reference's clip box (`layerExBase.hpp:113-116`), clamped to the image
/// the plane indexes.
fn clip_box(bitmap: LayerBitmap) -> (i64, i64, i64, i64) {
    let (left, top, width, height) = bitmap.clip;
    let left = left.clamp(0, i64::from(bitmap.width));
    let top = top.clamp(0, i64::from(bitmap.height));
    let width = width.clamp(0, i64::from(bitmap.width) - left);
    let height = height.clamp(0, i64::from(bitmap.height) - top);
    (left, top, width, height)
}

/// SourceOver (`CompositingModeSourceOver`, `LayerExDraw.cpp:993`) of a solid
/// ARGB colour over one straight-alpha RGBA pixel, with `coverage` scaling
/// the source alpha (`GdipGraphicsClear`/`FillPath` blend the antialiased
/// coverage into the colour).
fn blend_pixel(pixel: &mut [u8], colour: u32, coverage: f64) {
    blend_rgba(
        pixel,
        [
            f64::from((colour >> 16) & 0xff),
            f64::from((colour >> 8) & 0xff),
            f64::from(colour & 0xff),
            f64::from((colour >> 24) & 0xff),
        ],
        coverage,
    );
}

/// The same SourceOver blend for a source colour already in `[R, G, B, A]`
/// reals (`drawImage*`'s sampled pixels, whose alpha is part of the sample).
fn blend_rgba(pixel: &mut [u8], src: [f64; 4], coverage: f64) {
    let source_alpha = src[3] / 255.0 * coverage;
    let dest_alpha = f64::from(pixel[3]) / 255.0;
    let out_alpha = source_alpha + dest_alpha * (1.0 - source_alpha);
    if out_alpha <= 0.0 {
        pixel.fill(0);
        return;
    }
    let mut out = [0u8; 4];
    for channel in 0..3 {
        let value = (src[channel] * source_alpha
            + f64::from(pixel[channel]) * dest_alpha * (1.0 - source_alpha))
            / out_alpha;
        out[channel] = value.round().clamp(0.0, 255.0) as u8;
    }
    out[3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    pixel.copy_from_slice(&out);
}

// ---------------------------------------------------------------- strokes

/// Builds the stroke of a path as convex pieces: one rectangle per segment
/// (the pen's width, centred on the segment, with the reference's
/// `LineCapFlat` butt ends), a join wedge or disc at every interior vertex,
/// and the caps at the ends of an open figure. GDI+ defaults: `LineCapFlat`
/// and `LineJoinMiter` with miter limit 10.
fn stroke_pieces(figures: &[Figure], pen: &Pen) -> Vec<Vec<Point>> {
    let half = pen.width / 2.0;
    if !half.is_finite() || half <= 0.0 {
        return Vec::new();
    }
    let mut pieces = Vec::new();
    for figure in figures {
        let count = figure.points.len();
        if count < 2 {
            continue;
        }
        let segments: Vec<(Point, Point)> = (0..count - 1)
            .map(|index| (figure.points[index], figure.points[index + 1]))
            .collect();
        let segments = if figure.closed {
            let mut segments = segments;
            segments.push((figure.points[count - 1], figure.points[0]));
            segments
        } else {
            segments
        };

        for (index, &(start, end)) in segments.iter().enumerate() {
            let Some(direction) = unit(start, end) else {
                continue;
            };
            let square_start = pen.start_cap == Cap::Square && !figure.closed && index == 0;
            let square_end =
                pen.end_cap == Cap::Square && !figure.closed && index + 1 == segments.len();
            pieces.push(segment_quad(
                start,
                end,
                direction,
                half,
                square_start,
                square_end,
            ));
            if !figure.closed && index == 0 && pen.start_cap == Cap::Round {
                pieces.push(disc(start, half));
            }
            if !figure.closed && index + 1 == segments.len() && pen.end_cap == Cap::Round {
                pieces.push(disc(end, half));
            }
        }

        let vertex_count = if figure.closed { count } else { count - 2 };
        for step in 0..vertex_count {
            let vertex_index = if figure.closed { step } else { step + 1 };
            let vertex = figure.points[vertex_index];
            let previous = figure.points[(vertex_index + count - 1) % count];
            let next = figure.points[(vertex_index + 1) % count];
            let (Some(incoming), Some(outgoing)) = (unit(previous, vertex), unit(vertex, next))
            else {
                continue;
            };
            if let Some(join) = join_piece(vertex, incoming, outgoing, half, pen) {
                pieces.push(join);
            }
        }
    }
    pieces
}

fn unit(from: Point, to: Point) -> Option<Point> {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f64::EPSILON {
        None
    } else {
        Some(Point::new(dx / length, dy / length))
    }
}

/// One segment's stroke rectangle, butt by default, extended by `half` along
/// the direction at a square-capped end.
fn segment_quad(
    start: Point,
    end: Point,
    direction: Point,
    half: f64,
    square_start: bool,
    square_end: bool,
) -> Vec<Point> {
    let normal = Point::new(-direction.y, direction.x);
    let start_back = if square_start {
        Point::new(start.x - direction.x * half, start.y - direction.y * half)
    } else {
        start
    };
    let end_forward = if square_end {
        Point::new(end.x + direction.x * half, end.y + direction.y * half)
    } else {
        end
    };
    vec![
        Point::new(
            start_back.x + normal.x * half,
            start_back.y + normal.y * half,
        ),
        Point::new(
            end_forward.x + normal.x * half,
            end_forward.y + normal.y * half,
        ),
        Point::new(
            end_forward.x - normal.x * half,
            end_forward.y - normal.y * half,
        ),
        Point::new(
            start_back.x - normal.x * half,
            start_back.y - normal.y * half,
        ),
    ]
}

/// The wedge, miter or disc that fills the outside of a vertex between two
/// segments.
fn join_piece(
    vertex: Point,
    incoming: Point,
    outgoing: Point,
    half: f64,
    pen: &Pen,
) -> Option<Vec<Point>> {
    let cross = incoming.x * outgoing.y - incoming.y * outgoing.x;
    if cross.abs() <= f64::EPSILON {
        // Collinear (or a perfect reversal): the segment rectangles already
        // meet with no gap on the outside.
        return None;
    }
    // The outside of the turn is the side the segments rotate away from.
    let side = if cross > 0.0 { -1.0 } else { 1.0 };
    let normal_in = Point::new(-incoming.y * side, incoming.x * side);
    let normal_out = Point::new(-outgoing.y * side, outgoing.x * side);
    let a = Point::new(vertex.x + normal_in.x * half, vertex.y + normal_in.y * half);
    let b = Point::new(
        vertex.x + normal_out.x * half,
        vertex.y + normal_out.y * half,
    );
    match pen.join {
        Join::Round => Some(disc(vertex, half)),
        Join::Bevel => Some(vec![vertex, a, b]),
        Join::Miter => {
            let miter = line_intersection(a, incoming, b, outgoing)?;
            let limit = pen.miter_limit.max(1.0) * half;
            if distance2(miter, vertex) <= limit * limit {
                Some(vec![vertex, a, miter, b])
            } else {
                Some(vec![vertex, a, b])
            }
        }
    }
}

/// Intersection of the line through `a` along `da` with the line through `b`
/// along `db`, `None` when they are parallel.
fn line_intersection(a: Point, da: Point, b: Point, db: Point) -> Option<Point> {
    let denominator = da.x * db.y - da.y * db.x;
    if denominator.abs() <= f64::EPSILON {
        return None;
    }
    let t = ((b.x - a.x) * db.y - (b.y - a.y) * db.x) / denominator;
    Some(Point::new(a.x + da.x * t, a.y + da.y * t))
}

/// A regular polygon approximating a disc of radius `radius`; 16 sides keep
/// its outline inside the antialiased edge.
fn disc(centre: Point, radius: f64) -> Vec<Point> {
    const SIDES: usize = 16;
    (0..SIDES)
        .map(|index| {
            let angle = std::f64::consts::TAU * index as f64 / SIDES as f64;
            Point::new(
                centre.x + radius * angle.cos(),
                centre.y + radius * angle.sin(),
            )
        })
        .collect()
}
// ---------------------------------------------------------------- Layer

/// The `Layer` member table: name, the reference signature's parameter count
/// (`LayerExDraw.hpp:297-731`; ncbind requires all of them) and the handler.
type LayerHandler =
    fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>;

/// `NCB_ATTACH_CLASS_WITH_HOOK(LayerExDraw, Layer)` (`main.cpp:860-936`): the
/// four properties, the forty-one methods and the sixteen `EncoderValue`
/// constants, each with the reference's argument count.
fn install_layer_ex_draw(runtime: &mut Runtime<KrkrHost>) {
    let Variant::Object(layer) = runtime.global_member("Layer") else {
        return;
    };

    register_layer_properties(runtime, layer);

    let members: [(&str, usize, LayerHandler); 41] = [
        ("setViewTransform", 1, layer_set_view_transform),
        ("resetViewTransform", 0, layer_reset_view_transform),
        ("rotateViewTransform", 1, layer_rotate_view_transform),
        ("scaleViewTransform", 2, layer_scale_view_transform),
        ("translateViewTransform", 2, layer_translate_view_transform),
        ("setTransform", 1, layer_set_transform),
        ("resetTransform", 0, layer_reset_transform),
        ("rotateTransform", 1, layer_rotate_transform),
        ("scaleTransform", 2, layer_scale_transform),
        ("translateTransform", 2, layer_translate_transform),
        ("clear", 1, layer_clear),
        ("drawPath", 2, layer_draw_path),
        ("drawArc", 7, layer_draw_arc),
        ("drawPie", 7, layer_draw_pie),
        ("drawBezier", 9, layer_draw_bezier),
        ("drawBeziers", 2, layer_draw_beziers),
        ("drawClosedCurve", 2, layer_draw_closed_curve),
        ("drawClosedCurve2", 3, layer_draw_closed_curve2),
        ("drawCurve", 2, layer_draw_curve),
        ("drawCurve2", 3, layer_draw_curve2),
        ("drawCurve3", 5, layer_draw_curve3),
        ("drawEllipse", 5, layer_draw_ellipse),
        ("drawLine", 5, layer_draw_line),
        ("drawLines", 2, layer_draw_lines),
        ("drawPolygon", 2, layer_draw_polygon),
        ("drawRectangle", 5, layer_draw_rectangle),
        ("drawRectangles", 2, layer_draw_rectangles),
        ("drawPathString", 5, layer_draw_path_string),
        ("drawString", 5, layer_draw_string),
        ("measureString", 2, layer_measure_string),
        ("measureStringInternal", 2, layer_measure_string_internal),
        ("drawImage", 3, layer_draw_image),
        ("drawImageRect", 7, layer_draw_image_rect),
        ("drawImageStretch", 9, layer_draw_image_stretch),
        ("drawImageAffine", 12, layer_draw_image_affine),
        ("getRecordImage", 0, layer_get_record_image),
        ("redrawRecord", 0, layer_redraw_record),
        ("saveRecord", 1, layer_save_record),
        ("loadRecord", 1, layer_load_record),
        ("saveImage", 1, layer_save_image),
        ("getColorRegionRects", 1, layer_get_color_region_rects),
    ];
    for (name, count, handler) in members {
        register_unless_closure(runtime, layer, name, count, handler);
    }

    for &(name, value) in ENCODER_VALUE_CONSTANTS {
        runtime.set_object_member(layer, name, Variant::Integer(value));
    }
}

/// The four `NCB_PROPERTY` members of the attach block (`main.cpp:861-863,
/// 908`). Their values live in the per-layer state, like the reference's
/// instance fields; `record` has no metafile behind it and always reads
/// false, warning once when a script asks for recording.
fn register_layer_properties(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) {
    runtime.register_object_native_property(
        layer,
        "updateWhenDraw",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Integer(i64::from(layer_state_read(
                runtime,
                this_obj,
                |state| state.update_when_draw,
                true,
            ))))
        },
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            let update = value.to_integer().unwrap_or(0) != 0;
            layer_state_write(runtime, this_obj, |state| state.update_when_draw = update);
            Ok(())
        },
    );
    runtime.register_object_native_property(
        layer,
        "smoothingMode",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Integer(layer_state_read(
                runtime,
                this_obj,
                |state| state.smoothing_mode,
                SMOOTHING_MODE_ANTIALIAS,
            )))
        },
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            let mode = value.to_integer().unwrap_or(SMOOTHING_MODE_ANTIALIAS);
            layer_state_write(runtime, this_obj, |state| state.smoothing_mode = mode);
            Ok(())
        },
    );
    runtime.register_object_native_property(
        layer,
        "textRenderingHint",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Integer(layer_state_read(
                runtime,
                this_obj,
                |state| state.text_rendering_hint,
                TEXT_RENDERING_HINT_ANTIALIAS,
            )))
        },
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            let hint = value.to_integer().unwrap_or(TEXT_RENDERING_HINT_ANTIALIAS);
            layer_state_write(runtime, this_obj, |state| state.text_rendering_hint = hint);
            Ok(())
        },
    );
    runtime.register_object_native_property(
        layer,
        "record",
        |_runtime: &mut Runtime<KrkrHost>, _this_obj: Option<ObjectHandle>| {
            // `getRecord` answers `metafile != NULL` (`LayerExDraw.hpp:689-691`);
            // no metafile is ever created here.
            Ok(Variant::Integer(0))
        },
        |runtime: &mut Runtime<KrkrHost>, _this_obj: Option<ObjectHandle>, value: Variant| {
            if value.to_integer().unwrap_or(0) != 0 {
                warn_once(
                    runtime,
                    "record",
                    "metafile recording is not implemented: `record = true` records nothing and \
                     getRecordImage/saveRecord/redrawRecord have nothing to answer",
                );
            }
            Ok(())
        },
    );
}

fn layer_state_read<R>(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    read: impl FnOnce(&LayerState) -> R,
    fallback: R,
) -> R {
    match this_obj {
        Some(layer) => with_layer_state(runtime, layer, |state| read(state)),
        None => fallback,
    }
}

fn layer_state_write(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    write: impl FnOnce(&mut LayerState),
) {
    if let Some(layer) = this_obj {
        with_layer_state(runtime, layer, write);
    }
}

// ---------------------------------------------------------------- transforms

fn layer_set_view_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let matrix = require_matrix(runtime, &args, 0)?;
    layer_state_write(runtime, this_obj, |state| state.view_transform = matrix);
    Ok(Variant::Void)
}

fn layer_reset_view_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    layer_state_write(runtime, this_obj, |state| {
        state.view_transform = MATRIX_IDENTITY
    });
    Ok(Variant::Void)
}

fn layer_rotate_view_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let rotation = rotation_matrix(arg_real(&args, 0));
    layer_state_write(runtime, this_obj, |state| {
        state.view_transform = matrix_mul(state.view_transform, rotation)
    });
    Ok(Variant::Void)
}

fn layer_scale_view_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let scale = [arg_real(&args, 0), 0.0, 0.0, arg_real(&args, 1), 0.0, 0.0];
    layer_state_write(runtime, this_obj, |state| {
        state.view_transform = matrix_mul(state.view_transform, scale)
    });
    Ok(Variant::Void)
}

fn layer_translate_view_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let translate = [1.0, 0.0, 0.0, 1.0, arg_real(&args, 0), arg_real(&args, 1)];
    layer_state_write(runtime, this_obj, |state| {
        state.view_transform = matrix_mul(state.view_transform, translate)
    });
    Ok(Variant::Void)
}

fn layer_set_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let matrix = require_matrix(runtime, &args, 0)?;
    layer_state_write(runtime, this_obj, |state| state.transform = matrix);
    Ok(Variant::Void)
}

fn layer_reset_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    layer_state_write(runtime, this_obj, |state| state.transform = MATRIX_IDENTITY);
    Ok(Variant::Void)
}

fn layer_rotate_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let rotation = rotation_matrix(arg_real(&args, 0));
    layer_state_write(runtime, this_obj, |state| {
        state.transform = matrix_mul(state.transform, rotation)
    });
    Ok(Variant::Void)
}

fn layer_scale_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let scale = [arg_real(&args, 0), 0.0, 0.0, arg_real(&args, 1), 0.0, 0.0];
    layer_state_write(runtime, this_obj, |state| {
        state.transform = matrix_mul(state.transform, scale)
    });
    Ok(Variant::Void)
}

fn layer_translate_transform(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let translate = [1.0, 0.0, 0.0, 1.0, arg_real(&args, 0), arg_real(&args, 1)];
    layer_state_write(runtime, this_obj, |state| {
        state.transform = matrix_mul(state.transform, translate)
    });
    Ok(Variant::Void)
}

/// GDI+ `Matrix::Rotate(angle)`'s rotation, degrees clockwise in the y-down
/// frame (`LayerExDraw.cpp:1097-1101`).
fn rotation_matrix(angle: f64) -> [f64; 6] {
    let (sin, cos) = angle.to_radians().sin_cos();
    [cos, sin, -sin, cos, 0.0, 0.0]
}

/// The `Matrix*` argument of the transform setters (`getMatrix`,
/// `main.cpp:338-368`): an array or a dictionary, as the reference's
/// converter accepts. Anything else is `TJS_E_INVALIDPARAM`'s equivalent
/// here, where the reference's `NULL` silently resets the matrix.
fn require_matrix(runtime: &Runtime<KrkrHost>, args: &[Variant], index: usize) -> Result<[f64; 6]> {
    variant_matrix(runtime, args.get(index))
        .ok_or_else(|| TjsError::runtime("invalid parameter: Matrix"))
}

// ---------------------------------------------------------------- drawing

/// The appearance argument of every `draw*` method: the reference binds
/// `const Appearance*` through the native instance (`main.cpp:566-571`), a
/// non-Appearance object would be a null pointer there.
fn appearance_argument(
    runtime: &Runtime<KrkrHost>,
    args: &[Variant],
    index: usize,
) -> Result<Vec<DrawInfo>> {
    let handle = args.get(index).and_then(Variant::object_handle);
    handle
        .and_then(|handle| appearance_infos_of(runtime, handle))
        .ok_or_else(|| TjsError::runtime("invalid parameter: Appearance"))
}

/// `_drawPath` (`LayerExDraw.cpp:1204-1261`): each appearance entry draws the
/// path under `T(ox, oy) · calcTransform`, the returned `RectF` is the union
/// of the transformed bounds (`GraphicsPath::GetBounds` with the pen, so a
/// stroke's rect includes its width), and `updateRect` runs `Layer.update`
/// when `updateWhenDraw` is set (`:937-946`).
fn draw_figures(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    infos: &[DrawInfo],
    figures: &[Figure],
) -> Result<Variant> {
    let (transform, view_transform, update_when_draw, aa) =
        with_layer_state(runtime, layer, |state| {
            (
                state.transform,
                state.view_transform,
                state.update_when_draw,
                smoothing_enabled(state.smoothing_mode),
            )
        });
    // `calcTransform = transform · viewTransform` (`:1064-1073`), composed
    // with the entry's offset the way `draw`/`fill` prepend it (`:1184-1198`).
    let calc = matrix_mul(transform, view_transform);

    let mut jobs = Vec::new();
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for info in infos {
        let matrix = matrix_mul([1.0, 0.0, 0.0, 1.0, info.ox, info.oy], calc);
        let (geometry, colour) = match info.paint {
            Paint::Fill(colour) => (
                Geometry::Fill(
                    figures
                        .iter()
                        .map(|figure| figure.transform(matrix))
                        .collect(),
                ),
                colour,
            ),
            Paint::Stroke(pen) => {
                // The stroke outline is built at the pen's world-space width
                // and then mapped, so the transform scales the pen with the
                // path, the way GDI+ transforms a drawn pen (`:1182-1189`).
                let pieces = stroke_pieces(figures, &pen);
                let pieces = pieces
                    .iter()
                    .map(|piece| {
                        piece
                            .iter()
                            .map(|point| point.transform(matrix))
                            .collect::<Vec<Point>>()
                    })
                    .collect();
                (Geometry::Stroke(pieces), pen.colour)
            }
        };
        if let Some(found) = geometry_bounds(&geometry) {
            bounds = Some(match bounds {
                Some(current) => (
                    current.0.min(found.0),
                    current.1.min(found.1),
                    current.2.max(found.2),
                    current.3.max(found.3),
                ),
                None => found,
            });
        }
        jobs.push(Job {
            geometry,
            colour,
            aa,
        });
    }
    rasterize(runtime, layer, &jobs)?;
    if update_when_draw {
        layer_update(runtime, layer)?;
    }
    let rect = bounds.unwrap_or((0.0, 0.0, 0.0, 0.0));
    Ok(Variant::Object(new_rect_f(
        runtime,
        rect.0,
        rect.1,
        rect.2 - rect.0,
        rect.3 - rect.1,
    )))
}

/// Whether `smoothingMode` antialiases: GDI+'s `SmoothingModeHighQuality` (2)
/// and `SmoothingModeAntiAlias` (4) smooth; `Default` (0), `HighSpeed` (1),
/// `None` (3) and `Invalid` (-1) do not.
fn smoothing_enabled(mode: i64) -> bool {
    matches!(mode, SMOOTHING_MODE_HIGH_QUALITY | SMOOTHING_MODE_ANTIALIAS)
}

/// `Layer.clear(argb)` (`LayerExDraw.cpp:1121-1130`): `Graphics::Clear`
/// replaces every pixel of the clipping region with the colour (and ignores
/// the transform), then the no-argument `Layer.update()` runs.
fn layer_clear(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let colour = arg_int(&args, 0) as u32;
    let rgba = [
        ((colour >> 16) & 0xff) as u8,
        ((colour >> 8) & 0xff) as u8,
        (colour & 0xff) as u8,
        (colour >> 24) as u8,
    ];
    layer_bitmap_write(runtime, layer, |view| {
        let (left, top, width, height) = clip_box(view.bitmap);
        let pitch = view.bitmap.pitch as usize;
        for y in top..top + height {
            for x in left..left + width {
                let offset = y as usize * pitch + x as usize * 4;
                if let Some(pixel) = view.pixels.get_mut(offset..offset + 4) {
                    pixel.copy_from_slice(&rgba);
                }
            }
        }
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

/// `Layer.drawPath(app, path)` (`LayerExDraw.cpp:1266-1270`).
fn layer_draw_path(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let Some(path) = args.get(1).and_then(Variant::object_handle) else {
        return Err(TjsError::runtime("invalid parameter: Path"));
    };
    let Some(figures) = path_figures_of(runtime, path) else {
        return Err(TjsError::runtime("invalid parameter: Path"));
    };
    draw_figures(runtime, layer, &infos, &figures)
}

/// `Layer.drawArc` (`LayerExDraw.cpp:1282-1288`): one `AddArc` figure.
fn layer_draw_arc(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let figures = vec![Figure::open(arc_points(
        arg_real(&args, 1),
        arg_real(&args, 2),
        arg_real(&args, 3),
        arg_real(&args, 4),
        arg_real(&args, 5),
        arg_real(&args, 6),
    ))];
    draw_figures(runtime, layer, &infos, &figures)
}

/// `Layer.drawPie` (`LayerExDraw.cpp:1422-1428`): `AddPie`'s closed figure.
fn layer_draw_pie(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let x = arg_real(&args, 1);
    let y = arg_real(&args, 2);
    let width = arg_real(&args, 3);
    let height = arg_real(&args, 4);
    let mut points = vec![Point::new(x + width / 2.0, y + height / 2.0)];
    points.extend(arc_points(
        x,
        y,
        width,
        height,
        arg_real(&args, 5),
        arg_real(&args, 6),
    ));
    draw_figures(runtime, layer, &infos, &[Figure::closed(points)])
}

/// `Layer.drawBezier` (`LayerExDraw.cpp:1303-1309`): `AddBezier`.
fn layer_draw_bezier(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let control = [
        Point::new(arg_real(&args, 1), arg_real(&args, 2)),
        Point::new(arg_real(&args, 3), arg_real(&args, 4)),
        Point::new(arg_real(&args, 5), arg_real(&args, 6)),
        Point::new(arg_real(&args, 7), arg_real(&args, 8)),
    ];
    let mut points = vec![control[0]];
    flatten_cubic(
        &mut points,
        control[0],
        control[1],
        control[2],
        control[3],
        0,
    );
    draw_figures(runtime, layer, &infos, &[Figure::open(points)])
}

/// `Layer.drawBeziers` (`LayerExDraw.cpp:1317-1325`): `AddBeziers` over
/// `3n + 1` control points.
fn layer_draw_beziers(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let points = points_from_variant(runtime, args.get(1));
    if points.len() < 4 || !(points.len() - 1).is_multiple_of(3) {
        warn_once(
            runtime,
            "beziers-count",
            "drawBeziers needs 3n+1 control points; the call draws nothing",
        );
        return Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)));
    }
    let mut figures: Vec<Figure> = Vec::new();
    let mut index = 0;
    while index + 3 < points.len() {
        append_cubic(
            &mut figures,
            [
                points[index],
                points[index + 1],
                points[index + 2],
                points[index + 3],
            ],
        );
        index += 3;
    }
    draw_figures(runtime, layer, &infos, &figures)
}

/// `Layer.drawClosedCurve` (`LayerExDraw.cpp:1333-1341`), default tension
/// 0.5.
fn layer_draw_closed_curve(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let points = points_from_variant(runtime, args.get(1));
    draw_figures(
        runtime,
        layer,
        &infos,
        &[Figure::closed(curve_points(&points, 0.5, true))],
    )
}

/// `Layer.drawClosedCurve2` (`LayerExDraw.cpp:1350-1358`).
fn layer_draw_closed_curve2(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let points = points_from_variant(runtime, args.get(1));
    let tension = arg_real(&args, 2);
    draw_figures(
        runtime,
        layer,
        &infos,
        &[Figure::closed(curve_points(&points, tension, true))],
    )
}

/// `Layer.drawCurve` (`LayerExDraw.cpp:1366-1374`), default tension 0.5.
fn layer_draw_curve(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let points = points_from_variant(runtime, args.get(1));
    draw_figures(
        runtime,
        layer,
        &infos,
        &[Figure::open(curve_points(&points, 0.5, false))],
    )
}

/// `Layer.drawCurve2` (`LayerExDraw.cpp:1384-1391`).
fn layer_draw_curve2(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let points = points_from_variant(runtime, args.get(1));
    let tension = arg_real(&args, 2);
    draw_figures(
        runtime,
        layer,
        &infos,
        &[Figure::open(curve_points(&points, tension, false))],
    )
}

/// `Layer.drawCurve3` (`LayerExDraw.cpp:1402-1410`): `offset` and
/// `numberOfSegments` select the points; a slice outside them is GDI+'s
/// `InvalidParameter` and draws nothing.
fn layer_draw_curve3(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let points = points_from_variant(runtime, args.get(1));
    let offset = arg_int(&args, 2).max(0) as usize;
    let segments = arg_int(&args, 3).max(0) as usize;
    let tension = arg_real(&args, 4);
    let end = offset + segments;
    if segments == 0 || end >= points.len() {
        return Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)));
    }
    let slice = points[offset..=end].to_vec();
    draw_figures(
        runtime,
        layer,
        &infos,
        &[Figure::open(curve_points(&slice, tension, false))],
    )
}

/// `Layer.drawEllipse` (`LayerExDraw.cpp:1439-1445`): `AddEllipse`'s closed
/// figure.
fn layer_draw_ellipse(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let points = arc_points(
        arg_real(&args, 1),
        arg_real(&args, 2),
        arg_real(&args, 3),
        arg_real(&args, 4),
        0.0,
        360.0,
    );
    draw_figures(runtime, layer, &infos, &[Figure::closed(points)])
}

/// `Layer.drawLine` (`LayerExDraw.cpp:1456-1462`): `AddLine`.
fn layer_draw_line(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let figures = vec![Figure::open(vec![
        Point::new(arg_real(&args, 1), arg_real(&args, 2)),
        Point::new(arg_real(&args, 3), arg_real(&args, 4)),
    ])];
    draw_figures(runtime, layer, &infos, &figures)
}

/// `Layer.drawLines` (`LayerExDraw.cpp:1470-1478`): `AddLines`.
fn layer_draw_lines(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let points = points_from_variant(runtime, args.get(1));
    draw_figures(runtime, layer, &infos, &[Figure::open(points)])
}

/// `Layer.drawPolygon` (`LayerExDraw.cpp:1486-1494`): `AddPolygon`'s closed
/// figure.
fn layer_draw_polygon(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let points = points_from_variant(runtime, args.get(1));
    draw_figures(runtime, layer, &infos, &[Figure::closed(points)])
}

/// `Layer.drawRectangle` (`LayerExDraw.cpp:1506-1513`): `AddRectangle`.
fn layer_draw_rectangle(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let figure = rectangle_figure(
        arg_real(&args, 1),
        arg_real(&args, 2),
        arg_real(&args, 3),
        arg_real(&args, 4),
    );
    draw_figures(runtime, layer, &infos, &[figure])
}

/// `Layer.drawRectangles` (`LayerExDraw.cpp:1521-1529`): `AddRectangles`.
fn layer_draw_rectangles(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let infos = appearance_argument(runtime, &args, 0)?;
    let rects = rects_from_variant(runtime, args.get(1));
    let figures: Vec<Figure> = rects
        .iter()
        .map(|rect| rectangle_figure(rect[0], rect[1], rect[2], rect[3]))
        .collect();
    draw_figures(runtime, layer, &infos, &figures)
}

// ---------------------------------------------------------------- text

/// `Layer.drawPathString(font, app, x, y, text)` (`LayerExDraw.cpp:1540-1550`)
/// builds the glyph outlines of `text` and draws them. This engine has no font
/// backend reachable from plugin code, so the call warns once and draws
/// nothing; the returned rect is empty instead of the text's bounds.
fn layer_draw_path_string(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_once(
        runtime,
        "text",
        "drawPathString/drawString/measureString* are not implemented (no font backend \
         reachable from plugin code): text draws nothing and measures empty",
    );
    Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)))
}

/// `Layer.drawString(font, app, x, y, text)` (`LayerExDraw.cpp:1590-1636`):
/// see [`layer_draw_path_string`].
fn layer_draw_string(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_once(
        runtime,
        "text",
        "drawPathString/drawString/measureString* are not implemented (no font backend \
         reachable from plugin code): text draws nothing and measures empty",
    );
    Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)))
}

/// `Layer.measureString(font, text)` (`LayerExDraw.cpp:1644-1655`):
/// `Graphics::MeasureString` over a GDI+ font; no font backend here.
fn layer_measure_string(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_once(
        runtime,
        "text",
        "drawPathString/drawString/measureString* are not implemented (no font backend \
         reachable from plugin code): text draws nothing and measures empty",
    );
    Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)))
}

/// `Layer.measureStringInternal(font, text)` (`LayerExDraw.cpp:1663-1681`):
/// the character-range region bounds of the measured string.
fn layer_measure_string_internal(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_once(
        runtime,
        "text",
        "drawPathString/drawString/measureString* are not implemented (no font backend \
         reachable from plugin code): text draws nothing and measures empty",
    );
    Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)))
}

// ---------------------------------------------------------------- images

/// `Layer.drawImage(x, y, src)` (`LayerExDraw.cpp:1690-1701`): the image's
/// bounds position the copy, which is `drawImageRect(x + bounds.X, y +
/// bounds.Y, src, 0, 0, bounds.Width, bounds.Height)`. The reference's
/// converter accepts a **`Layer`** as the image as well as a GDI+ `Image`
/// (`main.cpp:424-445`, the `LayerExDraw` fallback), and a layer's pixels are
/// reachable here, so a layer source is sampled for real; a `GdiPlus.Image`
/// carries no pixels in this engine (the decoder is not reachable from plugin
/// code) and takes the reference's null-image path (`:1694`) with a warning.
fn layer_draw_image(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let Some(source) = args.get(2).and_then(Variant::object_handle) else {
        warn_image_stub(runtime);
        return Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)));
    };
    let Some((width, height)) = layer_image_size(runtime, source) else {
        warn_image_stub(runtime);
        return Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)));
    };
    // `getBounds` of a bitmap is `(0, 0, width, height)`.
    draw_image_affine(
        runtime,
        layer,
        Some(source),
        0.0,
        0.0,
        f64::from(width),
        f64::from(height),
        true,
        1.0,
        0.0,
        0.0,
        1.0,
        arg_real(&args, 0),
        arg_real(&args, 1),
    )
}

/// `Layer.drawImageRect` (`LayerExDraw.cpp:1714-1718`): forwards to
/// `drawImageAffine` with the identity transform.
fn layer_draw_image_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    draw_image_affine(
        runtime,
        layer,
        args.get(2).and_then(Variant::object_handle),
        arg_real(&args, 3),
        arg_real(&args, 4),
        arg_real(&args, 5),
        arg_real(&args, 6),
        true,
        1.0,
        0.0,
        0.0,
        1.0,
        arg_real(&args, 0),
        arg_real(&args, 1),
    )
}

/// `Layer.drawImageStretch` (`LayerExDraw.cpp:1733-1737`): an affine copy with
/// `dwidth/swidth`, `dheight/sheight` scaling.
fn layer_draw_image_stretch(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let swidth = arg_real(&args, 7);
    let sheight = arg_real(&args, 8);
    draw_image_affine(
        runtime,
        layer,
        args.get(4).and_then(Variant::object_handle),
        arg_real(&args, 5),
        arg_real(&args, 6),
        swidth,
        sheight,
        true,
        arg_real(&args, 2) / swidth,
        0.0,
        0.0,
        arg_real(&args, 3) / sheight,
        arg_real(&args, 0),
        arg_real(&args, 1),
    )
}

/// `Layer.drawImageAffine` (`LayerExDraw.cpp:1748-1800`): the parallelogram
/// `(A,B) (C,D) (E,F) (C-A+E, D-B+F)` of the source rect, or the affine form
/// `A·x + C·y + E / B·x + D·y + F`. The `src` argument is the reference's
/// `Image*`, which its converter also answers for a `Layer`
/// (`main.cpp:424-445`); only a layer has pixels here.
fn layer_draw_image_affine(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    draw_image_affine(
        runtime,
        layer,
        args.first().and_then(Variant::object_handle),
        arg_real(&args, 1),
        arg_real(&args, 2),
        arg_real(&args, 3),
        arg_real(&args, 4),
        arg_int(&args, 5) != 0,
        arg_real(&args, 6),
        arg_real(&args, 7),
        arg_real(&args, 8),
        arg_real(&args, 9),
        arg_real(&args, 10),
        arg_real(&args, 11),
    )
}

/// The shared body of the four `drawImage*` members: the source
/// `(sleft, stop, swidth, sheight)` rectangle is mapped onto the destination
/// parallelogram through `calcTransform`, sampled bilinearly and blended
/// SourceOver, and the returned rect is the transformed corners' bounding box
/// (`LayerExDraw.cpp:1748-1800`).
#[allow(clippy::too_many_arguments)]
fn draw_image_affine(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    source: Option<ObjectHandle>,
    sleft: f64,
    stop: f64,
    swidth: f64,
    sheight: f64,
    affine: bool,
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
) -> Result<Variant> {
    let Some(source) = source else {
        warn_image_stub(runtime);
        return Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)));
    };
    if layer_image_size(runtime, source).is_none() {
        // The reference's converter answers a null `Image*` for anything that
        // is neither a GDI+ `Image` nor a layer, and `drawImageAffine` then
        // keeps the zero rect (`:1752`).
        warn_image_stub(runtime);
        return Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)));
    }

    // Destination corners in world space (`:1753-1774`): the affine form maps
    // the source rect's own corners, the coordinate form takes three points
    // and derives the fourth as `p1 + p2 - p0`.
    let corners = if affine {
        [
            [e, f],
            [a * swidth + e, b * swidth + f],
            [c * sheight + e, d * sheight + f],
            [a * swidth + c * sheight + e, b * swidth + d * sheight + f],
        ]
    } else {
        [[a, b], [c, d], [e, f], [c - a + e, d - b + f]]
    };

    let (transform, view_transform, update_when_draw) = with_layer_state(runtime, layer, |state| {
        (
            state.transform,
            state.view_transform,
            state.update_when_draw,
        )
    });
    let calc = matrix_mul(transform, view_transform);
    let device: Vec<Point> = corners
        .iter()
        .map(|corner| Point::new(corner[0], corner[1]).transform(calc))
        .collect();
    // `calcTransform.TransformPoints(points, 4)` then the min/max box
    // (`:1781-1795`).
    let mut min_x = device[0].x;
    let mut max_x = device[0].x;
    let mut min_y = device[0].y;
    let mut max_y = device[0].y;
    for point in &device[1..] {
        min_x = min_x.min(point.x);
        max_x = max_x.max(point.x);
        min_y = min_y.min(point.y);
        max_y = max_y.max(point.y);
    }

    layer_bitmap_read_write(runtime, source, layer, |source_view, dest_view| {
        rasterize_image(
            source_view,
            dest_view,
            &device,
            (sleft, stop, swidth, sheight),
        );
    })?;
    if update_when_draw {
        layer_update(runtime, layer)?;
    }
    Ok(Variant::Object(new_rect_f(
        runtime,
        min_x,
        min_y,
        max_x - min_x,
        max_y - min_y,
    )))
}

fn warn_image_stub(runtime: &mut Runtime<KrkrHost>) {
    warn_once(
        runtime,
        "draw-image",
        "drawImage*: the source has no pixels. Pass a Layer as `src` (the reference's \
         converter accepts one and layer pixels are sampled); a GdiPlus.Image is not decoded \
         because this engine's decoder is not reachable from plugin code",
    );
}

/// The main image's size of a layer-shaped source, `None` when the object is
/// not a drawable layer (a `GdiPlus.Image` included).
fn layer_image_size(runtime: &mut Runtime<KrkrHost>, source: ObjectHandle) -> Option<(u32, u32)> {
    layer_bitmap_read(runtime, source, |view| {
        (view.bitmap.width, view.bitmap.height)
    })
    .ok()
}

/// Draws the source rect onto the destination parallelogram: for every
/// destination pixel whose centre falls inside the quad, the inverse of the
/// parallelogram map gives the source point, which is sampled bilinearly
/// (GDI+'s default interpolation; edge taps clamp) and blended SourceOver.
/// Destination coverage is the centre test — GDI+ does not antialias the
/// edges of a `DrawImage` parallelogram.
fn rasterize_image(
    source: &LayerBitmapView<'_>,
    dest: &mut LayerBitmapViewMut<'_>,
    corners: &[Point],
    rect: (f64, f64, f64, f64),
) {
    let (sleft, stop, swidth, sheight) = rect;
    if swidth <= 0.0 || sheight <= 0.0 {
        return;
    }
    let origin = corners[0];
    let u_axis = (corners[1].x - origin.x, corners[1].y - origin.y);
    let v_axis = (corners[2].x - origin.x, corners[2].y - origin.y);
    let det = u_axis.0 * v_axis.1 - u_axis.1 * v_axis.0;
    if det.abs() <= f64::EPSILON {
        return;
    }

    let bitmap = dest.bitmap;
    let pitch = bitmap.pitch as usize;
    let (clip_left, clip_top, clip_width, clip_height) = clip_box(bitmap);
    let xs = corners.iter().map(|point| point.x);
    let ys = corners.iter().map(|point| point.y);
    let (min_x, max_x) = xs.fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), x| {
        (min.min(x), max.max(x))
    });
    let (min_y, max_y) = ys.fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), y| {
        (min.min(y), max.max(y))
    });
    let x0 = (min_x.floor() as i64).max(clip_left).max(0);
    let y0 = (min_y.floor() as i64).max(clip_top).max(0);
    let x1 = (max_x.ceil() as i64)
        .min(clip_left + clip_width)
        .min(i64::from(bitmap.width));
    let y1 = (max_y.ceil() as i64)
        .min(clip_top + clip_height)
        .min(i64::from(bitmap.height));

    for row in y0..y1 {
        for column in x0..x1 {
            let dx = column as f64 + 0.5 - origin.x;
            let dy = row as f64 + 0.5 - origin.y;
            let u = (dx * v_axis.1 - dy * v_axis.0) / det;
            let v = (u_axis.0 * dy - u_axis.1 * dx) / det;
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let colour = sample_bilinear(source, sleft + u * swidth, stop + v * sheight);
            let offset = row as usize * pitch + column as usize * 4;
            if let Some(pixel) = dest.pixels.get_mut(offset..offset + 4) {
                blend_rgba(pixel, colour, 1.0);
            }
        }
    }
}

/// Bilinear taps at the source point `(x, y)`, in the layer's pixel frame
/// (pixel `(i, j)` covers `[i, i+1) x [j, j+1)`); taps outside the image
/// clamp to its edge, and a sample exactly on a pixel centre returns that
/// pixel unchanged.
fn sample_bilinear(source: &LayerBitmapView<'_>, x: f64, y: f64) -> [f64; 4] {
    let fx = x - 0.5;
    let fy = y - 0.5;
    let left = fx.floor();
    let top = fy.floor();
    let tx = fx - left;
    let ty = fy - top;
    let (left, top) = (left as i64, top as i64);
    let p00 = source_pixel(source, left, top);
    let p10 = source_pixel(source, left + 1, top);
    let p01 = source_pixel(source, left, top + 1);
    let p11 = source_pixel(source, left + 1, top + 1);
    let mut colour = [0.0; 4];
    for channel in 0..4 {
        let top_mix = p00[channel] + (p10[channel] - p00[channel]) * tx;
        let bottom_mix = p01[channel] + (p11[channel] - p01[channel]) * tx;
        colour[channel] = top_mix + (bottom_mix - top_mix) * ty;
    }
    colour
}

/// One source pixel as `[R, G, B, A]` reals, clamped to the image.
fn source_pixel(source: &LayerBitmapView<'_>, x: i64, y: i64) -> [f64; 4] {
    let width = i64::from(source.bitmap.width);
    let height = i64::from(source.bitmap.height);
    let x = x.clamp(0, (width - 1).max(0));
    let y = y.clamp(0, (height - 1).max(0));
    let offset = y as usize * source.bitmap.pitch as usize + x as usize * 4;
    match source.pixels.get(offset..offset + 4) {
        Some(pixel) => [
            pixel[0].into(),
            pixel[1].into(),
            pixel[2].into(),
            pixel[3].into(),
        ],
        None => [0.0; 4],
    }
}

// ---------------------------------------------------------------- metafile

/// `Layer.getRecordImage()` (`LayerExDraw.cpp:1884-1914`): replays the
/// recorded metafile into an `Image`. No metafile format exists here, so the
/// call warns once and answers void.
fn layer_get_record_image(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_record_stub(runtime);
    Ok(Variant::Void)
}

/// `Layer.redrawRecord()` (`LayerExDraw.cpp:1919-1929`): replays the record
/// into the layer, answering whether an image was produced.
fn layer_redraw_record(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_record_stub(runtime);
    Ok(Variant::Integer(0))
}

/// `Layer.saveRecord(filename)` (`LayerExDraw.cpp:1936-1964`): writes the
/// metafile bytes; without a metafile the reference's `ret` stays false.
fn layer_save_record(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_record_stub(runtime);
    Ok(Variant::Integer(0))
}

/// `Layer.loadRecord(filename)` (`LayerExDraw.cpp:1972-1983`): the reference
/// **always returns false**, even after loading and redrawing the image; the
/// port answers false without loading anything.
fn layer_load_record(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_record_stub(runtime);
    Ok(Variant::Integer(0))
}

fn warn_record_stub(runtime: &mut Runtime<KrkrHost>) {
    warn_once(
        runtime,
        "record",
        "metafile recording is not implemented: `record = true` records nothing and \
         getRecordImage/saveRecord/redrawRecord have nothing to answer",
    );
}

// ---------------------------------------------------------------- saveImage

/// `Layer.saveImage(filename[, mime[, params]])` (`LayerExDraw.cpp:2280-2319`):
/// a raw callback that picks a GDI+ encoder by MIME type (default
/// `image/bmp`), throws `unknown format:%1` for an unsupported one and
/// answers whether the save succeeded. This engine exposes no image encoder
/// to plugin code, so the call warns once and answers false.
fn layer_save_image(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_once(
        runtime,
        "save-image",
        "saveImage is not implemented: this engine's image encoders are not reachable from \
         plugin code, so the call answers false (the reference would write the file)",
    );
    Ok(Variant::Integer(0))
}

// ---------------------------------------------------------------- regions

/// `Layer.getColorRegionRects(color)` (`LayerExDraw.cpp:2328-2373`): scans
/// the bitmap for pixels equal to the ARGB colour, unions each row's runs into
/// a `Region` and answers its scan rectangles. The port answers the same
/// scanline runs merged vertically where they are identical, which is the
/// covered area's maximal horizontal decomposition — GDI+'s region scan
/// conversion can merge differently (documented divergence).
fn layer_get_color_region_rects(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let colour = arg_int(&args, 0) as u32;
    let runs = layer_bitmap_read(runtime, layer, |view| {
        let width = i64::from(view.bitmap.width);
        let height = i64::from(view.bitmap.height);
        let pitch = view.bitmap.pitch as usize;
        let mut runs: Vec<(i64, i64, i64)> = Vec::new();
        for y in 0..height {
            let row = y as usize * pitch;
            let mut x = 0;
            while x < width {
                let offset = row + x as usize * 4;
                if pixel_argb(&view.pixels[offset..offset + 4]) == colour {
                    let start = x;
                    while x < width {
                        let offset = row + x as usize * 4;
                        if pixel_argb(&view.pixels[offset..offset + 4]) != colour {
                            break;
                        }
                        x += 1;
                    }
                    runs.push((start, y, x - start));
                } else {
                    x += 1;
                }
            }
        }
        runs
    })?;

    let mut rects: Vec<(i64, i64, i64, i64)> = Vec::new();
    let mut open: Vec<(i64, i64, usize)> = Vec::new();
    let mut next_open: Vec<(i64, i64, usize)> = Vec::new();
    let mut current_row = i64::MIN;
    for (x, y, width) in runs {
        if y != current_row {
            current_row = y;
            open = std::mem::take(&mut next_open);
            next_open = Vec::new();
        }
        match open
            .iter()
            .find(|(open_x, open_width, _)| *open_x == x && *open_width == width)
        {
            Some(&(_, _, index)) => {
                rects[index].3 += 1;
                next_open.push((x, width, index));
            }
            None => {
                rects.push((x, y, width, 1));
                next_open.push((x, width, rects.len() - 1));
            }
        }
    }

    let elements: Vec<Variant> = rects
        .into_iter()
        .map(|(x, y, width, height)| {
            let values = vec![
                Variant::Real(x as f64),
                Variant::Real(y as f64),
                Variant::Real(width as f64),
                Variant::Real(height as f64),
            ];
            Variant::Object(runtime.alloc_array_object(values))
        })
        .collect();
    Ok(Variant::Object(runtime.alloc_array_object(elements)))
}

/// The reference's `getColor(Bitmap*, x, y)` value: the engine's R, G, B, A
/// pixel packed back into the ARGB dword (`LayerExDraw.cpp:2321-2326`).
fn pixel_argb(pixel: &[u8]) -> u32 {
    (u32::from(pixel[3]) << 24)
        | (u32::from(pixel[0]) << 16)
        | (u32::from(pixel[1]) << 8)
        | u32::from(pixel[2])
}

// ---------------------------------------------------------------- helpers

/// The layer a method acts on; a method called with no `this` is a script
/// error, not a crash (the reference dereferences null).
fn this_layer(this_obj: Option<ObjectHandle>) -> Result<ObjectHandle> {
    this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))
}

/// Registers a native method on the engine-owned `Layer` class unless a
/// script already defined that member as a Closure.
fn register_unless_closure(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &'static str,
    arg_count: usize,
    function: impl NativeFunction<KrkrHost> + 'static,
) {
    if matches!(runtime.object_member(object, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native_with_arg_count(
        object,
        name,
        NativeArgCount::AtLeast(arg_count),
        function,
    );
}

fn arg_real(args: &[Variant], index: usize) -> f64 {
    args.get(index)
        .map(|value| value.to_real().unwrap_or(0.0))
        .unwrap_or(0.0)
}

fn arg_int(args: &[Variant], index: usize) -> i64 {
    args.get(index)
        .map(|value| value.to_integer().unwrap_or(0))
        .unwrap_or(0)
}

fn arg_string(args: &[Variant], index: usize) -> String {
    args.get(index)
        .map(|value| value.to_tjs_string().unwrap_or_default())
        .unwrap_or_default()
}

/// The `index`-th element of an array as a Real (a missing element is 0, as
/// the reference's `ncbPropAccessor::getRealValue`).
fn element_real(values: &[Variant], index: usize) -> f64 {
    values
        .get(index)
        .map(|value| value.to_real().unwrap_or(0.0))
        .unwrap_or(0.0)
}

fn object_real(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> Option<f64> {
    runtime.object_member(object, name).to_real().ok()
}

fn object_has_member(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> bool {
    !matches!(runtime.object_member(object, name), Variant::Void)
}

/// The object a native method's `this` (or a state-carrying argument) really
/// is: a `new`-created instance arrives as a self-bound proxy whose bound
/// target owns the members, and `object_member`/`set_object_member` address
/// the proxy's own slot without this step.
fn resolved_this(
    runtime: &Runtime<KrkrHost>,
    handle: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    handle.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

/// A read-only native property: the reference declares these with
/// `NCB_PROPERTY_RO`, whose setter is `TJS_DENY_NATIVE_PROP_SETTER`, and a
/// script write answers `TJS_E_ACCESSDENYED` — the engine's
/// [`NativePropertyAccess::ReadOnly`].
fn register_readonly_property<G>(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: impl Into<String>,
    getter: G,
) where
    G: Fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>) -> Result<Variant> + Send + Sync + 'static,
{
    runtime.register_object_native_property_with_access(
        object,
        name,
        NativePropertyAccess::ReadOnly,
        getter,
        keep_setter,
    );
}

fn this_real(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, name: &str) -> f64 {
    match resolved_this(runtime, this_obj) {
        Some(handle) => runtime.object_member(handle, name).to_real().unwrap_or(0.0),
        None => 0.0,
    }
}

fn variant_real(runtime: &Runtime<KrkrHost>, value: Option<&Variant>, name: &str) -> f64 {
    // PointF/Matrix arguments may be `new` results (self-bound closures).
    value
        .and_then(Variant::object_handle)
        .map(|handle| runtime.object_member(handle, name).to_real().unwrap_or(0.0))
        .unwrap_or(0.0)
}

/// `getPoint` (`main.cpp:132-138`): a PointF instance, an array `[x, y]`, or a
/// dictionary with `x`/`y`; anything else is `(0, 0)`, the converter's
/// `T()` fallback.
fn point_from_variant(runtime: &Runtime<KrkrHost>, value: Option<&Variant>) -> Point {
    let Some(handle) = value.and_then(Variant::object_handle) else {
        return Point::new(0.0, 0.0);
    };
    if let Some(elements) = runtime.array_elements(handle) {
        return Point::new(element_real(elements, 0), element_real(elements, 1));
    }
    Point::new(
        object_real(runtime, handle, "x").unwrap_or(0.0),
        object_real(runtime, handle, "y").unwrap_or(0.0),
    )
}

/// `getPoints` (`LayerExDraw.cpp:486-496`): the argument is an array of
/// point-shaped values; a non-array has no points.
fn points_from_variant(runtime: &Runtime<KrkrHost>, value: Option<&Variant>) -> Vec<Point> {
    let Some(handle) = value.and_then(Variant::object_handle) else {
        return Vec::new();
    };
    let Some(elements) = runtime.array_elements(handle) else {
        return Vec::new();
    };
    elements
        .iter()
        .map(|element| point_from_variant(runtime, Some(element)))
        .collect()
}

/// `getRects` (`LayerExDraw.cpp:524-534`): an array of rect-shaped values.
fn rects_from_variant(runtime: &Runtime<KrkrHost>, value: Option<&Variant>) -> Vec<[f64; 4]> {
    let Some(handle) = value.and_then(Variant::object_handle) else {
        return Vec::new();
    };
    let Some(elements) = runtime.array_elements(handle) else {
        return Vec::new();
    };
    elements
        .iter()
        .map(|element| variant_rect(runtime, Some(element)))
        .collect()
}

/// Reads an object's flat numeric member, `None` when the member is missing
/// or is not one of our arrays.
fn read_number_member(
    runtime: &Runtime<KrkrHost>,
    owner: ObjectHandle,
    name: &str,
) -> Option<Vec<f64>> {
    let owner = resolved_this(runtime, Some(owner))?;
    let array = runtime.object_member(owner, name).object_handle()?;
    let array = resolved_this(runtime, Some(array)).unwrap_or(array);
    let elements = runtime.array_elements(array)?;
    Some(
        elements
            .iter()
            .map(|value| value.to_real().unwrap_or(0.0))
            .collect(),
    )
}

fn store_number_member(
    runtime: &mut Runtime<KrkrHost>,
    owner: ObjectHandle,
    name: &str,
    values: &[f64],
) {
    let owner = resolved_this(runtime, Some(owner)).unwrap_or(owner);
    let array =
        runtime.alloc_array_object(values.iter().map(|value| Variant::Real(*value)).collect());
    runtime.set_object_member(owner, name, Variant::Object(array));
}

/// Reads a `Path` object's figures, `None` when the object was not built by
/// this plugin's `Path` class.
fn read_figures(runtime: &Runtime<KrkrHost>, owner: ObjectHandle) -> Option<Vec<Figure>> {
    let owner = resolved_this(runtime, Some(owner))?;
    let outer = runtime.object_member(owner, "__figures").object_handle()?;
    let outer = resolved_this(runtime, Some(outer)).unwrap_or(outer);
    let elements = runtime.array_elements(outer)?.to_vec();
    let mut figures = Vec::new();
    for element in elements {
        let Some(inner) = element.object_handle() else {
            continue;
        };
        let inner = resolved_this(runtime, Some(inner)).unwrap_or(inner);
        let Some(values) = runtime.array_elements(inner) else {
            continue;
        };
        if values.is_empty() {
            continue;
        }
        let closed = values[0].to_integer().unwrap_or(0) != 0;
        let points = values[1..]
            .chunks(2)
            .filter(|chunk| chunk.len() == 2)
            .map(|chunk| {
                Point::new(
                    chunk[0].to_real().unwrap_or(0.0),
                    chunk[1].to_real().unwrap_or(0.0),
                )
            })
            .collect();
        figures.push(Figure { points, closed });
    }
    Some(figures)
}

/// Writes a `Path` object's figures back into its `__figures` member.
fn store_figures(runtime: &mut Runtime<KrkrHost>, owner: ObjectHandle, figures: &[Figure]) {
    let owner = resolved_this(runtime, Some(owner)).unwrap_or(owner);
    let inner: Vec<Variant> = figures
        .iter()
        .map(|figure| {
            let mut values = Vec::with_capacity(1 + figure.points.len() * 2);
            values.push(Variant::Real(if figure.closed { 1.0 } else { 0.0 }));
            for point in &figure.points {
                values.push(Variant::Real(point.x));
                values.push(Variant::Real(point.y));
            }
            Variant::Object(runtime.alloc_array_object(values))
        })
        .collect();
    let outer = runtime.alloc_array_object(inner);
    runtime.set_object_member(owner, "__figures", Variant::Object(outer));
}

/// A `Path` argument of `Layer.drawPath`: the object's figures, `None` when
/// it is not a `GdiPlus.Path`.
fn path_figures_of(runtime: &Runtime<KrkrHost>, owner: ObjectHandle) -> Option<Vec<Figure>> {
    read_figures(runtime, owner)
}

/// An `Appearance` argument's draw list, `None` when the object is not a
/// `GdiPlus.Appearance`.
fn appearance_infos_of(runtime: &Runtime<KrkrHost>, owner: ObjectHandle) -> Option<Vec<DrawInfo>> {
    let values = read_number_member(runtime, owner, "__drawInfos")?;
    Some(decode_draw_infos(&values))
}

/// Setter for read-only native properties: ignores the assigned value.
fn keep_setter(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _value: Variant,
) -> Result<()> {
    Ok(())
}

fn real_zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Real(0.0))
}

fn zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

/// Constructor receiver: use the VM-provided `this` when it is a real
/// instance object, otherwise allocate a fresh one (motion_player idiom).
fn fresh_instance(runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> ObjectHandle {
    this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object())
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::{ObjectHandle, Variant};

    use super::{GDIPLUS_ENUM_CONSTANTS, LayerExDrawPlugin};

    /// A 6x6 transparent layer plus a reusable appearance; every geometry
    /// test works on this canvas, where GDI+ pixel `(x, y)` is the square
    /// `[x, x+1) x [y, y+1)`.
    const CANVAS: &str = r#"
        global.layer = new Layer();
        layer.setImageSize(6, 6);
        layer.fillRect(0, 0, 6, 6, 0x00000000);
        global.app = new GdiPlus.Appearance();
    "#;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(LayerExDrawPlugin).expect("plugin");
        engine.execute_script("canvas.tjs", CANVAS).expect("canvas");
        engine
    }

    fn run(engine: &mut KrkrEngine, name: &str, script: &str) {
        engine.execute_script(name, script).expect(name);
    }

    fn expression(engine: &mut KrkrEngine, script: &str) -> Variant {
        engine.execute_expression("read.tjs", script).expect(script)
    }

    fn integer(engine: &mut KrkrEngine, script: &str) -> i64 {
        expression(engine, script).to_integer().expect("integer")
    }

    /// `getMainPixel`: the engine's 0xRRGGBB of the layer pixel.
    fn pixel(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        integer(engine, &format!("{layer}.getMainPixel({x}, {y})"))
    }

    /// `getMaskPixel`: the layer pixel's alpha.
    fn alpha(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        integer(engine, &format!("{layer}.getMaskPixel({x}, {y})"))
    }

    /// `Layer.update()` leaves `callOnPaint` set (`layer.rs` tests).
    fn call_on_paint(engine: &mut KrkrEngine, layer: &str) -> i64 {
        integer(engine, &format!("{layer}.callOnPaint"))
    }

    fn layer_handle(engine: &KrkrEngine, name: &str) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    fn member_names(engine: &KrkrEngine, object: ObjectHandle) -> Vec<String> {
        engine
            .tjs_runtime()
            .object_members(object)
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    fn is_callable_member(engine: &KrkrEngine, object: ObjectHandle, name: &str) -> bool {
        matches!(
            engine.tjs_runtime().object_member(object, name),
            Variant::Object(_) | Variant::Closure(_)
        )
    }

    /// Every `Layer` member of `main.cpp:860-936` plus the two krkr2-only ones
    /// (`main.cpp:592, 916`), with the reference's argument counts.
    #[test]
    fn the_layer_surface_is_the_reference_member_table() {
        let mut engine = engine();
        let layer = layer_handle(&engine, "Layer");

        let methods = [
            "setViewTransform",
            "resetViewTransform",
            "rotateViewTransform",
            "scaleViewTransform",
            "translateViewTransform",
            "setTransform",
            "resetTransform",
            "rotateTransform",
            "scaleTransform",
            "translateTransform",
            "clear",
            "drawPath",
            "drawArc",
            "drawPie",
            "drawBezier",
            "drawBeziers",
            "drawClosedCurve",
            "drawClosedCurve2",
            "drawCurve",
            "drawCurve2",
            "drawCurve3",
            "drawEllipse",
            "drawLine",
            "drawLines",
            "drawPolygon",
            "drawRectangle",
            "drawRectangles",
            "drawPathString",
            "drawString",
            "measureString",
            "measureStringInternal",
            "drawImage",
            "drawImageRect",
            "drawImageStretch",
            "drawImageAffine",
            "getRecordImage",
            "redrawRecord",
            "saveRecord",
            "loadRecord",
            "saveImage",
            "getColorRegionRects",
        ];
        for name in methods {
            assert!(
                is_callable_member(&engine, layer, name),
                "Layer.{name} is not registered"
            );
        }
        // The four properties (`main.cpp:861-863, 908`) with the reference's
        // defaults (`LayerExDraw.cpp:954-956`).
        assert_eq!(integer(&mut engine, "layer.updateWhenDraw"), 1);
        assert_eq!(integer(&mut engine, "layer.smoothingMode"), 4);
        assert_eq!(integer(&mut engine, "layer.textRenderingHint"), 4);
        assert_eq!(integer(&mut engine, "layer.record"), 0);

        // The sixteen EncoderValue constants (`main.cpp:918-934`).
        assert_eq!(integer(&mut engine, "layer.EncoderValueCompressionLZW"), 2);
        assert_eq!(
            integer(&mut engine, "layer.EncoderValueTransformFlipVertical"),
            17
        );
    }

    /// The `GdiPlus` namespace: the enum block (`main.cpp:601-773`), the two
    /// statics and the seven subclasses.
    #[test]
    fn the_gdiplus_namespace_keeps_its_enum_and_class_surface() {
        let mut engine = engine();
        // 154 in the `GdiPlus` block (`main.cpp:601-773`) plus the 16
        // `EncoderValue`s on `Layer` (`:918-934`) are the reference's 170.
        assert_eq!(
            GDIPLUS_ENUM_CONSTANTS.len(),
            154,
            "the reference's GdiPlus enum block"
        );
        for &(name, value) in GDIPLUS_ENUM_CONSTANTS {
            assert_eq!(
                integer(&mut engine, &format!("GdiPlus.{name}")),
                value,
                "GdiPlus.{name}"
            );
        }
        assert_eq!(integer(&mut engine, "GdiPlus.SmoothingModeAntiAlias"), 4);
        assert_eq!(integer(&mut engine, "GdiPlus.BrushTypeLinearGradient"), 4);
        assert_eq!(integer(&mut engine, "GdiPlus.LineJoinRound"), 2);
        for name in [
            "PointF",
            "RectF",
            "Matrix",
            "Image",
            "Font",
            "Appearance",
            "Path",
        ] {
            let gdiplus = expression(&mut engine, "GdiPlus")
                .object_handle()
                .expect("GdiPlus");
            assert!(
                matches!(
                    engine.tjs_runtime().object_member(gdiplus, name),
                    Variant::Object(_)
                ),
                "GdiPlus.{name} is not an object"
            );
        }

        // The members live on the class objects (`GdiPlus.Appearance` etc.),
        // which is what ncbind's subclass registration builds.
        let appearance = expression(&mut engine, "GdiPlus.Appearance")
            .object_handle()
            .expect("Appearance class");
        for name in ["clear", "addBrush", "addPen"] {
            assert!(is_callable_member(&engine, appearance, name), "{name}");
        }

        let path = expression(&mut engine, "GdiPlus.Path")
            .object_handle()
            .expect("Path class");
        for name in [
            "startFigure",
            "closeFigure",
            "drawArc",
            "drawPie",
            "drawBezier",
            "drawBeziers",
            "drawClosedCurve",
            "drawClosedCurve2",
            "drawCurve",
            "drawCurve2",
            "drawCurve3",
            "drawEllipse",
            "drawLine",
            "drawLines",
            "drawPolygon",
            "drawRectangle",
            "drawRectangles",
            "drawPath",
        ] {
            assert!(is_callable_member(&engine, path, name), "Path.{name}");
        }
        assert!(
            member_names(&engine, path)
                .iter()
                .any(|n| n.starts_with("draw")),
            "the Path object carries its draw members"
        );
    }

    /// The reference's ncbind check is `numparams < declared` (`ncbind.hpp:1186`),
    /// so a short call is `TJS_E_BADPARAMCOUNT`. The counts are the members'
    /// PMFs — `PointF::Equals` is `BOOL PointF::Equals(const PointF&) const`
    /// (`layerExDraw/main.cpp:129`, `NCB_METHOD(Equals)`), so one argument.
    #[test]
    fn short_calls_are_bad_parameter_counts() {
        let mut engine = engine();
        for (name, script) in [
            ("drawLine", "layer.drawLine(app, 0, 0, 1);"),
            ("drawArc", "layer.drawArc(app, 0, 0, 1, 1, 0);"),
            ("drawBezier", "layer.drawBezier(app, 0, 0, 0, 0, 0, 0, 0);"),
            ("drawImage", "layer.drawImage(0, 0);"),
            ("saveRecord", "layer.saveRecord();"),
            ("saveImage", "layer.saveImage();"),
            ("getColorRegionRects", "layer.getColorRegionRects();"),
            ("addPen", "app.addPen(0xffffffff, 1, 0);"),
            ("addBrush", "app.addBrush(0xff00ff00, 0);"),
            (
                "Path.drawLine",
                "var p = new GdiPlus.Path(); p.drawLine(0, 0, 1);",
            ),
            (
                "Path.drawArc",
                "var p = new GdiPlus.Path(); p.drawArc(0, 0, 1, 1, 0);",
            ),
            (
                "PointF.Equals",
                "var p = new GdiPlus.PointF(0, 0); p.Equals();",
            ),
        ] {
            let error = engine.execute_script("short.tjs", script).expect_err(name);
            assert_eq!(
                error.kind,
                krkr_tjs2::TjsErrorKind::BadParamCount,
                "{name}: {}",
                error.message
            );
            assert_eq!(
                error.kind.tjs_error_code(),
                Some(-1004),
                "{name} must be the reference's TJS_E_BADPARAMCOUNT"
            );
        }

        // The reference's own call shape (`manual.tjs:354`) still works: one
        // PointF compares both coordinates, and extra arguments are ignored.
        assert_eq!(
            integer(
                &mut engine,
                "(function() {\n\
                     var p = new GdiPlus.PointF(1, 2);\n\
                     var same = p.Equals(new GdiPlus.PointF(1, 2));\n\
                     var other = p.Equals(new GdiPlus.PointF(1, 3));\n\
                     var extra = p.Equals(new GdiPlus.PointF(1, 2), 1);\n\
                     return same * 10 + other + extra;\n\
                 })()"
            ),
            11
        );
    }

    /// A straight one-pixel line with integer ends: GDI+'s flat cap ends the
    /// stroke exactly at the endpoints, so the pixels `x = 0..3` of row 0 are
    /// covered with no partial coverage — the stroke rectangle is
    /// `[0, 4] x [0, 1]`.
    #[test]
    fn a_straight_line_covers_its_pixel_row() {
        let mut engine = engine();
        run(
            &mut engine,
            "line.tjs",
            "app.addPen(0xffffffff, 1, 0, 0);\n\
             global.rect = layer.drawLine(app, 0, 0.5, 4, 0.5);",
        );
        for x in 0..4 {
            assert_eq!(pixel(&mut engine, "layer", x, 0), 0xff_ffff, "x={x}");
            assert_eq!(alpha(&mut engine, "layer", x, 0), 255, "x={x}");
        }
        assert_eq!(
            alpha(&mut engine, "layer", 4, 0),
            0,
            "the stroke ends at x=4"
        );
        assert_eq!(alpha(&mut engine, "layer", 0, 1), 0, "one pixel tall");
        // `GraphicsPath::GetBounds` with the pen: the segment inflated by the
        // half width on both axes (`LayerExDraw.cpp:1229`).
        assert_eq!(integer(&mut engine, "rect.x"), 0);
        assert_eq!(integer(&mut engine, "rect.y"), 0);
        assert_eq!(integer(&mut engine, "rect.width"), 4);
        assert_eq!(integer(&mut engine, "rect.height"), 1);
        assert_eq!(call_on_paint(&mut engine, "layer"), 1, "Layer.update ran");
    }

    /// Half-integer endpoints make the end pixels half covered: the covered
    /// area of column 0 is `1 - 0.5 = 0.5` of the pixel square, so its alpha
    /// is `round(0.5 * 255) = 128`. The middle columns are fully covered.
    #[test]
    fn a_shifted_line_antialiases_its_half_covered_ends() {
        let mut engine = engine();
        run(
            &mut engine,
            "line.tjs",
            "app.addPen(0xffffffff, 1, 0, 0);\n\
             layer.drawLine(app, 0.5, 0.5, 3.5, 0.5);",
        );
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 128, "half of column 0");
        assert_eq!(alpha(&mut engine, "layer", 1, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 2, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 3, 0), 128, "half of column 3");
        assert_eq!(alpha(&mut engine, "layer", 4, 0), 0);
        assert_eq!(pixel(&mut engine, "layer", 1, 0), 0xff_ffff);
    }

    /// A pen width of 2 covers two rows: the segment at `y = 1` spans
    /// `y in [0, 2]`.
    #[test]
    fn the_pen_width_thickens_the_stroke() {
        let mut engine = engine();
        run(
            &mut engine,
            "line.tjs",
            "app.addPen(0xffffffff, 2, 0, 0);\n\
             layer.drawLine(app, 0, 1, 4, 1);",
        );
        for y in 0..2 {
            for x in 0..4 {
                assert_eq!(alpha(&mut engine, "layer", x, y), 255, "({x},{y})");
            }
        }
        assert_eq!(alpha(&mut engine, "layer", 0, 2), 0);
        assert_eq!(alpha(&mut engine, "layer", 4, 0), 0);
    }

    /// A filled triangle `(0,0) (4,0) (0,4)` under `FillModeAlternate`: the
    /// interior is fully covered, the hypotenuse cuts pixel `(3,0)` in half
    /// (the slice at height `y` ends at `x = 4 - y`, whose average over
    /// `y in (0,1)` is `3.5`), and the pixels beyond the hypotenuse stay
    /// transparent.
    #[test]
    fn a_filled_polygon_follows_the_even_odd_rule() {
        let mut engine = engine();
        run(
            &mut engine,
            "polygon.tjs",
            "app.addBrush(0xffff0000, 0, 0);\n\
             layer.drawPolygon(app, [[0, 0], [4, 0], [0, 4]]);",
        );
        assert_eq!(pixel(&mut engine, "layer", 0, 0), 0xff_0000);
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 2, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 3, 0), 128, "the hypotenuse");
        // The hypotenuse meets x = 0.5 at y = 3.5, so pixel (0,3) is half
        // covered: the slice at height y ends at 4 - y, whose average over
        // y in (3,4) is 0.5.
        assert_eq!(
            alpha(&mut engine, "layer", 0, 3),
            128,
            "the hypotenuse again"
        );
        assert_eq!(alpha(&mut engine, "layer", 0, 4), 0, "past the triangle");
        assert_eq!(alpha(&mut engine, "layer", 4, 0), 0);
        assert_eq!(
            alpha(&mut engine, "layer", 2, 1),
            128,
            "the hypotenuse again"
        );
    }

    /// Two overlapping rectangles in one path, GDI+'s default
    /// `FillModeAlternate`: the overlap is covered by both figures, so the
    /// parity is even and it stays unpainted — the reference's own fill rule,
    /// not a union of the rectangles.
    #[test]
    fn overlapping_figures_leave_their_overlap_unfilled() {
        let mut engine = engine();
        run(
            &mut engine,
            "overlap.tjs",
            "var p = new GdiPlus.Path();\n\
             p.drawRectangle(0, 0, 4, 4);\n\
             p.drawRectangle(2, 2, 4, 4);\n\
             app.addBrush(0xffff0000, 0, 0);\n\
             layer.drawPath(app, p);",
        );
        assert_eq!(
            alpha(&mut engine, "layer", 0, 0),
            255,
            "first rectangle only"
        );
        assert_eq!(
            alpha(&mut engine, "layer", 3, 1),
            255,
            "first rectangle only"
        );
        assert_eq!(alpha(&mut engine, "layer", 2, 2), 0, "the parity is even");
        assert_eq!(alpha(&mut engine, "layer", 3, 3), 0, "the whole overlap");
        assert_eq!(
            alpha(&mut engine, "layer", 4, 4),
            255,
            "second rectangle only"
        );
        assert_eq!(
            alpha(&mut engine, "layer", 5, 5),
            255,
            "second rectangle only"
        );
        assert_eq!(alpha(&mut engine, "layer", 5, 1), 0, "outside both");
    }

    /// The clip box (`Region(Rect(clipLeft, clipTop, clipWidth,
    /// clipHeight))`, `LayerExDraw.cpp:1006-1007`) bounds every draw and
    /// `clear`.
    #[test]
    fn drawing_and_clear_stay_inside_the_clip_box() {
        let mut engine = engine();
        run(
            &mut engine,
            "clip.tjs",
            "layer.setClip(1, 1, 2, 2);\n\
             app.addBrush(0xffff0000, 0, 0);\n\
             layer.drawRectangle(app, 0, 0, 6, 6);",
        );
        for (x, y) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
            assert_eq!(alpha(&mut engine, "layer", x, y), 255, "inside ({x},{y})");
        }
        for (x, y) in [(0, 0), (3, 1), (1, 3), (5, 5)] {
            assert_eq!(alpha(&mut engine, "layer", x, y), 0, "outside ({x},{y})");
        }

        run(&mut engine, "clear.tjs", "layer.clear(0xff00ff00);");
        for (x, y) in [(1, 1), (2, 2)] {
            assert_eq!(pixel(&mut engine, "layer", x, y), 0x00_ff00, "cleared");
        }
        assert_eq!(pixel(&mut engine, "layer", 0, 0), 0, "clear is clipped too");
    }

    /// `clear` replaces the pixels (`Graphics::Clear` ignores the composition
    /// mode) and calls the no-argument `Layer.update()`.
    #[test]
    fn clear_replaces_the_whole_surface() {
        let mut engine = engine();
        run(&mut engine, "clear.tjs", "layer.clear(0x80402010);");
        assert_eq!(pixel(&mut engine, "layer", 5, 5), 0x40_2010);
        assert_eq!(alpha(&mut engine, "layer", 5, 5), 0x80);
        assert_eq!(call_on_paint(&mut engine, "layer"), 1);
    }

    /// The transform stack: `translateTransform` moves the geometry, and a
    /// scale scales the pen with the path (`calcTransform` at
    /// `LayerExDraw.cpp:1064-1073`).
    #[test]
    fn the_transform_stack_moves_and_scales_the_stroke() {
        let mut engine = engine();
        run(
            &mut engine,
            "translate.tjs",
            "app.addPen(0xffffffff, 1, 0, 0);\n\
             layer.translateTransform(2, 0);\n\
             layer.drawLine(app, 0, 0.5, 2, 0.5);",
        );
        assert_eq!(alpha(&mut engine, "layer", 1, 0), 0);
        assert_eq!(alpha(&mut engine, "layer", 2, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 3, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 4, 0), 0);

        run(
            &mut engine,
            "scale.tjs",
            "layer.clear(0x00000000);\n\
             layer.resetTransform();\n\
             layer.scaleTransform(2, 2);\n\
             layer.drawLine(app, 0, 0.25, 1, 0.25);",
        );
        // The world-space stroke `y in [-0.25, 0.75]` scaled by 2 covers
        // `y in [-0.5, 1.5]`: row 0 fully, row 1 by half.
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 1, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 0, 1), 128);
        assert_eq!(alpha(&mut engine, "layer", 1, 1), 128);
        assert_eq!(alpha(&mut engine, "layer", 2, 0), 0, "x in [0, 2] only");
        assert_eq!(alpha(&mut engine, "layer", 0, 2), 0);
    }

    /// The view transform composes with the transform: `Layer.update` is the
    /// port's `updateRect` step, and the appearance's `(ox, oy)` offset is
    /// prepended to both (`LayerExDraw.cpp:1218, 1227`).
    #[test]
    fn the_appearance_offset_and_view_transform_apply() {
        let mut engine = engine();
        run(
            &mut engine,
            "offset.tjs",
            "app.addPen(0xff0000ff, 1, 2, 0);\n\
             layer.drawLine(app, 0, 0.5, 2, 0.5);",
        );
        assert_eq!(pixel(&mut engine, "layer", 2, 0), 0x00_00ff);
        assert_eq!(alpha(&mut engine, "layer", 3, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 0);

        run(
            &mut engine,
            "view.tjs",
            "layer.clear(0x00000000);\n\
             layer.setViewTransform(new GdiPlus.Matrix(1,0,0,1,1,1));\n\
             layer.drawLine(app, 0, 0.5, 2, 0.5);",
        );
        // The offset (2,0) still applies, then the view translate (1,1).
        assert_eq!(alpha(&mut engine, "layer", 3, 1), 255);
        assert_eq!(alpha(&mut engine, "layer", 4, 1), 255);
        assert_eq!(alpha(&mut engine, "layer", 2, 0), 0, "the view moved it");
    }

    /// `updateWhenDraw` gates only the repaint, not the drawing
    /// (`LayerExDraw.cpp:937-946`).
    #[test]
    fn update_when_draw_gates_the_repaint() {
        let mut engine = engine();
        run(
            &mut engine,
            "quiet.tjs",
            "layer.callOnPaint = 0;\n\
             layer.updateWhenDraw = 0;\n\
             app.addPen(0xffffffff, 1, 0, 0);\n\
             layer.drawLine(app, 0, 0.5, 4, 0.5);",
        );
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 255, "drawing still ran");
        assert_eq!(call_on_paint(&mut engine, "layer"), 0, "no repaint");

        run(&mut engine, "loud.tjs", "layer.updateWhenDraw = 1;");
        assert_eq!(integer(&mut engine, "layer.updateWhenDraw"), 1);
        assert_eq!(call_on_paint(&mut engine, "layer"), 0);
    }

    /// An appearance can carry several entries; each draws under its own
    /// offset, and an empty appearance still runs the repaint (`updateRect`
    /// with a zero rect, `LayerExDraw.cpp:1257`).
    #[test]
    fn an_appearance_holds_a_brush_and_a_pen_sequence() {
        let mut engine = engine();
        run(
            &mut engine,
            "sequence.tjs",
            "var app = new GdiPlus.Appearance();\n\
             app.addBrush(0xff00ff00, 0, 0);\n\
             app.addPen(0xffffffff, 1, 0, 0);\n\
             app.addPen(0xffffffff, 1, 0, 4);\n\
             layer.drawRectangle(app, 0, 0, 4, 4);",
        );
        // The brush fills the 4x4 rectangle, then the white pen strokes its
        // outline: the outline is centred on the rectangle's edge, so a
        // single-edge pixel is half covered (`(128, 255, 128)` over the green
        // interior), while the interior stays the fill's colour. The corner
        // pixel (0,0) is covered by both the top and the left edge, so its
        // stroke coverage is the *union* of the two half-covered bands,
        // `1 - 0.5*0.5 = 0.75` -> `0.75*255 + 0.25*0 = 191 = 0xBF`. The pen
        // offset by (0, 4) draws the outline again one pixel lower, where its
        // half-covered edge on row 4 meets the rectangle's own bottom edge.
        assert_eq!(pixel(&mut engine, "layer", 2, 2), 0x00_ff00, "the fill");
        assert_eq!(
            pixel(&mut engine, "layer", 0, 0),
            0xBF_FFBF,
            "the corner union"
        );
        // Interior rows carry the left and right edges; the left edge used to
        // be dropped by the span merge (see `merge_spans`).
        assert_eq!(
            pixel(&mut engine, "layer", 0, 2),
            0x80_FF80,
            "the left edge"
        );
        assert_eq!(
            pixel(&mut engine, "layer", 3, 2),
            0x80_FF80,
            "the right edge"
        );
        // Row 4 carries the rectangle's own bottom edge and the offset pen's
        // top edge: two half-covered white strokes composite to alpha
        // `0.5 + 0.502 * 0.5 = 0.75` (192), the reference drawing each
        // appearance entry in turn (`LayerExDraw.cpp:1204-1261`).
        assert_eq!(alpha(&mut engine, "layer", 2, 4), 192, "the offset pen");
        assert_eq!(alpha(&mut engine, "layer", 5, 0), 0);
    }

    /// The P1 regression: a stroked multi-piece path is the union of its
    /// pieces, so a piece lying to the *left* of an earlier one at a sampled
    /// row must survive. A 1-pixel pen strokes the rectangle's edges centred
    /// on them: at an interior row `y` the left edge covers `x in [0, 0.5]`
    /// of pixel 0 and the right edge `x in [3.5, 4]` of pixel 3, plus
    /// `[4, 4.5]` of pixel 4 (outside the fill, so it lands on transparency);
    /// the corner pixel (0,0) is the union of the two half-covered bands,
    /// `0.75 -> 0xBF` white over the green fill.
    #[test]
    fn a_stroked_rectangle_keeps_every_edge() {
        let mut engine = engine();
        run(
            &mut engine,
            "edges.tjs",
            "var app = new GdiPlus.Appearance();\n\
             app.addBrush(0xff00ff00, 0, 0);\n\
             app.addPen(0xffffffff, 1, 0, 0);\n\
             layer.drawRectangle(app, 0, 0, 4, 4);",
        );
        for row in 1..=2 {
            assert_eq!(
                pixel(&mut engine, "layer", 0, row),
                0x80_FF80,
                "left edge at row {row} (the dropped piece)"
            );
            assert_eq!(
                pixel(&mut engine, "layer", 3, row),
                0x80_FF80,
                "right edge at row {row}"
            );
            assert_eq!(alpha(&mut engine, "layer", 0, row), 255);
        }
        for column in 1..=2 {
            assert_eq!(
                pixel(&mut engine, "layer", column, 0),
                0x80_FF80,
                "top edge at column {column}"
            );
            assert_eq!(
                pixel(&mut engine, "layer", column, 3),
                0x80_FF80,
                "bottom edge at column {column}"
            );
        }
        assert_eq!(pixel(&mut engine, "layer", 0, 0), 0xBF_FFBF, "corner union");
        assert_eq!(pixel(&mut engine, "layer", 3, 0), 0xBF_FFBF, "corner union");
        assert_eq!(pixel(&mut engine, "layer", 2, 2), 0x00_FF00, "the fill");
        // The right edge's outer half lands on transparency, half covered.
        assert_eq!(alpha(&mut engine, "layer", 4, 2), 128);
        assert_eq!(pixel(&mut engine, "layer", 4, 2), 0xFF_FFFF);
        // The 90-degree miter join at (4,4) fills the outer square
        // [4, 4.5] x [4, 4.5] -- a quarter of pixel (4,4), white on
        // transparency.
        assert_eq!(alpha(&mut engine, "layer", 4, 4), 64, "the miter corner");
        assert_eq!(alpha(&mut engine, "layer", 5, 4), 0, "outside the stroke");
    }

    /// `getColorRegionRects` answers the colour's row runs merged vertically:
    /// a solid 2x2 block is one rectangle.
    #[test]
    fn color_region_rects_merge_identical_runs() {
        let mut engine = engine();
        run(
            &mut engine,
            "region.tjs",
            "layer.fillRect(1, 1, 2, 2, 0xffff0000);\n\
             global.rects = layer.getColorRegionRects(0xffff0000);",
        );
        assert_eq!(integer(&mut engine, "rects.length"), 1);
        assert_eq!(integer(&mut engine, "rects[0][0]"), 1);
        assert_eq!(integer(&mut engine, "rects[0][1]"), 1);
        assert_eq!(integer(&mut engine, "rects[0][2]"), 2);
        assert_eq!(integer(&mut engine, "rects[0][3]"), 2);
        assert_eq!(
            integer(&mut engine, "layer.getColorRegionRects(0xff00ff00).length"),
            0
        );
    }

    /// The reference's image converter answers `Image*` for a **`Layer`** as
    /// well as a GDI+ `Image` (`main.cpp:424-445`), so `drawImage*` samples a
    /// layer's pixels: a 1:1 copy at an integer offset is exact (bilinear
    /// taps land on pixel centres), a source sub-rect copies exactly, and a
    /// 2x stretch pins the quad's corners to the clamped source corners.
    #[test]
    fn draw_image_samples_a_layer_source() {
        let mut engine = engine();
        run(
            &mut engine,
            "source.tjs",
            "global.src = new Layer();\n\
             src.setImageSize(2, 2);\n\
             src.fillRect(0, 0, 1, 1, 0xffff0000);\n\
             src.fillRect(1, 0, 1, 1, 0xff00ff00);\n\
             src.fillRect(0, 1, 1, 1, 0xff0000ff);\n\
             src.fillRect(1, 1, 1, 1, 0xffffffff);",
        );

        run(
            &mut engine,
            "copy.tjs",
            "global.rect = layer.drawImage(1, 1, src);",
        );
        assert_eq!(pixel(&mut engine, "layer", 1, 1), 0xff_0000);
        assert_eq!(pixel(&mut engine, "layer", 2, 1), 0x00_ff00);
        assert_eq!(pixel(&mut engine, "layer", 1, 2), 0x00_00ff);
        assert_eq!(pixel(&mut engine, "layer", 2, 2), 0xff_ffff);
        assert_eq!(alpha(&mut engine, "layer", 2, 2), 255);
        assert_eq!(alpha(&mut engine, "layer", 3, 3), 0, "the copy's extent");
        assert_eq!(integer(&mut engine, "rect.width"), 2);
        assert_eq!(integer(&mut engine, "rect.height"), 2);

        // `drawImageRect`: the source's right column, one pixel wide.
        run(
            &mut engine,
            "rect.tjs",
            "layer.clear(0x00000000);\n\
             global.sub = layer.drawImageRect(0, 0, src, 1, 0, 1, 2);",
        );
        assert_eq!(pixel(&mut engine, "layer", 0, 0), 0x00_ff00);
        assert_eq!(pixel(&mut engine, "layer", 0, 1), 0xff_ffff);
        assert_eq!(alpha(&mut engine, "layer", 1, 0), 0);
        assert_eq!(integer(&mut engine, "sub.width"), 1);
        assert_eq!(integer(&mut engine, "sub.height"), 2);

        // `drawImageStretch`: 2x2 -> 4x4. The corner pixels clamp their taps
        // to the source corner (`(0.5, 0.5)` maps to source `(0.25, 0.25)`,
        // whose taps are all pixel (0,0); `(3.5, 3.5)` maps to `(1.75, 1.75)`,
        // taps 1 and 2 clamped to 1), so the corners are the source corners.
        run(
            &mut engine,
            "stretch.tjs",
            "layer.clear(0x00000000);\n\
             global.stretched = layer.drawImageStretch(0, 0, 4, 4, src, 0, 0, 2, 2);",
        );
        assert_eq!(pixel(&mut engine, "layer", 0, 0), 0xff_0000);
        assert_eq!(pixel(&mut engine, "layer", 3, 3), 0xff_ffff);
        assert_eq!(alpha(&mut engine, "layer", 4, 0), 0, "the quad's extent");
        assert_eq!(alpha(&mut engine, "layer", 0, 4), 0);
        assert_eq!(integer(&mut engine, "stretched.width"), 4);
        assert_eq!(integer(&mut engine, "stretched.height"), 4);

        // The destination's clip box still bounds the copy: the source is
        // 2x2, so a clip covering only (1,1) keeps that one pixel (the source
        // corner (1,1)) and drops the rest of the copy.
        run(
            &mut engine,
            "clip.tjs",
            "layer.clear(0x00000000);\n\
             layer.setClip(1, 1, 1, 1);\n\
             layer.drawImage(0, 0, src);\n\
             layer.setClip(0, 0, 6, 6);",
        );
        assert_eq!(
            pixel(&mut engine, "layer", 1, 1),
            0xff_ffff,
            "inside the clip"
        );
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 0, "outside the clip");
    }

    /// The stub members answer the reference's failure results and warn: text
    /// draws nothing and measures empty, image and record calls answer their
    /// zero results, `saveImage` answers false, and a `GdiPlus.Image` refuses
    /// a missing storage with the reference's `cannot open:%1`.
    #[test]
    fn the_unimplemented_members_answer_the_reference_failures() {
        let mut engine = engine();
        run(
            &mut engine,
            "stubs.tjs",
            "var font = new GdiPlus.Font(\"sans\", 12, 0);\n\
             var measured = layer.measureString(font, \"text\");\n\
             var drawn = layer.drawString(font, app, 0, 0, \"text\");\n\
             global.measured = measured.width + measured.height;\n\
             global.drawn = drawn.width + drawn.height;\n\
             global.image = layer.drawImage(0, 0, new GdiPlus.Image());\n\
             global.recorded = layer.redrawRecord() + layer.saveRecord(\"x\") + layer.loadRecord(\"x\");\n\
             global.saved = layer.saveImage(\"x.bmp\");",
        );
        assert_eq!(integer(&mut engine, "measured"), 0);
        assert_eq!(integer(&mut engine, "drawn"), 0);
        assert_eq!(integer(&mut engine, "image.width + image.height"), 0);
        assert_eq!(integer(&mut engine, "recorded"), 0);
        assert_eq!(integer(&mut engine, "saved"), 0, "saveImage answers false");
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 0, "nothing was drawn");

        let error = engine
            .execute_script("bad.tjs", "new GdiPlus.Image(\"missing.png\");")
            .expect_err("a missing storage");
        assert_eq!(error.message, "cannot open:missing.png");
    }

    /// A freed layer image is the family's `Not drawable layer type`, and the
    /// failed call must not resurrect the image nor repaint.
    #[test]
    fn a_freed_image_reports_not_drawable() {
        let mut engine = engine();
        run(
            &mut engine,
            "free.tjs",
            "layer.callOnPaint = 0;\n\
             app.addPen(0xffffffff, 1, 0, 0);\n\
             layer.freeImage();",
        );
        let error = engine
            .execute_script("bad.tjs", "layer.drawLine(app, 0, 0, 3, 0);")
            .expect_err("a freed image");
        assert_eq!(error.message, "Not drawable layer type");
        assert_eq!(
            integer(&mut engine, "layer.hasImage"),
            0,
            "the failed draw must not resurrect the image"
        );
        assert_eq!(call_on_paint(&mut engine, "layer"), 0);
    }

    /// Bad arguments are reported instead of dereferencing null the way the
    /// reference's converters would.
    #[test]
    fn non_objects_are_rejected_with_named_errors() {
        let mut engine = engine();
        let error = engine
            .execute_script("bad.tjs", "layer.drawLine(42, 0, 0, 1, 1);")
            .expect_err("a non-Appearance");
        assert_eq!(error.message, "invalid parameter: Appearance");

        run(&mut engine, "app.tjs", "app.addBrush(0xffffffff, 0, 0);");
        let error = engine
            .execute_script("bad.tjs", "layer.drawPath(app, 42);")
            .expect_err("a non-Path");
        assert_eq!(error.message, "invalid parameter: Path");

        let error = engine
            .execute_script("bad.tjs", "layer.setTransform(42);")
            .expect_err("a non-Matrix");
        assert_eq!(error.message, "invalid parameter: Matrix");

        // `createBrush`'s dispatch (`LayerExDraw.cpp:785-787`): an unknown
        // brush type throws `invalid brush type`.
        let error = engine
            .execute_script("bad.tjs", "app.addBrush(%[type: 9], 0, 0);")
            .expect_err("an unknown brush type");
        assert_eq!(error.message, "invalid brush type");

        // A non-solid brush type is accepted by the reference but has no
        // backend here: the entry is skipped, so the draw paints nothing.
        run(
            &mut engine,
            "gradient.tjs",
            "var app = new GdiPlus.Appearance();\n\
             app.addBrush(%[type: 4], 0, 0);\n\
             layer.drawRectangle(app, 0, 0, 4, 4);",
        );
        assert_eq!(alpha(&mut engine, "layer", 1, 1), 0);
    }

    /// The `Path` class accumulates figures across calls (`startFigure`,
    /// `closeFigure`) and `Layer.drawPath` strokes and fills them.
    #[test]
    fn the_path_class_accumulates_figures() {
        let mut engine = engine();
        run(
            &mut engine,
            "figures.tjs",
            "var p = new GdiPlus.Path();\n\
             p.startFigure();\n\
             p.drawLine(0, 0.5, 2, 0.5);\n\
             p.closeFigure();\n\
             p.drawLine(0, 2.5, 2, 2.5);\n\
             app.addPen(0xffffffff, 1, 0, 0);\n\
             var rect = layer.drawPath(app, p);",
        );
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 255, "the closed figure");
        assert_eq!(alpha(&mut engine, "layer", 1, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 0, 2), 255, "the second figure");
        assert_eq!(alpha(&mut engine, "layer", 3, 0), 0);
        assert_eq!(
            integer(&mut engine, "rect.width >= 2 && rect.height >= 2"),
            1,
            "the bounds cover both figures"
        );
    }

    /// `GdiPlus.RectF.Union(dst, a, b)` is GDI+'s static three-argument form
    /// (`BOOL Union(RectF& c, const RectF& a, const RectF& b)`, bound at
    /// `main.cpp:198`): it stores the union of `a` and `b` in the *first*
    /// argument and answers whether the result is non-empty. The two-argument
    /// instance form the previous diff used does not exist in the reference.
    #[test]
    fn rect_union_writes_into_its_first_argument() {
        let mut engine = engine();
        run(
            &mut engine,
            "union.tjs",
            "global.r = new GdiPlus.RectF(9, 9, 1, 1);\n\
             var a = new GdiPlus.RectF(0, 0, 2, 2);\n\
             var b = new GdiPlus.RectF(1, 1, 2, 2);\n\
             global.ok = GdiPlus.RectF.Union(r, a, b);\n\
             global.short = void;",
        );
        assert_eq!(integer(&mut engine, "r.x"), 0);
        assert_eq!(integer(&mut engine, "r.y"), 0);
        assert_eq!(integer(&mut engine, "r.width"), 3);
        assert_eq!(integer(&mut engine, "r.height"), 3);
        assert_eq!(integer(&mut engine, "ok"), 1);
        // The declared arity is the reference's three parameters, so the old
        // two-argument call is short.
        let error = engine
            .execute_script("short.tjs", "GdiPlus.RectF.Union(r, a);")
            .expect_err("two arguments");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        run(
            &mut engine,
            "empty.tjs",
            "var empty = new GdiPlus.RectF(0, 0, 0, 0);\n\
             global.empty = empty.IsEmptyArea();",
        );
        assert_eq!(integer(&mut engine, "empty"), 1);
    }

    /// `Matrix`'s `MatrixOrder` argument (`main.cpp:407-415`): the reference
    /// declares it on every `Multiply/Rotate/RotateAt/Scale/Shear/Translate`
    /// (GDI+ defaults are invisible to ncbind, `ncbind.hpp:1186`), so the
    /// argument is required; the default prepends, `Append` multiplies the
    /// other way around.
    #[test]
    fn matrix_order_selects_the_multiplication_side() {
        let mut engine = engine();
        run(
            &mut engine,
            "matrix.tjs",
            "global.prepend = new GdiPlus.Matrix(2, 0, 0, 2, 0, 0);\n\
             prepend.Multiply(new GdiPlus.Matrix(1, 0, 0, 1, 1, 1), 0);\n\
             global.append = new GdiPlus.Matrix(2, 0, 0, 2, 0, 0);\n\
             append.Multiply(new GdiPlus.Matrix(1, 0, 0, 1, 1, 1), 1);",
        );
        assert_eq!(integer(&mut engine, "prepend.OffsetX()"), 2);
        assert_eq!(integer(&mut engine, "prepend.OffsetY()"), 2);
        assert_eq!(integer(&mut engine, "append.OffsetX()"), 1);
        assert_eq!(integer(&mut engine, "append.OffsetY()"), 1);

        // The declared arity is the reference's two parameters.
        let error = engine
            .execute_script(
                "short.tjs",
                "var m = new GdiPlus.Matrix(1, 0, 0, 1, 0, 0);\n\
                 m.Multiply(new GdiPlus.Matrix(1, 0, 0, 1, 1, 1));",
            )
            .expect_err("one argument");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        let error = engine
            .execute_script(
                "short.tjs",
                "var m = new GdiPlus.Matrix(1, 0, 0, 1, 0, 0);\n\
                 m.Scale(2, 2);",
            )
            .expect_err("two arguments to Scale");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
    }

    /// The reference's read-only properties (`NCB_PROPERTY_RO`) deny a script
    /// write with `TJS_E_ACCESSDENYED`; they still read.
    #[test]
    fn read_only_geometry_properties_deny_writes() {
        let mut engine = engine();
        run(
            &mut engine,
            "readonly.tjs",
            "global.r = new GdiPlus.RectF(1, 2, 3, 4);\n\
             global.left = r.left;\n\
             global.font = new GdiPlus.Font(\"sans\", 12, 0);",
        );
        assert_eq!(integer(&mut engine, "left"), 1);
        for script in [
            "r.left = 9;",
            "r.right = 9;",
            "r.bounds = r;",
            "font.ascent = 9;",
        ] {
            let error = engine
                .execute_script("denied.tjs", script)
                .expect_err(script);
            assert_eq!(
                error.kind,
                krkr_tjs2::TjsErrorKind::AccessDenied,
                "{script}: {}",
                error.message
            );
        }
        // The engine's own writes bypass the policy, which is what the
        // constructors and `store_rect` rely on.
        assert_eq!(integer(&mut engine, "r.width"), 3);
    }

    /// The rasteriser's coverage model, exercised directly: a half-covered
    /// pixel blends with the destination, so a 50% white stroke over opaque
    /// red is `(255, 128, 128, 255)`, and a half-transparent source over a
    /// transparent destination keeps its colour with alpha 128.
    #[test]
    fn coverage_blends_into_the_destination() {
        let mut engine = engine();
        run(
            &mut engine,
            "blend.tjs",
            "layer.fillRect(0, 0, 6, 6, 0xffff0000);\n\
             app.addPen(0xffffffff, 1, 0, 0);\n\
             layer.drawLine(app, 0.5, 0.5, 1.5, 0.5);",
        );
        assert_eq!(
            pixel(&mut engine, "layer", 0, 0),
            0xff_8080,
            "half white over red"
        );
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 255);

        run(
            &mut engine,
            "alpha.tjs",
            "layer.clear(0x00000000);\n\
             var faded = new GdiPlus.Appearance();\n\
             faded.addPen(0x80ffffff, 1, 0, 0);\n\
             layer.drawLine(faded, 0, 0.5, 2, 0.5);",
        );
        assert_eq!(alpha(&mut engine, "layer", 0, 0), 128);
        assert_eq!(pixel(&mut engine, "layer", 0, 0), 0xff_ffff);
    }

    /// `smoothingMode = SmoothingModeNone` turns the antialiasing off: the
    /// half-covered end pixel is written whole when its centre is inside the
    /// stroke, and the pixel whose centre is outside stays empty.
    #[test]
    fn smoothing_mode_none_gives_hard_edges() {
        let mut engine = engine();
        run(
            &mut engine,
            "hard.tjs",
            "layer.smoothingMode = GdiPlus.SmoothingModeNone;\n\
             app.addPen(0xffffffff, 1, 0, 0);\n\
             layer.drawLine(app, 0.5, 0.5, 3.5, 0.5);",
        );
        assert_eq!(
            alpha(&mut engine, "layer", 0, 0),
            255,
            "centre x=0.5 is inside"
        );
        assert_eq!(
            alpha(&mut engine, "layer", 3, 0),
            0,
            "centre x=3.5 is outside"
        );
        assert_eq!(alpha(&mut engine, "layer", 1, 0), 255);
        assert_eq!(alpha(&mut engine, "layer", 2, 0), 255);
    }
}
