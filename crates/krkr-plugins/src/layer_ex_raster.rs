//! `layerExRaster.dll`: `Layer.copyRaster`, the sine-wave row shear.
//!
//! Real plugin: raster-style copy drawing (`layerExRaster/main.cpp`,
//! <https://github.com/wtnbgo/layerExRaster>).
//!
//! The family contract (`docs/plugins/layer-ex-family.md` §1): the plugin
//! attaches a member to the global `Layer` class, mutates the layer's raw
//! bitmap in place and calls `Layer.update()`. The reference reaches the
//! bitmap through `imageWidth`/`imageHeight`/`mainImageBuffer`/
//! `mainImageBufferPitch` and the clip box through `clipLeft`/`clipTop`/
//! `clipWidth`/`clipHeight` (`layerExDraw/layerExBase.hpp:76-129`, the base
//! `layerExRaster/main.cpp:19` includes); this port gets the same values from
//! [`krkr_engine::plugin_api::layer`] and calls [`layer_update`] for the
//! repaint.
//!
//! `copyRaster` copies the source layer into this layer row by row, shifting
//! each row horizontally by `d = sin(rad) * maxh` pixels and advancing
//! `rad` by `2*pi/lines` per row (`main.cpp:60-94`) — a travelling sine wave
//! that drags rows sideways. Whole pixels move, so the reference's B,G,R,A
//! byte order needs no translation (§B.3.4 applies only to per-channel work).
//!
//! Divergences from the reference, all of them crash paths the engine cannot
//! reproduce:
//!
//! * A source that is not a layer, or either layer without a main image, is a
//!   TJS error here; the reference dereferences the resulting null buffer.
//! * The reference reads and writes strictly inside its clip box, which the
//!   engine keeps inside the image, so row copies stay in bounds; the counts
//!   are clamped anyway so a corrupt clip box cannot panic the script thread.
//! * A degenerate `lines`/`cycle` (0) makes `rad` non-finite; the reference's
//!   `(int)` cast of NaN is VCL's `cvttsd2si`, which yields `INT_MIN` and skips
//!   the row. The port reproduces that result instead of the 0 a Rust `as`
//!   cast would give.

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{
        LayerBitmap, LayerBitmapView, LayerBitmapViewMut, layer_bitmap_read,
        layer_bitmap_read_write, layer_update,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer.copyRaster sine-wave row shear",
    notes: "copyRaster is a port of layerExRaster/main.cpp:38-97 over the engine's layer bitmap views: equal-size layers only, clip box applied, whole pixels moved, then Layer.update().",
    install: |engine| engine.register_plugin(LayerExRasterPlugin),
};

pub struct LayerExRasterPlugin;

impl KrkrPlugin for LayerExRasterPlugin {
    fn name(&self) -> &str {
        "layerExRaster.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `NCB_METHOD(copyRaster)` on a `void copyRaster(tTJSVariant, int, int,
        // int, tjs_int64)` (`main.cpp:38, 121-123`): five arguments, and a
        // shorter call is `TJS_E_BADPARAMCOUNT`.
        register_unless_closure(
            runtime,
            layer,
            "copyRaster",
            NativeArgCount::AtLeast(5),
            layer_copy_raster,
        );
        Ok(())
    }
}

fn layer_copy_raster(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = this_layer(this_obj)?;
    let Some(src) = args.first().and_then(Variant::object_handle) else {
        return Err(TjsError::runtime("copyRaster: src must be Layer."));
    };
    let maxh = arg_integer(&args, 1)? as i32;
    let lines = arg_integer(&args, 2)? as i32;
    let cycle = arg_integer(&args, 3)?;
    let time = arg_integer(&args, 4)?;

    // The reference reads the source's `imageWidth`/`imageHeight` and returns
    // without touching either layer — and without `redraw()` — when they differ
    // from its own (`main.cpp:56-58`). Checked before the write so a mismatch
    // commits nothing at all.
    let dest_size = layer_bitmap_read(runtime, dest, |view| {
        (view.bitmap.width, view.bitmap.height)
    })?;
    let source_size =
        layer_bitmap_read(runtime, src, |view| (view.bitmap.width, view.bitmap.height))?;
    if dest_size != source_size {
        return Ok(Variant::Void);
    }

    layer_bitmap_read_write(runtime, src, dest, |source, dest_view| {
        shear_rows(
            source,
            dest_view,
            maxh,
            lines,
            cycle,
            time,
            i64::from(source_size.1),
        );
    })?;
    layer_update(runtime, dest)?;
    Ok(Variant::Void)
}

