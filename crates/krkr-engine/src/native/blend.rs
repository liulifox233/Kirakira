//! Layer blit methods, ported from krkrz's `tTVPBaseBitmap::Blt`
//! (`visual/LayerBitmapIntf.cpp:1083`) and its blend functors
//! (`visual/gl/blend_functor_c.h`, `visual/gl/blend_variation.h`).
//!
//! `GetBltMethodFromOperationModeAndDrawFace` (`LayerIntf.cpp:3791`) maps a
//! TJS operation mode plus the destination draw face to one of these methods;
//! `Blt` then picks a variation wrapper per method from `tTVPBBBltMethod`:
//!
//! - `opa == 255, !hda`: the plain functor (`normal_op` / 2-arg functor)
//! - `opa == 255, hda`:  result colour, destination alpha kept
//! - `opa < 255, !hda`:  the `_o` wrapper (source alpha scaled by `opa`)
//! - `opa < 255, hda`:   `_HDA_o`
//!
//! Three families of functors exist and they do not share an alpha model:
//!
//! - `alpha_blend*` (bmAlpha/bmCopy family) mixes by the source alpha.
//! - `premulalpha_blend*` (bmAddAlpha, `LayerBitmapIntf.cpp:1360-1415`) is the
//!   additive-alpha ("premultiplied alpha") family: `TVPAdditiveAlphaBlend` is
//!   `premulalpha_blend_n_a_func` (`blend_util_func.h:33`), which scales the
//!   *destination* by `255 - src_alpha` and adds the raw source. `ltAddAlpha`
//!   layer data is premultiplied, so `bmAlphaOnAddAlpha` /`bmCopyOnAddAlpha`
//!   convert the source through `alpha_to_premulalpha` first.
//! - the `nsa` family (`add/sub/mul/dodge/darken/lighten/screen`) ignores the
//!   source alpha completely: `DEFINE_BLEND_MIN_VARIATION` (`blend_functor_c.h:30`)
//!   only ever exposes the `opa` as the blend amount. `bmAddAlphaOnAlpha` is a
//!   documented no-op (`LayerBitmapIntf.cpp:1383-1386`).
//!
//! The functors are written with packed-32-bit swar tricks in the official
//! sources; the ports below transliterate the same expressions with wrapping
//! arithmetic so the byte results are identical.

/// `tTVPBBBltMethod`, limited to the values the layer API can produce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Blt {
    /// `bmCopy` (`TVP_BB_COPY_MAIN|MASK` for `copyRect`): copy colour and
    /// alpha. The blt itself follows `Blt`'s `bmCopy` cases.
    CopyMask,
    /// `TVP_BB_COPY_MAIN`: copy colour, keep destination alpha.
    CopyColor,
    /// `TVP_BB_COPY_MASK`: copy the alpha channel, keep destination colour.
    CopyAlpha,
    /// `bmCopyOnAlpha` (`TVPCopyOpaqueImage` / `TVPConstAlphaBlend_d`).
    CopyOpaqueD,
    /// `bmCopyOnAddAlpha` (`TVPCopyOpaqueImage` / `TVPConstAlphaBlend_a`).
    CopyOpaqueA,
    /// `bmAlphaOnAlpha` (`TVPAlphaBlend_d`): alpha blend onto a valid-alpha
    /// destination.
    AlphaOnAlpha,
    /// `bmAlpha` (`TVPAlphaBlend`): alpha blend ignoring destination alpha.
    Alpha,
    /// `bmAlphaOnAddAlpha` (`TVPAlphaBlend_a`): plain-alpha source onto
    /// premultiplied destination.
    AlphaOnAddAlpha,
    /// `bmAddAlphaOnAlpha`: not implemented in the reference, a no-op
    /// (`LayerBitmapIntf.cpp:1383-1386`).
    AddAlphaOnAlpha,
    /// `bmAddAlpha` (`TVPAdditiveAlphaBlend`): premultiplied additive blend.
    AddAlpha,
    /// `bmAddAlphaOnAddAlpha` (`TVPAdditiveAlphaBlend_a`/`_ao`).
    AddAlphaOnAddAlpha,
    /// `bmAdd`/`bmSub`/`bmMul`/`bmDodge`/`bmDarken`/`bmLighten`/`bmScreen`.
    Blend(BlendOp),
    /// `bmPs*`: Photoshop blend modes.
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
/// (`LayerIntf.cpp:3791`). `None` mirrors the official `met_set = false`,
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
        2 if draw_face == DF_ADD_ALPHA => Blt::AlphaOnAddAlpha,
        2 if draw_face == DF_OPAQUE => Blt::Alpha,
        2 => return None,
        12 if draw_face == DF_ALPHA => Blt::AddAlphaOnAlpha,
        12 if draw_face == DF_ADD_ALPHA => Blt::AddAlphaOnAddAlpha,
        12 if draw_face == DF_OPAQUE => Blt::AddAlpha,
        12 => return None,
        1 if draw_face == DF_ALPHA => Blt::CopyOpaqueD,
        1 if draw_face == DF_ADD_ALPHA => Blt::CopyOpaqueA,
        1 if draw_face == DF_OPAQUE => Blt::CopyMask,
        1 => return None,
        _ => return None,
    };
    Some(blt)
}

/// `TVPIsTypeUsingAlpha` (`visual/drawable.h:55-75`): `ltAlpha` and the
/// Photoshop blend family carry straight alpha.
fn layer_type_uses_alpha(layer_type: i64) -> bool {
    layer_type == 2 || (13..=28).contains(&layer_type)
}

/// `TVPIsTypeUsingAddAlpha` (`visual/drawable.h:77-80`): `ltAddAlpha` is the
/// only premultiplied-alpha layer type.
fn layer_type_uses_add_alpha(layer_type: i64) -> bool {
    layer_type == 12
}

/// `TVPIsTypeUsingAlphaChannel` (`visual/drawable.h:82-87`).
fn layer_type_uses_alpha_channel(layer_type: i64) -> bool {
    layer_type_uses_add_alpha(layer_type) || layer_type_uses_alpha(layer_type)
}

/// The destination face `BltImage` and `operation_mode_to_blt` see for a
/// destination bitmap of the given layer type: the reference asks
/// `TVPIsTypeUsingAlpha` / `TVPIsTypeUsingAddAlpha` (`LayerIntf.cpp:5191-5203`).
fn draw_face_for_layer_type(layer_type: i64) -> i64 {
    if layer_type_uses_alpha(layer_type) {
        0
    } else if layer_type_uses_add_alpha(layer_type) {
        4
    } else {
        1
    }
}

/// `tTJSNI_BaseLayer::BltImage` (`LayerIntf.cpp:5164-5364`): how a child layer
/// of type `draw_type` is composited into a bitmap that belongs to a layer of
/// type `dest_type`, plus the `hda` flag that call passes to
/// `tTVPBaseBitmap::Blt` (`:5363`). `None` mirrors the reference's early
/// returns: `ltBinder` children draw nothing (`:5185-5187`) and a type outside
/// the `tTVPLayerType` table has no case at all (`:5359-5360`).
pub(crate) fn blt_image_for_layer_type(draw_type: i64, dest_type: i64) -> Option<(Blt, bool)> {
    if draw_type == 0 {
        return None;
    }
    let blt = operation_mode_to_blt(draw_type, draw_face_for_layer_type(dest_type))?;
    // Only the additive and Photoshop families take `hda` from the destination
    // having an alpha channel (`:5211`-`:5257`); `ltOpaque`/`ltAlpha`/
    // `ltAddAlpha` pick `OnAlpha`/`OnAddAlpha` variants by face instead.
    let hda = matches!(draw_type, 3..=11 | 13..=28) && layer_type_uses_alpha_channel(dest_type);
    Some((blt, hda))
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
    let opa = opacity.min(255);
    match blt {
        // `bmCopy`: the `opa == 255 && !hda` case is `Blt`'s `CopyRect` fast
        // path; the rest are the `const_alpha_blend` variants
        // (`LayerBitmapIntf.cpp:1267-1287`).
        Blt::CopyMask => match (opa == 255, hda) {
            (true, false) => s,
            (true, true) => (d & 0xff00_0000) | (s & 0x00ff_ffff),
            (false, false) => const_alpha_blend(d, s, opa),
            (false, true) => const_alpha_blend_hda(d, s, opa),
        },
        Blt::CopyColor => (d & 0xff00_0000) | (s & 0x00ff_ffff),
        Blt::CopyAlpha => (d & 0x00ff_ffff) | (s & 0xff00_0000),
        Blt::CopyOpaqueD => {
            if opa == 255 {
                color_opaque(s)
            } else {
                // `bmCopyOnAlpha` keeps the destination alpha.
                const_alpha_blend_d(d, s, opa)
            }
        }
        Blt::CopyOpaqueA => {
            if opa == 255 {
                color_opaque(s)
            } else {
                // `bmCopyOnAddAlpha` composites onto a premultiplied
                // destination (`const_alpha_blend_a_functor`).
                const_alpha_blend_a(d, s, opa)
            }
        }
        Blt::AlphaOnAlpha => alpha_blend_d(d, s, variation_alpha(s, opa)),
        Blt::Alpha => blend4_alpha(d, s, opa, hda),
        Blt::AlphaOnAddAlpha => {
            if opa == 255 {
                // `TVPAlphaBlend_a`: the premultiplied destination needs the
                // source premultiplied first (`alpha_to_premulalpha`).
                alpha_blend_a(d, s)
            } else {
                alpha_blend_ao(d, s, opa)
            }
        }
        // `bmAddAlphaOnAlpha`: "Not yet implemented" in the reference
        // (`LayerBitmapIntf.cpp:1383-1386`).
        Blt::AddAlphaOnAlpha => d,
        Blt::AddAlpha => blend4_add_alpha(d, s, opa, hda),
        Blt::AddAlphaOnAddAlpha => {
            if opa == 255 {
                premulalpha_blend_a_a(d, s)
            } else {
                premulalpha_blend_a_a_o(d, s, opa)
            }
        }
        Blt::Blend(op) => blend4_min_variation(d, s, op, opa, hda),
        Blt::Ps(op) => {
            let a = variation_alpha(s, opa);
            let blended = ps_op(op, d, s, a);
            let out = ps_alpha_blend(d, blended, a);
            if hda { (d & 0xff00_0000) | out } else { out }
        }
    }
}

fn src_alpha(px: u32) -> u32 {
    px >> 24
}

/// `translucent_op`'s alpha (`blend_variation.h:37-47`): `a = (sa * opa) >> 8`,
/// or the raw source alpha at `opa == 255` (`normal_op`). This is the alpha
/// model of the `alpha_blend` and Photoshop families only; the `nsa` family
/// never consults the source alpha.
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
    ((r.clamp(0, 255) as u32) << 16) | ((g.clamp(0, 255) as u32) << 8) | (b.clamp(0, 255) as u32)
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

/// `dest_alpha_op<alpha_blend_func>` (`blend_variation.h:108`): the source is
/// faded onto the destination alpha, then composited. The weight is the
/// official `TVPOpacityOnOpacityTable` value (the header's `#ifdef
/// NOT_USE_TABLE` branch computes the same float chain per pixel).
fn alpha_blend_d(d: u32, s: u32, a: u32) -> u32 {
    let sa = a;
    let da = src_alpha(d);
    let sopa = opacity_on_opacity_table(da, sa);
    let dest_alpha = negative_mul_table(da, sa);
    (dest_alpha << 24) | alpha_blend(d, s, sopa)
}

