//! Layer blit methods, ported from krkrz's `tTVPBaseBitmap::Blt`
//! (`visual/LayerBitmapIntf.cpp:1083`) and its blend functors
//! (`visual/gl/blend_functor_c.h`, `visual/gl/blend_variation.h`).
//!
//! `GetBltMethodFromOperationModeAndDrawFace` (`LayerIntf.cpp:3779`) maps a
//! TJS operation mode plus the destination draw face to one of these methods;
//! `Blt` then picks one of four variation wrappers per method:
//!
//! - `normal_op`:    `a = src_alpha`
//! - `hda_op`:       `a = src_alpha`, destination alpha preserved
//! - `translucent`:  `a = (src_alpha * opacity) >> 8`
//! - `hda + opacity`: both of the above
//!
//! The blend functors themselves are written with packed 32-bit tricks in the
//! official sources; the per-channel forms below reproduce the same results,
//! including the `>> 8` rounding (255 alpha reaches 254, not 255).

/// `tTVPBBBltMethod`, limited to the values the layer API can produce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Blt {
    /// `bmCopy` / `TVP_BB_COPY_MAIN|MASK`: copy colour and alpha.
    CopyMask,
    /// `TVP_BB_COPY_MAIN`: copy colour, keep destination alpha.
    CopyColor,
    /// `TVP_BB_COPY_MASK`: copy the alpha channel, keep destination colour.
    CopyAlpha,
    /// `bmCopyOnAlpha` / `TVPCopyOpaqueImage`: `0xff000000 | src`.
    CopyOpaque,
    /// `bmAlphaOnAlpha` (`TVPAlphaBlend_d`): alpha blend onto a valid-alpha
    /// destination.
    AlphaOnAlpha,
    /// `bmAlpha` (`TVPAlphaBlend`): alpha blend ignoring destination alpha.
    Alpha,
    /// `bmAddAlphaOnAlpha` (`TVPAdditiveAlphaBlend_a`).
    AddAlphaOnAlpha,
    /// `bmAddAlpha` (`TVPAdditiveAlphaBlend`): premultiplied additive blend.
    AddAlpha,
    /// `bmAdd`/`bmSub`/`bmMul`/`bmDodge`/`bmDarken`/`bmLighten`/`bmScreen`.
    Blend(BlendOp),
    /// `bmPs*`: Photoshop blend modes. Their functors return colour only, so
    /// without `hda` the destination alpha becomes 0 (official `normal_op`).
    Ps(PsOp),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BlendOp {
    Add,
    Sub,
    Mul,
    Dodge,
    Darken,
    Lighten,
    Screen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PsOp {
    Normal,
    Add,
    Sub,
    Mul,
    Screen,
    Overlay,
    HardLight,
    SoftLight,
    ColorDodge,
    ColorDodge5,
    ColorBurn,
    Lighten,
    Darken,
    Difference,
    Difference5,
    Exclusion,
}

/// `tTJSNI_BaseLayer::GetBltMethodFromOperationModeAndDrawFace`
/// (`LayerIntf.cpp:3779`). `None` mirrors the official `met_set = false`,
/// which makes the caller throw `Not drawable face type`.
pub(crate) fn operation_mode_to_blt(mode: i64, draw_face: i64) -> Option<Blt> {
    const DF_ALPHA: i64 = 0;
    const DF_OPAQUE: i64 = 1;
    const DF_ADD_ALPHA: i64 = 4;
    let blt = match mode {
        13 => Blt::Ps(PsOp::Normal),
        14 => Blt::Ps(PsOp::Add),
        15 => Blt::Ps(PsOp::Sub),
        16 => Blt::Ps(PsOp::Mul),
        17 => Blt::Ps(PsOp::Screen),
        18 => Blt::Ps(PsOp::Overlay),
        19 => Blt::Ps(PsOp::HardLight),
        20 => Blt::Ps(PsOp::SoftLight),
        21 => Blt::Ps(PsOp::ColorDodge),
        22 => Blt::Ps(PsOp::ColorDodge5),
        23 => Blt::Ps(PsOp::ColorBurn),
        24 => Blt::Ps(PsOp::Lighten),
        25 => Blt::Ps(PsOp::Darken),
        26 => Blt::Ps(PsOp::Difference),
        27 => Blt::Ps(PsOp::Difference5),
        28 => Blt::Ps(PsOp::Exclusion),
        3 => Blt::Blend(BlendOp::Add),
        4 => Blt::Blend(BlendOp::Sub),
        5 => Blt::Blend(BlendOp::Mul),
        8 => Blt::Blend(BlendOp::Dodge),
        9 => Blt::Blend(BlendOp::Darken),
        10 => Blt::Blend(BlendOp::Lighten),
        11 => Blt::Blend(BlendOp::Screen),
        2 if draw_face == DF_ALPHA => Blt::AlphaOnAlpha,
        2 if draw_face == DF_ADD_ALPHA => Blt::Alpha,
        2 if draw_face == DF_OPAQUE => Blt::Alpha,
        2 => return None,
        12 if draw_face == DF_ALPHA => Blt::AddAlphaOnAlpha,
        12 if draw_face == DF_ADD_ALPHA => Blt::AddAlpha,
        12 if draw_face == DF_OPAQUE => Blt::AddAlpha,
        12 => return None,
        1 if draw_face == DF_ALPHA || draw_face == DF_ADD_ALPHA => Blt::CopyOpaque,
        1 if draw_face == DF_OPAQUE => Blt::CopyMask,
        1 => return None,
        _ => return None,
    };
    Some(blt)
}

/// One row of `tTVPBaseBitmap::Blt`. `opacity` is the official `opa`
/// (0..=255) and `hda` is `HoldAlpha` ("hold destination alpha").
pub(crate) fn blt_row(dest: &mut [u8], src: &[u8], blt: Blt, opacity: u32, hda: bool) {
    let opacity = opacity.min(255);
    if opacity == 0 {
        return;
    }
    for (d, s) in dest.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let d_px = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);
        let s_px = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
        let out = blt_pixel(d_px, s_px, blt, opacity, hda);
        d.copy_from_slice(&out.to_le_bytes());
    }
}

