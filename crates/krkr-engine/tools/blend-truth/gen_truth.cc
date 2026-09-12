// Truth-table generator for the blend model in
// `crates/krkr-engine/src/native/blend.rs`.
//
// It includes the *unmodified* krkrz blend functor headers and rebuilds the
// tables `visual/tvpgl.c` builds, then prints the `TRUTH` array the Rust test
// `blt_pixel_matches_the_reference_functors` pins (288 rows x 18 methods).
//
// Build (the header is Shift-JIS, clang reads it as bytes):
//
//     clang++ -O0 -I <krkrz>/src/core/visual/gl -o gen_truth gen_truth.cc
//     ./gen_truth > truth.rs
//
// and paste the printed array over the `const TRUTH` block in `blend.rs`
// (keeping its doc comment). The Rust implementation must then still pass
// `cargo test -p krkr-engine --lib blend::tests`.
//
// Alignment target: the *shipped* x86 build, i.e. the SSE2 kernels that
// `TVPAfterSystemInit` (`base/win32/SysInitImpl.cpp:1305`) installs through
// `TVPGL_SSE2_Init` (`blend_function_sse2.cpp:1307`); the AVX2 init overrides
// only the `Ps*ScreenBlend` family (`blend_function_avx2.cpp:296-300`). The
// pure-C functors are used where no SIMD build exists, and for `screen` with
// `0 < opa < 255` the C body (`blend_functor_c.h:301`) disagrees with every
// shipped kernel, so the generator computes that case with a byte-level
// transliteration of `sse2_screen_blend_o_functor` (`blend_functor_sse2.h:1246`)
// and `sse2_screen_blend_hda_o_functor` (`:1288`) instead.

#include <cstdio>
#include <cstdint>
#include <cmath>
#include <cstdlib>
#include <vector>
typedef uint32_t tjs_uint32; typedef int32_t tjs_int32; typedef int tjs_int;
typedef uint8_t tjs_uint8; typedef int8_t tjs_int8; typedef uint16_t tjs_uint16; typedef int16_t tjs_int16;
typedef uint64_t tjs_uint64; typedef int64_t tjs_int64; typedef double tjs_real;
extern "C" {
tjs_uint32 TVPRecipTable256[256];
unsigned char TVPOpacityOnOpacityTable[256*256];
unsigned char TVPNegativeMulTable[256*256];
unsigned char TVPOpacityOnOpacityTable65[65*256];
unsigned char TVPNegativeMulTable65[65*256];
unsigned char TVPDivTable[256*256];
unsigned char TVPDitherTable_5_6[8][4][2][256];
unsigned char TVPDitherTable_676[3][4][4][256];
unsigned char TVP252DitherPalette[3][256];
}
#include "blend_functor_c.h"
// `WeightFunctor.cpp:16-18` includes `<float.h>` before the header.
#include <float.h>
#include "WeightFunctor.h"

// `WeightFunctor.cpp:22-27` defines these out of line.
const float BilinearWeight::RANGE = 1.0f;
const float BicubicWeight::RANGE = 2.0f;
const float Spline16Weight::RANGE = 2.0f;
const float Spline36Weight::RANGE = 3.0f;
const float GaussianWeight::RANGE = 2.0f;
const float BlackmanSincWeight::RANGE = 4.0f;

