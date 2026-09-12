//! `layerExAreaAverage.dll`: `Layer.stretchCopyAA`, the area-average rescale.
//!
//! Real plugin: area-average (box filter) shrink drawing
//! (`layerExAreaAverage/main.cpp`,
//! <https://github.com/wtnbgo/layerExAreaAverage>).
//!
//! One raw callback attached to the global `Layer` class
//! (`main.cpp:195-198`): `stretchCopyAA(dleft, dtop, dwidth, dheight, src,
//! sleft, stop, swidth, sheight)`. It refuses upscaling, clips the destination
//! against the layer, proportionally rescales the source rectangle, then walks
//! the destination pixels averaging the exact sub-pixel overlap of each mapped
//! source region in 12.12 fixed point (`main.cpp:17-192`).
//!
//! The reference works on a destination DWORD of `0xAARRGGBB` and a source
//! DWORD of the same layout; the engine's planes are R,G,B,A per byte, so every
//! channel read/write goes through the byte offsets in
//! [`krkr_engine::plugin_api::layer`] §B.3.4. Colour is weighted by alpha
//! (`area * alpha >> 8`, `main.cpp:147`), which already yields a straight-alpha
//! result for the engine to store.
//!
//! Faithfulness notes, both documented where they bite:
//!
//! * The reference's `continue` for a zero-area region (`main.cpp:157`) skips
//!   its `outpixel++` as well (`main.cpp:173`), so the rest of that row writes
//!   one pixel early. The port reproduces the reference's cursor exactly.
//! * The reference never validates a negative `dleft`/`dtop` and writes before
//!   the buffer; the port crops the destination the same way the reference's
//!   own overflow blocks do (`main.cpp:74-97`) so no byte outside the image is
//!   ever addressed, and clamps source reads into the source image.
//!
//! The krkr2 trunk variant of this plugin (`krkr2/.../layerExAreaAverage/
//! main.cpp`) adds a `totalarea_trgb` fallback that averages fully transparent
//! source colours instead of writing 0; the port follows the krkrz source the
//! dossier documents.

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{
        LayerBitmapView, LayerBitmapViewMut, layer_bitmap_read_write, layer_update,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer.stretchCopyAA area-average downscale",
    notes: "A port of layerExAreaAverage/main.cpp:17-192: 12.12 fixed-point area average, alpha-weighted colour, no upscaling (throws), destination clipped against the image. The reference's `ex`/`ey` clamp drops the source's last column/row for boxes that end on the image edge; reproduced as-is.",
    install: |engine| engine.register_plugin(LayerExAreaAveragePlugin),
};

pub struct LayerExAreaAveragePlugin;

impl KrkrPlugin for LayerExAreaAveragePlugin {
    fn name(&self) -> &str {
        "layerExAreaAverage.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `RawCallback("stretchCopyAA", ..., 0)` (`main.cpp:195-198`) with the
        // reference's own `numparams < 9` check (`main.cpp:22`).
        if matches!(
            runtime.object_member(layer, "stretchCopyAA"),
            Variant::Closure(_)
        ) {
            return Ok(());
        }
        runtime.register_object_native_with_arg_count(
            layer,
            "stretchCopyAA",
            NativeArgCount::AtLeast(9),
            layer_stretch_copy_aa,
        );
        Ok(())
    }
}

/// 12.12 fixed point: `DOTBASE` (`main.cpp:5`).
const FIXDOT_SHIFT: u32 = 12;
/// One pixel in fixed point, the reference's `INT2FIXDOT(1)` (`main.cpp:7`).
const ONE: i32 = 1 << FIXDOT_SHIFT;

/// `INT2FIXDOT(a)` (`main.cpp:6`).
fn int_to_fixdot(value: i32) -> i32 {
    value.wrapping_shl(FIXDOT_SHIFT)
}

/// `REAL2FIXDOT(a)` (`main.cpp:7`), truncating toward zero like the C cast.
fn real_to_fixdot(value: f64) -> i32 {
    (value * f64::from(ONE)) as i32
}

/// The `(tjs_int)` cast the clipping blocks apply to a real ratio
/// (`main.cpp:77-96`): pixels, truncated toward zero.
fn real_to_int(value: f64) -> i32 {
    value as i32
}

/// `MULFIXDOT(a, b)` (`main.cpp:10`): a wrapping 12.12 product.
fn mul_fixdot(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b) >> FIXDOT_SHIFT
}