/// The row loop of `copyRaster` (`main.cpp:60-94`).
///
/// `image_height` is the reference's `height`, the source layer's image height;
/// it only feeds the initial phase, through an integer `height/2`.
#[allow(clippy::too_many_arguments)]
fn shear_rows(
    source: &LayerBitmapView<'_>,
    dest: &mut LayerBitmapViewMut<'_>,
    maxh: i32,
    lines: i32,
    cycle: i64,
    time: i64,
    image_height: i64,
) {
    let pitch = source.bitmap.pitch as usize;
    let dest_pitch = dest.bitmap.pitch as usize;
    let (left, top, width, height) = clip_box(dest.bitmap);
    let width = width as i32;

    let omega = std::f64::consts::TAU / f64::from(lines);
    // `tjs_int CurH = (tjs_int)maxh`, the reference's commented-out envelope
    // replaced by the raw amplitude (`main.cpp:63-65`).
    let cur_h = f64::from(maxh);
    // `rad = -omega*time/cycle*(height/2)` with an *integer* `height/2`, then
    // `rad += omega*_clipTop` (`main.cpp:68-71`).
    let mut rad = -omega * time as f64 / cycle as f64 * (image_height / 2) as f64;
    rad += omega * f64::from(top);

    // `_buffer`/`buffer` are both advanced to the clip origin
    // (`main.cpp:72-73`).
    let source_base = top as usize * pitch + left as usize * 4;
    let dest_base = top as usize * dest_pitch + left as usize * 4;

    for row in 0..height as usize {
        let d = if rad.is_finite() {
            (rad.sin() * cur_h) as i32
        } else {
            i32::MIN
        };
        rad += omega;

        // `w`/`d` are `int` in the reference: `_clipWidth - d` and `_clipWidth
        // + d` wrap on the reference platform, and a non-positive `w` makes the
        // inner `for` copy nothing (`main.cpp:78-93`).
        let (count, source_shift, dest_shift) = if d >= 0 {
            (width.wrapping_sub(d), 0, d)
        } else {
            (width.wrapping_add(d), d.wrapping_neg(), 0)
        };
        if count <= 0 {
            continue;
        }

        let source_start = source_base + row * pitch + (source_shift.max(0) as usize) * 4;
        let dest_start = dest_base + row * dest_pitch + (dest_shift.max(0) as usize) * 4;
        copy_pixels(
            &source.pixels[source_start..],
            &mut dest.pixels[dest_start..],
            count as usize,
        );
    }
}

/// Copies `count` 4-byte pixels. `count` is clamped to what both sides hold:
/// the reference trusts the clip box to keep its row loop inside the buffer
/// (`main.cpp:81-92`), and the engine keeps it there too, but plugin code must
/// not be able to panic the script thread on a clip box it did not expect.
fn copy_pixels(source: &[u8], dest: &mut [u8], count: usize) {
    let count = count.min(source.len() / 4).min(dest.len() / 4);
    dest[..count * 4].copy_from_slice(&source[..count * 4]);
}

/// The reference's `_clipLeft`/`_clipTop`/`_clipWidth`/`_clipHeight`
/// (`layerExDraw/layerExBase.hpp:113-116`), clamped to the image the plane
/// indexes so the offsets above always address it.
fn clip_box(bitmap: LayerBitmap) -> (u32, u32, u32, u32) {
    let (left, top, width, height) = bitmap.clip;
    let left = left.clamp(0, i64::from(bitmap.width));
    let top = top.clamp(0, i64::from(bitmap.height));
    let width = width.clamp(0, i64::from(bitmap.width) - left);
    let height = height.clamp(0, i64::from(bitmap.height) - top);
    (left as u32, top as u32, width as u32, height as u32)
}

