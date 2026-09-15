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
//! # Alpha, blend modes and corner colours
//!
//! The canvas holds straight (non-premultiplied) R, G, B, A bytes — the
//! engine's own layer store — and every item is composited with the equation
//! the reference selects for it: the sprite's blend mode (below) enters as
//! `dst = src * a + dst * (1 - a)` for the default source-over case, with
//! `a = pixel_alpha * item.opacity * tint.alpha`. `item.opacity` is the
//! sprite's own opacity, which is where the motion's `opa` value ends up
//! (eluna divides the file's byte by 255; the adapter passes the field
//! through).
//!
//! Two per-sprite fields the reference always carried are honoured here:
//!
//! - [`SpriteBlend`] — the sprite's native `bm` low nibble, which the DLL
//!   turns into its D3D blend table ([`SpriteBlend`] carries the transcription
//!   and the addresses it was read from). It chooses the composite equation
//!   for the item instead of the fixed source-over the module used to apply.
//! - the four corner colours — the item's per-quad tint, interpolated across
//!   the quad barycentrically, with the neutral values (`0x808080` under
//!   MODULATE2X, `0xFFFFFFFF` otherwise) exactly 1.0 so an untinted sprite
//!   stays bit-identical to one drawn with no colour handling at all.
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

/// The reference's per-sprite blend equation, selected by the low nibble of a
/// sprite's native `bm` (`MotionDrawItem::blend_mode`).
///
/// The mapping is the shipping `ep` player's own blend table, read from the
/// D3D build's binary (`motionplayer.dll`, image base `0x10000000`): the
/// per-sprite state function at VA `0x10006c40` masks `bm` with `0x0f`
/// (`10006c4c: and $0xf,%ecx`), uses that as an index into a 7-entry table of
/// `(BLENDOP, DESTBLEND, SRCBLEND)` triples at VA `0x1012a980`
/// (`10006d5b: lea (%ebx,%ebx,2),%edi` … `10006d60: mov 0x1012a980(%edi,%edi,1),%edx`,
/// pushed with `D3DRS_BLENDOP` `0xab`), and selects the texture-colour stage
/// op from the high nibble (`10006c68: cmp $0x10,%eax` →
/// `SetTextureStageState(0, D3DTSS_COLOROP, 5)` MODULATE2X, else `4`
/// MODULATE). The draw path feeds it the sprite record's `bm` field
/// (`1004731c: mov 0xc4(%esi),%eax` … `10047320: call 0x10006c40`). The table
/// bytes, dumped from the file at `0x128f80`:
///
/// | `bm & 0xF` | BLENDOP | DESTBLEND | SRCBLEND | operation |
/// | ---------- | ------- | --------- | -------- | --------- |
/// | 0 | ADD (1) | INVSRCALPHA (6) | SRCALPHA (5) | source-over |
/// | 1 | ADD (1) | ONE (2) | SRCALPHA (5) | additive |
/// | 2 | REVSUBTRACT (3) | ONE (2) | SRCALPHA (5) | subtractive |
/// | 3 | ADD (1) | INVSRCALPHA (6) | DESTCOLOR (9) | multiply |
/// | 4 | ADD (1) | ONE (2) | INVDESTCOLOR (10) | screen |
/// | 5 | REVSUBTRACT (3) | ONE (2) | SRCALPHA (5) | subtractive |
/// | 6 | ADD (1) | ZERO (1) | ONE (2) | copy |
///
/// Entries 2 and 5 are byte-identical in the shipped table, and the no-D3D
/// build selects the *same* per-entry operation as a TVP layer type
/// (`motionplayer_nod3d.dll`, `1003a850_FUN_1003a850.c:393-406`):
///
/// ```c
/// switch(*puStack_350 & 0xf) {           // bm & 0xF
///   case 1:          puStack_468 = 0xe;  // ltPsAdditive
///   case 2: case 5:  puStack_468 = 0xf;  // ltPsSubtractive
///   case 3:          puStack_468 = 0x10; // ltPsMultiplicative
///   case 4:          puStack_468 = 0x11; // ltPsScreen
///   case 6:          bVar11 = true;      // …and falls through
///   case 0:          puStack_468 = 0x2;  // ltTransparent
/// }
/// ```
///
/// (the layer-type numbers are `visual/drawable.h:20-45` of the reference
/// tree). Two independent renderers therefore agree entry by entry, and the
/// `0xF == 6` copy entry is additionally special-cased as the opaque path in
/// `FUN_100385e0` (`decompiled/100385e0_FUN_100385e0.c:169-172`) and at
/// `1003a850_FUN_1003a850.c:284`.
///
/// Values outside the table fall back to [`SpriteBlend::Over`]; no shipped
/// PARQUET motion contains one. The corpus's own `bm` values are `0x10` (the
/// default), `0x00` (`m2logo`, `splash`, `yuzulogo`), `0x01` (an additive
/// `m2logo` sprite), `0x11` (`title_bg`'s additive sprite) and `0x13` (`sd101`'s
/// haze layer — `sprites=64 blend_modes={16: 57, 19: 7}` over the six sampled
/// ticks in each copy of the file).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpriteBlend {
    /// `bm & 0xF` 0 (and every value the table does not name): source-over.
    Over,
    /// `bm & 0xF` 1: additive.
    Add,
    /// `bm & 0xF` 2 or 5: subtractive (`dest - src * alpha`).
    Sub,
    /// `bm & 0xF` 3: multiply.
    Mul,
    /// `bm & 0xF` 4: screen.
    Screen,
    /// `bm & 0xF` 6: copy — the source replaces the destination, alpha
    /// included.
    Copy,
}

