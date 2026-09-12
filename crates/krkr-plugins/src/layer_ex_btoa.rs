//! `layerExBTOA.dll`: alpha and province-plane helpers for alpha movies.
//!
//! Real plugin: `copyRightBlueToLeftAlpha`, `copyBottomBlueToTopAlpha`,
//! `fillAlpha`, `copyAlphaToProvince`, `clipAlphaRect`, `fillByProvince`
//! (`layerExBTOA/Main.cpp`, <https://github.com/wtnbgo/layerExBTOA>).
//!
//! Six class-level functions on the global `Layer` class
//! (`Main.cpp:453-458`), called on an instance (`layer.fillAlpha()`, or the
//! `(Layer.copyRightBlueToLeftAlpha incontextof layer)()` form KAG's `Movie.tjs`
//! uses, `layerExBTOA/Movie.tjs:180-184`). They exist for alpha movies: a movie
//! plays into a layer with the alpha in the right (or bottom) half, and
//! `copyRightBlueToLeftAlpha`/`copyBottomBlueToTopAlpha` move that half's blue
//! channel into the other half's alpha channel (`Main.cpp:124-194`).
//!
//! Byte order: the reference reads and writes the alpha byte directly (byte 3
//! in both B,G,R,A and the engine's R,G,B,A), so only the two blue-channel
//! copies need the §B.3.4 translation — byte 0 in the reference is byte 2 in
//! the view. `fillByProvince`'s colour argument is the TJS `0xAARRGGBB` DWORD,
//! decomposed into the view's R,G,B,A order.
//!
//! Where this port differs from the reference, all of it crashes or
//! non-observable state there:
//!
//! * A layer without a main image, or a non-layer `this`, is the engine's
//!   `Not drawable layer type` error (`LayerBitmapError::NotDrawable`) where
//!   the reference throws `dest must be Layer.` / `src must be Layer.` after
//!   its `hasImage` check (`Main.cpp:134-136, 202-204, 315-320`).
//! * A layer without a province plane is the reference's `dst has no province
//!   image.` / `no province image.` error; nothing is allocated implicitly.
//! * The reference's clip box always lies inside the image (the engine clamps
//!   `setClip`, `classes.rs set_layer_clip_rect`), and offsets that would land
//!   outside the province plane are skipped instead of written.
//! * `clipAlphaRect`'s out-of-range path with `clear` fills the destination
//!   clip box and updates it (`Main.cpp:390-399`) — the reference dereferences
//!   a null buffer there, because its `goto none` jumps past the buffer fetch.
//! * An empty clip box (a script's `setClip(x, y, 0, 0)`) makes the clipped
//!   members no-ops; the reference's `GetClipSize` fails the `w > 0 && h > 0`
//!   test instead and throws `dest must be Layer.` (`Main.cpp:100-101`).
//!
//! The province plane is read and written through
//! [`layer_province_read`]/[`layer_province_write`]; the reference reaches it
//! through `provinceImageBuffer*` (`Main.cpp:22, 244-252, 423-430`).

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{
        LayerBitmap, LayerBitmapViewMut, LayerProvince, LayerProvinceView, layer_bitmap_read,
        layer_bitmap_read_write, layer_bitmap_write, layer_province_read, layer_province_write,
        layer_update,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer alpha/province helpers (copyRightBlueToLeftAlpha, copyBottomBlueToTopAlpha, fillAlpha, copyAlphaToProvince, clipAlphaRect, fillByProvince)",
    notes: "All six members ported from layerExBTOA/Main.cpp:128-451 over the layer bitmap and province views. `copyAlphaToProvince`/`fillByProvince` need an existing province plane and throw the reference's messages; offsets outside a plane are skipped instead of written.",
    install: |engine| engine.register_plugin(LayerExBtoaPlugin),
};

pub struct LayerExBtoaPlugin;