// The resampler's per-axis tap window, transliterated from
// `AxisParamCalculateAxis` (`visual/gl/ResampleImage.cpp:283-368`) followed by
// the edge fold and normalization of `AxisParamCalculateWeight` (`:232-280`).
// The weight functions themselves come from the unmodified `WeightFunctor.h`
// above. `sample` is the source pixel-centre coordinate the caller computes the
// same way the reference does (`cx = (dest + 0.5) * src_len / dst_len +
// src_start`, `:302`), i.e. `sample = cx - 0.5`.
template <class TWeightFunc>
static void axis_truth(FILE* out, int src_start, int src_len, int dst_len,
                       int dest_index, const TWeightFunc& func) {
    float cx = (dest_index + 0.5f) * src_len / dst_len + src_start;
    float sample = cx - 0.5f;
    float tap = TWeightFunc::RANGE;
    float range = src_len <= dst_len ? tap : tap * src_len / dst_len;
    float delta = src_len <= dst_len ? 1.0f : (float)dst_len / src_len;
    int left = (int)std::floor(cx - range);
    int right = (int)std::floor(cx + range);
    int count = right - left;
    std::vector<float> weight(count, 0.0f);
    for (int k = 0; k < count; k++) {
        weight[k] = func(std::abs((left + k + 0.5f - cx) * delta));
    }
    int leftedge = src_start - left;
    if (leftedge < 0) leftedge = 0;
    if (leftedge > count) leftedge = count;
    int start = left + leftedge;
    std::vector<float> folded(weight.begin() + leftedge, weight.end());
    if (leftedge > 0 && !folded.empty()) {
        for (int i = 0; i < leftedge; i++) folded[0] += weight[i];
    }
    int rightedge = right - (src_start + src_len);
    if (rightedge < 0) rightedge = 0;
    if (rightedge > (int)folded.size()) rightedge = (int)folded.size();
    if (rightedge > 0) {
        int keep = (int)folded.size() - rightedge;
        float sum = 0.0f;
        for (int i = keep; i < (int)folded.size(); i++) sum += folded[i];
        folded.resize(keep);
        if (!folded.empty()) folded[keep - 1] += sum;
    }
    float sum = 0.0f;
    for (size_t i = 0; i < folded.size(); i++) sum += folded[i];
    printf("    (%d, %d, %d, %d, %d, &[", src_start, src_len, dst_len, dest_index, start);
    for (size_t i = 0; i < folded.size(); i++) {
        printf("%s%.9f", i ? ", " : "", folded[i] / sum);
    }
    printf("]),\n");
    (void)sample;
}

static tjs_uint32 bmCopy(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    if (opa == 255) return hda ? color_copy_functor()(d, s) : s;
    return hda ? const_alpha_blend_hda_functor(opa)(d, s) : const_alpha_blend_functor(opa)(d, s);
}
static tjs_uint32 bmCopyOnAlpha(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    (void)hda;
    return opa == 255 ? color_opaque_functor()(d, s) : const_alpha_blend_d_functor(opa)(d, s);
}
static tjs_uint32 bmCopyOnAddAlpha(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    (void)hda;
    return opa == 255 ? color_opaque_functor()(d, s) : const_alpha_blend_a_functor(opa)(d, s);
}
static tjs_uint32 bmAlphaOnAlpha(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    (void)hda;
    return opa == 255 ? alpha_blend_d_functor()(d, s) : alpha_blend_do_functor(opa)(d, s);
}
static tjs_uint32 bmAlpha(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    if (opa == 255) return hda ? alpha_blend_HDA_functor()(d, s) : alpha_blend_functor()(d, s);
    return hda ? alpha_blend_HDA_o_functor(opa)(d, s) : alpha_blend_o_functor(opa)(d, s);
}
static tjs_uint32 bmAlphaOnAddAlpha(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    (void)hda;
    return opa == 255 ? alpha_blend_a_functor()(d, s) : alpha_blend_ao_functor(opa)(d, s);
}
static tjs_uint32 bmAddAlpha(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    if (opa == 255) return hda ? premulalpha_blend_HDA_functor()(d, s) : premulalpha_blend_functor()(d, s);
    return hda ? premulalpha_blend_HDA_o_functor(opa)(d, s) : premulalpha_blend_o_functor(opa)(d, s);
}
static tjs_uint32 bmAddAlphaOnAddAlpha(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    (void)hda;
    return opa == 255 ? premulalpha_blend_a_functor()(d, s) : premulalpha_blend_ao_functor(opa)(d, s);
}
template <class RAW, class O>
static tjs_uint32 min_var(RAW raw, O o, tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    if (opa == 255) {
        tjs_uint32 out = raw(d, s);
        return hda ? ((out & 0x00ffffff) | (d & 0xff000000)) : out;
    }
    tjs_uint32 out = o(d, s, opa);
    return hda ? ((out & 0x00ffffff) | (d & 0xff000000)) : out;
}