impl SpriteBlend {
    /// The blend equation a sprite's native `bm` selects.
    pub fn of(blend_mode: u32) -> Self {
        match blend_mode & 0x0F {
            1 => Self::Add,
            2 | 5 => Self::Sub,
            3 => Self::Mul,
            4 => Self::Screen,
            6 => Self::Copy,
            _ => Self::Over,
        }
    }

    /// Whether this sprite's corner colours are doubled, the reference's
    /// MODULATE2X texture-colour stage (`bm & 0xF0 == 0x10`).
    pub fn modulates_2x(blend_mode: u32) -> bool {
        (blend_mode & 0xF0) == 0x10
    }
}

/// The four corner-colour multipliers of one draw item, in
/// `[top-left, top-right, bottom-right, bottom-left]` order, or `None` when
/// every corner is neutral (which draws bit-identically to ignoring the field
/// entirely). That corner order is *inferred* from the quad's own build order,
/// not recovered from the DLL — see `MotionDrawItem::corner_colors`.
///
/// Each channel is scaled the way the reference's texture-colour stage scales
/// it: 0x80 is exactly 1.0 under MODULATE2X (`bm & 0xF0 == 0x10`, the neutral
/// colour the frames carry) and 0xFF is 1.0 under MODULATE. Only the colour
/// channels take that scale; the corner alpha is a plain 0..255 opacity
/// factor, so a neutral corner keeps the sprite's own alpha untouched.
///
/// A quad whose four corners carry the *same* colour is returned as
/// [`CornerShading::Uniform`]: the reference's vertex colours are then constant
/// across the quad, and interpolating four equal values would only add
/// rounding drift to an otherwise exact 1.0.
fn corner_shading(item: &MotionDrawItem) -> Option<CornerShading> {
    let neutral = if SpriteBlend::modulates_2x(item.blend_mode) {
        0x80
    } else {
        0xFF
    };
    let scale = neutral as f32;
    let factor = |packed: u32| {
        [
            ((packed >> 24) & 0xFF) as f32 / scale,
            ((packed >> 16) & 0xFF) as f32 / scale,
            ((packed >> 8) & 0xFF) as f32 / scale,
            (packed & 0xFF) as f32 / 255.0,
        ]
    };
    let uniform = *item.corner_colors.first()?;
    if item.corner_colors.iter().all(|corner| *corner == uniform) {
        let factor = factor(uniform);
        if factor == [1.0; 4] {
            return None;
        }
        return Some(CornerShading::Uniform(factor));
    }
    Some(CornerShading::PerCorner(item.corner_colors.map(factor)))
}

