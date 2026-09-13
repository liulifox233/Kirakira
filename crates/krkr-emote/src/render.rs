//! Software rendering of a sampled draw list into an RGBA canvas.
//!
//! The plugin-facing surface hands a player's `draw(layer)` the layer's main
//! bitmap; this module is what turns the eluna draw list into pixels for it,
//! so the rasterisation stays unit-testable without an engine.
//!
//! # Geometry
//!
//! A draw item is a textured quad. Its four corners are the sprite rectangle
//! (`center` ± `size` / 2) transformed the way eluna's own
//! `transform_emote_sprite_point` (`vendor/eluna/crates/eluna/src/emote.rs:776-791`)
//! defines the model: scale and rotation about `center` first, then the
//! sprite's `world_transform` (the accumulated layer transforms). The item's
//! `uv` rectangle is bilinearly sampled across the quad, so a sprite that
//! covers a sub-rectangle of its texture (FreeMote's icons) and one that owns
//! its texture (PARQUET's synthetic per-icon textures) both land correctly.
//!
//! # Alpha
//!
//! The canvas holds straight (non-premultiplied) R, G, B, A bytes — the
//! engine's own layer store — and items are composited source-over:
//! `dst = src * a + dst * (1 - a)` with `a = pixel_alpha * item.opacity *
//! tint.alpha`. `item.opacity` is the sprite's own opacity, which is where the
//! motion's `opa` value ends up (eluna divides the file's byte by 255; the
//! adapter passes the field through).
//!
//! # Not yet rendered
//!
//! Three parts of the reference's renderer are missing and are *counted*, not
//! silently dropped: mesh patches (a `MeshTransform`/`BezierPatch` item draws
//! as its plain quad), passes other than [`EmoteDrawPass::Normal`] (mask and
//! stencil-composite passes need a stencil buffer the layer path does not
//! have), and the reference's per-blit interpolation types (this sampler is
//! always bilinear). See [`RenderReport`].

use std::collections::BTreeMap;
use std::sync::Arc;

use eluna::EmoteDrawPass;

use crate::decode::DecodedTexture;
use crate::model::MotionDrawItem;
use crate::motion::Motion;

/// A tightly packed straight-alpha RGBA8 canvas, top-down.
///
/// This is the engine's layer bitmap layout (`krkr-engine`'s
/// `plugin_api::layer` views), so a plugin wraps the layer's pixels in a
/// [`Canvas`], renders into it, and writes the result back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    /// A transparent canvas of the given size.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    /// Wraps existing RGBA bytes; `None` when they do not fill `width x height`.
    pub fn from_pixels(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        (pixels.len() == width as usize * height as usize * 4).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /// One pixel, or `None` outside the canvas.
    pub fn pixel(&self, x: i32, y: i32) -> Option<[u8; 4]> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        let offset = (y as usize * self.width as usize + x as usize) * 4;
        Some([
            self.pixels[offset],
            self.pixels[offset + 1],
            self.pixels[offset + 2],
            self.pixels[offset + 3],
        ])
    }
}

/// Source-over composite of one straight-alpha pixel into an RGBA plane.
fn blend(pixels: &mut [u8], width: u32, x: i32, y: i32, rgba: [u8; 4], alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let offset = (y as usize * width as usize + x as usize) * 4;
    let da = pixels[offset + 3] as f32 / 255.0;
    // `1.0 - alpha`, the destination's weight, is the same subexpression in
    // `out_a` and in every channel, so it is evaluated once; each channel keeps
    // its own `dst * da * one_minus_alpha` association, which is not the same
    // f32 product as `dst * (da * one_minus_alpha)`.
    let one_minus_alpha = 1.0 - alpha;
    let out_a = alpha + da * one_minus_alpha;
    let divisor = out_a.max(f32::EPSILON);
    for channel in 0..3 {
        let src = rgba[channel] as f32;
        let dst = pixels[offset + channel] as f32;
        let value = (src * alpha + dst * da * one_minus_alpha) / divisor;
        pixels[offset + channel] = round_channel(value);
    }
    pixels[offset + 3] = round_channel(out_a * 255.0);
}

/// The colour filter a player's `setColor` applies to every item.
///
/// The E-mote SDK's character colour is an ARGB value whose RGB bytes are
/// *centred on 0x80*: the game resets the filter with `setColor(0xFF808080)`
/// and applies one with `setColor(0xFF000000 | colour)`
/// (`/tmp/m38/AffineSourceMotion.decomp.tjs:884,910`), so 0x80 is the neutral
/// multiplier of 1.0 and 0xFF/0x00 are 2x/0x. The alpha byte is a plain
/// opacity multiplier (0xFF = opaque). Which of the two readings the shipped
/// DLL implements is not yet verified against it — the game's own call sites
/// are the evidence — and both are identity at the default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

impl Default for Tint {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Tint {
    /// No filter at all.
    pub const IDENTITY: Self = Self {
        red: 1.0,
        green: 1.0,
        blue: 1.0,
        alpha: 1.0,
    };