// `sse2_screen_blend_o_functor`: per 16-bit lane the kernel computes
// `md = ~d`, `ms = ~(s * opa) >> 8`, `md = (md * ms) >> 8`, `md = ~md` over all
// four bytes (alpha included).
static tjs_uint32 sse2_screen_o(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa) {
    tjs_uint32 out = 0;
    for (unsigned shift = 0; shift < 32; shift += 8) {
        tjs_uint32 md = (~(d >> shift)) & 0xffu;
        tjs_uint32 lane = ((s >> shift) & 0xffu) * opa;   // fits 16 bits
        tjs_uint32 ms = ((0xffffu - lane) >> 8) & 0xffu;
        tjs_uint32 product = md * ms;                     // fits 16 bits
        out |= ((~(product >> 8)) & 0xffu) << shift;
    }
    return out;
}
// `sse2_screen_blend_hda_o_functor`: the same lanes, destination alpha restored.
static tjs_uint32 sse2_screen_hda_o(tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa) {
    return (d & 0xff000000u) | (sse2_screen_o(d, s, opa) & 0x00ffffffu);
}
template <class O>
static tjs_uint32 bmScreen(O sse2, tjs_uint32 d, tjs_uint32 s, tjs_uint32 opa, bool hda) {
    if (opa == 255) {
        tjs_uint32 out = screen_blend_functor()(d, s);
        return hda ? ((out & 0x00ffffff) | (d & 0xff000000)) : out;
    }
    return hda ? sse2_screen_hda_o(d, s, opa) : sse2(d, s, opa);
}

