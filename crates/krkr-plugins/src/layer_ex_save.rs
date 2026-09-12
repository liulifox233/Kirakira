//! `layerExSave.dll`: the family's save/clip half — `Layer.getCropRect` and
//! friends plus PNG/TLG5 writers.
//!
//! Real plugin: `krkrz/src/plugins/win32/layerExSave/`, upstream
//! <https://github.com/wtnbgo/layerExSave>
//! (`docs/plugins/layer-ex-family.md` §2.4). Two surfaces:
//!
//! * **`Layer` class functions** (`utils.cpp:120-512`, `savepng.cpp`,
//!   `savetlg5.cpp`): `getCropRect`, `getCropRectZero`, `getDiffRect`,
//!   `getDiffPixel`, `oozeColor`, `copyBlueToAlpha`, `isBlank`,
//!   `clearAlpha`, `saveLayerImagePng`, `saveLayerImagePngOctet`,
//!   `saveLayerImageTlg5`. (The compiled names come from the macros' first
//!   argument — `NCB_ATTACH_FUNCTION(oozeColor, …)`,
//!   `NCB_ATTACH_FUNCTION(copyBlueToAlpha, …)` — which is also what
//!   `manual.tjs` documents; the dossier's `OozeColor`/`CopyBlueToAlpha`
//!   capitalisation is not what the reference registers.)
//! * **`Window` methods** (`Main.cpp:342-346`): `startSaveLayerImage`,
//!   `cancelSaveLayerImage`, `stopSaveLayerImage`, with the
//!   `onSaveLayerImageProgress`/`onSaveLayerImageDone` events
//!   (`Main.cpp:143-321`).
//!
//! # Byte order and channel mapping
//!
//! The reference's buffer is B, G, R, A per pixel; the engine's plane is
//! R, G, B, A (`plugin_api::layer`), so every per-channel byte index is
//! translated the way §B.3.4 of
//! `docs/plugins/plugin-facing-engine-facilities.md` prescribes:
//!
//! | reference | engine plane | used by |
//! |---|---|---|
//! | alpha (`p[3]`) | byte 3 | `getCropRect`, `getDiffRect`, `clearAlpha` |
//! | blue (`p[0]`) | byte 2 | `copyBlueToAlpha`, `isBlank` |
//! | `0xAARRGGBB` DWORD | `[R, G, B, A]` bytes | `getDiffPixel`, `clearAlpha`, `oozeColor` |
//!
//! `getCropRectZero` and `IS_SAME_COLOR` are per-channel comparisons, so
//! their result is order-independent. The PNG writer emits R, G, B, A
//! (reference `writePixel` writes `p[2], p[1], p[0], p[3]`, `savepng.cpp:47-52`);
//! the TLG5 writer feeds its channel composition B, G, R, A, the order the
//! format defines (`savetlg5.cpp:96-121`).
//!
//! # Real vs mapped
//!
//! Real: all eight pixel helpers, the PNG writer (`savepng.cpp:158-255`
//! structure and error strings), the TLG5 writer (`savetlg5.cpp:20-171`,
//! including the `TLG0.0` tag container) and the PNG octet form.
//!
//! Mapped, with the reason:
//!
//! * **PNG compression is this module's own deflate** (`deflate` below):
//!   fixed-Huffman LZ77 for levels 1-9, stored blocks for level 0. The repo
//!   already depends on `flate2`, but this crate's production dependencies
//!   are `krkr-engine` and `krkr-tjs2` only, so no zlib is linkable here.
//!   The output is a normal zlib stream; `comp_lv` selects press/no-press
//!   and match-search effort rather than reproducing zlib's algorithm, and
//!   the reference's own readme already warns its PNG path is unfiltered and
//!   compresses worse than libpng.
//! * **`Window.startSaveLayerImage` runs synchronously**: it encodes and
//!   writes inside the call (the reference runs a worker thread and posts
//!   `WM_APP` messages back). The engine has no public way for a plugin to
//!   post an event to the script thread — the scheduler's posting entry
//!   points are `pub(crate)` — so the file is saved and
//!   `onSaveLayerImageProgress`/`onSaveLayerImageDone` fire inline, at the
//!   same progress points the reference's compressor reports. Cancellation
//!   still works the reference's way: a script that calls
//!   `cancelSaveLayerImage` from inside a progress handler stops the encoder
//!   before the file is written and the done event reports `canceled = 1`;
//!   `stopSaveLayerImage` silences the events and skips the write.
//! * **The save-layer clone** the reference makes for the background thread
//!   (`Main.cpp:229-260`) is a pixel snapshot here — the same stability
//!   guarantee without a second `Layer` object; the events therefore carry
//!   the caller's layer, not a clone.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{
        LayerBitmapView, LayerBitmapViewMut, layer_bitmap_read, layer_bitmap_read_write,
        layer_bitmap_write,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer.saveLayerImagePng/Tlg5/PngOctet and the crop/diff/ooze/blank helpers; Window.startSaveLayerImage",
    notes: "A port of layerExSave (utils.cpp:94-512 pixel helpers, savepng.cpp PNG writer, savetlg5.cpp TLG5 writer with the TLG0.0 tags container, Main.cpp:342-346 Window API). PNG and TLG5 are real decodable files (round-tripped through the engine's own loaders in tests); the deflate behind PNG is this module's fixed-Huffman LZ77 because no zlib is linkable from this crate, and comp_lv selects press/no-press rather than zlib's levels. Window.startSaveLayerImage is synchronous (the engine offers plugins no script-thread event posting), fires the reference's progress/done events inline and keeps the cancel/stop semantics; the reference's background thread and its save-layer clone become a pixel snapshot and the caller's layer object.",
    install: |engine| engine.register_plugin(LayerExSavePlugin),
};

pub struct LayerExSavePlugin;

impl KrkrPlugin for LayerExSavePlugin {
    fn name(&self) -> &str {
        "layerExSave.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `NCB_ATTACH_FUNCTION(<name>, Layer, <fn>)` (`utils.cpp:120-512`,
        // `savepng.cpp:244-274`, `savetlg5.cpp:264-275`), one class-level
        // function per member; the reference's own numparams checks become the
        // declared minimums.
        for (name, arg_count, function) in LAYER_FUNCTIONS {
            register_unless_closure(runtime, layer, name, *arg_count, *function);
        }

        // `NCB_ATTACH_CLASS_WITH_HOOK(WindowSaveImage, Window)`
        // (`Main.cpp:342-346`).
        let Variant::Object(window) = runtime.global_member("Window") else {
            runtime.host_mut().log(
                "layerExSave.dll: this engine has no Window class; the background save API is not installed",
            );
            return Ok(());
        };
        for (name, arg_count, function) in WINDOW_FUNCTIONS {
            register_unless_closure(runtime, window, name, *arg_count, *function);
        }
        Ok(())
    }
}

/// The `Layer` class functions of `utils.cpp`/`savepng.cpp`/`savetlg5.cpp`,
/// with the reference's own argument checks as the declared counts
/// (`numparams < 1` for the diff/writers, `< 4` for `isBlank`, `< 1` for
/// `oozeColor`; `copyBlueToAlpha`/`clearAlpha`/`getCropRect*` take any count
/// and fail on their first missing argument instead).
type LayerFunction = (
    &'static str,
    NativeArgCount,
    fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>,
);

static LAYER_FUNCTIONS: &[LayerFunction] = &[
    ("getCropRect", NativeArgCount::Any, layer_get_crop_rect),
    (
        "getCropRectZero",
        NativeArgCount::Any,
        layer_get_crop_rect_zero,
    ),
    (
        "getDiffRect",
        NativeArgCount::AtLeast(1),
        layer_get_diff_rect,
    ),
    (
        "getDiffPixel",
        NativeArgCount::AtLeast(1),
        layer_get_diff_pixel,
    ),
    ("oozeColor", NativeArgCount::AtLeast(1), layer_ooze_color),
    (
        "copyBlueToAlpha",
        NativeArgCount::Any,
        layer_copy_blue_to_alpha,
    ),
    ("isBlank", NativeArgCount::AtLeast(4), layer_is_blank),
    ("clearAlpha", NativeArgCount::Any, layer_clear_alpha),
    (
        "saveLayerImagePng",
        NativeArgCount::AtLeast(1),
        layer_save_png,
    ),
    (
        "saveLayerImagePngOctet",
        NativeArgCount::Any,
        layer_save_png_octet,
    ),
    (
        "saveLayerImageTlg5",
        NativeArgCount::AtLeast(1),
        layer_save_tlg5,
    ),
];

/// The `Window` methods of `Main.cpp:342-346`.
static WINDOW_FUNCTIONS: &[LayerFunction] = &[
    (
        "startSaveLayerImage",
        NativeArgCount::AtLeast(3),
        window_start_save,
    ),
    (
        "cancelSaveLayerImage",
        NativeArgCount::AtLeast(1),
        window_cancel_save,
    ),
    (
        "stopSaveLayerImage",
        NativeArgCount::AtLeast(1),
        window_stop_save,
    ),
];

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

/// The error the reference's `GetLayerBufferAndSize` failure turns into
/// (`utils.cpp:42-68`): "Invalid layer image.".
fn invalid_layer_image() -> TjsError {
    TjsError::runtime("Invalid layer image.")
}

fn this_layer(this_obj: Option<ObjectHandle>) -> Result<ObjectHandle> {
    this_obj.ok_or_else(invalid_layer_image)
}

/// One layer's plane geometry, as `GetLayerSize` reports it
/// (`utils.cpp:14-40`): width, height and the byte pitch.
///
/// The rectangle is clamped to what the byte slice can actually hold, so the
/// reference's raw pointer arithmetic — which trusts the layer to describe
/// its own buffer — can never index out of bounds here.
#[derive(Clone, Copy)]
struct Geometry {
    pitch: usize,
    width: usize,
    height: usize,
}

impl Geometry {
    fn of(pixels_len: usize, pitch: u32, width: u32, height: u32) -> Self {
        let pitch = pitch as usize;
        let width = (width as usize).min(pitch / 4);
        let height = (height as usize).min(pixels_len.checked_div(pitch).unwrap_or(0));
        Self {
            pitch,
            width,
            height,
        }
    }

    fn read(view: &LayerBitmapView<'_>) -> Self {
        Self::of(
            view.pixels.len(),
            view.bitmap.pitch,
            view.bitmap.width,
            view.bitmap.height,
        )
    }

    fn write(view: &LayerBitmapViewMut<'_>) -> Self {
        Self::of(
            view.pixels.len(),
            view.bitmap.pitch,
            view.bitmap.width,
            view.bitmap.height,
        )
    }

    fn offset(&self, x: usize, y: usize) -> usize {
        y * self.pitch + x * 4
    }

    fn pixel<'a>(&self, pixels: &'a [u8], x: usize, y: usize) -> &'a [u8] {
        let offset = self.offset(x, y);
        &pixels[offset..offset + 4]
    }
}

/// The four bytes of pixel `(x, y)` *as the reference's DWORD* — i.e. what a
/// `*(DWORD*)p` write of an `0xAARRGGBB` colour stores: low byte first, so
/// the engine's R, G, B, A plane receives `[R, G, B, A]`.
fn argb_dword(color: i64) -> [u8; 4] {
    let value = color as u32;
    [
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
        ((value >> 24) & 0xff) as u8,
    ]
}

fn rect_dictionary(runtime: &mut Runtime<KrkrHost>, x: i64, y: i64, w: i64, h: i64) -> Variant {
    let dictionary = runtime.alloc_dictionary_object();
    runtime.set_object_member(dictionary, "x", Variant::Integer(x));
    runtime.set_object_member(dictionary, "y", Variant::Integer(y));
    runtime.set_object_member(dictionary, "w", Variant::Integer(w));
    runtime.set_object_member(dictionary, "h", Variant::Integer(h));
    Variant::Object(dictionary)
}

