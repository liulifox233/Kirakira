//! `perspective.dll`: `Layer.perspectiveCopy`, the four-corner perspective blit.
//!
//! Real plugin: `krkrz/src/plugins/win32/layerExPerspective/` (upstream
//! <https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExPerspective>,
//! dossier `docs/plugins/layer-ex-family.md` §2.8). The build target keeps the
//! short name — `perspective.def` exports `V2Link`/`V2Unlink` from
//! `perspective.dll`, which is the spelling the catalog entry carries — while
//! the source folder is named after the family member.
//!
//! # The surface
//!
//! One member, attached to the global `Layer` **class object**:
//! `perspectiveCopy(src, left, top, width, height, x1, y1, x2, y2, x3, y3, x4,
//! y4)` — a `tTJSDispatch` (`Main.cpp:75-171`) placed with `addMethod`
//! (`:181-193`, called at `:233`). Thirteen parameters; fewer answers
//! `TJS_E_BADPARAMCOUNT` (`:84`). `V2Link` also registers a native *class id*
//! `LayerExBase` for the per-layer instance (`:216-218`) — an engine-internal
//! handle with no script-visible object, whose role in this engine is filled by
//! [`krkr_engine::plugin_api::layer`]'s scoped views; nothing a script can
//! name is missing.
//!
//! # What it draws
//!
//! The source layer's `left, top, width, height` rectangle (`:94-97`) is
//! rendered into the destination **quad** given by `(x1,y1)`…`(x4,y4)`. The
//! manual calls the corners 左上/右上/左下/右下 — top-left, top-right,
//! bottom-**left**, bottom-**right** (`manual.tjs`) — but the dispatch packs
//! them for AGG in the order TL, TR, BR, BL by reading `param[11]`/`param[12]`
//! into `quad[4]`/`quad[5]` and `param[9]`/`param[10]` into `quad[6]`/`quad[7]`
//! (`:99-107`), so the *script* order is TL, TR, BL, BR while the polygon AGG
//! sees is TL→TR→BR→BL. The port keeps the script order of
//! `layerExPerspective/Main.cpp`; the Kirikiroid2 port of the same plugin takes
//! the corners as TL, TR, LB, RB, which is the *same* script order
//! (`docs/plugins/layer-ex-family.md` §3), so no game sees a difference.
//!
//! The rectangle is mapped by `agg::trans_perspective` (`Main.cpp:139`), an
//! **inverse** projectivity from the quad back to the source rectangle, and the
//! quad is filled with an AGG scanline rasteriser (`:131-136`) whose spans are
//! sampled through a linear span interpolator over an adaptive-subdivision
//! adapter (`:141-144`) and a 2×2-tap image filter over a Hermite look-up
//! table (`:146-160`) that starts from a *transparent* background
//! (`agg::rgba_pre(0,0,0,0)`, `:158`). Destination pixels outside the quad are
//! never touched, and the whole call ends in `redraw()` — the reference's
//! `Layer.update(imageLeft, imageTop, imageWidth, imageHeight)`
//! (`layerExBase.cpp:92-102`) — **even when the transform was rejected**
//! (`Main.cpp:140, 168`).
//!
//! Note what the reference does *not* read: unlike `layerExRaster`'s
//! `copyRaster`, this member never consults the clip box. Its rasteriser is
//! clipped to the whole destination image (`:131`, `rubuf` at `:125`), the
//! scanline painter only writes where the quad covers, and the repaint covers
//! the layer's image. The port therefore ignores `bitmap.clip` here as well —
//! the clip box belongs to the *other* family members.
//!
//! # Where the numbers come from
//!
//! The reference links **AGG 2.3**, which its `readme.txt` makes the builder
//! vendor as `agg23/` and which is not part of the krkrz checkout. The port
//! below is written against the AGG 2.4-era headers of a public 2.6 mirror
//! (`agg-src/include/agg_*.h`, downloaded while writing this module), whose
//! arithmetic is the same lineage; the classes the reference names
//! (`trans_perspective`, `span_interpolator_linear`, `span_subdiv_adaptor`,
//! `span_image_filter_rgba_2x2`, `image_filter_lut` with
//! `image_filter_hermite`, `pixfmt_bgra32_pre`, `rasterizer_scanline_aa`) kept
//! their behaviour between 2.3 and 2.4, while the *template shape* of the 2x2
//! span filter changed (`color_type`/`order` parameters in 2.3, a source object
//! in 2.4). Every formula is cited below as `agg_*.h:line` of that mirror.
//! AGG's licence notice is preserved in [`AGG_NOTICE`], which the module also
//! logs at registration, the way `V2Link` prints it through
//! `TVPAddImportantLog` (`Main.cpp:9-18, 213`).
//!
//! * The projectivity is a direct port of `trans_perspective::square_to_quad`,
//!   `invert`, `quad_to_quad`, `quad_to_rect` and the `(quad, rect)`
//!   constructor, `transform` and `is_valid`
//!   (`agg_trans_perspective.h:287-335, 337-359, 371-379, 394-404, 416-421,
//!   571-578, 645-648`), including the `1e-14` tolerance of `is_valid` and the
//!   "all coefficients zero" state a rejected quad leaves behind.
//! * Spans are interpolated by `dda2_line_interpolator` (`agg_dda_line.h:85-208`)
//!   with the interpolator's `begin` endpoint rule — the span's end point is
//!   `transform(x + len, y)`, *not* the next pixel's centre
//!   (`agg_span_interpolator_linear.h:54-73`) — and the 0.5-pixel sample
//!   offsets of `span_image_filter` (`agg_span_image_filter.h:38-48`): sample at
//!   the destination pixel centre (`x + 0.5`), then shift the *source* result by
//!   `128` subpixel units (`agg_span_image_filter_rgba.h:451-452`).
//! * The 2×2 taps and their weights are `span_image_filter_rgba_2x2::generate`
//!   (`agg_span_image_filter_rgba.h:432-521`): the left/top tap takes its weight
//!   from `weight_array[256 + f]`, the right/bottom one from `weight_array[f]`
//!   (a symmetric table read backwards), each 2-D weight is
//!   `(wx * wy + 8192) >> 14`, the four products are accumulated as integers,
//!   shifted by `image_filter_shift` (14) and then clamped — alpha to 255 first,
//!   then every colour to the alpha (`:503-511`), the premultiplied invariant
//!   AGG's `span_image_filter_rgba` enforces on whatever bytes it was handed.
//! * The look-up table is `image_filter_lut(filter_kernel, false)` over
//!   `image_filter_hermite` — `calc_weight(x) = (2x - 3)·x² + 1`, radius 1,
//!   diameter 2, weights `iround(kernel(i/256) · 16384)` and **no
//!   normalisation** (`agg_image_filters.h:50-71, 146-153`). For two taps this
//!   is the Hermite/smoothstep pair `W(f)`, `W(1-f)`, which sums to exactly
//!   16384.
//! * The blend is `blender_rgba_pre::blend_pix` with a cover
//!   (`agg_pixfmt_rgba.h:207-238`): `mult_cover` every channel, then
//!   `p = prelerp(p, c, a)` per channel with `multiply(a,b) =
//!   ((a·b + 128) >> 8 + a·b + 128) >> 8` and `prelerp(p,q,a) = p + q -
//!   multiply(p,a)` in 8-bit arithmetic (`agg_color_rgba.h:395-448`), reached
//!   through `copy_or_blend_pix` (`agg_pixfmt_rgba.h:1591-1604`), which leaves
//!   a pixel whose sampled colour is fully transparent alone.
//!
//! # Divergences from the reference
//!
//! * **Coverage.** AGG's `rasterizer_scanline_aa` accumulates edge cells into
//!   1/256 cover units (`calculate_alpha`, `agg_rasterizer_scanline_aa.h:183-196`);
//!   the port computes the same quantity geometrically — the quad's exact
//!   intersection area with each pixel, `iround(area · 256)` clamped at
//!   `cover_full` (255) — instead of re-running AGG's cell accumulation. That is
//!   identical for full and empty pixels and for the axis-aligned quads games
//!   use; a slanted edge can differ by the last unit of AGG's own rounding. A
//!   **self-intersecting** quad falls back to a hard-edged non-zero-winding test
//!   at the pixel centre, where AGG would antialias: a bowtie quad is not
//!   producible by a sane script, and guessing at AGG's lobe decomposition would
//!   be a re-derivation, not a port.
//! * **Out-of-image taps.** AGG 2.3's 2x2 generator took the background colour
//!   for pixels outside the source (`agg::rgba_pre(0,0,0,0)` here, `Main.cpp:158`),
//!   so an edge sample fades to transparent instead of clamping to the border
//!   pixel (which is what a 2.4 `image_accessor_clone` source would do). The
//!   2.3 header is not on this machine, so that reading is *inferred* from the
//!   `back_color` argument the call site passes.
//! * **Crash paths become errors.** A source that is not a layer, or either
//!   layer without a main image, is a TJS error ("perspectiveCopy: src must be
//!   Layer.", the engine's "Not drawable layer type"); the reference
//!   dereferences the resulting null instance or buffer.
//! * **`iround` of a non-finite value.** `int(v < 0 ? v - 0.5 : v + 0.5)` is
//!   VCL's `cvttsd2si`, which yields `INT_MIN` for NaN and out-of-range values;
//!   the port reproduces that (`layer_ex_raster.rs` documents the same quirk)
//!   instead of Rust's saturating `as` cast.
//! * **Premultiplied bytes, the reference's own quirk.** The destination is
//!   declared `pixfmt_bgra32_pre` while `mainImageBufferForWrite` hands out the
//!   layer's raw bitmap, which is straight B,G,R,A — so the reference blends
//!   *premultiplied* over *straight* bytes, and so does this port: no silent
//!   conversion is inserted at the boundary, because that would change the
//!   output of every script that runs on the reference (an opaque source over a
//!   transparent destination is unaffected; a partially transparent source over
//!   an opaque destination is where the two conventions differ). The channel
//!   order is the only thing translated: `order_bgra` over the reference's
//!   buffer and R,G,B,A over this engine's store are the same arithmetic with
//!   the channels relabelled, and the clamps are channel-symmetric.
//! * **`Layer.update`.** The reference calls `update(imageLeft, imageTop,
//!   imageWidth, imageHeight)` (`layerExBase.cpp:92-102`); this port calls
//!   [`layer_update`], the engine's `Layer.update()`, which repaints the
//!   layer's whole rect (`classes.rs:7353` ignores its arguments) — a superset
//!   of the image rect the reference names.

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{
        LayerBitmapView, LayerBitmapViewMut, layer_bitmap_read_write, layer_update,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

/// Anti-Grain Geometry's licence notice, the text `V2Link` sends to
/// `TVPAddImportantLog` (`layerExPerspective/Main.cpp:9-18, 213`). AGG is
/// distributed under its own licence, which requires the notice to travel with
/// every copy, so it is both logged at registration and kept next to the ported
/// arithmetic below.
pub(crate) const AGG_NOTICE: &str = concat!(
    "----- AntiGrainGeometry Copyright START ----- ",
    "Anti-Grain Geometry - Version 2.3 Copyright (C) 2002-2005 Maxim Shemanarev (McSeem). ",
    "Permission to copy, use, modify, sell and distribute this software is granted provided ",
    "this copyright notice appears in all copies. This software is provided \"as is\" without ",
    "express or implied warranty, and with no claim as to its suitability for any purpose. ",
    "----- AntiGrainGeometry Copyright END -----"
);

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer.perspectiveCopy four-corner perspective blit",
    notes: "perspectiveCopy is a port of layerExPerspective/Main.cpp:84-170 over the engine's layer bitmap views: the source rectangle is mapped through AGG's trans_perspective quad_to_rect port, the quad is rasterised with geometric pixel-area coverage in AGG's 1/256 cover units, spans are sampled through a direct port of span_interpolator_linear's dda2 interpolator plus the 2x2 Hermite-LUT taps of span_image_filter_rgba_2x2, and each pixel is blended with blender_rgba_pre's premultiplied cover blend over the raw layer bytes (the reference's own straight-alpha/ premultiplied mismatch, reproduced rather than converted). A degenerate quad or source rectangle draws nothing and still repaints, as the reference does. AGG 2.3 itself is not on disk, so the third-party arithmetic is cited against the 2.4-era headers of a public mirror.",
    install: |engine| engine.register_plugin(LayerExPerspectivePlugin),
};

