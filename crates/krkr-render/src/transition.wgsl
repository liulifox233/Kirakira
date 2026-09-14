// Transition kernels.
//
// Each function below is one transition method's per-pixel kernel.  The options
// they read and the fidelity of each against the reference source are recorded
// per variant on krkr-core's `TransitionMethod`; the verdicts are:
//
//   crossfade      faithful     `TVPConstAlphaBlend_SD` over the scene beneath
//                               (the composite plan's own test proves it)
//   wave           faithful     ported element by element from `extrans/wave.cpp`
//                               in M36; the deviations it records are listed at
//                               the kernel
//   universal      approximate  the rule graphic is read as a scroll texture and
//                               the phase advances with `progress` instead of the
//                               reference's tick clock
//   scroll         approximate  direction from the numeric `from` code only, band
//                               widths linear in `progress`
//   mosaic         approximate  sine block ramp against the reference's integer
//                               triangle, grid anchored at the destination
//                               bitmap's origin instead of re-centred on the
//                               image
//   turn           approximate  no fold table, no specular gloss, pseudo-random
//                               tile order instead of the diagonal phase sweep
//   rotatezoom     approximate  twist runs the other way, fixed pivot, in-quad
//                               crossfade where the reference copies pixels
//   rotatevanish   approximate  closest of the six; the same three deviations
//   rotateswap     approximate  crossfade instead of region layering, no lateral
//                               slide, no vertical squash, linear scale ramps
//   ripple         approximate  travelling Gaussian band instead of the cached
//                               standing wave; `rwidth`/`speed` reinterpreted
//
// No kernel degrades to another method: a name that resolves to a method always
// runs that method's kernel.

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) tint: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
};

struct TransitionUniforms {
    data: array<vec4<f32>, 12>,
};

@group(0) @binding(0)
var old_image: texture_2d<f32>;

@group(0) @binding(1)
var old_sampler: sampler;

@group(0) @binding(2)
var new_image: texture_2d<f32>;

@group(0) @binding(3)
var new_sampler: sampler;

@group(0) @binding(4)
var<uniform> uniforms: TransitionUniforms;

@group(0) @binding(5)
var rule_image: texture_2d<f32>;

@group(0) @binding(6)
var rule_sampler: sampler;

@group(0) @binding(7)
var under_image: texture_2d<f32>;

@group(0) @binding(8)
var under_sampler: sampler;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>(input.position, 0.0, 1.0);
    output.tex_coord = input.tex_coord;
    return output;
}

fn progress() -> f32 {
    return clamp(uniforms.data[0].x, 0.0, 1.0);
}

fn viewport_size() -> vec2<f32> {
    return max(uniforms.data[1].xy, vec2<f32>(1.0, 1.0));
}

// Screen transitions run over the whole frame surface; these accessors expose
// the extra state the extrans kernels need on top of `data[0..8]`.
//
//   data[8]  = the destination layer's rectangle in logical frame pixels
//              (`tTVPDivisibleData::Dest`, `LayerIntf.cpp:6513-6540`); zero
//              width/height means the destination has no measurable geometry.
//   data[9]  = logical -> physical transform of the frame (scale, offset x,
//              offset y) and whether the under pass (binding 7) was rendered.
//   data[10] = the transition's duration in milliseconds (`Time`), 0 when the
//              caller did not supply the clock.
fn image_rect() -> vec4<f32> {
    return uniforms.data[8];
}

fn transform_scale() -> f32 {
    return max(uniforms.data[9].x, 1.0e-6);
}

fn transform_offset() -> vec2<f32> {
    return uniforms.data[9].yz;
}

fn under_available() -> bool {
    return uniforms.data[9].w >= 0.5;
}

fn duration_millis() -> f32 {
    return max(uniforms.data[10].x, 0.0);
}