/// `bmAlpha` (`LayerBitmapIntf.cpp:1303`): `TVP_BLEND_4(TVPAlphaBlend)`.
fn blend4_alpha(d: u32, s: u32, opa: u32, hda: bool) -> u32 {
    let out = alpha_blend(d, s, variation_alpha(s, opa));
    if hda {
        (d & 0xff00_0000) | (out & 0x00ff_ffff)
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// Additive alpha ("premultiplied alpha") family.
//
// `ltAddAlpha`/`dfAddAlpha` pixel data is premultiplied:
// `TVPAdditiveAlphaBlend` is `premulalpha_blend_n_a_func`
// (`blend_util_func.h:33-39`) — `Di = sat(Si, (1-Sa)*Di)`, with the packed
// per-byte form scaling the three colour bytes of the destination by
// `255 - src_alpha` and saturating the raw source onto them (the destination
// alpha is added to, not scaled).
// ---------------------------------------------------------------------------

/// `saturated_u8_add_func` (`blend_util_func.h:20`).
fn swar_sat_add(a: u32, b: u32) -> u32 {
    let tmp = (a & b).wrapping_add(((a ^ b) >> 1) & 0x7f7f_7f7f) & 0x8080_8080;
    let tmp = (tmp << 1).wrapping_sub(tmp >> 7);
    a.wrapping_add(b).wrapping_sub(tmp) | tmp
}

/// `premulalpha_blend_n_a_func` (`blend_util_func.h:33`): scale the
/// destination colour by `255 - src_alpha`, then saturate-add the raw source
/// (alpha included).
fn premulalpha_blend_n_a(d: u32, s: u32) -> u32 {
    let sopa = (!s) >> 24;
    swar_sat_add(
        (((d & 0xff00ff).wrapping_mul(sopa) >> 8) & 0xff00ff)
            .wrapping_add(((d & 0xff00).wrapping_mul(sopa) >> 8) & 0xff00),
        s,
    )
}

/// `premulalpha_blend_o_functor` (`blend_functor_c.h:107`): the whole source
/// (alpha included) is scaled by `opa` before the additive-alpha blend.
fn premulalpha_blend_o(d: u32, s: u32, opa: u32) -> u32 {
    premulalpha_blend_n_a(d, premulalpha_blend_o_scale(s, opa))
}

/// `premulalpha_blend_HDA_o_functor` (`blend_functor_c.h:125`): the reference
/// computes `dopa` and then never uses it in the returned value, so this is
/// the same blend as the non-HDA `_o` variant.
fn premulalpha_blend_hda_o(d: u32, s: u32, opa: u32) -> u32 {
    let _dopa = d & 0xff00_0000;
    let s = premulalpha_blend_o_scale(s, opa);
    let sopa = (!s) >> 24;
    let a = (((d & 0xff00ff).wrapping_mul(sopa) >> 8) & 0xff00ff)
        .wrapping_add(((d & 0xff00).wrapping_mul(sopa) >> 8) & 0xff00);
    swar_sat_add(a, s)
}

/// `translucent`-style source scaling shared by the additive-alpha `_o`
/// variants.
fn premulalpha_blend_o_scale(s: u32, opa: u32) -> u32 {
    ((s & 0xff00ff).wrapping_mul(opa) >> 8) & 0xff00ff
        | (((s >> 8) & 0xff00ff).wrapping_mul(opa) & 0xff00ff00)
}

/// `premulalpha_blend_a_a_func` (`blend_util_func.h:66`): premultiplied source
/// onto a premultiplied destination.
fn premulalpha_blend_a_a(d: u32, s: u32) -> u32 {
    let mut da = d >> 24;
    let sa = s >> 24;
    da = da.wrapping_add(sa).wrapping_sub((da * sa) >> 8);
    da -= da >> 8;
    let sa_inv = sa ^ 0xff;
    let sc = s & 0x00ff_ffff;
    (da << 24).wrapping_add(swar_sat_add(
        (((d & 0xff00ff).wrapping_mul(sa_inv) >> 8) & 0xff00ff)
            .wrapping_add(((d & 0xff00).wrapping_mul(sa_inv) >> 8) & 0xff00),
        sc,
    ))
}

/// `premulalpha_blend_a_a_o_func` (`blend_util_func.h:78`).
fn premulalpha_blend_a_a_o(d: u32, s: u32, opa: u32) -> u32 {
    premulalpha_blend_a_a(d, premulalpha_blend_o_scale(s, opa))
}

/// `mul_color` (`blend_util_func.h:86`): scale a colour's components by `fac`
/// (the alpha byte is not part of either masked lane).
fn mul_color(color: u32, fac: u32) -> u32 {
    ((((color & 0x00ff00).wrapping_mul(fac)) & 0x00ff_0000)
        .wrapping_add((color & 0xff00ff).wrapping_mul(fac) & 0xff00_ff00))
        >> 8
}

/// `alpha_to_additive_alpha` (`blend_util_func.h:93`).
fn alpha_to_additive_alpha(a: u32) -> u32 {
    mul_color(a, a >> 24).wrapping_add(a & 0xff00_0000)
}

/// `TVPDivTable` (`visual/tvpgl.c:251-260`): `b * 255 / a`, truncated and
/// capped at 255, and 0 for a zero divisor.
fn div_table(alpha: u32, value: u32) -> u32 {
    if alpha == 0 {
        0
    } else {
        (value * 255 / alpha).min(255)
    }
}

/// `convet_alpha_to_premulalpha_functor` (`blend_functor_c.h:871`), the
/// `tvpgl.c`-installed `TVPConvertAlphaToAdditiveAlpha` (`:585` →
/// `TVP_convet_alpha_to_premulalpha` `blend_function.cpp:147`): rewrite one
/// straight-alpha pixel as a premultiplied ("additive alpha") one. Its body is
/// `alpha_and_color_to_premulalpha_func` (`blend_util_func.h:133`), which is
/// the per-pixel form of `alpha_to_additive_alpha` above.
pub(crate) fn alpha_pixel_to_additive_alpha(px: u32) -> u32 {
    alpha_to_additive_alpha(px)
}

/// `convet_premulalpha_to_alpha_functor` (`blend_functor_c.h:861`), installed
/// as `TVPConvertAdditiveAlphaToAlpha` (`blend_function.cpp:584`): colour =
/// `colour * 255 / alpha` through `TVPDivTable`, alpha byte kept.
pub(crate) fn alpha_pixel_to_alpha(px: u32) -> u32 {
    let alpha = px >> 24;
    (px & 0xff00_0000)
        | (div_table(alpha, (px >> 16) & 0xff) << 16)
        | (div_table(alpha, (px >> 8) & 0xff) << 8)
        | div_table(alpha, px & 0xff)
}

/// `alpha_blend_a_functor` (`blend_functor_c.h:76`): plain alpha source mixed
/// onto a premultiplied destination, so the source is premultiplied first
/// (its alpha byte is kept).
fn alpha_blend_a(d: u32, s: u32) -> u32 {
    premulalpha_blend_a_a(d, alpha_to_additive_alpha(s))
}

/// `alpha_blend_ao_functor` (`blend_functor_c.h:85`) →
/// `premulalpha_blend_a_d_o_func` (`blend_util_func.h:102`): only the source
/// alpha is scaled by `opa`, the colour bytes are premultiplied afterwards.
fn alpha_blend_ao(d: u32, s: u32, opa: u32) -> u32 {
    let s = (s & 0x00ff_ffff) | ((((s >> 24) * opa) >> 8) << 24);
    premulalpha_blend_a_a(d, alpha_to_additive_alpha(s))
}

/// `bmAddAlpha` (`LayerBitmapIntf.cpp:1360`):
/// `TVP_BLEND_4(TVPAdditiveAlphaBlend)`.
fn blend4_add_alpha(d: u32, s: u32, opa: u32, hda: bool) -> u32 {
    if opa == 255 {
        let out = premulalpha_blend_n_a(d, s);
        if hda {
            (d & 0xff00_0000) | (out & 0x00ff_ffff)
        } else {
            out
        }
    } else if hda {
        premulalpha_blend_hda_o(d, s, opa)
    } else {
        premulalpha_blend_o(d, s, opa)
    }
}

/// `const_alpha_blend_functor` (`blend_functor_c.h:572`): constant alpha lerp
/// including the alpha byte (`bmCopy` with opacity).
fn const_alpha_blend(d: u32, s: u32, opa: u32) -> u32 {
    let d1 = d & 0xff00ff;
    let d1 = d1.wrapping_add((s & 0xff00ff).wrapping_sub(d1).wrapping_mul(opa) >> 8) & 0xff00ff;
    let dm = d & 0xff00;
    let sm = s & 0xff00;
    d1 | ((dm.wrapping_add(sm.wrapping_sub(dm).wrapping_mul(opa) >> 8)) & 0xff00)
}

/// `const_alpha_blend_hda_functor` (`blend_functor_c.h:646`).
fn const_alpha_blend_hda(d: u32, s: u32, opa: u32) -> u32 {
    let d1 = d & 0xff00ff;
    let d1 = d1.wrapping_add((s & 0xff00ff).wrapping_sub(d1).wrapping_mul(opa) >> 8) & 0xff00ff
        | (d & 0xff00_0000);
    let dm = d & 0xff00;
    let sm = s & 0xff00;
    d1 | ((dm.wrapping_add(sm.wrapping_sub(dm).wrapping_mul(opa) >> 8)) & 0xff00)
}

fn color_opaque(s: u32) -> u32 {
    0xff00_0000 | (s & 0x00ff_ffff)
}

// ---------------------------------------------------------------------------
// The `nsa` ("no source alpha") family: bmAdd/bmSub/bmMul/bmDodge/bmDarken/
// bmLighten/bmScreen, `DEFINE_BLEND_MIN_VARIATION` (`blend_functor_c.h:30`).
// The wrappers are `translucent_nsa_op` / `hda_nsa_op` /
// `hda_translucent_nsa_op` (`blend_variation.h:48-104`), none of which read
// the source alpha: `opa` is the only blend amount.
// ---------------------------------------------------------------------------

/// `add_blend_functor` (`blend_functor_c.h:172`), also
/// `saturated_u8_add_func`: saturating add of every byte.
fn add_blend_raw(a: u32, b: u32) -> u32 {
    swar_sat_add(a, b)
}

/// `add_blend_func` (`blend_functor_c.h:180`): fade the source by `a`, then
/// saturate-add (alpha included).
fn add_blend_faded(d: u32, s: u32, a: u32) -> u32 {
    let s = ((s & 0x00ff00).wrapping_mul(a) >> 8) & 0x00ff00
        | ((s & 0xff00ff).wrapping_mul(a) >> 8) & 0xff00ff;
    add_blend_raw(d, s)
}

/// `sub_blend_functor` (`blend_functor_c.h:190`): `d - (255 - s)` saturated
/// (the neutral value of `ltSubtractive` is white), which the packed form
/// expresses as `d + s - overflow`.
fn sub_blend_raw(d: u32, s: u32) -> u32 {
    let tmp = (s & d).wrapping_add(((s ^ d) >> 1) & 0x7f7f_7f7f) & 0x8080_8080;
    let tmp = (tmp << 1).wrapping_sub(tmp >> 7);
    s.wrapping_add(d).wrapping_sub(tmp) & tmp
}

/// `sub_blend_func` (`blend_functor_c.h:197`): the source's complement is
/// faded by `a`, then subtracted.
fn sub_blend_faded(d: u32, s: u32, a: u32) -> u32 {
    let s = !s;
    let s = !(((s & 0x00ff00).wrapping_mul(a) >> 8) & 0x00ff00
        | (s & 0xff00ff).wrapping_mul(a) >> 8 & 0xff00ff);
    sub_blend_raw(d, s)
}

/// `mul_blend_functor` (`blend_functor_c.h:208`): the product of both bytes'
/// high halves; the alpha byte of the result is 0.
fn mul_blend_raw(d: u32, s: u32) -> u32 {
    let mut tmp = ((d & 0xff) * (s & 0xff)) & 0xff00;
    tmp |= (((d & 0xff00) >> 8) * (s & 0xff00)) & 0xff_0000;
    tmp |= (((d & 0xff_0000) >> 16) * (s & 0xff_0000)) & 0xff00_0000;
    tmp >> 8
}

/// `mul_blend_func` (`blend_functor_c.h:216`).
fn mul_blend_faded(d: u32, s: u32, a: u32) -> u32 {
    let s = !s;
    let s = !(((s & 0x00ff00).wrapping_mul(a) >> 8) & 0x00ff00
        | ((s & 0xff00ff).wrapping_mul(a) >> 8) & 0xff00ff);
    mul_blend_raw(d, s)
}

/// `TVPRecipTable256[i] = 65536 / i` (`visual/tvpgl.c:172`).
fn recip_table256(index: u32) -> u32 {
    if index == 0 { 65536 } else { 65536 / index }
}

/// The `(tmp | (~(tmp - limit) >> 31)) & 0xff` saturate of
/// `color_dodge_blend_functor` (`blend_functor_c.h:227`).
fn dodge_saturate(value: u32, limit: u32) -> u32 {
    if value >= limit { 0xffff_ffff } else { value }
}

/// `color_dodge_blend_functor` (`blend_functor_c.h:227`).
fn color_dodge_raw(d: u32, s: u32) -> u32 {
    let tmp2 = !s;
    let tmp = (d & 0xff).wrapping_mul(recip_table256(tmp2 & 0xff)) >> 8;
    let mut tmp3 = dodge_saturate(tmp, 0x100) & 0xff;
    let tmp = ((d & 0xff00) >> 8).wrapping_mul(recip_table256((tmp2 & 0xff00) >> 8));
    tmp3 |= dodge_saturate(tmp, 0x10000) & 0xff00;
    let tmp = ((d & 0xff_0000) >> 16).wrapping_mul(recip_table256((tmp2 & 0xff_0000) >> 16));
    tmp3 |= (dodge_saturate(tmp, 0x10000) & 0xff00) << 8;
    tmp3
}

/// `color_dodge_blend_func` (`blend_functor_c.h:239`): fade the source, then
/// dodge.
fn color_dodge_faded(d: u32, s: u32, a: u32) -> u32 {
    let s = ((s & 0x00ff00).wrapping_mul(a) >> 8) & 0x00ff00
        | ((s & 0xff00ff).wrapping_mul(a) >> 8) & 0xff00ff;
    color_dodge_raw(d, s)
}

/// `darken_blend_functor` (`blend_functor_c.h:249`): per-byte minimum.
fn darken_raw(d: u32, s: u32) -> u32 {
    let m_src = !s;
    let tmp = (m_src & d).wrapping_add(((m_src ^ d) >> 1) & 0x7f7f_7f7f) & 0x8080_8080;
    let tmp = (tmp << 1).wrapping_sub(tmp >> 7);
    d ^ ((d ^ s) & tmp)
}

/// `darken_blend_func` (`blend_functor_c.h:258`): lerp the destination toward
/// the per-byte minimum by `a`.
fn darken_faded(d: u32, s: u32, a: u32) -> u32 {
    let m_src = !s;
    let mut tmp = (m_src & d).wrapping_add(((m_src ^ d) >> 1) & 0x7f7f_7f7f) & 0x8080_8080;
    tmp = (tmp << 1).wrapping_sub(tmp >> 7);
    tmp = d ^ ((d ^ s) & tmp);
    let d1 = d & 0xff00ff;
    let d1 = d1.wrapping_add((tmp & 0xff00ff).wrapping_sub(d1).wrapping_mul(a) >> 8) & 0xff00ff;
    let dm = d & 0xff00;
    let tm = tmp & 0xff00;
    d1 | ((dm.wrapping_add(tm.wrapping_sub(dm).wrapping_mul(a) >> 8)) & 0xff00)
}

/// `lighten_blend_functor` (`blend_functor_c.h:274`): per-byte maximum.
fn lighten_raw(d: u32, s: u32) -> u32 {
    let m_dest = !d;
    let tmp = (s & m_dest).wrapping_add(((s ^ m_dest) >> 1) & 0x7f7f_7f7f) & 0x8080_8080;
    let tmp = (tmp << 1).wrapping_sub(tmp >> 7);
    d ^ ((d ^ s) & tmp)
}

/// `lighten_blend_func` (`blend_functor_c.h:283`).
fn lighten_faded(d: u32, s: u32, a: u32) -> u32 {
    let m_dest = !d;
    let mut tmp = (s & m_dest).wrapping_add(((s ^ m_dest) >> 1) & 0x7f7f_7f7f) & 0x8080_8080;
    tmp = (tmp << 1).wrapping_sub(tmp >> 7);
    tmp = d ^ ((d ^ s) & tmp);
    let d1 = d & 0xff00ff;
    let d1 = d1.wrapping_add((tmp & 0xff00ff).wrapping_sub(d1).wrapping_mul(a) >> 8) & 0xff00ff;
    let dm = d & 0xff00;
    let tm = tmp & 0xff00;
    d1 | ((dm.wrapping_add(tm.wrapping_sub(dm).wrapping_mul(a) >> 8)) & 0xff00)
}

/// `screen_blend_functor` (`blend_functor_c.h:292`): `255 - (255-d)*(255-s)`.
fn screen_raw(d: u32, s: u32) -> u32 {
    let s = !s;
    let d = !d;
    let mut tmp = ((d & 0xff) * (s & 0xff)) & 0xff00;
    tmp |= (((d & 0xff00) >> 8) * (s & 0xff00)) & 0xff_0000;
    tmp |= (((d & 0xff0000) >> 16) * (s & 0xff0000)) & 0xff00_0000;
    !(tmp >> 8)
}

/// `bmScreen` with `0 < opa < 255` and `!hda`.
///
/// The shipped x86 build runs the SSE2 kernel, not the pure-C body:
/// `TVPGL_SSE2_Init` installs `TVPScreenBlend_o = TVPScreenBlend_o_sse2_c`
/// (`blend_function_sse2.cpp:1364`) and `TVPAfterSystemInit` calls it
/// unconditionally (`base/win32/SysInitImpl.cpp:1305`); the AVX2 init replaces
/// only the `Ps*ScreenBlend` family (`blend_function_avx2.cpp:296-300`). The
/// pure-C `screen_blend_func` (`blend_functor_c.h:301`) is the fallback used
/// when neither is available, and it omits the final complement its own
/// `screen_blend_functor` (`:292`), `screen_blend_HDA_o_func` (`:313`) and the
/// `opa == 255` path all apply — i.e. it is a pre-SSE2 body bug. Kirikiroid2's
/// ARM build is a *third* formulation (`tvpgl_arm.cpp:1223` `do_ScreenBlend_o`:
/// set the source alpha to `opa`, premultiply, screen) and does not match the C
/// body either.
///
/// `sse2_screen_blend_o_functor` (`blend_functor_sse2.h:1246-1287`) drives all
/// four bytes through the same lane: `out = ~(((~d) * ~fade(s, opa)) >> 8)`,
/// with `fade(s, opa) = (s * opa) >> 8` per byte — the alpha byte included, so
/// the destination alpha comes out as `255 - ((255-da)*(255-faded_sa) >> 8)`.
fn screen_faded(d: u32, s: u32, a: u32) -> u32 {
    let mut out = 0u32;
    for shift in (0..32).step_by(8) {
        let d_byte = (d >> shift) & 0xff;
        let s_byte = (s >> shift) & 0xff;
        let faded = (s_byte * a) >> 8;
        out |= (255 - (((255 - d_byte) * (255 - faded)) >> 8)) << shift;
    }
    out
}

/// `bmScreen` with `0 < opa < 255` and `hda`.
///
/// `sse2_screen_blend_hda_o_functor` (`blend_functor_sse2.h:1288`) runs the
/// same lane arithmetic as [`screen_faded`] and then drops its alpha byte and
/// restores the destination's (`and mulmask` / `or dest alpha`), so the colour
/// bytes match the non-HDA variant while the destination alpha is held.
fn screen_hda_faded(d: u32, s: u32, a: u32) -> u32 {
    (d & 0xff00_0000) | (screen_faded(d, s, a) & 0x00ff_ffff)
}

/// `bmAdd`/`bmSub`/`bmMul`/`bmDodge`/`bmDarken`/`bmLighten`/`bmScreen`
/// (`LayerBitmapIntf.cpp:1320-1357`): the plain functor at `opa == 255`, the
/// `_o` wrapper below it, and the `hda` mask either way.
fn blend4_min_variation(d: u32, s: u32, op: BlendOp, opa: u32, hda: bool) -> u32 {
    let raw = |d: u32, s: u32| match op {
        BlendOp::Add => add_blend_raw(d, s),
        BlendOp::Sub => sub_blend_raw(d, s),
        BlendOp::Mul => mul_blend_raw(d, s),
        BlendOp::Dodge => color_dodge_raw(d, s),
        BlendOp::Darken => darken_raw(d, s),
        BlendOp::Lighten => lighten_raw(d, s),
        BlendOp::Screen => screen_raw(d, s),
    };
    let faded = |d: u32, s: u32| match op {
        BlendOp::Add => add_blend_faded(d, s, opa),
        BlendOp::Sub => sub_blend_faded(d, s, opa),
        BlendOp::Mul => mul_blend_faded(d, s, opa),
        BlendOp::Dodge => color_dodge_faded(d, s, opa),
        BlendOp::Darken => darken_faded(d, s, opa),
        BlendOp::Lighten => lighten_faded(d, s, opa),
        BlendOp::Screen => screen_faded(d, s, opa),
    };
    if opa == 255 {
        let out = raw(d, s);
        if hda {
            (d & 0xff00_0000) | (out & 0x00ff_ffff)
        } else {
            out
        }
    } else if hda {
        if op == BlendOp::Screen {
            screen_hda_faded(d, s, opa)
        } else {
            (d & 0xff00_0000) | (faded(d, s) & 0x00ff_ffff)
        }
    } else {
        faded(d, s)
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
            PsOp::Difference5 => (d - ((s * a as i32) >> 8)).abs(),
            PsOp::Exclusion => d + s - (s * d * 2) / 255,
        };
    }
    pack(out[0], out[1], out[2])
}

/// `TVPOpacityOnOpacityTable[opa << 8 | dopa]` (`visual/tvpgl.c:190-213`): the
/// source weight when a source of opacity `opa` meets a destination of
/// opacity `dopa`, built exactly like the official table (float maths).
fn opacity_on_opacity_table(dopa: u32, opa: u32) -> u32 {
    if dopa == 0 {
        return 255;
    }
    let at = (f64::from(dopa) / 255.0) as f32;
    let bt = (f64::from(opa) / 255.0) as f32;
    let mut c = bt / at;
    c /= (1.0 - f64::from(bt) + f64::from(c)) as f32;
    let ci = (c * 255.0) as i64;
    if ci >= 256 { 255 } else { ci.max(0) as u32 }
}

/// `TVPNegativeMulTable[opa << 8 | dopa]` (`visual/tvpgl.c:213`): the resulting
/// opacity of the same pair.
fn negative_mul_table(dopa: u32, opa: u32) -> u32 {
    255 - (255 - dopa) * (255 - opa) / 255
}

/// `TVPConstAlphaBlend_d` (`const_alpha_blend_d_functor`,
/// `blend_functor_c.h:628`): constant blend that keeps the destination alpha
/// model, weighting by the opacity-on-opacity table.
fn const_alpha_blend_d(d: u32, s: u32, opacity: u32) -> u32 {
    let dopa = d >> 24;
    let alpha = opacity_on_opacity_table(dopa, opacity);
    let d1 = d & 0xff00ff;
    let d1 = (d1.wrapping_add((s & 0xff00ff).wrapping_sub(d1).wrapping_mul(alpha) >> 8) & 0xff00ff)
        | (negative_mul_table(dopa, opacity) << 24);
    let dm = d & 0xff00;
    let sm = s & 0xff00;
    d1 | ((dm.wrapping_add(sm.wrapping_sub(dm).wrapping_mul(alpha) >> 8)) & 0xff00)
}

/// `TVPConstAlphaBlend_a` (`const_alpha_blend_a_functor`,
/// `blend_functor_c.h:603`): constant blend onto a premultiplied destination
/// (`bmCopyOnAddAlpha` with opacity).
fn const_alpha_blend_a(d: u32, s: u32, opacity: u32) -> u32 {
    premulalpha_blend_a_a(d, (s & 0x00ff_ffff) | (opacity << 24))
}

/// `const_alpha_fill_blend_a_functor` (`blend_functor_c.h:704`) via
/// `premulalpha_blend_a_ca_func` (`blend_util_func.h:113`): the `colorRect`
/// fill of a `dfAddAlpha` face. `color` is the resolved `TVPToActualColor`
/// value; only its colour bytes take part.
pub(crate) fn const_alpha_fill_blend_a(d: u32, color: u32, opacity: u32) -> u32 {
    let opa = opacity.min(255);
    let color = mul_color(color, opa);
    let opa_inv = opa ^ 0xff;
    let mut dopa = d >> 24;
    dopa = dopa.wrapping_add(opa).wrapping_sub((dopa * opa) >> 8);
    dopa -= dopa >> 8;
    (dopa << 24)
        | swar_sat_add(
            (((d & 0xff00ff).wrapping_mul(opa_inv) >> 8) & 0xff00ff)
                .wrapping_add(((d & 0xff00).wrapping_mul(opa_inv) >> 8) & 0xff00),
            color,
        )
}

/// `tTVPBBStretchType` (`LayerBitmapIntf.h:61`), collapsed to the kernels the
/// resamplers implement.
///
/// Every `stFast*` variant shares its precise counterpart's *weight* function —
/// `stFastCubic`/`stFastLanczos2`/… dispatch to the same `TWeightFunc` through
/// the fixed-point resampler (`ResampleImage.cpp:741-764`) — and so do
/// `stLinear` and `stSemiFastLinear`: the plain C path sends both to
/// `BilinearWeight` (`:711-712`, `:738-739`) and the shipped SSE2 path sends
/// `stLinear` to `TVPWeightResampleSSE2<BilinearWeightSSE>` and
/// `stSemiFastLinear` to `TVPWeightResampleSSE2Fix<BilinearWeightSSE>`
/// (`ResampleImageSSE2.cpp:1151-1158`), which differ only in the fixed-point
/// weight arithmetic this engine does not model. `stNearest`/`stFastNearest`
/// never reach the resampler at all — `StretchBlt` routes every type below
/// `stLinear` to `AffineBlt` (`LayerBitmapIntf.cpp:1857-1875`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StretchType {
    Nearest,
    Bilinear,
    Bicubic,
    Lanczos2,
    Lanczos3,
    Spline16,
    Spline36,
    AreaAvg,
    Gaussian,
    BlackmanSinc,
}

pub(crate) fn stretch_type_from_i64(value: i64) -> StretchType {
    // `stTypeMask = 0x0000ffff` in the C++ header selects the interpolation
    // type; the `stFlagMask` bits (`stRefNoClip`) are not part of it
    // (`visual/LayerBitmapIntf.h:82`).
    match value & 0xffff {
        1 | 2 | 4 => StretchType::Bilinear,
        3 | 5 => StretchType::Bicubic,
        6 | 7 => StretchType::Lanczos2,
        8 | 9 => StretchType::Lanczos3,
        10 | 11 => StretchType::Spline16,
        12 | 13 => StretchType::Spline36,
        14 | 15 => StretchType::AreaAvg,
        16 | 17 => StretchType::Gaussian,
        18 | 19 => StretchType::BlackmanSinc,
        _ => StretchType::Nearest,
    }
}

/// Filter kernels from `visual/gl/WeightFunctor.h`.
fn stretch_weight(kind: StretchType, distance: f64) -> f64 {
    let x = distance.abs();
    match kind {
        StretchType::Nearest | StretchType::AreaAvg => {
            if x < 0.5 { 1.0 } else { 0.0 }
        }
        // `BilinearWeight` (`WeightFunctor.h:19`), RANGE 1.
        StretchType::Bilinear => (1.0 - x).max(0.0),
        // `BicubicWeight` with the default `coeff = -1` (`WeightFunctor.h:34`).
        StretchType::Bicubic => {
            if x <= 1.0 {
                1.0 - 2.0 * x * x + x * x * x
            } else if x <= 2.0 {
                4.0 - 8.0 * x + 5.0 * x * x - x * x * x
            } else {
                0.0
            }
        }
        // `LanczosWeight<TTap>` (`WeightFunctor.h:65`).
        StretchType::Lanczos2 | StretchType::Lanczos3 => {
            let tap = if kind == StretchType::Lanczos2 { 2.0 } else { 3.0 };
            if x < f64::EPSILON {
                1.0
            } else if x >= tap {
                0.0
            } else {
                let pi = std::f64::consts::PI;
                (pi * distance).sin() * (pi * distance / tap).sin()
                    / (pi * pi * distance * distance / tap)
            }
        }
        // `Spline16Weight` / `Spline36Weight` (`WeightFunctor.h:80` / `:97`).
        StretchType::Spline16 => {
            if x <= 1.0 {
                x * x * x - x * x * 9.0 / 5.0 - x / 5.0 + 1.0
            } else if x <= 2.0 {
                -x * x * x / 3.0 + x * x * 9.0 / 5.0 - x * 46.0 / 15.0 + 8.0 / 5.0
            } else {
                0.0
            }
        }
        StretchType::Spline36 => {
            if x <= 1.0 {
                x * x * x * 13.0 / 11.0 - x * x * 453.0 / 209.0 - x * 3.0 / 209.0 + 1.0
            } else if x <= 2.0 {
                -x * x * x * 6.0 / 11.0 + x * x * 612.0 / 209.0 - x * 1038.0 / 209.0
                    + 540.0 / 209.0
            } else if x <= 3.0 {
                x * x * x / 11.0 - x * x * 159.0 / 209.0 + x * 434.0 / 209.0 - 384.0 / 209.0
            } else {
                0.0
            }
        }
        // `GaussianWeight` (`WeightFunctor.h:115`), RANGE 2.
        StretchType::Gaussian => (-2.0 * x * x).exp() * (2.0 / std::f64::consts::PI).sqrt(),
        // `BlackmanSincWeight` (`WeightFunctor.h:131`), RANGE 4.
        StretchType::BlackmanSinc => {
            if x >= 4.0 {
                0.0
            } else if x < f64::EPSILON {
                1.0
            } else {
                let pi = std::f64::consts::PI;
                (0.42 + 0.5 * (pi * x / 4.0).cos() + 0.08 * (2.0 * pi * x / 4.0).cos())
                    * (pi * x).sin()
                    / (pi * x)
            }
        }
    }
}

fn stretch_range(kind: StretchType) -> f64 {
    match kind {
        StretchType::Nearest | StretchType::AreaAvg | StretchType::Bilinear => 1.0,
        StretchType::Bicubic | StretchType::Spline16 | StretchType::Gaussian => 2.0,
        StretchType::Lanczos2 => 2.0,
        StretchType::Lanczos3 | StretchType::Spline36 => 3.0,
        StretchType::BlackmanSinc => 4.0,
    }
}