impl KrkrPlugin for LayerExBtoaPlugin {
    fn name(&self) -> &str {
        "layerExBTOA.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `NCB_ATTACH_FUNCTION(<name>, Layer, <name>)` (`Main.cpp:453-458`):
        // class-level functions a call on an instance still reaches, with `this`
        // bound to that instance.
        register_unless_closure(
            runtime,
            layer,
            "copyRightBlueToLeftAlpha",
            NativeArgCount::Any,
            copy_right_blue_to_left_alpha,
        );
        register_unless_closure(
            runtime,
            layer,
            "copyBottomBlueToTopAlpha",
            NativeArgCount::Any,
            copy_bottom_blue_to_top_alpha,
        );
        register_unless_closure(runtime, layer, "fillAlpha", NativeArgCount::Any, fill_alpha);
        register_unless_closure(
            runtime,
            layer,
            "copyAlphaToProvince",
            NativeArgCount::Any,
            copy_alpha_to_province,
        );
        // `if (numparams < 7) return TJS_E_BADPARAMCOUNT` and
        // `if (numparams < 2) ...` (`Main.cpp:298, 408`).
        register_unless_closure(
            runtime,
            layer,
            "clipAlphaRect",
            NativeArgCount::AtLeast(7),
            clip_alpha_rect,
        );
        register_unless_closure(
            runtime,
            layer,
            "fillByProvince",
            NativeArgCount::AtLeast(2),
            fill_by_province,
        );
        Ok(())
    }
}

// ------------------------------------------------- layer pixel/buffer helpers

fn this_layer(this_obj: Option<ObjectHandle>) -> Result<ObjectHandle> {
    this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))
}

/// `GetLayerBufferAndSize` (`Main.cpp:56-69`): the whole-image size and pitch,
/// or a failure the callers turn into their `dest must be Layer.` exception.
fn layer_size(bitmap: LayerBitmap) -> (usize, usize, usize) {
    (
        bitmap.width as usize,
        bitmap.height as usize,
        bitmap.pitch as usize,
    )
}

/// `GetClipBufferAndSize` (`Main.cpp:104-121`): the clip box the reference
/// offsets its buffer by (`ptr += pitch * t + l * 4`).
fn clip_box(bitmap: LayerBitmap) -> (usize, usize, usize, usize) {
    let (left, top, width, height) = bitmap.clip;
    let left = left.clamp(0, i64::from(bitmap.width));
    let top = top.clamp(0, i64::from(bitmap.height));
    let width = width.clamp(0, i64::from(bitmap.width) - left);
    let height = height.clamp(0, i64::from(bitmap.height) - top);
    (left as usize, top as usize, width as usize, height as usize)
}

/// Copies one byte of a pixel into another, ignoring offsets outside the plane
/// — the reference's raw pointer writes assume its clip box is inside the
/// image, which the engine guarantees but a stale province plane may not.
fn copy_channel(pixels: &mut [u8], dest: usize, source: usize) {
    let Some(value) = pixels.get(source).copied() else {
        return;
    };
    if let Some(slot) = pixels.get_mut(dest) {
        *slot = value;
    }
}

// ------------------------------------------------ copyRightBlueToLeftAlpha