// Every kernel's geometry is measured in the destination layer's own bitmap
// pixels, which is where the reference's handlers work: `tTVPDivisibleData::
// Left`/`Top` are offsets inside that bitmap (`LayerIntf.cpp:6575-6576`), and
// the rule's sampling and repeat, the rotate pivots, the mosaic blocks, the
// turn tiles and the ripple front are all taken in those pixels.  The frame's
// physical viewport -- the window's size and its DPI scale -- must never enter
// it: expressing this geometry in window pixels repeated a rule transition
// once per (window / rule) axis, four quarter-screen copies at a 2x window.
//
// `image_local` maps a frame uv to destination-bitmap pixels and `frame_uv` is
// its inverse; both read the same transform the wave kernel already converts
// with (`(uv * frame - origin) / scale`).
fn image_local(uv: vec2<f32>) -> vec2<f32> {
    let image = image_rect();
    return (uv * viewport_size() - transform_offset()) / transform_scale() - image.xy;
}

fn frame_uv(local: vec2<f32>) -> vec2<f32> {
    let image = image_rect();
    return (transform_offset() + (image.xy + local) * transform_scale()) / viewport_size();
}

fn in_bounds(uv: vec2<f32>) -> bool {
    return uv.x >= 0.0 && uv.y >= 0.0 && uv.x <= 1.0 && uv.y <= 1.0;
}

fn sample_old(uv: vec2<f32>, bg: vec4<f32>) -> vec4<f32> {
    if (!in_bounds(uv)) {
        return bg;
    }
    return textureSample(old_image, old_sampler, uv);
}

fn sample_new(uv: vec2<f32>, bg: vec4<f32>) -> vec4<f32> {
    if (!in_bounds(uv)) {
        return bg;
    }
    return textureSample(new_image, new_sampler, uv);
}

fn acceleration(t: f32, accel: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0);
    if (accel >= 0.01) {
        return pow(x, accel);
    }
    if (accel <= -0.01) {
        return 1.0 - pow(1.0 - x, -accel);
    }
    return x;
}

fn scroll_direction(origin: f32) -> vec2<f32> {
    if (origin >= 2.5) {
        return vec2<f32>(0.0, 1.0);
    }
    if (origin >= 1.5) {
        return vec2<f32>(1.0, 0.0);
    }
    if (origin >= 0.5) {
        return vec2<f32>(0.0, -1.0);
    }
    return vec2<f32>(-1.0, 0.0);
}

fn transition_crossfade(uv: vec2<f32>) -> vec4<f32> {
    return mix(textureSample(old_image, old_sampler, uv), textureSample(new_image, new_sampler, uv), progress());
}

fn transition_universal(uv: vec2<f32>) -> vec4<f32> {
    let old_color = textureSample(old_image, old_sampler, uv);
    let new_color = textureSample(new_image, new_sampler, uv);
    var rule_value = uv.x;
    if (uniforms.data[0].z > 0.5) {
        // The rule is read at the destination bitmap's own coordinates --
        // `data.Left`/`data.Top`, the offsets the reference samples the rule
        // scan line at (`TransIntf.cpp:825-851` with `LayerIntf.cpp:6575-6576`)
        // -- and it repeats only where the reference's loader repeats it: the
        // rule is loaded tiled to the destination layer's size
        // (`imagepro->LoadImage(rulename, 8, 0x02ffffff, src1w, src1h, &scpro)`,
        // `TransIntf.cpp:781`; `GraphicsLoaderIntf.cpp:869`, `:877`,
        // `:950-975`), so the repeat period is the rule's size in *logical*
        // destination pixels.  Taking it from the physical viewport instead
        // tiled the whole transition once per (window / rule) axis -- four
        // quarter-screen copies at a 2x window, the reported symptom.
        let image = image_rect();
        let local = image_local(uv);
        let rule_dims = vec2<f32>(textureDimensions(rule_image));
        var rule_uv = local / max(rule_dims, vec2<f32>(1.0, 1.0));
        if (rule_dims.x < image.z) {
            rule_uv.x = fract(rule_uv.x);
        }
        if (rule_dims.y < image.w) {
            rule_uv.y = fract(rule_uv.y);
        }
        rule_uv = clamp(rule_uv, vec2<f32>(0.0, 0.0), vec2<f32>(0.9999, 0.9999));
        let rule_color = textureSample(rule_image, rule_sampler, rule_uv);
        rule_value = dot(rule_color.rgb, vec3<f32>(0.299, 0.587, 0.114));
    }
    let vague = max(uniforms.data[1].z / 255.0, 1.0 / 255.0);
    let phase = progress() * (1.0 + vague);
    let amount = clamp((phase - rule_value) / vague, 0.0, 1.0);
    return mix(old_color, new_color, amount);
}