pub struct LayerExPerspectivePlugin;

impl KrkrPlugin for LayerExPerspectivePlugin {
    fn name(&self) -> &str {
        "perspective.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // `V2Link` announces the AGG copyright before touching the script
        // surface (`Main.cpp:213`).
        runtime.host_mut().log(AGG_NOTICE);
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `NCB_METHOD`-less `addMethod(dispatch, L"perspectiveCopy", ...)`
        // (`Main.cpp:233`): the only member this plugin adds, and the only
        // place a script can reach it.
        register_unless_closure(
            runtime,
            layer,
            "perspectiveCopy",
            NativeArgCount::AtLeast(13),
            layer_perspective_copy,
        );
        Ok(())
    }
}

/// `perspectiveCopy` (`layerExPerspective/Main.cpp:79-170`).
fn layer_perspective_copy(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = this_layer(this_obj)?;
    let Some(src) = args.first().and_then(Variant::object_handle) else {
        return Err(TjsError::runtime("perspectiveCopy: src must be Layer."));
    };

    // `:94-97`: the source rectangle is `left/top/width/height`, the corner is
    // computed as `left + width` / `top + height` (not read as a right/bottom).
    let left = arg_real(&args, 1)?;
    let top = arg_real(&args, 2)?;
    let right = left + arg_real(&args, 3)?;
    let bottom = top + arg_real(&args, 4)?;

    // `:99-107`: the quad in AGG's corner order, from the script's
    // TL, TR, BL, BR parameter order.
    let quad = [
        arg_real(&args, 5)?,
        arg_real(&args, 6)?,
        arg_real(&args, 7)?,
        arg_real(&args, 8)?,
        arg_real(&args, 11)?,
        arg_real(&args, 12)?,
        arg_real(&args, 9)?,
        arg_real(&args, 10)?,
    ];

    // `:139`: `trans_perspective tr(quad, g_x1, g_y1, g_x2, g_y2)` — the
    // inverse map the sampler needs, destination quad → source rectangle. A
    // rejected quad leaves the all-zero matrix, which `is_valid` rejects.
    let transform = TransPerspective::quad_to_rect(&quad, left, top, right, bottom);
    if transform.is_valid(AFFINE_EPSILON) {
        // `:110-166`: AGG reads and writes the two layers' whole images. The
        // views are the engine's scoped equivalents of
        // `mainImageBufferForWrite` + `mainImageBufferPitch`
        // (`layerExBase.cpp:79-85`), and a layer without an image fails here
        // with "Not drawable layer type" where the reference would crash.
        layer_bitmap_read_write(runtime, src, dest, |source, dest_view| {
            draw(source, dest_view, &quad, &transform);
        })?;
    }

    // `:168`: `dest->redraw(objthis)` runs for every call, including one whose
    // transform was rejected (`:140`), so the layer repaints either way.
    layer_update(runtime, dest)?;
    Ok(Variant::Void)
}