fn arg_integer(args: &[Variant], index: usize) -> i64 {
    args.get(index)
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Crop, diff and blank helpers (utils.cpp:94-512)
// ---------------------------------------------------------------------------

/// `GetCropRect`/`GetCropRectZero` (`utils.cpp:94-165`): scan the layer image
/// for the first and last pixel the predicate accepts, from the left, right,
/// top and bottom, and answer the enclosed rectangle — or `None` for the
/// reference's `void` answer when nothing qualifies.
///
/// `accepted(x, y)` is `CheckTransp` (`utils.cpp:82-88`, alpha != 0) or
/// `CheckZerop` (`:124-130`, any byte non-zero), or the two-plane `CheckDiff`
/// (`:173-178`) of `getDiffRect`.
fn scan_rect(
    width: usize,
    height: usize,
    accepted: impl Fn(usize, usize) -> bool,
) -> Option<(i64, i64, i64, i64)> {
    if width == 0 || height == 0 {
        return None;
    }
    // `for (p=r; x1 < w; x1++,p+=nc) if (CheckTransp(p, nl, h)) break;`
    let column_has = |x: usize| (0..height).any(|y| accepted(x, y));
    let mut x1 = 0usize;
    while x1 < width && !column_has(x1) {
        x1 += 1;
    }
    if x1 >= width {
        return None; // all transparent -> void
    }
    // `for (p=r+x2*nc; x2 >= 0; x2--,p-=nc) …`, starting at w-1.
    let mut x2 = width - 1;
    while !column_has(x2) {
        x2 -= 1;
    }
    let rw = x2 - x1 + 1;
    // The top/bottom scans cover the columns `[x1, x2]` only.
    let row_has = |y: usize| (x1..=x2).any(|x| accepted(x, y));
    let mut y1 = 0usize;
    while y1 < height && !row_has(y1) {
        y1 += 1;
    }
    let mut y2 = height - 1;
    while !row_has(y2) {
        y2 -= 1;
    }
    Some((x1 as i64, y1 as i64, rw as i64, (y2 - y1 + 1) as i64))
}

/// `IS_SAME_COLOR` (`utils.cpp:168-171`): equal alpha, and either both fully
/// transparent or equal R, G and B.
fn is_same_color(a: &[u8], b: &[u8]) -> bool {
    a[3] == b[3] && (a[3] == 0 || (a[0] == b[0] && a[1] == b[1] && a[2] == b[2]))
}

/// `GetDiffRect` (`utils.cpp:191-225`): the same four-sided scan as
/// [`scan_rect`], with `CheckDiff` (`:173-178`) as the predicate.
fn diff_rect(dest: &[u8], base: &[u8], geometry: Geometry) -> Option<(i64, i64, i64, i64)> {
    scan_rect(geometry.width, geometry.height, |x, y| {
        let offset = geometry.offset(x, y);
        !is_same_color(&dest[offset..offset + 4], &base[offset..offset + 4])
    })
}

/// `GetDiffPixel` (`utils.cpp:239-284`): count differing pixels over the
/// whole image, optionally painting the same and different pixels with the
/// `samecol`/`diffcol` colours (both `0xAARRGGBB`, written as a DWORD).
fn diff_pixel(
    dest: &mut [u8],
    base: &[u8],
    geometry: Geometry,
    same_color: Option<i64>,
    diff_color: Option<i64>,
) -> i64 {
    let mut count = 0i64;
    for y in 0..geometry.height {
        for x in 0..geometry.width {
            let offset = geometry.offset(x, y);
            let same = is_same_color(&dest[offset..offset + 4], &base[offset..offset + 4]);
            if same {
                if let Some(color) = same_color {
                    dest[offset..offset + 4].copy_from_slice(&argb_dword(color));
                }
            } else {
                if let Some(color) = diff_color {
                    dest[offset..offset + 4].copy_from_slice(&argb_dword(color));
                }
                count += 1;
            }
        }
    }
    count
}

/// `OozeColor` (`utils.cpp:302-384`): clear the colour of every pixel whose
/// alpha is below `threshold` to `fillColor`, then dilate the remaining
/// colours into the cleared area for `level` rounds, averaging the
/// already-accepted neighbours per round.
///
/// The reference's `oozed` map is `(w+2) x (h+2)` with a zero border so edge
/// pixels never read outside the image; `-1` marks a pixel the next round may
/// grow from, `1` marks one grown during the current round (it only spreads
/// in the following round) — reproduced here as [`Oozed::Accepted`] and
/// [`Oozed::Fresh`].
fn ooze_color(pixels: &mut [u8], geometry: Geometry, level: i64, threshold: u8, fill: [u8; 3]) {
    let (width, height) = (geometry.width, geometry.height);
    if width == 0 || height == 0 {
        return;
    }
    let map_width = width + 2;
    let mut oozed = vec![Oozed::Unmarked; map_width * (height + 2)];
    for y in 0..height {
        for x in 0..width {
            let index = (y + 1) * map_width + (x + 1);
            let offset = geometry.offset(x, y);
            if pixels[offset + 3] >= threshold {
                oozed[index] = Oozed::Accepted;
            } else {
                pixels[offset] = fill[0];
                pixels[offset + 1] = fill[1];
                pixels[offset + 2] = fill[2];
            }
        }
    }
    for _ in 0..level {
        for y in 0..height {
            for x in 0..width {
                let index = (y + 1) * map_width + (x + 1);
                if oozed[index] != Oozed::Unmarked {
                    continue;
                }
                let up = oozed[index - map_width] == Oozed::Accepted;
                let down = oozed[index + map_width] == Oozed::Accepted;
                let left = oozed[index - 1] == Oozed::Accepted;
                let right = oozed[index + 1] == Oozed::Accepted;
                if !(up || down || left || right) {
                    continue;
                }
                let offset = geometry.offset(x, y);
                let mut sum = [0i32; 3];
                let mut count = 0i32;
                // `AddColor` reads the neighbour's R, G, B bytes; the map
                // guard means only in-image neighbours are read.
                if up {
                    accumulate(pixels, geometry, x, y - 1, &mut sum, &mut count);
                }
                if down {
                    accumulate(pixels, geometry, x, y + 1, &mut sum, &mut count);
                }
                if left {
                    accumulate(pixels, geometry, x - 1, y, &mut sum, &mut count);
                }
                if right {
                    accumulate(pixels, geometry, x + 1, y, &mut sum, &mut count);
                }
                for channel in 0..3 {
                    pixels[offset + channel] = (sum[channel] / count) as u8;
                }
                oozed[index] = Oozed::Fresh;
            }
        }
        for y in 0..height {
            for x in 0..width {
                let index = (y + 1) * map_width + (x + 1);
                if oozed[index] == Oozed::Fresh {
                    oozed[index] = Oozed::Accepted;
                }
            }
        }
    }
}

/// `AddColor` (`utils.cpp:287-289`): add one neighbour's R, G and B bytes to
/// the dilation sum and count it.
fn accumulate(
    pixels: &[u8],
    geometry: Geometry,
    x: usize,
    y: usize,
    sum: &mut [i32; 3],
    count: &mut i32,
) {
    let offset = geometry.offset(x, y);
    for channel in 0..3 {
        sum[channel] += i32::from(pixels[offset + channel]);
    }
    *count += 1;
}

/// The `oozed` map's sentinel values (`utils.cpp:302-384`).
#[derive(Clone, Copy, Eq, PartialEq)]
enum Oozed {
    /// Never written: a candidate for dilation.
    Unmarked,
    /// Grown during the current round: only spreads next round.
    Fresh,
    /// Accepted (alpha >= threshold, or grown in an earlier round).
    Accepted,
}

/// `CopyBlueToAlpha` (`utils.cpp:392-428`): the source's blue byte (engine
/// byte 2, reference byte 0) becomes the destination's alpha byte, over the
/// smaller of the two images.
fn copy_blue_to_alpha(
    source: &[u8],
    dest: &mut [u8],
    source_geometry: Geometry,
    dest_geometry: Geometry,
) {
    let width = source_geometry.width.min(dest_geometry.width);
    let height = source_geometry.height.min(dest_geometry.height);
    for y in 0..height {
        for x in 0..width {
            let source_offset = source_geometry.offset(x, y);
            let dest_offset = dest_geometry.offset(x, y);
            dest[dest_offset + 3] = source[source_offset + 2];
        }
    }
}

/// `isBlank` (`utils.cpp:436-477`): whether every pixel of the rectangle has
/// a zero blue byte (engine byte 2 — the reference's `*buffer`, byte 0).
///
/// The bounds check is the reference's own, including its `top < 0` typo
/// where `height < 0` was intended (`:455-458`): a negative width or height
/// passes the check and makes the loops empty, i.e. "blank".
fn is_blank(
    pixels: &[u8],
    geometry: Geometry,
    left: i64,
    top: i64,
    width: i64,
    height: i64,
) -> Result<bool> {
    let (image_width, image_height) = (geometry.width as i64, geometry.height as i64);
    if left < 0 || top < 0 || left + width > image_width || top + height > image_height {
        return Err(TjsError::runtime("invalid layer range"));
    }
    for y in top..top + height {
        for x in left..left + width {
            let offset = geometry.offset(x as usize, y as usize);
            if pixels[offset + 2] != 0 {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// `clearAlpha` (`utils.cpp:485-510`): every pixel whose alpha is at or below
/// `threshold` becomes `fillColor & 0xffffff` with a zero alpha byte.
fn clear_alpha(pixels: &mut [u8], geometry: Geometry, threshold: i64, fill_color: i64) {
    let fill = argb_dword(fill_color & 0xffffff);
    for y in 0..geometry.height {
        for x in 0..geometry.width {
            let offset = geometry.offset(x, y);
            if i64::from(pixels[offset + 3]) <= threshold {
                pixels[offset..offset + 4].copy_from_slice(&fill);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The Layer functions
// ---------------------------------------------------------------------------

/// `Layer.getCropRect` (`utils.cpp:94-118`): the clip rectangle of the
/// non-transparent content, or `void` when the image is fully transparent.
fn layer_get_crop_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let rect = layer_bitmap_read(runtime, layer, |view| {
        let geometry = Geometry::read(view);
        scan_rect(geometry.width, geometry.height, |x, y| {
            view.pixels[geometry.offset(x, y) + 3] != 0
        })
    })
    .map_err(|_| invalid_layer_image())?;
    Ok(match rect {
        Some((x, y, w, h)) => rect_dictionary(runtime, x, y, w, h),
        None => Variant::Void,
    })
}

/// `Layer.getCropRectZero` (`utils.cpp:138-162`): the clip rectangle of the
/// pixels that are not fully zero, or `void` when all four bytes are zero.
fn layer_get_crop_rect_zero(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let rect = layer_bitmap_read(runtime, layer, |view| {
        let geometry = Geometry::read(view);
        scan_rect(geometry.width, geometry.height, |x, y| {
            let offset = geometry.offset(x, y);
            view.pixels[offset..offset + 4]
                .iter()
                .any(|byte| *byte != 0)
        })
    })
    .map_err(|_| invalid_layer_image())?;
    Ok(match rect {
        Some((x, y, w, h)) => rect_dictionary(runtime, x, y, w, h),
        None => Variant::Void,
    })
}

/// `Layer.getDiffRect(base)` (`utils.cpp:191-225`).
fn layer_get_diff_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = this_layer(this_obj)?;
    let base = args
        .first()
        .and_then(Variant::object_handle)
        .ok_or_else(invalid_layer_image)?;
    let (base_pixels, base_geometry) = layer_bitmap_read(runtime, base, |view| {
        (view.pixels.to_vec(), Geometry::read(view))
    })
    .map_err(|_| invalid_layer_image())?;
    let rect = layer_bitmap_read(runtime, dest, |view| {
        let geometry = Geometry::read(view);
        if (geometry.width, geometry.height) != (base_geometry.width, base_geometry.height) {
            return Err(TjsError::runtime("Different layer size."));
        }
        Ok(diff_rect(view.pixels, &base_pixels, geometry))
    })
    .map_err(|_| invalid_layer_image())??;
    Ok(match rect {
        Some((x, y, w, h)) => rect_dictionary(runtime, x, y, w, h),
        None => Variant::Void,
    })
}

/// `Layer.getDiffPixel(base, samecol, diffcol)` (`utils.cpp:239-284`):
/// returns the number of differing pixels and optionally paints the equal
/// and differing pixels with `samecol`/`diffcol`.
fn layer_get_diff_pixel(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = this_layer(this_obj)?;
    let base = args
        .first()
        .and_then(Variant::object_handle)
        .ok_or_else(invalid_layer_image)?;
    let same_color = match args.get(1) {
        Some(Variant::Void) | None => None,
        Some(value) => Some(value.to_integer()?),
    };
    let diff_color = match args.get(2) {
        Some(Variant::Void) | None => None,
        Some(value) => Some(value.to_integer()?),
    };

    let dest_size = layer_bitmap_read(runtime, dest, |view| {
        (view.bitmap.width, view.bitmap.height)
    })
    .map_err(|_| invalid_layer_image())?;
    let base_size = layer_bitmap_read(runtime, base, |view| {
        (view.bitmap.width, view.bitmap.height)
    })
    .map_err(|_| invalid_layer_image())?;
    if dest_size != base_size {
        return Err(TjsError::runtime("Different layer size."));
    }

    let count = layer_bitmap_read_write(runtime, base, dest, |base_view, dest_view| {
        diff_pixel(
            dest_view.pixels,
            base_view.pixels,
            Geometry::write(dest_view),
            same_color,
            diff_color,
        )
    })?;
    Ok(Variant::Integer(count))
}

/// `Layer.oozeColor(level, threshold=1, fillColor=0)` (`utils.cpp:302-384`).
fn layer_ooze_color(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let level = arg_integer(&args, 0);
    if level <= 0 {
        return Err(TjsError::runtime("Invalid level count."));
    }
    // `(unsigned char)` truncation, then the reference's `threshold < 1`
    // clamp (`utils.cpp:311-317`); a byte cannot exceed 255.
    let threshold = (arg_integer(&args, 1) as u8).max(1);
    let fill_color = arg_integer(&args, 2);
    let fill = [
        ((fill_color >> 16) & 0xff) as u8,
        ((fill_color >> 8) & 0xff) as u8,
        (fill_color & 0xff) as u8,
    ];
    layer_bitmap_write(runtime, layer, |view| {
        let geometry = Geometry::write(view);
        ooze_color(view.pixels, geometry, level, threshold, fill);
    })
    .map_err(|_| invalid_layer_image())?;
    Ok(Variant::Void)
}

/// `Layer.copyBlueToAlpha(src)` (`utils.cpp:392-428`).
fn layer_copy_blue_to_alpha(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = this_layer(this_obj)?;
    let Some(src) = args.first().and_then(Variant::object_handle) else {
        return Err(TjsError::runtime("src must be Layer."));
    };
    layer_bitmap_read(runtime, src, |_| ()).map_err(|_| TjsError::runtime("src must be Layer."))?;
    layer_bitmap_read_write(runtime, src, dest, |source, dest_view| {
        copy_blue_to_alpha(
            source.pixels,
            dest_view.pixels,
            Geometry::read(source),
            Geometry::write(dest_view),
        );
    })
    .map_err(|_| TjsError::runtime("dest must be Layer."))?;
    Ok(Variant::Void)
}

/// `Layer.isBlank(x, y, w, h)` (`utils.cpp:436-477`).
fn layer_is_blank(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let blank = layer_bitmap_read(runtime, layer, |view| {
        is_blank(
            view.pixels,
            Geometry::read(view),
            arg_integer(&args, 0),
            arg_integer(&args, 1),
            arg_integer(&args, 2),
            arg_integer(&args, 3),
        )
    })
    .map_err(|_| TjsError::runtime("src must be Layer."))??;
    Ok(Variant::Integer(i64::from(blank)))
}

/// `Layer.clearAlpha(threthold=0, fillColor=0)` (`utils.cpp:485-510`).
fn layer_clear_alpha(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let threshold = arg_integer(&args, 0);
    let fill_color = arg_integer(&args, 1);
    layer_bitmap_write(runtime, layer, |view| {
        let geometry = Geometry::write(view);
        clear_alpha(view.pixels, geometry, threshold, fill_color);
    })
    .map_err(|_| TjsError::runtime("dest must be Layer."))?;
    Ok(Variant::Void)
}

// ---------------------------------------------------------------------------
// Save targets and tags
// ---------------------------------------------------------------------------

/// A copy of one layer image, the reference's "layer image information"
/// (`compress.hpp:139-156`: width, height, buffer, pitch).
struct SavedPlane {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    pitch: usize,
}

impl SavedPlane {
    /// `GetLayerBufferAndSize` from the write-side view (`utils.cpp:70-80`).
    fn snapshot(view: &LayerBitmapView<'_>) -> Self {
        let geometry = Geometry::read(view);
        Self {
            pixels: view.pixels.to_vec(),
            width: geometry.width,
            height: geometry.height,
            pitch: geometry.pitch,
        }
    }

    fn geometry(&self) -> Geometry {
        Geometry {
            pitch: self.pitch,
            width: self.width,
            height: self.height,
        }
    }
}

/// The tag dictionary the PNG writer reads (`savepng.cpp:158-232`): the
/// `pHYs`/`oFFs`/`vpAg` chunk values plus the compression level.
#[derive(Default)]
struct PngTags {
    reso: Option<(i64, i64, i64)>,
    offs: Option<(i64, i64, i64)>,
    vpag: Option<(i64, i64, i64)>,
    comp_lv: i64,
}

/// A dictionary member, through the normal dispatch path so a getter is
/// honoured; `None` when the member does not exist (`ncbPropAccessor`'s
/// `HasValue` false, `savepng.cpp:170-211`).
fn dict_member(
    runtime: &mut Runtime<KrkrHost>,
    dictionary: ObjectHandle,
    name: &str,
) -> Option<Variant> {
    if !runtime.has_object_member(dictionary, name) {
        return None;
    }
    runtime.resolve_object_member(dictionary, name).ok()
}

/// `ncbPropAccessor::getIntValue` (`savepng.cpp:174-205`): a missing member
/// reads 0, so `reso_x`/`offs_y`/… each stand alone.
fn dict_integer(runtime: &mut Runtime<KrkrHost>, dictionary: ObjectHandle, name: &str) -> i64 {
    dict_member(runtime, dictionary, name)
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(0)
}

/// `writeUnitType` (`savepng.cpp:42-44`): 1 when the unit member equals the
/// expected spelling, else 0.
fn dict_unit(
    runtime: &mut Runtime<KrkrHost>,
    dictionary: ObjectHandle,
    name: &str,
    one_value: &str,
) -> i64 {
    let value = dict_member(runtime, dictionary, name)
        .and_then(|value| value.to_tjs_string().ok())
        .unwrap_or_default();
    i64::from(value == one_value)
}

/// Reads the PNG tag dictionary (`savepng.cpp:158-232`): each chunk appears
/// when either of its coordinates is present, and `comp_lv` defaults to
/// zlib's `Z_DEFAULT_COMPRESSION` (`-1`).
fn read_png_tags(runtime: &mut Runtime<KrkrHost>, dictionary: Option<ObjectHandle>) -> PngTags {
    let mut tags = PngTags {
        comp_lv: -1,
        ..PngTags::default()
    };
    let Some(dictionary) = dictionary else {
        return tags;
    };
    if runtime.has_object_member(dictionary, "reso_x")
        || runtime.has_object_member(dictionary, "reso_y")
    {
        tags.reso = Some((
            dict_integer(runtime, dictionary, "reso_x"),
            dict_integer(runtime, dictionary, "reso_y"),
            dict_unit(runtime, dictionary, "reso_unit", "meter"),
        ));
    }
    if runtime.has_object_member(dictionary, "offs_x")
        || runtime.has_object_member(dictionary, "offs_y")
    {
        tags.offs = Some((
            dict_integer(runtime, dictionary, "offs_x"),
            dict_integer(runtime, dictionary, "offs_y"),
            dict_unit(runtime, dictionary, "offs_unit", "micrometer"),
        ));
    }
    if runtime.has_object_member(dictionary, "vpag_w")
        || runtime.has_object_member(dictionary, "vpag_h")
    {
        tags.vpag = Some((
            dict_integer(runtime, dictionary, "vpag_w"),
            dict_integer(runtime, dictionary, "vpag_h"),
            dict_unit(runtime, dictionary, "vpag_unit", "micrometer"),
        ));
    }
    if runtime.has_object_member(dictionary, "comp_lv") {
        tags.comp_lv = dict_integer(runtime, dictionary, "comp_lv");
    }
    tags
}

/// The `tags` string of the TLG0.0 container (`savetlg5.cpp:206-247`):
/// `EnumMembers` visited in order, each entry written as
/// `<name-length>:<name>=<value-length>:<value>,`.
///
/// The reference's lengths are `GetNarrowStrLen()`; with a Rust string the
/// UTF-8 byte length is the equivalent, and the members come out in the
/// runtime's member order.
fn tlg_tags_string(runtime: &Runtime<KrkrHost>, dictionary: Option<ObjectHandle>) -> String {
    let Some(dictionary) = dictionary else {
        return String::new();
    };
    let mut tags = String::new();
    for (name, value) in runtime.object_members(dictionary) {
        let value = value.to_tjs_string().unwrap_or_default();
        tags.push_str(&format!(
            "{}:{}={}:{},",
            name.len(),
            name,
            value.len(),
            value
        ));
    }
    tags
}

// ---------------------------------------------------------------------------
// PNG writer (savepng.cpp:158-274)
// ---------------------------------------------------------------------------

/// One PNG, as `CompressPNG::compress` builds it (`savepng.cpp:158-232`):
/// signature, `IHDR` with the fixed 8-bit RGBA type, the optional tag
/// chunks, then a single unfiltered `IDAT` and `IEND`.
fn encode_png(
    plane: &SavedPlane,
    tags: &PngTags,
    progress: &mut dyn FnMut(i32) -> bool,
) -> std::result::Result<Encoded, String> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\x0D\x0A\x1A\x0A");

    // `compress_first` (`savepng.cpp:196-208`): `PNGTYPE_RGBA8888` packs bit
    // depth 8, colour type 6, compression 0 and filter 0 into one DWORD, then
    // the interlace byte follows.
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(plane.width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(plane.height as u32).to_be_bytes());
    ihdr.extend_from_slice(&0x0806_0000u32.to_be_bytes());
    ihdr.push(0);
    png_chunk(&mut out, b"IHDR", &ihdr);

    // `compress_second` (`savepng.cpp:209-232`).
    if let Some((x, y, unit)) = tags.reso {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(x as u32).to_be_bytes());
        payload.extend_from_slice(&(y as u32).to_be_bytes());
        payload.push(unit as u8);
        png_chunk(&mut out, b"pHYs", &payload);
    }
    if let Some((x, y, unit)) = tags.offs {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(x as u32).to_be_bytes());
        payload.extend_from_slice(&(y as u32).to_be_bytes());
        payload.push(unit as u8);
        png_chunk(&mut out, b"oFFs", &payload);
    }
    if let Some((w, h, unit)) = tags.vpag {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(w as u32).to_be_bytes());
        payload.extend_from_slice(&(h as u32).to_be_bytes());
        payload.push(unit as u8);
        png_chunk(&mut out, b"vpAg", &payload);
    }

    // `compress_third` (`savepng.cpp:233-255`): one filter byte 0 per row,
    // then R, G, B, A per pixel.
    let mut raw = Vec::with_capacity(plane.height * (1 + plane.width * 4));
    for y in 0..plane.height {
        raw.push(0);
        for x in 0..plane.width {
            raw.extend_from_slice(plane.geometry().pixel(&plane.pixels, x, y));
        }
    }
    let compressed = match deflate(&raw, tags.comp_lv, progress)? {
        Encoded::Bytes(bytes) => bytes,
        Encoded::Canceled => return Ok(Encoded::Canceled),
    };
    let mut zlib = Vec::with_capacity(compressed.len() + 6);
    zlib.push(0x78);
    zlib.push(0x01);
    zlib.extend_from_slice(&compressed);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    png_chunk(&mut out, b"IDAT", &zlib);
    png_chunk(&mut out, b"IEND", &[]);
    Ok(Encoded::Bytes(out))
}

/// One PNG chunk: length, type, payload, CRC over type+payload.
fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(payload);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// `crc32()` from zlib (`savepng.cpp:36`), the PNG chunk checksum.
struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Self(0xffff_ffff)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            let index = ((self.0 ^ u32::from(*byte)) & 0xff) as usize;
            self.0 = (self.0 >> 8) ^ CRC_TABLE[index];
        }
    }

    fn finish(self) -> u32 {
        self.0 ^ 0xffff_ffff
    }
}