pub(crate) fn blt_pixel(d: u32, s: u32, blt: Blt, opacity: u32, hda: bool) -> u32 {
    match blt {
        Blt::CopyMask => s,
        Blt::CopyColor => (d & 0xff00_0000) | (s & 0x00ff_ffff),
        Blt::CopyAlpha => (d & 0x00ff_ffff) | (s & 0xff00_0000),
        Blt::CopyOpaque => {
            if opacity == 255 {
                // `TVPCopyOpaqueImage`.
                0xff00_0000 | (s & 0x00ff_ffff)
            } else {
                // `TVPConstAlphaBlend_d`: constant blend that keeps the
                // destination alpha.
                const_alpha_blend_d(d, s, opacity)
            }
        }
        Blt::AlphaOnAlpha => alpha_blend_d(d, s, variation_alpha(s, opacity)),
        Blt::Alpha => alpha_blend(d, s, variation_alpha(s, opacity)),
        Blt::AddAlpha => additive_alpha_blend(d, s, variation_alpha(s, opacity)),
        Blt::AddAlphaOnAlpha => additive_alpha_blend(d, s, variation_alpha(s, opacity)),
        Blt::Blend(op) => {
            let a = variation_alpha(s, opacity);
            let out = blend_op(op, d, s, a);
            if hda {
                (d & 0xff00_0000) | (out & 0x00ff_ffff)
            } else {
                out
            }
        }
        Blt::Ps(op) => {
            let a = variation_alpha(s, opacity);
            let blended = ps_op(op, d, s, a);
            let out = ps_alpha_blend(d, blended, a);
            if hda {
                (d & 0xff00_0000) | out
            } else {
                out
            }
        }
    }
}

fn src_alpha(px: u32) -> u32 {
    px >> 24
}

/// `translucent_op`: `a = (src_alpha * opacity) >> 8`.
fn variation_alpha(s: u32, opacity: u32) -> u32 {
    if opacity == 255 {
        src_alpha(s)
    } else {
        (src_alpha(s) * opacity) >> 8
    }
}

fn channel(px: u32, shift: u32) -> i32 {
    ((px >> shift) & 0xff) as i32
}

fn pack(r: i32, g: i32, b: i32) -> u32 {
    ((r.clamp(0, 255) as u32) << 16)
        | ((g.clamp(0, 255) as u32) << 8)
        | (b.clamp(0, 255) as u32)
}