    /// The filter `setColor(argb)` selects, with the neutral-centred RGB
    /// reading described on the type (`0x80` → 1.0).
    pub fn from_emote_colour(argb: u32) -> Self {
        Self {
            red: ((argb >> 16) & 0xff) as f32 / 128.0,
            green: ((argb >> 8) & 0xff) as f32 / 128.0,
            blue: (argb & 0xff) as f32 / 128.0,
            alpha: ((argb >> 24) & 0xff) as f32 / 255.0,
        }
    }

    fn applied(self, rgba: [u8; 4]) -> [u8; 4] {
        if self.red == 1.0 && self.green == 1.0 && self.blue == 1.0 {
            return rgba;
        }
        [
            round_channel(rgba[0] as f32 * self.red),
            round_channel(rgba[1] as f32 * self.green),
            round_channel(rgba[2] as f32 * self.blue),
            rgba[3],
        ]
    }
}

/// `x.round().clamp(0.0, 255.0) as u8` for the non-negative channel values this
/// module produces, without the libm `roundf` call the per-channel rounding
/// storm cost.
///
/// `f32::round` rounds halves away from zero; for a non-negative `x` that is
/// `floor(x + 0.5)` in exact arithmetic. Carrying `x + 0.5` in f64 is exact for
/// everything but denormal inputs, and there the error stays orders of
/// magnitude below the `2^-25` that separates the closest f32 from any
/// half-integer — so truncating the f64 sum reproduces `f32::round` bit for bit
/// over the values this module sums (bounded by a few hundred). Negative
/// inputs and NaN, which the original rounding and clamping also folded to 0,
/// ride the same clamp.
#[inline]
fn round_channel(x: f32) -> u8 {
    ((x as f64 + 0.5) as i32).clamp(0, 255) as u8
}

/// Decoded textures of one motion, decoded once per resource and reused.
pub struct TextureCache {
    motion: Arc<Motion>,
    textures: BTreeMap<u32, Option<DecodedTexture>>,
    errors: BTreeMap<u32, String>,
}

impl TextureCache {
    pub fn new(motion: Arc<Motion>) -> Self {
        Self {
            motion,
            textures: BTreeMap::new(),
            errors: BTreeMap::new(),
        }
    }

    /// The decoded texture of a resource, `None` when it cannot be decoded
    /// (the reason is reported through [`TextureCache::errors`]).
    pub fn texture(&mut self, resource_index: u32) -> Option<&DecodedTexture> {
        if !self.textures.contains_key(&resource_index) {
            let decoded = match self.motion.texture_pixels(resource_index) {
                Ok(texture) => Some(texture),
                Err(error) => {
                    self.errors
                        .entry(resource_index)
                        .or_insert_with(|| error.to_string());
                    None
                }
            };
            self.textures.insert(resource_index, decoded);
        }
        self.textures.get(&resource_index).and_then(Option::as_ref)
    }

    /// The decode failures seen so far, in resource order, for logging.
    pub fn errors(&self) -> Vec<(u32, &str)> {
        self.errors
            .iter()
            .map(|(&index, message)| (index, message.as_str()))
            .collect()
    }
}

/// What one [`render_draw_list`] call did, for diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderReport {
    /// Items composited into the canvas.
    pub drawn: usize,
    /// Items skipped because they are invisible or fully transparent.
    pub skipped_invisible: usize,
    /// Items skipped for not being on the normal pass (mask/stencil passes).
    pub skipped_pass: usize,
    /// Items skipped because their texture could not be decoded.
    pub skipped_missing_texture: usize,
    /// Items whose mesh patch was ignored and whose plain quad was drawn.
    pub plain_quad_mesh: usize,
}

/// Renders a draw list into an owned [`Canvas`], in the list's own draw order.
pub fn render_draw_list(
    canvas: &mut Canvas,
    items: &[MotionDrawItem],
    textures: &mut TextureCache,
    tint: Tint,
) -> RenderReport {
    let (width, height) = (canvas.width(), canvas.height());
    render_draw_list_into(canvas.pixels_mut(), width, height, items, textures, tint)
}

/// [`render_draw_list`] into a plane a caller already owns — the engine's
/// layer bitmap, for instance. `pixels` must be `width * height * 4` RGBA
/// bytes; a plane of the wrong size is left untouched and reported as empty.
pub fn render_draw_list_into(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    items: &[MotionDrawItem],
    textures: &mut TextureCache,
    tint: Tint,
) -> RenderReport {
    let mut report = RenderReport::default();
    if pixels.len() != width as usize * height as usize * 4 {
        return report;
    }
    for item in items {
        if !item.visible || item.opacity <= 0.0 {
            report.skipped_invisible += 1;
            continue;
        }
        if item.pass != EmoteDrawPass::Normal {
            report.skipped_pass += 1;
            continue;
        }
        if item.mesh.is_some() {
            report.plain_quad_mesh += 1;
        }
        let alpha = item.opacity.clamp(0.0, 1.0) * tint.alpha;
        let Some(texture) = textures.texture(item.resource_index) else {
            report.skipped_missing_texture += 1;
            continue;
        };
        draw_quad_into(pixels, width, height, texture, item, tint, alpha);
        report.drawn += 1;
    }
    report
}