/// The destination layer the member was called on.
fn this_layer(this_obj: Option<ObjectHandle>) -> Result<ObjectHandle> {
    this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))
}

/// `param[i]->AsReal()` (`Main.cpp:94-107`): a missing argument is `void`, which
/// converts to 0.
fn arg_real(args: &[Variant], index: usize) -> Result<f64> {
    args.get(index)
        .map(Variant::to_real)
        .transpose()
        .map(|value| value.unwrap_or(0.0))
}

/// Registers `function` unless a script already owns the member, the way
/// `layer_ex_raster.rs` and the rest of the family attach their surface.
fn register_unless_closure(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &'static str,
    arg_count: NativeArgCount,
    function: impl NativeFunction<KrkrHost> + 'static,
) {
    if matches!(runtime.object_member(object, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native_with_arg_count(object, name, arg_count, function);
}

// ---------------------------------------------------------------- the mapping

/// `image_subpixel_shift` (`agg_basics.h:239-256`): source coordinates are
/// interpolated in 1/256 pixel units.
const SUBPIXEL_SHIFT: u32 = 8;

/// `image_filter_shift` / `image_filter_scale` (`agg_image_filters.h:33-40`):
/// the LUT's weights are 16-bit fixed-point with 14 fractional bits.
const FILTER_SHIFT: u32 = 14;

/// `affine_epsilon` (`agg_trans_affine.h`), the tolerance `is_valid` tests.
const AFFINE_EPSILON: f64 = 1e-14;

/// `agg::trans_perspective` (`agg_trans_perspective.h`), the nine-coefficient
/// 3×3 projectivity, with only the members this plugin uses.
#[derive(Clone, Copy, Debug)]
struct TransPerspective {
    sx: f64,
    shy: f64,
    w0: f64,
    shx: f64,
    sy: f64,
    w1: f64,
    tx: f64,
    ty: f64,
    w2: f64,
}

impl TransPerspective {
    /// The rejected state: every coefficient zero, which `is_valid` refuses.
    /// `square_to_quad` and `invert` leave exactly this
    /// (`agg_trans_perspective.h:312-318, 348-352`).
    const ZERO: Self = Self {
        sx: 0.0,
        shy: 0.0,
        w0: 0.0,
        shx: 0.0,
        sy: 0.0,
        w1: 0.0,
        tx: 0.0,
        ty: 0.0,
        w2: 0.0,
    };

    /// `trans_perspective(const double* quad, double x1, double y1, double x2,
    /// double y2)` (`agg_trans_perspective.h:416-421`), i.e.
    /// `quad_to_quad(quad, rect)` (`:371-379`) with the rectangle's corners in
    /// the same TL, TR, BR, BL order (`quad_to_rect`, `:394-404`).
    fn quad_to_rect(quad: &[f64; 8], x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        let rect = [x1, y1, x2, y1, x2, y2, x1, y2];
        let mut forward = Self::ZERO;
        if !forward.square_to_quad(quad) {
            return Self::ZERO;
        }
        forward.invert();

        let mut rect_map = Self::ZERO;
        if !rect_map.square_to_quad(&rect) {
            return Self::ZERO;
        }
        forward.multiply(&rect_map);
        forward
    }

    /// `square_to_quad` (`agg_trans_perspective.h:287-335`): the unit square to
    /// the quadrilateral, in the affine and the general case.
    fn square_to_quad(&mut self, q: &[f64; 8]) -> bool {
        let dx = q[0] - q[2] + q[4] - q[6];
        let dy = q[1] - q[3] + q[5] - q[7];
        if dx == 0.0 && dy == 0.0 {
            self.sx = q[2] - q[0];
            self.shy = q[3] - q[1];
            self.w0 = 0.0;
            self.shx = q[4] - q[2];
            self.sy = q[5] - q[3];
            self.w1 = 0.0;
            self.tx = q[0];
            self.ty = q[1];
            self.w2 = 1.0;
            return true;
        }
        let dx1 = q[2] - q[4];
        let dy1 = q[3] - q[5];
        let dx2 = q[6] - q[4];
        let dy2 = q[7] - q[5];
        let den = dx1 * dy2 - dx2 * dy1;
        if den == 0.0 {
            *self = Self::ZERO;
            return false;
        }
        let u = (dx * dy2 - dy * dx2) / den;
        let v = (dy * dx1 - dx * dy1) / den;
        self.sx = q[2] - q[0] + u * q[2];
        self.shy = q[3] - q[1] + u * q[3];
        self.w0 = u;
        self.shx = q[6] - q[0] + v * q[6];
        self.sy = q[7] - q[1] + v * q[7];
        self.w1 = v;
        self.tx = q[0];
        self.ty = q[1];
        self.w2 = 1.0;
        true
    }

    /// `invert` (`agg_trans_perspective.h:337-359`); a singular matrix leaves
    /// the all-zero state.
    fn invert(&mut self) {
        let d0 = self.sy * self.w2 - self.w1 * self.ty;
        let d1 = self.w0 * self.ty - self.shy * self.w2;
        let d2 = self.shy * self.w1 - self.w0 * self.sy;
        let d = self.sx * d0 + self.shx * d1 + self.tx * d2;
        if d == 0.0 {
            *self = Self::ZERO;
            return;
        }
        let d = 1.0 / d;
        let a = *self;
        self.sx = d * d0;
        self.shy = d * d1;
        self.w0 = d * d2;
        self.shx = d * (a.w1 * a.tx - a.shx * a.w2);
        self.sy = d * (a.sx * a.w2 - a.w0 * a.tx);
        self.w1 = d * (a.w0 * a.shx - a.sx * a.w1);
        self.tx = d * (a.shx * a.ty - a.sy * a.tx);
        self.ty = d * (a.shy * a.tx - a.sx * a.ty);
        self.w2 = d * (a.sx * a.sy - a.shy * a.shx);
    }

    /// `multiply` (`agg_trans_perspective.h:459-478`): `self = a · self`.
    fn multiply(&mut self, a: &Self) {
        let b = *self;
        self.sx = a.sx * b.sx + a.shx * b.shy + a.tx * b.w0;
        self.shx = a.sx * b.shx + a.shx * b.sy + a.tx * b.w1;
        self.tx = a.sx * b.tx + a.shx * b.ty + a.tx * b.w2;
        self.shy = a.shy * b.sx + a.sy * b.shy + a.ty * b.w0;
        self.sy = a.shy * b.shx + a.sy * b.sy + a.ty * b.w1;
        self.ty = a.shy * b.tx + a.sy * b.ty + a.ty * b.w2;
        self.w0 = a.w0 * b.sx + a.w1 * b.shy + a.w2 * b.w0;
        self.w1 = a.w0 * b.shx + a.w1 * b.sy + a.w2 * b.w1;
        self.w2 = a.w0 * b.tx + a.w1 * b.ty + a.w2 * b.w2;
    }

    /// `transform` (`agg_trans_perspective.h:571-578`): the perspective divide,
    /// which produces infinities on the vanishing line just as the reference's
    /// does.
    fn transform(&self, x: f64, y: f64) -> (f64, f64) {
        let m = 1.0 / (x * self.w0 + y * self.w1 + self.w2);
        (
            m * (x * self.sx + y * self.shx + self.tx),
            m * (x * self.shy + y * self.sy + self.ty),
        )
    }

    /// `is_valid` (`agg_trans_perspective.h:645-648`).
    fn is_valid(&self, epsilon: f64) -> bool {
        self.sx.abs() > epsilon && self.sy.abs() > epsilon && self.w2.abs() > epsilon
    }
}

/// `agg::dda2_line_interpolator` (`agg_dda_line.h:85-176`), forward-adjusted
/// constructor — the one `span_interpolator_linear::begin` builds
/// (`agg_span_interpolator_linear.h:59-79`). Its arithmetic wraps like the
/// reference's `int` does, hence the `wrapping_*` calls: the test profile runs
/// with overflow checks on, and a port must not panic where the reference
/// merely produced a garbage coordinate.
#[derive(Clone, Copy, Debug)]
struct Dda2LineInterpolator {
    cnt: i32,
    lft: i32,
    rem: i32,
    modulo: i32,
    y: i32,
}

impl Dda2LineInterpolator {
    fn new(y1: i32, y2: i32, count: i32) -> Self {
        let mut interpolator = Self {
            cnt: if count <= 0 { 1 } else { count },
            lft: 0,
            rem: 0,
            modulo: 0,
            y: y1,
        };
        let delta = y2.wrapping_sub(y1);
        interpolator.lft = delta / interpolator.cnt;
        interpolator.rem = delta % interpolator.cnt;
        interpolator.modulo = interpolator.rem;
        if interpolator.modulo <= 0 {
            interpolator.modulo = interpolator.modulo.wrapping_add(count);
            interpolator.rem = interpolator.rem.wrapping_add(count);
            interpolator.lft = interpolator.lft.wrapping_sub(1);
        }
        interpolator.modulo = interpolator.modulo.wrapping_sub(count);
        interpolator
    }

    fn step(&mut self) {
        self.modulo = self.modulo.wrapping_add(self.rem);
        self.y = self.y.wrapping_add(self.lft);
        if self.modulo > 0 {
            self.modulo = self.modulo.wrapping_sub(self.cnt);
            self.y = self.y.wrapping_add(1);
        }
    }

    fn value(&self) -> i32 {
        self.y
    }
}

/// `agg::iround` (`agg_basics.h:186-191`): `int(v < 0 ? v - 0.5 : v + 0.5)`.
///
/// The `int` cast of the reference platform is VCL's `cvttsd2si`, which answers
/// `INT_MIN` for NaN and for anything outside the `int` range; Rust's `as i32`
/// saturates, so the two disagree exactly where the reference produced garbage
/// (`layer_ex_raster.rs` documents the same cast for `copyRaster`'s NaN).
fn iround(v: f64) -> i32 {
    let shifted = if v < 0.0 { v - 0.5 } else { v + 0.5 };
    if shifted.is_finite() && shifted >= f64::from(i32::MIN) && shifted <= f64::from(i32::MAX) {
        shifted as i32
    } else {
        i32::MIN
    }
}

// -------------------------------------------------------------- the sampling

/// `image_filter_hermite::calc_weight` (`agg_image_filters.h:145-153`).
fn hermite(x: f64) -> f64 {
    (2.0 * x - 3.0) * x * x + 1.0
}

/// One entry of `image_filter_lut` over the Hermite kernel
/// (`agg_image_filters.h:50-71`): `iround(kernel(i / 256) · 16384)`, built with
/// `normalization = false` as the reference's `image_filter_lut
/// filter(filter_kernel, false)` does (`Main.cpp:154`). The table is symmetric,
/// so `lut_weight(f)` is the weight of the tap at distance `f/256` and
/// `lut_weight(256 - f)` the weight of the one a pixel further on.
fn lut_weight(index: i32) -> i32 {
    iround(hermite(f64::from(index) / 256.0) * f64::from(1 << FILTER_SHIFT))
}

/// `span_image_filter_rgba_2x2::generate` (`agg_span_image_filter_rgba.h:432-521`)
/// for one sample: the two weights per axis, the four taps, the 2-D weight
/// `(wx · wy + 8192) >> 14`, the `>> 14` of the accumulated channels and AGG's
/// clamps (alpha to 255, then each colour to the alpha).
///
/// `x_hr`/`y_hr` are source coordinates in 1/256 units *after* the -128 the
/// generator subtracts (`:451-452`), i.e. relative to the source pixel grid
/// rather than to pixel centres.
fn sample_color(source: &LayerBitmapView<'_>, x_hr: i32, y_hr: i32) -> [u8; 4] {
    let x_lr = x_hr >> SUBPIXEL_SHIFT;
    let y_lr = y_hr >> SUBPIXEL_SHIFT;
    let x_fr = x_hr & 255;
    let y_fr = y_hr & 255;
    let weights_x = [lut_weight(x_fr), lut_weight(256 - x_fr)];
    let weights_y = [lut_weight(y_fr), lut_weight(256 - y_fr)];

    let mut fg = [0i64; 4];
    for (tap_x, weight_x) in weights_x.iter().enumerate() {
        for (tap_y, weight_y) in weights_y.iter().enumerate() {
            let weight = (weight_x * weight_y + (1 << (FILTER_SHIFT - 1))) >> FILTER_SHIFT;
            if weight == 0 {
                // A zero weight contributes nothing; AGG likewise multiplies
                // the tap by 0 whatever byte it read.
                continue;
            }
            let pixel = source_pixel(source, x_lr + tap_x as i32, y_lr + tap_y as i32);
            for channel in 0..4 {
                fg[channel] += i64::from(weight) * i64::from(pixel[channel]);
            }
        }
    }

    let mut color = [0u8; 4];
    for channel in 0..4 {
        // `downshift(fg[c], image_filter_shift)` (`agg_color_rgba.h:423-427`):
        // the LUT's 14 fractional bits come off, and the alpha clamp
        // `if(fg[order_type::A] > full_value())` (`agg_span_image_filter_rgba.h:508`)
        // is what the `u8` conversion's saturation expresses here.
        color[channel] = (fg[channel] >> FILTER_SHIFT).clamp(0, 255) as u8;
    }
    // `:509-511`: AGG's premultiplied invariant, no colour above the alpha.
    for channel in 0..3 {
        color[channel] = color[channel].min(color[3]);
    }
    color
}

/// One tap of the source image, in the engine's R, G, B, A byte order.
///
/// Outside the image the tap is the generator's background colour, the
/// `agg::rgba_pre(0, 0, 0, 0)` the reference passes (`Main.cpp:158`). The byte
/// order is the only translation from the reference's `order_bgra` layout: the
/// weights are applied per channel and the clamps are channel-symmetric, so
/// relabelling the channels changes nothing but the memory order.
fn source_pixel(source: &LayerBitmapView<'_>, x: i32, y: i32) -> [u8; 4] {
    let width = source.bitmap.width as i32;
    let height = source.bitmap.height as i32;
    if x < 0 || y < 0 || x >= width || y >= height {
        return [0, 0, 0, 0];
    }
    let pitch = source.bitmap.pitch as usize;
    let index = y as usize * pitch + x as usize * 4;
    let Some(pixel) = source.pixels.get(index..index + 4) else {
        return [0, 0, 0, 0];
    };
    [pixel[0], pixel[1], pixel[2], pixel[3]]
}

// ---------------------------------------------------------------- the blend

/// `agg::rgba8_t::multiply` (`agg_color_rgba.h:395-399`), AGG's exact 8-bit
/// fixed-point multiply.
fn multiply(a: u8, b: u8) -> u8 {
    let t = i32::from(a) * i32::from(b) + 128;
    (((t >> 8) + t) >> 8) as u8
}

/// `agg::rgba8_t::prelerp` (`agg_color_rgba.h:445-448`): `p + q - multiply(p,
/// a)`, evaluated in 8-bit arithmetic. The subtraction is what keeps a
/// premultiplied source-over compositing result bounded; over straight-alpha
/// bytes — which is what the reference's `pixfmt_bgra32_pre` is handed — it can
/// wrap, and the port wraps with it.
fn prelerp(p: u8, q: u8, a: u8) -> u8 {
    p.wrapping_add(q).wrapping_sub(multiply(p, a))
}

/// `blender_rgba_pre::blend_pix` with a cover (`agg_pixfmt_rgba.h:219-237`):
/// every channel is scaled by the cover first, then the premultiplied
/// source-over lerp runs with the scaled alpha. A cover of 0 is a no-op and a
/// fully transparent source is skipped, exactly like
/// `copy_or_blend_pix` (`agg_pixfmt_rgba.h:1591-1604`) — the blend itself would
/// be an identity there anyway.
fn blend_pre(pixel: &mut [u8], color: [u8; 4], cover: u8) {
    if cover == 0 || color[3] == 0 {
        return;
    }
    let (r, g, b, a) = if cover == 255 {
        (color[0], color[1], color[2], color[3])
    } else {
        (
            multiply(color[0], cover),
            multiply(color[1], cover),
            multiply(color[2], cover),
            multiply(color[3], cover),
        )
    };
    pixel[0] = prelerp(pixel[0], r, a);
    pixel[1] = prelerp(pixel[1], g, a);
    pixel[2] = prelerp(pixel[2], b, a);
    pixel[3] = prelerp(pixel[3], a, a);
}

// ---------------------------------------------------------------- the render

/// The reference's rendering block (`Main.cpp:110-166`): rasterise the quad
/// over the destination image, sample the source through the projectivity and
/// blend, leaving every pixel the quad does not cover alone.
fn draw(
    source: &LayerBitmapView<'_>,
    dest: &mut LayerBitmapViewMut<'_>,
    quad: &[f64; 8],
    transform: &TransPerspective,
) {
    let corners = [
        [quad[0], quad[1]],
        [quad[2], quad[3]],
        [quad[4], quad[5]],
        [quad[6], quad[7]],
    ];
    let convex = quad_is_convex(&corners);
    let dest_width = dest.bitmap.width as i32;
    let dest_height = dest.bitmap.height as i32;
    let dest_pitch = dest.bitmap.pitch as usize;

    // The rasteriser's clip box is the destination image (`Main.cpp:131`), and
    // it only sweeps rows and columns the quad touches (`:133-136`).
    let (min_x, max_x) = bounds(corners.iter().map(|corner| corner[0]));
    let (min_y, max_y) = bounds(corners.iter().map(|corner| corner[1]));
    let x0 = min_x.floor().max(0.0) as i32;
    let x1 = max_x.ceil().min(f64::from(dest_width)) as i32;
    let y0 = min_y.floor().max(0.0) as i32;
    let y1 = max_y.ceil().min(f64::from(dest_height)) as i32;

    let mut covers: Vec<u8> = Vec::new();
    for y in y0..y1 {
        covers.clear();
        for x in x0..x1 {
            covers.push(pixel_coverage(&corners, convex, f64::from(x), f64::from(y)));
        }
        // AGG hands the span generator one run of covered pixels per scanline
        // (`agg_rasterizer_scanline_aa.h:198-243`), so the interpolator starts
        // at the run's first pixel and steps over exactly its length.
        let mut index = 0usize;
        while index < covers.len() {
            if covers[index] == 0 {
                index += 1;
                continue;
            }
            let start = index;
            while index < covers.len() && covers[index] != 0 {
                index += 1;
            }
            draw_span(
                source,
                dest,
                transform,
                x0 + start as i32,
                y,
                &covers[start..index],
                dest_pitch,
            );
        }
    }
}

/// One AGG span (`render_scanline_aa`, `agg_renderer_scanline.h:156-178`): the
/// generator is asked for the run's colours, and each is blended with its own
/// cover.
#[allow(clippy::too_many_arguments)]
fn draw_span(
    source: &LayerBitmapView<'_>,
    dest: &mut LayerBitmapViewMut<'_>,
    transform: &TransPerspective,
    x: i32,
    y: i32,
    covers: &[u8],
    dest_pitch: usize,
) {
    let len = covers.len() as i32;
    let x_start = f64::from(x) + 0.5;
    let y_center = f64::from(y) + 0.5;
    // `span_image_filter::generate` begins at the span's first pixel *centre*
    // and `len` pixels on (`x + len`, not `x + len + 0.5`) in the same row
    // (`agg_span_image_filter.h:37-48`, `agg_span_interpolator_linear.h:63-79`).
    let (tx, ty) = transform.transform(x_start, y_center);
    let (tx_end, ty_end) = transform.transform(x_start + f64::from(len), y_center);
    let mut line_x = Dda2LineInterpolator::new(iround(tx * 256.0), iround(tx_end * 256.0), len);
    let mut line_y = Dda2LineInterpolator::new(iround(ty * 256.0), iround(ty_end * 256.0), len);

    for (offset, &cover) in covers.iter().enumerate() {
        // `span_image_filter_rgba_2x2::generate` subtracts the filter's 128
        // subpixel units before addressing the source (`:451-452`).
        let x_hr = line_x.value().wrapping_sub(128);
        let y_hr = line_y.value().wrapping_sub(128);
        let color = sample_color(source, x_hr, y_hr);
        let index = (y as usize) * dest_pitch + (x as usize + offset) * 4;
        if let Some(pixel) = dest.pixels.get_mut(index..index + 4) {
            blend_pre(pixel, color, cover);
        }
        line_x.step();
        line_y.step();
    }
}

/// The destination-area extremes of one coordinate.
fn bounds(values: impl Iterator<Item = f64>) -> (f64, f64) {
    values.fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
        (min.min(value), max.max(value))
    })
}