/// `FIXDOT2INT(a)` (`main.cpp:8`): the arithmetic shift an `int >> 12` is on
/// every target krkrz builds for.
fn fixdot_to_int(value: i32) -> i32 {
    value >> FIXDOT_SHIFT
}

/// A `(left, top, width, height)` rectangle in layer pixels, as the reference
/// keeps its eight integers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rect {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

fn layer_stretch_copy_aa(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    let Some(src) = args.get(4).and_then(Variant::object_handle) else {
        return Err(TjsError::runtime("stretchCopyAA: src must be Layer."));
    };
    let mut dest_rect = Rect {
        left: arg_integer(&args, 0)?,
        top: arg_integer(&args, 1)?,
        width: arg_integer(&args, 2)?,
        height: arg_integer(&args, 3)?,
    };
    let mut source_rect = Rect {
        left: arg_integer(&args, 5)?,
        top: arg_integer(&args, 6)?,
        width: arg_integer(&args, 7)?,
        height: arg_integer(&args, 8)?,
    };

    // 拡大処理は行なえません (`main.cpp:66-71`).
    if dest_rect.width > source_rect.width || dest_rect.height > source_rect.height {
        return Err(TjsError::runtime(
            "stretchCopyAA は拡大処理を行なえません。",
        ));
    }

    layer_bitmap_read_write(runtime, src, dest, |source, dest_view| {
        stretch_one(source, dest_view, &mut dest_rect, &mut source_rect);
    })?;
    // The reference updates the clipped destination rectangle
    // (`main.cpp:177-187`); the engine's `Layer.update()` repaints the layer, so
    // the rectangle is not part of the call.
    layer_update(runtime, dest)?;
    Ok(Variant::Void)
}

/// The clipping blocks (`main.cpp:73-97`) and the area-average loop
/// (`main.cpp:99-175`).
fn stretch_one(
    source: &LayerBitmapView<'_>,
    dest: &mut LayerBitmapViewMut<'_>,
    dest_rect: &mut Rect,
    source_rect: &mut Rect,
) {
    let (s_image_width, s_image_height) = (source.bitmap.width as i32, source.bitmap.height as i32);
    let (d_image_width, d_image_height) = (dest.bitmap.width as i32, dest.bitmap.height as i32);
    let d_pitch = dest.bitmap.pitch as usize;
    let s_pitch = source.bitmap.pitch as usize;

    // クリッピング（main.cpp:74-97）, in the reference's order: the destination
    // is held inside its image and the source shrinks with the same ratio.
    clip_dest_right(dest_rect, source_rect, d_image_width);
    clip_source_right(source_rect, dest_rect, s_image_width);
    clip_dest_bottom(dest_rect, source_rect, d_image_height);
    clip_source_bottom(source_rect, dest_rect, s_image_height);

    // The reference does not validate these two: a negative origin makes
    // `dBuffer + (y+dTop)*dPitch + dLeft*4` address memory before the image.
    // Crop instead, mirroring the overflow blocks above — the dropped
    // destination pixels drop the same share of the source rectangle.
    clip_dest_left(dest_rect, source_rect);
    clip_dest_top(dest_rect, source_rect);

    let sl = int_to_fixdot(source_rect.left);
    let st = int_to_fixdot(source_rect.top);
    let rw = real_to_fixdot(f64::from(source_rect.width) / f64::from(dest_rect.width));
    let rh = real_to_fixdot(f64::from(source_rect.height) / f64::from(dest_rect.height));

    for y in 0..dest_rect.height {
        // `outpixel` is a cursor, not a per-x index: the reference only
        // advances it after a write, and `continue` skips that advance
        // (`main.cpp:157-173`). Kept exactly, so the rows after a zero-area
        // region land where the reference lands them.
        let mut outpixel = (y + dest_rect.top) as i64 * d_pitch as i64 + dest_rect.left as i64 * 4;
        for x in 0..dest_rect.width {
            let x1 = sl.wrapping_add(x.wrapping_mul(rw));
            let y1 = st.wrapping_add(y.wrapping_mul(rh));
            let x2 = x1.wrapping_add(rw);
            let y2 = y1.wrapping_add(rh);

            let sx = fixdot_to_int(x1).max(0);
            let sy = fixdot_to_int(y1).max(0);
            let ex = fixdot_to_int(x2.wrapping_add(ONE).wrapping_sub(1));
            let ey = fixdot_to_int(y2.wrapping_add(ONE).wrapping_sub(1));
            let ex = if ex >= s_image_width {
                s_image_width - 1
            } else {
                ex
            };
            let ey = if ey >= s_image_height {
                s_image_height - 1
            } else {
                ey
            };

            let mut totalarea_a: i32 = 0;
            let mut a: i32 = 0;
            let mut totalarea_rgb: i32 = 0;
            let (mut r, mut g, mut b) = (0i32, 0i32, 0i32);
            for ay in sy..ey {
                let row = ay as usize * s_pitch + sx as usize * 4;
                let mut e1 = int_to_fixdot(ay);
                let mut e2 = int_to_fixdot(ay.wrapping_add(1));
                if e1 < y1 {
                    e1 = y1;
                }
                if e2 > y2 {
                    e2 = y2;
                }
                let ah = e2.wrapping_sub(e1);
                let mut inpixel = row;
                for ax in sx..ex {
                    let mut e1 = int_to_fixdot(ax);
                    let mut e2 = int_to_fixdot(ax.wrapping_add(1));
                    if e1 < x1 {
                        e1 = x1;
                    }
                    if e2 > x2 {
                        e2 = x2;
                    }
                    let aw = e2.wrapping_sub(e1);
                    let mut area = mul_fixdot(aw, ah);
                    totalarea_a = totalarea_a.wrapping_add(area);
                    let Some(pixel) = source.pixels.get(inpixel..inpixel + 4) else {
                        break;
                    };
                    let alpha = i32::from(pixel[3]);
                    a = a.wrapping_add(alpha.wrapping_mul(area));
                    area = (area.wrapping_mul(alpha)) >> 8;
                    r = r.wrapping_add(i32::from(pixel[0]).wrapping_mul(area));
                    g = g.wrapping_add(i32::from(pixel[1]).wrapping_mul(area));
                    b = b.wrapping_add(i32::from(pixel[2]).wrapping_mul(area));
                    totalarea_rgb = totalarea_rgb.wrapping_add(area);
                    inpixel += 4;
                }
            }

            if totalarea_a == 0 {
                continue;
            }

            a /= totalarea_a;
            if totalarea_rgb == 0 {
                r = 0;
                g = 0;
                b = 0;
            } else {
                r /= totalarea_rgb;
                g /= totalarea_rgb;
                b /= totalarea_rgb;
            }
            let offset = outpixel as usize;
            if let Some(pixel) = dest.pixels.get_mut(offset..offset + 4) {
                pixel[0] = r as u8;
                pixel[1] = g as u8;
                pixel[2] = b as u8;
                pixel[3] = a as u8;
            }
            outpixel += 4;
        }
    }
}