/// One textured, affine-transformed quad: the sprite rectangle transformed by
/// `scale`/`rotation` about its centre and then `world_transform`.
#[derive(Clone, Copy, Debug)]
struct Vertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
}

#[cfg(test)]
fn draw_quad(
    canvas: &mut Canvas,
    texture: &DecodedTexture,
    item: &MotionDrawItem,
    tint: Tint,
    alpha: f32,
) {
    let (width, height) = (canvas.width(), canvas.height());
    draw_quad_into(
        canvas.pixels_mut(),
        width,
        height,
        texture,
        item,
        tint,
        alpha,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_quad_into(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    texture: &DecodedTexture,
    item: &MotionDrawItem,
    tint: Tint,
    alpha: f32,
) {
    let (w, h) = (texture.width as f32, texture.height as f32);
    let quad = [
        sprite_vertex(
            item,
            [item.left(), item.top()],
            [item.uv[0] * w, item.uv[1] * h],
        ),
        sprite_vertex(
            item,
            [item.right(), item.top()],
            [item.uv[2] * w, item.uv[1] * h],
        ),
        sprite_vertex(
            item,
            [item.right(), item.bottom()],
            [item.uv[2] * w, item.uv[3] * h],
        ),
        sprite_vertex(
            item,
            [item.left(), item.bottom()],
            [item.uv[0] * w, item.uv[3] * h],
        ),
    ];
    rasterize_triangle(
        pixels, width, height, texture, quad[0], quad[1], quad[2], tint, alpha,
    );
    rasterize_triangle(
        pixels, width, height, texture, quad[0], quad[2], quad[3], tint, alpha,
    );
}

/// Maps one sprite-local point onto the canvas: scale/rotation about the
/// sprite's centre, then the sprite's accumulated `world_transform`
/// (eluna's `transform_emote_sprite_point`, `emote.rs:776-791`).
fn sprite_vertex(item: &MotionDrawItem, point: [f32; 2], uv: [f32; 2]) -> Vertex {
    let sx = finite_or(item.scale[0], 1.0);
    let sy = finite_or(item.scale[1], 1.0);
    let angle = finite_or(item.rotation_degrees, 0.0).to_radians();
    let (sin, cos) = angle.sin_cos();
    let dx = (point[0] - item.center[0]) * sx;
    let dy = (point[1] - item.center[1]) * sy;
    let local = [
        item.center[0] + dx * cos - dy * sin,
        item.center[1] + dx * sin + dy * cos,
    ];
    let m = item.world_transform.map(|value| finite_or(value, 0.0));
    // `world_transform` is eluna's `EmoteTransform2D::as_array`:
    // `x' = m11*x + m12*y + tx`, `y' = m21*x + m22*y + ty`
    // (`vendor/eluna/crates/eluna/src/emote.rs:1776-1778`).
    Vertex {
        x: m[0] * local[0] + m[1] * local[1] + m[4],
        y: m[2] * local[0] + m[3] * local[1] + m[5],
        u: uv[0],
        v: uv[1],
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// Half-space function; positive inside a positively wound triangle.
fn edge(a: Vertex, b: Vertex, x: f32, y: f32) -> f32 {
    (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x)
}

/// Whether a pixel exactly on the edge `a -> b` belongs to this triangle: only
/// the top and left edges own their pixels, so a quad's two triangles never
/// blend the same pixel twice.
fn is_top_left(a: Vertex, b: Vertex) -> bool {
    let dy = b.y - a.y;
    let dx = b.x - a.x;
    dy < 0.0 || (dy == 0.0 && dx > 0.0)
}

#[allow(clippy::too_many_arguments)]
fn rasterize_triangle(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    texture: &DecodedTexture,
    v0: Vertex,
    mut v1: Vertex,
    mut v2: Vertex,
    tint: Tint,
    alpha: f32,
) {
    let mut area = edge(v0, v1, v2.x, v2.y);
    if !area.is_finite() || area == 0.0 {
        return;
    }
    if area < 0.0 {
        std::mem::swap(&mut v1, &mut v2);
        area = -area;
    }

    let min_x = v0.x.min(v1.x).min(v2.x).floor().max(0.0) as i32;
    let min_y = v0.y.min(v1.y).min(v2.y).floor().max(0.0) as i32;
    let max_x = v0.x.max(v1.x).max(v2.x).ceil().min(width as f32) as i32;
    let max_y = v0.y.max(v1.y).max(v2.y).ceil().min(height as f32) as i32;
    if min_x >= max_x || min_y >= max_y {
        return;
    }

    let top0 = is_top_left(v0, v1);
    let top1 = is_top_left(v1, v2);
    let top2 = is_top_left(v2, v0);
    // `edge`'s loop-invariant factors, hoisted by hand; the per-pixel
    // expressions below evaluate to exactly what `edge(v0, v1, px, py)` and
    // friends evaluate, same operands in the same association.
    let (dx01, dy01) = (v1.x - v0.x, v1.y - v0.y);
    let (dx12, dy12) = (v2.x - v1.x, v2.y - v1.y);
    let (dx20, dy20) = (v0.x - v2.x, v0.y - v2.y);
    for y in min_y..max_y {
        let py = y as f32 + 0.5;
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let w0 = dx01 * (py - v0.y) - dy01 * (px - v0.x);
            let w1 = dx12 * (py - v1.y) - dy12 * (px - v1.x);
            let w2 = dx20 * (py - v2.y) - dy20 * (px - v2.x);
            let inside = (w0 > 0.0 || (w0 == 0.0 && top0))
                && (w1 > 0.0 || (w1 == 0.0 && top1))
                && (w2 > 0.0 || (w2 == 0.0 && top2));
            if !inside {
                continue;
            }
            let u = (w1 * v0.u + w2 * v1.u + w0 * v2.u) / area;
            let v = (w1 * v0.v + w2 * v1.v + w0 * v2.v) / area;
            let rgba = tint.applied(sample_bilinear(texture, u, v));
            blend(pixels, width, x, y, rgba, alpha * rgba[3] as f32 / 255.0);
        }
    }
}

/// `x.floor()` as an integer and as an f32, without the libm `floorf` call.
///
/// Below `2^24` the truncating cast is exact and `t as f32` round-trips, so the
/// only non-integral truncations are the negative ones, which the branch folds
/// to the floor below. `tf - 1.0` is exact (both values are integers), so the
/// caller's `x - floor` keeps the original subtraction's rounding. Values where
/// the fast path does not apply (NaN, infinities, magnitudes at or above
/// `2^24`) fall back to `f32::floor`, reproducing the original behaviour
/// exactly — including how NaN and infinities make it to the caller.
#[inline]
fn floor_parts(x: f32) -> (i32, f32) {
    if x.abs() < 16_777_216.0 {
        let truncated = x as i32;
        let truncated_f = truncated as f32;
        if truncated_f > x {
            (truncated - 1, truncated_f - 1.0)
        } else {
            (truncated, truncated_f)
        }
    } else {
        let floor = x.floor();
        (floor as i32, floor)
    }
}

/// One bilinear tap: `(1 - fx) * (1 - fy)` and companions for the other three
/// texels, accumulated per channel with the module's per-tap rounding.
///
/// A tap whose texel is fully transparent contributes exactly zero to every
/// channel (`0 * weight` is `+0.0`, and `round(out + 0.0) == out` for the
/// integer-valued `out` the previous taps left), so it is skipped. A weight of
/// zero skips too, as it always did.
#[inline]
fn accumulate(out: &mut [u8; 4], texel: [u8; 4], weight: f32) {
    if weight <= 0.0 || texel == [0, 0, 0, 0] {
        return;
    }
    for channel in 0..4 {
        out[channel] = round_channel(out[channel] as f32 + texel[channel] as f32 * weight);
    }
}

/// Bilinear sample at texture coordinates in pixels (the texel centre is at
/// `x + 0.5`), clamped at the edges.
fn sample_bilinear(texture: &DecodedTexture, u: f32, v: f32) -> [u8; 4] {
    let x = u - 0.5;
    let y = v - 0.5;
    let (x0, x0f) = floor_parts(x);
    let (y0, y0f) = floor_parts(y);
    let fx = x - x0f;
    let fy = y - y0f;
    // The four texels share two clamped columns and two clamped rows, so the
    // clamps `pixel_clamped` would repeat per corner are done once each.
    let last_x = texture.width.saturating_sub(1) as i32;
    let last_y = texture.height.saturating_sub(1) as i32;
    let col0 = x0.clamp(0, last_x) as usize * 4;
    let col1 = (x0 + 1).clamp(0, last_x) as usize * 4;
    let row0 = y0.clamp(0, last_y) as usize * texture.width as usize * 4;
    let row1 = (y0 + 1).clamp(0, last_y) as usize * texture.width as usize * 4;
    let rgba = texture.rgba.as_slice();
    let mut out = [0u8; 4];
    let corners = [
        (row0 + col0, (1.0 - fx) * (1.0 - fy)),
        (row0 + col1, fx * (1.0 - fy)),
        (row1 + col0, (1.0 - fx) * fy),
        (row1 + col1, fx * fy),
    ];
    for (offset, weight) in corners {
        accumulate(
            &mut out,
            [
                rgba[offset],
                rgba[offset + 1],
                rgba[offset + 2],
                rgba[offset + 3],
            ],
            weight,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MotionDrawItem;

    fn texture(width: u32, height: u32, rgba: [u8; 4]) -> DecodedTexture {
        DecodedTexture {
            width,
            height,
            rgba: rgba
                .iter()
                .cycle()
                .take(width as usize * height as usize * 4)
                .copied()
                .collect(),
        }
    }

    fn item(center: [f32; 2], size: [f32; 2], opacity: f32) -> MotionDrawItem {
        MotionDrawItem {
            texture: "hero/body".to_owned(),
            resource_index: 0,
            icon: "body".to_owned(),
            uv: [0.0, 0.0, 1.0, 1.0],
            center,
            size,
            scale: [1.0, 1.0],
            rotation_degrees: 0.0,
            opacity,
            z: 0.0,
            visible: true,
            world_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            mesh: None,
            label: None,
            motion: "idle".to_owned(),
            draw_index: 0,
            pass: EmoteDrawPass::Normal,
        }
    }

    /// A quad's two triangles must not blend the shared diagonal twice: with a
    /// half-transparent source every covered pixel keeps exactly one blend, so
    /// its alpha is 128 rather than the 192 a double blend would leave.
    #[test]
    fn quad_triangles_do_not_double_blend_the_seam() {
        let texture = texture(8, 8, [255, 255, 255, 255]);
        let mut canvas = Canvas::new(16, 16);
        let item = item([8.0, 8.0], [8.0, 8.0], 0.5);
        draw_quad(&mut canvas, &texture, &item, Tint::IDENTITY, 0.5);
        for y in 4..12 {
            for x in 4..12 {
                let pixel = canvas.pixel(x, y).expect("inside");
                assert_eq!(
                    pixel,
                    [255, 255, 255, 128],
                    "at {x},{y}: one source-over blend of a half-transparent white"
                );
            }
        }
        assert_eq!(
            canvas.pixel(3, 8),
            Some([0, 0, 0, 0]),
            "outside stays clear"
        );
    }

    #[test]
    fn scaling_and_translation_place_the_quad() {
        let texture = texture(2, 2, [10, 20, 30, 255]);
        let mut canvas = Canvas::new(8, 8);
        let mut item = item([4.0, 4.0], [4.0, 4.0], 1.0);
        item.scale = [0.5, 0.5];
        item.world_transform = [1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        draw_quad(&mut canvas, &texture, &item, Tint::IDENTITY, 1.0);
        // The 4x4 quad ([2,2]..[6,6]) scaled by 0.5 about its centre covers
        // [3,3]..[5,5]; the world transform shifts it to [4,4]..[6,6], i.e.
        // pixel centres 4.5 and 5.5.
        assert_eq!(canvas.pixel(4, 4), Some([10, 20, 30, 255]));
        assert_eq!(canvas.pixel(5, 5), Some([10, 20, 30, 255]));
        assert_eq!(canvas.pixel(3, 3), Some([0, 0, 0, 0]));
        assert_eq!(canvas.pixel(6, 6), Some([0, 0, 0, 0]));
    }

    #[test]
    fn tint_scales_channels_and_alpha() {
        let texture = texture(2, 2, [200, 100, 50, 255]);
        let mut canvas = Canvas::new(2, 2);
        let item = item([1.0, 1.0], [2.0, 2.0], 1.0);
        let tint = Tint::from_emote_colour(0xFF40_2010);
        draw_quad(&mut canvas, &texture, &item, tint, 1.0);
        // 0x40/128 = 0.5, 0x20/128 = 0.25, 0x10/128 = 0.125.
        assert_eq!(canvas.pixel(0, 0), Some([100, 25, 6, 255]));
    }

    /// The pre-optimisation rasteriser, copied verbatim from the base commit of
    /// the M154 fast path: the same pixels through the old `f32::round`,
    /// `f32::floor` and `pixel_clamped` calls are the contract the optimised
    /// paths are held to, bit for bit.
    mod reference {
        use super::*;

        pub(super) fn applied(tint: Tint, rgba: [u8; 4]) -> [u8; 4] {
            [
                (rgba[0] as f32 * tint.red).round().clamp(0.0, 255.0) as u8,
                (rgba[1] as f32 * tint.green).round().clamp(0.0, 255.0) as u8,
                (rgba[2] as f32 * tint.blue).round().clamp(0.0, 255.0) as u8,
                rgba[3],
            ]
        }

        pub(super) fn blend(
            pixels: &mut [u8],
            width: u32,
            x: i32,
            y: i32,
            rgba: [u8; 4],
            alpha: f32,
        ) {
            let alpha = alpha.clamp(0.0, 1.0);
            if alpha <= 0.0 {
                return;
            }
            let offset = (y as usize * width as usize + x as usize) * 4;
            let da = pixels[offset + 3] as f32 / 255.0;
            let out_a = alpha + da * (1.0 - alpha);
            for channel in 0..3 {
                let src = rgba[channel] as f32;
                let dst = pixels[offset + channel] as f32;
                let value = (src * alpha + dst * da * (1.0 - alpha)) / out_a.max(f32::EPSILON);
                pixels[offset + channel] = value.round().clamp(0.0, 255.0) as u8;
            }
            pixels[offset + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
        }

        pub(super) fn sample_bilinear(texture: &DecodedTexture, u: f32, v: f32) -> [u8; 4] {
            let x = u - 0.5;
            let y = v - 0.5;
            let x0 = x.floor();
            let y0 = y.floor();
            let fx = x - x0;
            let fy = y - y0;
            let x0 = x0 as i32;
            let y0 = y0 as i32;
            let mut out = [0u8; 4];
            let corners = [
                (x0, y0, (1.0 - fx) * (1.0 - fy)),
                (x0 + 1, y0, fx * (1.0 - fy)),
                (x0, y0 + 1, (1.0 - fx) * fy),
                (x0 + 1, y0 + 1, fx * fy),
            ];
            for (cx, cy, weight) in corners {
                if weight <= 0.0 {
                    continue;
                }
                let texel = texture.pixel_clamped(cx, cy);
                for channel in 0..4 {
                    out[channel] = (out[channel] as f32 + texel[channel] as f32 * weight)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
            }
            out
        }

        #[allow(clippy::too_many_arguments)]
        pub(super) fn rasterize_triangle(
            pixels: &mut [u8],
            width: u32,
            height: u32,
            texture: &DecodedTexture,
            v0: Vertex,
            mut v1: Vertex,
            mut v2: Vertex,
            tint: Tint,
            alpha: f32,
        ) {
            let mut area = edge(v0, v1, v2.x, v2.y);
            if !area.is_finite() || area == 0.0 {
                return;
            }
            if area < 0.0 {
                std::mem::swap(&mut v1, &mut v2);
                area = -area;
            }

            let min_x = v0.x.min(v1.x).min(v2.x).floor().max(0.0) as i32;
            let min_y = v0.y.min(v1.y).min(v2.y).floor().max(0.0) as i32;
            let max_x = v0.x.max(v1.x).max(v2.x).ceil().min(width as f32) as i32;
            let max_y = v0.y.max(v1.y).max(v2.y).ceil().min(height as f32) as i32;
            if min_x >= max_x || min_y >= max_y {
                return;
            }

            let top0 = is_top_left(v0, v1);
            let top1 = is_top_left(v1, v2);
            let top2 = is_top_left(v2, v0);
            for y in min_y..max_y {
                for x in min_x..max_x {
                    let px = x as f32 + 0.5;
                    let py = y as f32 + 0.5;
                    let w0 = edge(v0, v1, px, py);
                    let w1 = edge(v1, v2, px, py);
                    let w2 = edge(v2, v0, px, py);
                    let inside = (w0 > 0.0 || (w0 == 0.0 && top0))
                        && (w1 > 0.0 || (w1 == 0.0 && top1))
                        && (w2 > 0.0 || (w2 == 0.0 && top2));
                    if !inside {
                        continue;
                    }
                    let u = (w1 * v0.u + w2 * v1.u + w0 * v2.u) / area;
                    let v = (w1 * v0.v + w2 * v1.v + w0 * v2.v) / area;
                    let rgba = applied(tint, sample_bilinear(texture, u, v));
                    blend(pixels, width, x, y, rgba, alpha * rgba[3] as f32 / 255.0);
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub(super) fn draw_quad_into(
            pixels: &mut [u8],
            width: u32,
            height: u32,
            texture: &DecodedTexture,
            item: &MotionDrawItem,
            tint: Tint,
            alpha: f32,
        ) {
            let (w, h) = (texture.width as f32, texture.height as f32);
            let quad = [
                sprite_vertex(
                    item,
                    [item.left(), item.top()],
                    [item.uv[0] * w, item.uv[1] * h],
                ),
                sprite_vertex(
                    item,
                    [item.right(), item.top()],
                    [item.uv[2] * w, item.uv[1] * h],
                ),
                sprite_vertex(
                    item,
                    [item.right(), item.bottom()],
                    [item.uv[2] * w, item.uv[3] * h],
                ),
                sprite_vertex(
                    item,
                    [item.left(), item.bottom()],
                    [item.uv[0] * w, item.uv[3] * h],
                ),
            ];
            rasterize_triangle(
                pixels, width, height, texture, quad[0], quad[1], quad[2], tint, alpha,
            );
            rasterize_triangle(
                pixels, width, height, texture, quad[0], quad[2], quad[3], tint, alpha,
            );
        }
    }

    /// A texture with per-texel noise: smooth weights alone rarely land the
    /// per-tap accumulations on a rounding tie, random bytes do.
    fn noisy_texture(width: u32, height: u32, seed: u64) -> DecodedTexture {
        let mut state = seed | 1;
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width as usize * height as usize * 4 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            rgba.push((state >> 33) as u8);
        }
        DecodedTexture {
            width,
            height,
            rgba,
        }
    }

    /// Every sampled value the fast sampler must reproduce: exact texel
    /// centres, quarter and tenth offsets, values a f32 step below and above a
    /// half-integer (the rounding ties), negatives, magnitudes around the
    /// `floor` fast path's `2^24` bound, and the non-finite inputs. `+inf` and
    /// `f32::MAX` are left out on purpose: both paths compute `x0 + 1` for the
    /// next corner, which overflows in a debug build (identically) rather than
    /// sampling anything.
    fn probe_values() -> Vec<f32> {
        let mut values = Vec::new();
        for step in [-3i32, -2, -1, 0, 1, 2, 3, 7] {
            let base = step as f32;
            values.extend([
                base,
                base + 0.5,
                base + 0.25,
                base + 0.75,
                base + 0.1,
                base + 0.9,
            ]);
            for delta in [f32::EPSILON * 8.0, 1e-7, 2.9e-5, 7e-5] {
                values.push(base + 0.5 - delta);
                values.push(base + 0.5 + delta);
            }
        }
        values.extend([
            0.0,
            -0.0,
            1e-30,
            f32::MIN_POSITIVE,
            8_388_608.0,
            16_777_215.0,
            16_777_216.0,
            16_777_217.0,
            -16_777_216.0,
            1e9,
            -1e30,
            f32::NEG_INFINITY,
            f32::NAN,
        ]);
        values
    }

    /// The fast rounding is a claim about *every* f32 in the channel range, not
    /// just the values the synthetic quads happen to land on: sweep the whole
    /// f32 domain from 0 to 2^21 in bit-pattern steps (every binade, every
    /// mantissa phase) plus the negatives, and hold it to `f32::round` and its
    /// clamp.
    #[test]
    fn channel_rounding_matches_f32_round_across_the_channel_range() {
        let sweep = |from: u32, to: u32| {
            let mut checked = 0usize;
            let mut bits = from;
            while bits <= to {
                let x = f32::from_bits(bits);
                let expected = x.round().clamp(0.0, 255.0) as u8;
                assert_eq!(round_channel(x), expected, "x={x} (bits {bits:#010x})");
                checked += 1;
                bits += 4093;
            }
            assert!(checked > 250_000, "swept {checked} values");
        };
        // 0.0 ..= 2^21 (the blend's and the tint's channels never exceed 511).
        sweep(0x0000_0000, 0x4a00_0000);
        // -0.0 ..= -2^21: every one of them must clamp to 0, as `round` did.
        sweep(0x8000_0000, 0xca00_0000);
    }

    /// The sampler is the hot path the M154 optimisation rewrote; every probe
    /// pair must land on the same bytes as the reference sampler.
    #[test]
    fn bilinear_sampler_matches_the_reference_bit_for_bit() {
        let textures = [
            noisy_texture(1, 1, 7),
            noisy_texture(3, 2, 11),
            noisy_texture(5, 4, 13),
            noisy_texture(16, 9, 17),
            texture(4, 4, [255, 255, 255, 255]),
            texture(4, 4, [0, 0, 0, 0]),
        ];
        let values = probe_values();
        for texture in &textures {
            for &u in &values {
                for &v in &values {
                    assert_eq!(
                        sample_bilinear(texture, u, v),
                        reference::sample_bilinear(texture, u, v),
                        "{}x{} at u={u} v={v}",
                        texture.width,
                        texture.height
                    );
                }
            }
        }
    }

    /// The blend step rounds every channel and the alpha; the fast rounding
    /// must be indistinguishable from `f32::round` over the whole input domain
    /// the rasteriser can produce (all byte pairs and a spread of item alphas).
    #[test]
    fn blend_matches_the_reference_bit_for_bit() {
        let alphas = [
            0.0,
            1.0,
            0.5,
            1.0 / 3.0,
            0.1,
            0.999,
            128.0 / 255.0,
            1e-6,
            1e-30,
            f32::NAN,
            -0.5,
            2.0,
        ];
        for &alpha in &alphas {
            for src in 0..=255u8 {
                for dst_a in 0..=255u8 {
                    for dst_rgb in [0u8, 1, 127, 128, 254, 255] {
                        let rgba = [src, src / 2, src.wrapping_add(1), src.wrapping_sub(1)];
                        let mut old = [dst_rgb, dst_rgb.wrapping_add(3), dst_rgb, dst_a];
                        let mut new = old;
                        reference::blend(&mut old, 1, 0, 0, rgba, alpha);
                        blend(&mut new, 1, 0, 0, rgba, alpha);
                        assert_eq!(
                            old, new,
                            "alpha={alpha} src={src} dst={dst_rgb}/{dst_a} rgba={rgba:?}"
                        );
                    }
                }
            }
        }
    }

    /// The tint filter rounds each channel after scaling; the same fast
    /// rounding must reproduce it for every byte and filter value the plugin
    /// can select.
    #[test]
    fn tint_filter_matches_the_reference_bit_for_bit() {
        let tints = [
            Tint::IDENTITY,
            Tint::from_emote_colour(0xFF40_2010),
            Tint::from_emote_colour(0xFF80_8080),
            Tint::from_emote_colour(0x0080_8080),
            Tint::from_emote_colour(0xFFFF_FFFF),
            Tint::from_emote_colour(0xFF00_0000),
            Tint {
                red: 0.7,
                green: 1.3,
                blue: 0.0,
                alpha: 0.25,
            },
        ];
        for tint in tints {
            for value in 0..=255u8 {
                let rgba = [value, value.wrapping_mul(3), value.wrapping_add(200), value];
                assert_eq!(
                    tint.applied(rgba),
                    reference::applied(tint, rgba),
                    "tint={tint:?} rgba={rgba:?}"
                );
            }
        }
    }

    /// Whole quads — geometry, coverage, sampling, filter, blend and all —
    /// rendered through the optimised and the reference paths must leave
    /// byte-identical canvases: a fractional-pixel grid of translations,
    /// scales, rotations, sub-rect uvs, opacities and filters (including the
    /// no-op ones the tint fast path now shortcuts).
    #[test]
    fn rendered_quads_match_the_reference_bit_for_bit() {
        let textures = [
            noisy_texture(7, 5, 23),
            noisy_texture(32, 32, 29),
            texture(3, 3, [200, 40, 10, 255]),
            texture(2, 2, [0, 0, 0, 0]),
        ];
        let tints = [
            Tint::IDENTITY,
            Tint::from_emote_colour(0xFF80_8080),
            Tint::from_emote_colour(0xFF40_C020),
            Tint::from_emote_colour(0x8070_9090),
        ];
        let mut cases = 0usize;
        for texture in &textures {
            for &center_offset in &[0.0f32, 0.25, 0.5, 0.1] {
                for &scale in &[0.37f32, 1.0, 1.9, 3.0] {
                    for &rotation in &[0.0f32, 7.3, 45.0, 90.0, 180.0] {
                        for &opacity in &[1.0f32, 0.5, 0.2] {
                            for &uv in &[
                                [0.0f32, 0.0, 1.0, 1.0],
                                [0.25, 0.25, 0.75, 0.75],
                                [0.1, 0.2, 0.3, 0.9],
                            ] {
                                let mut item = item([12.0, 9.0], [10.0, 7.0], opacity);
                                item.center = [
                                    item.center[0] + center_offset,
                                    item.center[1] + center_offset,
                                ];
                                item.scale = [scale, scale * 0.75];
                                item.rotation_degrees = rotation;
                                item.uv = uv;
                                item.world_transform = [1.0, 0.0, 0.0, 1.0, 2.5, -1.5];
                                for &tint in &tints {
                                    let mut fast = Canvas::new(24, 18);
                                    let mut old = Canvas::new(24, 18);
                                    draw_quad(&mut fast, texture, &item, tint, opacity);
                                    let (width, height) = (old.width(), old.height());
                                    reference::draw_quad_into(
                                        old.pixels_mut(),
                                        width,
                                        height,
                                        texture,
                                        &item,
                                        tint,
                                        opacity,
                                    );
                                    assert_eq!(
                                        fast.pixels(),
                                        old.pixels(),
                                        "texture {}x{} center={:?} scale={scale} rot={rotation} \
                                         opacity={opacity} uv={uv:?} tint={tint:?}",
                                        texture.width,
                                        texture.height,
                                        item.center
                                    );
                                    cases += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(cases >= 1_000, "the matrix rendered {cases} quads");
    }

    /// A deterministic spread of quads with wilder parameters (reflections,
    /// off-canvas geometry, tiny and huge scales, alpha-zero filters) through
    /// both paths, so nothing in the fast paths depends on the tidy cases.
    #[test]
    fn randomised_quads_match_the_reference_bit_for_bit() {
        struct Rng(u64);

        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0
            }

            fn f32(&mut self) -> f32 {
                (self.next() >> 40) as f32 / 16_777_216.0
            }
        }

        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut digest = 0u64;
        for case in 0..256 {
            let texture = noisy_texture(
                1 + (rng.next() % 40) as u32,
                1 + (rng.next() % 40) as u32,
                rng.next(),
            );
            let mut item = item([0.0, 0.0], [4.0, 4.0], 1.0);
            item.center = [rng.f32() * 64.0 - 16.0, rng.f32() * 48.0 - 12.0];
            item.size = [rng.f32() * 40.0 + 1.0, rng.f32() * 40.0 + 1.0];
            item.scale = [rng.f32() * 3.0, rng.f32() * 3.0];
            item.rotation_degrees = rng.f32() * 720.0 - 360.0;
            item.opacity = rng.f32();
            item.uv = [
                rng.f32() - 0.1,
                rng.f32() - 0.1,
                rng.f32() + 0.5,
                rng.f32() + 0.5,
            ];
            item.world_transform = [
                rng.f32() * 2.0 - 1.0,
                rng.f32() - 0.5,
                rng.f32() - 0.5,
                rng.f32() * 2.0 - 1.0,
                rng.f32() * 8.0 - 4.0,
                rng.f32() * 8.0 - 4.0,
            ];
            let tint = Tint::from_emote_colour(rng.next() as u32);
            let alpha = rng.f32();
            let mut fast = Canvas::new(48, 32);
            let mut old = Canvas::new(48, 32);
            draw_quad(&mut fast, &texture, &item, tint, alpha);
            let (width, height) = (old.width(), old.height());
            reference::draw_quad_into(
                old.pixels_mut(),
                width,
                height,
                &texture,
                &item,
                tint,
                alpha,
            );
            assert_eq!(
                fast.pixels(),
                old.pixels(),
                "case {case}: {item:?} {tint:?}"
            );
            for &byte in fast.pixels() {
                digest = digest.wrapping_mul(31).wrapping_add(u64::from(byte));
            }
        }
        assert_ne!(digest, 0, "the digest saw pixels");
    }
}