fn transition_scroll(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let dir = scroll_direction(uniforms.data[1].w);
    let stay = uniforms.data[2].x;
    let new_disp = dir * (1.0 - p);
    let old_disp = -dir * p;
    let new_uv = uv - new_disp;
    let old_uv = uv - old_disp;
    let transparent = vec4<f32>(0.0, 0.0, 0.0, 0.0);

    if (stay >= 1.5) {
        if (in_bounds(old_uv)) {
            return sample_old(old_uv, transparent);
        }
        return textureSample(new_image, new_sampler, uv);
    }

    if (stay >= 0.5) {
        if (in_bounds(new_uv)) {
            return sample_new(new_uv, transparent);
        }
        return textureSample(old_image, old_sampler, uv);
    }

    if (in_bounds(new_uv)) {
        return sample_new(new_uv, transparent);
    }
    if (in_bounds(old_uv)) {
        return sample_old(old_uv, transparent);
    }
    return transparent;
}

// The kernel's clock: `0` means the caller supplied no duration and the phase
// comes from `progress` alone; otherwise this is the provider-clamped duration,
// `>= 2` ms (`wave.cpp:336`).  The clamp is applied again at the use site so a
// caller that hands over a shorter value cannot divide by a zero half-time.

// `BlendRatio = CurTime * 255 / Time` (`extrans/wave.cpp:156`).  `Blend` scales
// by 1/256 rather than 1/255 (`common.h:18-31`), so the blend factor is the
// reference's integer ratio over 256.  Without the millisecond clock the phase
// is the ratio itself.
fn wave_blend_ratio(timed: bool, cur_time: f32, total: f32, p: f32) -> f32 {
    if (!timed) {
        return p;
    }
    return floor(cur_time * 255.0 / total) / 256.0;
}