/// One item's per-corner colour multipliers.
#[derive(Clone, Copy, Debug, PartialEq)]
enum CornerShading {
    /// All four corners carry the same colour.
    Uniform([f32; 4]),
    /// A gradient: one factor set per quad corner, in
    /// `[top-left, top-right, bottom-right, bottom-left]` order.
    PerCorner([[f32; 4]; 4]),
}

/// One sprite pixel into the straight-alpha plane under `mode`.
///
/// Every mode is the reference's own software shape: compute the mode's
/// *colour* from the destination and the source, then alpha-blend that colour
/// onto the destination with the source's alpha. That is what TVP's
/// Photoshop-family kernels do — the nod3d build reaches them through the
/// layer types the DLL selects for `bm` (see [`SpriteBlend`]), and their math
/// is `visual/gl/blend_functor_c.h`: `ps_alpha_blend_func` is
/// `d + (s - d) * a / 255`, `ps_mul_blend_func` pre-multiplies
/// `s = d * s / 255` into the same lerp, `ps_add_blend_func` saturates
/// `d + s`, `ps_sub_blend_func` saturates `d - s`, and `ps_screen_blend_func`
/// lerps toward `s + d - s * d / 255`.
///
/// The plane stays straight alpha, so the premultiplied result is divided back
/// out exactly as the source-over case always did; with an opaque destination
/// (what the game clears its draw target to) the division is by 1 and the
/// equation is literally the reference's lerp.
fn composite(
    pixels: &mut [u8],
    width: u32,
    x: i32,
    y: i32,
    rgba: [u8; 4],
    alpha: f32,
    mode: SpriteBlend,
) {
    let alpha = alpha.clamp(0.0, 1.0);
    // A fully transparent source contributes nothing under every blending
    // mode except copy, where it is exactly the clear it says it is.
    if alpha <= 0.0 && mode != SpriteBlend::Copy {
        return;
    }
    let offset = (y as usize * width as usize + x as usize) * 4;
    let da = pixels[offset + 3] as f32 / 255.0;
    let dst = [
        pixels[offset] as f32,
        pixels[offset + 1] as f32,
        pixels[offset + 2] as f32,
    ];
    let dp = [dst[0] * da, dst[1] * da, dst[2] * da];
    let src = [rgba[0] as f32, rgba[1] as f32, rgba[2] as f32];
    if mode == SpriteBlend::Copy {
        // The copy entry replaces the destination outright, alpha included.
        let dp = [src[0] * alpha, src[1] * alpha, src[2] * alpha];
        let divisor = alpha.max(f32::EPSILON);
        for channel in 0..3 {
            pixels[offset + channel] = round_channel((dp[channel] / divisor).clamp(0.0, 255.0));
        }
        pixels[offset + 3] = round_channel(alpha * 255.0);
        return;
    }
    let colour = match mode {
        SpriteBlend::Over | SpriteBlend::Copy => src,
        // The mode colour is computed on the *straight* destination, which is
        // what the reference's kernels read out of the target.
        SpriteBlend::Add => [
            (src[0] + dst[0]).min(255.0),
            (src[1] + dst[1]).min(255.0),
            (src[2] + dst[2]).min(255.0),
        ],
        SpriteBlend::Sub => [
            (dst[0] - src[0]).max(0.0),
            (dst[1] - src[1]).max(0.0),
            (dst[2] - src[2]).max(0.0),
        ],
        SpriteBlend::Mul => [
            dst[0] * src[0] / 255.0,
            dst[1] * src[1] / 255.0,
            dst[2] * src[2] / 255.0,
        ],
        SpriteBlend::Screen => [
            src[0] + dst[0] - dst[0] * src[0] / 255.0,
            src[1] + dst[1] - dst[1] * src[1] / 255.0,
            src[2] + dst[2] - dst[2] * src[2] / 255.0,
        ],
    };
    let out_a = (alpha + da * (1.0 - alpha)).clamp(0.0, 1.0);
    let divisor = out_a.max(f32::EPSILON);
    for channel in 0..3 {
        let value = colour[channel] * alpha + dp[channel] * (1.0 - alpha);
        pixels[offset + channel] = round_channel((value / divisor).clamp(0.0, 255.0));
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
    /// Items composited with a non-default blend equation
    /// ([`SpriteBlend`]): additive, subtractive, multiply, screen, copy.
    pub blended_add: usize,
    pub blended_sub: usize,
    pub blended_mul: usize,
    pub blended_screen: usize,
    pub blended_copy: usize,
    /// Items whose four corner colours are not the neutral pair, so their quad
    /// was tinted per corner instead of sampling the texture as authored.
    pub tinted_corners: usize,
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
        match SpriteBlend::of(item.blend_mode) {
            SpriteBlend::Over => {}
            SpriteBlend::Add => report.blended_add += 1,
            SpriteBlend::Sub => report.blended_sub += 1,
            SpriteBlend::Mul => report.blended_mul += 1,
            SpriteBlend::Screen => report.blended_screen += 1,
            SpriteBlend::Copy => report.blended_copy += 1,
        }
        if corner_shading(item).is_some() {
            report.tinted_corners += 1;
        }
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
    // The quad's corners are built in `[top-left, top-right, bottom-right,
    // bottom-left]` order, which is the order `corner_factors` reports; the
    // two triangles carry the corners they span.
    let shade = Shading {
        blend: SpriteBlend::of(item.blend_mode),
        colours: corner_shading(item),
    };
    rasterize_triangle(
        pixels,
        width,
        height,
        texture,
        quad[0],
        quad[1],
        quad[2],
        [0, 1, 2],
        tint,
        alpha,
        shade,
    );
    rasterize_triangle(
        pixels,
        width,
        height,
        texture,
        quad[0],
        quad[2],
        quad[3],
        [0, 2, 3],
        tint,
        alpha,
        shade,
    );
}

/// One item's blend equation and its corner-colour shading.
#[derive(Clone, Copy, Debug)]
struct Shading {
    blend: SpriteBlend,
    /// `None` when every corner is neutral, which is the common case and the
    /// one that must stay bit-identical to drawing without colour handling.
    colours: Option<CornerShading>,
}

impl Shading {
    /// The three vertex colours of a triangle spanning quad corners
    /// `[a, b, c]`, or `None` when the item is untinted or uniformly tinted
    /// (whose per-vertex values would all be that same colour).
    fn triangle(&self, corners: [usize; 3]) -> Option<[[f32; 4]; 3]> {
        match self.colours? {
            CornerShading::Uniform(_) => None,
            CornerShading::PerCorner(colours) => Some([
                colours[corners[0]],
                colours[corners[1]],
                colours[corners[2]],
            ]),
        }
    }
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
    mut corners: [usize; 3],
    tint: Tint,
    alpha: f32,
    shading: Shading,
) {
    let mut area = edge(v0, v1, v2.x, v2.y);
    if !area.is_finite() || area == 0.0 {
        return;
    }
    if area < 0.0 {
        std::mem::swap(&mut v1, &mut v2);
        corners.swap(1, 2);
        area = -area;
    }
    let colours = shading.triangle(corners);
    let uniform = match shading.colours {
        Some(CornerShading::Uniform(factor)) => Some(factor),
        _ => None,
    };

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
            let (rgba, alpha) = match (colours, uniform) {
                // The reference's four-corner colours, interpolated across the
                // quad exactly like the texture coordinates are: each vertex
                // carries its corner's factor and the pixel takes the same
                // barycentric mix.
                (Some([c0, c1, c2]), _) => {
                    let mut factor = [0.0f32; 4];
                    for (channel, out) in factor.iter_mut().enumerate() {
                        *out = (w1 * c0[channel] + w2 * c1[channel] + w0 * c2[channel]) / area;
                    }
                    shaded(rgba, alpha, factor)
                }
                (None, Some(factor)) => shaded(rgba, alpha, factor),
                (None, None) => (rgba, alpha),
            };
            composite(
                pixels,
                width,
                x,
                y,
                rgba,
                alpha * rgba[3] as f32 / 255.0,
                shading.blend,
            );
        }
    }
}

