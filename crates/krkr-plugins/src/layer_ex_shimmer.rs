//! `layerExShimmer.dll`: the heat-haze ("kagerou") layer compositor.
//!
//! Real plugin: KAICHO Soft's *ShimmerPlugin* 0.3.1.0, `main.cpp` (CP932 with
//! CRLF; the source tree mirrored at <https://github.com/uyjulian/layerExShimmer>
//! — neither PARQUET nor GINKA ships the DLL, so that source is the authority
//! for everything below, and every line number here is that file). The
//! `layerEx*` family contract holds (`docs/plugins/layer-ex-family.md` §1 —
//! the dossier predates the mirror and has no `layerExShimmer` section, but
//! the shared rules in §1 are the same for every member): the plugin attaches
//! members to the global `Layer` class, `NCB_ATTACH_CLASS_WITH_HOOK(
//! layerExShimmer, Layer)` (`main.cpp:1439-1443`), the base class caches the
//! layer's image properties before every call
//! (`layerExDraw/layerExBase.hpp:105-110`, driven by the instance hook at
//! `main.cpp:1421-1436`), and the layer's raw bitmap is mutated in place.
//!
//! | member | arguments | reference |
//! |---|---|---|
//! | `shimmer(src, map, mask, scalex, scaley, clipx, clipy, clipw, cliph)` | 9 | `main.cpp:663-962` |
//! | `shimmerBuildMap(map1, map1x, map1y, map2, map2x, map2y)` | 6 | `main.cpp:1343-1416` |
//!
//! Neither member calls `Layer.update()`: the reference has no `redraw()` call
//! anywhere (the base's `redraw()` is never reached,
//! `layerExDraw/layerExBase.hpp:96-100`),
//! so the port commits the bytes through the plugin API's write path — visible
//! to a following `getMainPixel`/`saveLayerImage` — and a script that wants
//! the repaint calls `Layer.update()` itself. The same holds for
//! `shimmerBuildMap`.
//!
//! # `shimmer(src, map, mask, scalex, scaley, clipx, clipy, clipw, cliph)`
//!
//! The clip box defaults to the **source's** image size when `clipw`/`cliph`
//! are 0, a negative origin trims the size, and the box is then clamped to
//! *this* layer's image (`_width`/`_height`, `main.cpp:709-724`). The map (and
//! the optional mask) must be at least as large as the resulting box, or the
//! call returns without touching anything (`:726-728`).
//!
//! For every pixel of the box the destination content is replaced by the
//! source sample at `(x + dx, y + dy)`, where `dx`/`dy` come from the *slope*
//! of the map's blue channel (`main.cpp:142-143`, the "blue element only, the
//! map image is grey" comment):
//!
//! * the interior (`main.cpp:150-360`) reads the map at the pixel's own
//!   clip-relative index `(i, j)` and takes the centred differences
//!   `blue(i+1, j) - blue(i-1, j)` and `blue(i, j+1) - blue(i, j-1)`;
//! * the one-pixel frame (`main.cpp:733-908`) exists because those taps would
//!   leave the map; the reference computes eight special blocks — the four
//!   edges with one-sided taps in the crossing direction, and the four corners
//!   with one-sided taps in both. The taps are clip-relative, so a clip box
//!   that is not anchored at the layer's origin samples the map from its own
//!   origin;
//! * `scalex`/`scaley` scale the slope; with a mask layer the mask's blue byte
//!   scales it per pixel (`*mskp/255`, `:750`, `:409-410`).
//!
//! The source sample is **clamped** into the source image, never wrapped: the
//! frame blocks call `bufadr3`/`ZERO2MAX2` (`main.cpp:60-63`) and the shipped
//! SSE2 interior clamps with `pminsw`/`pmaxsw` (`:211-212`), while the
//! non-built scalar interior (`:147`, `bufadr2`/`ZERO2MAX`, `:46`) would wrap.
//! See the build-variant section below. Whole 32-bit pixels are copied, so the
//! engine's R, G, B, A byte order needs no translation (§B.3.4); only the
//! map/mask blue byte is read, and that is the engine view's byte 2, the
//! reference's byte 0.
//!
//! # `shimmerBuildMap(map1, map1x, map1y, map2, map2x, map2y)`
//!
//! Builds a map image in *this* layer, over its whole image (`_buffer`, not
//! the clip box, `main.cpp:1357-1360`). `map1` is tiled across the layer with
//! the wrap origin `map1x`/`map1y` (`ZERO2MAX`, `:986-987`): destination
//! `(x, y)` reads `map1[(x - map1x) mod width][(y - map1y) mod height]`. With
//! a second map the two are averaged; `void` for `map2` copies `map1` alone.
//! The reference's own use is `shimmer`'s map: a grey image whose blue channel
//! becomes the slope field.
//!
//! # The two build variants
//!
//! `main.cpp:8-9` both `#define USE_SSE2` and `#define MULTI_THREAD`, and
//! `HowToBuild.txt:38-47` says the SSE2 instruction set is selected in the
//! project settings ("0.3.0.0 からマルチスレッド" / "SSE2ばりばりに"), so the
//! DLL games load is the SSE2 + thread-pool build. Where the scalar
//! `#ifndef USE_SSE2` branches differ, this port follows the shipped build —
//! the same choice `kaicho_trans.rs` makes for this author's transition
//! plugin, and each divergence is reproduced or listed here:
//!
//! * **Interior rounding.** The shipped interior multiplies in `float` and
//!   converts with `cvtps2dq` (`main.cpp:203-209`): round to nearest, ties to
//!   even. The frame blocks always use the integer fixed-point form
//!   `(slope * (int)(scale * 0x10000)) >> 16` (`:678`, `:746`), whose shift
//!   floors — so a slope/scale pair with a fractional product genuinely lands
//!   one pixel apart on the frame and in the interior, and both are
//!   reproduced. `shimmerBuildMap`'s two-map average is `pavgb`'s
//!   `(a + b + 1) / 2` per channel (`:1174`), not the scalar `(b1+b2)/2` with
//!   a forced opaque alpha (`:1114-1128`).
//! * **Interior edges.** The shipped interior clamps the source coordinate
//!   (`pminsw`/`pmaxsw`, `main.cpp:211-212`); the scalar branch would wrap
//!   (`bufadr2`). The port clamps everywhere.
//! * **`shimmerBuildMap` copies whole pixels.** The shipped path copies 16-
//!   or 4-byte pixels (`movdqu`/`movd`, `:1021-1089`), the scalar branch
//!   writes only the blue byte (`:992-994`). The port copies whole pixels.
//! * **Threading.** `main.cpp:934-960` splits the interior into row bands and
//!   runs them through `KThreadPool`; the bands are disjoint, every worker
//!   reads the same source/map/mask snapshots and writes only its own rows, so
//!   the port runs the same loop on one thread with the same result.
//! * **Two reference artefacts are not reproduced**: the SSE2 interior's
//!   4-pixel blocks read one pixel past the map's right edge for some widths
//!   (`:196-197`), and the two-map build's tiling rewinds by whole map widths
//!   while its loop copies `width/4` blocks, so a map whose width is not a
//!   multiple of 4 drifts (`:1135-1141`). The port reads only the map region
//!   the size check guarantees and tiles exactly.
//!
//! # Reproduced quirks
//!
//! * The four edge blocks stop one pixel short of the corner
//!   (`for (x = clipx+1; x < clipx+clipw-2; x++)`, `main.cpp:743`, and the
//!   same at `:769`, `:793`, `:818`) while the interior covers
//!   `x <= clipx+clipw-2` (`:910`): the four box pixels
//!   `(clipx+clipw-2, clipy)`, `(clipx+clipw-2, clipy+cliph-1)`,
//!   `(clipx, clipy+cliph-2)` and `(clipx+clipw-1, clipy+cliph-2)` keep their
//!   previous content. The port leaves them alone too.
//! * A layer that is not a `Layer`, and a layer whose main image the script
//!   freed, are TJS errors here — the reference dereferences the resulting
//!   null buffer, or reads `imageWidth` and gets "Not drawable layer type"
//!   (`LayerIntf.cpp:2319-2323`). A clip box that is degenerate or too large
//!   for the map, and a map with no pixels, return silently, exactly like the
//!   reference's early returns. Degenerate one-pixel clip boxes make the
//!   reference read outside the map (a row above, a column before); the port
//!   clamps those reads to the map's edge instead of reading whatever
//!   precedes it in memory.