/// The reflected IEEE polynomial 0xEDB88320 table.
const CRC_TABLE: [u32; 256] = build_crc_table();

const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                (value >> 1) ^ 0xedb8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

/// `adler32` over the uncompressed data, the zlib stream trailer.
fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// deflate (this module's stand-in for zlib, see the module docs)
// ---------------------------------------------------------------------------

/// What an encoder produced: the bytes, or the request to stop without
/// writing anything (`compress.hpp:139-156`: `compress` returning canceled).
enum Encoded {
    Bytes(Vec<u8>),
    Canceled,
}

/// zlib's `deflate()` at the level the reference asked for
/// (`savepng.cpp:57-99`): level 0 is stored blocks, levels 1-9 are one
/// fixed-Huffman block over a greedy LZ77 match search. Level -1 is zlib's
/// `Z_DEFAULT_COMPRESSION`; anything outside -1..=9 is the reference's
/// "deflate initialize" error.
fn deflate(
    data: &[u8],
    level: i64,
    progress: &mut dyn FnMut(i32) -> bool,
) -> std::result::Result<Encoded, String> {
    if !(-1..=9).contains(&level) {
        return Err("deflate initialize".to_string());
    }
    if level <= 0 {
        return Ok(deflate_stored(data, progress));
    }
    let depth = 4 + (level as usize) * 4;

    let mut writer = BitWriter::new();
    writer.write_bits(1, 1); // final block
    writer.write_bits(1, 2); // fixed Huffman codes
    let mut head = vec![u32::MAX; HASH_SIZE];
    let mut prev = vec![u32::MAX; data.len()];
    let mut index = 0usize;
    let mut reported = 0usize;
    while index < data.len() {
        let (length, distance) = if index + MIN_MATCH <= data.len() {
            find_match(data, index, &head, &prev, depth)
        } else {
            (0, 0)
        };
        if length >= MIN_MATCH {
            write_length(&mut writer, length);
            write_distance(&mut writer, distance);
            for position in index..index + length {
                insert_match(data, position, &mut head, &mut prev);
            }
            index += length;
        } else {
            let (code, bits) = fixed_code(u32::from(data[index]));
            writer.write_code(code, bits);
            insert_match(data, index, &mut head, &mut prev);
            index += 1;
        }
        if index.saturating_sub(reported) >= 4096 {
            reported = index;
            let percent = (index * 100 / data.len().max(1)) as i32;
            if progress(percent) {
                return Ok(Encoded::Canceled);
            }
        }
    }
    let (code, bits) = fixed_code(256);
    writer.write_code(code, bits);
    if progress(100) {
        return Ok(Encoded::Canceled);
    }
    Ok(Encoded::Bytes(writer.finish()))
}