/// Whether the quad is convex (collinear corners allowed), i.e. whether its
/// interior is the intersection of four half planes — the case
/// [`clip_quad_to_pixel`] computes exactly.
fn quad_is_convex(corners: &[[f64; 2]; 4]) -> bool {
    let mut sign = 0i32;
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        let c = corners[(i + 2) % 4];
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        if cross == 0.0 {
            continue;
        }
        let current = if cross > 0.0 { 1 } else { -1 };
        if sign == 0 {
            sign = current;
        } else if current != sign {
            return false;
        }
    }
    true
}

/// One pixel's coverage in `rasterizer_scanline_aa`'s cover units: the quad's
/// area inside `[x, x + 1] × [y, y + 1]`, scaled to 1/256 and clamped at
/// `cover_full` — the quantity `calculate_alpha` derives from its cell areas
/// (`agg_rasterizer_scanline_aa.h:183-196`), computed geometrically here.
///
/// A self-intersecting quad has no such area decomposition, so it falls back to
/// a hard-edged non-zero-winding test at the pixel centre: AGG would antialias
/// the same lobes, but reconstructing them would be a re-derivation rather than
/// a port (see the module docs).
fn pixel_coverage(corners: &[[f64; 2]; 4], convex: bool, x: f64, y: f64) -> u8 {
    if !convex {
        return if winding_number(corners, x + 0.5, y + 0.5) != 0 {
            255
        } else {
            0
        };
    }
    let (clipped, count) = clip_quad_to_pixel(corners, x, y);
    let area = polygon_area(&clipped[..count]);
    let cover = iround(area * 256.0);
    cover.clamp(0, 255) as u8
}