// Every member answers through the plugin host's `Result<_, TjsError>` ABI, so
// this module carries the crate-wide `result_large_err` shape like every other
// plugin here (`add_font.rs`, `alpha_movie.rs`, …); allowing it keeps this
// file's own clippy run clean without hiding a defect.
#![allow(clippy::result_large_err)]

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{
        LayerBitmapView, LayerBitmapViewMut, layer_bitmap_read, layer_bitmap_write,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer.shimmer heat-haze displacement and Layer.shimmerBuildMap map construction",
    notes: "Both members ported from layerExShimmer/main.cpp:675-962 and :1353-1416 over the engine's layer bitmap views, with the shipped SSE2 build's arithmetic (float rounding in the interior, integer fixed point on the frame, pavgb averaging in the map build). The reference's clip/map-size early returns, its four unwritten frame pixels and its lack of Layer.update() are reproduced; input errors are the family's TJS errors.",
    install: |engine| engine.register_plugin(LayerExShimmerPlugin),
};

pub struct LayerExShimmerPlugin;

impl KrkrPlugin for LayerExShimmerPlugin {
    fn name(&self) -> &str {
        "layerExShimmer.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `NCB_ATTACH_CLASS_WITH_HOOK(layerExShimmer, Layer)` with
        // `NCB_METHOD(shimmer)` / `NCB_METHOD(shimmerBuildMap)`
        // (`main.cpp:1440-1443`). ncbind fails a call with
        // `_numparams < ArgsCount` (`ncbind.hpp:1186`) and ignores arguments
        // past the declaration, so nine and six are minimums.
        register_unless_closure(
            runtime,
            layer,
            "shimmer",
            NativeArgCount::AtLeast(9),
            layer_shimmer,
        );
        register_unless_closure(
            runtime,
            layer,
            "shimmerBuildMap",
            NativeArgCount::AtLeast(6),
            layer_shimmer_build_map,
        );
        Ok(())
    }
}

/// One source layer's plane, the reference's
/// `(mainImageBuffer, imageWidth, imageHeight, mainImageBufferPitch)`
/// quadruple (`main.cpp:686-706`).
struct Plane {
    width: i64,
    height: i64,
    pitch: usize,
    pixels: Vec<u8>,
}

impl Plane {
    fn snapshot(view: &LayerBitmapView<'_>) -> Self {
        Plane {
            width: i64::from(view.bitmap.width),
            height: i64::from(view.bitmap.height),
            pitch: view.bitmap.pitch as usize,
            pixels: view.pixels.to_vec(),
        }
    }

    /// The reference's `*(mapp ± TJSPIXELSIZE)` byte read (`main.cpp:138-139`)
    /// is the map pixel's blue element; the engine's own view is R, G, B, A
    /// per pixel (§B.3.4), so that is byte 2.
    fn blue(&self, x: i64, y: i64) -> i32 {
        self.blue_byte(x, y).into()
    }

    fn blue_byte(&self, x: i64, y: i64) -> u8 {
        self.sample(x, y).map_or(0, |pixel| pixel[2])
    }

    /// The reference's `bufadr2`/`bufadr3` sample: `bufadr3` clamps the
    /// coordinates into the image (`ZERO2MAX2`, `main.cpp:60-63`), which is
    /// what the frame blocks and the shipped interior use. Reads the reference
    /// makes outside the map (degenerate one-pixel clip boxes) clamp here
    /// instead of reading the neighbouring allocation.
    fn sample(&self, x: i64, y: i64) -> Option<&[u8]> {
        if self.width <= 0 || self.height <= 0 || self.pitch < 4 {
            return None;
        }
        let column = x.clamp(0, self.width - 1) as usize;
        let row = y.clamp(0, self.height - 1) as usize;
        let index = row * self.pitch + column * 4;
        self.pixels.get(index..index + 4)
    }
}

/// The clip box `shimmer` works over, in this layer's coordinates
/// (`main.cpp:709-724`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Clip {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

/// `shimmer`'s clip handling (`main.cpp:709-728`): 0 sizes mean the source's
/// size, a negative origin trims, the box is clamped to the destination image
/// and the map/mask must cover it. `None` is the reference's silent `return`.
#[allow(clippy::too_many_arguments)]
fn clip_box(
    dest_width: i64,
    dest_height: i64,
    source: &Plane,
    map: &Plane,
    mask: Option<&Plane>,
    mut x: i64,
    mut y: i64,
    mut w: i64,
    mut h: i64,
) -> Option<Clip> {
    if w == 0 {
        w = source.width;
    }
    if h == 0 {
        h = source.height;
    }
    if x < 0 {
        w += x;
        x = 0;
    }
    if y < 0 {
        h += y;
        y = 0;
    }
    if w <= 0 || h <= 0 || x >= dest_width || y >= dest_height {
        return None;
    }
    if x + w > dest_width {
        w = dest_width - x;
    }
    if y + h > dest_height {
        h = dest_height - y;
    }
    if w > map.width || h > map.height {
        return None;
    }
    if let Some(mask) = mask
        && (w > mask.width || h > mask.height)
    {
        return None;
    }
    Some(Clip { x, y, w, h })
}

/// The map and mask taps feeding one frame pixel's displacement
/// (`main.cpp:733-908`), in clip-box-relative coordinates.
#[derive(Clone, Copy)]
struct Taps {
    /// The two map samples whose blue difference is the horizontal slope.
    x: [(i64, i64); 2],
    /// The two map samples whose blue difference is the vertical slope.
    y: [(i64, i64); 2],
    /// The mask sample; unused when the call had no mask layer.
    mask: (i64, i64),
}