fn clip_dest_right(dest: &mut Rect, source: &mut Rect, image_width: i32) {
    if dest.left + dest.width > image_width {
        let dw = image_width - dest.left;
        source.width =
            real_to_int(f64::from(source.width) * (f64::from(dw) / f64::from(dest.width)));
        dest.width = dw;
    }
}

fn clip_source_right(source: &mut Rect, dest: &mut Rect, image_width: i32) {
    if source.left + source.width > image_width {
        let sw = image_width - source.left;
        dest.width = real_to_int(f64::from(dest.width) * (f64::from(sw) / f64::from(source.width)));
        source.width = sw;
    }
}

fn clip_dest_bottom(dest: &mut Rect, source: &mut Rect, image_height: i32) {
    if dest.top + dest.height > image_height {
        let dh = image_height - dest.top;
        source.height =
            real_to_int(f64::from(source.height) * (f64::from(dh) / f64::from(dest.height)));
        dest.height = dh;
    }
}

fn clip_source_bottom(source: &mut Rect, dest: &mut Rect, image_height: i32) {
    if source.top + source.height > image_height {
        let sh = image_height - source.top;
        dest.height =
            real_to_int(f64::from(dest.height) * (f64::from(sh) / f64::from(source.height)));
        source.height = sh;
    }
}

/// The negative-`left` mirror of [`clip_dest_right`]: the pixels before the
/// image are dropped from the destination and the same share from the source's
/// leading edge.
fn clip_dest_left(dest: &mut Rect, source: &mut Rect) {
    if dest.left >= 0 || dest.width <= 0 {
        return;
    }
    let kept = dest.left + dest.width;
    source.left +=
        real_to_int(f64::from(source.width) * (f64::from(-dest.left) / f64::from(dest.width)));
    source.width = real_to_int(f64::from(source.width) * (f64::from(kept) / f64::from(dest.width)));
    dest.left = 0;
    dest.width = kept;
}

