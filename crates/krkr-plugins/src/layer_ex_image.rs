//! `layerExImage.dll`: per-layer image filters.
//!
//! Real plugin: `light`, `colorize`, `modulate`, `noise`,
//! `generateWhiteNoise`, `gaussianBlur` (`layerExImage/LayerExImage.cpp`,
//! <https://github.com/wtnbgo/layerExImage>), attached to the global `Layer`
//! class with a per-call instance hook (`layerExImage/Main.cpp:15-41`).
//!
//! Every member operates on the layer's **clip box**: the reference's
//! `reset()` moves `_buffer` to `clipTop * pitch + clipLeft * 4` and sets
//! `_width`/`_height` to the clip size (`LayerExImage.cpp:17-25`), so a filter
//! touches the clipped rectangle and nothing else. `_pitch` stays the image
//! row pitch, which the two-pass blur needs for its column walk (`:658-664`).
//!
//! Byte order: the reference's `p[0]` is blue and `p[2]` is red
//! (`LayerExImage.cpp:34-37`), the engine's view is R,G,B,A — every channel
//! access below names the reference's channel and indexes the view's byte.
//! Alpha is left alone by `light`, `colorize`, `modulate` and
//! `generateWhiteNoise`; `noise` and `gaussianBlur` treat it as the fourth
//! channel exactly as the reference does (`:349-355`, `:652-663`).
//!
//! Two reference behaviours are engine-visible:
//!
//! * `lut` is declared but never registered (`LayerExImage.h:21`), so it stays
//!   an internal helper here.
//! * `noise`/`generateWhiteNoise` call the CRT `rand()` (`:349, :372`).
//!   krkrz never calls `srand`, so the stream starts at the CRT's default seed
//!   and is deterministic; [`crt_rand`] reproduces MSVC's LCG so the sequence
//!   matches the reference's on the platform the plugin ships for.

use std::cell::Cell;

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{layer_bitmap_write, layer_update},
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer image filters (light, colorize, modulate, noise, generateWhiteNoise, gaussianBlur)",
    notes: "All six registered members ported from layerExImage/LayerExImage.cpp:48-672 over the layer bitmap view, each confined to the clip box. `lut` is unregistered in the reference and stays internal.",
    install: |engine| engine.register_plugin(LayerExImagePlugin),
};

pub struct LayerExImagePlugin;

impl KrkrPlugin for LayerExImagePlugin {
    fn name(&self) -> &str {
        "layerExImage.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `NCB_METHOD(<name>)` for each member (`Main.cpp:34-41`), with the
        // reference's declared arity (`LayerExImage.h:21-61`).
        register_unless_closure(
            runtime,
            layer,
            "light",
            NativeArgCount::AtLeast(2),
            layer_light,
        );
        register_unless_closure(
            runtime,
            layer,
            "colorize",
            NativeArgCount::AtLeast(3),
            layer_colorize,
        );
        register_unless_closure(
            runtime,
            layer,
            "modulate",
            NativeArgCount::AtLeast(3),
            layer_modulate,
        );
        register_unless_closure(
            runtime,
            layer,
            "noise",
            NativeArgCount::AtLeast(1),
            layer_noise,
        );
        register_unless_closure(
            runtime,
            layer,
            "generateWhiteNoise",
            NativeArgCount::Any,
            layer_generate_white_noise,
        );
        register_unless_closure(
            runtime,
            layer,
            "gaussianBlur",
            NativeArgCount::AtLeast(1),
            layer_gaussian_blur,
        );
        Ok(())
    }
}

/// The clip box every filter works in, plus the image row pitch its column walk
/// needs (`LayerExImage.cpp:17-25`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClipBox {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    pitch: usize,
}

fn clip_box(bitmap: krkr_engine::plugin_api::layer::LayerBitmap) -> ClipBox {
    let (left, top, width, height) = bitmap.clip;
    let left = left.clamp(0, i64::from(bitmap.width));
    let top = top.clamp(0, i64::from(bitmap.height));
    let width = width.clamp(0, i64::from(bitmap.width) - left);
    let height = height.clamp(0, i64::from(bitmap.height) - top);
    ClipBox {
        left: left as usize,
        top: top as usize,
        width: width as usize,
        height: height as usize,
        pitch: bitmap.pitch as usize,
    }
}

impl ClipBox {
    /// Byte offset of the clip's pixel `(x, y)`.
    fn offset(&self, x: usize, y: usize) -> usize {
        (self.top + y) * self.pitch + (self.left + x) * 4
    }

    /// Applies `f` to every pixel's four bytes in the clip box.
    fn for_each_pixel(&self, pixels: &mut [u8], mut f: impl FnMut(&mut [u8; 4])) {
        let stride = self.pitch;
        for y in 0..self.height {
            for x in 0..self.width {
                let offset = (self.top + y) * stride + (self.left + x) * 4;
                let Some(pixel) = pixels.get_mut(offset..offset + 4) else {
                    continue;
                };
                f(pixel.try_into().expect("four bytes"));
            }
        }
    }
}