/// `(int)(scale * 0x10000)` (`main.cpp:678`): the fixed-point scale the frame
/// blocks use. Out-of-range and NaN inputs are the reference's `cvttss2si`
/// "integer indefinite" result, `INT_MIN`.
fn fixed_scale(scale: f32) -> i32 {
    let value = scale * 65536.0;
    if (-2147483648.0..2147483648.0).contains(&value) {
        value as i32
    } else {
        i32::MIN
    }
}

/// A frame block's displacement (`main.cpp:746`, `:750`, `:771`, `:774`, …):
/// `(slope * sx) >> 16`, or `(slope * sx * mask / 255) >> 16` with a mask, in
/// the reference's 32-bit wrapping integers.
fn frame_offset(slope: i32, sx: i32, mask: Option<u8>) -> i32 {
    let product = slope.wrapping_mul(sx);
    match mask {
        None => product >> 16,
        Some(mask) => (product.wrapping_mul(i32::from(mask)) / 255) >> 16,
    }
}

/// The shipped interior's displacement (`main.cpp:203-209`): the slope, the
/// scale and the mask's `v/255` multiplied in `float`, then converted with
/// `cvtps2dq` — round to nearest, ties to even.
fn interior_offset(slope: i32, scale: f32, mask: Option<u8>) -> i32 {
    let mut value = slope as f32 * scale;
    if let Some(mask) = mask {
        value *= f32::from(mask) / 255.0;
    }
    round_to_i32(value)
}

/// `cvtps2dq`: round to nearest with ties to even (`-0.5`, `0.5`, `1.5` … to
/// the even neighbour), and the integer-indefinite `INT_MIN` for NaN and
/// out-of-range values. `round_ties_even` propagates NaN, which fails both
/// bounds checks.
fn round_to_i32(value: f32) -> i32 {
    let rounded = value.round_ties_even();
    if (-2147483648.0..2147483648.0).contains(&rounded) {
        rounded as i32
    } else {
        i32::MIN
    }
}

/// `shimmer` (`main.cpp:675-962`).
fn layer_shimmer(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = this_layer(runtime, this_obj)?;
    // The instance hook's `reset()` runs before every call
    // (`main.cpp:1424-1432`), so this layer's image is resolved first; a freed
    // one is the reference's "Not drawable layer type" before any argument is
    // examined.
    let (dest_width, dest_height) = layer_bitmap_read(runtime, dest, |view| {
        (i64::from(view.bitmap.width), i64::from(view.bitmap.height))
    })?;

    let src = layer_argument(&args, 0, "shimmer: srclayer must be Layer.")?;
    let map = layer_argument(&args, 1, "shimmer: maplayer must be Layer.")?;
    // `msklayer.Type() == tvtVoid` (`main.cpp:698`); any other value must be a
    // layer — `null` included, which is an object variant in TJS2 and would
    // crash the reference's `PropGet`.
    let mask = match args.get(2) {
        None | Some(Variant::Void) => None,
        Some(value) => Some(
            value
                .object_handle()
                .ok_or_else(|| TjsError::runtime("shimmer: msklayer must be Layer."))?,
        ),
    };
    let scale_x = argument_real(&args, 3)? as f32;
    let scale_y = argument_real(&args, 4)? as f32;
    let clip_x = argument_integer(&args, 5)?;
    let clip_y = argument_integer(&args, 6)?;
    let clip_w = argument_integer(&args, 7)?;
    let clip_h = argument_integer(&args, 8)?;

    let source = read_plane(runtime, src)?;
    let map_plane = read_plane(runtime, map)?;
    let mask_plane = match mask {
        Some(mask) => Some(read_plane(runtime, mask)?),
        None => None,
    };

    // The reference can still return before touching the destination
    // (`main.cpp:718`, `:726-728`); validated before the write so a rejected
    // call commits nothing at all.
    let Some(clip) = clip_box(
        dest_width,
        dest_height,
        &source,
        &map_plane,
        mask_plane.as_ref(),
        clip_x,
        clip_y,
        clip_w,
        clip_h,
    ) else {
        return Ok(Variant::Void);
    };

    layer_bitmap_write(runtime, dest, |view| {
        shimmer_into(
            view,
            &source,
            &map_plane,
            mask_plane.as_ref(),
            scale_x,
            scale_y,
            clip,
        );
    })?;
    Ok(Variant::Void)
}

/// The whole of `shimmer`'s pixel work, in the reference's order: the eight
/// frame blocks (`main.cpp:733-908`) and then the interior (`:910-960`).
fn shimmer_into(
    dest: &mut LayerBitmapViewMut<'_>,
    source: &Plane,
    map: &Plane,
    mask: Option<&Plane>,
    scale_x: f32,
    scale_y: f32,
    clip: Clip,
) {
    let sx = fixed_scale(scale_x);
    let sy = fixed_scale(scale_y);

    // `main.cpp:733-757`: the top row, one pixel in from each corner.
    for i in 1..clip.w - 2 {
        let taps = Taps {
            x: [(i - 1, 0), (i + 1, 0)],
            y: [(i, 0), (i, 1)],
            mask: (i, 0),
        };
        write_frame_pixel(dest, source, map, mask, sx, sy, clip.x + i, clip.y, taps);
    }
    // `main.cpp:760-782`: the bottom row.
    for i in 1..clip.w - 2 {
        let taps = Taps {
            x: [(i - 1, clip.h - 1), (i + 1, clip.h - 1)],
            y: [(i, clip.h - 2), (i, clip.h - 1)],
            mask: (i, clip.h - 1),
        };
        write_frame_pixel(
            dest,
            source,
            map,
            mask,
            sx,
            sy,
            clip.x + i,
            clip.y + clip.h - 1,
            taps,
        );
    }
    // `main.cpp:784-807`: the left column.
    for j in 1..clip.h - 2 {
        let taps = Taps {
            x: [(0, j), (1, j)],
            y: [(0, j - 1), (0, j + 1)],
            mask: (0, j),
        };
        write_frame_pixel(dest, source, map, mask, sx, sy, clip.x, clip.y + j, taps);
    }
    // `main.cpp:809-832`: the right column.
    for j in 1..clip.h - 2 {
        let taps = Taps {
            x: [(clip.w - 2, j), (clip.w - 1, j)],
            y: [(clip.w - 1, j - 1), (clip.w - 1, j + 1)],
            mask: (clip.w - 1, j),
        };
        write_frame_pixel(
            dest,
            source,
            map,
            mask,
            sx,
            sy,
            clip.x + clip.w - 1,
            clip.y + j,
            taps,
        );
    }
    // `main.cpp:834-908`: the four corners, in the reference's order.
    let corners = [
        (
            clip.x,
            clip.y,
            Taps {
                x: [(0, 0), (1, 0)],
                y: [(0, 0), (0, 1)],
                mask: (0, 0),
            },
        ),
        (
            clip.x + clip.w - 1,
            clip.y,
            Taps {
                x: [(clip.w - 2, 0), (clip.w - 1, 0)],
                y: [(clip.w - 1, 0), (clip.w - 1, 1)],
                mask: (clip.w - 1, 0),
            },
        ),
        (
            clip.x,
            clip.y + clip.h - 1,
            Taps {
                x: [(0, clip.h - 1), (1, clip.h - 1)],
                y: [(0, clip.h - 2), (0, clip.h - 1)],
                mask: (0, clip.h - 1),
            },
        ),
        (
            clip.x + clip.w - 1,
            clip.y + clip.h - 1,
            Taps {
                x: [(clip.w - 2, clip.h - 1), (clip.w - 1, clip.h - 1)],
                y: [(clip.w - 1, clip.h - 2), (clip.w - 1, clip.h - 1)],
                mask: (clip.w - 1, clip.h - 1),
            },
        ),
    ];
    for (x, y, taps) in corners {
        write_frame_pixel(dest, source, map, mask, sx, sy, x, y, taps);
    }

    // `clipx += 1, clipw -= 2, clipy += 1, cliph -= 2` then the early return
    // (`main.cpp:910-912`); the interior is `x <= clipx+clipw-2` and
    // `y <= clipy+cliph-2` for the original box.
    if clip.w - 2 <= 0 || clip.h - 2 <= 0 {
        return;
    }
    if clip.x + 1 >= i64::from(dest.bitmap.width) || clip.y + 1 >= i64::from(dest.bitmap.height) {
        return;
    }
    for j in 1..clip.h - 1 {
        for i in 1..clip.w - 1 {
            let mask_byte = mask.map(|plane| plane.blue_byte(i, j));
            let slope_x = map.blue(i + 1, j) - map.blue(i - 1, j);
            let slope_y = map.blue(i, j + 1) - map.blue(i, j - 1);
            let offset_x = interior_offset(slope_x, scale_x, mask_byte);
            let offset_y = interior_offset(slope_y, scale_y, mask_byte);
            write_pixel(dest, source, clip.x + i, clip.y + j, offset_x, offset_y);
        }
    }
}