/// `alpha_blend_func` (`blend_functor_c.h:64`): `d + ((s - d) * a >> 8)` per
/// colour channel, destination alpha dropped.
fn alpha_blend(d: u32, s: u32, a: u32) -> u32 {
    let a = a as i32;
    pack(
        channel(d, 16) + (((channel(s, 16) - channel(d, 16)) * a) >> 8),
        channel(d, 8) + (((channel(s, 8) - channel(d, 8)) * a) >> 8),
        channel(d, 0) + (((channel(s, 0) - channel(d, 0)) * a) >> 8),
    )
}

/// `dest_alpha_op<alpha_blend_func>` (`blend_variation.h:124`): the source is
/// faded onto the destination alpha, then composited.
fn alpha_blend_d(d: u32, s: u32, a: u32) -> u32 {
    let sa = a;
    let da = src_alpha(d);
    let sopa = if da == 0 {
        255
    } else {
        // TVPOpacityOnOpacityTable[sa][da] == (sa*255)/da * 255/(255-sa + ...)
        // expressed as the float form in the official `NOT_USE_TABLE` branch.
        let bt = sa as f64 / 255.0;
        let at = da as f64 / 255.0;
        let c = (bt / at) / (1.0 - bt + bt / at);
        (c * 255.0) as u32
    };
    let dest_alpha = 255 - (255 - da) * (255 - sa) / 255;
    (dest_alpha << 24) | alpha_blend(d, s, sopa)
}

/// `additive_alpha_blend_functor` (`blend_functor_c.h:141`): premultiplied
/// additive blend.
fn additive_alpha_blend(d: u32, s: u32, a: u32) -> u32 {
    let a = a as i32;
    let fade = |c: i32| ((c * a) >> 8).clamp(0, 255);
    let out = pack(
        fade(channel(s, 16)) + channel(d, 16),
        fade(channel(s, 8)) + channel(d, 8),
        fade(channel(s, 0)) + channel(d, 0),
    );
    let alpha = (channel(d, 24) + fade(channel(s, 24))).clamp(0, 255) as u32;
    (alpha << 24) | out
}

/// `add_blend_func` (`blend_functor_c.h:167`): `src` faded by alpha, then a
/// saturating add on every channel (alpha included).
fn blend_op(op: BlendOp, d: u32, s: u32, a: u32) -> u32 {
    let fade = |px: u32| {
        let a = a as i32;
        pack(
            (channel(px, 16) * a) >> 8,
            (channel(px, 8) * a) >> 8,
            (channel(px, 0) * a) >> 8,
        ) | ((((channel(px, 24) * a) >> 8).clamp(0, 255) as u32) << 24)
    };
    match op {
        BlendOp::Add => {
            let s = fade(s);
            sat_add(d, s)
        }
        BlendOp::Sub => {
            let s = fade(s);
            sat_sub(d, s)
        }
        BlendOp::Mul => {
            let s = fade(s);
            pack(
                (channel(d, 16) * channel(s, 16)) >> 8,
                (channel(d, 8) * channel(s, 8)) >> 8,
                (channel(d, 0) * channel(s, 0)) >> 8,
            ) | ((((channel(d, 24) * channel(s, 24)) >> 8).clamp(0, 255) as u32) << 24)
        }
        BlendOp::Dodge => {
            let s = fade(s);
            pack(
                dodge(channel(d, 16), channel(s, 16)),
                dodge(channel(d, 8), channel(s, 8)),
                dodge(channel(d, 0), channel(s, 0)),
            ) | (d & 0xff00_0000)
        }
        BlendOp::Darken => {
            let s = fade(s);
            pack(
                channel(d, 16).min(channel(s, 16)),
                channel(d, 8).min(channel(s, 8)),
                channel(d, 0).min(channel(s, 0)),
            ) | (d & 0xff00_0000)
        }
        BlendOp::Lighten => {
            let s = fade(s);
            pack(
                channel(d, 16).max(channel(s, 16)),
                channel(d, 8).max(channel(s, 8)),
                channel(d, 0).max(channel(s, 0)),
            ) | (d & 0xff00_0000)
        }
        BlendOp::Screen => {
            let s = fade(s);
            pack(
                255 - (((255 - channel(d, 16)) * (255 - channel(s, 16))) >> 8),
                255 - (((255 - channel(d, 8)) * (255 - channel(s, 8))) >> 8),
                255 - (((255 - channel(d, 0)) * (255 - channel(s, 0))) >> 8),
            ) | (d & 0xff00_0000)
        }
    }
}