int main() {
    // tables, exactly as `visual/tvpgl.c:172-215` builds them
    TVPRecipTable256[0] = 65536;
    for (int i = 1; i < 256; i++) TVPRecipTable256[i] = 65536 / i;
    // `visual/tvpgl.c:251-260`
    for (int b = 0; b < 256; b++) {
        TVPDivTable[(0 << 8) + b] = 0;
        for (int a = 1; a < 256; a++) {
            int tmp = (int)(b * 255 / a);
            if (tmp > 255) tmp = 255;
            TVPDivTable[(a << 8) + b] = (tjs_uint8)tmp;
        }
    }
    for (int a = 0; a < 256; a++) {
        for (int b = 0; b < 256; b++) {
            int addr = b * 256 + a;
            float c; int ci;
            if (a) {
                float at = (float)(a / 255.0), bt = (float)(b / 255.0);
                c = bt / at;
                c /= (float)(1.0 - bt + c);
                ci = (int)(c * 255);
                if (ci >= 256) ci = 255;
            } else ci = 255;
            TVPOpacityOnOpacityTable[addr] = (unsigned char)ci;
            TVPNegativeMulTable[addr] = (unsigned char)(255 - (255 - a) * (255 - b) / 255);
        }
    }

    static const tjs_uint32 ds[] = {0xff0000ffu, 0x00000000u, 0x00ffffffu, 0x80402010u, 0xff123456u, 0x7f808080u};
    static const tjs_uint32 ss[] = {0xffff0000u, 0x8000ff00u, 0xffffffffu, 0x40c0a080u, 0x00abcdefu, 0x7f808080u};
    static const tjs_uint32 ops[] = {255, 200, 128, 1};
    printf("const TRUTH: &[(u32, u32, u32, bool, [u32; 18])] = &[\n");
    for (unsigned i = 0; i < 6; i++) {
        for (unsigned j = 0; j < 6; j++) {
            tjs_uint32 d = ds[i], s = ss[j];
            for (unsigned k = 0; k < 4; k++) {
                tjs_uint32 opa = ops[k];
                for (int h = 0; h < 2; h++) {
                    bool hda = h != 0;
                    tjs_uint32 v[18];
                    v[0] = bmCopy(d, s, opa, hda);
                    v[1] = color_copy_functor()(d, s);
                    v[2] = alpha_copy_functor()(d, s);
                    v[3] = bmCopyOnAlpha(d, s, opa, hda);
                    v[4] = bmCopyOnAddAlpha(d, s, opa, hda);
                    v[5] = bmAlphaOnAlpha(d, s, opa, hda);
                    v[6] = bmAlpha(d, s, opa, hda);
                    v[7] = bmAlphaOnAddAlpha(d, s, opa, hda);
                    v[8] = d;  // bmAddAlphaOnAlpha
                    v[9] = bmAddAlpha(d, s, opa, hda);
                    v[10] = bmAddAlphaOnAddAlpha(d, s, opa, hda);
                    v[11] = min_var([](tjs_uint32 x, tjs_uint32 y) { return add_blend_functor()(x, y); },
                                    [](tjs_uint32 x, tjs_uint32 y, tjs_uint32 a) { return add_blend_func()(x, y, a); }, d, s, opa, hda);
                    v[12] = min_var([](tjs_uint32 x, tjs_uint32 y) { return sub_blend_functor()(x, y); },
                                    [](tjs_uint32 x, tjs_uint32 y, tjs_uint32 a) { return sub_blend_func()(x, y, a); }, d, s, opa, hda);
                    v[13] = min_var([](tjs_uint32 x, tjs_uint32 y) { return mul_blend_functor()(x, y); },
                                    [](tjs_uint32 x, tjs_uint32 y, tjs_uint32 a) { return mul_blend_func()(x, y, a); }, d, s, opa, hda);
                    v[14] = min_var([](tjs_uint32 x, tjs_uint32 y) { return color_dodge_blend_functor()(x, y); },
                                    [](tjs_uint32 x, tjs_uint32 y, tjs_uint32 a) { return color_dodge_blend_func()(x, y, a); }, d, s, opa, hda);
                    v[15] = min_var([](tjs_uint32 x, tjs_uint32 y) { return darken_blend_functor()(x, y); },
                                    [](tjs_uint32 x, tjs_uint32 y, tjs_uint32 a) { return darken_blend_func()(x, y, a); }, d, s, opa, hda);
                    v[16] = min_var([](tjs_uint32 x, tjs_uint32 y) { return lighten_blend_functor()(x, y); },
                                    [](tjs_uint32 x, tjs_uint32 y, tjs_uint32 a) { return lighten_blend_func()(x, y, a); }, d, s, opa, hda);
                    v[17] = bmScreen(sse2_screen_o, d, s, opa, hda);
                    printf("    (0x%08x, 0x%08x, %u, %s, [", d, s, opa, hda ? "true" : "false");
                    for (int n = 0; n < 18; n++) printf("%s0x%08x", n ? ", " : "", v[n]);
                    printf("]),\n");
                }
            }
        }
    }
    printf("];\n");

    // The resampler's axis windows
    // (`resample_axis_weights` in `blend.rs`): every kernel tap window for a
    // grid of source/destination lengths, with and without a sub-rectangle
    // source offset. Rows are `(src_start, src_len, dst_len, dest_index,
    // start, &weights)`; the Rust side recomputes the sample coordinate as
    // `(dest_index + 0.5) * src_len / dst_len - 0.5`.
    printf("const RESAMPLE_AXIS_TRUTH: &[(i64, i64, i64, i64, i64, &[f64])] = &[\n");
    {
        BilinearWeight bilinear;
        static const int lens[] = {1, 2, 3, 4, 8};
        for (unsigned s = 0; s < sizeof(lens) / sizeof(lens[0]); s++) {
            for (unsigned d = 0; d < sizeof(lens) / sizeof(lens[0]); d++) {
                int src_len = lens[s], dst_len = lens[d];
                for (int dest = 0; dest < dst_len; dest++) {
                    axis_truth(stdout, 0, src_len, dst_len, dest, bilinear);
                }
            }
        }
        // A sub-rectangle source (the fold happens at the rectangle's edges).
        for (int dest = 0; dest < 4; dest++) {
            axis_truth(stdout, 3, 4, 3, dest, bilinear);
            axis_truth(stdout, 5, 2, 7, dest, bilinear);
        }
    }
    printf("];\n");

    // `convertType` truth: the two directions of the `dfAlpha` <->
    // `dfAddAlpha` pixel rewrite, straight from the functors
    // `TVPConvertAlphaToAdditiveAlpha` / `TVPConvertAdditiveAlphaToAlpha`
    // install (`blend_function.cpp:584-585`, functors at
    // `blend_functor_c.h:861`/`:871`). Rows are
    // `(input, alpha_to_additive, additive_to_alpha)`; the checksum at the end
    // sweeps every alpha with one fixed colour.
    static const tjs_uint32 conv_px[] = {
        0x00000000u, 0xff000000u, 0xffffffffu, 0x80ffffffu, 0x80808080u,
        0x01020304u, 0xff123456u, 0x7f7f7f7fu, 0x40c0a080u, 0x0000ffffu,
        0xcc00ff00u, 0x33669900u,
    };
    printf("const CONVERT_TRUTH: &[(u32, u32, u32)] = &[\n");
    for (unsigned i = 0; i < sizeof(conv_px) / sizeof(conv_px[0]); i++) {
        tjs_uint32 px = conv_px[i];
        printf("    (0x%08x, 0x%08x, 0x%08x),\n", px,
               convet_alpha_to_premulalpha_functor()(px),
               convet_premulalpha_to_alpha_functor()(px));
    }
    printf("];\n");

    unsigned long long to_additive = 0, to_alpha = 0;
    for (tjs_uint32 a = 0; a < 256; a++) {
        tjs_uint32 px = (a << 24) | 0x00102030u;
        to_additive = to_additive * 1000003ull + convet_alpha_to_premulalpha_functor()(px);
        to_alpha = to_alpha * 1000003ull + convet_premulalpha_to_alpha_functor()(px);
    }
    printf("// checksums for convert_type_matches_the_reference_functors:\n");
    printf("// to_additive=0x%016llx\n", to_additive);
    printf("// to_alpha=0x%016llx\n", to_alpha);

    // The eight checksums pinned by
    // `alpha_functors_match_the_reference_over_every_alpha` (every
    // destination-alpha/source-alpha pair with one fixed colour each).
    unsigned long long alpha_d = 0, alpha_ao = 0, addalpha = 0, addalpha_o = 0;
    unsigned long long aa = 0, aa_o = 0, fill_a = 0, const_d = 0;
    for (tjs_uint32 dopa = 0; dopa < 256; dopa++) {
        for (tjs_uint32 sa = 0; sa < 256; sa++) {
            tjs_uint32 d = (dopa << 24) | 0x00102030u;
            tjs_uint32 s = (sa << 24) | 0x00405060u;
            alpha_d = alpha_d * 1000003ull + alpha_blend_d_functor()(d, s);
            addalpha = addalpha * 1000003ull + premulalpha_blend_functor()(d, s);
            aa = aa * 1000003ull + premulalpha_blend_a_functor()(d, s);
            const_d = const_d * 1000003ull
                + const_alpha_blend_d_functor((tjs_int)(17 + (sa & 200)))(d, s);
            for (tjs_uint32 opa = 1; opa < 256; opa += 97) {
                alpha_ao = alpha_ao * 1000003ull + alpha_blend_ao_functor(opa)(d, s);
                addalpha_o = addalpha_o * 1000003ull + premulalpha_blend_o_functor(opa)(d, s);
                aa_o = aa_o * 1000003ull + premulalpha_blend_ao_functor(opa)(d, s);
                fill_a = fill_a * 1000003ull
                    + const_alpha_fill_blend_a_functor(opa, 0x0055aacc)(d);
            }
        }
    }
    printf("// checksums for alpha_functors_match_the_reference_over_every_alpha:\n");
    printf("// alpha_d=0x%016llx\n", alpha_d);
    printf("// alpha_ao=0x%016llx\n", alpha_ao);
    printf("// addalpha=0x%016llx\n", addalpha);
    printf("// addalpha_o=0x%016llx\n", addalpha_o);
    printf("// aa=0x%016llx\n", aa);
    printf("// aa_o=0x%016llx\n", aa_o);
    printf("// fill_a=0x%016llx\n", fill_a);
    printf("// const_d=0x%016llx\n", const_d);
    return 0;
}