/// The negative-`top` mirror of [`clip_dest_bottom`].
fn clip_dest_top(dest: &mut Rect, source: &mut Rect) {
    if dest.top >= 0 || dest.height <= 0 {
        return;
    }
    let kept = dest.top + dest.height;
    source.top +=
        real_to_int(f64::from(source.height) * (f64::from(-dest.top) / f64::from(dest.height)));
    source.height =
        real_to_int(f64::from(source.height) * (f64::from(kept) / f64::from(dest.height)));
    dest.top = 0;
    dest.height = kept;
}

fn arg_integer(args: &[Variant], index: usize) -> Result<i32> {
    let value = args
        .get(index)
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0);
    Ok(value as i32)
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};

    use super::LayerExAreaAveragePlugin;

    /// A 4x4 source whose pixels are opaque and distinct, and a 2x2
    /// destination of transparent black: 4x4 -> 2x2 is an exact 2x2 box filter.
    const SCRIPT: &str = r#"
        global.src = new Layer();
        src.setImageSize(4, 4);
        global.dst = new Layer();
        dst.setImageSize(2, 2);
        for (var y = 0; y < 4; y++) {
            for (var x = 0; x < 4; x++) {
                var n = y * 4 + x + 1;
                src.fillRect(x, y, 1, 1, 0xff000000 | (0x0f * n << 16)
                    | (0x0e * n << 8) | 0x0d * n);
            }
        }
        dst.fillRect(0, 0, 2, 2, 0x00000000);
    "#;

    /// Per-channel step of source pixel `n` (`n = y*4 + x + 1`).
    const STEPS: (i64, i64, i64) = (0x0f, 0x0e, 0x0d);

    /// One source pixel's `0xRRGGBB`.
    fn pixel_color(n: i64) -> i64 {
        ((STEPS.0 * n) << 16) | ((STEPS.1 * n) << 8) | (STEPS.2 * n)
    }

    /// The exact box average the reference computes for destination pixel
    /// `(x, y)` of this 4x4 -> 2x2 shrink, truncated like its `int` division.
    ///
    /// The column/row range is the mapping `main.cpp:111-122` derives, clamp
    /// included: `ex`/`ey` are the *exclusive* ends of the mapped box, and the
    /// `ex >= sImageWidth` clamp sets them to `sImageWidth - 1`, so a box that
    /// ends exactly on the source's right/bottom edge loses its last
    /// column/row. Destination `(1, 0)`, `(0, 1)` and `(1, 1)` are that case.
    fn box_average(x: usize, y: usize) -> i64 {
        let clamp = |end: usize, image: usize| if end >= image { image - 1 } else { end };
        let ex = clamp(x * 2 + 2, 4);
        let ey = clamp(y * 2 + 2, 4);
        let mut sum = 0;
        let mut count = 0;
        for ay in y * 2..ey {
            for ax in x * 2..ex {
                sum += (ay * 4 + ax + 1) as i64;
                count += 1;
            }
        }
        ((STEPS.0 * sum / count) << 16) | ((STEPS.1 * sum / count) << 8) | (STEPS.2 * sum / count)
    }

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(LayerExAreaAveragePlugin)
            .expect("plugin");
        engine.execute_script("inline.tjs", SCRIPT).expect("script");
        engine
    }

    /// `main.cpp:104-171` with the 2x2 source box per destination pixel: the
    /// average of the mapped source pixels, each channel independently, with
    /// the reference's edge clamp for the boxes that reach the image edge.
    #[test]
    fn stretch_copy_aa_averages_the_source_box() {
        let mut engine = engine();
        engine
            .execute_script("aa.tjs", "dst.stretchCopyAA(0, 0, 2, 2, src, 0, 0, 4, 4);")
            .expect("stretchCopyAA");

        // Source pixel (x, y) is `(0x0f, 0x0e, 0x0d) * (y*4 + x + 1)`; each
        // destination pixel is the average of the box `box_average` maps.
        assert_eq!(main_pixel(&mut engine, "dst", 0, 0), box_average(0, 0));
        assert_eq!(main_pixel(&mut engine, "dst", 1, 0), box_average(1, 0));
        assert_eq!(main_pixel(&mut engine, "dst", 0, 1), box_average(0, 1));
        assert_eq!(main_pixel(&mut engine, "dst", 1, 1), box_average(1, 1));
        // Opaque source: the destination alpha is 255 everywhere.
        assert_eq!(mask_pixel(&mut engine, "dst", 0, 0), 255);
        assert_eq!(mask_pixel(&mut engine, "dst", 1, 1), 255);
        assert_eq!(call_on_paint(&mut engine, "dst"), 1);
    }

    /// Upscaling is refused with the reference's own message
    /// (`main.cpp:66-71`).
    #[test]
    fn stretch_copy_aa_refuses_to_upscale() {
        let mut engine = engine();
        let error = engine
            .execute_script(
                "upscale.tjs",
                "dst.stretchCopyAA(0, 0, 4, 4, src, 0, 0, 2, 2);",
            )
            .expect_err("upscale");
        assert_eq!(error.message, "stretchCopyAA は拡大処理を行なえません。");
        assert_eq!(main_pixel(&mut engine, "dst", 0, 0), 0, "nothing was drawn");
        assert_eq!(call_on_paint(&mut engine, "dst"), 0);
    }

    /// The destination clip blocks (`main.cpp:74-97`): a rectangle that runs
    /// past the image is truncated, and the source keeps the same ratio.
    #[test]
    fn stretch_copy_aa_clips_the_destination_against_the_image() {
        let mut engine = engine();
        // Two destination columns of an intended four: the source's left half,
        // so each destination pixel is one source pixel.
        engine
            .execute_script(
                "clip.tjs",
                "dst.stretchCopyAA(0, 0, 4, 2, src, 0, 0, 4, 2);",
            )
            .expect("stretchCopyAA");
        assert_eq!(main_pixel(&mut engine, "dst", 0, 0), pixel_color(1));
        assert_eq!(main_pixel(&mut engine, "dst", 1, 0), pixel_color(2));
    }

    /// A negative destination origin is cropped instead of writing before the
    /// buffer, the way the reference's overflow blocks crop.
    #[test]
    fn stretch_copy_aa_crops_a_negative_origin_instead_of_underflowing() {
        let mut engine = engine();
        engine
            .execute_script(
                "negative.tjs",
                "dst.stretchCopyAA(-1, 0, 2, 1, src, 0, 0, 3, 1);",
            )
            .expect("stretchCopyAA");

        // One destination pixel survives, and the source keeps the leading half
        // of its three columns: a 1.5 -> 1 pixel-wide box at source column 1.
        assert_eq!(main_pixel(&mut engine, "dst", 0, 0), pixel_color(2));
        assert_eq!(main_pixel(&mut engine, "dst", 1, 0), 0, "not touched");
    }

    /// A fully transparent source region writes the reference's zero colour
    /// (`main.cpp:157-171`) instead of leaving the destination alone.
    #[test]
    fn a_fully_transparent_source_region_writes_zero() {
        let mut engine = engine();
        engine
            .execute_script(
                "alpha.tjs",
                r#"
                var clear = new Layer();
                clear.setImageSize(4, 4);
                clear.fillRect(0, 0, 4, 4, 0x00000000);
                dst.fillRect(0, 0, 2, 2, 0xffffffff);
                dst.stretchCopyAA(0, 0, 2, 2, clear, 0, 0, 4, 4);
                "#,
            )
            .expect("stretchCopyAA");
        assert_eq!(main_pixel(&mut engine, "dst", 1, 1), 0);
        assert_eq!(mask_pixel(&mut engine, "dst", 1, 1), 0);
    }

    /// The reference's argument checks: `numparams < 9` is
    /// `TJS_E_BADPARAMCOUNT` (`main.cpp:22`), and a non-layer source must not
    /// crash.
    #[test]
    fn stretch_copy_aa_reports_bad_arguments() {
        let mut engine = engine();
        let error = engine
            .execute_script("bad.tjs", "dst.stretchCopyAA(0, 0, 2, 2, src, 0, 0, 4);")
            .expect_err("short argument list");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);

        let error = engine
            .execute_script("bad.tjs", "dst.stretchCopyAA(0, 0, 2, 2, 7, 0, 0, 4, 4);")
            .expect_err("non-layer source");
        assert_eq!(error.message, "stretchCopyAA: src must be Layer.");
    }

    /// A destination with a freed image is reported, not resurrected.
    #[test]
    fn stretch_copy_aa_reports_a_destination_without_an_image() {
        let mut engine = engine();
        engine
            .execute_script("free.tjs", "dst.freeImage();")
            .expect("freeImage");
        let error = engine
            .execute_script("aa.tjs", "dst.stretchCopyAA(0, 0, 2, 2, src, 0, 0, 4, 4);")
            .expect_err("freed destination");
        assert_eq!(error.message, "Not drawable layer type");
    }

    fn main_pixel(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMainPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    fn mask_pixel(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMaskPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    fn call_on_paint(engine: &mut KrkrEngine, layer: &str) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.callOnPaint"))
            .expect("callOnPaint")
            .to_integer()
            .expect("integer")
    }
}