/// zlib's level 0: uncompressed deflate blocks (`savepng.cpp:57-99` still goes
/// through `deflate` for them).
fn deflate_stored(data: &[u8], progress: &mut dyn FnMut(i32) -> bool) -> Encoded {
    let mut writer = BitWriter::new();
    let mut offset = 0usize;
    loop {
        let chunk = (data.len() - offset).min(65535);
        let last = offset + chunk >= data.len();
        writer.write_bits(u32::from(last), 1);
        writer.write_bits(0, 2); // stored
        writer.align();
        let length = chunk as u16;
        writer.push_bytes(&length.to_le_bytes());
        writer.push_bytes(&(!length).to_le_bytes());
        writer.push_bytes(&data[offset..offset + chunk]);
        offset += chunk;
        let percent = (offset * 100 / data.len().max(1)) as i32;
        if progress(percent) {
            return Encoded::Canceled;
        }
        if last {
            return Encoded::Bytes(writer.finish());
        }
    }
}

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const WINDOW: usize = 32 * 1024;
const HASH_BITS: u32 = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;

fn hash3(data: &[u8], index: usize) -> usize {
    let value = (u32::from(data[index]) << 16)
        | (u32::from(data[index + 1]) << 8)
        | u32::from(data[index + 2]);
    (value.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
}

/// Inserts `index` into the hash chain, once its 3-byte prefix is known.
fn insert_match(data: &[u8], index: usize, head: &mut [u32], prev: &mut [u32]) {
    if index + MIN_MATCH > data.len() {
        return;
    }
    let hash = hash3(data, index);
    prev[index] = head[hash];
    head[hash] = index as u32;
}

/// The longest match among the `depth` most recent positions with the same
/// 3-byte prefix, at most one window back.
fn find_match(
    data: &[u8],
    index: usize,
    head: &[u32],
    prev: &[u32],
    depth: usize,
) -> (usize, usize) {
    let max_length = MAX_MATCH.min(data.len() - index);
    if max_length < MIN_MATCH {
        return (0, 0);
    }
    let mut best_length = 0usize;
    let mut best_distance = 0usize;
    let mut candidate = head[hash3(data, index)];
    let mut steps = 0usize;
    while candidate != u32::MAX && steps < depth {
        steps += 1;
        let position = candidate as usize;
        let distance = index - position;
        if distance == 0 || distance > WINDOW {
            break;
        }
        let mut length = 0usize;
        while length < max_length && data[position + length] == data[index + length] {
            length += 1;
        }
        if length > best_length {
            best_length = length;
            best_distance = distance;
            if length == max_length {
                break;
            }
        }
        candidate = prev[position];
    }
    if best_length >= MIN_MATCH {
        (best_length, best_distance)
    } else {
        (0, 0)
    }
}

fn fixed_code(symbol: u32) -> (u32, u32) {
    match symbol {
        0..=143 => (0x30 + symbol, 8),
        144..=255 => (0x190 + (symbol - 144), 9),
        256..=279 => (symbol - 256, 7),
        280..=287 => (0xc0 + (symbol - 280), 8),
        _ => unreachable!("fixed Huffman has 288 symbols"),
    }
}

const LENGTH_BASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Emits a length symbol plus its extra bits (RFC 1951 §3.2.5).
fn write_length(writer: &mut BitWriter, length: usize) {
    let index = LENGTH_BASE
        .iter()
        .rposition(|base| (*base as usize) <= length)
        .expect("length >= 3");
    let symbol = 257 + index as u32;
    let (code, bits) = fixed_code(symbol);
    writer.write_code(code, bits);
    writer.write_bits(length as u32 - LENGTH_BASE[index], LENGTH_EXTRA[index]);
}

/// Emits a distance symbol plus its extra bits; the fixed code is the symbol
/// number in five bits (RFC 1951 §3.2.6).
fn write_distance(writer: &mut BitWriter, distance: usize) {
    let index = DISTANCE_BASE
        .iter()
        .rposition(|base| (*base as usize) <= distance)
        .expect("distance >= 1");
    writer.write_code(index as u32, 5);
    writer.write_bits(
        distance as u32 - DISTANCE_BASE[index],
        DISTANCE_EXTRA[index],
    );
}

/// Bits go into the stream least-significant first; Huffman codes are given
/// most-significant bit first (RFC 1951 §3.1.1).
struct BitWriter {
    out: Vec<u8>,
    bits: u32,
    count: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            bits: 0,
            count: 0,
        }
    }

    fn write_bits(&mut self, value: u32, count: u32) {
        self.bits |= value << self.count;
        self.count += count;
        while self.count >= 8 {
            self.out.push((self.bits & 0xff) as u8);
            self.bits >>= 8;
            self.count -= 8;
        }
    }

    fn write_code(&mut self, code: u32, bits: u32) {
        let mut reversed = 0u32;
        for index in 0..bits {
            reversed |= ((code >> index) & 1) << (bits - 1 - index);
        }
        self.write_bits(reversed, bits);
    }

    fn align(&mut self) {
        if self.count > 0 {
            let padding = 8 - self.count;
            self.write_bits(0, padding);
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.out.push((self.bits & 0xff) as u8);
        }
        self.out
    }
}

// ---------------------------------------------------------------------------
// TLG5 writer (savetlg5.cpp:20-171)
// ---------------------------------------------------------------------------

/// `BLOCK_HEIGHT` (`savetlg5.cpp:6`): four rows per block.
const TLG_BLOCK_HEIGHT: usize = 4;

/// The plane bytes each TLG5 channel carries, in the format's own order:
/// channel 0 is B, 1 is G, 2 is R, 3 is A (`savetlg5.cpp:96-121`, and
/// `tlg5_compose_colors4` on the decoding side), while the engine plane is
/// R, G, B, A.
const TLG_CHANNEL_BYTE: [usize; 4] = [2, 1, 0, 3];

/// `CompressTLG5::compress` (`savetlg5.cpp:174-262`): the raw stream, wrapped
/// in a `TLG0.0` container when the tags string is non-empty.
fn compress_tlg5(plane: &SavedPlane, tags: &str, progress: &mut dyn FnMut(i32) -> bool) -> Encoded {
    if tags.is_empty() {
        return tlg5_stream(plane, progress);
    }
    let raw = match tlg5_stream(plane, progress) {
        Encoded::Bytes(bytes) => bytes,
        Encoded::Canceled => return Encoded::Canceled,
    };
    let mut out = Vec::with_capacity(raw.len() + tags.len() + 20);
    out.extend_from_slice(b"TLG0.0\x00sds\x1a");
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out.extend_from_slice(&raw);
    out.extend_from_slice(b"tags");
    out.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    out.extend_from_slice(tags.as_bytes());
    Encoded::Bytes(out)
}

/// `CompressTLG5::main` (`savetlg5.cpp:20-171`): the `TLG5.0` header, the
/// back-patched block-size table, then one 4-row block at a time — per
/// channel the row/inter-pixel deltas, the B/G/R/A channel composition and
/// either the LZSS payload or the raw bytes, whichever is smaller.
fn tlg5_stream(plane: &SavedPlane, progress: &mut dyn FnMut(i32) -> bool) -> Encoded {
    let (width, height) = (plane.width, plane.height);
    let geometry = plane.geometry();
    let block_height = TLG_BLOCK_HEIGHT;
    // `((height - 1) / block_height) + 1` truncates toward zero like the
    // reference; a zero-height image still claims one block.
    let block_count = if height == 0 {
        1
    } else {
        (height - 1) / block_height + 1
    };
    let mut out = Vec::new();
    out.extend_from_slice(b"TLG5.0\x00raw\x1a");
    out.push(4); // colors, always ARGB (`savetlg5.cpp:25-26`)
    out.extend_from_slice(&(width as u32).to_le_bytes());
    out.extend_from_slice(&(height as u32).to_le_bytes());
    out.extend_from_slice(&(block_height as u32).to_le_bytes());
    let table_position = out.len();
    out.resize(table_position + block_count * 4, 0);

    let mut encoder = SlideEncoder::new();
    let mut block_sizes = vec![0u32; block_count];
    for (block, block_size) in block_sizes.iter_mut().enumerate() {
        let first_row = block * block_height;
        let last_row = (first_row + block_height).min(height);
        let rows = last_row - first_row;
        if progress((first_row * 100 / height.max(1)) as i32) {
            // The reference still reports its final step before returning the
            // canceled flag (`savetlg5.cpp:160-168`); the PNG path does not
            // (`savepng.cpp:94-99`).
            progress(100);
            return Encoded::Canceled;
        }
        if rows == 0 {
            continue;
        }
        let mut channels = [
            vec![0u8; width * rows],
            vec![0u8; width * rows],
            vec![0u8; width * rows],
            vec![0u8; width * rows],
        ];
        let mut count = 0usize;
        for y in first_row..last_row {
            // `prevcl` is reset per scan line; `upper` is the row above, or
            // zero for the image's first row (`savetlg5.cpp:63-88`).
            let mut previous_channel = [0i32; 4];
            let previous_offset = (y > 0).then(|| geometry.offset(0, y - 1));
            for x in 0..width {
                let offset = geometry.offset(x, y);
                let mut values = [0i32; 4];
                for channel in 0..4 {
                    let byte = TLG_CHANNEL_BYTE[channel];
                    let current = i32::from(plane.pixels[offset + byte]);
                    let upper = previous_offset
                        .map(|previous| i32::from(plane.pixels[previous + x * 4 + byte]))
                        .unwrap_or(0);
                    let cl = current - upper;
                    values[channel] = cl - previous_channel[channel];
                    previous_channel[channel] = cl;
                }
                channels[0][count] = (values[0] - values[1]) as u8;
                channels[1][count] = values[1] as u8;
                channels[2][count] = (values[2] - values[1]) as u8;
                channels[3][count] = values[3] as u8;
                count += 1;
            }
        }

        let mut written = 0u32;
        for channel in &mut channels {
            let state = encoder.snapshot();
            let mut compressed = Vec::new();
            encoder.encode(&channel[..count], &mut compressed);
            if compressed.len() < count {
                out.push(0x00);
                out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
                out.extend_from_slice(&compressed);
                written += 1 + 4 + compressed.len() as u32;
            } else {
                encoder.restore(state);
                out.push(0x01);
                out.extend_from_slice(&(count as u32).to_le_bytes());
                out.extend_from_slice(&channel[..count]);
                written += 1 + 4 + count as u32;
            }
        }
        *block_size = written;
    }
    for (block, size) in block_sizes.iter().enumerate() {
        let position = table_position + block * 4;
        out[position..position + 4].copy_from_slice(&size.to_le_bytes());
    }
    progress(100);
    Encoded::Bytes(out)
}