/// Sutherland–Hodgman clipping of the convex quad against the unit pixel square;
/// a 4-gon clipped by four half planes has at most eight vertices, so the caller
/// gets a fixed-size buffer and a length.
fn clip_quad_to_pixel(corners: &[[f64; 2]; 4], x: f64, y: f64) -> ([[f64; 2]; 8], usize) {
    let mut current = [[0.0f64; 2]; 8];
    current[..4].copy_from_slice(corners);
    let mut count = 4usize;
    // (axis, bound, keep the side that is >= the bound)
    let planes = [
        (0usize, x, true),
        (0usize, x + 1.0, false),
        (1usize, y, true),
        (1usize, y + 1.0, false),
    ];
    let mut scratch = [[0.0f64; 2]; 8];
    for (axis, bound, keep_greater) in planes {
        let mut out = 0usize;
        for i in 0..count {
            let current_point = current[i];
            let next_point = current[(i + 1) % count];
            let d_current = current_point[axis] - bound;
            let d_next = next_point[axis] - bound;
            let inside_current = if keep_greater {
                d_current >= 0.0
            } else {
                d_current <= 0.0
            };
            let inside_next = if keep_greater {
                d_next >= 0.0
            } else {
                d_next <= 0.0
            };
            if inside_current && out < scratch.len() {
                scratch[out] = current_point;
                out += 1;
            }
            if inside_current != inside_next && out < scratch.len() {
                let t = d_current / (d_current - d_next);
                scratch[out] = [
                    current_point[0] + (next_point[0] - current_point[0]) * t,
                    current_point[1] + (next_point[1] - current_point[1]) * t,
                ];
                out += 1;
            }
        }
        current[..out].copy_from_slice(&scratch[..out]);
        count = out;
        if count == 0 {
            break;
        }
    }
    (current, count)
}