fn this_layer(this_obj: Option<ObjectHandle>) -> Result<ObjectHandle> {
    this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))
}

fn arg_integer(args: &[Variant], index: usize) -> Result<i64> {
    args.get(index)
        .map(Variant::to_integer)
        .transpose()
        .map(|value| value.unwrap_or(0))
}

/// Registers `function` unless a script already owns the member, the way
/// `layer_ex_draw.rs` attaches the rest of the family's surface.
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

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::ObjectHandle;

    use super::LayerExRasterPlugin;

    /// A source layer whose pixel `(x, y)` is `0x100000*(y*5+x+1) | alpha`,
    /// so every pixel of the 5x2 plane is distinguishable, plus a destination
    /// of the same size filled with transparent black.
    const SCRIPT: &str = r#"
        global.src = new Layer();
        src.setImageSize(5, 2);
        global.dst = new Layer();
        dst.setImageSize(5, 2);
        for (var y = 0; y < 2; y++) {
            for (var x = 0; x < 5; x++) {
                src.fillRect(x, y, 1, 1, 0x100000 * (y * 5 + x + 1) | 0x80000000);
            }
        }
        dst.fillRect(0, 0, 5, 2, 0x00000000);
    "#;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(LayerExRasterPlugin).expect("plugin");
        engine.execute_script("inline.tjs", SCRIPT).expect("script");
        engine
    }

    /// `main.cpp:61-78`: `omega = 2*pi/lines`, `rad = -omega*time/cycle*
    /// (height/2)`, `d = (int)(sin(rad)*maxh)` advancing by `omega` per row.
    /// For 5x2, `maxh=3`, `lines=4`, `cycle=4`, `time=1`: `d = -1` on row 0
    /// (`sin(-pi/8)*3 = -1.148`) and `d = 2` on row 1 (`sin(3pi/8)*3 = 2.772`).
    #[test]
    fn copy_raster_shifts_rows_by_the_sine_offsets() {
        let mut engine = engine();
        engine
            .execute_script("raster.tjs", "dst.copyRaster(src, 3, 4, 4, 1);")
            .expect("copyRaster");

        // d = -1: `w = width + d = 4`, source starts one pixel in, destination
        // starts at the clip origin; the last destination pixel keeps its own
        // content.
        for x in 0..4 {
            assert_eq!(
                pixel(&mut engine, "dst", x as i64, 0),
                color(x + 1, 0),
                "row 0 x={x}"
            );
        }
        assert_eq!(pixel(&mut engine, "dst", 4, 0), 0, "row 0 past the copy");

        // d = 2: `w = width - d = 3`, the row lands two pixels to the right and
        // the first two destination pixels keep their own content.
        assert_eq!(pixel(&mut engine, "dst", 0, 1), 0);
        assert_eq!(pixel(&mut engine, "dst", 1, 1), 0);
        for x in 0..3 {
            assert_eq!(
                pixel(&mut engine, "dst", (x + 2) as i64, 1),
                color(x, 1),
                "row 1 x={x}"
            );
        }

        // The source is untouched, and the family contract's repaint happened:
        // `Layer.update()` leaves `callOnPaint` set (`classes.rs:7218-7222`).
        assert_eq!(pixel(&mut engine, "src", 4, 1), color(4, 1));
        assert_eq!(call_on_paint(&mut engine, "dst"), 1);
    }

    /// The reference applies the *destination's* clip box to both buffers
    /// (`main.cpp:71-73`), and `update()` covers exactly that box.
    #[test]
    fn copy_raster_stays_inside_the_clip_box() {
        let mut engine = engine();
        engine
            .execute_script(
                "clip.tjs",
                "dst.setClip(1, 0, 3, 1); dst.copyRaster(src, 3, 4, 4, 1);",
            )
            .expect("copyRaster in a clip box");

        // `d = -1` over `clipWidth = 3`: two pixels copied one to the left.
        assert_eq!(pixel(&mut engine, "dst", 1, 0), color(2, 0));
        assert_eq!(pixel(&mut engine, "dst", 2, 0), color(3, 0));
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0, "outside the clip box");
        assert_eq!(pixel(&mut engine, "dst", 3, 0), 0, "outside the clip box");
        assert_eq!(pixel(&mut engine, "dst", 1, 1), 0, "outside the clip box");
    }

    /// `main.cpp:56-58`: a size mismatch does nothing at all, not even
    /// `redraw()`.
    #[test]
    fn copy_raster_ignores_layers_of_a_different_size() {
        let mut engine = engine();
        engine
            .execute_script(
                "mismatch.tjs",
                "var other = new Layer(); other.setImageSize(4, 2); other.fillRect(0, 0, 4, 2, 0xffffffff);",
            )
            .expect("other layer");
        let before = generation(&mut engine, "src");
        let dest_generation = generation(&mut engine, "dst");
        engine
            .execute_script("mismatch.tjs", "dst.copyRaster(other, 3, 4, 4, 1);")
            .expect("copyRaster");
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0, "nothing was copied");
        assert_eq!(
            generation(&mut engine, "src"),
            before,
            "the source keeps its image"
        );
        assert_eq!(
            call_on_paint(&mut engine, "dst"),
            0,
            "the reference returns before redraw()"
        );
        assert_eq!(
            generation(&mut engine, "dst"),
            dest_generation,
            "a mismatched copy does not even replace the destination image"
        );
    }

    /// Errors are the engine's, not a crash: a non-layer source, a layer
    /// without an image, and a short argument list.
    #[test]
    fn copy_raster_reports_bad_arguments_instead_of_crashing() {
        let mut engine = engine();
        let error = engine
            .execute_script("bad.tjs", "dst.copyRaster(42, 3, 4, 4, 1);")
            .expect_err("a non-layer source");
        assert_eq!(error.message, "copyRaster: src must be Layer.");

        let error = engine
            .execute_script("bad.tjs", "dst.copyRaster(src, 3, 4, 4);")
            .expect_err("a short argument list");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);

        engine
            .execute_script("free.tjs", "src.freeImage();")
            .expect("freeImage");
        let error = engine
            .execute_script("bad.tjs", "dst.copyRaster(src, 3, 4, 4, 1);")
            .expect_err("a freed source image");
        assert_eq!(error.message, "Not drawable layer type");
        assert_eq!(
            pixel(&mut engine, "dst", 0, 0),
            0,
            "the failed call copied nothing"
        );
    }

    /// A degenerate `lines`/`cycle` makes `rad` non-finite; the reference's
    /// `(int)` cast yields `INT_MIN`, whose wrapped `w` copies nothing.
    #[test]
    fn a_zero_period_or_line_count_copies_no_rows() {
        let mut engine = engine();
        engine
            .execute_script("degenerate.tjs", "dst.copyRaster(src, 3, 0, 4, 1);")
            .expect("copyRaster with lines = 0");
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0);
        assert_eq!(pixel(&mut engine, "dst", 3, 1), 0);
        engine
            .execute_script("degenerate.tjs", "dst.copyRaster(src, 3, 4, 0, 7);")
            .expect("copyRaster with cycle = 0");
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0);
    }

    /// `0xRRGGBB` of the pattern pixel `(x, y)`.
    fn color(x: usize, y: usize) -> i64 {
        0x100000 * ((y * 5 + x + 1) as i64)
    }

    fn pixel(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMainPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    /// `Layer.update()` leaves `callOnPaint` set (`classes.rs:7218-7222`);
    /// nothing else in these scripts touches it.
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

    /// The texture id of the layer's image, which a commit replaces.
    fn generation(engine: &mut KrkrEngine, name: &str) -> u64 {
        let handle = layer_handle(engine, name);
        krkr_engine::plugin_api::layer::layer_bitmap_read(
            engine.tjs_runtime_mut(),
            handle,
            |view| view.bitmap.generation,
        )
        .expect("generation")
    }
}