/// The reference's `SlideCompressor` (`slide.h`, `slide.cpp:100-213`): a
/// 4096-byte ring dictionary whose tokens store the *ring slot* of the match,
/// not a distance, because the decoder's write position tracks the encoder's
/// byte for byte (`tvpgl` `tlg5_decompress_slide`).
///
/// Differences from the reference, all on the search side only — the token
/// stream stays decodable:
///
/// * The reference walks a doubly linked chain of every position with the
///   same two-byte prefix and deletes entries as the ring overwrites them;
///   this port bounds the walk at [`SLIDE_SEARCH_DEPTH`] candidates and lets
///   stale links dangle. A candidate is accepted only after its bytes are
///   compared against the actual history, so a stale link costs search time,
///   never correctness.
/// * `Store`/`Restore` copy the ring, position and fill count — the format
///   state a raw channel does not advance (the decoder ignores raw channels
///   for the ring too) — but not the search tables, which are a heuristic.
/// * The reference seeds the map with all 4096 slots of the zero ring, so it
///   can match the initial zeros; this port inserts only written slots.
struct SlideEncoder {
    ring: [u8; SLIDE_N],
    position: usize,
    filled: usize,
    head: Vec<u32>,
    prev: [u32; SLIDE_N],
    history: Vec<u8>,
}

/// The reference's `SLIDE_N`/`SLIDE_M` (`slide.h:3-4`): a 4096-byte ring and
/// a 273-byte longest match.
const SLIDE_N: usize = 4096;
const SLIDE_MASK: usize = SLIDE_N - 1;
const SLIDE_MAX_MATCH: usize = 18 + 255;
const SLIDE_NONE: u32 = u32::MAX;
const SLIDE_SEARCH_DEPTH: usize = 32;

struct SlideState {
    position: usize,
    filled: usize,
    ring: [u8; SLIDE_N],
}

impl SlideEncoder {
    fn new() -> Self {
        Self {
            ring: [0; SLIDE_N],
            position: 0,
            filled: 0,
            head: vec![SLIDE_NONE; 256 * 256],
            prev: [SLIDE_NONE; SLIDE_N],
            history: Vec::new(),
        }
    }

    /// `SlideCompressor::Store` (`slide.cpp:177-190`).
    fn snapshot(&self) -> SlideState {
        SlideState {
            position: self.position,
            filled: self.filled,
            ring: self.ring,
        }
    }

    /// `SlideCompressor::Restore` (`slide.cpp:192-205`), minus the search
    /// tables (see the struct docs).
    fn restore(&mut self, state: SlideState) {
        self.position = state.position;
        self.filled = state.filled;
        self.ring = state.ring;
    }

    /// `SlideCompressor::Encode` (`slide.cpp:100-175`): token framing exactly
    /// as the reference emits it — a control byte whose bits, least
    /// significant first, say literal (0) or match (1) for the next eight
    /// tokens.
    fn encode(&mut self, input: &[u8], out: &mut Vec<u8>) {
        if input.is_empty() {
            return;
        }
        self.history.clear();
        let count = self.filled.min(SLIDE_N - 1);
        for index in 0..count {
            let slot = (self.position + SLIDE_N - count + index) & SLIDE_MASK;
            self.history.push(self.ring[slot]);
        }
        let mut code = [0u8; 40];
        let mut code_pointer = 1usize;
        let mut mask = 1u8;
        code[0] = 0;
        let mut index = 0usize;
        while index < input.len() {
            let (length, slot) = self.find_match(input, index);
            if length >= 3 {
                code[0] |= mask;
                if length >= 18 {
                    code[code_pointer] = (slot & 0xff) as u8;
                    code[code_pointer + 1] = (((slot & 0xf00) >> 8) as u8) | 0xf0;
                    code[code_pointer + 2] = (length - 18) as u8;
                    code_pointer += 3;
                } else {
                    code[code_pointer] = (slot & 0xff) as u8;
                    code[code_pointer + 1] =
                        (((slot & 0xf00) >> 8) as u8) | (((length - 3) as u8) << 4);
                    code_pointer += 2;
                }
                for _ in 0..length {
                    self.write_byte(input[index]);
                    index += 1;
                }
            } else {
                let byte = input[index];
                self.write_byte(byte);
                index += 1;
                code[code_pointer] = byte;
                code_pointer += 1;
            }
            mask <<= 1;
            if mask == 0 {
                out.extend_from_slice(&code[..code_pointer]);
                mask = 1;
                code_pointer = 1;
                code[0] = 0;
            }
        }
        if mask != 1 {
            out.extend_from_slice(&code[..code_pointer]);
        }
    }

    /// The best candidate among the slots with the same two-byte prefix,
    /// longest match wins; `(0, 0)` when nothing reaches three bytes.
    fn find_match(&self, input: &[u8], index: usize) -> (usize, usize) {
        let max_length = SLIDE_MAX_MATCH.min(input.len() - index);
        if max_length < 3 || index + 2 > input.len() {
            return (0, 0);
        }
        let base = self.history.len() as isize;
        let hash = usize::from(input[index]) | (usize::from(input[index + 1]) << 8);
        let mut best_length = 0usize;
        let mut best_slot = 0usize;
        let mut candidate = self.head[hash];
        let mut steps = 0usize;
        while candidate != SLIDE_NONE && steps < SLIDE_SEARCH_DEPTH {
            steps += 1;
            let slot = candidate as usize;
            let distance = (self.position + SLIDE_N - slot) & SLIDE_MASK;
            // A candidate is only usable when its source bytes are in the
            // stream the decoder has already produced: at most everything the
            // history holds plus what this input has produced so far.
            if distance != 0 && distance as isize <= base + index as isize {
                let mut length = 0usize;
                while length < max_length {
                    let current = base + (index + length) as isize;
                    if self.source(base, input, current)
                        == self.source(base, input, current - distance as isize)
                    {
                        length += 1;
                    } else {
                        break;
                    }
                }
                if length > best_length {
                    best_length = length;
                    best_slot = slot;
                    if length == max_length {
                        break;
                    }
                }
            }
            candidate = self.prev[slot];
        }
        if best_length >= 3 {
            (best_length, best_slot)
        } else {
            (0, 0)
        }
    }

    /// One byte of the conceptual stream `history ++ input`: what the decoder
    /// reads for a match source. A negative index would be a ring slot that
    /// was never written, i.e. the decoder's initial zero byte; candidates
    /// are validated against `base + index`, so it cannot happen, and the
    /// guard keeps the indexing sound either way.
    fn source(&self, base: isize, input: &[u8], position: isize) -> u8 {
        if position < base {
            self.history[position.max(0) as usize]
        } else {
            input[(position - base) as usize]
        }
    }

    /// One ring write and the two prefix-table updates it disturbs
    /// (`slide.cpp:126-152`: `DeleteMap`/`AddMap` around the store).
    fn write_byte(&mut self, byte: u8) {
        let slot = self.position;
        self.ring[slot] = byte;
        self.position = (slot + 1) & SLIDE_MASK;
        self.filled = (self.filled + 1).min(SLIDE_N);
        self.insert((slot + SLIDE_MASK) & SLIDE_MASK);
        self.insert(slot);
    }

    fn insert(&mut self, slot: usize) {
        let hash =
            usize::from(self.ring[slot]) | (usize::from(self.ring[(slot + 1) & SLIDE_MASK]) << 8);
        let previous = self.head[hash];
        self.head[hash] = slot as u32;
        self.prev[slot] = previous;
    }
}

// ---------------------------------------------------------------------------
// The Layer save functions (savepng.cpp:244-274, savetlg5.cpp:264-275)
// ---------------------------------------------------------------------------