/// The shoelace area of a polygon.
fn polygon_area(points: &[[f64; 2]]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        sum += a[0] * b[1] - b[0] * a[1];
    }
    (sum / 2.0).abs()
}

/// The quad's non-zero winding number at a point, for the fallback above.
fn winding_number(corners: &[[f64; 2]; 4], px: f64, py: f64) -> i32 {
    let mut winding = 0;
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        if a[1] <= py {
            if b[1] > py && is_left(a, b, [px, py]) > 0.0 {
                winding += 1;
            }
        } else if b[1] <= py && is_left(a, b, [px, py]) < 0.0 {
            winding -= 1;
        }
    }
    winding
}

fn is_left(a: [f64; 2], b: [f64; 2], point: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (point[1] - a[1]) - (point[0] - a[0]) * (b[1] - a[1])
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine, KrkrPlugin};
    use krkr_tjs2::runtime::{ObjectHandle, Variant};

    use super::LayerExPerspectivePlugin;
    use crate::catalog::PluginStatus;

    /// A transparent destination plus the source the test needs. The layer
    /// pattern helper `fillRect` is the family's usual way of building a
    /// bitmap from a script. `0x010203 * n` stays inside the 24-bit colour for
    /// every `n` this 4x4 source produces (`n` up to 16 gives `0x102030`), so
    /// every pixel is distinguishable through `getMainPixel`.
    const SOURCE_4X4: &str = r#"
        global.src = new Layer();
        src.setImageSize(4, 4);
        for (var y = 0; y < 4; y++) {
            for (var x = 0; x < 4; x++) {
                src.fillRect(x, y, 1, 1, 0x010203 * (y * 4 + x + 1) | 0xff000000);
            }
        }
    "#;

    const DEST_4X4: &str = r#"
        global.dest = new Layer();
        dest.setImageSize(4, 4);
        dest.fillRect(0, 0, 4, 4, 0x00000000);
    "#;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(LayerExPerspectivePlugin)
            .expect("plugin");
        engine
    }

    fn run(engine: &mut KrkrEngine, name: &str, script: &str) {
        engine.execute_script(name, script).expect("script");
    }

    fn layer_class(engine: &KrkrEngine) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member("Layer")
            .object_handle()
            .expect("Layer class")
    }

    /// Registered natives are stored as function objects and script functions
    /// as closures; both are callable members.
    fn is_callable_member(engine: &KrkrEngine, object: ObjectHandle, name: &str) -> bool {
        match engine.tjs_runtime().object_member(object, name) {
            Variant::Closure(_) => true,
            Variant::Object(handle) => engine.tjs_runtime().object_is_callable(handle),
            _ => false,
        }
    }

    fn member_names(engine: &KrkrEngine, object: ObjectHandle) -> Vec<String> {
        let mut names: Vec<String> = engine
            .tjs_runtime()
            .object_members(object)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    fn pixel(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMainPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    /// `getMaskPixel` reads the alpha plane, the other half of a blended pixel.
    fn mask(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMaskPixel({x}, {y})"))
            .expect("mask")
            .to_integer()
            .expect("integer")
    }

    /// `Layer.update()` leaves `callOnPaint` set (`classes.rs:7218-7222`).
    fn call_on_paint(engine: &mut KrkrEngine, layer: &str) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.callOnPaint"))
            .expect("callOnPaint")
            .to_integer()
            .expect("integer")
    }

    fn layer_handle(engine: &KrkrEngine, name: &str) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    /// The texture id of a layer's image, which a commit replaces.
    fn generation(engine: &mut KrkrEngine, name: &str) -> u64 {
        let handle = layer_handle(engine, name);
        krkr_engine::plugin_api::layer::layer_bitmap_read(
            engine.tjs_runtime_mut(),
            handle,
            |view| view.bitmap.generation,
        )
        .expect("generation")
    }

    /// The reference's decoded surface is one member on `Layer`'s class object
    /// (`Main.cpp:233`) and, apart from the engine-internal native class id
    /// (`:216-218`), nothing else — so registering this module must add exactly
    /// `perspectiveCopy` and no global.
    #[test]
    fn this_module_registers_exactly_the_reference_member() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let layer = layer_class(&engine);
        let before = member_names(&engine, layer);
        assert!(!before.contains(&"perspectiveCopy".to_string()));
        engine
            .register_plugin(LayerExPerspectivePlugin)
            .expect("plugin");
        let after = member_names(&engine, layer);
        let added: Vec<&String> = after.iter().filter(|name| !before.contains(name)).collect();
        assert_eq!(
            added,
            vec!["perspectiveCopy"],
            "the reference adds exactly one member"
        );
        assert!(is_callable_member(&engine, layer, "perspectiveCopy"));
        assert_eq!(
            engine.tjs_runtime().global_member("LayerExBase"),
            Variant::Void,
            "the reference's LayerExBase is a native class id, not a script global"
        );
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("AntiGrainGeometry")),
            "`V2Link` announces the AGG copyright (Main.cpp:213)"
        );
    }

    /// `Plugins.link("perspective.dll")` — the name `perspective.def` exports
    /// and the catalog entry carries — must reach *this* implementation: the
    /// entry states `Implemented` and installing *that entry* puts the member on
    /// the `Layer` class object. A placeholder (the state this module replaced)
    /// installs no surface at all, so this is the wiring check.
    #[test]
    fn the_catalog_entry_points_at_this_implementation() {
        let entry = crate::catalog::resolve("perspective.dll").expect("catalog entry");
        assert_eq!(entry.meta.status, PluginStatus::Implemented);
        assert_eq!(
            LayerExPerspectivePlugin.name(),
            "perspective.dll",
            "the registered plugin name is the DLL the game links"
        );

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        assert!(!is_callable_member(
            &engine,
            layer_class(&engine),
            "perspectiveCopy"
        ));
        crate::catalog::install_plugin(&mut engine, entry).expect("install the catalog entry");
        assert!(is_callable_member(
            &engine,
            layer_class(&engine),
            "perspectiveCopy"
        ));
    }

    /// `Main.cpp:84`: fewer than thirteen parameters is
    /// `TJS_E_BADPARAMCOUNT`, and a full call runs the member.
    #[test]
    fn argument_counts_are_the_reference_check() {
        let mut engine = engine();
        run(&mut engine, "source.tjs", SOURCE_4X4);
        run(&mut engine, "dest.tjs", DEST_4X4);
        let error = engine
            .execute_script(
                "bad.tjs",
                "dest.perspectiveCopy(src, 0, 0, 4, 4, 0, 0, 4, 0, 4, 4, 0);",
            )
            .expect_err("twelve parameters");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);

        // The decoded call order is src, left, top, width, height, then TL, TR,
        // BL, BR (`manual.tjs`): the identity quad needs the corners written so
        // that AGG's TL, TR, BR, BL polygon comes out `(0,0) (4,0) (4,4) (0,4)`.
        run(
            &mut engine,
            "ok.tjs",
            "dest.perspectiveCopy(src, 0, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);",
        );
        assert_eq!(pixel(&mut engine, "dest", 3, 3), 0x010203 * 16);
    }

    /// The identity quad: the quad is the destination's own rectangle and the
    /// source rect is the whole source, so every destination pixel samples the
    /// source pixel under it (the interpolator lands exactly on source pixel
    /// centres, weights `W(0) = 16384` and `W(1) = 0`), the coverage is 255 in
    /// every pixel, and an opaque source comes out unchanged through
    /// `multiply(p, 255) == p`.
    #[test]
    fn the_identity_quad_copies_the_source() {
        let mut engine = engine();
        run(&mut engine, "source.tjs", SOURCE_4X4);
        run(&mut engine, "dest.tjs", DEST_4X4);
        let source_generation = generation(&mut engine, "src");

        run(
            &mut engine,
            "copy.tjs",
            "dest.perspectiveCopy(src, 0, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);",
        );

        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(
                    pixel(&mut engine, "dest", x, y),
                    0x010203 * (y * 4 + x + 1),
                    "dest({x},{y})"
                );
                assert_eq!(mask(&mut engine, "dest", x, y), 0xff, "dest({x},{y}) alpha");
            }
        }
        assert_eq!(
            generation(&mut engine, "src"),
            source_generation,
            "the source keeps its image"
        );
        assert_eq!(
            call_on_paint(&mut engine, "dest"),
            1,
            "the family contract's Layer.update() ran"
        );
    }

    /// A two-times magnification of a 2x2 source into a 4x4 destination: the
    /// destination pixel `(1, 1)` samples the source at `(0.25, 0.25)`, i.e.
    /// 16384 units of source pixel `(0, 0)` and 2560 of each neighbour —
    /// `iround(hermite(0.25) * 16384) = 13824` for the near tap and
    /// `iround(hermite(0.75) * 16384) = 2560` for the far one, whose 2-D
    /// products are `(13824*13824 + 8192) >> 14 = 11664`,
    /// `(13824*2560 + 8192) >> 14 = 2160` and `(2560*2560 + 8192) >> 14 = 400`
    /// (they sum to 16384, the LUT's unnormalised total).
    ///
    /// The source is opaque red/green/blue/white, so with `>> 14` truncating:
    /// R = `(11664 + 400) * 255 / 16384 = 187.76 -> 187`,
    /// G = B = `(2160 + 400) * 255 / 16384 = 39.84 -> 39`, A = 255. Destination
    /// pixel `(0, 0)` is the edge case: its sample falls at `(-0.25, -0.25)`, so
    /// three of its four taps are outside the source image and contribute the
    /// transparent background, leaving `11664 * 255 / 16384 = 181.53 -> 181` of
    /// the red corner with alpha 181; the premultiplied blend over the
    /// transparent destination writes exactly that.
    #[test]
    fn a_two_times_warp_weights_the_neighbours_through_the_hermite_lut() {
        let mut engine = engine();
        run(
            &mut engine,
            "setup.tjs",
            r#"
            global.src = new Layer();
            src.setImageSize(2, 2);
            src.fillRect(0, 0, 1, 1, 0xffff0000);
            src.fillRect(1, 0, 1, 1, 0xff00ff00);
            src.fillRect(0, 1, 1, 1, 0xff0000ff);
            src.fillRect(1, 1, 1, 1, 0xffffffff);

            global.dest = new Layer();
            dest.setImageSize(4, 4);
            dest.fillRect(0, 0, 4, 4, 0x00000000);
            "#,
        );

        run(
            &mut engine,
            "warp.tjs",
            "dest.perspectiveCopy(src, 0, 0, 2, 2, 0, 0, 4, 0, 0, 4, 4, 4);",
        );

        assert_eq!(pixel(&mut engine, "dest", 1, 1), 0xbb2727, "187, 39, 39");
        assert_eq!(mask(&mut engine, "dest", 1, 1), 0xff);
        assert_eq!(pixel(&mut engine, "dest", 0, 0), 0xb50000, "181 of red");
        assert_eq!(mask(&mut engine, "dest", 0, 0), 0xb5, "alpha 181");
        // The far corner mirrors that: the sample `(1.75, 1.75)` puts weight
        // 11664 on the white source corner and 2560 + 2560 + 400 on the three
        // taps *outside* the 2x2 image, which are transparent — so every channel
        // is 11664 * 255 / 16384 = 181.53 -> 181.
        assert_eq!(pixel(&mut engine, "dest", 3, 3), 0xb5b5b5, "181 grey");
        assert_eq!(mask(&mut engine, "dest", 3, 3), 0xb5, "alpha 181");
    }

    /// A strong perspective warp: the source's 5x3 rectangle maps onto the quad
    /// `TL(0,0) TR(5,0) BR(1,0.6) BL(0,0.6)`, i.e. the projectivity
    /// `(u, v) -> (u/(1 + 4v/3), v/(1 + 4v/3))` and its inverse
    /// `(u, v) = (X, Y)/(1 - 4Y/3)`.
    ///
    /// Destination pixel `(0, 0)` samples at `(0.5, 0.5)/(1 - 2/3) = (1.5, 1.5)`
    /// — exactly source pixel `(1, 1)` once the generator's -0.5 shift lands on
    /// a pixel edge (`x_hr = 384 - 128 = 256`, `x_lr = 1`, fraction 0). Its
    /// coverage is the part of the pixel inside the quad: the quad's bottom edge
    /// is at `Y = 0.6` and its right edge is far to the right of `x = 1`, so the
    /// area is `0.6`, i.e. `iround(0.6 * 256) = 154` cover units. The source
    /// pixel is opaque white, so the cover-scaled blend writes
    /// `mult_cover(255, 154) = 154` for every channel and the transparent
    /// destination keeps it: `(154, 154, 154)` with alpha 154 — a *premultiplied*
    /// result, the reference's own convention over a straight-alpha layer.
    ///
    /// Destination pixel `(1, 0)` samples `(4.5, 1.5)` — source pixel `(4, 1)`,
    /// the last column — with coverage
    /// `0.45 + integral 0.45..0.6 (4 - 20y/3) dy = 0.45 + 0.075 = 0.525`, i.e.
    /// cover 134, and that source pixel is opaque black, so only its alpha
    /// survives: `(0, 0, 0)` with alpha 134. Pixels `(2, 0)`..`(4, 0)` are
    /// covered only by the thin sliver of the slanted edge and sample beyond the
    /// source (`u >= 7`), whose taps are the transparent background — the
    /// reference's transparent-background rule — so they stay untouched, as do
    /// all rows below the quad.
    #[test]
    fn a_trapezoid_quad_samples_through_the_reference_projectivity() {
        let mut engine = engine();
        run(
            &mut engine,
            "setup.tjs",
            r#"
            global.src = new Layer();
            src.setImageSize(5, 3);
            src.fillRect(0, 0, 5, 3, 0xff101010);
            src.fillRect(1, 1, 1, 1, 0xffffffff);
            src.fillRect(4, 1, 1, 1, 0xff000000);

            global.dest = new Layer();
            dest.setImageSize(6, 3);
            dest.fillRect(0, 0, 6, 3, 0x00000000);
            "#,
        );

        run(
            &mut engine,
            "warp.tjs",
            "dest.perspectiveCopy(src, 0, 0, 5, 3, 0, 0, 5, 0, 0, 0.6, 1, 0.6);",
        );

        assert_eq!(pixel(&mut engine, "dest", 0, 0), 0x9a9a9a, "154, 154, 154");
        assert_eq!(mask(&mut engine, "dest", 0, 0), 0x9a, "alpha 154");
        assert_eq!(pixel(&mut engine, "dest", 1, 0), 0x000000, "opaque black");
        assert_eq!(mask(&mut engine, "dest", 1, 0), 0x86, "alpha 134");
        for x in 2..6 {
            assert_eq!(
                pixel(&mut engine, "dest", x, 0),
                0,
                "the sliver at ({x}, 0) samples the transparent background"
            );
            assert_eq!(mask(&mut engine, "dest", x, 0), 0);
        }
        for y in 1..3 {
            for x in 0..6 {
                assert_eq!(pixel(&mut engine, "dest", x, y), 0, "outside the quad");
                assert_eq!(mask(&mut engine, "dest", x, y), 0);
            }
        }
        assert_eq!(call_on_paint(&mut engine, "dest"), 1);
    }

    /// A quad with no extent leaves AGG's matrix singular
    /// (`square_to_quad`'s affine branch gives `sx = sy = 0`), so `is_valid`
    /// refuses it and nothing is rasterised — but `redraw()` still runs
    /// (`Main.cpp:140, 168`). A zero-sized source *rectangle* is the same
    /// singular rectangle in `quad_to_rect`; a source layer with no image at all
    /// is the same call again (this engine refuses `setImageSize(0, 0)` with
    /// "Cannot create empty layer image", so a layer without an image is the
    /// zero-size source it can represent).
    #[test]
    fn a_degenerate_quad_or_source_draws_nothing_but_repaints() {
        let mut engine = engine();
        run(&mut engine, "source.tjs", SOURCE_4X4);
        run(
            &mut engine,
            "setup.tjs",
            r#"
            global.dest = new Layer();
            dest.setImageSize(4, 4);
            dest.fillRect(0, 0, 4, 4, 0xff204060);

            global.empty = new Layer();
            "#,
        );
        let source_generation = generation(&mut engine, "src");

        // All four corners on one point.
        run(
            &mut engine,
            "degenerate.tjs",
            "dest.perspectiveCopy(src, 0, 0, 4, 4, 2, 2, 2, 2, 2, 2, 2, 2);",
        );
        assert_eq!(pixel(&mut engine, "dest", 0, 0), 0x204060, "untouched");
        assert_eq!(pixel(&mut engine, "dest", 3, 3), 0x204060, "untouched");
        assert_eq!(call_on_paint(&mut engine, "dest"), 1, "still repaints");

        // A zero-sized source rectangle over a well-formed quad.
        run(
            &mut engine,
            "degenerate.tjs",
            "dest.perspectiveCopy(src, 0, 0, 0, 0, 0, 0, 4, 0, 0, 4, 4, 4);",
        );
        assert_eq!(pixel(&mut engine, "dest", 0, 0), 0x204060, "untouched");

        // A source layer that never had an image, through the same call.
        run(
            &mut engine,
            "degenerate.tjs",
            "dest.perspectiveCopy(empty, 0, 0, 0, 0, 0, 0, 4, 0, 0, 4, 4, 4);",
        );
        assert_eq!(pixel(&mut engine, "dest", 0, 0), 0x204060, "untouched");
        assert_eq!(
            generation(&mut engine, "src"),
            source_generation,
            "the source keeps its image"
        );
    }

    /// The reference's failure paths are the engine's errors here: a source
    /// that is not a layer (the reference dereferences a null instance) and a
    /// layer whose image was freed (`mainImageBufferForWrite` answers void, and
    /// the reference renders into a null buffer).
    #[test]
    fn a_bad_source_is_an_error_not_a_crash() {
        let mut engine = engine();
        run(&mut engine, "source.tjs", SOURCE_4X4);
        run(&mut engine, "dest.tjs", DEST_4X4);

        let error = engine
            .execute_script(
                "bad.tjs",
                "dest.perspectiveCopy(42, 0, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);",
            )
            .expect_err("a non-layer source");
        assert_eq!(error.message, "perspectiveCopy: src must be Layer.");

        run(&mut engine, "free.tjs", "src.freeImage();");
        let error = engine
            .execute_script(
                "bad.tjs",
                "dest.perspectiveCopy(src, 0, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);",
            )
            .expect_err("a freed source image");
        assert_eq!(error.message, "Not drawable layer type");
        assert_eq!(pixel(&mut engine, "dest", 0, 0), 0, "nothing was drawn");
    }

    /// This module registers one member *on top of* the engine's layer surface,
    /// so the family's shared base members keep answering through the same
    /// instance, and a sibling family member registered on the same class object
    /// keeps its own member.
    #[test]
    fn the_family_base_surface_still_answers_through_this_instance() {
        let mut engine = engine();
        engine
            .register_plugin(crate::layer_ex_raster::LayerExRasterPlugin)
            .expect("sibling plugin");
        run(&mut engine, "source.tjs", SOURCE_4X4);
        run(&mut engine, "dest.tjs", DEST_4X4);

        let layer = layer_class(&engine);
        for name in ["perspectiveCopy", "copyRaster"] {
            assert!(
                is_callable_member(&engine, layer, name),
                "Layer.{name} is registered"
            );
        }

        // The base contract the reference's `layerExBase.hpp` reads: the image
        // size and the clip box are still properties of the instance this
        // module wrote through.
        run(
            &mut engine,
            "base.tjs",
            "dest.setClip(1, 1, 3, 3); dest.perspectiveCopy(src, 0, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);",
        );
        assert_eq!(
            engine
                .execute_expression("base.tjs", "dest.imageWidth")
                .expect("imageWidth")
                .to_integer()
                .expect("integer"),
            4
        );
        assert_eq!(
            engine
                .execute_expression("base.tjs", "dest.clipWidth")
                .expect("clipWidth")
                .to_integer()
                .expect("integer"),
            3
        );
        // The clip box does not clip `perspectiveCopy` (the reference's
        // rasteriser is clipped to the whole image, Main.cpp:131), so the pixel
        // outside the box was written.
        assert_eq!(pixel(&mut engine, "dest", 0, 0), 0x010203);
    }
}