/// The reference resampler's tap window and normalized weights for one axis
/// (`AxisParamCalculateAxis`, `visual/gl/ResampleImage.cpp:283-368`, followed by
/// the edge fold and normalization of `AxisParamCalculateWeight`, `:232-280`).
///
/// `dest_index` is the destination pixel's index inside the destination
/// rectangle; the reference samples at the source position
/// `cx = (dst + 0.5) * src_len / dst_len + src_start` (`:302`) counted in an
/// edge frame where source pixel `i` covers `[i, i+1)` and its centre sits at
/// `i + 0.5`, and weights tap `left + k` by
/// `func(|left + k + 0.5 - cx|)` (`:317-322`). Magnification
/// (`src_len <= dst_len`) uses the plain kernel; shrinking widens it by the
/// ratio and scales the distances by `delta = dst_len / src_len` (`:328`,
/// `:357`). The reference computes all of this in `float`s, so the ports below
/// keep `f32` for the window arithmetic.
fn resample_axis_weights(
    source_start: i64,
    source_end: i64,
    source_length: i64,
    dest_length: i64,
    dest_index: i64,
    kind: StretchType,
) -> Option<(i64, Vec<f64>)> {
    let tap = stretch_range(kind) as f32;
    let cx = (dest_index as f32 + 0.5) * source_length as f32 / dest_length as f32
        + source_start as f32;
    let (range, delta) = if source_length <= dest_length {
        (tap, 1.0f32)
    } else {
        (
            tap * source_length as f32 / dest_length as f32,
            dest_length as f32 / source_length as f32,
        )
    };
    let left = (cx - range).floor() as i64;
    let right = (cx + range).floor() as i64;
    let count = right - left;
    if count <= 0 {
        return None;
    }
    let mut weights = Vec::with_capacity(count as usize);
    for index in 0..count {
        let distance = ((left + index) as f32 + 0.5 - cx) * delta;
        weights.push(stretch_weight(kind, f64::from(distance)));
    }
    // Taps before `srcstart` fold onto the first in-range tap, taps at or past
    // `srcend` onto the last (`:235-257`).
    let left_edge = (source_start - left).clamp(0, count);
    let start = left + left_edge;
    if start >= source_end {
        return None;
    }
    let mut folded: Vec<f64> = weights[left_edge as usize..].to_vec();
    if left_edge > 0 {
        folded[0] += weights[..left_edge as usize].iter().sum::<f64>();
    }
    let right_edge = (right - source_end).clamp(0, folded.len() as i64);
    if right_edge > 0 {
        let keep = folded.len() as i64 - right_edge;
        if keep <= 0 {
            return None;
        }
        let folded_sum: f64 = folded[keep as usize..].iter().sum();
        folded.truncate(keep as usize);
        *folded.last_mut().expect("non-empty") += folded_sum;
    }
    let sum: f64 = folded.iter().sum();
    if sum.abs() <= f64::EPSILON {
        return None;
    }
    for weight in &mut folded {
        *weight /= sum;
    }
    Some((start, folded))
}

/// `TVPResampleImage`'s sampler: the separable two-pass weight application of
/// `tTVPSampler::samplingVertical` / `samplingHorizontal`
/// (`visual/gl/ResampleImage.cpp:407-466`) with the fixed-point-free channel
/// clamping of `:428-431`.
///
/// `source_rect` is the source rectangle in pixels, which is where the
/// reference's kernel folds; the destination size decides whether the kernel is
/// widened for a shrink, and `dest_offset` is the destination pixel's index
/// inside the destination rectangle (its source sample position follows the
/// reference's `cx` formula for that index).
pub(crate) fn sample_resample_rgba(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    source_rect: (i64, i64, i64, i64),
    dest_size: (i64, i64),
    dest_offset: (i64, i64),
    kind: StretchType,
) -> Option<[u8; 4]> {
    let (left, top, right, bottom) = source_rect;
    if right <= left || bottom <= top {
        return None;
    }
    if kind == StretchType::AreaAvg {
        // `stAreaAvg` runs its own accumulator (`TVPCalculateAxisAreaAvg`,
        // `visual/gl/ResampleImage.cpp:374-379` + `:468-506`); this engine keeps
        // its box average over the sample cell as an approximation, so the
        // weighted path below is not used for it.
        let cx = (dest_offset.0 as f32 + 0.5) * (right - left) as f32 / dest_size.0 as f32
            + left as f32;
        let cy = (dest_offset.1 as f32 + 0.5) * (bottom - top) as f32 / dest_size.1 as f32
            + top as f32;
        return sample_rgba(
            source,
            source_width,
            source_height,
            f64::from(cx) - 0.5,
            f64::from(cy) - 0.5,
            kind,
        );
    }
    let (start_x, weights_x) =
        resample_axis_weights(left, right, right - left, dest_size.0, dest_offset.0, kind)?;
    let (start_y, weights_y) =
        resample_axis_weights(top, bottom, bottom - top, dest_size.1, dest_offset.1, kind)?;

    let mut sums = [0f64; 4];
    for (index_y, weight_y) in weights_y.iter().enumerate() {
        let source_y = start_y + index_y as i64;
        if source_y < 0 || source_y >= i64::from(source_height) {
            continue;
        }
        for (index_x, weight_x) in weights_x.iter().enumerate() {
            let source_x = start_x + index_x as i64;
            if source_x < 0 || source_x >= i64::from(source_width) {
                continue;
            }
            let weight = weight_x * weight_y;
            let index = ((source_y as u32 * source_width + source_x as u32) * 4) as usize;
            if index + 4 > source.len() {
                continue;
            }
            for (channel, sum) in sums.iter_mut().enumerate() {
                *sum += f64::from(source[index + channel]) * weight;
            }
        }
    }
    Some([
        sums[0].clamp(0.0, 255.0) as u8,
        sums[1].clamp(0.0, 255.0) as u8,
        sums[2].clamp(0.0, 255.0) as u8,
        sums[3].clamp(0.0, 255.0) as u8,
    ])
}