/// `main.cpp:128-158`: the right half's blue into the left half's alpha, over
/// the whole image (no clip).
fn copy_right_blue_to_left_alpha(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    layer_bitmap_write(runtime, layer, |view| {
        let (width, height, pitch) = layer_size(view.bitmap);
        let half = width / 2;
        for y in 0..height {
            let row = y * pitch;
            for x in 0..half {
                copy_channel(view.pixels, row + x * 4 + 3, row + (x + half) * 4 + 2);
            }
        }
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

/// `main.cpp:164-194`: the bottom half's blue into the top half's alpha, over
/// the whole width (no clip).
fn copy_bottom_blue_to_top_alpha(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    layer_bitmap_write(runtime, layer, |view| {
        let (width, height, pitch) = layer_size(view.bitmap);
        let half = height / 2;
        for y in 0..half {
            let row = y * pitch;
            let source_row = (y + half) * pitch;
            for x in 0..width {
                copy_channel(view.pixels, row + x * 4 + 3, source_row + x * 4 + 2);
            }
        }
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

/// `main.cpp:196-218`: `0xff` into the clip box's alpha channel.
fn fill_alpha(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    layer_bitmap_write(runtime, layer, |view| {
        let pitch = view.bitmap.pitch as usize;
        let (left, top, width, height) = clip_box(view.bitmap);
        for y in 0..height {
            for x in 0..width {
                if let Some(slot) = view.pixels.get_mut((top + y) * pitch + (left + x) * 4 + 3) {
                    *slot = 0xff;
                }
            }
        }
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

// ------------------------------------------------- copyAlphaToProvince

/// The reference's three province-writing modes (`main.cpp:256-274`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AlphaProvinceMode {
    /// `threshold < 0`: copy the alpha byte.
    Raw,
    /// `0 <= threshold < 256`: `alpha >= threshold` becomes 1, else 0.
    Threshold(u8),
    /// `threshold >= 256`: zero the region.
    Zero,
}

/// `main.cpp:220-281`: the clip box's alpha into the layer's province plane.
fn copy_alpha_to_province(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    // `threshold` defaults to -1, and a `void` argument keeps the default
    // (`Main.cpp:227-230`).
    let threshold = match args.first() {
        None | Some(Variant::Void) => -1,
        Some(value) => value.to_integer()?,
    };
    let mode = if threshold < 0 {
        AlphaProvinceMode::Raw
    } else if threshold < 256 {
        AlphaProvinceMode::Threshold(threshold as u8)
    } else {
        AlphaProvinceMode::Zero
    };

    // `GetClipSize` reads the main image (`Main.cpp:232-234`), so the alphas
    // come first and the province write follows: the engine lends one plane at
    // a time.
    let (left, top, width, height, alphas) = layer_bitmap_read(runtime, layer, |view| {
        let pitch = view.bitmap.pitch as usize;
        let (left, top, width, height) = clip_box(view.bitmap);
        let mut alphas = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                alphas.push(
                    view.pixels
                        .get((top + y) * pitch + (left + x) * 4 + 3)
                        .copied()
                        .unwrap_or(0),
                );
            }
        }
        (left, top, width, height, alphas)
    })?;

    layer_province_write(runtime, layer, false, |view| {
        if view.province.width == 0 || view.province.height == 0 {
            // `TVPThrowExceptionMessage(TJS_W("dst has no province image."))`
            // (`Main.cpp:246`).
            return Err(TjsError::runtime("dst has no province image."));
        }
        let pitch = view.province.width as usize;
        for y in 0..height {
            for x in 0..width {
                let alpha = alphas[y * width + x];
                let value = match mode {
                    AlphaProvinceMode::Raw => alpha,
                    AlphaProvinceMode::Threshold(threshold) => u8::from(alpha >= threshold),
                    AlphaProvinceMode::Zero => 0,
                };
                if let Some(slot) = view.pixels.get_mut((top + y) * pitch + left + x) {
                    *slot = value;
                }
            }
        }
        Ok(())
    })??;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

// ----------------------------------------------------------- clipAlphaRect

/// The surviving rectangles of `clipAlphaRect`'s clipping cascade
/// (`Main.cpp:322-353`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AlphaClip {
    /// Clip-box-relative destination origin.
    dest_left: i32,
    dest_top: i32,
    /// Destination extent in pixels.
    width: i32,
    height: i32,
    /// Source origin in the source image.
    source_left: i32,
    source_top: i32,
}

/// `main.cpp:283-400`: multiply the destination's alpha by the source's over a
/// clipped rectangle, optionally clearing the rest of the destination clip box
/// to `clear`.
fn clip_alpha_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = this_layer(this_obj)?;
    let (dleft, dtop) = (arg_integer(&args, 0)?, arg_integer(&args, 1)?);
    let Some(src) = args.get(2).and_then(Variant::object_handle) else {
        return Err(TjsError::runtime("clipAlphaRect: src must be Layer."));
    };
    let (sleft, stop) = (arg_integer(&args, 3)?, arg_integer(&args, 4)?);
    let (width, height) = (arg_integer(&args, 5)?, arg_integer(&args, 6)?);
    // `clear` is active only for 0..=255; anything else (including `void`)
    // leaves the destination alone (`Main.cpp:307-311`).
    let clear = match args.get(7) {
        None | Some(Variant::Void) => None,
        Some(value) => {
            let value = value.to_integer()?;
            (0..256).contains(&value).then_some(value as u8)
        }
    };
    if width <= 0 || height <= 0 {
        return Err(TjsError::invalid_param());
    }

    // `GetClipSize(dst)` then `GetLayerSize(src)` (`Main.cpp:314-320`), both
    // before any buffer is fetched.
    let dest_clip = layer_bitmap_read(runtime, dest, |view| clip_box(view.bitmap))?;
    let source_size = layer_bitmap_read(runtime, src, |view| {
        (view.bitmap.width as i32, view.bitmap.height as i32)
    })?;
    let clip = clip_alpha_cascade(
        dest_clip,
        source_size,
        dleft,
        dtop,
        sleft,
        stop,
        width,
        height,
    );

    let Some(clip) = clip else {
        // `Main.cpp:390-399`: nothing is multiplied. With `clear` the whole
        // destination clip box is filled — the reference reaches for a null
        // destination buffer here.
        if let Some(value) = clear {
            layer_bitmap_write(runtime, dest, |view| {
                clear_alpha_clip(view, dest_clip, value);
            })?;
            layer_update(runtime, dest)?;
        }
        return Ok(Variant::Void);
    };

    // `Main.cpp:364-383`: `dbuf` is the destination clip's origin, `sbuf` the
    // source image's, and only the destination's clip box bounds the walk.
    layer_bitmap_read_write(runtime, src, dest, |source, dest_view| {
        let (clip_left, clip_top, _, _) = dest_clip;
        let pitch = dest_view.bitmap.pitch as usize;
        let source_pitch = source.bitmap.pitch as usize;
        let base = clip_top * pitch + clip_left * 4;

        if let Some(value) = clear {
            // Rows above and below the multiplied band, over the clip width.
            for y in 0..clip.dest_top as usize {
                for x in 0..dest_clip.2 {
                    if let Some(slot) = dest_view.pixels.get_mut(base + y * pitch + x * 4 + 3) {
                        *slot = value;
                    }
                }
            }
            for y in (clip.dest_top + clip.height) as usize..dest_clip.3 {
                for x in 0..dest_clip.2 {
                    if let Some(slot) = dest_view.pixels.get_mut(base + y * pitch + x * 4 + 3) {
                        *slot = value;
                    }
                }
            }
        }

        for y in 0..clip.height as usize {
            let dest_row = base + (clip.dest_top as usize + y) * pitch;
            if let Some(value) = clear {
                for x in 0..clip.dest_left as usize {
                    if let Some(slot) = dest_view.pixels.get_mut(dest_row + x * 4 + 3) {
                        *slot = value;
                    }
                }
            }
            for x in 0..clip.width as usize {
                let dest_offset = dest_row + (clip.dest_left + x as i32) as usize * 4 + 3;
                let source_offset = (clip.source_top + y as i32) as usize * source_pitch
                    + (clip.source_left + x as i32) as usize * 4
                    + 3;
                let Some(&source_alpha) = source.pixels.get(source_offset) else {
                    continue;
                };
                let Some(&dest_alpha) = dest_view.pixels.get(dest_offset) else {
                    continue;
                };
                let product = u32::from(dest_alpha) * u32::from(source_alpha);
                if let Some(slot) = dest_view.pixels.get_mut(dest_offset) {
                    *slot = ((product + (product >> 7)) >> 8) as u8;
                }
            }
            if let Some(value) = clear {
                for x in (clip.dest_left + clip.width) as usize..dest_clip.2 {
                    if let Some(slot) = dest_view.pixels.get_mut(dest_row + x * 4 + 3) {
                        *slot = value;
                    }
                }
            }
        }
    })?;
    layer_update(runtime, dest)?;
    Ok(Variant::Void)
}

/// The two update rectangles the reference reports when nothing is
/// multiplied: only the `clear` path repaints.
fn clear_alpha_clip(
    view: &mut LayerBitmapViewMut<'_>,
    clip: (usize, usize, usize, usize),
    value: u8,
) {
    let pitch = view.bitmap.pitch as usize;
    let (left, top, width, height) = clip;
    for y in 0..height {
        for x in 0..width {
            if let Some(slot) = view.pixels.get_mut((top + y) * pitch + (left + x) * 4 + 3) {
                *slot = value;
            }
        }
    }
}

/// The `Main.cpp:322-353` cascade over the destination's clip box and the
/// source image's size: `None` is the reference's `goto none`.
#[allow(clippy::too_many_arguments)]
fn clip_alpha_cascade(
    dest_clip: (usize, usize, usize, usize),
    source_size: (i32, i32),
    dleft: i32,
    dtop: i32,
    sleft: i32,
    stop: i32,
    mut width: i32,
    mut height: i32,
) -> Option<AlphaClip> {
    let (_, _, diw, dih) = dest_clip;
    let (diw, dih) = (diw as i32, dih as i32);
    let (siw, sih) = source_size;
    let (mut dx, mut dy) = (dleft, dtop);
    let (mut sx, mut sy) = (sleft, stop);

    // srcが範囲外
    if sx + width <= 0 || sy + height <= 0 || sx >= siw || sy >= sih {
        return None;
    }
    // srcの負方向のカット
    if sx < 0 {
        width += sx;
        dx -= sx;
        sx = 0;
    }
    if sy < 0 {
        height += sy;
        dy -= sy;
        sy = 0;
    }
    // srcの正方向のカット
    let cut = sx + width - siw;
    if cut > 0 {
        width -= cut;
    }
    let cut = sy + height - sih;
    if cut > 0 {
        height -= cut;
    }
    // dstが範囲外
    if dx + width <= 0 || dy + height <= 0 || dx >= diw || dy >= dih {
        return None;
    }
    // dstの負方向のカット
    if dx < 0 {
        width += dx;
        sx -= dx;
        dx = 0;
    }
    if dy < 0 {
        height += dy;
        sy -= dy;
        dy = 0;
    }
    // dstの正方向のカット
    let cut = dx + width - diw;
    if cut > 0 {
        width -= cut;
    }
    let cut = dy + height - dih;
    if cut > 0 {
        height -= cut;
    }
    if width <= 0 || height <= 0 {
        return None;
    }

    Some(AlphaClip {
        dest_left: dx,
        dest_top: dy,
        width,
        height,
        source_left: sx,
        source_top: sy,
    })
}

// --------------------------------------------------------- fillByProvince

/// `main.cpp:403-451`: paint the clip box's pixels whose province index is
/// `index` with `0xAARRGGBB` `color`.
fn fill_by_province(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    // `(unsigned char)(int)*param[0]` and `(DWORD)(int)*param[1]`
    // (`Main.cpp:409-410`).
    let index = arg_integer(&args, 0)? as u8;
    let color = arg_integer(&args, 1)? as u32;

    let provinces = layer_province_read(runtime, layer, |view| {
        if view.province.width == 0 || view.province.height == 0 {
            // `TVPThrowExceptionMessage(TJS_W("no province image."))`
            // (`Main.cpp:425`).
            return Err(TjsError::runtime("no province image."));
        }
        Ok(province_plane(view))
    })??;

    layer_bitmap_write(runtime, layer, |view| {
        let pitch = view.bitmap.pitch as usize;
        let (left, top, width, height) = clip_box(view.bitmap);
        for y in 0..height {
            for x in 0..width {
                if provinces.value(left + x, top + y) != index {
                    continue;
                }
                let offset = (top + y) * pitch + (left + x) * 4;
                if let Some(pixel) = view.pixels.get_mut(offset..offset + 4) {
                    pixel[0] = ((color >> 16) & 0xff) as u8;
                    pixel[1] = ((color >> 8) & 0xff) as u8;
                    pixel[2] = (color & 0xff) as u8;
                    pixel[3] = ((color >> 24) & 0xff) as u8;
                }
            }
        }
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

/// A copy of the province plane: the layer's main bitmap and its province
/// plane are handed out one at a time, so the indices are read first.
struct ProvincePlane {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl ProvincePlane {
    /// The province index at an image coordinate; outside the plane reads 0,
    /// like `ProvinceImage::pixel` (`krkr-core/src/lib.rs:1427-1435`).
    fn value(&self, x: usize, y: usize) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.pixels.get(y * self.width + x).copied().unwrap_or(0)
    }
}

fn province_plane(view: &LayerProvinceView<'_>) -> ProvincePlane {
    let province: LayerProvince = view.province;
    ProvincePlane {
        width: province.width as usize,
        height: province.height as usize,
        pixels: view.pixels.to_vec(),
    }
}

fn arg_integer(args: &[Variant], index: usize) -> Result<i32> {
    let value = args
        .get(index)
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0);
    Ok(value as i32)
}

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

    use super::LayerExBtoaPlugin;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(LayerExBtoaPlugin).expect("plugin");
        engine
    }

    /// `main.cpp:142-154`: the right half's blue byte (engine byte 2) becomes
    /// the left half's alpha byte (engine byte 3).
    #[test]
    fn copy_right_blue_to_left_alpha_moves_the_split_half() {
        let mut engine = engine();
        engine
            .execute_script(
                "srca.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 2);
                layer.fillRect(0, 0, 4, 2, 0x00000000);
                layer.fillRect(2, 0, 1, 1, 0x00000011);
                layer.fillRect(3, 0, 1, 1, 0x00000022);
                layer.fillRect(2, 1, 1, 1, 0x00000033);
                layer.fillRect(3, 1, 1, 1, 0x00000044);
                layer.copyRightBlueToLeftAlpha();
                "#,
            )
            .expect("copyRightBlueToLeftAlpha");

        assert_eq!(mask(&mut engine, 0, 0), 0x11);
        assert_eq!(mask(&mut engine, 1, 0), 0x22);
        assert_eq!(mask(&mut engine, 0, 1), 0x33);
        assert_eq!(mask(&mut engine, 1, 1), 0x44);
        // The left half's own colour is untouched, and so is the right half.
        assert_eq!(main(&mut engine, 0, 0), 0x000000);
        assert_eq!(main(&mut engine, 3, 0), 0x000022);
        assert_eq!(
            mask(&mut engine, 3, 0),
            0x00,
            "the right half keeps its own alpha"
        );
        assert_eq!(call_on_paint(&mut engine), 1);
    }

    /// `main.cpp:178-190`: the bottom half's blue into the top half's alpha,
    /// across the whole width.
    #[test]
    fn copy_bottom_blue_to_top_alpha_moves_the_split_half() {
        let mut engine = engine();
        engine
            .execute_script(
                "srcb.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(2, 4);
                layer.fillRect(0, 0, 2, 4, 0x00000000);
                layer.fillRect(0, 2, 1, 1, 0x00000055);
                layer.fillRect(1, 2, 1, 1, 0x00000066);
                layer.fillRect(0, 3, 1, 1, 0x00000077);
                layer.fillRect(1, 3, 1, 1, 0x00000088);
                layer.copyBottomBlueToTopAlpha();
                "#,
            )
            .expect("copyBottomBlueToTopAlpha");

        assert_eq!(mask(&mut engine, 0, 0), 0x55);
        assert_eq!(mask(&mut engine, 1, 0), 0x66);
        assert_eq!(mask(&mut engine, 0, 1), 0x77);
        assert_eq!(mask(&mut engine, 1, 1), 0x88);
        assert_eq!(main(&mut engine, 0, 0), 0x000000);
        assert_eq!(call_on_paint(&mut engine), 1);
    }

    /// `main.cpp:205-214`: `0xff` into the clip box's alpha only.
    #[test]
    fn fill_alpha_covers_the_clip_box_only() {
        let mut engine = engine();
        engine
            .execute_script(
                "fillalpha.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 2);
                layer.fillRect(0, 0, 4, 2, 0x00123456);
                layer.setClip(1, 0, 2, 2);
                layer.fillAlpha();
                "#,
            )
            .expect("fillAlpha");

        for x in 0..4 {
            for y in 0..2 {
                let inside = (1..3).contains(&x);
                assert_eq!(
                    mask(&mut engine, x, y),
                    if inside { 0xff } else { 0x00 },
                    "({x}, {y})"
                );
                assert_eq!(main(&mut engine, x, y), 0x123456, "the colour is untouched");
            }
        }
        assert_eq!(call_on_paint(&mut engine), 1);
    }

    /// `main.cpp:256-277`: raw, thresholded and zeroed province writes over the
    /// clip box.
    #[test]
    fn copy_alpha_to_province_has_the_three_reference_modes() {
        let mut engine = engine();
        engine
            .execute_script(
                "province.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 1);
                layer.fillRect(0, 0, 4, 1, 0x00000000);
                layer.setMaskPixel(0, 0, 0x00);
                layer.setMaskPixel(1, 0, 0x40);
                layer.setMaskPixel(2, 0, 0x7f);
                layer.setMaskPixel(3, 0, 0xff);
                // Allocates the province plane the way setProvincePixel does.
                layer.setProvincePixel(0, 0, 3);
                layer.copyAlphaToProvince();
                "#,
            )
            .expect("raw province copy");
        assert_eq!(province(&mut engine, 0, 0), 0x00);
        assert_eq!(province(&mut engine, 1, 0), 0x40);
        assert_eq!(province(&mut engine, 3, 0), 0xff);

        engine
            .execute_script("threshold.tjs", "layer.copyAlphaToProvince(0x40);")
            .expect("thresholded province copy");
        assert_eq!(province(&mut engine, 0, 0), 0);
        assert_eq!(province(&mut engine, 1, 0), 1);
        assert_eq!(province(&mut engine, 3, 0), 1);

        engine
            .execute_script("zero.tjs", "layer.copyAlphaToProvince(256);")
            .expect("zeroing province copy");
        assert_eq!(province(&mut engine, 1, 0), 0);
        assert_eq!(province(&mut engine, 3, 0), 0);
        assert_eq!(call_on_paint(&mut engine), 1);
    }

    /// `main.cpp:244-252`: a layer without a province plane throws the
    /// reference's message instead of allocating one.
    #[test]
    fn copy_alpha_to_province_needs_an_existing_plane() {
        let mut engine = engine();
        engine
            .execute_script(
                "noplane.tjs",
                "global.layer = new Layer(); layer.setImageSize(2, 2); layer.fillRect(0, 0, 2, 2, 0xff00ff00);",
            )
            .expect("layer");
        let error = engine
            .execute_script("noplane.tjs", "layer.copyAlphaToProvince();")
            .expect_err("no province plane");
        assert_eq!(error.message, "dst has no province image.");
    }

    /// `main.cpp:373-383`: the destination alpha is multiplied by the source
    /// alpha, `(n + (n >> 7)) >> 8`.
    #[test]
    fn clip_alpha_rect_multiplies_the_alpha_bytes() {
        let mut engine = engine();
        engine
            .execute_script(
                "cliprect.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 1);
                layer.fillRect(0, 0, 4, 1, 0x80808080);
                global.src = new Layer();
                src.setImageSize(4, 1);
                src.fillRect(0, 0, 4, 1, 0xffffffff);
                src.setMaskPixel(0, 0, 0x80);
                src.setMaskPixel(1, 0, 0x80);
                layer.clipAlphaRect(0, 0, src, 0, 0, 2, 1);
                "#,
            )
            .expect("clipAlphaRect");

        // 128 * 128 -> (16384 + 128) >> 8 = 64; 128 * 255 -> 128.
        assert_eq!(mask(&mut engine, 0, 0), 64);
        assert_eq!(mask(&mut engine, 1, 0), 64);
        assert_eq!(mask(&mut engine, 2, 0), 128);
        assert_eq!(mask(&mut engine, 3, 0), 128);
        assert_eq!(main(&mut engine, 0, 0), 0x808080, "only alpha is written");
        assert_eq!(call_on_paint(&mut engine), 1);
    }

    /// `main.cpp:307-311, 369-372`: `clear` fills everything outside the
    /// multiplied rectangle inside the clip box.
    #[test]
    fn clip_alpha_rect_clear_fills_the_rest_of_the_clip_box() {
        let mut engine = engine();
        engine
            .execute_script(
                "clear.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 3);
                layer.fillRect(0, 0, 4, 3, 0x80808080);
                global.src = new Layer();
                src.setImageSize(4, 3);
                src.fillRect(0, 0, 4, 3, 0xffffffff);
                layer.clipAlphaRect(1, 1, src, 1, 1, 2, 1, 0);
                "#,
            )
            .expect("clipAlphaRect with clear");

        // The multiplied band (1..3, 1) keeps 128 * 255 -> 128.
        assert_eq!(mask(&mut engine, 1, 1), 128);
        assert_eq!(mask(&mut engine, 2, 1), 128);
        // Everything else in the clip box is cleared to 0.
        for x in 0..4 {
            for y in 0..3 {
                let multiplied = y == 1 && (1..3).contains(&x);
                if !multiplied {
                    assert_eq!(mask(&mut engine, x, y), 0, "({x}, {y})");
                }
            }
        }
        // `void` as the clear argument means "do not clear".
        engine
            .execute_script(
                "noclear.tjs",
                "layer.fillRect(0, 0, 4, 3, 0x80808080); layer.clipAlphaRect(1, 1, src, 1, 1, 2, 1, void);",
            )
            .expect("clipAlphaRect without clear");
        assert_eq!(mask(&mut engine, 0, 0), 128, "the rest of the box is kept");
    }

    /// The reference clears from a null buffer when the rectangle is fully
    /// outside (`Main.cpp:390-399`); the port performs that clear.
    #[test]
    fn clip_alpha_rect_clears_when_the_source_rectangle_is_off_image() {
        let mut engine = engine();
        engine
            .execute_script(
                "off.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(2, 2);
                layer.fillRect(0, 0, 2, 2, 0x80808080);
                global.src = new Layer();
                src.setImageSize(2, 2);
                src.fillRect(0, 0, 2, 2, 0xffffffff);
                layer.clipAlphaRect(0, 0, src, 40, 40, 2, 2, 0);
                "#,
            )
            .expect("out-of-range clipAlphaRect");
        assert_eq!(mask(&mut engine, 0, 0), 0);
        assert_eq!(mask(&mut engine, 1, 1), 0);

        // Without `clear` the reference does nothing at all.
        engine
            .execute_script(
                "keep.tjs",
                "layer.fillRect(0, 0, 2, 2, 0x80808080); layer.clipAlphaRect(0, 0, src, 40, 40, 2, 2);",
            )
            .expect("out-of-range clipAlphaRect without clear");
        assert_eq!(mask(&mut engine, 0, 0), 0x80);
    }

    /// `main.cpp:312`: a non-positive extent is `TJS_E_INVALIDPARAM`, and a
    /// short argument list is `TJS_E_BADPARAMCOUNT`.
    #[test]
    fn clip_alpha_rect_reports_bad_arguments() {
        let mut engine = engine();
        engine
            .execute_script(
                "args.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(2, 2);
                layer.fillRect(0, 0, 2, 2, 0x80808080);
                global.src = new Layer();
                src.setImageSize(2, 2);
                src.fillRect(0, 0, 2, 2, 0xffffffff);
                "#,
            )
            .expect("layers");
        let error = engine
            .execute_script("args.tjs", "layer.clipAlphaRect(0, 0, src, 0, 0, 0, 1);")
            .expect_err("zero width");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::InvalidParam);

        let error = engine
            .execute_script("args.tjs", "layer.clipAlphaRect(0, 0, src, 0, 0, 1);")
            .expect_err("short argument list");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
    }

    /// `main.cpp:436-447`: pixels whose province index matches get the packed
    /// `0xAARRGGBB` colour.
    #[test]
    fn fill_by_province_paints_the_matching_pixels() {
        let mut engine = engine();
        engine
            .execute_script(
                "province.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(3, 1);
                layer.fillRect(0, 0, 3, 1, 0x00000000);
                layer.setProvincePixel(0, 0, 0);
                layer.setProvincePixel(1, 0, 7);
                layer.setProvincePixel(2, 0, 7);
                layer.fillByProvince(7, 0xaabbccdd);
                "#,
            )
            .expect("fillByProvince");

        assert_eq!(main(&mut engine, 0, 0), 0x000000);
        assert_eq!(mask(&mut engine, 0, 0), 0x00);
        assert_eq!(main(&mut engine, 1, 0), 0xbbccdd);
        assert_eq!(mask(&mut engine, 1, 0), 0xaa);
        assert_eq!(main(&mut engine, 2, 0), 0xbbccdd);
        assert_eq!(call_on_paint(&mut engine), 1);
    }

    /// `main.cpp:423-430`: `fillByProvince` reports a missing plane, and needs
    /// both arguments.
    #[test]
    fn fill_by_province_reports_missing_planes_and_arguments() {
        let mut engine = engine();
        engine
            .execute_script(
                "noplane.tjs",
                "global.layer = new Layer(); layer.setImageSize(2, 2); layer.fillRect(0, 0, 2, 2, 0x00ffffff);",
            )
            .expect("layer");
        let error = engine
            .execute_script("noplane.tjs", "layer.fillByProvince(1, 0xff0000);")
            .expect_err("no province plane");
        assert_eq!(error.message, "no province image.");

        let error = engine
            .execute_script("noplane.tjs", "layer.fillByProvince(1);")
            .expect_err("short argument list");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
    }

    /// Every member needs a layer with an image; a freed one reports the
    /// engine's error instead of writing through a null buffer.
    #[test]
    fn the_members_report_a_layer_without_an_image() {
        let mut engine = engine();
        engine
            .execute_script(
                "free.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(2, 2);
                layer.fillRect(0, 0, 2, 2, 0xff00ff00);
                layer.freeImage();
                "#,
            )
            .expect("freeImage");
        for call in [
            "layer.copyRightBlueToLeftAlpha();",
            "layer.copyBottomBlueToTopAlpha();",
            "layer.fillAlpha();",
        ] {
            let error = engine
                .execute_script("free.tjs", call)
                .expect_err("a freed image");
            assert_eq!(error.message, "Not drawable layer type", "{call}");
        }
    }

    /// The class-level call form `Movie.tjs` uses
    /// (`(global.Layer.fillAlpha incontextof layer)()`).
    #[test]
    fn the_members_work_through_incontextof() {
        let mut engine = engine();
        engine
            .execute_script(
                "incontextof.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(2, 1);
                layer.fillRect(0, 0, 2, 1, 0x00123456);
                (global.Layer.fillAlpha incontextof layer)();
                "#,
            )
            .expect("incontextof call");
        assert_eq!(mask(&mut engine, 0, 0), 0xff);
    }

    fn main(engine: &mut KrkrEngine, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("layer.getMainPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    fn mask(engine: &mut KrkrEngine, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("layer.getMaskPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    fn province(engine: &mut KrkrEngine, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("layer.getProvincePixel({x}, {y})"))
            .expect("province pixel")
            .to_integer()
            .expect("integer")
    }

    fn call_on_paint(engine: &mut KrkrEngine) -> i64 {
        engine
            .execute_expression("read.tjs", "layer.callOnPaint")
            .expect("callOnPaint")
            .to_integer()
            .expect("integer")
    }
}