/// Applies one corner-colour factor set to a sampled texel.
fn shaded(rgba: [u8; 4], alpha: f32, factor: [f32; 4]) -> ([u8; 4], f32) {
    let mut out = rgba;
    for channel in 0..3 {
        out[channel] = round_channel(out[channel] as f32 * factor[channel]);
    }
    (out, alpha * factor[3])
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
            blend_mode: 0x10,
            blend_parameter: 0.0,
            corner_colors: [0x8080_80FF; 4],
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

    /// The reference's blend table, entry by entry, and the raster behaviour of
    /// each equation over an opaque destination.
    #[test]
    fn blend_modes_select_the_references_equations() {
        assert_eq!(SpriteBlend::of(0x10), SpriteBlend::Over);
        assert_eq!(SpriteBlend::of(0x00), SpriteBlend::Over);
        assert_eq!(SpriteBlend::of(0x11), SpriteBlend::Add);
        assert_eq!(SpriteBlend::of(0x12), SpriteBlend::Sub);
        assert_eq!(SpriteBlend::of(0x15), SpriteBlend::Sub);
        assert_eq!(SpriteBlend::of(0x13), SpriteBlend::Mul);
        assert_eq!(SpriteBlend::of(0x14), SpriteBlend::Screen);
        assert_eq!(SpriteBlend::of(0x16), SpriteBlend::Copy);
        assert_eq!(SpriteBlend::of(0x17), SpriteBlend::Over);
        assert!(SpriteBlend::modulates_2x(0x10));
        assert!(!SpriteBlend::modulates_2x(0x00));
    }

    /// Draws one 4x4 opaque-white quad with `alpha` over a mid-gray pixel and
    /// returns what the centre became.
    fn over_gray(blend_mode: u32, alpha: u8, colour: u32) -> [u8; 4] {
        let texture = texture(4, 4, [255, 255, 255, 255]);
        let mut canvas = Canvas::new(8, 8);
        for pixel in canvas.pixels_mut().chunks_exact_mut(4) {
            pixel.copy_from_slice(&[128, 128, 128, 255]);
        }
        let mut item = item([4.0, 4.0], [4.0, 4.0], 1.0);
        item.blend_mode = blend_mode;
        item.corner_colors = [colour; 4];
        draw_quad(
            &mut canvas,
            &texture,
            &item,
            Tint::IDENTITY,
            f32::from(alpha) / 255.0,
        );
        canvas.pixel(4, 4).expect("centre")
    }

    /// Each equation's arithmetic against the reference's software kernels:
    /// `d + (c - d) * a` with `c` the mode's own colour — white for source-over,
    /// `d + s` additive, `d - s` subtractive, `s * d / 255` multiply and
    /// `s + d - s * d / 255` screen.
    #[test]
    fn blend_equations_land_on_the_reference_values() {
        let white = 0xFFFF_FFFF;
        assert_eq!(
            over_gray(0x10, 128, white),
            [192, 192, 192, 255],
            "source-over: the reference's lerp, rounded"
        );
        assert_eq!(
            over_gray(0x11, 128, white),
            [192, 192, 192, 255],
            "additive saturates the sum, then alpha-blends it (ps_add_blend_func)"
        );
        assert_eq!(
            over_gray(0x12, 128, white),
            [64, 64, 64, 255],
            "subtractive bottoms the sum out at 0, then alpha-blends it"
        );
        assert_eq!(
            over_gray(0x13, 128, white),
            [128, 128, 128, 255],
            "white multiplies to the destination itself"
        );
        assert_eq!(
            over_gray(0x14, 128, white),
            [192, 192, 192, 255],
            "white screens the destination to 255, then alpha-blends it"
        );
        assert_eq!(
            over_gray(0x16, 128, white),
            [255, 255, 255, 128],
            "copy replaces the destination and its alpha"
        );

        // A dark source shows the difference between over and multiply.
        let black = 0x0000_00FF;
        assert_eq!(over_gray(0x10, 255, black), [0, 0, 0, 255]);
        assert_eq!(over_gray(0x13, 255, black), [0, 0, 0, 255]);
        assert_eq!(
            over_gray(0x12, 255, black),
            [128, 128, 128, 255],
            "subtracting black leaves the destination"
        );
    }

    /// The corner colours tint the quad: the neutral pair is an exact identity,
    /// MODULATE2X doubles, and a gradient reaches the corners it names.
    #[test]
    fn corner_colours_tint_the_quad() {
        let texture = texture(4, 4, [100, 100, 100, 255]);
        let draw = |colour: u32, blend_mode: u32| {
            let mut canvas = Canvas::new(1, 1);
            let mut item = item([0.5, 0.5], [1.0, 1.0], 1.0);
            item.blend_mode = blend_mode;
            item.corner_colors = [colour; 4];
            draw_quad(&mut canvas, &texture, &item, Tint::IDENTITY, 1.0);
            canvas.pixel(0, 0).expect("the single pixel")
        };
        assert_eq!(
            draw(0x8080_80FF, 0x10),
            [100, 100, 100, 255],
            "the D3D MODULATE2X neutral gray is exactly 1.0"
        );
        assert_eq!(
            draw(0xFFFF_FFFF, 0x00),
            [100, 100, 100, 255],
            "the MODULATE neutral white is exactly 1.0"
        );
        assert_eq!(
            draw(0xFFFF_FFFF, 0x10),
            [199, 199, 199, 255],
            "MODULATE2X doubles the texel (100 * 255/128 = 199.2)"
        );
        assert_eq!(
            draw(0x8000_00FF, 0x10),
            [100, 0, 0, 255],
            "one channel through the same 1.0 scale"
        );
        assert_eq!(draw(0xFF00_00FF, 0x10), [199, 0, 0, 255], "and doubled");
        assert_eq!(
            draw(0x4040_4080, 0x10),
            [50, 50, 50, 128],
            "0x40 through the 1.0 scale, and the corner alpha scales opacity"
        );
    }

    /// The four corners are interpolated across the quad, so a gradient tints
    /// each corner with its own colour.
    #[test]
    fn a_corner_gradient_reaches_each_corner() {
        let texture = texture(4, 4, [255, 255, 255, 255]);
        let mut canvas = Canvas::new(4, 4);
        let mut item = item([2.0, 2.0], [4.0, 4.0], 1.0);
        item.blend_mode = 0x10;
        // Top-left red, top-right green, bottom-right blue, bottom-left black.
        item.corner_colors = [0xFF00_00FF, 0x00FF_00FF, 0x0000_FFFF, 0x0000_00FF];
        draw_quad(&mut canvas, &texture, &item, Tint::IDENTITY, 1.0);
        let [r, g, b, _] = canvas.pixel(0, 0).expect("top-left");
        assert!(
            r > g && r > b && g < 96 && b < 96,
            "top-left is red: {:?}",
            (r, g, b)
        );
        let [r, g, b, _] = canvas.pixel(3, 0).expect("top-right");
        assert!(g > r && g > b, "top-right is green: {:?}", (r, g, b));
        let [r, g, b, _] = canvas.pixel(3, 3).expect("bottom-right");
        assert!(b > r && b > g, "bottom-right is blue: {:?}", (r, g, b));
        let [r, g, b, _] = canvas.pixel(0, 3).expect("bottom-left");
        assert!(
            r < 96 && g < 96 && b < 96,
            "bottom-left is black: {:?}",
            (r, g, b)
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
                        composite(&mut new, 1, 0, 0, rgba, alpha, SpriteBlend::Over);
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
