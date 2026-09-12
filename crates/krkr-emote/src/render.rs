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
//! `transform_emote_sprite_point` (`vendor/eluna/crates/eluna/src/emote.rs:367-380`)
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
//! motion's `opa` value ends up (see [`crate::normalize`] for the scaling).
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
    let out_a = alpha + da * (1.0 - alpha);
    for channel in 0..3 {
        let src = rgba[channel] as f32;
        let dst = pixels[offset + channel] as f32;
        let value = (src * alpha + dst * da * (1.0 - alpha)) / out_a.max(f32::EPSILON);
        pixels[offset + channel] = value.round().clamp(0.0, 255.0) as u8;
    }
    pixels[offset + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
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
        [
            (rgba[0] as f32 * self.red).round().clamp(0.0, 255.0) as u8,
            (rgba[1] as f32 * self.green).round().clamp(0.0, 255.0) as u8,
            (rgba[2] as f32 * self.blue).round().clamp(0.0, 255.0) as u8,
            rgba[3],
        ]
    }
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
/// (eluna's `transform_emote_sprite_point`, `emote.rs:367-380`).
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
    // (`vendor/eluna/crates/eluna/src/emote.rs:754-763`).
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
            let rgba = tint.applied(sample_bilinear(texture, u, v));
            blend(pixels, width, x, y, rgba, alpha * rgba[3] as f32 / 255.0);
        }
    }
}

/// Bilinear sample at texture coordinates in pixels (the texel centre is at
/// `x + 0.5`), clamped at the edges.
fn sample_bilinear(texture: &DecodedTexture, u: f32, v: f32) -> [u8; 4] {
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
}