/// One frame pixel: the taps' two slopes through the frame blocks' integer
/// arithmetic (`main.cpp:746`, `:750`).
#[allow(clippy::too_many_arguments)]
fn write_frame_pixel(
    dest: &mut LayerBitmapViewMut<'_>,
    source: &Plane,
    map: &Plane,
    mask: Option<&Plane>,
    sx: i32,
    sy: i32,
    x: i64,
    y: i64,
    taps: Taps,
) {
    let mask_byte = mask.map(|plane| plane.blue_byte(taps.mask.0, taps.mask.1));
    let slope_x = map.blue(taps.x[1].0, taps.x[1].1) - map.blue(taps.x[0].0, taps.x[0].1);
    let slope_y = map.blue(taps.y[1].0, taps.y[1].1) - map.blue(taps.y[0].0, taps.y[0].1);
    let offset_x = frame_offset(slope_x, sx, mask_byte);
    let offset_y = frame_offset(slope_y, sy, mask_byte);
    write_pixel(dest, source, x, y, offset_x, offset_y);
}

/// `*dstp = *(TJSPIXEL*)bufadr3(srcbuf, x + dx, y + dy, …)`: one whole
/// 32-bit source pixel, clamped into the source image (`main.cpp:64-94`).
fn write_pixel(
    dest: &mut LayerBitmapViewMut<'_>,
    source: &Plane,
    x: i64,
    y: i64,
    offset_x: i32,
    offset_y: i32,
) {
    let Some(pixel) = source.sample(x + i64::from(offset_x), y + i64::from(offset_y)) else {
        return;
    };
    let index = y as usize * dest.bitmap.pitch as usize + x as usize * 4;
    if let Some(target) = dest.pixels.get_mut(index..index + 4) {
        target.copy_from_slice(pixel);
    }
}

/// `shimmerBuildMap` (`main.cpp:1353-1416`).
fn layer_shimmer_build_map(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dest = this_layer(runtime, this_obj)?;
    // The base's `reset()` runs first, as in `shimmer`.
    layer_bitmap_read(runtime, dest, |_| ())?;

    let map1 = layer_argument(&args, 0, "shimmerBuildMap: maplayer1 must be Layer.")?;
    let map1_x = argument_integer(&args, 1)?;
    let map1_y = argument_integer(&args, 2)?;
    // `maplayer2.Type() == tvtVoid` (`main.cpp:1372`) selects the one-map path.
    let map2 = match args.get(3) {
        None | Some(Variant::Void) => None,
        Some(value) => Some(
            value
                .object_handle()
                .ok_or_else(|| TjsError::runtime("shimmerBuildMap: maplayer2 must be Layer."))?,
        ),
    };
    let map2_x = argument_integer(&args, 4)?;
    let map2_y = argument_integer(&args, 5)?;

    let plane1 = read_plane(runtime, map1)?;
    let plane2 = match map2 {
        Some(map2) => Some(read_plane(runtime, map2)?),
        None => None,
    };
    // `ZERO2MAX(map1x, map1width)` divides by the map's width (`main.cpp:986`);
    // a map with no pixels has nothing to tile, and the reference would divide
    // by zero — for either map.
    if plane1.width <= 0 || plane1.height <= 0 {
        return Ok(Variant::Void);
    }
    if plane2
        .as_ref()
        .is_some_and(|plane| plane.width <= 0 || plane.height <= 0)
    {
        return Ok(Variant::Void);
    }

    layer_bitmap_write(runtime, dest, |view| {
        build_map(
            view,
            &plane1,
            map1_x,
            map1_y,
            plane2.as_ref(),
            map2_x,
            map2_y,
        );
    })?;
    Ok(Variant::Void)
}

/// `shimmerBuildMap`'s whole-image tile (`main.cpp:977-1094` and `:1098-1340`):
/// this layer's every pixel is `map1`'s wrapped sample, or the per-channel
/// average of `map1` and `map2`.
fn build_map(
    dest: &mut LayerBitmapViewMut<'_>,
    map1: &Plane,
    map1_x: i64,
    map1_y: i64,
    map2: Option<&Plane>,
    map2_x: i64,
    map2_y: i64,
) {
    let pitch = dest.bitmap.pitch as usize;
    let width = i64::from(dest.bitmap.width);
    let height = i64::from(dest.bitmap.height);
    let origin1 = (wrap(map1_x, map1.width), wrap(map1_y, map1.height));
    let origin2 = map2.map(|map2| (wrap(map2_x, map2.width), wrap(map2_y, map2.height)));

    for y in 0..height {
        for x in 0..width {
            let column = wrap(x - origin1.0, map1.width);
            let row = wrap(y - origin1.1, map1.height);
            let Some(first) = map1.sample(column, row) else {
                continue;
            };
            let mut pixel = [first[0], first[1], first[2], first[3]];
            if let (Some(map2), Some(origin2)) = (map2, origin2) {
                let column = wrap(x - origin2.0, map2.width);
                let row = wrap(y - origin2.1, map2.height);
                let Some(second) = map2.sample(column, row) else {
                    continue;
                };
                for (byte, other) in pixel.iter_mut().zip(second) {
                    // `pavgb` (`main.cpp:1174`): the rounded-up mean per byte.
                    *byte = (u16::from(*byte) + u16::from(*other)).div_ceil(2) as u8;
                }
            }
            let index = y as usize * pitch + x as usize * 4;
            if let Some(target) = dest.pixels.get_mut(index..index + 4) {
                target.copy_from_slice(&pixel);
            }
        }
    }
}