// `wave` (`extrans/wave.cpp:16-265`): raster scroll.  Each destination row is
// shifted by `d = (int)(sin(y*CurOmega + CurRadStart) * CurH)` samples, the
// vacated strip on the row becomes the animated `bgcolor1 -> bgcolor2` colour,
// and the covered span lerps `Src1` into `Src2` at `BlendRatio`
// (`tTVPWaveTransHandler::Process`, `wave.cpp:173-261`).  Options: `maxh` (50),
// `maxomega` (0.2), `bgcolor1`/`bgcolor2` (0), `wavetype` (0), `time` in
// milliseconds clamped to at least 2 (`:336`); the image extent is the
// destination rectangle in logical frame pixels, where the reference uses the
// destination layer's bitmap size (`src1w`/`src1h`, `:319-320`, `:355`) -- equal
// unless the layer's bitmap is scaled inside its rect.
//
// Deviations the M36 port records rather than hides:
//   * the reference runs on the integer millisecond clock (`CurTime =
//     tick - StartTick`, `wave.cpp:129-133`); this kernel rebuilds it from
//     `progress` and `data[10].x`, and only when the caller supplied no clock
//     (`Time == 0`) does it use the normalized phase, which is the same curve
//     minus the millisecond quantization;
//   * the destination layer type (`ltAlpha`/`ltAddAlpha`, `wave.cpp:238-246`)
//     has no channel in the transition model, so the plain
//     `TVPConstAlphaBlend_SD` path runs;
//   * the lerp is the float form of the reference's per-byte integer
//     `a + ((b-a)*opa >> 8)` (`common.h:26`), so it can differ by 1/255;
//   * the composite's `old`/`new` faces are whole scenes (the destination's
//     bitmap composited over the scene beneath), so for covered pixels that
//     scene is read at the *shifted* position where the reference composites
//     the blended bitmap over the unshifted scene.  Both agree wherever the
//     layer bitmaps are opaque; semi-transparent layers shift what shows
//     through by `d`.
fn transition_wave(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let frame = viewport_size();
    let image = image_rect();
    let scale = transform_scale();

    // The reference's handlers work in the destination bitmap's own pixels
    // (`tTVPDivisibleData::Left/Top/Width/Height`, `LayerIntf.cpp:6513-6540`);
    // image pixels are the layer's logical pixels.
    let local = image_local(uv);

    // `StartProcess` (`wave.cpp:120-166`).
    let time = duration_millis();
    let timed = time > 0.0;
    // `if(time < 2) time = 2;` (`wave.cpp:336`, the ctor call is `:355`): every
    // extrans provider clamps `time` before it builds the handler, because a
    // shorter duration would make `HalfTime = Time / 2` zero.  `0` stays the
    // "no clock supplied" case and runs on `progress` alone.
    let total = select(1.0, max(time, 2.0), timed);
    let cur_time = select(p, p * total, timed);
    // `HalfTime = Time / 2` is integer division (`wave.cpp:47`); with the clamp
    // above it is at least 1.
    let half = select(0.5, floor(total * 0.5), timed);
    var t = clamp(cur_time, 0.0, total);
    if (t >= half) {
        t = total - t;
    }
    let ramp = sin((3.14159265359 * 0.5) * t / half);
    // `CurH = tt * MaxH` (`:145`), truncated to `tjs_int`.
    let cur_h = trunc(ramp * uniforms.data[2].z);
    // `CurOmega` per `wavetype` (`:147-158`); `wavetype` is read as `tjs_int`, so
    // anything outside 0..2 follows the default branch (the reference's switch
    // leaves `CurOmega` untouched there).
    let wave_type = trunc(uniforms.data[2].y);
    let max_omega = uniforms.data[2].w;
    var omega = max_omega * ramp;
    if (wave_type == 1.0) {
        omega = max_omega * (total - cur_time) / total;
    }
    if (wave_type == 2.0) {
        omega = max_omega * cur_time / total;
    }
    // `rad = data->Top * CurOmega + CurRadStart` (`wave.cpp:175,183`): one phase
    // per integer image row (`data->Top + n`), so the row index floors.
    let rad = floor(local.y) * omega - omega * floor(image.w * 0.5);
    // `d = (int)(sin(rad) * CurH)` (`:186`).
    let d = trunc(sin(rad) * cur_h);
    let ratio = wave_blend_ratio(timed, cur_time, total, p);

    // `Clip(l, r, Left, Left + Width)` (`:227-236`): outside the covered span the
    // destination bitmap's samples are replaced by `CurBGColor`.
    let src_x = local.x - d;
    if (src_x < 0.0 || src_x >= image.z) {
        // `TVPFillARGB(dest, ..., CurBGColor)` (`:203-221`) writes the colour into
        // the destination's own bitmap; the layer manager then composites that
        // bitmap over the scene beneath, which is the `under` pass.
        let bg = mix(uniforms.data[3], uniforms.data[4], ratio);
        if (!under_available()) {
            return bg;
        }
        let under = textureSample(under_image, under_sampler, uv);
        return vec4<f32>(
            bg.rgb * bg.a + under.rgb * (1.0 - bg.a),
            bg.a + under.a * (1.0 - bg.a),
        );
    }

    // `TVPConstAlphaBlend_SD(dest, src1, src2, len, BlendRatio)` (`:238-246`):
    // both bitmaps are read at the shifted column and lerped sample by sample.
    let shifted = uv - vec2<f32>(d * scale / frame.x, 0.0);
    return mix(
        textureSample(old_image, old_sampler, shifted),
        textureSample(new_image, new_sampler, shifted),
        ratio,
    );
}