// ------------------------------------------------------------------- light

/// `layerExImage::lut` (`LayerExImage.cpp:27-41`): the table over the three
/// colour bytes, alpha untouched.
fn lut(pixels: &mut [u8], clip: ClipBox, table: &[u8; 256]) {
    clip.for_each_pixel(pixels, |pixel| {
        pixel[0] = table[pixel[0] as usize];
        pixel[1] = table[pixel[1] as usize];
        pixel[2] = table[pixel[2] as usize];
    });
}

/// `LayerExImage.cpp:48-59`: brightness and contrast through a 256-entry table.
fn layer_light(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let brightness = arg_integer(&args, 0)? as i32 + 128;
    let contrast = arg_integer(&args, 1)? as i32;
    let c = (100 + contrast) as f32 / 100.0;
    let mut table = [0u8; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        // `(BYTE)max(0, min(255, (int)((i-128)*c + brightness)))`
        // (`LayerExImage.cpp:55`).
        let value = (i as f32 - 128.0) * c + brightness as f32;
        *slot = (value as i32).clamp(0, 255) as u8;
    }

    layer_bitmap_write(runtime, layer, |view| {
        let clip = clip_box(view.bitmap);
        lut(view.pixels, clip, &table);
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

// -------------------------------------------------- RGB <-> HSL (CxImage)

/// `RGBtoHSL` (`LayerExImage.cpp:73-114`). The reference carries its result in
/// an `RGBQUAD` whose bytes are L, S, H.
fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    const HSL_MAX: u32 = 255;
    const RGB_MAX: u32 = 255;
    const HSL_UNDEFINED: u32 = HSL_MAX * 2 / 3;

    let (r, g, b) = (u32::from(r), u32::from(g), u32::from(b));
    let c_max = r.max(g).max(b);
    let c_min = r.min(g).min(b);
    let l = (((c_max + c_min) * HSL_MAX) + RGB_MAX) / (2 * RGB_MAX);
    if c_max == c_min {
        return (HSL_UNDEFINED as u8, 0, l as u8);
    }
    let s = if l <= HSL_MAX / 2 {
        (((c_max - c_min) * HSL_MAX) + ((c_max + c_min) / 2)) / (c_max + c_min)
    } else {
        (((c_max - c_min) * HSL_MAX) + ((2 * RGB_MAX - c_max - c_min) / 2))
            / (2 * RGB_MAX - c_max - c_min)
    };
    let r_delta = (((c_max - r) * (HSL_MAX / 6)) + ((c_max - c_min) / 2)) / (c_max - c_min);
    let g_delta = (((c_max - g) * (HSL_MAX / 6)) + ((c_max - c_min) / 2)) / (c_max - c_min);
    let b_delta = (((c_max - b) * (HSL_MAX / 6)) + ((c_max - c_min) / 2)) / (c_max - c_min);
    let mut h = if r == c_max {
        b_delta.wrapping_sub(g_delta)
    } else if g == c_max {
        (HSL_MAX / 3 + r_delta).wrapping_sub(b_delta)
    } else {
        ((2 * HSL_MAX) / 3 + g_delta).wrapping_sub(r_delta)
    };
    if h > HSL_MAX {
        h -= HSL_MAX;
    }
    (h as u8, s as u8, l as u8)
}

/// `HueToRGB` (`LayerExImage.cpp:116-137`).
fn hue_to_rgb(n1: f32, n2: f32, hue: f32) -> f32 {
    let hue = if hue > 360.0 {
        hue - 360.0
    } else if hue < 0.0 {
        hue + 360.0
    } else {
        hue
    };
    if hue < 60.0 {
        n1 + (n2 - n1) * hue / 60.0
    } else if hue < 180.0 {
        n2
    } else if hue < 240.0 {
        n1 + (n2 - n1) * (240.0 - hue) / 60.0
    } else {
        n1
    }
}

/// `HSLtoRGB` (`LayerExImage.cpp:139-166`).
fn hsl_to_rgb(h: u8, s: u8, l: u8) -> (u8, u8, u8) {
    let h = f32::from(h) * 360.0 / 255.0;
    let s = f32::from(s) / 255.0;
    let l = f32::from(l) / 255.0;
    let m2 = if l <= 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let m1 = 2.0 * l - m2;
    if s == 0.0 {
        let grey = (l * 255.0) as u8;
        return (grey, grey, grey);
    }
    (
        (hue_to_rgb(m1, m2, h + 120.0) * 255.0) as u8,
        (hue_to_rgb(m1, m2, h) * 255.0) as u8,
        (hue_to_rgb(m1, m2, h - 120.0) * 255.0) as u8,
    )
}

/// `LayerExImage.cpp:174-220`: force hue and saturation, blended with the
/// original by `blend` (`(new*a0 + old*a1) >> 8`).
fn layer_colorize(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let hue = arg_integer(&args, 0)? as u8;
    let sat = arg_integer(&args, 1)? as u8;
    let blend = arg_real(&args, 2)?.clamp(0.0, 1.0);
    let a0 = (256.0 * blend) as i32;
    let a1 = 256 - a0;
    let full_blend = blend > 0.999;

    layer_bitmap_write(runtime, layer, |view| {
        let clip = clip_box(view.bitmap);
        clip.for_each_pixel(view.pixels, |pixel| {
            let (r, g, b) = (
                i32::from(pixel[0]),
                i32::from(pixel[1]),
                i32::from(pixel[2]),
            );
            // Both branches run `RGBtoHSL` and force hue/saturation before
            // `HSLtoRGB` (`LayerExImage.cpp:194-211`); only the blend differs.
            let (_, _, l) = rgb_to_hsl(pixel[0], pixel[1], pixel[2]);
            let (new_r, new_g, new_b) = hsl_to_rgb(hue, sat, l);
            if full_blend {
                pixel[0] = new_r;
                pixel[1] = new_g;
                pixel[2] = new_b;
            } else {
                let mix = |new: u8, old: i32| (((new as i32 * a0 + old * a1) >> 8) & 0xff) as u8;
                pixel[0] = mix(new_r, r);
                pixel[1] = mix(new_g, g);
                pixel[2] = mix(new_b, b);
            }
        });
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

// --------------------------------------------------------------- modulate

/// `hue2rgb` (`LayerExImage.cpp:222-237`), the normalized-HSL helper.
fn hue2rgb(n1: f64, n2: f64, hue: f64) -> i32 {
    let hue = if hue < 0.0 {
        hue + 1.0
    } else if hue > 1.0 {
        hue - 1.0
    } else {
        hue
    };
    let color = if hue < 1.0 / 6.0 {
        n1 + (n2 - n1) * hue * 6.0
    } else if hue < 1.0 / 2.0 {
        n2
    } else if hue < 2.0 / 3.0 {
        n1 + (n2 - n1) * (2.0 / 3.0 - hue) * 6.0
    } else {
        n1
    };
    (color * 255.0) as i32
}

/// `modulate(b, g, r, h, s, l)` (`LayerExImage.cpp:239-304`): an HSL shift with
/// `h` in turns, `s`/`l` in -1..1.
fn modulate_pixel(b: u8, g: u8, r: u8, h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let red = f64::from(r) / 255.0;
    let green = f64::from(g) / 255.0;
    let blue = f64::from(b) / 255.0;

    let c_max = red.max(green).max(blue);
    let c_min = red.min(green).min(blue);
    let delta = c_max - c_min;
    let add = c_max + c_min;
    let mut luminance = add / 2.0;
    let mut hue;
    let mut saturation;
    if delta == 0.0 {
        saturation = 0.0;
        hue = 0.0;
    } else {
        saturation = if luminance < 0.5 {
            delta / add
        } else {
            delta / (2.0 - add)
        };
        hue = if red == c_max {
            (green - blue) / delta
        } else if green == c_max {
            2.0 + (blue - red) / delta
        } else {
            4.0 + (red - green) / delta
        };
        hue /= 6.0;
    }
    hue += h;
    while hue < 0.0 {
        hue += 1.0;
    }
    while hue > 1.0 {
        hue -= 1.0;
    }
    if s > 0.0 {
        saturation += (1.0 - saturation) * s;
    } else {
        saturation += saturation * s;
    }
    if l > 0.0 {
        luminance += (1.0 - luminance) * l;
    } else {
        luminance += luminance * l;
    }

    if saturation == 0.0 {
        let grey = (luminance * 255.0) as u8;
        return (grey, grey, grey);
    }
    let m2 = if luminance <= 0.5 {
        luminance * (1.0 + saturation)
    } else {
        luminance + saturation - luminance * saturation
    };
    let m1 = 2.0 * luminance - m2;
    (
        hue2rgb(m1, m2, hue - 1.0 / 3.0) as u8,
        hue2rgb(m1, m2, hue) as u8,
        hue2rgb(m1, m2, hue + 1.0 / 3.0) as u8,
    )
}

/// `LayerExImage.cpp:313-336`.
fn layer_modulate(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let h = arg_integer(&args, 0)? as f64 / 360.0;
    let s = arg_integer(&args, 1)? as f64 / 100.0;
    let l = arg_integer(&args, 2)? as f64 / 100.0;

    layer_bitmap_write(runtime, layer, |view| {
        let clip = clip_box(view.bitmap);
        clip.for_each_pixel(view.pixels, |pixel| {
            let (b, g, r) = modulate_pixel(pixel[2], pixel[1], pixel[0], h, s, l);
            pixel[0] = r;
            pixel[1] = g;
            pixel[2] = b;
        });
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

// ------------------------------------------------------------------- noise

/// The CRT's `RAND_MAX` (`LayerExImage.cpp:349`).
const RAND_MAX: f64 = 32767.0;

thread_local! {
    /// The reference's `rand()` state (`LayerExImage.cpp:349, 372`). krkrz never
    /// seeds it, so the stream starts at the CRT's default seed 1.
    static NOISE_STATE: Cell<u32> = const { Cell::new(1) };
}

/// MSVC's `rand()`: `state = state * 214013 + 2531011`, `(state >> 16) & 0x7fff`.
/// Reproducing it keeps `noise`/`generateWhiteNoise` deterministic here and on
/// the reference's own sequence.
fn crt_rand() -> f64 {
    NOISE_STATE.with(|state| {
        let next = state.get().wrapping_mul(214013).wrapping_add(2531011);
        state.set(next);
        f64::from((next >> 16) & 0x7fff)
    })
}

/// `LayerExImage.cpp:342-360`: per-channel noise, one `rand()` per byte.
fn layer_noise(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let level = arg_integer(&args, 0)? as f64;

    layer_bitmap_write(runtime, layer, |view| {
        let clip = clip_box(view.bitmap);
        clip.for_each_pixel(view.pixels, |pixel| {
            // One `rand()` per colour byte, in the reference's order
            // (`LayerExImage.cpp:349-354`); alpha keeps its value.
            for channel in pixel.iter_mut().take(3) {
                let n = ((crt_rand() / RAND_MAX - 0.5) * level) as i32;
                *channel = (i32::from(*channel) + n).clamp(0, 255) as u8;
            }
        });
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

/// `LayerExImage.cpp:365-378`: grey white noise over the colour bytes, alpha
/// preserved.
fn layer_generate_white_noise(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    layer_bitmap_write(runtime, layer, |view| {
        let clip = clip_box(view.bitmap);
        clip.for_each_pixel(view.pixels, |pixel| {
            // `(BYTE)(rand()/(RAND_MAX/255))` with an integer division: 32767/255
            // is 128.
            let n = (crt_rand() / 128.0) as u8;
            pixel[0] = n;
            pixel[1] = n;
            pixel[2] = n;
        });
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

// ------------------------------------------------------------ gaussianBlur

/// `gen_convolve_matrix` (`LayerExImage.cpp:390-465`): a normalized 1-D
/// gaussian kernel of length `2*ceil(2*sigma - 0.5) + 1`.
fn gen_convolve_matrix(radius: f32) -> Vec<f32> {
    // `radius = (float)fabs(0.5*radius) + 0.25f` — the radius parameter is the
    // standard deviation, the radius of effect twice that (`:400-418`).
    let radius = (0.5 * radius).abs() + 0.25;
    let std_dev = radius;
    let radius = std_dev * 2.0;
    let mut matrix_length = (2.0 * (radius - 0.5).ceil() + 1.0) as i32;
    if matrix_length <= 0 {
        matrix_length = 1;
    }
    let length = matrix_length as usize;
    let mut cmatrix = vec![0.0f32; length];

    for i in (matrix_length / 2 + 1)..matrix_length {
        let base_x = i as f32 - (matrix_length / 2) as f32 - 0.5;
        let mut sum = 0.0f32;
        for j in 1..=50 {
            // `0.02` is a double literal, so the C++ expression evaluates in
            // double before the cast back to float (`:437-439`).
            let x = f64::from(base_x) + 0.02 * f64::from(j);
            if x <= f64::from(radius) {
                sum += (-(x * x) / f64::from(2.0 * std_dev * std_dev)).exp() as f32;
            }
        }
        cmatrix[i as usize] = sum / 50.0;
    }
    for i in 0..=matrix_length / 2 {
        cmatrix[i as usize] = cmatrix[(matrix_length - 1 - i) as usize];
    }

    let mut sum = 0.0f32;
    for j in 0..=50 {
        let x = 0.5 + 0.02 * f64::from(j);
        sum += (-(x * x) / f64::from(2.0 * std_dev * std_dev)).exp() as f32;
    }
    cmatrix[(matrix_length / 2) as usize] = sum / 51.0;

    let total: f32 = cmatrix.iter().sum();
    for value in &mut cmatrix {
        *value /= total;
    }
    cmatrix
}

/// `gen_lookup_table` (`LayerExImage.cpp:475-492`): `cmatrix[i] * value`,
/// indexed by matrix position then by input byte.
fn gen_lookup_table(cmatrix: &[f32]) -> Vec<f32> {
    let mut table = Vec::with_capacity(cmatrix.len() * 256);
    for weight in cmatrix {
        for value in 0..256 {
            table.push(weight * value as f32);
        }
    }
    table
}

/// `blur_line` (`LayerExImage.cpp:500-604`): one pass over a line of `y`
/// pixels stored tightly as `bytes` channels each, edges scaled to one.
fn blur_line(
    ctable: &[f32],
    cmatrix: &[f32],
    y: i32,
    cur_col: &[u8],
    dest_col: &mut [u8],
    bytes: usize,
) {
    let cmatrix_length = cmatrix.len() as i32;
    let cmatrix_middle = cmatrix_length / 2;
    let channel = |pixel: i32, i: usize| (pixel * bytes as i32 + i as i32) as usize;

    if cmatrix_length > y {
        // Very small pictures: the straightforward scan with an explicit scale
        // (`:519-544`).
        for row in 0..y {
            let mut scale = 0.0f32;
            for j in 0..y {
                let index = j + cmatrix_middle - row;
                if index >= 0 && index < cmatrix_length {
                    scale += cmatrix[index as usize];
                }
            }
            for i in 0..bytes {
                let mut sum = 0.0f32;
                for j in 0..y {
                    if j >= row - cmatrix_middle && j <= row + cmatrix_middle {
                        sum += f32::from(cur_col[channel(j, i)]) * cmatrix[j as usize];
                    }
                }
                dest_col[channel(row, i)] = (0.5 + sum / scale) as u8;
            }
        }
        return;
    }

    let mut row = 0;
    while row < cmatrix_middle {
        let mut scale = 0.0f32;
        for j in (cmatrix_middle - row)..cmatrix_length {
            scale += cmatrix[j as usize];
        }
        for i in 0..bytes {
            let mut sum = 0.0f32;
            for j in (cmatrix_middle - row)..cmatrix_length {
                sum +=
                    f32::from(cur_col[channel(row + j - cmatrix_middle, i)]) * cmatrix[j as usize];
            }
            dest_col[channel(row, i)] = (0.5 + sum / scale) as u8;
        }
        row += 1;
    }
    while row < y - cmatrix_middle {
        let base = row - cmatrix_middle;
        for i in 0..bytes {
            let mut sum = 0.0f32;
            for j in 0..cmatrix_length {
                let value = cur_col[channel(base + j, i)] as usize;
                sum += ctable[j as usize * 256 + value];
            }
            dest_col[channel(row, i)] = (0.5 + sum) as u8;
        }
        row += 1;
    }
    while row < y {
        let last = y - row + cmatrix_middle;
        let mut scale = 0.0f32;
        for j in 0..last {
            scale += cmatrix[j as usize];
        }
        for i in 0..bytes {
            let mut sum = 0.0f32;
            for j in 0..last {
                sum +=
                    f32::from(cur_col[channel(row + j - cmatrix_middle, i)]) * cmatrix[j as usize];
            }
            dest_col[channel(row, i)] = (0.5 + sum / scale) as u8;
        }
        row += 1;
    }
}

/// `getCol` (`LayerExImage.cpp:607-618`): one column of a row-major plane into
/// a tight column buffer.
fn get_col(source: &[u8], column: usize, pitch: usize, height: usize, dest: &mut [u8]) {
    for y in 0..height {
        let offset = y * pitch + column * 4;
        if let (Some(src), Some(dst)) = (
            source.get(offset..offset + 4),
            dest.get_mut(y * 4..y * 4 + 4),
        ) {
            dst.copy_from_slice(src);
        }
    }
}

/// `setCol` (`LayerExImage.cpp:621-632`): the tight column back into the plane.
fn set_col(dest: &mut [u8], column: usize, pitch: usize, height: usize, source: &[u8]) {
    for y in 0..height {
        let offset = y * pitch + column * 4;
        if let (Some(src), Some(dst)) = (
            source.get(y * 4..y * 4 + 4),
            dest.get_mut(offset..offset + 4),
        ) {
            dst.copy_from_slice(src);
        }
    }
}

/// `layerExImage::gaussianBlur` (`LayerExImage.cpp:634-672`): a separable
/// convolution, rows into a tight temporary plane and then columns back.
fn layer_gaussian_blur(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let radius = arg_real(&args, 0)? as f32;

    layer_bitmap_write(runtime, layer, |view| {
        let clip = clip_box(view.bitmap);
        if clip.width == 0 || clip.height == 0 {
            return;
        }
        let cmatrix = gen_convolve_matrix(radius);
        let ctable = gen_lookup_table(&cmatrix);
        let tmp_pitch = clip.width * 4;

        // Blur the rows into the temporary plane (`:649-653`).
        let mut tmp = vec![0u8; tmp_pitch * clip.height];
        let mut line = vec![0u8; tmp_pitch];
        for y in 0..clip.height {
            let row = clip.offset(0, y);
            if let Some(source) = view.pixels.get(row..row + tmp_pitch) {
                line.copy_from_slice(source);
            }
            blur_line(
                &ctable,
                &cmatrix,
                clip.width as i32,
                &line,
                &mut tmp[y * tmp_pitch..(y + 1) * tmp_pitch],
                4,
            );
        }

        // Blur the columns straight back into the layer (`:655-664`).
        let mut cur_col = vec![0u8; clip.height * 4];
        let mut dest_col = vec![0u8; clip.height * 4];
        for x in 0..clip.width {
            get_col(&tmp, x, tmp_pitch, clip.height, &mut cur_col);
            blur_line(
                &ctable,
                &cmatrix,
                clip.height as i32,
                &cur_col,
                &mut dest_col,
                4,
            );
            set_col(
                view.pixels,
                clip.left + x,
                clip.pitch,
                clip.height,
                &dest_col,
            );
        }
    })?;
    layer_update(runtime, layer)?;
    Ok(Variant::Void)
}

// ------------------------------------------------------------------ helpers

fn this_layer(this_obj: Option<ObjectHandle>) -> Result<ObjectHandle> {
    this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))
}

fn arg_integer(args: &[Variant], index: usize) -> Result<i64> {
    args.get(index)
        .map(Variant::to_integer)
        .transpose()
        .map(|value| value.unwrap_or(0))
}

fn arg_real(args: &[Variant], index: usize) -> Result<f64> {
    args.get(index)
        .map(Variant::to_real)
        .transpose()
        .map(|value| value.unwrap_or(0.0))
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

    use super::LayerExImagePlugin;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(LayerExImagePlugin).expect("plugin");
        engine
    }

    /// `LayerExImage.cpp:48-59` with a zero contrast: the table is
    /// `clamp(channel + brightness)`, and only the clip box is touched.
    #[test]
    fn light_adds_brightness_inside_the_clip_box() {
        let mut engine = engine();
        engine
            .execute_script(
                "light.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 1);
                layer.fillRect(0, 0, 4, 1, 0x80404040);
                layer.setClip(0, 0, 2, 1);
                layer.light(10, 0);
                "#,
            )
            .expect("light");

        assert_eq!(main(&mut engine, 0, 0), 0x4a4a4a);
        assert_eq!(main(&mut engine, 1, 0), 0x4a4a4a);
        assert_eq!(main(&mut engine, 2, 0), 0x404040, "outside the clip box");
        assert_eq!(mask(&mut engine, 0, 0), 0x80, "alpha is untouched");
        assert_eq!(call_on_paint(&mut engine), 1);
    }

    /// The same table with contrast: `clamp((channel - 128) * c + b + 128)`
    /// (`LayerExImage.cpp:51-56`).
    #[test]
    fn light_scales_around_the_midpoint_with_contrast() {
        let mut engine = engine();
        engine
            .execute_script(
                "contrast.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(3, 1);
                layer.setMainPixel(0, 0, 0x000000);
                layer.setMainPixel(1, 0, 0x408080);
                layer.setMainPixel(2, 0, 0xc0ffff);
                layer.light(0, 100);
                "#,
            )
            .expect("light with contrast");

        // c = 2, brightness = 128: 0 -> -256 -> 0, 0x40 (64) -> 0,
        // 0x80 (128) -> 128, 0xc0 (192) -> 256 -> 255.
        assert_eq!(main(&mut engine, 0, 0), 0x000000);
        assert_eq!(main(&mut engine, 1, 0), (0x80 << 8) | 0x80);
        assert_eq!(main(&mut engine, 2, 0), 0xffffff);
    }

    /// `LayerExImage.cpp:174-220`: `blend` 0 keeps the pixel, and a full blend
    /// with saturation 0 greys it by its HSL lightness.
    #[test]
    fn colorize_blends_towards_the_forced_hue_and_saturation() {
        let mut engine = engine();
        engine
            .execute_script(
                "colorize.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(3, 1);
                layer.fillRect(0, 0, 3, 1, 0xffff0000);
                layer.setMainPixel(1, 0, 0x0000ff00);
                layer.setMainPixel(2, 0, 0x000000ff);
                "#,
            )
            .expect("layer");

        // blend = 0 -> `(new*0 + old*256) >> 8` is the original pixel.
        engine
            .execute_script("colorize.tjs", "layer.colorize(0, 0, 0);")
            .expect("colorize with no blend");
        assert_eq!(main(&mut engine, 0, 0), 0xff0000);

        // Full blend, saturation 0: red (L = 128) and blue (L = 128) grey.
        engine
            .execute_script("colorize.tjs", "layer.colorize(0, 0, 1);")
            .expect("colorize");
        assert_eq!(main(&mut engine, 0, 0), 0x808080);
        assert_eq!(main(&mut engine, 2, 0), 0x808080);
        assert_eq!(mask(&mut engine, 0, 0), 0xff, "alpha is untouched");
    }

    /// `LayerExImage.cpp:239-336`: `modulate` in HSL with the hue in degrees,
    /// saturation and luminance in percent.
    #[test]
    fn modulate_shifts_the_hsl_components() {
        let mut engine = engine();
        engine
            .execute_script(
                "modulate.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(2, 1);
                layer.setMainPixel(0, 0, 0x808080);
                layer.setMainPixel(1, 0, 0xff0000);
                layer.modulate(0, 0, 100);
                "#,
            )
            .expect("modulate");

        // Grey (delta 0) with luminance +100% is white; the red pixel keeps its
        // hue and becomes lighter.
        assert_eq!(main(&mut engine, 0, 0), 0xffffff);
        let red = main(&mut engine, 1, 0);
        assert!(red > 0xff0000, "red got lighter: {red:#x}");

        // Saturation -100% drains the colour, leaving the lightness: red's
        // luminance is 0.5, i.e. `(int)(0.5 * 255)` = 127.
        engine
            .execute_script(
                "modulate.tjs",
                "layer.setMainPixel(0, 0, 0xff0000); layer.setMainPixel(1, 0, 0xff0000); layer.modulate(0, -100, 0);",
            )
            .expect("modulate desaturate");
        assert_eq!(main(&mut engine, 1, 0), 0x7f7f7f);
    }

    /// `LayerExImage.cpp:342-360`: `level` 0 leaves the pixels alone, and a
    /// larger level changes them without touching alpha.
    #[test]
    fn noise_keeps_level_zero_and_skips_alpha() {
        let mut engine = engine();
        engine
            .execute_script(
                "noise.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 1);
                layer.fillRect(0, 0, 4, 1, 0x80808080);
                layer.noise(0);
                "#,
            )
            .expect("noise 0");
        assert_eq!(main(&mut engine, 0, 0), 0x808080);
        assert_eq!(mask(&mut engine, 0, 0), 0x80);

        engine
            .execute_script("noise.tjs", "layer.noise(255);")
            .expect("noise 255");
        assert_ne!(main(&mut engine, 0, 0), 0x808080, "the level changed it");
        assert_eq!(mask(&mut engine, 0, 0), 0x80, "alpha is untouched");
    }

    /// `LayerExImage.cpp:365-378`: grey noise over the colour bytes, alpha
    /// preserved.
    #[test]
    fn generate_white_noise_writes_grey_and_keeps_alpha() {
        let mut engine = engine();
        engine
            .execute_script(
                "white.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(8, 1);
                layer.fillRect(0, 0, 8, 1, 0x40123456);
                layer.generateWhiteNoise();
                "#,
            )
            .expect("generateWhiteNoise");

        let mut values = Vec::new();
        for x in 0..8 {
            let pixel = main(&mut engine, x, 0);
            let (r, g, b) = (pixel >> 16, (pixel >> 8) & 0xff, pixel & 0xff);
            assert_eq!((r, g, b), (r, r, r), "pixel {x} is grey");
            assert_eq!(mask(&mut engine, x, 0), 0x40, "alpha is untouched");
            values.push(r);
        }
        assert!(
            values.iter().any(|&value| value != values[0]),
            "the noise varies: {values:?}"
        );
    }

    /// `LayerExImage.cpp:411-464`: a radius of 0 gives a one-tap kernel, so the
    /// blur is the identity (`cmatrix = [1.0]`).
    #[test]
    fn gaussian_blur_with_radius_zero_is_the_identity() {
        let mut engine = engine();
        engine
            .execute_script(
                "blur.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 2);
                for (var y = 0; y < 2; y++) {
                    for (var x = 0; x < 4; x++) {
                        layer.fillRect(x, y, 1, 1, 0xff000000 | (0x10 * (y * 4 + x + 1) << 16));
                    }
                }
                layer.gaussianBlur(0);
                "#,
            )
            .expect("gaussianBlur(0)");
        for y in 0..2 {
            for x in 0..4 {
                let n = y * 4 + x + 1;
                assert_eq!(main(&mut engine, x, y), (0x10 * n) << 16, "({x}, {y})");
            }
        }
        assert_eq!(call_on_paint(&mut engine), 1);
    }

    /// `LayerExImage.cpp:634-672`: the row and column passes spread a bright
    /// dot symmetrically and keep a constant field constant.
    #[test]
    fn gaussian_blur_spreads_a_dot_and_keeps_a_flat_field() {
        let mut engine = engine();
        engine
            .execute_script(
                "blur.tjs",
                r#"
                global.flat = new Layer();
                flat.setImageSize(8, 8);
                flat.fillRect(0, 0, 8, 8, 0xff404040);
                flat.gaussianBlur(2);

                global.dot = new Layer();
                dot.setImageSize(9, 9);
                dot.fillRect(0, 0, 9, 9, 0xff000000);
                dot.setMainPixel(4, 4, 0xffffff);
                dot.gaussianBlur(2);
                "#,
            )
            .expect("gaussianBlur");

        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(main_of(&mut engine, x, y, "flat"), 0x404040, "({x}, {y})");
            }
        }
        // The reference's centre tap is only ~0.6% heavier than its ±1 taps
        // (its `find center val` averages the gaussian over 0.5..1.5,
        // `LayerExImage.cpp:449-457`), so after the row pass truncates to bytes
        // they come out equal and only the ±2 ring is dimmer.
        let centre = main_of(&mut engine, 4, 4, "dot");
        let near = main_of(&mut engine, 3, 4, "dot");
        let far = main_of(&mut engine, 2, 4, "dot");
        assert!(centre < 0xffffff, "the dot spread: {centre:#x}");
        assert!(
            centre >= near && near > far && far > 0,
            "the spread falls off: {centre:#x}/{near:#x}/{far:#x}"
        );
        assert_eq!(
            near,
            main_of(&mut engine, 5, 4, "dot"),
            "horizontal symmetry"
        );
        assert_eq!(
            main_of(&mut engine, 4, 3, "dot"),
            main_of(&mut engine, 4, 5, "dot"),
            "vertical symmetry"
        );
        assert_eq!(main_of(&mut engine, 0, 0, "dot"), 0, "corner untouched");
    }

    /// The filters are confined to the clip box (`LayerExImage.cpp:17-25`).
    #[test]
    fn a_filter_stays_inside_the_clip_box() {
        let mut engine = engine();
        engine
            .execute_script(
                "clip.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(4, 1);
                layer.fillRect(0, 0, 4, 1, 0x80000000);
                layer.setClip(1, 0, 2, 1);
                layer.generateWhiteNoise();
                layer.light(0, 0);
                "#,
            )
            .expect("filters in a clip box");
        assert_eq!(main(&mut engine, 0, 0), 0x000000, "outside the clip box");
        assert_eq!(main(&mut engine, 3, 0), 0x000000, "outside the clip box");
        assert_eq!(mask(&mut engine, 1, 0), 0x80, "alpha is untouched");
    }

    /// Every member needs a layer with an image; a freed one reports the
    /// engine's error.
    #[test]
    fn the_filters_report_a_layer_without_an_image() {
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
            "layer.light(10, 10);",
            "layer.colorize(0, 0, 1);",
            "layer.modulate(0, 0, 0);",
            "layer.noise(10);",
            "layer.generateWhiteNoise();",
            "layer.gaussianBlur(1);",
        ] {
            let error = engine
                .execute_script("free.tjs", call)
                .expect_err("a freed image");
            assert_eq!(error.message, "Not drawable layer type", "{call}");
        }
    }

    /// `gen_convolve_matrix` against an independent transcription of
    /// `LayerExImage.cpp:390-465` (integrating `e^-(x^2/2s^2)` over each tap's
    /// unit interval, mirroring the top half, then overriding the centre with
    /// its own 51-sample average). The centre tap comes out only ~0.6% heavier
    /// than its neighbours, which is why the blur above looks so flat.
    #[test]
    fn the_gaussian_kernel_matches_the_reference_integration() {
        let kernel = super::gen_convolve_matrix(2.0);
        assert_eq!(kernel.len(), 5, "2*ceil(2*sigma - 0.5) + 1");
        let expected = [0.1051, 0.2628, 0.2643, 0.2628, 0.1051];
        for (value, expected) in kernel.iter().zip(expected) {
            assert!(
                (value - expected).abs() < 5e-5,
                "kernel {kernel:?} vs {expected}"
            );
        }
        assert!(
            (kernel.iter().sum::<f32>() - 1.0).abs() < 1e-6,
            "normalized"
        );

        // A radius below the first quantum is a single tap, i.e. the identity.
        assert_eq!(super::gen_convolve_matrix(0.0), vec![1.0]);

        // `gen_lookup_table` (`:475-492`) is `cmatrix[i] * value`.
        let table = super::gen_lookup_table(&kernel);
        assert_eq!(table.len(), kernel.len() * 256);
        assert!((table[256 + 200] - kernel[1] * 200.0).abs() < 1e-6);
    }

    /// `layer.getMainPixel`, and the same for a named layer.
    fn main(engine: &mut KrkrEngine, x: i64, y: i64) -> i64 {
        main_of(engine, x, y, "layer")
    }

    fn main_of(engine: &mut KrkrEngine, x: i64, y: i64, layer: &str) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("{layer}.getMainPixel({x}, {y})"))
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

    fn call_on_paint(engine: &mut KrkrEngine) -> i64 {
        engine
            .execute_expression("read.tjs", "layer.callOnPaint")
            .expect("callOnPaint")
            .to_integer()
            .expect("integer")
    }
}