/// `ps_alpha_blend_func` (`blend_functor_c.h:345`): linear interpolation of
/// the blended colour by the source alpha.
fn ps_alpha_blend(d: u32, s: u32, a: u32) -> u32 {
    let a = a as i32;
    pack(
        channel(d, 16) + (((channel(s, 16) - channel(d, 16)) * a) >> 8),
        channel(d, 8) + (((channel(s, 8) - channel(d, 8)) * a) >> 8),
        channel(d, 0) + (((channel(s, 0) - channel(d, 0)) * a) >> 8),
    )
}

/// The `ps_*_blend_func` colour functions (`blend_functor_c.h:353`..`:560`).
fn ps_op(op: PsOp, d: u32, s: u32, a: u32) -> u32 {
    let dc = [channel(d, 16), channel(d, 8), channel(d, 0)];
    let sc = [channel(s, 16), channel(s, 8), channel(s, 0)];
    let mut out = [0i32; 3];
    for i in 0..3 {
        let (d, s) = (dc[i], sc[i]);
        out[i] = match op {
            PsOp::Normal => s,
            PsOp::Add => (d + s).min(255),
            PsOp::Sub => (d - s).max(0),
            PsOp::Mul => (d * s) >> 8,
            PsOp::Screen => 255 - (((255 - d) * (255 - s)) >> 8),
            // `ps_overlay_table::TABLE[s][d]` (`blend_function.cpp:465`).
            PsOp::Overlay => {
                if d < 128 {
                    (s * d * 2) / 255
                } else {
                    (s + d) * 2 - (s * d * 2) / 255 - 255
                }
            }
            // Hard light is overlay with the operands swapped.
            PsOp::HardLight => {
                if s < 128 {
                    (d * s * 2) / 255
                } else {
                    (d + s) * 2 - (d * s * 2) / 255 - 255
                }
            }
            // `ps_soft_light_table::TABLE[s][d]` (`blend_function.cpp:458`).
            PsOp::SoftLight => {
                let value = if s >= 128 {
                    (d as f64 / 255.0).powf(128.0 / s as f64)
                } else {
                    (d as f64 / 255.0).powf((1.0 - s as f64 / 255.0) / 0.5)
                };
                (value * 255.0) as i32
            }
            // `ps_color_dodge_table` (`blend_function.cpp:461`).
            PsOp::ColorDodge | PsOp::ColorDodge5 => {
                if 255 - s <= d {
                    255
                } else {
                    (d * 255) / (255 - s)
                }
            }
            // `ps_color_burn_table` (`blend_function.cpp:463`).
            PsOp::ColorBurn => {
                if s <= 255 - d {
                    0
                } else {
                    255 - ((255 - d) * 255) / s
                }
            }
            PsOp::Lighten => d.max(s),
            PsOp::Darken => d.min(s),
                PsOp::Difference => (d - s).abs(),
            PsOp::Difference5 => ((d - ((s * a as i32) >> 8))).abs(),
            PsOp::Exclusion => d + s - (s * d * 2) / 255,
        };
    }
    pack(out[0], out[1], out[2])
}

fn sat_add(a: u32, b: u32) -> u32 {
    pack(
        channel(a, 16) + channel(b, 16),
        channel(a, 8) + channel(b, 8),
        channel(a, 0) + channel(b, 0),
    ) | (((channel(a, 24) + channel(b, 24)).clamp(0, 255) as u32) << 24)
}

fn sat_sub(a: u32, b: u32) -> u32 {
    pack(
        channel(a, 16) - channel(b, 16),
        channel(a, 8) - channel(b, 8),
        channel(a, 0) - channel(b, 0),
    ) | (((channel(a, 24) - channel(b, 24)).clamp(0, 255) as u32) << 24)
}

fn dodge(d: i32, s: i32) -> i32 {
    if 255 - s <= d {
        255
    } else {
        (d * 255) / (255 - s)
    }
}

/// `TVPConstAlphaBlend_d`: constant blend that keeps the destination alpha.
fn const_alpha_blend_d(d: u32, s: u32, opacity: u32) -> u32 {
    let a = opacity as i32;
    (d & 0xff00_0000)
        | pack(
            channel(d, 16) + (((channel(s, 16) - channel(d, 16)) * a) >> 8),
            channel(d, 8) + (((channel(s, 8) - channel(d, 8)) * a) >> 8),
            channel(d, 0) + (((channel(s, 0) - channel(d, 0)) * a) >> 8),
        )
}