/// `ZERO2MAX` (`main.cpp:46`): the reference's signed modulo, into
/// `0..size-1`.
fn wrap(value: i64, size: i64) -> i64 {
    ((value % size) + size) % size
}

fn read_plane(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Result<Plane> {
    layer_bitmap_read(runtime, layer, Plane::snapshot)
}

fn layer_argument(args: &[Variant], index: usize, message: &str) -> Result<ObjectHandle> {
    args.get(index)
        .and_then(Variant::object_handle)
        .ok_or_else(|| TjsError::runtime(message))
}

fn argument_integer(args: &[Variant], index: usize) -> Result<i64> {
    args.get(index)
        .map(Variant::to_integer)
        .transpose()
        .map(|value| value.unwrap_or(0))
}

fn argument_real(args: &[Variant], index: usize) -> Result<f64> {
    args.get(index)
        .map(Variant::to_real)
        .transpose()
        .map(|value| value.unwrap_or(0.0))
}

fn this_layer(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Result<ObjectHandle> {
    let layer = this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    // The per-layer state is keyed by object, so a bound method acts on the
    // layer it binds.
    Ok(runtime.bound_this(layer).unwrap_or(layer))
}

/// Registers `function` unless a script already owns the member, the way the
/// rest of the family's ports attach their surface.
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
    use krkr_tjs2::runtime::{ObjectHandle, Variant};

    use super::{LayerExShimmerPlugin, fixed_scale, frame_offset, interior_offset};

    /// A 6x6 source whose pixel `(x, y)` is `(6*y + x + 1) << 16` (distinct in
    /// every pixel, red channel), a 6x6 destination filled transparent, and a
    /// 6x6 map whose blue channel is the ramp `COLUMNS` repeated down every
    /// column, so the horizontal slope is `COLUMNS[i+1] - COLUMNS[i-1]` and the
    /// vertical one is 0.
    const SETUP: &str = r#"
        global.src = new Layer();
        src.setImageSize(6, 6);
        global.dst = new Layer();
        dst.setImageSize(6, 6);
        dst.fillRect(0, 0, 6, 6, 0x00000000);
        global.map = new Layer();
        map.setImageSize(6, 6);
        for (var y = 0; y < 6; y++) {
            for (var x = 0; x < 6; x++) {
                src.fillRect(x, y, 1, 1, 0xff000000 | ((y * 6 + x + 1) << 16));
            }
        }
        var columns = [0, 2, 3, 9, 10, 12];
        for (var i = 0; i < 6; i++) {
            map.fillRect(i, 0, 1, 6, 0xff000000 | columns[i]);
        }
    "#;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(LayerExShimmerPlugin)
            .expect("plugin");
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
    }

    /// `src(x, y)`'s colour, the setup's `(6*y + x + 1) << 16`.
    fn colour(x: i64, y: i64) -> i64 {
        (y * 6 + x + 1) << 16
    }

    fn pixel(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMainPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    fn alpha(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMaskPixel({x}, {y})"))
            .expect("mask pixel")
            .to_integer()
            .expect("integer")
    }

    fn integer(engine: &mut KrkrEngine, expression: &str) -> i64 {
        engine
            .execute_expression("read.tjs", expression)
            .expect("integer")
            .to_integer()
            .expect("integer")
    }

    /// Every registered native lives on the global `Layer` class, so members
    /// are checked there.
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

    /// The texture id of the layer's image, which a commit replaces.
    fn generation(engine: &mut KrkrEngine, name: &str) -> u64 {
        let handle = engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .expect("layer");
        krkr_engine::plugin_api::layer::layer_bitmap_read(
            engine.tjs_runtime_mut(),
            handle,
            |view| view.bitmap.generation,
        )
        .expect("generation")
    }

    fn assert_grid(engine: &mut KrkrEngine, expected: [[i64; 6]; 6]) {
        for (y, row) in expected.iter().enumerate() {
            for (x, value) in row.iter().enumerate() {
                assert_eq!(
                    pixel(engine, "dst", x as i64, y as i64),
                    *value,
                    "dst({x}, {y})"
                );
            }
        }
    }

    /// The two members exist on the class with the reference's declared
    /// argument counts (`main.cpp:675`, `:1353`, `:1441-1442`); ncbind fails a
    /// short call with `_numparams < ArgsCount` (`ncbind.hpp:1186`) and ignores
    /// arguments past the declaration.
    #[test]
    fn the_two_members_are_registered_with_the_reference_arity() {
        let class_engine = engine();
        let layer = layer_class(&class_engine);
        for name in ["shimmer", "shimmerBuildMap"] {
            assert!(
                is_callable_member(&class_engine, layer, name),
                "Layer.{name} is registered"
            );
        }

        let mut engine = engine();
        for (call, name) in [
            (
                "dst.shimmer(src, map, void, 1, 1, 0, 0, 0);",
                "a nine-argument call is the minimum for shimmer",
            ),
            (
                "dst.shimmerBuildMap(map, 0, 0, void, 0);",
                "a six-argument call is the minimum for shimmerBuildMap",
            ),
        ] {
            let error = engine.execute_script("arity.tjs", call).expect_err(name);
            assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount, "{name}");
        }
        // Past the declared count the extra arguments are ignored, as ncbind
        // does.
        engine
            .execute_script(
                "arity.tjs",
                "dst.shimmer(src, map, void, 1, 1, 0, 0, 0, 0, 99); \
                 dst.shimmerBuildMap(map, 0, 0, void, 0, 0, 99);",
            )
            .expect("the reference's argument counts");
    }

    /// The two-frame case: the same map and source with `scalex = scaley = 0.5`
    /// and then `1.0`. Every expectation is hand-derived from the reference —
    /// the frame blocks' `(slope * (int)(scale * 0x10000)) >> 16`
    /// (`main.cpp:746`, `:750`) and the shipped interior's
    /// `round_ties_even(slope * scale)` (`:203-209`) — and the two frames
    /// differ in both, e.g. the slope 3 at clip column 1 gives `(3*32768)>>16
    /// = 1` on the top row but `round(3*0.5) = 2` one row below.
    #[test]
    fn two_frames_displace_by_the_map_slope_with_the_reference_arithmetic() {
        let mut engine = engine();
        engine
            .execute_script(
                "frame1.tjs",
                "dst.shimmer(src, map, void, 0.5, 0.5, 0, 0, 0, 0);",
            )
            .expect("frame 1");

        // Frame 1, scale 0.5. Frame offsets: slope 3 -> 1, slope 7 -> 3 (their
        // products with 32768 floor-shifted); interior offsets: 3 -> 2, 7 -> 4
        // (ties to even). The source clamps at column 5, and the four frame
        // pixels the reference's loops never reach — (4, 0), (4, 5), (0, 4),
        // (5, 4) — keep the transparent fill.
        assert_grid(
            &mut engine,
            [
                [0x020000, 0x030000, 0x060000, 0x060000, 0x000000, 0x060000],
                [0x080000, 0x0a0000, 0x0c0000, 0x0c0000, 0x0c0000, 0x0c0000],
                [0x0e0000, 0x100000, 0x120000, 0x120000, 0x120000, 0x120000],
                [0x140000, 0x160000, 0x180000, 0x180000, 0x180000, 0x180000],
                [0x000000, 0x1c0000, 0x1e0000, 0x1e0000, 0x1e0000, 0x000000],
                [0x200000, 0x210000, 0x240000, 0x240000, 0x000000, 0x240000],
            ],
        );

        engine
            .execute_script(
                "frame2.tjs",
                "dst.callOnPaint = 0; dst.shimmer(src, map, void, 1.0, 1.0, 0, 0, 0, 0);",
            )
            .expect("frame 2");
        // Frame 2, scale 1.0: both arithmetics agree, so the interior moves by
        // the whole slope (3, 7, 7, 3 from clip column 1 to 4) and the source
        // clamps wherever it leaves the image.
        assert_grid(
            &mut engine,
            [
                [0x030000, 0x050000, 0x060000, 0x060000, 0x000000, 0x060000],
                [0x090000, 0x0b0000, 0x0c0000, 0x0c0000, 0x0c0000, 0x0c0000],
                [0x0f0000, 0x110000, 0x120000, 0x120000, 0x120000, 0x120000],
                [0x150000, 0x170000, 0x180000, 0x180000, 0x180000, 0x180000],
                [0x000000, 0x1d0000, 0x1e0000, 0x1e0000, 0x1e0000, 0x000000],
                [0x210000, 0x230000, 0x240000, 0x240000, 0x000000, 0x240000],
            ],
        );

        // The copied pixels carry the source's alpha; the source itself is
        // untouched, and nothing posted a repaint: the reference has no
        // `redraw()` call at all, so `Layer.update()` stays the script's job.
        assert_eq!(alpha(&mut engine, "dst", 1, 1), 0xff);
        assert_eq!(
            alpha(&mut engine, "dst", 4, 0),
            0,
            "an unwritten frame pixel"
        );
        assert_eq!(pixel(&mut engine, "src", 5, 5), colour(5, 5));
        assert_eq!(integer(&mut engine, "dst.callOnPaint"), 0);
    }

    /// The boundary case: a map whose slopes point the other way asks for
    /// source coordinates left of the image, and the reference's `bufadr3`
    /// clamps them (`ZERO2MAX2`, `main.cpp:60-63`) — the shipped interior's
    /// `pminsw`/`pmaxsw` clamp too (`:211-212`). The non-built scalar interior
    /// would wrap instead (`bufadr2`, `:147`); this port follows the shipped
    /// build.
    #[test]
    fn a_negative_slope_clamps_the_source_at_zero() {
        let mut engine = engine();
        engine
            .execute_script(
                "negative.tjs",
                r#"
                global.neg = new Layer();
                neg.setImageSize(6, 6);
                var columns = [12, 10, 9, 3, 2, 0];
                for (var i = 0; i < 6; i++) {
                    neg.fillRect(i, 0, 1, 6, 0xff000000 | columns[i]);
                }
                dst.shimmer(src, neg, void, 1.0, 1.0, 0, 0, 0, 0);
                "#,
            )
            .expect("negative slopes");

        // The interior slopes are -3, -7, -7, -3, so columns 1..3 clamp to 0
        // and column 4 (4-3 = 1) reads the source's own second column.
        assert_eq!(pixel(&mut engine, "dst", 1, 1), colour(0, 1));
        assert_eq!(pixel(&mut engine, "dst", 2, 1), colour(0, 1));
        assert_eq!(pixel(&mut engine, "dst", 3, 1), colour(0, 1));
        assert_eq!(pixel(&mut engine, "dst", 4, 1), colour(1, 1));
        // The frame blocks: slope -2 at both corners, so the top-left corner
        // and the whole first interior column clamp too.
        assert_eq!(pixel(&mut engine, "dst", 0, 0), colour(0, 0));
        assert_eq!(pixel(&mut engine, "dst", 1, 0), colour(0, 0));
        assert_eq!(pixel(&mut engine, "dst", 0, 1), colour(0, 1));
        assert_eq!(pixel(&mut engine, "dst", 5, 1), colour(3, 1));
    }

    /// A clip box that is not the whole layer: the reference samples the map
    /// and mask from the *box's* origin (`main.cpp:737-741`, `:923-925`),
    /// writes nothing outside it, and leaves the four frame pixels its loops
    /// never reach unwritten.
    #[test]
    fn the_clip_box_moves_the_map_origin_and_bounds_the_writes() {
        let mut engine = engine();
        engine
            .execute_script(
                "clip.tjs",
                "dst.shimmer(src, map, void, 1.0, 1.0, 1, 1, 4, 4);",
            )
            .expect("clipped shimmer");

        // Every expectation by hand: the corners and frame blocks use the
        // box-relative taps, the 2x2 interior covers (2..3, 2..3), and
        // (3, 1), (3, 4), (1, 3), (4, 3) are the reference's unwritten frame
        // pixels.
        assert_grid(
            &mut engine,
            [
                [0, 0, 0, 0, 0, 0],
                [0, 0x0a0000, 0x0c0000, 0, 0x0c0000, 0],
                [0, 0x100000, 0x120000, 0x120000, 0x120000, 0],
                [0, 0, 0x180000, 0x180000, 0, 0],
                [0, 0x1c0000, 0x1e0000, 0, 0x1e0000, 0],
                [0, 0, 0, 0, 0, 0],
            ],
        );
    }

    /// A mask layer scales the displacement per pixel by its blue byte
    /// (`*mskp/255`, `main.cpp:750` for the frame, `:409-410` for the
    /// interior), on top of the two arithmetics above.
    #[test]
    fn a_mask_scales_the_displacement_per_pixel() {
        let mut engine = engine();
        engine
            .execute_script(
                "mask.tjs",
                r#"
                global.msk = new Layer();
                msk.setImageSize(6, 6);
                msk.fillRect(0, 0, 6, 6, 0xff000080);   // blue 128 everywhere
                msk.fillRect(3, 0, 1, 1, 0xff000000);   // except (3, 0)
                dst.shimmer(src, map, msk, 1.0, 1.0, 0, 0, 0, 0);
                "#,
            )
            .expect("masked shimmer");

        // Top row, slope 3 with mask 128: ((3*65536)*128/255)>>16 = 98689>>16
        // = 1, where the unmasked frame gives 3.
        assert_eq!(pixel(&mut engine, "dst", 1, 0), colour(2, 0));
        // Slope 7 with mask 128: ((7*65536)*128/255)>>16 = 230275>>16 = 3.
        assert_eq!(pixel(&mut engine, "dst", 2, 0), colour(5, 0));
        // The mask value 0 at (3, 0) leaves the pixel where it is.
        assert_eq!(pixel(&mut engine, "dst", 3, 0), colour(3, 0));
        // Interior, slope 3 with mask 128: round(3 * 128/255) = round(1.5059)
        // = 2, and slope 7: round(3.5137) = 4.
        assert_eq!(pixel(&mut engine, "dst", 1, 1), colour(3, 1));
        assert_eq!(pixel(&mut engine, "dst", 3, 1), colour(5, 1));
    }

    /// The reference's early returns (`main.cpp:718`, `:726-728`): a clip box
    /// it cannot cover with the map, and a box that starts outside the layer,
    /// write nothing at all — not even a new image.
    #[test]
    fn an_undersized_map_or_a_degenerate_clip_writes_nothing() {
        let mut engine = engine();
        engine
            .execute_script(
                "small.tjs",
                r#"
                global.small = new Layer();
                small.setImageSize(4, 4);
                small.fillRect(0, 0, 4, 4, 0xff000009);
                dst.callOnPaint = 0;
                "#,
            )
            .expect("small map");

        for call in [
            "dst.shimmer(src, small, void, 1, 1, 0, 0, 0, 0);", // 6x6 box, 4x4 map
            "dst.shimmer(src, map, void, 1, 1, 6, 0, 0, 0);",   // clipx >= _width
            "dst.shimmer(src, map, void, 1, 1, 0, 6, 0, 0);",   // clipy >= _height
            "dst.shimmer(src, map, void, 1, 1, 0, 0, -1, 0);",  // negative width
        ] {
            let before = generation(&mut engine, "dst");
            engine.execute_script("noop.tjs", call).expect(call);
            assert_eq!(pixel(&mut engine, "dst", 0, 0), 0, "{call}");
            assert_eq!(
                generation(&mut engine, "dst"),
                before,
                "{call} must not replace the image"
            );
            assert_eq!(integer(&mut engine, "dst.callOnPaint"), 0, "{call}");
        }

        // An over-large clip height is clamped to the layer, not rejected
        // (`main.cpp:720-723`), so this one does write.
        engine
            .execute_script(
                "clamped.tjs",
                "dst.shimmer(src, map, void, 1, 1, 0, 0, 0, 99);",
            )
            .expect("clamped box");
        assert_eq!(pixel(&mut engine, "dst", 1, 0), colour(4, 0));
    }

    /// Errors are the engine's, not a crash: a non-layer argument, a freed
    /// image, and a `void`-less mask slot.
    #[test]
    fn shimmer_reports_bad_arguments_instead_of_crashing() {
        let mut engine = engine();
        for (call, message) in [
            (
                "dst.shimmer(42, map, void, 1, 1, 0, 0, 0, 0);",
                "shimmer: srclayer must be Layer.",
            ),
            (
                "dst.shimmer(src, 42, void, 1, 1, 0, 0, 0, 0);",
                "shimmer: maplayer must be Layer.",
            ),
            (
                "dst.shimmer(src, map, 42, 1, 1, 0, 0, 0, 0);",
                "shimmer: msklayer must be Layer.",
            ),
            (
                "dst.shimmer(src, map, null, 1, 1, 0, 0, 0, 0);",
                "shimmer: msklayer must be Layer.",
            ),
        ] {
            let error = engine.execute_script("bad.tjs", call).expect_err(call);
            assert_eq!(error.message, message, "{call}");
        }

        engine
            .execute_script("free.tjs", "src.freeImage();")
            .expect("freeImage");
        let error = engine
            .execute_script("bad.tjs", "dst.shimmer(src, map, void, 1, 1, 0, 0, 0, 0);")
            .expect_err("a freed source");
        assert_eq!(error.message, "Not drawable layer type");
        assert_eq!(
            pixel(&mut engine, "dst", 0, 0),
            0,
            "the failed call wrote nothing"
        );

        engine
            .execute_script("free.tjs", "dst.freeImage();")
            .expect("freeImage");
        let error = engine
            .execute_script("bad.tjs", "dst.shimmer(src, map, void, 1, 1, 0, 0, 0, 0);")
            .expect_err("a freed destination");
        assert_eq!(error.message, "Not drawable layer type");
    }

    /// `shimmerBuildMap` (`main.cpp:977-1094`): every pixel of *this* layer —
    /// the clip box is not consulted — becomes the wrapped sample of the map,
    /// whole 32-bit pixels included (the shipped path's `movdqu`/`movd`).
    #[test]
    fn shimmer_build_map_tiles_a_wrapped_map_copy() {
        let mut engine = engine();
        engine
            .execute_script(
                "build.tjs",
                r#"
                global.tile = new Layer();
                tile.setImageSize(3, 2);
                for (var y = 0; y < 2; y++) {
                    for (var x = 0; x < 3; x++) {
                        tile.fillRect(x, y, 1, 1, 0x80000000 | (y * 3 + x + 1));
                    }
                }
                dst.setClip(1, 1, 2, 2);            // ignored by the build
                dst.shimmerBuildMap(tile, 1, 1, void, 0, 0);
                "#,
            )
            .expect("build map");

        // `ZERO2MAX(map1x, 3)` = 1 and `ZERO2MAX(map1y, 2)` = 1, so column 0
        // of the destination reads column `(0-1) mod 3 = 2`, row `(0-1) mod 2 =
        // 1`, i.e. map pixel 6; the pattern repeats with period 3 across and 2
        // down, over the whole 6x6 image and alpha included.
        let expected = [[6, 4, 5, 6, 4, 5], [3, 1, 2, 3, 1, 2]];
        for y in 0..6i64 {
            for x in 0..6i64 {
                assert_eq!(
                    pixel(&mut engine, "dst", x, y),
                    expected[(y % 2) as usize][(x % 3) as usize],
                    "dst({x}, {y})"
                );
                assert_eq!(alpha(&mut engine, "dst", x, y), 0x80, "dst({x}, {y})");
            }
        }

        // The one-map path copies the whole pixel; the scalar branch would
        // write only the blue byte.
        engine
            .execute_script("void.tjs", "dst.shimmerBuildMap(tile, 0, 0, void, 0, 0);")
            .expect("single-map build");
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0x000001);
        assert_eq!(alpha(&mut engine, "dst", 0, 0), 0x80);
        assert_eq!(pixel(&mut engine, "dst", 5, 5), 0x000006);
    }

    /// The two-map build (`main.cpp:1098-1340`): the shipped `pavgb` average
    /// per channel — `(a + b + 1) / 2`, alpha included — not the scalar
    /// branch's blue-grey with a forced opaque alpha.
    #[test]
    fn shimmer_build_map_averages_two_maps_per_channel() {
        let mut engine = engine();
        engine
            .execute_script(
                "averages.tjs",
                r#"
                global.first = new Layer();
                first.setImageSize(1, 1);
                first.fillRect(0, 0, 1, 1, 0xff020304);
                global.second = new Layer();
                second.setImageSize(1, 1);
                second.fillRect(0, 0, 1, 1, 0xfe040608);
                dst.shimmerBuildMap(first, 0, 0, second, 0, 0);
                "#,
            )
            .expect("two-map build");
        // (255+254+1)/2 = 255, (2+4+1)/2 = 3, (3+6+1)/2 = 5, (4+8+1)/2 = 6.
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0x030506);
        assert_eq!(alpha(&mut engine, "dst", 0, 0), 0xff);
        assert_eq!(pixel(&mut engine, "dst", 5, 5), 0x030506);

        // An odd sum rounds up: (4 + 9 + 1) / 2 = 7, where the scalar branch's
        // truncating (b1 + b2) / 2 answers 6.
        engine
            .execute_script(
                "odd.tjs",
                "first.fillRect(0, 0, 1, 1, 0xff000004); \
                 second.fillRect(0, 0, 1, 1, 0xff000009); \
                 dst.shimmerBuildMap(first, 0, 0, second, 0, 0);",
            )
            .expect("odd average");
        assert_eq!(pixel(&mut engine, "dst", 0, 0), 0x000007);
        assert_eq!(alpha(&mut engine, "dst", 0, 0), 0xff);
    }

    #[test]
    fn shimmer_build_map_reports_bad_arguments_instead_of_crashing() {
        let mut engine = engine();
        for (call, message) in [
            (
                "dst.shimmerBuildMap(42, 0, 0, void, 0, 0);",
                "shimmerBuildMap: maplayer1 must be Layer.",
            ),
            (
                "dst.shimmerBuildMap(map, 0, 0, 42, 0, 0);",
                "shimmerBuildMap: maplayer2 must be Layer.",
            ),
            (
                "dst.shimmerBuildMap(map, 0, 0, null, 0, 0);",
                "shimmerBuildMap: maplayer2 must be Layer.",
            ),
        ] {
            let error = engine.execute_script("bad.tjs", call).expect_err(call);
            assert_eq!(error.message, message, "{call}");
        }

        engine
            .execute_script("free.tjs", "map.freeImage();")
            .expect("freeImage");
        let error = engine
            .execute_script("bad.tjs", "dst.shimmerBuildMap(map, 0, 0);")
            .expect_err("a short argument list");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);

        let error = engine
            .execute_script("bad.tjs", "dst.shimmerBuildMap(map, 0, 0, void, 0, 0);")
            .expect_err("a freed map");
        assert_eq!(error.message, "Not drawable layer type");
    }

    /// The reference's arithmetic, each branch with the value only that branch
    /// answers: the frame blocks floor their fixed-point product, the shipped
    /// interior rounds to even, and out-of-range inputs give x86's integer
    /// indefinite `INT_MIN`.
    #[test]
    fn the_frame_and_interior_offsets_follow_the_shipped_build() {
        assert_eq!(fixed_scale(1.0), 65536);
        assert_eq!(fixed_scale(0.5), 32768);
        assert_eq!(fixed_scale(-1.5), -98304);
        assert_eq!(
            fixed_scale(1.0e6),
            i32::MIN,
            "out of range is cvttss2si's INT_MIN"
        );

        // (3 * 32768) >> 16 = 1 and (7 * 32768) >> 16 = 3, where the interior's
        // round-to-nearest answers 2 and 4 — the two frames' shapes in one
        // call.
        assert_eq!(frame_offset(3, 32768, None), 1);
        assert_eq!(frame_offset(7, 32768, None), 3);
        assert_eq!(interior_offset(3, 0.5, None), 2);
        assert_eq!(interior_offset(7, 0.5, None), 4);
        // A negative product floors (`>>` on a signed integer): (-3 * 32768)
        // >> 16 = -2, and the interior's -1.5 rounds to -2 as well.
        assert_eq!(frame_offset(-3, 32768, None), -2);
        assert_eq!(interior_offset(-3, 0.5, None), -2);
        // Ties to even: 0.5 -> 0, 1.5 -> 2, 2.5 -> 2, -0.5 -> 0.
        assert_eq!(interior_offset(1, 0.5, None), 0);
        assert_eq!(interior_offset(5, 0.5, None), 2);
        assert_eq!(interior_offset(-1, 0.5, None), 0);
        // The mask multiplies: the frame's ((3*65536)*128/255)>>16 = 98689>>16
        // = 1 against the interior's round(3 * 128/255) = 2.
        assert_eq!(frame_offset(3, 65536, Some(128)), 1);
        assert_eq!(interior_offset(3, 1.0, Some(128)), 2);
        // NaN and infinity reach INT_MIN through both branches.
        assert_eq!(interior_offset(3, f32::NAN, None), i32::MIN);
        assert_eq!(interior_offset(3, f32::INFINITY, None), i32::MIN);
    }

    /// A wild scale factor is the reference's 32-bit overflow; the port must
    /// stay deterministic and inside the image rather than panic.
    #[test]
    fn an_out_of_range_scale_factor_writes_within_the_image() {
        let mut engine = engine();
        engine
            .execute_script(
                "wild.tjs",
                "dst.shimmer(src, map, void, 100000000, 100000000, 0, 0, 0, 0);",
            )
            .expect("a wild scale");
        for y in 0..6i64 {
            for x in 0..6i64 {
                let value = pixel(&mut engine, "dst", x, y);
                assert!(
                    value == 0 || (0x010000..=0x240000).contains(&value),
                    "{value:#x}"
                );
            }
        }
    }

    /// This module registers two members *on top of* the engine's layer
    /// surface, so the family's shared base members keep answering through the
    /// same instance, a sibling family member registered on the same class
    /// object still answers, and a script-owned member is never clobbered.
    #[test]
    fn the_family_base_surface_still_answers_through_this_instance() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(crate::layer_ex_raster::LayerExRasterPlugin)
            .expect("sibling plugin");
        engine
            .register_plugin(LayerExShimmerPlugin)
            .expect("plugin");
        engine.execute_script("setup.tjs", SETUP).expect("setup");

        let layer = layer_class(&engine);
        for name in ["shimmer", "shimmerBuildMap", "copyRaster"] {
            assert!(
                is_callable_member(&engine, layer, name),
                "Layer.{name} is registered"
            );
        }

        // The base contract `layerExBase.hpp:105-110` reads through `this`:
        // the image size and the clip box are still properties of the instance
        // the plugin wrote through.
        engine
            .execute_script(
                "base.tjs",
                "dst.setClip(1, 1, 3, 3); dst.shimmer(src, map, void, 1, 1, 0, 0, 0, 0);",
            )
            .expect("clipped shimmer");
        assert_eq!(integer(&mut engine, "dst.imageWidth"), 6);
        assert_eq!(integer(&mut engine, "dst.clipWidth"), 3);

        // A member the script already owns keeps answering.
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "pre.tjs",
                "global.called = 0; Layer.shimmer = function () { global.called = 1; };",
            )
            .expect("script member");
        engine
            .register_plugin(LayerExShimmerPlugin)
            .expect("plugin");
        engine
            .execute_script("call.tjs", "global.layer = new Layer(); layer.shimmer();")
            .expect("call");
        assert_eq!(integer(&mut engine, "called"), 1);
    }
}