fn snapshot_layer(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Result<SavedPlane> {
    layer_bitmap_read(runtime, layer, SavedPlane::snapshot).map_err(|_| invalid_layer_image())
}

/// `Layer.saveLayerImagePng(filename, tags=void)` (`savepng.cpp:244-255`).
fn layer_save_png(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let filename = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let plane = snapshot_layer(runtime, layer)
        .map_err(|_| TjsError::runtime(format!("{filename}:invalid layer")))?;
    let tags = read_png_tags(runtime, object_argument(&args, 1));
    let encoded = encode_png(&plane, &tags, &mut |_| false).map_err(TjsError::runtime)?;
    write_saved(runtime, &filename, encoded)
}

/// `Layer.saveLayerImagePngOctet(compression_level=1)` (`savepng.cpp:263-274`):
/// the same stream as an octet, and an empty string when the layer has no
/// image (`encodeToOctet` clears the result first).
fn layer_save_png_octet(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let level = match args.first() {
        Some(Variant::Void) | None => 1,
        Some(value) => value.to_integer().unwrap_or(0),
    };
    let Ok(plane) = snapshot_layer(runtime, layer) else {
        return Ok(Variant::String(String::new()));
    };
    let tags = PngTags {
        comp_lv: level,
        ..PngTags::default()
    };
    let encoded = encode_png(&plane, &tags, &mut |_| false).map_err(TjsError::runtime)?;
    Ok(match encoded {
        Encoded::Bytes(bytes) => Variant::Octet(bytes),
        Encoded::Canceled => Variant::String(String::new()),
    })
}

/// `Layer.saveLayerImageTlg5(filename, tags=void)` (`savetlg5.cpp:264-275`).
fn layer_save_tlg5(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let filename = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let plane = snapshot_layer(runtime, layer)
        .map_err(|_| TjsError::runtime(format!("{filename}:invalid layer")))?;
    let tags = tlg_tags_string(runtime, object_argument(&args, 1));
    let encoded = compress_tlg5(&plane, &tags, &mut |_| false);
    write_saved(runtime, &filename, encoded)
}

/// `CompressBase::save`'s storage step (`compress.hpp:157-196`): a canceled
/// compression writes nothing; a storage the project cannot open throws
/// `<filename>:can't open`.
fn write_saved(
    runtime: &mut Runtime<KrkrHost>,
    filename: &str,
    encoded: Encoded,
) -> Result<Variant> {
    let Encoded::Bytes(bytes) = encoded else {
        return Ok(Variant::Void);
    };
    runtime
        .host_mut()
        .write_binary_storage(filename, "w", &bytes)
        .map_err(|_| TjsError::runtime(format!("{filename}:can't open")))?;
    Ok(Variant::Void)
}

/// The tag dictionary argument, when it is an object (`param[1]->AsObjectNoAddRef()`).
fn object_argument(args: &[Variant], index: usize) -> Option<ObjectHandle> {
    match args.get(index) {
        Some(Variant::Object(handle)) => Some(*handle),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Window.startSaveLayerImage (Main.cpp:143-346)
// ---------------------------------------------------------------------------

/// One in-flight (here: in-call) save's shared flags. The reference's
/// `SaveInfo` keeps `canceled` for the compressor and a `notify` pointer the
/// `stop()` call clears (`Main.cpp:33-113`); this is both.
#[derive(Default)]
struct SaveJobState {
    /// `cancelSaveLayerImage`: stop at the next progress point, report done.
    canceled: AtomicBool,
    /// `stopSaveLayerImage`: no events at all, and no file.
    stopped: AtomicBool,
}

thread_local! {
    /// Save jobs by window object, with the reference's handler numbering:
    /// the lowest free slot, or the end of the list (`Main.cpp:211-221`).
    static SAVE_JOBS: RefCell<BTreeMap<ObjectHandle, Vec<Option<Arc<SaveJobState>>>>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// `WindowSaveImage::startSaveLayerImage` (`Main.cpp:204-266`).
///
/// Synchronous: the reference clones the layer onto a worker thread and
/// posts its events through the window message queue, which this engine
/// gives plugins no way to do (see the module docs). The visible contract —
/// handler numbering, the file, the two events, `cancel`/`stop` — is kept.
fn window_start_save(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let window = this_obj
        .map(|window| runtime.bound_this(window).unwrap_or(window))
        .ok_or_else(|| TjsError::runtime("Window method requires this"))?;
    let layer = args
        .first()
        .and_then(Variant::object_handle)
        .ok_or_else(invalid_layer_image)?;
    let filename = args.get(1).cloned().unwrap_or_default().to_tjs_string()?;
    let tags = object_argument(&args, 2);
    let plane = snapshot_layer(runtime, layer)
        .map_err(|_| TjsError::runtime(format!("{filename}:invalid layer")))?;

    // Format by extension (`Main.cpp:298-306`): `.png` exactly, everything
    // else TLG5.
    let is_png = storage_extension(&filename) == ".png";
    let png_tags = if is_png {
        read_png_tags(runtime, tags)
    } else {
        PngTags::default()
    };
    let tlg_tags = if is_png {
        String::new()
    } else {
        tlg_tags_string(runtime, tags)
    };

    let job = Arc::new(SaveJobState::default());
    let handler = register_save_job(window, Arc::clone(&job));
    let layer_value = Variant::Object(layer);
    let filename_value = Variant::String(filename.clone());
    let mut hook_error: Option<TjsError> = None;
    let encoded = {
        let mut progress = |percent: i32| -> bool {
            let args = vec![
                Variant::Integer(handler),
                Variant::Integer(i64::from(percent)),
                layer_value.clone(),
                filename_value.clone(),
            ];
            if let Err(error) = fire_window_hook(runtime, window, "onSaveLayerImageProgress", args)
            {
                hook_error = Some(error);
                return true;
            }
            job.canceled.load(Ordering::Relaxed) || job.stopped.load(Ordering::Relaxed)
        };
        if is_png {
            encode_png(&plane, &png_tags, &mut progress).map_err(TjsError::runtime)
        } else {
            Ok(compress_tlg5(&plane, &tlg_tags, &mut progress))
        }
    };

    let canceled = job.canceled.load(Ordering::Relaxed);
    let stopped = job.stopped.load(Ordering::Relaxed);
    let outcome = match encoded {
        Err(error) => Err(error),
        Ok(Encoded::Canceled) => Ok(()),
        Ok(Encoded::Bytes(_)) if canceled || stopped => Ok(()),
        Ok(Encoded::Bytes(bytes)) => runtime
            .host_mut()
            .write_binary_storage(&filename, "w", &bytes)
            .map_err(|_| TjsError::runtime(format!("{filename}:can't open"))),
    };

    // The reference clears the handler slot before firing done (so a handler
    // that reacts to the event cannot cancel a finished job).
    clear_save_job(window, handler);
    if let Some(error) = hook_error {
        return Err(error);
    }
    outcome?;
    if !stopped {
        let args = vec![
            Variant::Integer(handler),
            Variant::Integer(i64::from(canceled)),
            layer_value,
            filename_value,
        ];
        fire_window_hook(runtime, window, "onSaveLayerImageDone", args)?;
    }
    Ok(Variant::Integer(handler))
}

/// `WindowSaveImage::cancelSaveLayerImage` (`Main.cpp:270-275`).
fn window_cancel_save(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let window = this_obj
        .map(|window| runtime.bound_this(window).unwrap_or(window))
        .ok_or_else(|| TjsError::runtime("Window method requires this"))?;
    let handler = arg_integer(&args, 0);
    if let Some(job) = save_job(window, handler) {
        job.canceled.store(true, Ordering::Relaxed);
    }
    Ok(Variant::Void)
}

/// `WindowSaveImage::stopSaveLayerImage` (`Main.cpp:280-285`).
fn window_stop_save(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let window = this_obj
        .map(|window| runtime.bound_this(window).unwrap_or(window))
        .ok_or_else(|| TjsError::runtime("Window method requires this"))?;
    let handler = arg_integer(&args, 0);
    if let Some(job) = save_job(window, handler) {
        job.stopped.store(true, Ordering::Relaxed);
        clear_save_job(window, handler);
    }
    Ok(Variant::Void)
}

/// `TVPExtractStorageExt` + `ToLowerCase` (`Main.cpp:301-303`): the
/// extension of the file name part, after the last `/`, `\\` or `>`.
fn storage_extension(name: &str) -> String {
    let start = name
        .rfind(['/', '\\', '>'])
        .map(|index| index + 1)
        .unwrap_or(0);
    let Some(index) = name[start..].rfind('.') else {
        return String::new();
    };
    name[start + index..].to_ascii_lowercase()
}

/// The lowest free handler slot, or a new one (`Main.cpp:211-221`).
fn register_save_job(window: ObjectHandle, state: Arc<SaveJobState>) -> i64 {
    SAVE_JOBS.with(|jobs| {
        let mut jobs = jobs.borrow_mut();
        let slots = jobs.entry(window).or_default();
        match slots.iter().position(Option::is_none) {
            Some(handler) => {
                slots[handler] = Some(state);
                handler as i64
            }
            None => {
                slots.push(Some(state));
                (slots.len() - 1) as i64
            }
        }
    })
}

fn save_job(window: ObjectHandle, handler: i64) -> Option<Arc<SaveJobState>> {
    if handler < 0 {
        return None;
    }
    SAVE_JOBS.with(|jobs| jobs.borrow().get(&window)?.get(handler as usize)?.clone())
}

fn clear_save_job(window: ObjectHandle, handler: i64) {
    if handler < 0 {
        return;
    }
    SAVE_JOBS.with(|jobs| {
        let mut jobs = jobs.borrow_mut();
        if let Some(slots) = jobs.get_mut(&window)
            && let Some(slot) = slots.get_mut(handler as usize)
        {
            *slot = None;
        }
    });
}

/// Fires one of the window's `onSaveLayerImage*` events the way the
/// reference's `FuncCall` does (`Main.cpp:60-76`): a member that is absent or
/// not callable is ignored, an exception inside the handler propagates.
fn fire_window_hook(
    runtime: &mut Runtime<KrkrHost>,
    window: ObjectHandle,
    name: &str,
    args: Vec<Variant>,
) -> Result<()> {
    let member = runtime.resolve_object_member(window, name)?;
    let callable = match &member {
        Variant::Closure(_) => true,
        Variant::Object(handle) => runtime.object_is_callable(*handle),
        _ => false,
    };
    if !callable {
        return Ok(());
    }
    runtime.call_object_method(window, name, args)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::{ObjectHandle, Variant};

    use super::LayerExSavePlugin;

    /// An engine whose project storage is in memory, so a save is one host
    /// call away from the bytes it wrote.
    fn engine() -> KrkrEngine {
        let storage = krkr_assets::ProjectStorage::from_memory(Vec::<(String, Vec<u8>)>::new());
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(LayerExSavePlugin).expect("plugin");
        engine
    }

    /// The rectangle utilities the scripts below use.
    const HELPERS: &str = r#"
        global.rectString = function(r) {
            return r === void ? "void" : r.x + "," + r.y + "," + r.w + "," + r.h;
        };
    "#;

    fn run(engine: &mut KrkrEngine, name: &str, script: &str) {
        if !engine.tjs_runtime().global_member("rectString").is_truthy() {
            engine
                .execute_script("helpers.tjs", HELPERS)
                .expect("helpers");
        }
        engine.execute_script(name, script).expect(name);
    }

    fn saved(engine: &KrkrEngine, name: &str) -> Vec<u8> {
        engine
            .host()
            .read_binary_storage(name)
            .unwrap_or_else(|error| panic!("read {name}: {error:?}"))
    }

    fn rect(engine: &mut KrkrEngine, expression: &str) -> String {
        engine
            .execute_expression("rect.tjs", expression)
            .expect("rectangle")
            .to_tjs_string()
            .expect("string")
    }

    fn pixel(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("pixel.tjs", &format!("{layer}.getMainPixel({x}, {y})"))
            .expect("main pixel")
            .to_integer()
            .expect("integer")
    }

    fn mask(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("mask.tjs", &format!("{layer}.getMaskPixel({x}, {y})"))
            .expect("mask pixel")
            .to_integer()
            .expect("integer")
    }

    fn class_object(engine: &KrkrEngine, name: &str) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} class"))
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

    /// Every member of both reference surfaces exists
    /// (`utils.cpp:120-512`, `savepng.cpp:244-274`, `savetlg5.cpp:264-275`,
    /// `Main.cpp:342-346`).
    #[test]
    fn the_surface_is_registered() {
        let engine = engine();
        let layer = class_object(&engine, "Layer");
        for name in [
            "getCropRect",
            "getCropRectZero",
            "getDiffRect",
            "getDiffPixel",
            "oozeColor",
            "copyBlueToAlpha",
            "isBlank",
            "clearAlpha",
            "saveLayerImagePng",
            "saveLayerImagePngOctet",
            "saveLayerImageTlg5",
        ] {
            assert!(is_callable_member(&engine, layer, name), "Layer.{name}");
        }
        let window = class_object(&engine, "Window");
        for name in [
            "startSaveLayerImage",
            "cancelSaveLayerImage",
            "stopSaveLayerImage",
        ] {
            assert!(is_callable_member(&engine, window, name), "Window.{name}");
        }
    }

    /// The reference's own `numparams` checks (`utils.cpp:196-202, 244-251,
    /// 261-298, 443-446`, `savepng.cpp:236-238`, `savetlg5.cpp:257-259`).
    #[test]
    fn argument_counts_are_the_reference_checks() {
        let mut engine = engine();
        run(
            &mut engine,
            "layer.tjs",
            "global.layer = new Layer(); layer.setImageSize(2, 2); layer.fillRect(0, 0, 2, 2, 0xff00ff00);",
        );
        for call in [
            "layer.getDiffRect();",
            "layer.getDiffPixel();",
            "layer.oozeColor();",
            "layer.isBlank(0, 0, 1);",
            "layer.saveLayerImagePng();",
            "layer.saveLayerImageTlg5();",
        ] {
            let error = engine.execute_script("bad.tjs", call).expect_err(call);
            assert_eq!(
                error.kind,
                krkr_tjs2::TjsErrorKind::BadParamCount,
                "{call}: {error:?}"
            );
        }
        // `copyBlueToAlpha` declares no minimum; a missing argument reaches
        // its own check instead (`utils.cpp:394-400`).
        let error = engine
            .execute_script("bad.tjs", "layer.copyBlueToAlpha();")
            .expect_err("missing src");
        assert_eq!(error.message, "src must be Layer.");
        // `getCropRect`/`clearAlpha` accept a bare call.
        run(
            &mut engine,
            "ok.tjs",
            "layer.clearAlpha(); layer.getCropRect();",
        );
    }

    /// `GetCropRect`/`GetCropRectZero` (`utils.cpp:94-165`): the content box,
    /// `void` for an empty image, and the pixel-zero variant's extra scope.
    #[test]
    fn crop_rects_scan_the_whole_image() {
        let mut engine = engine();
        run(
            &mut engine,
            "layer.tjs",
            r#"
            global.layer = new Layer();
            layer.setImageSize(5, 4);
            layer.fillRect(0, 0, 5, 4, 0x00000000);
            layer.fillRect(1, 1, 3, 2, 0xff112233);
            "#,
        );
        assert_eq!(
            rect(&mut engine, "rectString(layer.getCropRect())"),
            "1,1,3,2"
        );
        assert_eq!(
            rect(&mut engine, "rectString(layer.getCropRectZero())"),
            "1,1,3,2"
        );

        // A transparent pixel with colour is invisible to `getCropRect`
        // (alpha test, `utils.cpp:82-88`) but visible to `getCropRectZero`
        // (any byte, `:124-130`).
        run(
            &mut engine,
            "colour.tjs",
            "layer.fillRect(0, 0, 1, 1, 0x00123456);",
        );
        assert_eq!(
            rect(&mut engine, "rectString(layer.getCropRect())"),
            "1,1,3,2"
        );
        assert_eq!(
            rect(&mut engine, "rectString(layer.getCropRectZero())"),
            "0,0,4,3",
            "the colour-only pixel enters the box, the empty row does not"
        );

        // A fully transparent image answers void.
        run(
            &mut engine,
            "empty.tjs",
            "global.empty = new Layer(); empty.setImageSize(3, 3); empty.fillRect(0, 0, 3, 3, 0x00000000);",
        );
        assert_eq!(rect(&mut engine, "rectString(empty.getCropRect())"), "void");
        assert_eq!(
            rect(&mut engine, "rectString(empty.getCropRectZero())"),
            "void"
        );
    }

    /// `GetDiffRect`/`GetDiffPixel` (`utils.cpp:191-284`).
    #[test]
    fn diff_rect_and_pixel_compare_two_layers() {
        let mut transparent = engine();
        let mut engine = engine();
        run(
            &mut engine,
            "layers.tjs",
            r#"
            global.base = new Layer();
            base.setImageSize(3, 2);
            base.fillRect(0, 0, 3, 2, 0xff102030);
            global.layer = new Layer();
            layer.setImageSize(3, 2);
            layer.fillRect(0, 0, 3, 2, 0xff102030);
            "#,
        );
        assert_eq!(
            rect(&mut engine, "rectString(layer.getDiffRect(base))"),
            "void"
        );
        assert_eq!(
            engine
                .execute_expression("count.tjs", "layer.getDiffPixel(base)")
                .expect("count")
                .to_integer()
                .expect("integer"),
            0
        );

        run(
            &mut engine,
            "change.tjs",
            "layer.fillRect(2, 1, 1, 1, 0xff0a0b0c);",
        );
        assert_eq!(
            rect(&mut engine, "rectString(layer.getDiffRect(base))"),
            "2,1,1,1"
        );
        assert_eq!(
            engine
                .execute_expression("count.tjs", "layer.getDiffPixel(base)")
                .expect("count")
                .to_integer()
                .expect("integer"),
            1
        );

        // Transparent pixels are equal whatever their colour
        // (`IS_SAME_COLOR`, `utils.cpp:168-171`).
        run(
            &mut transparent,
            "layers.tjs",
            r#"
            global.base = new Layer();
            base.setImageSize(1, 1);
            base.fillRect(0, 0, 1, 1, 0x00112233);
            global.layer = new Layer();
            layer.setImageSize(1, 1);
            layer.fillRect(0, 0, 1, 1, 0x00445566);
            "#,
        );
        assert_eq!(
            rect(&mut transparent, "rectString(layer.getDiffRect(base))"),
            "void"
        );

        // The fill colours are `0xAARRGGBB` DWORD writes
        // (`utils.cpp:270-272`, `:213-225`).
        run(
            &mut engine,
            "fill.tjs",
            "layer.getDiffPixel(base, 0xff00ff00, 0xff0000ff);",
        );
        assert_eq!(pixel(&mut engine, "layer", 0, 0), 0x00ff00);
        assert_eq!(pixel(&mut engine, "layer", 2, 1), 0x0000ff);

        // A size mismatch is the reference's error (`utils.cpp:216-217`).
        run(
            &mut engine,
            "other.tjs",
            "global.other = new Layer(); other.setImageSize(2, 2); other.fillRect(0, 0, 2, 2, 0xff000000);",
        );
        let error = engine
            .execute_script("bad.tjs", "layer.getDiffRect(other);")
            .expect_err("size mismatch");
        assert_eq!(error.message, "Different layer size.");
        let error = engine
            .execute_script("bad.tjs", "layer.getDiffPixel(other);")
            .expect_err("size mismatch");
        assert_eq!(error.message, "Different layer size.");
    }

    /// `OozeColor` (`utils.cpp:302-384`): the below-threshold colour clear,
    /// the round-by-round dilation and the fill colour.
    #[test]
    fn ooze_color_clears_and_dilates() {
        let mut engine = engine();
        run(
            &mut engine,
            "strip.tjs",
            r#"
            global.layer = new Layer();
            layer.setImageSize(5, 1);
            layer.fillRect(0, 0, 5, 1, 0x00000000);
            layer.fillRect(2, 0, 1, 1, 0xffff0000);
            "#,
        );
        // One round reaches the two neighbours only.
        run(
            &mut engine,
            "ooze.tjs",
            "layer.oozeColor(1, 1, 0x0000ff00);",
        );
        assert_eq!(
            pixel(&mut engine, "layer", 1, 0),
            0xff0000,
            "left neighbour"
        );
        assert_eq!(
            pixel(&mut engine, "layer", 3, 0),
            0xff0000,
            "right neighbour"
        );
        assert_eq!(
            pixel(&mut engine, "layer", 0, 0),
            0x00ff00,
            "out of reach stays at the fill colour"
        );
        assert_eq!(pixel(&mut engine, "layer", 4, 0), 0x00ff00);
        assert_eq!(
            mask(&mut engine, "layer", 1, 0),
            0,
            "alpha is never written"
        );

        // The reference re-clears every below-threshold pixel on each call
        // (`utils.cpp:326-340`), so reaching the ends takes one call with
        // `level = 2`, not two calls.
        run(
            &mut engine,
            "ooze2.tjs",
            "layer.oozeColor(1, 1, 0x0000ff00);",
        );
        assert_eq!(
            pixel(&mut engine, "layer", 0, 0),
            0x00ff00,
            "the second call starts over, so pixel 1 cannot seed pixel 0"
        );
        run(
            &mut engine,
            "reset.tjs",
            r#"
            global.layer = new Layer();
            layer.setImageSize(5, 1);
            layer.fillRect(0, 0, 5, 1, 0x00000000);
            layer.fillRect(2, 0, 1, 1, 0xffff0000);
            "#,
        );
        run(
            &mut engine,
            "ooze3.tjs",
            "layer.oozeColor(2, 1, 0x0000ff00);",
        );
        assert_eq!(pixel(&mut engine, "layer", 0, 0), 0xff0000, "round two");
        assert_eq!(pixel(&mut engine, "layer", 4, 0), 0xff0000, "round two");

        // The threshold gates which pixels are accepted in the first place.
        run(
            &mut engine,
            "half.tjs",
            r#"
            global.half = new Layer();
            half.setImageSize(1, 1);
            half.fillRect(0, 0, 1, 1, 0x80112233);
            "#,
        );
        run(&mut engine, "step.tjs", "half.oozeColor(1, 100, 0);");
        assert_eq!(
            pixel(&mut engine, "half", 0, 0),
            0x112233,
            "alpha 0x80 >= 100 is accepted"
        );
        run(
            &mut engine,
            "step.tjs",
            "half.oozeColor(1, 200, 0x00000000);",
        );
        assert_eq!(
            pixel(&mut engine, "half", 0, 0),
            0,
            "alpha 0x80 < 200 is cleared to the fill colour"
        );

        let error = engine
            .execute_script("bad.tjs", "layer.oozeColor(0);")
            .expect_err("level 0");
        assert_eq!(error.message, "Invalid level count.");
    }

    /// `CopyBlueToAlpha` (`utils.cpp:392-428`): the source's blue byte (engine
    /// byte 2) into the destination's alpha, over the smaller image.
    #[test]
    fn copy_blue_to_alpha_transfers_the_blue_channel() {
        let mut engine = engine();
        run(
            &mut engine,
            "layers.tjs",
            r#"
            global.source = new Layer();
            source.setImageSize(2, 1);
            source.fillRect(0, 0, 1, 1, 0xff112233);
            source.fillRect(1, 0, 1, 1, 0xff445566);
            global.layer = new Layer();
            layer.setImageSize(3, 1);
            layer.fillRect(0, 0, 3, 1, 0xffffffff);
            "#,
        );
        run(&mut engine, "copy.tjs", "layer.copyBlueToAlpha(source);");
        assert_eq!(mask(&mut engine, "layer", 0, 0), 0x33);
        assert_eq!(mask(&mut engine, "layer", 1, 0), 0x66);
        assert_eq!(mask(&mut engine, "layer", 2, 0), 0xff, "past the source");

        let error = engine
            .execute_script("bad.tjs", "layer.copyBlueToAlpha(42);")
            .expect_err("not a layer");
        assert_eq!(error.message, "src must be Layer.");
    }

    /// `isBlank` (`utils.cpp:436-477`): the blue byte test and the
    /// reference's own bounds check, typo included.
    #[test]
    fn is_blank_tests_the_blue_byte() {
        let mut red = engine();
        let mut engine = engine();
        run(
            &mut engine,
            "layer.tjs",
            r#"
            global.layer = new Layer();
            layer.setImageSize(3, 1);
            layer.fillRect(0, 0, 3, 1, 0x00000000);
            layer.fillRect(1, 0, 1, 1, 0x000000ff);
            "#,
        );
        let blank = |engine: &mut KrkrEngine, call: &str| -> i64 {
            engine
                .execute_expression("blank.tjs", call)
                .expect("isBlank")
                .to_integer()
                .expect("integer")
        };
        assert_eq!(
            blank(&mut engine, "layer.isBlank(0, 0, 1, 1)"),
            1,
            "no blue"
        );
        assert_eq!(
            blank(&mut engine, "layer.isBlank(1, 0, 1, 1)"),
            0,
            "blue set"
        );
        assert_eq!(blank(&mut engine, "layer.isBlank(0, 0, 3, 1)"), 0);
        assert_eq!(
            blank(&mut engine, "layer.isBlank(0, 0, 1, 1)"),
            1,
            "the blue byte is the test"
        );
        // A negative width passes the bounds check and scans nothing.
        assert_eq!(blank(&mut engine, "layer.isBlank(1, 0, -1, 1)"), 1);

        let error = engine
            .execute_script("bad.tjs", "layer.isBlank(2, 0, 2, 1);")
            .expect_err("past the edge");
        assert_eq!(error.message, "invalid layer range");
        let error = engine
            .execute_script("bad.tjs", "layer.isBlank(-1, 0, 1, 1);")
            .expect_err("negative origin");
        assert_eq!(error.message, "invalid layer range");

        // Only red, no blue: still "blank" — the reference tests byte 0 of a
        // B, G, R, A buffer, which is the blue channel.
        run(
            &mut red,
            "red.tjs",
            "global.layer = new Layer(); layer.setImageSize(1, 1); layer.fillRect(0, 0, 1, 1, 0x00ff0000);",
        );
        assert_eq!(
            red.execute_expression("blank.tjs", "layer.isBlank(0, 0, 1, 1)")
                .expect("isBlank")
                .to_integer()
                .expect("integer"),
            1
        );
    }

    /// `clearAlpha` (`utils.cpp:485-510`): pixels at or below the threshold
    /// become `fillColor & 0xffffff` with a zero alpha.
    #[test]
    fn clear_alpha_paints_below_the_threshold() {
        let mut engine = engine();
        run(
            &mut engine,
            "layer.tjs",
            r#"
            global.layer = new Layer();
            layer.setImageSize(3, 1);
            layer.fillRect(0, 0, 1, 1, 0x00000000);
            layer.fillRect(1, 0, 1, 1, 0x80112233);
            layer.fillRect(2, 0, 1, 1, 0xff445566);
            "#,
        );
        run(
            &mut engine,
            "clear.tjs",
            "layer.clearAlpha(0x80, 0x00123456);",
        );
        assert_eq!(pixel(&mut engine, "layer", 0, 0), 0x123456);
        assert_eq!(mask(&mut engine, "layer", 0, 0), 0);
        assert_eq!(
            pixel(&mut engine, "layer", 1, 0),
            0x123456,
            "at the threshold"
        );
        assert_eq!(mask(&mut engine, "layer", 1, 0), 0);
        assert_eq!(pixel(&mut engine, "layer", 2, 0), 0x445566, "above");
        assert_eq!(mask(&mut engine, "layer", 2, 0), 0xff);
    }

    /// A layer whose pixels differ in every channel, and which avoids the
    /// engine's magenta colour key.
    const PATTERN: &str = r#"
        global.layer = new Layer();
        layer.setImageSize(4, 3);
        layer.fillRect(0, 0, 4, 3, 0x00000000);
        layer.fillRect(0, 0, 1, 1, 0x80112233);
        layer.fillRect(3, 2, 1, 1, 0xff445566);
        layer.fillRect(1, 1, 2, 1, 0xff070809);
    "#;

    /// Loads `storage` into a fresh layer and returns whether every pixel
    /// matches `layer`.
    fn round_trips(engine: &mut KrkrEngine, storage: &str) -> bool {
        run(
            engine,
            "load.tjs",
            &format!(
                r#"
                global.loadWindow = new Window();
                global.loaded = new Layer(loadWindow, null);
                loaded.loadImages("{storage}");
                "#
            ),
        );
        let mut same = true;
        for y in 0..3 {
            for x in 0..4 {
                same &= pixel(engine, "loaded", x, y) == pixel(engine, "layer", x, y);
                same &= mask(engine, "loaded", x, y) == mask(engine, "layer", x, y);
            }
        }
        same
    }

    /// The PNG writer (`savepng.cpp:158-255`): a real PNG that the engine's
    /// own loader decodes back to the same pixels.
    #[test]
    fn save_layer_image_png_round_trips_through_the_engine_loader() {
        let mut engine = engine();
        run(&mut engine, "layer.tjs", PATTERN);
        run(
            &mut engine,
            "save.tjs",
            "layer.saveLayerImagePng(\"shot.png\");",
        );
        let bytes = saved(&engine, "shot.png");
        assert_eq!(&bytes[..8], b"\x89PNG\x0D\x0A\x1A\x0A");
        assert_eq!(&bytes[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 4);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 3);
        assert_eq!(&bytes[24..29], &[8, 6, 0, 0, 0], "8-bit RGBA, no interlace");
        assert!(bytes.windows(4).any(|window| window == b"IDAT"));
        let end = bytes.len();
        assert_eq!(
            &bytes[end - 12..end - 4],
            &[0, 0, 0, 0, b'I', b'E', b'N', b'D']
        );
        assert!(round_trips(&mut engine, "shot.png"));
    }

    /// The tag dictionary becomes the `pHYs`/`oFFs`/`vpAg` chunks
    /// (`savepng.cpp:209-232`).
    #[test]
    fn save_layer_image_png_writes_the_tag_chunks() {
        let mut engine = engine();
        run(&mut engine, "layer.tjs", PATTERN);
        run(
            &mut engine,
            "save.tjs",
            r#"
            layer.saveLayerImagePng("tagged.png", %[
                reso_x: 72, reso_y: 72, reso_unit: "meter",
                offs_x: 1, offs_y: 2, offs_unit: "micrometer",
                vpag_w: 10, vpag_h: 20,
                comp_lv: 1,
            ]);
            "#,
        );
        let bytes = saved(&engine, "tagged.png");
        // The signature and IHDR come first: pHYs starts at 8 + 25 = 33.
        assert_eq!(u32::from_be_bytes(bytes[33..37].try_into().unwrap()), 9);
        assert_eq!(&bytes[37..41], b"pHYs");
        assert_eq!(u32::from_be_bytes(bytes[41..45].try_into().unwrap()), 72);
        assert_eq!(u32::from_be_bytes(bytes[45..49].try_into().unwrap()), 72);
        assert_eq!(bytes[49], 1, "meter");
        assert!(bytes.windows(4).any(|window| window == b"oFFs"));
        assert!(bytes.windows(4).any(|window| window == b"vpAg"));
    }

    /// `comp_lv` is honoured (`savepng.cpp:230-231`): 0 stores, 1-9
    /// LZSS-press, and a level zlib would reject is the reference's
    /// "deflate initialize" (`savepng.cpp:61-62`).
    #[test]
    fn save_layer_image_png_levels() {
        let mut engine = engine();
        run(&mut engine, "layer.tjs", PATTERN);
        for level in [0, 1, 9] {
            let name = format!("level{level}.png");
            run(
                &mut engine,
                "save.tjs",
                &format!("layer.saveLayerImagePng(\"{name}\", %[comp_lv: {level}]);"),
            );
            assert!(round_trips(&mut engine, &name), "level {level}");
        }
        let error = engine
            .execute_script(
                "bad.tjs",
                "layer.saveLayerImagePng(\"bad.png\", %[comp_lv: 10]);",
            )
            .expect_err("level 10");
        assert_eq!(error.message, "deflate initialize");
        let error = engine
            .execute_expression("bad.tjs", "layer.saveLayerImagePngOctet(10)")
            .expect_err("octet level 10");
        assert_eq!(error.message, "deflate initialize");
    }

    /// `saveLayerImagePngOctet` (`savepng.cpp:263-274`): the same stream as
    /// the file, and an empty string when the layer has no image.
    #[test]
    fn save_layer_image_png_octet_matches_the_file() {
        let mut engine = engine();
        run(&mut engine, "layer.tjs", PATTERN);
        run(
            &mut engine,
            "save.tjs",
            "layer.saveLayerImagePng(\"level1.png\", %[comp_lv: 1]);",
        );
        let file = saved(&engine, "level1.png");
        let octet = engine
            .execute_expression("octet.tjs", "layer.saveLayerImagePngOctet(1)")
            .expect("octet");
        let Variant::Octet(bytes) = octet else {
            panic!("expected an octet, got {octet:?}");
        };
        assert_eq!(bytes, file, "the octet form is the file form");
        // The default level is 1 (`savepng.cpp:269`).
        let default = engine
            .execute_expression("octet.tjs", "layer.saveLayerImagePngOctet()")
            .expect("octet");
        assert_eq!(default, Variant::Octet(file.clone()));

        run(&mut engine, "free.tjs", "layer.freeImage();");
        let empty = engine
            .execute_expression("octet.tjs", "layer.saveLayerImagePngOctet()")
            .expect("octet");
        assert_eq!(
            empty.to_tjs_string().expect("string"),
            "",
            "no image: the reference clears the result (`savepng.cpp:297-299`)"
        );
    }

    /// The TLG5 writer (`savetlg5.cpp:20-171`): a real TLG5 stream the
    /// engine's own decoder reads back.
    #[test]
    fn save_layer_image_tlg5_round_trips_through_the_engine_loader() {
        let mut engine = engine();
        run(&mut engine, "layer.tjs", PATTERN);
        run(
            &mut engine,
            "save.tjs",
            "layer.saveLayerImageTlg5(\"shot.tlg5\");",
        );
        let bytes = saved(&engine, "shot.tlg5");
        assert_eq!(&bytes[..11], b"TLG5.0\x00raw\x1a");
        assert_eq!(bytes[11], 4, "four channels");
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 4);
        // The back-patched block-size table covers the rest of the file.
        let block_size = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        assert_eq!(block_size as usize, bytes.len() - 28);
        assert!(round_trips(&mut engine, "shot.tlg5"));
    }

    /// A non-empty tag dictionary wraps the stream in the `TLG0.0` container
    /// (`savetlg5.cpp:216-247`), which the engine's loader unwraps.
    #[test]
    fn save_layer_image_tlg5_wraps_the_tags_container() {
        let mut engine = engine();
        run(&mut engine, "layer.tjs", PATTERN);
        run(
            &mut engine,
            "save.tjs",
            r#"layer.saveLayerImageTlg5("tagged.tlg5", %[comment: "hello", n: 3]);"#,
        );
        let bytes = saved(&engine, "tagged.tlg5");
        assert_eq!(&bytes[..11], b"TLG0.0\x00sds\x1a");
        let raw_length = u32::from_le_bytes(bytes[11..15].try_into().unwrap()) as usize;
        // magic (11) + length (4) + raw stream + "tags" (4) + size (4) + tags
        let tags_offset = 15 + raw_length;
        assert_eq!(&bytes[tags_offset..tags_offset + 4], b"tags");
        let tags_length =
            u32::from_le_bytes(bytes[tags_offset + 4..tags_offset + 8].try_into().unwrap())
                as usize;
        assert_eq!(tags_offset + 8 + tags_length, bytes.len());
        // Members come out in the runtime's order (alphabetical), each as
        // `<len>:<name>=<len>:<value>,` (`savetlg5.cpp:230-235`).
        let tags = "7:comment=5:hello,1:n=1:3,";
        assert_eq!(tags_length, tags.len());
        assert!(
            bytes
                .windows(tags.len())
                .any(|window| window == tags.as_bytes()),
            "the tags string is written verbatim"
        );
        assert!(round_trips(&mut engine, "tagged.tlg5"));
    }

    /// The writers' error shapes (`compress.hpp:139-156`,
    /// `utils.cpp:42-68`).
    #[test]
    fn the_writers_report_invalid_layers_and_bad_targets() {
        let mut engine = engine();
        run(
            &mut engine,
            "freed.tjs",
            "global.layer = new Layer(); layer.setImageSize(2, 2); layer.fillRect(0, 0, 2, 2, 0xff00ff00); layer.freeImage();",
        );
        for (call, message) in [
            ("layer.saveLayerImagePng(\"x.png\");", "x.png:invalid layer"),
            (
                "layer.saveLayerImageTlg5(\"x.tlg\");",
                "x.tlg:invalid layer",
            ),
            ("layer.getCropRect();", "Invalid layer image."),
            ("layer.getDiffRect(layer);", "Invalid layer image."),
            ("layer.oozeColor(1);", "Invalid layer image."),
            ("layer.clearAlpha();", "dest must be Layer."),
            ("layer.isBlank(0, 0, 1, 1);", "src must be Layer."),
        ] {
            let error = engine.execute_script("bad.tjs", call).expect_err(call);
            assert_eq!(error.message, message, "{call}");
        }
        // An octet save with no image is not an error: the reference clears
        // the result instead (`savepng.cpp:297-299`).
        assert_eq!(
            engine
                .execute_expression("bad.tjs", "layer.saveLayerImagePngOctet()")
                .expect("octet")
                .to_tjs_string()
                .expect("string"),
            ""
        );

        // A storage the project refuses to open (`compress.hpp:172-176`).
        run(&mut engine, "layer.tjs", PATTERN);
        let error = engine
            .execute_script("bad.tjs", "layer.saveLayerImagePng(\"..\\\\outside.png\");")
            .expect_err("a traversal name");
        assert_eq!(error.message, "..\\outside.png:can't open");
    }

    /// `Window.startSaveLayerImage` (`Main.cpp:204-266`): the handler, the
    /// file, and the two events with the reference's arguments. The save runs
    /// synchronously here (the module docs explain why), so the events are
    /// already in the script's log when the call returns.
    #[test]
    fn window_start_save_writes_the_file_and_fires_the_events() {
        let mut engine = engine();
        run(&mut engine, "layer.tjs", PATTERN);
        run(
            &mut engine,
            "events.tjs",
            r#"
            global.events = [];
            global.window = new Window();
            window.onSaveLayerImageProgress = function(handler, percent, layer, filename) {
                global.events.push("progress:" + handler + ":" + percent + ":" + filename);
            };
            window.onSaveLayerImageDone = function(handler, canceled, layer, filename) {
                global.events.push("done:" + handler + ":" + canceled + ":" + filename);
            };
            "#,
        );
        let handler = engine
            .execute_expression(
                "save.tjs",
                "window.startSaveLayerImage(layer, \"window.png\", void)",
            )
            .expect("start")
            .to_integer()
            .expect("integer");
        assert_eq!(handler, 0, "the first free handler slot");
        assert!(engine.host().storage_exists("window.png"));
        assert_eq!(
            events(&mut engine),
            "progress:0:100:window.png|done:0:0:window.png"
        );

        // The next save takes the next slot, and a non-`.png` name selects
        // TLG5 (`Main.cpp:298-306`).
        let second = engine
            .execute_expression(
                "save.tjs",
                "window.startSaveLayerImage(layer, \"second.tlg5\", void)",
            )
            .expect("start")
            .to_integer()
            .expect("integer");
        assert_eq!(
            second, 0,
            "done frees the handler slot, so the next save reuses it"
        );
        assert_eq!(&saved(&engine, "second.tlg5")[..11], b"TLG5.0\x00raw\x1a");
        assert_eq!(
            events(&mut engine),
            "progress:0:100:window.png|done:0:0:window.png|progress:0:0:second.tlg5|progress:0:100:second.tlg5|done:0:0:second.tlg5",
            "the TLG5 path reports per block (`savetlg5.cpp:58-61`)"
        );
    }

    /// `cancelSaveLayerImage` from a progress handler stops the save before
    /// the file is written and reports `canceled = 1`; `stopSaveLayerImage`
    /// silences the events and the write (`Main.cpp:79-113, 268-285`).
    #[test]
    fn window_cancel_and_stop_stop_the_save() {
        let mut stopped_engine = engine();
        let mut engine = engine();
        run(&mut engine, "layer.tjs", PATTERN);
        run(
            &mut engine,
            "events.tjs",
            r#"
            global.events = [];
            global.window = new Window();
            window.onSaveLayerImageProgress = function(handler, percent, layer, filename) {
                global.events.push("progress:" + percent);
                window.cancelSaveLayerImage(handler);
            };
            window.onSaveLayerImageDone = function(handler, canceled, layer, filename) {
                global.events.push("done:" + canceled);
            };
            "#,
        );
        engine
            .execute_expression(
                "save.tjs",
                "window.startSaveLayerImage(layer, \"canceled.png\", void)",
            )
            .expect("start");
        assert!(!engine.host().storage_exists("canceled.png"));
        assert_eq!(events(&mut engine), "progress:100|done:1");

        let stopped = &mut stopped_engine;
        run(stopped, "layer.tjs", PATTERN);
        run(
            stopped,
            "events.tjs",
            r#"
            global.events = [];
            global.window = new Window();
            window.onSaveLayerImageProgress = function(handler, percent, layer, filename) {
                global.events.push("progress:" + percent);
                window.stopSaveLayerImage(handler);
            };
            window.onSaveLayerImageDone = function(handler, canceled, layer, filename) {
                global.events.push("done:" + canceled);
            };
            "#,
        );
        stopped
            .execute_expression(
                "save.tjs",
                "window.startSaveLayerImage(layer, \"stopped.png\", void)",
            )
            .expect("start");
        assert!(!stopped.host().storage_exists("stopped.png"));
        assert_eq!(events(stopped), "progress:100", "no done event");
        // Unknown handlers are ignored by both (`Main.cpp:270-285`).
        run(
            stopped,
            "bad.tjs",
            "window.cancelSaveLayerImage(99); window.stopSaveLayerImage(99); window.cancelSaveLayerImage(-1);",
        );
    }

    /// The events' log line from the scripts above.
    fn events(engine: &mut KrkrEngine) -> String {
        engine
            .execute_expression("events.tjs", "global.events.join(\"|\")")
            .expect("events")
            .to_tjs_string()
            .expect("string")
    }
}
