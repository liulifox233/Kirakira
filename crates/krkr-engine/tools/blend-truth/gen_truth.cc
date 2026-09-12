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