fn transition_mosaic(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let pi = 3.14159265359;
    let block = max(1.0, 1.0 + sin(p * pi) * uniforms.data[5].x);
    // The block grid is measured in the destination bitmap's pixels
    // (`mosaic.cpp:123-143` anchors it from `Width`/`Height`), so the window
    // scale cannot change the blocks.
    let block_uv = frame_uv((floor(image_local(uv) / block) + vec2<f32>(0.5, 0.5)) * block);
    return mix(textureSample(old_image, old_sampler, block_uv), textureSample(new_image, new_sampler, block_uv), p);
}

fn hash_tile(tile: vec2<f32>) -> f32 {
    return fract(sin(dot(tile, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

fn transition_turn(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let bg = uniforms.data[3];
    // `xcount = (Width-1)/64 + 1` (`turn.cpp:141-142`): the 64x64 tile grid is
    // counted in the destination bitmap's pixels, never the window's.
    let extent = max(image_rect().zw, vec2<f32>(1.0, 1.0));
    let tile_count = max(floor(extent / 64.0), vec2<f32>(1.0, 1.0));
    let tile_size = extent / tile_count;
    let local_px = image_local(uv);
    let tile = floor(local_px / tile_size);
    let local = local_px / tile_size - tile;
    let delay = hash_tile(tile) * 0.25;
    let t = clamp((p - delay) / max(1.0 - delay, 0.001), 0.0, 1.0);
    let width = max(abs(t - 0.5) * 2.0, 0.04);
    if (abs(local.x - 0.5) > width * 0.5) {
        return bg;
    }
    let corrected_local = vec2<f32>((local.x - 0.5) / width + 0.5, local.y);
    let sample_uv = frame_uv((tile + corrected_local) * tile_size);
    if (t < 0.5) {
        return textureSample(old_image, old_sampler, sample_uv);
    }
    return textureSample(new_image, new_sampler, sample_uv);
}

// Rotation happens in the destination bitmap's own pixels: `rotatetrans.cpp`
// builds its matrix from `Width`/`Height` and `CenterX`/`CenterY` (`:66-115`),
// so both the pivot and the pixel aspect are the destination's -- the window's
// scale must not move or shear them.
fn rotate_uv(uv: vec2<f32>, center: vec2<f32>, scale: f32, angle: f32) -> vec2<f32> {
    let c = cos(-angle);
    let s = sin(-angle);
    let pivot = image_local(center);
    let delta = image_local(uv) - pivot;
    let rotated = vec2<f32>(delta.x * c - delta.y * s, delta.x * s + delta.y * c) / max(scale, 0.001);
    return frame_uv(pivot + rotated);
}

// The destination bitmap's centre, the reference's default pivot
// (`rotatetrans.cpp:185-186`: `centerx = src1w / 2`, `centery = src1h / 2`).
fn default_center() -> vec2<f32> {
    let image = image_rect();
    return frame_uv(image.zw * 0.5);
}

// `centerx`/`centery` are destination-bitmap pixels (`rotatetrans.cpp:185-210`:
// the default is `src1w / 2`, `src1h / 2` and the option is read as an integer
// of that bitmap), so the pivot's uv is the destination's own logical position
// and never depends on the window.
fn transition_center() -> vec2<f32> {
    if (uniforms.data[6].y >= 0.0 && uniforms.data[6].z >= 0.0) {
        return frame_uv(uniforms.data[6].yz);
    }
    return default_center();
}

fn transition_rotatezoom(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let pi = 3.14159265359;
    let center = transition_center();
    let scale_t = acceleration(p, uniforms.data[5].z);
    let twist_t = acceleration(p, uniforms.data[6].x);
    let scale = mix(max(uniforms.data[5].y, 0.001), 1.0, scale_t);
    let angle = uniforms.data[5].w * pi * 2.0 * (1.0 - twist_t);
    let sample_uv = rotate_uv(uv, center, scale, angle);
    let old_color = textureSample(old_image, old_sampler, uv);
    if (!in_bounds(sample_uv)) {
        return old_color;
    }
    let new_color = textureSample(new_image, new_sampler, sample_uv);
    return mix(old_color, new_color, p);
}

fn transition_rotatevanish(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let pi = 3.14159265359;
    let center = transition_center();
    let scale_t = acceleration(p, uniforms.data[5].z);
    let twist_t = acceleration(p, uniforms.data[6].x);
    let scale = max(1.0 - scale_t, 0.001);
    let angle = uniforms.data[5].w * pi * 2.0 * twist_t;
    let sample_uv = rotate_uv(uv, center, scale, angle);
    let new_color = textureSample(new_image, new_sampler, uv);
    if (!in_bounds(sample_uv)) {
        return new_color;
    }
    let old_color = textureSample(old_image, old_sampler, sample_uv);
    return mix(old_color, new_color, p);
}

fn transition_rotateswap(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let pi = 3.14159265359;
    let twist = uniforms.data[5].w * pi * 2.0;
    let bg = uniforms.data[3];
    // `rotateswap` reads no `centerx`/`centery` (`rotatetrans.cpp:394-475`); its
    // pivot is the destination bitmap's centre (`:339-340`).
    let center = default_center();
    let old_uv = rotate_uv(uv, center, mix(1.0, 0.25, p), twist * p);
    let new_uv = rotate_uv(uv, center, mix(0.25, 1.0, p), twist * (p - 1.0));
    let old_color = sample_old(old_uv, bg);
    let new_color = sample_new(new_uv, bg);
    return mix(old_color, new_color, p);
}

fn transition_ripple(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    // `ripple.cpp` builds its displacement tables from `Width`/`Height` (the
    // destination bitmap, `:1072`) and `rwidth`/`maxdrift` are pixels of that
    // bitmap (`:1478-1525`): `local/extent` is the old frame uv with the window
    // removed, so the front, the band and the drift are destination-locked.
    let extent = max(image_rect().zw, vec2<f32>(1.0, 1.0));
    let roundness = max(uniforms.data[7].x, 0.01);
    let aspect_vec = vec2<f32>(extent.x / extent.y / roundness, roundness);
    let center = transition_center();
    let local_uv = image_local(uv) / extent;
    let center_uv = image_local(center) / extent;
    let delta = (local_uv - center_uv) * aspect_vec;
    let dist = length(delta);
    let corner = max(center_uv, vec2<f32>(1.0, 1.0) - center_uv) * aspect_vec;
    let max_dist = length(corner);
    let width = uniforms.data[6].w / min(extent.x, extent.y);
    let front = p * (max_dist + width * uniforms.data[7].y);
    let reveal = 1.0 - smoothstep(front - width, front + width, dist);
    let dir = normalize(delta + vec2<f32>(0.0001, 0.0)) / aspect_vec;
    let wave = sin((dist - front) * uniforms.data[7].y * 24.0);
    let envelope = exp(-abs(dist - front) / max(width * 3.0, 0.001)) * (1.0 - p);
    let drift = wave * envelope * uniforms.data[7].z / min(extent.x, extent.y);
    let sample_uv = uv + dir * drift;
    let old_color = sample_old(sample_uv, textureSample(old_image, old_sampler, uv));
    let new_color = sample_new(sample_uv, textureSample(new_image, new_sampler, uv));
    return mix(old_color, new_color, reveal);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let method = uniforms.data[0].y;
    if (method < 0.5) {
        return transition_crossfade(input.tex_coord);
    }
    if (method < 1.5) {
        return transition_universal(input.tex_coord);
    }
    if (method < 2.5) {
        return transition_scroll(input.tex_coord);
    }
    if (method < 3.5) {
        return transition_wave(input.tex_coord);
    }
    if (method < 4.5) {
        return transition_mosaic(input.tex_coord);
    }
    if (method < 5.5) {
        return transition_turn(input.tex_coord);
    }
    if (method < 6.5) {
        return transition_rotatezoom(input.tex_coord);
    }
    if (method < 7.5) {
        return transition_rotatevanish(input.tex_coord);
    }
    if (method < 8.5) {
        return transition_rotateswap(input.tex_coord);
    }
    return transition_ripple(input.tex_coord);
}