/// Resample one source pixel at a fractional source coordinate. `None` means
/// the sample falls outside the texture.
pub(crate) fn sample_rgba(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    x: f64,
    y: f64,
    kind: StretchType,
) -> Option<[u8; 4]> {
    if kind == StretchType::Nearest {
        let sx = x.round() as i64;
        let sy = y.round() as i64;
        if sx < 0 || sy < 0 || sx >= i64::from(source_width) || sy >= i64::from(source_height) {
            return None;
        }
        let index = ((sy as u32 * source_width + sx as u32) * 4) as usize;
        return source.get(index..index + 4).map(|p| [p[0], p[1], p[2], p[3]]);
    }
    if kind == StretchType::AreaAvg {
        let x0 = x.floor().max(0.0) as i64;
        let y0 = y.floor().max(0.0) as i64;
        let x1 = (x.ceil() as i64).min(i64::from(source_width));
        let y1 = (y.ceil() as i64).min(i64::from(source_height));
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let mut sum = [0u64; 4];
        let mut count = 0u64;
        for sy in y0..y1 {
            for sx in x0..x1 {
                let index = ((sy as u32 * source_width + sx as u32) * 4) as usize;
                for (channel, value) in sum.iter_mut().enumerate() {
                    *value += u64::from(source[index + channel]);
                }
                count += 1;
            }
        }
        return Some([
            (sum[0] / count) as u8,
            (sum[1] / count) as u8,
            (sum[2] / count) as u8,
            (sum[3] / count) as u8,
        ]);
    }

    let range = stretch_range(kind);
    let base_x = x.floor() as i64;
    let base_y = y.floor() as i64;
    let mut sum = [0f64; 4];
    let mut weight_sum = 0f64;
    for ty in 0..=(range as i64 * 2) {
        let sy = base_y + ty - range as i64 + 1;
        if sy < 0 || sy >= i64::from(source_height) {
            continue;
        }
        let wy = stretch_weight(kind, y - sy as f64);
        if wy == 0.0 {
            continue;
        }
        for tx in 0..=(range as i64 * 2) {
            let sx = base_x + tx - range as i64 + 1;
            if sx < 0 || sx >= i64::from(source_width) {
                continue;
            }
            let weight = wy * stretch_weight(kind, x - sx as f64);
            if weight == 0.0 {
                continue;
            }
            let index = ((sy as u32 * source_width + sx as u32) * 4) as usize;
            for channel in 0..4 {
                sum[channel] += f64::from(source[index + channel]) * weight;
            }
            weight_sum += weight;
        }
    }
    if weight_sum.abs() <= f64::EPSILON {
        return None;
    }
    Some([
        (sum[0] / weight_sum).round().clamp(0.0, 255.0) as u8,
        (sum[1] / weight_sum).round().clamp(0.0, 255.0) as u8,
        (sum[2] / weight_sum).round().clamp(0.0, 255.0) as u8,
        (sum[3] / weight_sum).round().clamp(0.0, 255.0) as u8,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground truth for `blt_pixel`, produced by compiling the official
    /// functor bodies (`krkrz/src/core/visual/gl/blend_functor_c.h` plus the
    /// tables built in `visual/tvpgl.c`) and running every method of the
    /// `tTVPBaseBitmap::Blt` switch over this grid of pixels, opacities and
    /// `hda` flags. Each row is `(dest, src, opa, hda, expected[VARIANTS])`.
    ///
    /// Regenerate with `crates/krkr-engine/tools/blend-truth/gen_truth.cc`,
    /// which prints this array and documents the alignment target (the shipped
    /// SSE2 kernels, including the `bmScreen` `_o`/`_HDA_o` bodies the pure-C
    /// fallback disagrees with).
    ///
    /// The Photoshop (`bmPs*`) methods are not part of the table: they are not
    /// touched by this port and their ports intentionally approximate the
    /// official table-driven kernels.
const TRUTH: &[(u32, u32, u32, bool, [u32; 18])] = &[
    (0xff0000ff, 0xffff0000, 255, false, [0xffff0000, 0xffff0000, 0xff0000ff, 0xffff0000, 0xffff0000, 0xfffe0000, 0x00fe0000, 0xfffe0000, 0xff0000ff, 0xffff0000, 0xffff0000, 0xffff00ff, 0xff000000, 0x00000000, 0x000000ff, 0xff000000, 0xffff00ff, 0xffff01ff]),
    (0xff0000ff, 0xffff0000, 255, true, [0xffff0000, 0xffff0000, 0xff0000ff, 0xffff0000, 0xffff0000, 0xfffe0000, 0xfffe0000, 0xfffe0000, 0xff0000ff, 0xffff0000, 0xffff0000, 0xffff00ff, 0xff000000, 0xff000000, 0xff0000ff, 0xff000000, 0xffff00ff, 0xffff01ff]),
    (0xff0000ff, 0xffff0000, 200, false, [0x00c70037, 0xffff0000, 0xff0000ff, 0xffc70037, 0xffff0036, 0xffc60038, 0x00c60038, 0xffc60037, 0xff0000ff, 0xc7c70037, 0xffc70037, 0xffc700ff, 0xff000038, 0x00000037, 0x000000ff, 0x00000037, 0x00c700ff, 0xffc801ff]),
    (0xff0000ff, 0xffff0000, 200, true, [0xffc70037, 0xffff0000, 0xff0000ff, 0xffc70037, 0xffff0036, 0xffc60038, 0xffc60038, 0xffc60037, 0xff0000ff, 0xc7c70037, 0xffc70037, 0xffc700ff, 0xff000038, 0xff000037, 0xff0000ff, 0xff000037, 0xffc700ff, 0xffc801ff]),
    (0xff0000ff, 0xffff0000, 128, false, [0x007f007f, 0xffff0000, 0xff0000ff, 0xff7f007f, 0xffff007e, 0xff7e0080, 0x007e0080, 0xff7e007f, 0xff0000ff, 0x7f7f007f, 0xff7f007f, 0xff7f00ff, 0xff000080, 0x0000007f, 0x000000ff, 0x0000007f, 0x007f00ff, 0xff8001ff]),
    (0xff0000ff, 0xffff0000, 128, true, [0xff7f007f, 0xffff0000, 0xff0000ff, 0xff7f007f, 0xffff007e, 0xff7e0080, 0xff7e0080, 0xff7e007f, 0xff0000ff, 0x7f7f007f, 0xff7f007f, 0xff7f00ff, 0xff000080, 0xff00007f, 0xff0000ff, 0xff00007f, 0xff7f00ff, 0xff8001ff]),
    (0xff0000ff, 0xffff0000, 1, false, [0x000000fe, 0xffff0000, 0xff0000ff, 0xff0000fe, 0xffff00fd, 0xff0000ff, 0x000000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0x000000fe, 0x000000ff, 0x000000fe, 0x000000ff, 0xff0101ff]),
    (0xff0000ff, 0xffff0000, 1, true, [0xff0000fe, 0xffff0000, 0xff0000ff, 0xff0000fe, 0xffff00fd, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0101ff]),
    (0xff0000ff, 0x8000ff00, 255, false, [0x8000ff00, 0xff00ff00, 0x800000ff, 0xff00ff00, 0xff00ff00, 0xff007f7f, 0x00007f7f, 0xff007f7e, 0xff0000ff, 0x8000ff7e, 0xff00ff7e, 0xff00ffff, 0x80000000, 0x00000000, 0x000000ff, 0x80000000, 0xff00ffff, 0xff01ffff]),
    (0xff0000ff, 0x8000ff00, 255, true, [0xff00ff00, 0xff00ff00, 0x800000ff, 0xff00ff00, 0xff00ff00, 0xff007f7f, 0xff007f7f, 0xff007f7e, 0xff0000ff, 0xff00ff7e, 0xff00ff7e, 0xff00ffff, 0xff000000, 0xff000000, 0xff0000ff, 0xff000000, 0xff00ffff, 0xff01ffff]),
    (0xff0000ff, 0x8000ff00, 200, false, [0x0000c737, 0xff00ff00, 0x800000ff, 0xff00c737, 0xff00ff36, 0xff00639b, 0x0000639b, 0xff00639a, 0xff0000ff, 0x6400c79a, 0xff00c79a, 0xff00c7ff, 0xff000038, 0x00000037, 0x000000ff, 0x00000037, 0x0000c7ff, 0xff01c8ff]),
    (0xff0000ff, 0x8000ff00, 200, true, [0xff00c737, 0xff00ff00, 0x800000ff, 0xff00c737, 0xff00ff36, 0xff00639b, 0xff00639b, 0xff00639a, 0xff0000ff, 0x6400c79a, 0xff00c79a, 0xff00c7ff, 0xff000038, 0xff000037, 0xff0000ff, 0xff000037, 0xff00c7ff, 0xff01c8ff]),
    (0xff0000ff, 0x8000ff00, 128, false, [0x00007f7f, 0xff00ff00, 0x800000ff, 0xff007f7f, 0xff00ff7e, 0xff003fbf, 0x00003fbf, 0xff003fbe, 0xff0000ff, 0x40007fbe, 0xff007fbe, 0xff007fff, 0xff000080, 0x0000007f, 0x000000ff, 0x0000007f, 0x00007fff, 0xff0180ff]),
    (0xff0000ff, 0x8000ff00, 128, true, [0xff007f7f, 0xff00ff00, 0x800000ff, 0xff007f7f, 0xff00ff7e, 0xff003fbf, 0xff003fbf, 0xff003fbe, 0xff0000ff, 0x40007fbe, 0xff007fbe, 0xff007fff, 0xff000080, 0xff00007f, 0xff0000ff, 0xff00007f, 0xff007fff, 0xff0180ff]),
    (0xff0000ff, 0x8000ff00, 1, false, [0x000000fe, 0xff00ff00, 0x800000ff, 0xff0000fe, 0xff00fffd, 0xff0000ff, 0x000000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0x000000fe, 0x000000ff, 0x000000fe, 0x000000ff, 0xff0101ff]),
    (0xff0000ff, 0x8000ff00, 1, true, [0xff0000fe, 0xff00ff00, 0x800000ff, 0xff0000fe, 0xff00fffd, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0101ff]),
    (0xff0000ff, 0xffffffff, 255, false, [0xffffffff, 0xffffffff, 0xff0000ff, 0xffffffff, 0xffffffff, 0xfffefeff, 0x00fefeff, 0xfffefefe, 0xff0000ff, 0xffffffff, 0xffffffff, 0xffffffff, 0xff0000ff, 0x000000fe, 0x000000ff, 0xff0000ff, 0xffffffff, 0xffffffff]),
    (0xff0000ff, 0xffffffff, 255, true, [0xffffffff, 0xffffffff, 0xff0000ff, 0xffffffff, 0xffffffff, 0xfffefeff, 0xfffefeff, 0xfffefefe, 0xff0000ff, 0xffffffff, 0xffffffff, 0xffffffff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xffffffff, 0xffffffff]),
    (0xff0000ff, 0xffffffff, 200, false, [0x00c7c7ff, 0xffffffff, 0xff0000ff, 0xffc7c7ff, 0xffffffff, 0xffc6c6ff, 0x00c6c6ff, 0xffc6c6fd, 0xff0000ff, 0xc7c7c7fe, 0xffc7c7fe, 0xffc7c7ff, 0xff0000ff, 0x000000fe, 0x000000ff, 0x000000ff, 0x00c7c7ff, 0xffc8c8ff]),
    (0xff0000ff, 0xffffffff, 200, true, [0xffc7c7ff, 0xffffffff, 0xff0000ff, 0xffc7c7ff, 0xffffffff, 0xffc6c6ff, 0xffc6c6ff, 0xffc6c6fd, 0xff0000ff, 0xc7c7c7fe, 0xffc7c7fe, 0xffc7c7ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xffc7c7ff, 0xffc8c8ff]),
    (0xff0000ff, 0xffffffff, 128, false, [0x007f7fff, 0xffffffff, 0xff0000ff, 0xff7f7fff, 0xffffffff, 0xff7e7eff, 0x007e7eff, 0xff7e7efd, 0xff0000ff, 0x7f7f7ffe, 0xff7f7ffe, 0xff7f7fff, 0xff0000ff, 0x000000fe, 0x000000ff, 0x000000ff, 0x007f7fff, 0xff8080ff]),
    (0xff0000ff, 0xffffffff, 128, true, [0xff7f7fff, 0xffffffff, 0xff0000ff, 0xff7f7fff, 0xffffffff, 0xff7e7eff, 0xff7e7eff, 0xff7e7efd, 0xff0000ff, 0x7f7f7ffe, 0xff7f7ffe, 0xff7f7fff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xff7f7fff, 0xff8080ff]),
    (0xff0000ff, 0xffffffff, 1, false, [0x000000ff, 0xffffffff, 0xff0000ff, 0xff0000ff, 0xffffffff, 0xff0000ff, 0x000000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0x000000fe, 0x000000ff, 0x000000ff, 0x000000ff, 0xff0101ff]),
    (0xff0000ff, 0xffffffff, 1, true, [0xff0000ff, 0xffffffff, 0xff0000ff, 0xff0000ff, 0xffffffff, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xff0000ff, 0xff0101ff]),
    (0xff0000ff, 0x40c0a080, 255, false, [0x40c0a080, 0xffc0a080, 0x400000ff, 0xffc0a080, 0xffc0a080, 0xff3028df, 0x003028df, 0xff3028de, 0xff0000ff, 0x40c0a0ff, 0xffc0a0ff, 0xffc0a0ff, 0x40000080, 0x0000007f, 0x000000ff, 0x40000080, 0xffc0a0ff, 0xffc1a1ff]),
    (0xff0000ff, 0x40c0a080, 255, true, [0xffc0a080, 0xffc0a080, 0x400000ff, 0xffc0a080, 0xffc0a080, 0xff3028df, 0xff3028df, 0xff3028de, 0xff0000ff, 0xffc0a0ff, 0xffc0a0ff, 0xffc0a0ff, 0xff000080, 0xff00007f, 0xff0000ff, 0xff000080, 0xffc0a0ff, 0xffc1a1ff]),
    (0xff0000ff, 0x40c0a080, 200, false, [0x00967d9b, 0xffc0a080, 0x400000ff, 0xff967d9b, 0xffc0a0b6, 0xff251fe6, 0x00251fe6, 0xff251fe5, 0xff0000ff, 0x32967dff, 0xff967dff, 0xff967dff, 0xff00009c, 0x0000009b, 0x000000ff, 0x0000009b, 0x00967dff, 0xff977eff]),
    (0xff0000ff, 0x40c0a080, 200, true, [0xff967d9b, 0xffc0a080, 0x400000ff, 0xff967d9b, 0xffc0a0b6, 0xff251fe6, 0xff251fe6, 0xff251fe5, 0xff0000ff, 0x32967dff, 0xff967dff, 0xff967dff, 0xff00009c, 0xff00009b, 0xff0000ff, 0xff00009b, 0xff967dff, 0xff977eff]),
    (0xff0000ff, 0x40c0a080, 128, false, [0x006050bf, 0xffc0a080, 0x400000ff, 0xff6050bf, 0xffc0a0fe, 0xff1814ef, 0x001814ef, 0xff1814ee, 0xff0000ff, 0x206050ff, 0xff6050ff, 0xff6050ff, 0xff0000c0, 0x000000bf, 0x000000ff, 0x000000bf, 0x006050ff, 0xff6151ff]),
    (0xff0000ff, 0x40c0a080, 128, true, [0xff6050bf, 0xffc0a080, 0x400000ff, 0xff6050bf, 0xffc0a0fe, 0xff1814ef, 0xff1814ef, 0xff1814ee, 0xff0000ff, 0x206050ff, 0xff6050ff, 0xff6050ff, 0xff0000c0, 0xff0000bf, 0xff0000ff, 0xff0000bf, 0xff6050ff, 0xff6151ff]),
    (0xff0000ff, 0x40c0a080, 1, false, [0x000000fe, 0xffc0a080, 0x400000ff, 0xff0000fe, 0xffc0a0ff, 0xff0000ff, 0x000000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0x000000fe, 0x000000ff, 0x000000fe, 0x000000ff, 0xff0101ff]),
    (0xff0000ff, 0x40c0a080, 1, true, [0xff0000fe, 0xffc0a080, 0x400000ff, 0xff0000fe, 0xffc0a0ff, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0101ff]),
    (0xff0000ff, 0x00abcdef, 255, false, [0x00abcdef, 0xffabcdef, 0x000000ff, 0xffabcdef, 0xffabcdef, 0xff0000ff, 0x000000ff, 0xff0000fe, 0xff0000ff, 0x00abcdff, 0xffabcdff, 0xffabcdff, 0x000000ef, 0x000000ee, 0x000000ff, 0x000000ef, 0xffabcdff, 0xffacceff]),
    (0xff0000ff, 0x00abcdef, 255, true, [0xffabcdef, 0xffabcdef, 0x000000ff, 0xffabcdef, 0xffabcdef, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xffabcdff, 0xffabcdff, 0xffabcdff, 0xff0000ef, 0xff0000ee, 0xff0000ff, 0xff0000ef, 0xffabcdff, 0xffacceff]),
    (0xff0000ff, 0x00abcdef, 200, false, [0x0085a0f2, 0xffabcdef, 0x000000ff, 0xff85a0f2, 0xffabcdff, 0xff0000ff, 0x000000ff, 0xff0000fe, 0xff0000ff, 0x0085a0ff, 0xff85a0ff, 0xff85a0ff, 0xff0000f3, 0x000000f2, 0x000000ff, 0x000000f2, 0x0085a0ff, 0xff86a1ff]),
    (0xff0000ff, 0x00abcdef, 200, true, [0xff85a0f2, 0xffabcdef, 0x000000ff, 0xff85a0f2, 0xffabcdff, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0x0085a0ff, 0xff85a0ff, 0xff85a0ff, 0xff0000f3, 0xff0000f2, 0xff0000ff, 0xff0000f2, 0xff85a0ff, 0xff86a1ff]),
    (0xff0000ff, 0x00abcdef, 128, false, [0x005566f7, 0xffabcdef, 0x000000ff, 0xff5566f7, 0xffabcdff, 0xff0000ff, 0x000000ff, 0xff0000fe, 0xff0000ff, 0x005566ff, 0xff5566ff, 0xff5566ff, 0xff0000f7, 0x000000f6, 0x000000ff, 0x000000f7, 0x005566ff, 0xff5667ff]),
    (0xff0000ff, 0x00abcdef, 128, true, [0xff5566f7, 0xffabcdef, 0x000000ff, 0xff5566f7, 0xffabcdff, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0x005566ff, 0xff5566ff, 0xff5566ff, 0xff0000f7, 0xff0000f6, 0xff0000ff, 0xff0000f7, 0xff5566ff, 0xff5667ff]),
    (0xff0000ff, 0x00abcdef, 1, false, [0x000000fe, 0xffabcdef, 0x000000ff, 0xff0000fe, 0xffabcdff, 0xff0000ff, 0x000000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0x000000fe, 0x000000ff, 0x000000fe, 0x000000ff, 0xff0101ff]),
    (0xff0000ff, 0x00abcdef, 1, true, [0xff0000fe, 0xffabcdef, 0x000000ff, 0xff0000fe, 0xffabcdff, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0101ff]),
    (0xff0000ff, 0x7f808080, 255, false, [0x7f808080, 0xff808080, 0x7f0000ff, 0xff808080, 0xff808080, 0xff3f3fbf, 0x003f3fbf, 0xff3f3fbe, 0xff0000ff, 0x7f8080ff, 0xff8080ff, 0xff8080ff, 0x7f000080, 0x0000007f, 0x000000ff, 0x7f000080, 0xff8080ff, 0xff8181ff]),
    (0xff0000ff, 0x7f808080, 255, true, [0xff808080, 0xff808080, 0x7f0000ff, 0xff808080, 0xff808080, 0xff3f3fbf, 0xff3f3fbf, 0xff3f3fbe, 0xff0000ff, 0xff8080ff, 0xff8080ff, 0xff8080ff, 0xff000080, 0xff00007f, 0xff0000ff, 0xff000080, 0xff8080ff, 0xff8181ff]),
    (0xff0000ff, 0x7f808080, 200, false, [0x0064649b, 0xff808080, 0x7f0000ff, 0xff64649b, 0xff8080b6, 0xff3131cd, 0x003131cd, 0xff3131cc, 0xff0000ff, 0x636464ff, 0xff6464ff, 0xff6464ff, 0xff00009c, 0x0000009b, 0x000000ff, 0x0000009b, 0x006464ff, 0xff6565ff]),
    (0xff0000ff, 0x7f808080, 200, true, [0xff64649b, 0xff808080, 0x7f0000ff, 0xff64649b, 0xff8080b6, 0xff3131cd, 0xff3131cd, 0xff3131cc, 0xff0000ff, 0x636464ff, 0xff6464ff, 0xff6464ff, 0xff00009c, 0xff00009b, 0xff0000ff, 0xff00009b, 0xff6464ff, 0xff6565ff]),
    (0xff0000ff, 0x7f808080, 128, false, [0x004040bf, 0xff808080, 0x7f0000ff, 0xff4040bf, 0xff8080fe, 0xff1f1fdf, 0x001f1fdf, 0xff1f1fde, 0xff0000ff, 0x3f4040ff, 0xff4040ff, 0xff4040ff, 0xff0000c0, 0x000000bf, 0x000000ff, 0x000000bf, 0x004040ff, 0xff4141ff]),
    (0xff0000ff, 0x7f808080, 128, true, [0xff4040bf, 0xff808080, 0x7f0000ff, 0xff4040bf, 0xff8080fe, 0xff1f1fdf, 0xff1f1fdf, 0xff1f1fde, 0xff0000ff, 0x3f4040ff, 0xff4040ff, 0xff4040ff, 0xff0000c0, 0xff0000bf, 0xff0000ff, 0xff0000bf, 0xff4040ff, 0xff4141ff]),
    (0xff0000ff, 0x7f808080, 1, false, [0x000000fe, 0xff808080, 0x7f0000ff, 0xff0000fe, 0xff8080ff, 0xff0000ff, 0x000000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0x000000fe, 0x000000ff, 0x000000fe, 0x000000ff, 0xff0101ff]),
    (0xff0000ff, 0x7f808080, 1, true, [0xff0000fe, 0xff808080, 0x7f0000ff, 0xff0000fe, 0xff8080ff, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0x000000fe, 0xff0000fe, 0xff0000ff, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0000fe, 0xff0000ff, 0xff0101ff]),
    (0x00000000, 0xffff0000, 255, false, [0xffff0000, 0x00ff0000, 0xff000000, 0xffff0000, 0xffff0000, 0xfffe0000, 0x00fe0000, 0xfffe0000, 0x00000000, 0xffff0000, 0xffff0000, 0xffff0000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0xffff0000, 0xffff0101]),
    (0x00000000, 0xffff0000, 255, true, [0x00ff0000, 0x00ff0000, 0xff000000, 0xffff0000, 0xffff0000, 0xfffe0000, 0x00fe0000, 0xfffe0000, 0x00000000, 0x00ff0000, 0xffff0000, 0x00ff0000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00ff0000, 0x00ff0101]),
    (0x00000000, 0xffff0000, 200, false, [0x00c70000, 0x00ff0000, 0xff000000, 0xc8fe0000, 0xc8ff0000, 0xc7fe0000, 0x00c60000, 0xc7c60000, 0x00000000, 0xc7c70000, 0xc7c70000, 0x00c70000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00c70000, 0xc8c80101]),
    (0x00000000, 0xffff0000, 200, true, [0x00c70000, 0x00ff0000, 0xff000000, 0xc8fe0000, 0xc8ff0000, 0xc7fe0000, 0x00c60000, 0xc7c60000, 0x00000000, 0xc7c70000, 0xc7c70000, 0x00c70000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00c70000, 0x00c80101]),
    (0x00000000, 0xffff0000, 128, false, [0x007f0000, 0x00ff0000, 0xff000000, 0x80fe0000, 0x80ff0000, 0x7ffe0000, 0x007e0000, 0x7f7e0000, 0x00000000, 0x7f7f0000, 0x7f7f0000, 0x007f0000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x007f0000, 0x80800101]),
    (0x00000000, 0xffff0000, 128, true, [0x007f0000, 0x00ff0000, 0xff000000, 0x80fe0000, 0x80ff0000, 0x7ffe0000, 0x007e0000, 0x7f7e0000, 0x00000000, 0x7f7f0000, 0x7f7f0000, 0x007f0000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x007f0000, 0x00800101]),
    (0x00000000, 0xffff0000, 1, false, [0x00000000, 0x00ff0000, 0xff000000, 0x01fe0000, 0x01ff0000, 0x00fe0000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x01010101]),
    (0x00000000, 0xffff0000, 1, true, [0x00000000, 0x00ff0000, 0xff000000, 0x01fe0000, 0x01ff0000, 0x00fe0000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00010101]),
    (0x00000000, 0x8000ff00, 255, false, [0x8000ff00, 0x0000ff00, 0x80000000, 0xff00ff00, 0xff00ff00, 0x8000fe00, 0x00007f00, 0x80007f00, 0x00000000, 0x8000ff00, 0x8000ff00, 0x8000ff00, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x8000ff00, 0xff01ff01]),
    (0x00000000, 0x8000ff00, 255, true, [0x0000ff00, 0x0000ff00, 0x80000000, 0xff00ff00, 0xff00ff00, 0x8000fe00, 0x00007f00, 0x80007f00, 0x00000000, 0x0000ff00, 0x8000ff00, 0x0000ff00, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x0000ff00, 0x0001ff01]),
    (0x00000000, 0x8000ff00, 200, false, [0x0000c700, 0x0000ff00, 0x80000000, 0xc800fe00, 0xc800ff00, 0x6400fe00, 0x00006300, 0x64006300, 0x00000000, 0x6400c700, 0x6400c700, 0x0000c700, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x0000c700, 0x6501c801]),
    (0x00000000, 0x8000ff00, 200, true, [0x0000c700, 0x0000ff00, 0x80000000, 0xc800fe00, 0xc800ff00, 0x6400fe00, 0x00006300, 0x64006300, 0x00000000, 0x6400c700, 0x6400c700, 0x0000c700, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x0000c700, 0x0001c801]),
    (0x00000000, 0x8000ff00, 128, false, [0x00007f00, 0x0000ff00, 0x80000000, 0x8000fe00, 0x8000ff00, 0x4000fe00, 0x00003f00, 0x40003f00, 0x00000000, 0x40007f00, 0x40007f00, 0x00007f00, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00007f00, 0x41018001]),
    (0x00000000, 0x8000ff00, 128, true, [0x00007f00, 0x0000ff00, 0x80000000, 0x8000fe00, 0x8000ff00, 0x4000fe00, 0x00003f00, 0x40003f00, 0x00000000, 0x40007f00, 0x40007f00, 0x00007f00, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00007f00, 0x00018001]),
    (0x00000000, 0x8000ff00, 1, false, [0x00000000, 0x0000ff00, 0x80000000, 0x0100fe00, 0x0100ff00, 0x0000fe00, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x01010101]),
    (0x00000000, 0x8000ff00, 1, true, [0x00000000, 0x0000ff00, 0x80000000, 0x0100fe00, 0x0100ff00, 0x0000fe00, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00010101]),
    (0x00000000, 0xffffffff, 255, false, [0xffffffff, 0x00ffffff, 0xff000000, 0xffffffff, 0xffffffff, 0xfffefefe, 0x00fefefe, 0xfffefefe, 0x00000000, 0xffffffff, 0xffffffff, 0xffffffff, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0xffffffff, 0xffffffff]),
    (0x00000000, 0xffffffff, 255, true, [0x00ffffff, 0x00ffffff, 0xff000000, 0xffffffff, 0xffffffff, 0xfffefefe, 0x00fefefe, 0xfffefefe, 0x00000000, 0x00ffffff, 0xffffffff, 0x00ffffff, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00ffffff, 0x00ffffff]),
    (0x00000000, 0xffffffff, 200, false, [0x00c7c7c7, 0x00ffffff, 0xff000000, 0xc8fefefe, 0xc8ffffff, 0xc7fefefe, 0x00c6c6c6, 0xc7c6c6c6, 0x00000000, 0xc7c7c7c7, 0xc7c7c7c7, 0x00c7c7c7, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00c7c7c7, 0xc8c8c8c8]),
    (0x00000000, 0xffffffff, 200, true, [0x00c7c7c7, 0x00ffffff, 0xff000000, 0xc8fefefe, 0xc8ffffff, 0xc7fefefe, 0x00c6c6c6, 0xc7c6c6c6, 0x00000000, 0xc7c7c7c7, 0xc7c7c7c7, 0x00c7c7c7, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00c7c7c7, 0x00c8c8c8]),
    (0x00000000, 0xffffffff, 128, false, [0x007f7f7f, 0x00ffffff, 0xff000000, 0x80fefefe, 0x80ffffff, 0x7ffefefe, 0x007e7e7e, 0x7f7e7e7e, 0x00000000, 0x7f7f7f7f, 0x7f7f7f7f, 0x007f7f7f, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x007f7f7f, 0x80808080]),
    (0x00000000, 0xffffffff, 128, true, [0x007f7f7f, 0x00ffffff, 0xff000000, 0x80fefefe, 0x80ffffff, 0x7ffefefe, 0x007e7e7e, 0x7f7e7e7e, 0x00000000, 0x7f7f7f7f, 0x7f7f7f7f, 0x007f7f7f, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x007f7f7f, 0x00808080]),
    (0x00000000, 0xffffffff, 1, false, [0x00000000, 0x00ffffff, 0xff000000, 0x01fefefe, 0x01ffffff, 0x00fefefe, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x01010101]),
    (0x00000000, 0xffffffff, 1, true, [0x00000000, 0x00ffffff, 0xff000000, 0x01fefefe, 0x01ffffff, 0x00fefefe, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00010101]),
    (0x00000000, 0x40c0a080, 255, false, [0x40c0a080, 0x00c0a080, 0x40000000, 0xffc0a080, 0xffc0a080, 0x40bf9f7f, 0x00302820, 0x40302820, 0x00000000, 0x40c0a080, 0x40c0a080, 0x40c0a080, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x40c0a080, 0xffc1a181]),
    (0x00000000, 0x40c0a080, 255, true, [0x00c0a080, 0x00c0a080, 0x40000000, 0xffc0a080, 0xffc0a080, 0x40bf9f7f, 0x00302820, 0x40302820, 0x00000000, 0x00c0a080, 0x40c0a080, 0x00c0a080, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00c0a080, 0x00c1a181]),
    (0x00000000, 0x40c0a080, 200, false, [0x00967d64, 0x00c0a080, 0x40000000, 0xc8bf9f7f, 0xc8c0a080, 0x32bf9f7f, 0x00251f19, 0x32251f19, 0x00000000, 0x32967d64, 0x32967d64, 0x00967d64, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00967d64, 0x33977e65]),
    (0x00000000, 0x40c0a080, 200, true, [0x00967d64, 0x00c0a080, 0x40000000, 0xc8bf9f7f, 0xc8c0a080, 0x32bf9f7f, 0x00251f19, 0x32251f19, 0x00000000, 0x32967d64, 0x32967d64, 0x00967d64, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00967d64, 0x00977e65]),
    (0x00000000, 0x40c0a080, 128, false, [0x00605040, 0x00c0a080, 0x40000000, 0x80bf9f7f, 0x80c0a080, 0x20bf9f7f, 0x00181410, 0x20181410, 0x00000000, 0x20605040, 0x20605040, 0x00605040, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00605040, 0x21615141]),
    (0x00000000, 0x40c0a080, 128, true, [0x00605040, 0x00c0a080, 0x40000000, 0x80bf9f7f, 0x80c0a080, 0x20bf9f7f, 0x00181410, 0x20181410, 0x00000000, 0x20605040, 0x20605040, 0x00605040, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00605040, 0x00615141]),
    (0x00000000, 0x40c0a080, 1, false, [0x00000000, 0x00c0a080, 0x40000000, 0x01bf9f7f, 0x01c0a080, 0x00bf9f7f, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x01010101]),
    (0x00000000, 0x40c0a080, 1, true, [0x00000000, 0x00c0a080, 0x40000000, 0x01bf9f7f, 0x01c0a080, 0x00bf9f7f, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00010101]),
    (0x00000000, 0x00abcdef, 255, false, [0x00abcdef, 0x00abcdef, 0x00000000, 0xffabcdef, 0xffabcdef, 0x00aaccee, 0x00000000, 0x00000000, 0x00000000, 0x00abcdef, 0x00abcdef, 0x00abcdef, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00abcdef, 0xffaccef0]),
    (0x00000000, 0x00abcdef, 255, true, [0x00abcdef, 0x00abcdef, 0x00000000, 0xffabcdef, 0xffabcdef, 0x00aaccee, 0x00000000, 0x00000000, 0x00000000, 0x00abcdef, 0x00abcdef, 0x00abcdef, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00abcdef, 0x00accef0]),
    (0x00000000, 0x00abcdef, 200, false, [0x0085a0ba, 0x00abcdef, 0x00000000, 0xc8aaccee, 0xc8abcdef, 0x00aaccee, 0x00000000, 0x00000000, 0x00000000, 0x0085a0ba, 0x0085a0ba, 0x0085a0ba, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x0085a0ba, 0x0186a1bb]),
    (0x00000000, 0x00abcdef, 200, true, [0x0085a0ba, 0x00abcdef, 0x00000000, 0xc8aaccee, 0xc8abcdef, 0x00aaccee, 0x00000000, 0x00000000, 0x00000000, 0x0085a0ba, 0x0085a0ba, 0x0085a0ba, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x0085a0ba, 0x0086a1bb]),
    (0x00000000, 0x00abcdef, 128, false, [0x00556677, 0x00abcdef, 0x00000000, 0x80aaccee, 0x80abcdef, 0x00aaccee, 0x00000000, 0x00000000, 0x00000000, 0x00556677, 0x00556677, 0x00556677, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00556677, 0x01566778]),
    (0x00000000, 0x00abcdef, 128, true, [0x00556677, 0x00abcdef, 0x00000000, 0x80aaccee, 0x80abcdef, 0x00aaccee, 0x00000000, 0x00000000, 0x00000000, 0x00556677, 0x00556677, 0x00556677, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00556677, 0x00566778]),
    (0x00000000, 0x00abcdef, 1, false, [0x00000000, 0x00abcdef, 0x00000000, 0x01aaccee, 0x01abcdef, 0x00aaccee, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x01010101]),
    (0x00000000, 0x00abcdef, 1, true, [0x00000000, 0x00abcdef, 0x00000000, 0x01aaccee, 0x01abcdef, 0x00aaccee, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00010101]),
    (0x00000000, 0x7f808080, 255, false, [0x7f808080, 0x00808080, 0x7f000000, 0xff808080, 0xff808080, 0x7f7f7f7f, 0x003f3f3f, 0x7f3f3f3f, 0x00000000, 0x7f808080, 0x7f808080, 0x7f808080, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x7f808080, 0xff818181]),
    (0x00000000, 0x7f808080, 255, true, [0x00808080, 0x00808080, 0x7f000000, 0xff808080, 0xff808080, 0x7f7f7f7f, 0x003f3f3f, 0x7f3f3f3f, 0x00000000, 0x00808080, 0x7f808080, 0x00808080, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00808080, 0x00818181]),
    (0x00000000, 0x7f808080, 200, false, [0x00646464, 0x00808080, 0x7f000000, 0xc87f7f7f, 0xc8808080, 0x637f7f7f, 0x00313131, 0x63313131, 0x00000000, 0x63646464, 0x63646464, 0x00646464, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00646464, 0x64656565]),
    (0x00000000, 0x7f808080, 200, true, [0x00646464, 0x00808080, 0x7f000000, 0xc87f7f7f, 0xc8808080, 0x637f7f7f, 0x00313131, 0x63313131, 0x00000000, 0x63646464, 0x63646464, 0x00646464, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00646464, 0x00656565]),
    (0x00000000, 0x7f808080, 128, false, [0x00404040, 0x00808080, 0x7f000000, 0x807f7f7f, 0x80808080, 0x3f7f7f7f, 0x001f1f1f, 0x3f1f1f1f, 0x00000000, 0x3f404040, 0x3f404040, 0x00404040, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00404040, 0x40414141]),
    (0x00000000, 0x7f808080, 128, true, [0x00404040, 0x00808080, 0x7f000000, 0x807f7f7f, 0x80808080, 0x3f7f7f7f, 0x001f1f1f, 0x3f1f1f1f, 0x00000000, 0x3f404040, 0x3f404040, 0x00404040, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00404040, 0x00414141]),
    (0x00000000, 0x7f808080, 1, false, [0x00000000, 0x00808080, 0x7f000000, 0x017f7f7f, 0x01808080, 0x007f7f7f, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x01010101]),
    (0x00000000, 0x7f808080, 1, true, [0x00000000, 0x00808080, 0x7f000000, 0x017f7f7f, 0x01808080, 0x007f7f7f, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00010101]),
    (0x00ffffff, 0xffff0000, 255, false, [0xffff0000, 0x00ff0000, 0xffffffff, 0xffff0000, 0xffff0000, 0xffff0000, 0x00ff0000, 0xfffe0000, 0x00ffffff, 0xffff0000, 0xffff0000, 0xffffffff, 0x00ff0000, 0x00fe0000, 0x00ffffff, 0x00ff0000, 0xffffffff, 0xffffffff]),
    (0x00ffffff, 0xffff0000, 255, true, [0x00ff0000, 0x00ff0000, 0xffffffff, 0xffff0000, 0xffff0000, 0xffff0000, 0x00ff0000, 0xfffe0000, 0x00ffffff, 0x00ff0000, 0xffff0000, 0x00ffffff, 0x00ff0000, 0x00fe0000, 0x00ffffff, 0x00ff0000, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0xffff0000, 200, false, [0x00ff3737, 0x00ff0000, 0xffffffff, 0xc8ff0000, 0xc8ff3636, 0xc7ff0000, 0x00ff3838, 0xc7fd3737, 0x00ffffff, 0xc7fe3737, 0xc7fe3737, 0x00ffffff, 0x00ff3838, 0x00fe3737, 0x00ffffff, 0x00ff3737, 0x00ffffff, 0xc8ffffff]),
    (0x00ffffff, 0xffff0000, 200, true, [0x00ff3737, 0x00ff0000, 0xffffffff, 0xc8ff0000, 0xc8ff3636, 0xc7ff0000, 0x00ff3838, 0xc7fd3737, 0x00ffffff, 0xc7fe3737, 0xc7fe3737, 0x00ffffff, 0x00ff3838, 0x00fe3737, 0x00ffffff, 0x00ff3737, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0xffff0000, 128, false, [0x00ff7f7f, 0x00ff0000, 0xffffffff, 0x80ff0000, 0x80ff7e7e, 0x7fff0000, 0x00ff8080, 0x7ffd7f7f, 0x00ffffff, 0x7ffe7f7f, 0x7ffe7f7f, 0x00ffffff, 0x00ff8080, 0x00fe7f7f, 0x00ffffff, 0x00ff7f7f, 0x00ffffff, 0x80ffffff]),
    (0x00ffffff, 0xffff0000, 128, true, [0x00ff7f7f, 0x00ff0000, 0xffffffff, 0x80ff0000, 0x80ff7e7e, 0x7fff0000, 0x00ff8080, 0x7ffd7f7f, 0x00ffffff, 0x7ffe7f7f, 0x7ffe7f7f, 0x00ffffff, 0x00ff8080, 0x00fe7f7f, 0x00ffffff, 0x00ff7f7f, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0xffff0000, 1, false, [0x00fffefe, 0x00ff0000, 0xffffffff, 0x01ff0000, 0x01fffdfd, 0x00ff0000, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fffefe, 0x00ffffff, 0x01ffffff]),
    (0x00ffffff, 0xffff0000, 1, true, [0x00fffefe, 0x00ff0000, 0xffffffff, 0x01ff0000, 0x01fffdfd, 0x00ff0000, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fffefe, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x8000ff00, 255, false, [0x8000ff00, 0x0000ff00, 0x80ffffff, 0xff00ff00, 0xff00ff00, 0x8000ff00, 0x007fff7f, 0x807efd7e, 0x00ffffff, 0x807eff7e, 0x807eff7e, 0x80ffffff, 0x0000ff00, 0x0000fe00, 0x00ffffff, 0x0000ff00, 0x80ffffff, 0xffffffff]),
    (0x00ffffff, 0x8000ff00, 255, true, [0x0000ff00, 0x0000ff00, 0x80ffffff, 0xff00ff00, 0xff00ff00, 0x8000ff00, 0x007fff7f, 0x807efd7e, 0x00ffffff, 0x007eff7e, 0x807eff7e, 0x00ffffff, 0x0000ff00, 0x0000fe00, 0x00ffffff, 0x0000ff00, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x8000ff00, 200, false, [0x0037ff37, 0x0000ff00, 0x80ffffff, 0xc800ff00, 0xc836ff36, 0x6400ff00, 0x009bff9b, 0x649afd9a, 0x00ffffff, 0x649aff9a, 0x649aff9a, 0x00ffffff, 0x0038ff38, 0x0037fe37, 0x00ffffff, 0x0037ff37, 0x00ffffff, 0x65ffffff]),
    (0x00ffffff, 0x8000ff00, 200, true, [0x0037ff37, 0x0000ff00, 0x80ffffff, 0xc800ff00, 0xc836ff36, 0x6400ff00, 0x009bff9b, 0x649afd9a, 0x00ffffff, 0x649aff9a, 0x649aff9a, 0x00ffffff, 0x0038ff38, 0x0037fe37, 0x00ffffff, 0x0037ff37, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x8000ff00, 128, false, [0x007fff7f, 0x0000ff00, 0x80ffffff, 0x8000ff00, 0x807eff7e, 0x4000ff00, 0x00bfffbf, 0x40befdbe, 0x00ffffff, 0x40beffbe, 0x40beffbe, 0x00ffffff, 0x0080ff80, 0x007ffe7f, 0x00ffffff, 0x007fff7f, 0x00ffffff, 0x41ffffff]),
    (0x00ffffff, 0x8000ff00, 128, true, [0x007fff7f, 0x0000ff00, 0x80ffffff, 0x8000ff00, 0x807eff7e, 0x4000ff00, 0x00bfffbf, 0x40befdbe, 0x00ffffff, 0x40beffbe, 0x40beffbe, 0x00ffffff, 0x0080ff80, 0x007ffe7f, 0x00ffffff, 0x007fff7f, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x8000ff00, 1, false, [0x00fefffe, 0x0000ff00, 0x80ffffff, 0x0100ff00, 0x01fdfffd, 0x0000ff00, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefffe, 0x00ffffff, 0x01ffffff]),
    (0x00ffffff, 0x8000ff00, 1, true, [0x00fefffe, 0x0000ff00, 0x80ffffff, 0x0100ff00, 0x01fdfffd, 0x0000ff00, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefffe, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0xffffffff, 255, false, [0xffffffff, 0x00ffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0x00ffffff, 0xfffefefe, 0x00ffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0xffffffff, 0xffffffff]),
    (0x00ffffff, 0xffffffff, 255, true, [0x00ffffff, 0x00ffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0x00ffffff, 0xfffefefe, 0x00ffffff, 0x00ffffff, 0xffffffff, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0xffffffff, 200, false, [0x00ffffff, 0x00ffffff, 0xffffffff, 0xc8ffffff, 0xc8ffffff, 0xc7ffffff, 0x00ffffff, 0xc7fdfdfd, 0x00ffffff, 0xc7fefefe, 0xc7fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0xc8ffffff]),
    (0x00ffffff, 0xffffffff, 200, true, [0x00ffffff, 0x00ffffff, 0xffffffff, 0xc8ffffff, 0xc8ffffff, 0xc7ffffff, 0x00ffffff, 0xc7fdfdfd, 0x00ffffff, 0xc7fefefe, 0xc7fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0xffffffff, 128, false, [0x00ffffff, 0x00ffffff, 0xffffffff, 0x80ffffff, 0x80ffffff, 0x7fffffff, 0x00ffffff, 0x7ffdfdfd, 0x00ffffff, 0x7ffefefe, 0x7ffefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x80ffffff]),
    (0x00ffffff, 0xffffffff, 128, true, [0x00ffffff, 0x00ffffff, 0xffffffff, 0x80ffffff, 0x80ffffff, 0x7fffffff, 0x00ffffff, 0x7ffdfdfd, 0x00ffffff, 0x7ffefefe, 0x7ffefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0xffffffff, 1, false, [0x00ffffff, 0x00ffffff, 0xffffffff, 0x01ffffff, 0x01ffffff, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x01ffffff]),
    (0x00ffffff, 0xffffffff, 1, true, [0x00ffffff, 0x00ffffff, 0xffffffff, 0x01ffffff, 0x01ffffff, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x40c0a080, 255, false, [0x40c0a080, 0x00c0a080, 0x40ffffff, 0xffc0a080, 0xffc0a080, 0x40c0a080, 0x00efe7df, 0x40eee6de, 0x00ffffff, 0x40ffffff, 0x40ffffff, 0x40ffffff, 0x00c0a080, 0x00bf9f7f, 0x00ffffff, 0x00c0a080, 0x40ffffff, 0xffffffff]),
    (0x00ffffff, 0x40c0a080, 255, true, [0x00c0a080, 0x00c0a080, 0x40ffffff, 0xffc0a080, 0xffc0a080, 0x40c0a080, 0x00efe7df, 0x40eee6de, 0x00ffffff, 0x00ffffff, 0x40ffffff, 0x00ffffff, 0x00c0a080, 0x00bf9f7f, 0x00ffffff, 0x00c0a080, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x40c0a080, 200, false, [0x00cdb49b, 0x00c0a080, 0x40ffffff, 0xc8c0a080, 0xc8f6d6b6, 0x32c0a080, 0x00f2ece6, 0x32f1ebe5, 0x00ffffff, 0x32ffffff, 0x32ffffff, 0x00ffffff, 0x00ceb59c, 0x00cdb49b, 0x00ffffff, 0x00cdb49b, 0x00ffffff, 0x33ffffff]),
    (0x00ffffff, 0x40c0a080, 200, true, [0x00cdb49b, 0x00c0a080, 0x40ffffff, 0xc8c0a080, 0xc8f6d6b6, 0x32c0a080, 0x00f2ece6, 0x32f1ebe5, 0x00ffffff, 0x32ffffff, 0x32ffffff, 0x00ffffff, 0x00ceb59c, 0x00cdb49b, 0x00ffffff, 0x00cdb49b, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x40c0a080, 128, false, [0x00dfcfbf, 0x00c0a080, 0x40ffffff, 0x80c0a080, 0x80fffffe, 0x20c0a080, 0x00f7f3ef, 0x20f6f2ee, 0x00ffffff, 0x20ffffff, 0x20ffffff, 0x00ffffff, 0x00e0d0c0, 0x00dfcfbf, 0x00ffffff, 0x00dfcfbf, 0x00ffffff, 0x21ffffff]),
    (0x00ffffff, 0x40c0a080, 128, true, [0x00dfcfbf, 0x00c0a080, 0x40ffffff, 0x80c0a080, 0x80fffffe, 0x20c0a080, 0x00f7f3ef, 0x20f6f2ee, 0x00ffffff, 0x20ffffff, 0x20ffffff, 0x00ffffff, 0x00e0d0c0, 0x00dfcfbf, 0x00ffffff, 0x00dfcfbf, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x40c0a080, 1, false, [0x00fefefe, 0x00c0a080, 0x40ffffff, 0x01c0a080, 0x01ffffff, 0x00c0a080, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x01ffffff]),
    (0x00ffffff, 0x40c0a080, 1, true, [0x00fefefe, 0x00c0a080, 0x40ffffff, 0x01c0a080, 0x01ffffff, 0x00c0a080, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x00abcdef, 255, false, [0x00abcdef, 0x00abcdef, 0x00ffffff, 0xffabcdef, 0xffabcdef, 0x00abcdef, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00abcdef, 0x00aaccee, 0x00ffffff, 0x00abcdef, 0x00ffffff, 0xffffffff]),
    (0x00ffffff, 0x00abcdef, 255, true, [0x00abcdef, 0x00abcdef, 0x00ffffff, 0xffabcdef, 0xffabcdef, 0x00abcdef, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00abcdef, 0x00aaccee, 0x00ffffff, 0x00abcdef, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x00abcdef, 200, false, [0x00bdd7f2, 0x00abcdef, 0x00ffffff, 0xc8abcdef, 0xc8e1ffff, 0x00abcdef, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00bed8f3, 0x00bdd7f2, 0x00ffffff, 0x00bdd7f2, 0x00ffffff, 0x01ffffff]),
    (0x00ffffff, 0x00abcdef, 200, true, [0x00bdd7f2, 0x00abcdef, 0x00ffffff, 0xc8abcdef, 0xc8e1ffff, 0x00abcdef, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00bed8f3, 0x00bdd7f2, 0x00ffffff, 0x00bdd7f2, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x00abcdef, 128, false, [0x00d5e6f7, 0x00abcdef, 0x00ffffff, 0x80abcdef, 0x80ffffff, 0x00abcdef, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00d5e6f7, 0x00d4e5f6, 0x00ffffff, 0x00d5e6f7, 0x00ffffff, 0x01ffffff]),
    (0x00ffffff, 0x00abcdef, 128, true, [0x00d5e6f7, 0x00abcdef, 0x00ffffff, 0x80abcdef, 0x80ffffff, 0x00abcdef, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00ffffff, 0x00d5e6f7, 0x00d4e5f6, 0x00ffffff, 0x00d5e6f7, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x00abcdef, 1, false, [0x00fefefe, 0x00abcdef, 0x00ffffff, 0x01abcdef, 0x01ffffff, 0x00abcdef, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x01ffffff]),
    (0x00ffffff, 0x00abcdef, 1, true, [0x00fefefe, 0x00abcdef, 0x00ffffff, 0x01abcdef, 0x01ffffff, 0x00abcdef, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x7f808080, 255, false, [0x7f808080, 0x00808080, 0x7fffffff, 0xff808080, 0xff808080, 0x7f808080, 0x00bfbfbf, 0x7fbebebe, 0x00ffffff, 0x7fffffff, 0x7fffffff, 0x7fffffff, 0x00808080, 0x007f7f7f, 0x00ffffff, 0x00808080, 0x7fffffff, 0xffffffff]),
    (0x00ffffff, 0x7f808080, 255, true, [0x00808080, 0x00808080, 0x7fffffff, 0xff808080, 0xff808080, 0x7f808080, 0x00bfbfbf, 0x7fbebebe, 0x00ffffff, 0x00ffffff, 0x7fffffff, 0x00ffffff, 0x00808080, 0x007f7f7f, 0x00ffffff, 0x00808080, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x7f808080, 200, false, [0x009b9b9b, 0x00808080, 0x7fffffff, 0xc8808080, 0xc8b6b6b6, 0x63808080, 0x00cdcdcd, 0x63cccccc, 0x00ffffff, 0x63ffffff, 0x63ffffff, 0x00ffffff, 0x009c9c9c, 0x009b9b9b, 0x00ffffff, 0x009b9b9b, 0x00ffffff, 0x64ffffff]),
    (0x00ffffff, 0x7f808080, 200, true, [0x009b9b9b, 0x00808080, 0x7fffffff, 0xc8808080, 0xc8b6b6b6, 0x63808080, 0x00cdcdcd, 0x63cccccc, 0x00ffffff, 0x63ffffff, 0x63ffffff, 0x00ffffff, 0x009c9c9c, 0x009b9b9b, 0x00ffffff, 0x009b9b9b, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x7f808080, 128, false, [0x00bfbfbf, 0x00808080, 0x7fffffff, 0x80808080, 0x80fefefe, 0x3f808080, 0x00dfdfdf, 0x3fdedede, 0x00ffffff, 0x3fffffff, 0x3fffffff, 0x00ffffff, 0x00c0c0c0, 0x00bfbfbf, 0x00ffffff, 0x00bfbfbf, 0x00ffffff, 0x40ffffff]),
    (0x00ffffff, 0x7f808080, 128, true, [0x00bfbfbf, 0x00808080, 0x7fffffff, 0x80808080, 0x80fefefe, 0x3f808080, 0x00dfdfdf, 0x3fdedede, 0x00ffffff, 0x3fffffff, 0x3fffffff, 0x00ffffff, 0x00c0c0c0, 0x00bfbfbf, 0x00ffffff, 0x00bfbfbf, 0x00ffffff, 0x00ffffff]),
    (0x00ffffff, 0x7f808080, 1, false, [0x00fefefe, 0x00808080, 0x7fffffff, 0x01808080, 0x01ffffff, 0x00808080, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x01ffffff]),
    (0x00ffffff, 0x7f808080, 1, true, [0x00fefefe, 0x00808080, 0x7fffffff, 0x01808080, 0x01ffffff, 0x00808080, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00fefefe, 0x00ffffff, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00fefefe, 0x00ffffff, 0x00ffffff]),
    (0x80402010, 0xffff0000, 255, false, [0xffff0000, 0x80ff0000, 0xff402010, 0xffff0000, 0xffff0000, 0xfffe0000, 0x00fe0000, 0xfffe0000, 0x80402010, 0xffff0000, 0xffff0000, 0xffff2010, 0x80400000, 0x003f0000, 0x00ff2010, 0x80400000, 0xffff2010, 0xffff2111]),
    (0x80402010, 0xffff0000, 255, true, [0x80ff0000, 0x80ff0000, 0xff402010, 0xffff0000, 0xffff0000, 0xfffe0000, 0x80fe0000, 0xfffe0000, 0x80402010, 0x80ff0000, 0xffff0000, 0x80ff2010, 0x80400000, 0x803f0000, 0x80ff2010, 0x80400000, 0x80ff2010, 0x80ff2111]),
    (0x80402010, 0xffff0000, 200, false, [0x00d50703, 0x80ff0000, 0xff402010, 0xe4e70402, 0xe4ff0603, 0xe4e60402, 0x00d40703, 0xe4d40703, 0x80402010, 0xc7d50703, 0xe4d50703, 0x80ff2010, 0x80400000, 0x003f0703, 0x00ff2010, 0x00400703, 0x00d52010, 0xe4d62111]),
    (0x80402010, 0xffff0000, 200, true, [0x80d50703, 0x80ff0000, 0xff402010, 0xe4e70402, 0xe4ff0603, 0xe4e60402, 0x80d40703, 0xe4d40703, 0x80402010, 0xc7d50703, 0xe4d50703, 0x80ff2010, 0x80400000, 0x803f0703, 0x80ff2010, 0x80400703, 0x80d52010, 0x80d62111]),
    (0x80402010, 0xffff0000, 128, false, [0x009f1008, 0x80ff0000, 0xff402010, 0xc0be0a05, 0xc0ff0f07, 0xc0be0a05, 0x009e1008, 0xc09e1008, 0x80402010, 0x7f9f1008, 0xc09f1008, 0x80bf2010, 0x80400000, 0x003f1008, 0x00802010, 0x00401008, 0x009f2010, 0xc0a02111]),
    (0x80402010, 0xffff0000, 128, true, [0x809f1008, 0x80ff0000, 0xff402010, 0xc0be0a05, 0xc0ff0f07, 0xc0be0a05, 0x809e1008, 0xc09e1008, 0x80402010, 0x7f9f1008, 0xc09f1008, 0x80bf2010, 0x80400000, 0x803f1008, 0x80802010, 0x80401008, 0x809f2010, 0x80a02111]),
    (0x80402010, 0xffff0000, 1, false, [0x00401f0f, 0x80ff0000, 0xff402010, 0x81401f0f, 0x81ff1f0f, 0x80402010, 0x00402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x003f1f0f, 0x00402010, 0x00401f0f, 0x00402010, 0x81412111]),
    (0x80402010, 0xffff0000, 1, true, [0x80401f0f, 0x80ff0000, 0xff402010, 0x81401f0f, 0x81ff1f0f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x80401f0f, 0x80402010, 0x80412111]),
    (0x80402010, 0x8000ff00, 255, false, [0x8000ff00, 0x8000ff00, 0x80402010, 0xff00ff00, 0xff00ff00, 0xc015b405, 0x00208f08, 0xc01f8e07, 0x80402010, 0x801fff07, 0xc01fff07, 0xff40ff10, 0x01002000, 0x00001f00, 0x0040ff10, 0x80002000, 0x8040ff10, 0xff41ff11]),
    (0x80402010, 0x8000ff00, 255, true, [0x8000ff00, 0x8000ff00, 0x80402010, 0xff00ff00, 0xff00ff00, 0xc015b405, 0x80208f08, 0xc01f8e07, 0x80402010, 0x801fff07, 0xc01fff07, 0x8040ff10, 0x80002000, 0x80001f00, 0x8040ff10, 0x80002000, 0x8040ff10, 0x8041ff11]),
    (0x80402010, 0x8000ff00, 200, false, [0x000ece03, 0x8000ff00, 0x80402010, 0xe408e302, 0xe40dff03, 0xb21c9c07, 0x00277709, 0xb2267609, 0x80402010, 0x6426da09, 0xb226da09, 0x8040e710, 0x80002000, 0x000e1f03, 0x00409210, 0x000e2003, 0x0040ce10, 0xb341cf11]),
    (0x80402010, 0x8000ff00, 200, true, [0x800ece03, 0x8000ff00, 0x80402010, 0xe408e302, 0xe40dff03, 0xb21c9c07, 0x80277709, 0xb2267609, 0x80402010, 0x6426da09, 0xb226da09, 0x8040e710, 0x80002000, 0x800e1f03, 0x80409210, 0x800e2003, 0x8040ce10, 0x8041cf11]),
    (0x80402010, 0x8000ff00, 128, false, [0x00208f08, 0x8000ff00, 0x80402010, 0xc015b405, 0xc01fff07, 0xa0267809, 0x0030570c, 0xa02f560b, 0x80402010, 0x402f960b, 0xa02f960b, 0x80409f10, 0x80002000, 0x00201f08, 0x00404010, 0x00202008, 0x00408f10, 0xa1419011]),
    (0x80402010, 0x8000ff00, 128, true, [0x80208f08, 0x8000ff00, 0x80402010, 0xc015b405, 0xc01fff07, 0xa0267809, 0x8030570c, 0xa02f560b, 0x80402010, 0x402f960b, 0xa02f960b, 0x80409f10, 0x80002000, 0x80201f08, 0x80404010, 0x80202008, 0x80408f10, 0x80419011]),
    (0x80402010, 0x8000ff00, 1, false, [0x003f200f, 0x8000ff00, 0x80402010, 0x813f200f, 0x813fff0f, 0x80402010, 0x00402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x003f1f0f, 0x00402010, 0x003f200f, 0x00402010, 0x81412111]),
    (0x80402010, 0x8000ff00, 1, true, [0x803f200f, 0x8000ff00, 0x80402010, 0x813f200f, 0x813fff0f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x803f200f, 0x80402010, 0x80412111]),
    (0x80402010, 0xffffffff, 255, false, [0xffffffff, 0x80ffffff, 0xff402010, 0xffffffff, 0xffffffff, 0xfffefefe, 0x00fefefe, 0xfffefefe, 0x80402010, 0xffffffff, 0xffffffff, 0xffffffff, 0x80402010, 0x003f1f0f, 0x00ffffff, 0x80402010, 0xffffffff, 0xffffffff]),
    (0x80402010, 0xffffffff, 255, true, [0x80ffffff, 0x80ffffff, 0xff402010, 0xffffffff, 0xffffffff, 0xfffefefe, 0x80fefefe, 0xfffefefe, 0x80402010, 0x80ffffff, 0xffffffff, 0x80ffffff, 0x80402010, 0x803f1f0f, 0x80ffffff, 0x80402010, 0x80ffffff, 0x80ffffff]),
    (0x80402010, 0xffffffff, 200, false, [0x00d5ceca, 0x80ffffff, 0xff402010, 0xe4e7e3e1, 0xe4ffffff, 0xe4e6e2e0, 0x00d4cdc9, 0xe4d4cdc9, 0x80402010, 0xc7d5ceca, 0xe4d5ceca, 0x80ffe7d7, 0x80402010, 0x003f1f0f, 0x00ff9249, 0x00402010, 0x00d5ceca, 0xe4d6cfcb]),
    (0x80402010, 0xffffffff, 200, true, [0x80d5ceca, 0x80ffffff, 0xff402010, 0xe4e7e3e1, 0xe4ffffff, 0xe4e6e2e0, 0x80d4cdc9, 0xe4d4cdc9, 0x80402010, 0xc7d5ceca, 0xe4d5ceca, 0x80ffe7d7, 0x80402010, 0x803f1f0f, 0x80ff9249, 0x80402010, 0x80d5ceca, 0x80d6cfcb]),
    (0x80402010, 0xffffffff, 128, false, [0x009f8f87, 0x80ffffff, 0xff402010, 0xc0beb4ae, 0xc0ffffff, 0xc0beb3ad, 0x009e8e86, 0xc09e8e86, 0x80402010, 0x7f9f8f87, 0xc09f8f87, 0x80bf9f8f, 0x80402010, 0x003f1f0f, 0x00804020, 0x00402010, 0x009f8f87, 0xc0a09088]),
    (0x80402010, 0xffffffff, 128, true, [0x809f8f87, 0x80ffffff, 0xff402010, 0xc0beb4ae, 0xc0ffffff, 0xc0beb3ad, 0x809e8e86, 0xc09e8e86, 0x80402010, 0x7f9f8f87, 0xc09f8f87, 0x80bf9f8f, 0x80402010, 0x803f1f0f, 0x80804020, 0x80402010, 0x809f8f87, 0x80a09088]),
    (0x80402010, 0xffffffff, 1, false, [0x00402010, 0x80ffffff, 0xff402010, 0x81402010, 0x81ffffff, 0x80402010, 0x00402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x003f1f0f, 0x00402010, 0x00402010, 0x00402010, 0x81412111]),
    (0x80402010, 0xffffffff, 1, true, [0x80402010, 0x80ffffff, 0xff402010, 0x81402010, 0x81ffffff, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x80402010, 0x80402010, 0x80412111]),
    (0x80402010, 0x40c0a080, 255, false, [0x40c0a080, 0x80c0a080, 0x40402010, 0xffc0a080, 0xffc0a080, 0xa073533c, 0x0060402c, 0xa05f3f2b, 0x80402010, 0x40efb78b, 0xa0efb78b, 0xc0ffc090, 0x00010000, 0x00301408, 0x00ff5620, 0x40402010, 0x80c0a080, 0xffd0ad89]),
    (0x80402010, 0x40c0a080, 255, true, [0x80c0a080, 0x80c0a080, 0x40402010, 0xffc0a080, 0xffc0a080, 0xa073533c, 0x8060402c, 0xa05f3f2b, 0x80402010, 0x80efb78b, 0xa0efb78b, 0x80ffc090, 0x80010000, 0x80301408, 0x80ff5620, 0x80402010, 0x80c0a080, 0x80d0ad89]),
    (0x80402010, 0x40c0a080, 200, false, [0x00a48467, 0x80c0a080, 0x40402010, 0xe4b09072, 0xe4cda683, 0x99694934, 0x00593925, 0x99583825, 0x80402010, 0x32c99670, 0x99c99670, 0x80d69d74, 0x800f0000, 0x00331609, 0x009c3f1a, 0x00402010, 0x00a48467, 0x9ab18e6f]),
    (0x80402010, 0x40c0a080, 200, true, [0x80a48467, 0x80c0a080, 0x40402010, 0xe4b09072, 0xe4cda683, 0x99694934, 0x80593925, 0x99583825, 0x80402010, 0x32c99670, 0x99c99670, 0x80d69d74, 0x800f0000, 0x80331609, 0x809c3f1a, 0x80402010, 0x80a48467, 0x80b18e6f]),
    (0x80402010, 0x40c0a080, 128, false, [0x00806048, 0x80c0a080, 0x40402010, 0xc095755a, 0xc0dfaf87, 0x905c3c28, 0x0050301e, 0x904f2f1d, 0x80402010, 0x20976b4d, 0x90976b4d, 0x80a07050, 0x80210000, 0x00381a0c, 0x00672e15, 0x00402010, 0x00806048, 0x9189674d]),
    (0x80402010, 0x40c0a080, 128, true, [0x80806048, 0x80c0a080, 0x40402010, 0xc095755a, 0xc0dfaf87, 0x905c3c28, 0x8050301e, 0x904f2f1d, 0x80402010, 0x20976b4d, 0x90976b4d, 0x80a07050, 0x80210000, 0x80381a0c, 0x80672e15, 0x80402010, 0x80806048, 0x8089674d]),
    (0x80402010, 0x40c0a080, 1, false, [0x00402010, 0x80c0a080, 0x40402010, 0x81402010, 0x81ffbf8f, 0x80402010, 0x00402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x003f1f0f, 0x00402010, 0x00402010, 0x00402010, 0x81412111]),
    (0x80402010, 0x40c0a080, 1, true, [0x80402010, 0x80c0a080, 0x40402010, 0x81402010, 0x81ffbf8f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x80402010, 0x80402010, 0x80412111]),
    (0x80402010, 0x00abcdef, 255, false, [0x00abcdef, 0x80abcdef, 0x00402010, 0xffabcdef, 0xffabcdef, 0x80402010, 0x00402010, 0x803f1f0f, 0x80402010, 0x00eaecfe, 0x80eaecfe, 0x80ebedff, 0x00000000, 0x002a190e, 0x00c3a3ff, 0x00402010, 0x80abcdef, 0xffc1d4f1]),
    (0x80402010, 0x00abcdef, 255, true, [0x80abcdef, 0x80abcdef, 0x00402010, 0xffabcdef, 0xffabcdef, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x80eaecfe, 0x80eaecfe, 0x80ebedff, 0x80000000, 0x802a190e, 0x80c3a3ff, 0x80402010, 0x80abcdef, 0x80c1d4f1]),
    (0x80402010, 0x00abcdef, 200, false, [0x0093a7be, 0x80abcdef, 0x00402010, 0xe49db7d3, 0xe4b8d3f2, 0x80402010, 0x00402010, 0x803f1f0f, 0x80402010, 0x00c4bfc9, 0x80c4bfc9, 0x80c5c0ca, 0x80000004, 0x002f1b0f, 0x0086563b, 0x00402010, 0x0093a7be, 0x81a4adbf]),
    (0x80402010, 0x00abcdef, 200, true, [0x8093a7be, 0x80abcdef, 0x00402010, 0xe49db7d3, 0xe4b8d3f2, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x00c4bfc9, 0x80c4bfc9, 0x80c5c0ca, 0x80000004, 0x802f1b0f, 0x8086563b, 0x80402010, 0x8093a7be, 0x80a4adbf]),
    (0x80402010, 0x00abcdef, 128, false, [0x0075767f, 0x80abcdef, 0x00402010, 0xc08792a4, 0xc0cadcf6, 0x80402010, 0x00402010, 0x803f1f0f, 0x80402010, 0x00948586, 0x80948586, 0x80958687, 0x80160708, 0x00351c0f, 0x0060351e, 0x00402010, 0x0075767f, 0x81817a81]),
    (0x80402010, 0x00abcdef, 128, true, [0x8075767f, 0x80abcdef, 0x00402010, 0xc08792a4, 0xc0cadcf6, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x00948586, 0x80948586, 0x80958687, 0x80160708, 0x80351c0f, 0x8060351e, 0x80402010, 0x8075767f, 0x80817a81]),
    (0x80402010, 0x00abcdef, 1, false, [0x00402010, 0x80abcdef, 0x00402010, 0x81402010, 0x81eaecfe, 0x80402010, 0x00402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x003f1f0f, 0x00402010, 0x00402010, 0x00402010, 0x81412111]),
    (0x80402010, 0x00abcdef, 1, true, [0x80402010, 0x80abcdef, 0x00402010, 0x81402010, 0x81eaecfe, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x80402010, 0x80402010, 0x80412111]),
    (0x80402010, 0x7f808080, 255, false, [0x7f808080, 0x80808080, 0x7f402010, 0xff808080, 0xff808080, 0xc06a5f59, 0x005f4f47, 0xc05f4f47, 0x80402010, 0x7fa09088, 0xc0a09088, 0xffc0a090, 0x00000000, 0x00201008, 0x00814020, 0x7f402010, 0x80808080, 0xffa19189]),
    (0x80402010, 0x7f808080, 255, true, [0x80808080, 0x80808080, 0x7f402010, 0xff808080, 0xff808080, 0xc06a5f59, 0x805f4f47, 0xc05f4f47, 0x80402010, 0x80a09088, 0xc0a09088, 0x80c0a090, 0x80000000, 0x80201008, 0x80814020, 0x80402010, 0x80808080, 0x80a19189]),
    (0x80402010, 0x7f808080, 200, false, [0x00726b67, 0x80808080, 0x7f402010, 0xe4787472, 0xe48d8683, 0xb263554e, 0x0058453b, 0xb258443a, 0x80402010, 0x638b776d, 0xb28b776d, 0x80a48474, 0x80000000, 0x00271309, 0x0069341a, 0x00402010, 0x00726b67, 0xb28c786f]),
    (0x80402010, 0x7f808080, 200, true, [0x80726b67, 0x80808080, 0x7f402010, 0xe4787472, 0xe48d8683, 0xb263554e, 0x8058453b, 0xb258443a, 0x80402010, 0x638b776d, 0xb28b776d, 0x80a48474, 0x80000000, 0x80271309, 0x8069341a, 0x80402010, 0x80726b67, 0x808c786f]),
    (0x80402010, 0x7f808080, 128, false, [0x00605048, 0x80808080, 0x7f402010, 0xc06a5f5a, 0xc09f8f87, 0xa059453b, 0x004f372b, 0xa04f372b, 0x80402010, 0x3f70584c, 0xa070584c, 0x80806050, 0x80010000, 0x0030180c, 0x00552a15, 0x00402010, 0x00605048, 0xa071594d]),
    (0x80402010, 0x7f808080, 128, true, [0x80605048, 0x80808080, 0x7f402010, 0xc06a5f5a, 0xc09f8f87, 0xa059453b, 0x804f372b, 0xa04f372b, 0x80402010, 0x3f70584c, 0xa070584c, 0x80806050, 0x80010000, 0x8030180c, 0x80552a15, 0x80402010, 0x80605048, 0x8071594d]),
    (0x80402010, 0x7f808080, 1, false, [0x00402010, 0x80808080, 0x7f402010, 0x81402010, 0x81bf9f8f, 0x80402010, 0x00402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x003f1f0f, 0x00402010, 0x00402010, 0x00402010, 0x81412111]),
    (0x80402010, 0x7f808080, 1, true, [0x80402010, 0x80808080, 0x7f402010, 0x81402010, 0x81bf9f8f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x003f1f0f, 0x803f1f0f, 0x80402010, 0x80402010, 0x803f1f0f, 0x80402010, 0x80402010, 0x80402010, 0x80412111]),
    (0xff123456, 0xffff0000, 255, false, [0xffff0000, 0xffff0000, 0xff123456, 0xffff0000, 0xffff0000, 0xfffe0000, 0x00fe0000, 0xfffe0000, 0xff123456, 0xffff0000, 0xffff0000, 0xffff3456, 0xff120000, 0x00110000, 0x00ff3456, 0xff120000, 0xffff3456, 0xffff3557]),
    (0xff123456, 0xffff0000, 255, true, [0xffff0000, 0xffff0000, 0xff123456, 0xffff0000, 0xffff0000, 0xfffe0000, 0xfffe0000, 0xfffe0000, 0xff123456, 0xffff0000, 0xffff0000, 0xffff3456, 0xff120000, 0xff110000, 0xffff3456, 0xff120000, 0xffff3456, 0xffff3557]),
    (0xff123456, 0xffff0000, 200, false, [0x00cb0b12, 0xffff0000, 0xff123456, 0xffcb0b12, 0xffff0b12, 0xffca0b13, 0x00ca0b13, 0xffc90b12, 0xff123456, 0xc7ca0b12, 0xffca0b12, 0xffd93456, 0xff120000, 0x00110b12, 0x00523456, 0x00120b12, 0x00cb3456, 0xffcc3557]),
    (0xff123456, 0xffff0000, 200, true, [0xffcb0b12, 0xffff0000, 0xff123456, 0xffcb0b12, 0xffff0b12, 0xffca0b13, 0xffca0b13, 0xffc90b12, 0xff123456, 0xc7ca0b12, 0xffca0b12, 0xffd93456, 0xff120000, 0xff110b12, 0xff523456, 0xff120b12, 0xffcb3456, 0xffcc3557]),
    (0xff123456, 0xffff0000, 128, false, [0x00881a2b, 0xffff0000, 0xff123456, 0xff881a2b, 0xffff192a, 0xff871a2b, 0x00871a2b, 0xff871a2b, 0xff123456, 0x7f881a2b, 0xff881a2b, 0xff913456, 0xff120000, 0x00111a2b, 0x00243456, 0x00121a2b, 0x00883456, 0xff893557]),
    (0xff123456, 0xffff0000, 128, true, [0xff881a2b, 0xffff0000, 0xff123456, 0xff881a2b, 0xffff192a, 0xff871a2b, 0xff871a2b, 0xff871a2b, 0xff123456, 0x7f881a2b, 0xff881a2b, 0xff913456, 0xff120000, 0xff111a2b, 0xff243456, 0xff121a2b, 0xff883456, 0xff893557]),
    (0xff123456, 0xffff0000, 1, false, [0x00123355, 0xffff0000, 0xff123456, 0xff123355, 0xffff3355, 0xff123456, 0x00123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0x00113355, 0x00123456, 0x00123355, 0x00123456, 0xff133557]),
    (0xff123456, 0xffff0000, 1, true, [0xff123355, 0xffff0000, 0xff123456, 0xff123355, 0xffff3355, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0xff123355, 0xff123456, 0xff133557]),
    (0xff123456, 0x8000ff00, 255, false, [0x8000ff00, 0xff00ff00, 0x80123456, 0xff00ff00, 0xff00ff00, 0xff09992b, 0x0009992b, 0xff08982a, 0xff123456, 0x8008ff2a, 0xff08ff2a, 0xff12ff56, 0x80003400, 0x00003300, 0x0012ff56, 0x80003400, 0xff12ff56, 0xff13ff57]),
    (0xff123456, 0x8000ff00, 255, true, [0xff00ff00, 0xff00ff00, 0x80123456, 0xff00ff00, 0xff00ff00, 0xff09992b, 0xff09992b, 0xff08982a, 0xff123456, 0xff08ff2a, 0xff08ff2a, 0xff12ff56, 0xff003400, 0xff003300, 0xff12ff56, 0xff003400, 0xff12ff56, 0xff13ff57]),
    (0xff123456, 0x8000ff00, 200, false, [0x0003d212, 0xff00ff00, 0x80123456, 0xff03d212, 0xff03ff12, 0xff0a8334, 0x000a8334, 0xff0a8234, 0xff123456, 0x640ae634, 0xff0ae634, 0xff12fb56, 0xff003400, 0x00033312, 0x0012ed56, 0x00033412, 0x0012d256, 0xff13d357]),
    (0xff123456, 0x8000ff00, 200, true, [0xff03d212, 0xff00ff00, 0x80123456, 0xff03d212, 0xff03ff12, 0xff0a8334, 0xff0a8334, 0xff0a8234, 0xff123456, 0x640ae634, 0xff0ae634, 0xff12fb56, 0xff003400, 0xff033312, 0xff12ed56, 0xff033412, 0xff12d256, 0xff13d357]),
    (0xff123456, 0x8000ff00, 128, false, [0x0009992b, 0xff00ff00, 0x80123456, 0xff09992b, 0xff08ff2a, 0xff0d6640, 0x000d6640, 0xff0d6540, 0xff123456, 0x400da540, 0xff0da540, 0xff12b356, 0xff003400, 0x0009332b, 0x00126856, 0x0009342b, 0x00129956, 0xff139a57]),
    (0xff123456, 0x8000ff00, 128, true, [0xff09992b, 0xff00ff00, 0x80123456, 0xff09992b, 0xff08ff2a, 0xff0d6640, 0xff0d6640, 0xff0d6540, 0xff123456, 0x400da540, 0xff0da540, 0xff12b356, 0xff003400, 0xff09332b, 0xff126856, 0xff09342b, 0xff129956, 0xff139a57]),
    (0xff123456, 0x8000ff00, 1, false, [0x00113455, 0xff00ff00, 0x80123456, 0xff113455, 0xff11ff55, 0xff123456, 0x00123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0x00113355, 0x00123456, 0x00113455, 0x00123456, 0xff133557]),
    (0xff123456, 0x8000ff00, 1, true, [0xff113455, 0xff00ff00, 0x80123456, 0xff113455, 0xff11ff55, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0xff113455, 0xff123456, 0xff133557]),
    (0xff123456, 0xffffffff, 255, false, [0xffffffff, 0xffffffff, 0xff123456, 0xffffffff, 0xffffffff, 0xfffefefe, 0x00fefefe, 0xfffefefe, 0xff123456, 0xffffffff, 0xffffffff, 0xffffffff, 0xff123456, 0x00113355, 0x00ffffff, 0xff123456, 0xffffffff, 0xffffffff]),
    (0xff123456, 0xffffffff, 255, true, [0xffffffff, 0xffffffff, 0xff123456, 0xffffffff, 0xffffffff, 0xfffefefe, 0xfffefefe, 0xfffefefe, 0xff123456, 0xffffffff, 0xffffffff, 0xffffffff, 0xff123456, 0xff113355, 0xffffffff, 0xff123456, 0xffffffff, 0xffffffff]),
    (0xff123456, 0xffffffff, 200, false, [0x00cbd2da, 0xffffffff, 0xff123456, 0xffcbd2da, 0xffffffff, 0xffcad1d9, 0x00cad1d9, 0xffc9d1d8, 0xff123456, 0xc7cad2d9, 0xffcad2d9, 0xffd9fbff, 0xff123456, 0x00113355, 0x0052edff, 0x00123456, 0x00cbd2da, 0xffccd3db]),
    (0xff123456, 0xffffffff, 200, true, [0xffcbd2da, 0xffffffff, 0xff123456, 0xffcbd2da, 0xffffffff, 0xffcad1d9, 0xffcad1d9, 0xffc9d1d8, 0xff123456, 0xc7cad2d9, 0xffcad2d9, 0xffd9fbff, 0xff123456, 0xff113355, 0xff52edff, 0xff123456, 0xffcbd2da, 0xffccd3db]),
    (0xff123456, 0xffffffff, 128, false, [0x008899aa, 0xffffffff, 0xff123456, 0xff8899aa, 0xffffffff, 0xff8798a9, 0x008798a9, 0xff8798a9, 0xff123456, 0x7f8899aa, 0xff8899aa, 0xff91b3d5, 0xff123456, 0x00113355, 0x002468ac, 0x00123456, 0x008899aa, 0xff899aab]),
    (0xff123456, 0xffffffff, 128, true, [0xff8899aa, 0xffffffff, 0xff123456, 0xff8899aa, 0xffffffff, 0xff8798a9, 0xff8798a9, 0xff8798a9, 0xff123456, 0x7f8899aa, 0xff8899aa, 0xff91b3d5, 0xff123456, 0xff113355, 0xff2468ac, 0xff123456, 0xff8899aa, 0xff899aab]),
    (0xff123456, 0xffffffff, 1, false, [0x00123456, 0xffffffff, 0xff123456, 0xff123456, 0xffffffff, 0xff123456, 0x00123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0x00113355, 0x00123456, 0x00123456, 0x00123456, 0xff133557]),
    (0xff123456, 0xffffffff, 1, true, [0xff123456, 0xffffffff, 0xff123456, 0xff123456, 0xffffffff, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0xff123456, 0xff123456, 0xff133557]),
    (0xff123456, 0x40c0a080, 255, false, [0x40c0a080, 0xffc0a080, 0x40123456, 0xffc0a080, 0xffc0a080, 0xff3d4f60, 0x003d4f60, 0xff3d4e60, 0xff123456, 0x40cdc6c0, 0xffcdc6c0, 0xffd2d4d6, 0x40000000, 0x000d202b, 0x00498bad, 0x40123456, 0xffc0a080, 0xffc5b4ac]),
    (0xff123456, 0x40c0a080, 255, true, [0xffc0a080, 0xffc0a080, 0x40123456, 0xffc0a080, 0xffc0a080, 0xff3d4f60, 0xff3d4f60, 0xff3d4e60, 0xff123456, 0xffcdc6c0, 0xffcdc6c0, 0xffd2d4d6, 0xff000000, 0xff0d202b, 0xff498bad, 0xff123456, 0xffc0a080, 0xffc5b4ac]),
    (0xff123456, 0x40c0a080, 200, false, [0x00998876, 0xffc0a080, 0x40123456, 0xff998876, 0xffc3ab92, 0xff33495e, 0x0033495e, 0xff33485d, 0xff123456, 0x32a4a6a8, 0xffa4a6a8, 0xffa8b1ba, 0xff000000, 0x000e2434, 0x002b668d, 0x00123456, 0x00998876, 0xff9e9899]),
    (0xff123456, 0x40c0a080, 200, true, [0xff998876, 0xffc0a080, 0x40123456, 0xff998876, 0xffc3ab92, 0xff33495e, 0xff33495e, 0xff33485d, 0xff123456, 0x32a4a6a8, 0xffa4a6a8, 0xffa8b1ba, 0xff000000, 0xff0e2434, 0xff2b668d, 0xff123456, 0xff998876, 0xff9e9899]),
    (0xff123456, 0x40c0a080, 128, false, [0x00696a6b, 0xffc0a080, 0x40123456, 0xff696a6b, 0xffc8b9aa, 0xff27415b, 0x0027415b, 0xff27415a, 0xff123456, 0x206f7d8a, 0xff6f7d8a, 0xff728496, 0xff000517, 0x000f2a40, 0x001c4b73, 0x00123456, 0x00696a6b, 0xff6c7581]),
    (0xff123456, 0x40c0a080, 128, true, [0xff696a6b, 0xffc0a080, 0x40123456, 0xff696a6b, 0xffc8b9aa, 0xff27415b, 0xff27415b, 0xff27415a, 0xff123456, 0x206f7d8a, 0xff6f7d8a, 0xff728496, 0xff000517, 0xff0f2a40, 0xff1c4b73, 0xff123456, 0xff696a6b, 0xff6c7581]),
    (0xff123456, 0x40c0a080, 1, false, [0x00123456, 0xffc0a080, 0x40123456, 0xff123456, 0xffd1d3d5, 0xff123456, 0x00123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0x00113355, 0x00123456, 0x00123456, 0x00123456, 0xff133557]),
    (0xff123456, 0x40c0a080, 1, true, [0xff123456, 0xffc0a080, 0x40123456, 0xff123456, 0xffd1d3d5, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0xff123456, 0xff123456, 0xff133557]),
    (0xff123456, 0x00abcdef, 255, false, [0x00abcdef, 0xffabcdef, 0x00123456, 0xffabcdef, 0xffabcdef, 0xff123456, 0x00123456, 0xff113355, 0xff123456, 0x00bcffff, 0xffbcffff, 0xffbdffff, 0x00000246, 0x000c2950, 0x0036ffff, 0x00123456, 0xffabcdef, 0xffb2d8f5]),
    (0xff123456, 0x00abcdef, 255, true, [0xffabcdef, 0xffabcdef, 0x00123456, 0xffabcdef, 0xffabcdef, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0xffbcffff, 0xffbcffff, 0xffbdffff, 0xff000246, 0xff0c2950, 0xff36ffff, 0xff123456, 0xffabcdef, 0xffb2d8f5]),
    (0xff123456, 0x00abcdef, 200, false, [0x0089abcd, 0xffabcdef, 0x00123456, 0xff89abcd, 0xffaed8ff, 0xff123456, 0x00123456, 0xff113355, 0xff123456, 0x0096d3ff, 0xff96d3ff, 0xff97d4ff, 0xff000d4a, 0x000d2b51, 0x00258bff, 0x00123456, 0x0089abcd, 0xff8fb4d2]),
    (0xff123456, 0x00abcdef, 200, true, [0xff89abcd, 0xffabcdef, 0x00123456, 0xff89abcd, 0xffaed8ff, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0x0096d3ff, 0xff96d3ff, 0xff97d4ff, 0xff000d4a, 0xff0d2b51, 0xff258bff, 0xff123456, 0xff89abcd, 0xff8fb4d2]),
    (0xff123456, 0x00abcdef, 128, false, [0x005e80a2, 0xffabcdef, 0x00123456, 0xff5e80a2, 0xffb3e6ff, 0xff123456, 0x00123456, 0xff113355, 0xff123456, 0x006699cc, 0xff6699cc, 0xff679acd, 0xff001b4e, 0x000e2e52, 0x001b56a1, 0x00123456, 0x005e80a2, 0xff6286a6]),
    (0xff123456, 0x00abcdef, 128, true, [0xff5e80a2, 0xffabcdef, 0x00123456, 0xff5e80a2, 0xffb3e6ff, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0x006699cc, 0xff6699cc, 0xff679acd, 0xff001b4e, 0xff0e2e52, 0xff1b56a1, 0xff123456, 0xff5e80a2, 0xff6286a6]),
    (0xff123456, 0x00abcdef, 1, false, [0x00123456, 0xffabcdef, 0x00123456, 0xff123456, 0xffbcffff, 0xff123456, 0x00123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0x00113355, 0x00123456, 0x00123456, 0x00123456, 0xff133557]),
    (0xff123456, 0x00abcdef, 1, true, [0xff123456, 0xffabcdef, 0x00123456, 0xff123456, 0xffbcffff, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0xff123456, 0xff123456, 0xff133557]),
    (0xff123456, 0x7f808080, 255, false, [0x7f808080, 0xff808080, 0x7f123456, 0xff808080, 0xff808080, 0xff48596a, 0x0048596a, 0xff48596a, 0xff123456, 0x7f899aab, 0xff899aab, 0xff92b4d6, 0x7f000000, 0x00091a2b, 0x002468ad, 0x7f123456, 0xff808080, 0xff8a9bac]),
    (0xff123456, 0x7f808080, 255, true, [0xff808080, 0xff808080, 0x7f123456, 0xff808080, 0xff808080, 0xff48596a, 0xff48596a, 0xff48596a, 0xff123456, 0xff899aab, 0xff899aab, 0xff92b4d6, 0xff000000, 0xff091a2b, 0xff2468ad, 0xff123456, 0xff808080, 0xff8a9bac]),
    (0xff123456, 0x7f808080, 200, false, [0x00676f76, 0xff808080, 0x7f123456, 0xff676f76, 0xff838b92, 0xff3c5166, 0x003c5166, 0xff3b5065, 0xff123456, 0x636e8398, 0xff6e8398, 0xff7698ba, 0xff000000, 0x000a1f34, 0x001d558d, 0x00123456, 0x00676f76, 0xff708599]),
    (0xff123456, 0x7f808080, 200, true, [0xff676f76, 0xff808080, 0x7f123456, 0xff676f76, 0xff838b92, 0xff3c5166, 0xff3c5166, 0xff3b5065, 0xff123456, 0x636e8398, 0xff6e8398, 0xff7698ba, 0xff000000, 0xff0a1f34, 0xff1d558d, 0xff123456, 0xff676f76, 0xff708599]),
    (0xff123456, 0x7f808080, 128, false, [0x00495a6b, 0xff808080, 0x7f123456, 0xff495a6b, 0xff8899aa, 0xff2d4660, 0x002d4660, 0xff2c465f, 0xff123456, 0x3f4d6780, 0xff4d6780, 0xff527496, 0xff000017, 0x000d2740, 0x00184573, 0x00123456, 0x00495a6b, 0xff4f6881]),
    (0xff123456, 0x7f808080, 128, true, [0xff495a6b, 0xff808080, 0x7f123456, 0xff495a6b, 0xff8899aa, 0xff2d4660, 0xff2d4660, 0xff2c465f, 0xff123456, 0x3f4d6780, 0xff4d6780, 0xff527496, 0xff000017, 0xff0d2740, 0xff184573, 0xff123456, 0xff495a6b, 0xff4f6881]),
    (0xff123456, 0x7f808080, 1, false, [0x00123456, 0xff808080, 0x7f123456, 0xff123456, 0xff91b3d5, 0xff123456, 0x00123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0x00113355, 0x00123456, 0x00123456, 0x00123456, 0xff133557]),
    (0xff123456, 0x7f808080, 1, true, [0xff123456, 0xff808080, 0x7f123456, 0xff123456, 0xff91b3d5, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0x00113355, 0xff113355, 0xff123456, 0xff123456, 0xff113355, 0xff123456, 0xff123456, 0xff123456, 0xff133557]),
    (0x7f808080, 0xffff0000, 255, false, [0xffff0000, 0x7fff0000, 0xff808080, 0xffff0000, 0xffff0000, 0xfffe0000, 0x00fe0000, 0xfffe0000, 0x7f808080, 0xffff0000, 0xffff0000, 0xffff8080, 0x7f800000, 0x007f0000, 0x00ff8080, 0x7f800000, 0xffff8080, 0xffff8181]),
    (0x7f808080, 0xffff0000, 255, true, [0x7fff0000, 0x7fff0000, 0xff808080, 0xffff0000, 0xffff0000, 0xfffe0000, 0x7ffe0000, 0xfffe0000, 0x7f808080, 0x7fff0000, 0xffff0000, 0x7fff8080, 0x7f800000, 0x7f7f0000, 0x7fff8080, 0x7f800000, 0x7fff8080, 0x7fff8181]),
    (0x7f808080, 0xffff0000, 200, false, [0x00e31c1c, 0x7fff0000, 0xff808080, 0xe4ef1010, 0xe4ff1b1b, 0xe3ee1010, 0x00e21c1c, 0xe4e21c1c, 0x7f808080, 0xc7e31c1c, 0xe4e31c1c, 0x7fff8080, 0x7f800000, 0x007f1c1c, 0x00ff8080, 0x00801c1c, 0x00e38080, 0xe3e48181]),
    (0x7f808080, 0xffff0000, 200, true, [0x7fe31c1c, 0x7fff0000, 0xff808080, 0xe4ef1010, 0xe4ff1b1b, 0xe3ee1010, 0x7fe21c1c, 0xe4e21c1c, 0x7f808080, 0xc7e31c1c, 0xe4e31c1c, 0x7fff8080, 0x7f800000, 0x7f7f1c1c, 0x7fff8080, 0x7f801c1c, 0x7fe38080, 0x7fe48181]),
    (0x7f808080, 0xffff0000, 128, false, [0x00bf4040, 0x7fff0000, 0xff808080, 0xc0d42b2b, 0xc0ff3f3f, 0xbfd32b2b, 0x00bf4040, 0xbfbe4040, 0x7f808080, 0x7fbf4040, 0xbfbf4040, 0x7fff8080, 0x7f800101, 0x007f4040, 0x00ff8080, 0x00804040, 0x00bf8080, 0xbfc08181]),
    (0x7f808080, 0xffff0000, 128, true, [0x7fbf4040, 0x7fff0000, 0xff808080, 0xc0d42b2b, 0xc0ff3f3f, 0xbfd32b2b, 0x7fbf4040, 0xbfbe4040, 0x7f808080, 0x7fbf4040, 0xbfbf4040, 0x7fff8080, 0x7f800101, 0x7f7f4040, 0x7fff8080, 0x7f804040, 0x7fbf8080, 0x7fc08181]),
    (0x7f808080, 0xffff0000, 1, false, [0x00807f7f, 0x7fff0000, 0xff808080, 0x80807f7f, 0x80ff7f7f, 0x7f808080, 0x00808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x007f7f7f, 0x00808080, 0x00807f7f, 0x00808080, 0x80818181]),
    (0x7f808080, 0xffff0000, 1, true, [0x7f807f7f, 0x7fff0000, 0xff808080, 0x80807f7f, 0x80ff7f7f, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x7f807f7f, 0x7f808080, 0x7f818181]),
    (0x7f808080, 0x8000ff00, 255, false, [0x8000ff00, 0x7f00ff00, 0x80808080, 0xff00ff00, 0xff00ff00, 0xc02bd42b, 0x0040bf40, 0xc03fbe3f, 0x7f808080, 0x803fff3f, 0xc03fff3f, 0xff80ff80, 0x00008000, 0x00007f00, 0x0080ff80, 0x7f008000, 0x8080ff80, 0xff81ff81]),
    (0x7f808080, 0x8000ff00, 255, true, [0x7f00ff00, 0x7f00ff00, 0x80808080, 0xff00ff00, 0xff00ff00, 0xc02bd42b, 0x7f40bf40, 0xc03fbe3f, 0x7f808080, 0x7f3fff3f, 0xc03fff3f, 0x7f80ff80, 0x7f008000, 0x7f007f00, 0x7f80ff80, 0x7f008000, 0x7f80ff80, 0x7f81ff81]),
    (0x7f808080, 0x8000ff00, 200, false, [0x001ce31c, 0x7f00ff00, 0x80808080, 0xe410ef10, 0xe41bff1b, 0xb238c638, 0x004eb14e, 0xb24db04d, 0x7f808080, 0x644dff4d, 0xb24dff4d, 0x7f80ff80, 0x7f008000, 0x001c7f1c, 0x0080ff80, 0x001c801c, 0x0080e380, 0xb281e481]),
    (0x7f808080, 0x8000ff00, 200, true, [0x7f1ce31c, 0x7f00ff00, 0x80808080, 0xe410ef10, 0xe41bff1b, 0xb238c638, 0x7f4eb14e, 0xb24db04d, 0x7f808080, 0x644dff4d, 0xb24dff4d, 0x7f80ff80, 0x7f008000, 0x7f1c7f1c, 0x7f80ff80, 0x7f1c801c, 0x7f80e380, 0x7f81e481]),
    (0x7f808080, 0x8000ff00, 128, false, [0x0040bf40, 0x7f00ff00, 0x80808080, 0xc02bd42b, 0xc03fff3f, 0xa04db24d, 0x00609f60, 0xa05f9e5f, 0x7f808080, 0x405fde5f, 0xa05fde5f, 0x7f80ff80, 0x7f018001, 0x00407f40, 0x0080ff80, 0x00408040, 0x0080bf80, 0xa081c081]),
    (0x7f808080, 0x8000ff00, 128, true, [0x7f40bf40, 0x7f00ff00, 0x80808080, 0xc02bd42b, 0xc03fff3f, 0xa04db24d, 0x7f609f60, 0xa05f9e5f, 0x7f808080, 0x405fde5f, 0xa05fde5f, 0x7f80ff80, 0x7f018001, 0x7f407f40, 0x7f80ff80, 0x7f408040, 0x7f80bf80, 0x7f81c081]),
    (0x7f808080, 0x8000ff00, 1, false, [0x007f807f, 0x7f00ff00, 0x80808080, 0x807f807f, 0x807fff7f, 0x7f808080, 0x00808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x007f7f7f, 0x00808080, 0x007f807f, 0x00808080, 0x80818181]),
    (0x7f808080, 0x8000ff00, 1, true, [0x7f7f807f, 0x7f00ff00, 0x80808080, 0x807f807f, 0x807fff7f, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x7f7f807f, 0x7f808080, 0x7f818181]),
    (0x7f808080, 0xffffffff, 255, false, [0xffffffff, 0x7fffffff, 0xff808080, 0xffffffff, 0xffffffff, 0xfffefefe, 0x00fefefe, 0xfffefefe, 0x7f808080, 0xffffffff, 0xffffffff, 0xffffffff, 0x7f808080, 0x007f7f7f, 0x00ffffff, 0x7f808080, 0xffffffff, 0xffffffff]),
    (0x7f808080, 0xffffffff, 255, true, [0x7fffffff, 0x7fffffff, 0xff808080, 0xffffffff, 0xffffffff, 0xfffefefe, 0x7ffefefe, 0xfffefefe, 0x7f808080, 0x7fffffff, 0xffffffff, 0x7fffffff, 0x7f808080, 0x7f7f7f7f, 0x7fffffff, 0x7f808080, 0x7fffffff, 0x7fffffff]),
    (0x7f808080, 0xffffffff, 200, false, [0x00e3e3e3, 0x7fffffff, 0xff808080, 0xe4efefef, 0xe4ffffff, 0xe3eeeeee, 0x00e2e2e2, 0xe4e2e2e2, 0x7f808080, 0xc7e3e3e3, 0xe4e3e3e3, 0x7fffffff, 0x7f808080, 0x007f7f7f, 0x00ffffff, 0x00808080, 0x00e3e3e3, 0xe3e4e4e4]),
    (0x7f808080, 0xffffffff, 200, true, [0x7fe3e3e3, 0x7fffffff, 0xff808080, 0xe4efefef, 0xe4ffffff, 0xe3eeeeee, 0x7fe2e2e2, 0xe4e2e2e2, 0x7f808080, 0xc7e3e3e3, 0xe4e3e3e3, 0x7fffffff, 0x7f808080, 0x7f7f7f7f, 0x7fffffff, 0x7f808080, 0x7fe3e3e3, 0x7fe4e4e4]),
    (0x7f808080, 0xffffffff, 128, false, [0x00bfbfbf, 0x7fffffff, 0xff808080, 0xc0d4d4d4, 0xc0ffffff, 0xbfd3d3d3, 0x00bfbfbf, 0xbfbebebe, 0x7f808080, 0x7fbfbfbf, 0xbfbfbfbf, 0x7fffffff, 0x7f808080, 0x007f7f7f, 0x00ffffff, 0x00808080, 0x00bfbfbf, 0xbfc0c0c0]),
    (0x7f808080, 0xffffffff, 128, true, [0x7fbfbfbf, 0x7fffffff, 0xff808080, 0xc0d4d4d4, 0xc0ffffff, 0xbfd3d3d3, 0x7fbfbfbf, 0xbfbebebe, 0x7f808080, 0x7fbfbfbf, 0xbfbfbfbf, 0x7fffffff, 0x7f808080, 0x7f7f7f7f, 0x7fffffff, 0x7f808080, 0x7fbfbfbf, 0x7fc0c0c0]),
    (0x7f808080, 0xffffffff, 1, false, [0x00808080, 0x7fffffff, 0xff808080, 0x80808080, 0x80ffffff, 0x7f808080, 0x00808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x007f7f7f, 0x00808080, 0x00808080, 0x00808080, 0x80818181]),
    (0x7f808080, 0xffffffff, 1, true, [0x7f808080, 0x7fffffff, 0xff808080, 0x80808080, 0x80ffffff, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f808080, 0x7f818181]),
    (0x7f808080, 0x40c0a080, 255, false, [0x40c0a080, 0x7fc0a080, 0x40808080, 0xffc0a080, 0xffc0a080, 0xa0998c80, 0x00908880, 0xa08f877f, 0x7f808080, 0x40ffffdf, 0xa0ffffdf, 0xbfffffff, 0x00412101, 0x00605040, 0x00ffffff, 0x40808080, 0x7fc0a080, 0xffe0d0c0]),
    (0x7f808080, 0x40c0a080, 255, true, [0x7fc0a080, 0x7fc0a080, 0x40808080, 0xffc0a080, 0xffc0a080, 0xa0998c80, 0x7f908880, 0xa08f877f, 0x7f808080, 0x7fffffdf, 0xa0ffffdf, 0x7fffffff, 0x7f412101, 0x7f605040, 0x7fffffff, 0x7f808080, 0x7fc0a080, 0x7fe0d0c0]),
    (0x7f808080, 0x40c0a080, 200, false, [0x00b29980, 0x7fc0a080, 0x40808080, 0xe4b89c80, 0xe4dbbb9b, 0x99948a80, 0x008c8680, 0x998b857f, 0x7f808080, 0x32fce3ca, 0x99fce3ca, 0x7ffffde4, 0x7f4f361d, 0x00675a4e, 0x00fffcd3, 0x00808080, 0x00b29980, 0x99cbbfb3]),
    (0x7f808080, 0x40c0a080, 200, true, [0x7fb29980, 0x7fc0a080, 0x40808080, 0xe4b89c80, 0xe4dbbb9b, 0x99948a80, 0x7f8c8680, 0x998b857f, 0x7f808080, 0x32fce3ca, 0x99fce3ca, 0x7ffffde4, 0x7f4f361d, 0x7f675a4e, 0x7ffffcd3, 0x7f808080, 0x7fb29980, 0x7fcbbfb3]),
    (0x7f808080, 0x40c0a080, 128, false, [0x00a09080, 0x7fc0a080, 0x40808080, 0xc0aa9580, 0xc0ffdfbf, 0x908e8780, 0x00888480, 0x9087837f, 0x7f808080, 0x20cfbfaf, 0x90cfbfaf, 0x7fe0d0c0, 0x7f615141, 0x00706860, 0x00cebbab, 0x00808080, 0x00a09080, 0x90b1a9a1]),
    (0x7f808080, 0x40c0a080, 128, true, [0x7fa09080, 0x7fc0a080, 0x40808080, 0xc0aa9580, 0xc0ffdfbf, 0x908e8780, 0x7f888480, 0x9087837f, 0x7f808080, 0x20cfbfaf, 0x90cfbfaf, 0x7fe0d0c0, 0x7f615141, 0x7f706860, 0x7fcebbab, 0x7f808080, 0x7fa09080, 0x7fb1a9a1]),
    (0x7f808080, 0x40c0a080, 1, false, [0x00808080, 0x7fc0a080, 0x40808080, 0x80808080, 0x80ffffff, 0x7f808080, 0x00808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x007f7f7f, 0x00808080, 0x00808080, 0x00808080, 0x80818181]),
    (0x7f808080, 0x40c0a080, 1, true, [0x7f808080, 0x7fc0a080, 0x40808080, 0x80808080, 0x80ffffff, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f808080, 0x7f818181]),
    (0x7f808080, 0x00abcdef, 255, false, [0x00abcdef, 0x7fabcdef, 0x00808080, 0xffabcdef, 0xffabcdef, 0x7f808080, 0x00808080, 0x7f7f7f7f, 0x7f808080, 0x00ffffff, 0x7fffffff, 0x7fffffff, 0x002c4e70, 0x00556677, 0x00ffffff, 0x00808080, 0x7fabcdef, 0xffd6e7f8]),
    (0x7f808080, 0x00abcdef, 255, true, [0x7fabcdef, 0x7fabcdef, 0x00808080, 0xffabcdef, 0xffabcdef, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x7fffffff, 0x7fffffff, 0x7fffffff, 0x7f2c4e70, 0x7f556677, 0x7fffffff, 0x7f808080, 0x7fabcdef, 0x7fd6e7f8]),
    (0x7f808080, 0x00abcdef, 200, false, [0x00a1bcd6, 0x7fabcdef, 0x00808080, 0xe4a5c3e1, 0xe4c6e8ff, 0x7f808080, 0x00808080, 0x7f7f7f7f, 0x7f808080, 0x00ffffff, 0x7fffffff, 0x7fffffff, 0x7f3f5974, 0x005f6c79, 0x00ffffff, 0x00808080, 0x00a1bcd6, 0x80c3d0dd]),
    (0x7f808080, 0x00abcdef, 200, true, [0x7fa1bcd6, 0x7fabcdef, 0x00808080, 0xe4a5c3e1, 0xe4c6e8ff, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x00ffffff, 0x7fffffff, 0x7fffffff, 0x7f3f5974, 0x7f5f6c79, 0x7fffffff, 0x7f808080, 0x7fa1bcd6, 0x7fc3d0dd]),
    (0x7f808080, 0x00abcdef, 128, false, [0x0095a6b7, 0x7fabcdef, 0x00808080, 0xc09cb3c9, 0xc0eaffff, 0x7f808080, 0x00808080, 0x7f7f7f7f, 0x7f808080, 0x00d4e5f6, 0x7fd4e5f6, 0x7fd5e6f7, 0x7f566778, 0x006a737b, 0x00c0d6f0, 0x00808080, 0x0095a6b7, 0x80abb4bc]),
    (0x7f808080, 0x00abcdef, 128, true, [0x7f95a6b7, 0x7fabcdef, 0x00808080, 0xc09cb3c9, 0xc0eaffff, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x00d4e5f6, 0x7fd4e5f6, 0x7fd5e6f7, 0x7f566778, 0x7f6a737b, 0x7fc0d6f0, 0x7f808080, 0x7f95a6b7, 0x7fabb4bc]),
    (0x7f808080, 0x00abcdef, 1, false, [0x00808080, 0x7fabcdef, 0x00808080, 0x80808080, 0x80ffffff, 0x7f808080, 0x00808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x007f7f7f, 0x00808080, 0x00808080, 0x00808080, 0x80818181]),
    (0x7f808080, 0x00abcdef, 1, true, [0x7f808080, 0x7fabcdef, 0x00808080, 0x80808080, 0x80ffffff, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f808080, 0x7f818181]),
    (0x7f808080, 0x7f808080, 255, false, [0x7f808080, 0x7f808080, 0x7f808080, 0xff808080, 0xff808080, 0xbf808080, 0x00808080, 0xbf7f7f7f, 0x7f808080, 0x7fc0c0c0, 0xbfc0c0c0, 0xfeffffff, 0x00010101, 0x00404040, 0x00ffffff, 0x7f808080, 0x7f808080, 0xffc0c0c0]),
    (0x7f808080, 0x7f808080, 255, true, [0x7f808080, 0x7f808080, 0x7f808080, 0xff808080, 0xff808080, 0xbf808080, 0x7f808080, 0xbf7f7f7f, 0x7f808080, 0x7fc0c0c0, 0xbfc0c0c0, 0x7fffffff, 0x7f010101, 0x7f404040, 0x7fffffff, 0x7f808080, 0x7f808080, 0x7fc0c0c0]),
    (0x7f808080, 0x7f808080, 200, false, [0x00808080, 0x7f808080, 0x7f808080, 0xe4808080, 0xe49b9b9b, 0xb1808080, 0x00808080, 0xb17f7f7f, 0x7f808080, 0x63b2b2b2, 0xb1b2b2b2, 0x7fe4e4e4, 0x7f1d1d1d, 0x004e4e4e, 0x00d3d3d3, 0x00808080, 0x00808080, 0xb1b3b3b3]),
    (0x7f808080, 0x7f808080, 200, true, [0x7f808080, 0x7f808080, 0x7f808080, 0xe4808080, 0xe49b9b9b, 0xb1808080, 0x7f808080, 0xb17f7f7f, 0x7f808080, 0x63b2b2b2, 0xb1b2b2b2, 0x7fe4e4e4, 0x7f1d1d1d, 0x7f4e4e4e, 0x7fd3d3d3, 0x7f808080, 0x7f808080, 0x7fb3b3b3]),
    (0x7f808080, 0x7f808080, 128, false, [0x00808080, 0x7f808080, 0x7f808080, 0xc0808080, 0xc0bfbfbf, 0x9f808080, 0x00808080, 0x9f7f7f7f, 0x7f808080, 0x3fa0a0a0, 0x9fa0a0a0, 0x7fc0c0c0, 0x7f414141, 0x00606060, 0x00ababab, 0x00808080, 0x00808080, 0x9fa1a1a1]),
    (0x7f808080, 0x7f808080, 128, true, [0x7f808080, 0x7f808080, 0x7f808080, 0xc0808080, 0xc0bfbfbf, 0x9f808080, 0x7f808080, 0x9f7f7f7f, 0x7f808080, 0x3fa0a0a0, 0x9fa0a0a0, 0x7fc0c0c0, 0x7f414141, 0x7f606060, 0x7fababab, 0x7f808080, 0x7f808080, 0x7fa1a1a1]),
    (0x7f808080, 0x7f808080, 1, false, [0x00808080, 0x7f808080, 0x7f808080, 0x80808080, 0x80ffffff, 0x7f808080, 0x00808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x007f7f7f, 0x00808080, 0x00808080, 0x00808080, 0x80818181]),
    (0x7f808080, 0x7f808080, 1, true, [0x7f808080, 0x7f808080, 0x7f808080, 0x80808080, 0x80ffffff, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x007f7f7f, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f7f7f7f, 0x7f808080, 0x7f808080, 0x7f808080, 0x7f818181]),
];

/// The blend methods the `Blt` switch can reach for a layer blit, in the
/// order the generated table lists them.
const VARIANTS: [Blt; 18] = [
    Blt::CopyMask,
    Blt::CopyColor,
    Blt::CopyAlpha,
    Blt::CopyOpaqueD,
    Blt::CopyOpaqueA,
    Blt::AlphaOnAlpha,
    Blt::Alpha,
    Blt::AlphaOnAddAlpha,
    Blt::AddAlphaOnAlpha,
    Blt::AddAlpha,
    Blt::AddAlphaOnAddAlpha,
    Blt::Blend(BlendOp::Add),
    Blt::Blend(BlendOp::Sub),
    Blt::Blend(BlendOp::Mul),
    Blt::Blend(BlendOp::Dodge),
    Blt::Blend(BlendOp::Darken),
    Blt::Blend(BlendOp::Lighten),
    Blt::Blend(BlendOp::Screen),
];

#[test]
fn blt_pixel_matches_the_reference_functors() {
    for &(d, s, opa, hda, expected) in TRUTH {
        for (blt, expected) in VARIANTS.iter().zip(expected) {
            assert_eq!(
                blt_pixel(d, s, *blt, opa, hda),
                expected,
                "d={d:#010x} s={s:#010x} opa={opa} hda={hda} blt={blt:?}"
            );
        }
    }
}

/// Checksums over the full 256x256 alpha cross-product for the functors whose
/// behaviour cannot be pinned by a handful of pixels: every (destination
/// alpha, source alpha) pair with one fixed colour each. The values come from
/// the same official functors (and the `TVPOpacityOnOpacityTable` /
/// `TVPNegativeMulTable` bodies from `visual/tvpgl.c`) as `TRUTH`.
#[test]
fn alpha_functors_match_the_reference_over_every_alpha() {
    let mut alpha_d: u64 = 0;
    let mut alpha_ao: u64 = 0;
    let mut addalpha: u64 = 0;
    let mut addalpha_o: u64 = 0;
    let mut aa: u64 = 0;
    let mut aa_o: u64 = 0;
    let mut fill_a: u64 = 0;
    let mut const_d: u64 = 0;
    for dopa in 0..256u32 {
        for sa in 0..256u32 {
            let d = (dopa << 24) | 0x0010_2030;
            let s = (sa << 24) | 0x0040_5060;
            let mut mix = |hash: &mut u64, value: u32| {
                *hash = hash.wrapping_mul(1_000_003).wrapping_add(u64::from(value));
            };
            mix(&mut alpha_d, alpha_blend_d(d, s, sa));
            mix(&mut addalpha, premulalpha_blend_n_a(d, s));
            mix(&mut aa, premulalpha_blend_a_a(d, s));
            mix(&mut const_d, const_alpha_blend_d(d, s, 17 + (sa & 200)));
            for opa in (1..256u32).step_by(97) {
                mix(&mut alpha_ao, alpha_blend_ao(d, s, opa));
                mix(&mut addalpha_o, premulalpha_blend_o(d, s, opa));
                mix(&mut aa_o, premulalpha_blend_a_a_o(d, s, opa));
                mix(&mut fill_a, const_alpha_fill_blend_a(d, 0x0055_aacc, opa));
            }
        }
    }
    assert_eq!(alpha_d, 0x9a46_ced9_aab9_e51d, "TVPAlphaBlend_d");
    assert_eq!(alpha_ao, 0x0c93_8e1e_cfb5_ad00, "TVPAlphaBlend_ao");
    assert_eq!(addalpha, 0xd6a0_a091_aef8_7000, "TVPAdditiveAlphaBlend");
    assert_eq!(addalpha_o, 0x2ed5_34b5_ac69_3e00, "TVPAdditiveAlphaBlend_o");
    assert_eq!(aa, 0x5a0d_3b5b_13f8_7000, "TVPAdditiveAlphaBlend_a");
    assert_eq!(aa_o, 0x66e7_ec54_9869_3e00, "TVPAdditiveAlphaBlend_ao");
    assert_eq!(fill_a, 0x154f_814a_e2d4_0000, "TVPConstColorAlphaBlend_a");
    assert_eq!(const_d, 0xa9d2_ca77_1491_d8c0, "TVPConstAlphaBlend_d");
}

/// `TVPConvertAlphaToAdditiveAlpha` / `TVPConvertAdditiveAlphaToAlpha`
/// (`blend_function.cpp:584-585`): the two directions of the
/// `dfAlpha` <-> `dfAddAlpha` pixel rewrite `tTJSNI_BaseLayer::convertType`
/// applies (`LayerIntf.cpp:1703-1727`). Rows are
/// `(input, alpha -> additive alpha, additive alpha -> alpha)`, produced by the
/// same generator as `TRUTH` (which also builds `TVPDivTable` exactly as
/// `visual/tvpgl.c:251-260` does).
const CONVERT_TRUTH: &[(u32, u32, u32)] = &[
    (0x00000000, 0x00000000, 0x00000000),
    (0xff000000, 0xff000000, 0xff000000),
    (0xffffffff, 0xfffefefe, 0xffffffff),
    (0x80ffffff, 0x807f7f7f, 0x80ffffff),
    (0x80808080, 0x80404040, 0x80ffffff),
    (0x01020304, 0x01000000, 0x01ffffff),
    (0xff123456, 0xff113355, 0xff123456),
    (0x7f7f7f7f, 0x7f3f3f3f, 0x7fffffff),
    (0x40c0a080, 0x40302820, 0x40ffffff),
    (0x0000ffff, 0x00000000, 0x00000000),
    (0xcc00ff00, 0xcc00cb00, 0xcc00ff00),
    (0x33669900, 0x33141e00, 0x33ffff00),
];

/// The reference resampler's per-axis tap window and normalized weights for a
/// grid of source/destination lengths, with and without a sub-rectangle source
/// offset. Rows are `(src_start, src_len, dst_len, dest_index, start,
/// &weights)`, produced by the same generator as `TRUTH`: it instantiates the
/// real `BilinearWeight` from `visual/gl/WeightFunctor.h` (with the `RANGE`
/// definitions of `WeightFunctor.cpp:22`) inside a verbatim transliteration of
/// `AxisParamCalculateAxis` (`gl/ResampleImage.cpp:283-368`) and
/// `AxisParamCalculateWeight` (`:232-280`). The generator works in the
/// reference's `float`s, so the comparison below allows 1e-6.
const RESAMPLE_AXIS_TRUTH: &[(i64, i64, i64, i64, i64, &[f64])] = &[
    (0, 1, 1, 0, 0, &[1.000000000]),
    (0, 1, 2, 0, 0, &[1.000000000]),
    (0, 1, 2, 1, 0, &[1.000000000]),
    (0, 1, 3, 0, 0, &[1.000000000]),
    (0, 1, 3, 1, 0, &[1.000000000]),
    (0, 1, 3, 2, 0, &[1.000000000]),
    (0, 1, 4, 0, 0, &[1.000000000]),
    (0, 1, 4, 1, 0, &[1.000000000]),
    (0, 1, 4, 2, 0, &[1.000000000]),
    (0, 1, 4, 3, 0, &[1.000000000]),
    (0, 1, 8, 0, 0, &[1.000000000]),
    (0, 1, 8, 1, 0, &[1.000000000]),
    (0, 1, 8, 2, 0, &[1.000000000]),
    (0, 1, 8, 3, 0, &[1.000000000]),
    (0, 1, 8, 4, 0, &[1.000000000]),
    (0, 1, 8, 5, 0, &[1.000000000]),
    (0, 1, 8, 6, 0, &[1.000000000]),
    (0, 1, 8, 7, 0, &[1.000000000]),
    (0, 2, 1, 0, 0, &[0.500000000, 0.500000000]),
    (0, 2, 2, 0, 0, &[1.000000000]),
    (0, 2, 2, 1, 0, &[0.000000000, 1.000000000]),
    (0, 2, 3, 0, 0, &[1.000000000]),
    (0, 2, 3, 1, 0, &[0.500000000, 0.500000000]),
    (0, 2, 3, 2, 0, &[0.000000000, 1.000000000]),
    (0, 2, 4, 0, 0, &[1.000000000]),
    (0, 2, 4, 1, 0, &[1.000000000]),
    (0, 2, 4, 2, 0, &[0.250000000, 0.750000000]),
    (0, 2, 4, 3, 0, &[0.000000000, 1.000000000]),
    (0, 2, 8, 0, 0, &[1.000000000]),
    (0, 2, 8, 1, 0, &[1.000000000]),
    (0, 2, 8, 2, 0, &[1.000000000]),
    (0, 2, 8, 3, 0, &[1.000000000]),
    (0, 2, 8, 4, 0, &[0.375000000, 0.625000000]),
    (0, 2, 8, 5, 0, &[0.125000000, 0.875000000]),
    (0, 2, 8, 6, 0, &[0.000000000, 1.000000000]),
    (0, 2, 8, 7, 0, &[0.000000000, 1.000000000]),
    (0, 3, 1, 0, 0, &[0.333333313, 0.333333343, 0.333333313]),
    (0, 3, 2, 0, 0, &[0.666666627, 0.333333343]),
    (0, 3, 2, 1, 0, &[0.000000000, 0.375000030, 0.625000000]),
    (0, 3, 3, 0, 0, &[1.000000000]),
    (0, 3, 3, 1, 0, &[0.000000000, 1.000000000]),
    (0, 3, 3, 2, 1, &[0.000000000, 1.000000000]),
    (0, 3, 4, 0, 0, &[1.000000000]),
    (0, 3, 4, 1, 0, &[0.375000000, 0.625000000]),
    (0, 3, 4, 2, 0, &[0.000000000, 1.000000000]),
    (0, 3, 4, 3, 1, &[0.000000000, 1.000000000]),
    (0, 3, 8, 0, 0, &[1.000000000]),
    (0, 3, 8, 1, 0, &[1.000000000]),
    (0, 3, 8, 2, 0, &[1.000000000]),
    (0, 3, 8, 3, 0, &[0.187500000, 0.812500000]),
    (0, 3, 8, 4, 0, &[0.000000000, 1.000000000]),
    (0, 3, 8, 5, 1, &[0.437500000, 0.562500000]),
    (0, 3, 8, 6, 1, &[0.062500000, 0.937500000]),
    (0, 3, 8, 7, 1, &[0.000000000, 1.000000000]),
    (0, 4, 1, 0, 0, &[0.281250000, 0.218750000, 0.218750000, 0.281250000]),
    (0, 4, 2, 0, 0, &[0.500000000, 0.375000000, 0.125000000]),
    (0, 4, 2, 1, 1, &[0.125000000, 0.375000000, 0.500000000]),
    (0, 4, 3, 0, 0, &[0.727272689, 0.272727281]),
    (0, 4, 3, 1, 0, &[0.000000000, 0.500000000, 0.500000000]),
    (0, 4, 3, 2, 1, &[0.000000000, 0.300000042, 0.699999928]),
    (0, 4, 4, 0, 0, &[1.000000000]),
    (0, 4, 4, 1, 0, &[0.000000000, 1.000000000]),
    (0, 4, 4, 2, 1, &[0.000000000, 1.000000000]),
    (0, 4, 4, 3, 2, &[0.000000000, 1.000000000]),
    (0, 4, 8, 0, 0, &[1.000000000]),
    (0, 4, 8, 1, 0, &[1.000000000]),
    (0, 4, 8, 2, 0, &[0.250000000, 0.750000000]),
    (0, 4, 8, 3, 0, &[0.000000000, 1.000000000]),
    (0, 4, 8, 4, 1, &[0.250000000, 0.750000000]),
    (0, 4, 8, 5, 1, &[0.000000000, 1.000000000]),
    (0, 4, 8, 6, 2, &[0.250000000, 0.750000000]),
    (0, 4, 8, 7, 2, &[0.000000000, 1.000000000]),
    (0, 8, 1, 0, 0, &[0.195312500, 0.085937500, 0.101562500, 0.117187500, 0.117187500, 0.101562500, 0.085937500, 0.195312500]),
    (0, 8, 2, 0, 0, &[0.281250000, 0.218750000, 0.218750000, 0.156250000, 0.093750000, 0.031250000]),
    (0, 8, 2, 1, 2, &[0.031250000, 0.093750000, 0.156250000, 0.218750000, 0.218750000, 0.281250000]),
    (0, 8, 3, 0, 0, &[0.372093022, 0.348837197, 0.209302321, 0.069767468]),
    (0, 8, 3, 1, 1, &[0.024390243, 0.170731708, 0.317073166, 0.317073166, 0.170731708]),
    (0, 8, 3, 2, 3, &[0.000000000, 0.069767468, 0.209302351, 0.348837227, 0.372092992]),
    (0, 8, 4, 0, 0, &[0.500000000, 0.375000000, 0.125000000]),
    (0, 8, 4, 1, 1, &[0.125000000, 0.375000000, 0.375000000, 0.125000000]),
    (0, 8, 4, 2, 3, &[0.125000000, 0.375000000, 0.375000000, 0.125000000]),
    (0, 8, 4, 3, 5, &[0.125000000, 0.375000000, 0.500000000]),
    (0, 8, 8, 0, 0, &[1.000000000]),
    (0, 8, 8, 1, 0, &[0.000000000, 1.000000000]),
    (0, 8, 8, 2, 1, &[0.000000000, 1.000000000]),
    (0, 8, 8, 3, 2, &[0.000000000, 1.000000000]),
    (0, 8, 8, 4, 3, &[0.000000000, 1.000000000]),
    (0, 8, 8, 5, 4, &[0.000000000, 1.000000000]),
    (0, 8, 8, 6, 5, &[0.000000000, 1.000000000]),
    (0, 8, 8, 7, 6, &[0.000000000, 1.000000000]),
    (3, 4, 3, 0, 3, &[0.727272630, 0.272727311]),
    (5, 2, 7, 0, 5, &[1.000000000]),
    (3, 4, 3, 1, 3, &[0.000000000, 0.500000000, 0.500000000]),
    (5, 2, 7, 1, 5, &[1.000000000]),
    (3, 4, 3, 2, 4, &[0.000000000, 0.300000191, 0.699999809]),
    (5, 2, 7, 2, 5, &[1.000000000]),
    (3, 4, 3, 3, 6, &[1.000000000]),
    (5, 2, 7, 3, 5, &[0.500000000, 0.500000000]),
];

#[test]
fn resample_axis_windows_match_the_reference() {
    for &(source_start, source_len, dest_len, dest_index, start, weights) in RESAMPLE_AXIS_TRUTH {
        let (actual_start, actual_weights) = resample_axis_weights(
            source_start,
            source_start + source_len,
            source_len,
            dest_len,
            dest_index,
            StretchType::Bilinear,
        )
        .expect("weights");
        assert_eq!(
            actual_start, start,
            "start for src_start={source_start} src={source_len} dst={dest_len} dest={dest_index}"
        );
        assert_eq!(
            actual_weights.len(),
            weights.len(),
            "tap count for src_start={source_start} src={source_len} dst={dest_len} dest={dest_index}"
        );
        for (index, (actual, expected)) in actual_weights.iter().zip(weights).enumerate() {
            assert!(
                (actual - expected).abs() < 1e-6,
                "tap {index} for src_start={source_start} src={source_len} dst={dest_len} \
                 dest={dest_index}: {actual} != {expected}"
            );
        }
    }
}

#[test]
fn convert_functors_match_the_reference() {
    for &(input, to_additive, to_alpha) in CONVERT_TRUTH {
        assert_eq!(
            alpha_pixel_to_additive_alpha(input),
            to_additive,
            "alpha -> additive alpha of {input:#010x}"
        );
        assert_eq!(
            alpha_pixel_to_alpha(input),
            to_alpha,
            "additive alpha -> alpha of {input:#010x}"
        );
    }
    // The `TVPDivTable`-driven direction over every alpha with one colour.
    let mut to_additive: u64 = 0;
    let mut to_alpha: u64 = 0;
    for alpha in 0..256u32 {
        let px = (alpha << 24) | 0x0010_2030;
        to_additive = to_additive
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(alpha_pixel_to_additive_alpha(px)));
        to_alpha = to_alpha
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(alpha_pixel_to_alpha(px)));
    }
    assert_eq!(to_additive, 0x3779_017b_c9b0_dfd0, "TVPConvertAlphaToAdditiveAlpha");
    assert_eq!(to_alpha, 0xc798_3edc_6480_0432, "TVPConvertAdditiveAlphaToAlpha");
}
}
