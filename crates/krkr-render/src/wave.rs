//! CPU mirror of `transition.wgsl`'s `wave` kernel.
//!
//! The engine's transitions run in WGSL, which no unit test can execute on every
//! platform, so the kernel lives in two places: the shader
//! (`transition.wgsl::transition_wave`) and this module, which transcribes it
//! line by line over the same uniform layout (`TransitionUniforms::wave_frame`
//! reads the slots the shader's accessors read).  The two are kept honest from
//! both sides:
//!
//! * `crates/krkr-render/src/lib.rs`'s `wave_shader_carries_the_cpu_kernel_constants`
//!   extracts the shader's kernel text and checks the constants and the formula
//!   markers this module uses, and
//! * `wave_shader_matches_the_cpu_kernel_on_the_gpu` renders the composite pass
//!   on a real device and compares every output pixel against this module.
//!
//! The reference being mirrored is `extrans/wave.cpp` (`tTVPWaveTransHandler`,
//! `:120-261`); the fidelity test in this module compares the kernel against a
//! test-only transcription of that C++ over fixed parameter sets.

/// The frame state the `wave` kernel reads: the uniform slots the shader's
/// accessors expose, in the shader's own units (physical viewport pixels for
/// `viewport`, logical frame pixels for `image_rect`, milliseconds for
/// `duration_millis`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WaveFrame {
    pub progress: f32,
    /// `data[10].x`: the transition's duration (`tTVPWaveTransHandler::Time`).
    /// `0` means the caller did not supply the clock.
    pub duration_millis: f32,
    /// `viewport_size()`: the physical target size.
    pub viewport: [f32; 2],
    /// `transform_scale()`: logical frame pixels to physical pixels.
    pub scale: f32,
    /// `transform_offset()`: the letterbox offset in physical pixels.
    pub origin: [f32; 2],
    /// `image_rect()`: the destination layer's rectangle, logical frame pixels,
    /// `[x, y, width, height]`.
    pub image_rect: [f32; 4],
    /// `data[2].y`, `data[2].z`, `data[2].w`: `wavetype`, `maxh`, `maxomega`.
    pub wave_type: f32,
    pub max_h: f32,
    pub max_omega: f32,
    /// `data[3]`, `data[4]`: `bgcolor1`, `bgcolor2`, straight alpha.
    pub bg_color1: [f32; 4],
    pub bg_color2: [f32; 4],
    /// `under_available()`: whether the under pass was bound (binding 7).
    pub under_available: bool,
}

/// The per-frame state `tTVPWaveTransHandler::StartProcess` computes
/// (`wave.cpp:120-166`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WaveState {
    /// `CurH`: the amplitude, truncated to whole image pixels.
    pub cur_h: f32,
    /// `CurOmega`: the phase step per image row.
    pub omega: f32,
    /// `CurRadStart = -CurOmega * (Height / 2)`; `Height / 2` is integer.
    pub rad_start: f32,
    /// `BlendRatio`, already scaled the way `Blend` scales it (`/256`).
    pub blend_ratio: f32,
    /// `CurBGColor`, straight alpha.
    pub bg_color: [f32; 4],
}

/// The reference's `Blend` ratio scale (`common.h:18-31`): `BlendRatio` steps in
/// 255ths and `Blend` scales by 256ths.
pub(crate) const WAVE_RATIO_STEPS: f32 = 255.0;
pub(crate) const WAVE_RATIO_DIVISOR: f32 = 256.0;

/// `if(time < 2) time = 2;` (`extrans/wave.cpp:336`): the shortest duration the
/// reference's handler can be built with, and what a shorter option becomes.
pub(crate) const WAVE_MIN_TIME_MILLIS: f32 = 2.0;

/// The shader's `3.14159265359`, kept as one constant so the sync test can
/// check both sides carry the same value.  (`3.14159265359` and
/// `f32::consts::PI` are the same `f32`; the shader spells the literal out
/// because WGSL has no `PI`.)
pub(crate) const WAVE_PI: f32 = std::f32::consts::PI;

impl WaveFrame {
    /// `StartProcess` (`wave.cpp:120-166`).  With `duration_millis == 0` the
    /// normalized `progress` stands in for `CurTime / Time` and `Time` is 1, so
    /// only the millisecond quantization of the reference's clock is skipped.
    /// A supplied duration below 2 ms is clamped the way every extrans provider
    /// clamps it (`if(time < 2) time = 2;`, `wave.cpp:336`).
    pub(crate) fn state(&self) -> WaveState {
        let p = self.progress.clamp(0.0, 1.0);
        let time = self.duration_millis.max(0.0);
        let timed = time > 0.0;
        // `if(time < 2) time = 2;` (`wave.cpp:336`, the ctor call is `:355`):
        // `HalfTime = Time / 2` would otherwise be zero and every row's phase a
        // division by zero.  `0` stays the "no clock supplied" case.
        let total = if timed { time.max(WAVE_MIN_TIME_MILLIS) } else { 1.0 };
        let cur_time = if timed { p * total } else { p };
        // `HalfTime = Time / 2`, integer division (`wave.cpp:47`); the clamp
        // above keeps it at least 1.
        let half = if timed { (total * 0.5).floor() } else { 0.5 };
        // `t = CurTime; if (t >= HalfTime) t = Time - t; if (t < 0) t = 0;`
        let mut t = cur_time.clamp(0.0, total);
        if t >= half {
            t = total - t;
        }
        let ramp = (WAVE_PI * 0.5 * t / half).sin();
        // `CurH = (int)(tt * MaxH)` (`wave.cpp:145`).
        let cur_h = (ramp * self.max_h).trunc();
        // `CurOmega` per `wavetype` (`wave.cpp:147-158`).  The reference's switch
        // has no default arm, so a `wavetype` outside 0..2 leaves `CurOmega` at
        // its previous value; the documented approximation is to keep the
        // `wavetype == 0` ramp for those.
        let wave_type = self.wave_type.trunc();
        let mut omega = self.max_omega * ramp;
        if wave_type == 1.0 {
            omega = self.max_omega * (total - cur_time) / total;
        }
        if wave_type == 2.0 {
            omega = self.max_omega * cur_time / total;
        }
        // `CurRadStart = -CurOmega * (Height / 2)` (`wave.cpp:160`), integer.
        let rad_start = -omega * (self.image_rect[3] * 0.5).floor();
        // `BlendRatio = CurTime * 255 / Time` (`wave.cpp:156`), then `Blend`
        // scales by `/256` (`common.h:26`).
        let blend_ratio = if timed {
            (cur_time * WAVE_RATIO_STEPS / total).floor() / WAVE_RATIO_DIVISOR
        } else {
            p
        };
        let bg_color = mix(self.bg_color1, self.bg_color2, blend_ratio);
        WaveState {
            cur_h,
            omega,
            rad_start,
            blend_ratio,
            bg_color,
        }
    }

    /// The layer-local pixel coordinates of a frame-space `uv`, the way the
    /// shader's `local` is computed: image pixels are the layer's logical
    /// pixels.
    pub(crate) fn local(&self, uv: [f32; 2]) -> [f32; 2] {
        // `transform_scale()` guards against a zero scale; mirror that.
        let scale = self.scale.max(1.0e-6);
        [
            (uv[0] * self.viewport[0] - self.origin[0]) / scale - self.image_rect[0],
            (uv[1] * self.viewport[1] - self.origin[1]) / scale - self.image_rect[1],
        ]
    }

    /// `d = (int)(sin(rad) * CurH)` (`wave.cpp:186`) for one integer image row.
    pub(crate) fn row_shift(&self, state: &WaveState, image_row: f32) -> f32 {
        let rad = image_row * state.omega + state.rad_start;
        (rad.sin() * state.cur_h).trunc()
    }
}

/// The kernel for one composite pixel.
///
/// `uv` is the frame-space coordinate; the samplers take the same `uv` and
/// return the frame textures' values (premultiplied, as the render targets
/// hold them), so the caller decides what "the scene" is.
pub(crate) fn wave_pixel(
    frame: &WaveFrame,
    uv: [f32; 2],
    sample_old: &dyn Fn([f32; 2]) -> [f32; 4],
    sample_new: &dyn Fn([f32; 2]) -> [f32; 4],
    sample_under: &dyn Fn([f32; 2]) -> [f32; 4],
) -> [f32; 4] {
    let state = frame.state();
    let local = frame.local(uv);
    let shift = frame.row_shift(&state, local[1].floor());
    // `Clip(l, r, Left, Left + Width)` (`wave.cpp:227-236`): outside the covered
    // span the destination bitmap's samples are replaced by `CurBGColor`.
    let source_x = local[0] - shift;
    if source_x < 0.0 || source_x >= frame.image_rect[2] {
        let bg = state.bg_color;
        if !frame.under_available {
            return bg;
        }
        // `TVPFillARGB(dest, ..., CurBGColor)` (`wave.cpp:203-221`) writes into
        // the destination layer's own bitmap, which the layer manager then
        // composites over the scene beneath (`LayerIntf.cpp:6513-6540`).
        return straight_over(bg, sample_under(uv));
    }
    // `TVPConstAlphaBlend_SD(dest, src1, src2, len, BlendRatio)` (`wave.cpp:238-246`).
    let shifted = [uv[0] - shift * frame.scale / frame.viewport[0], uv[1]];
    mix(sample_old(shifted), sample_new(shifted), state.blend_ratio)
}

fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for channel in 0..4 {
        out[channel] = a[channel] + (b[channel] - a[channel]) * t;
    }
    out
}

/// A straight-alpha colour over a premultiplied frame sample.
fn straight_over(color: [f32; 4], under: [f32; 4]) -> [f32; 4] {
    let alpha = color[3];
    [
        color[0] * alpha + under[0] * (1.0 - alpha),
        color[1] * alpha + under[1] * (1.0 - alpha),
        color[2] * alpha + under[2] * (1.0 - alpha),
        alpha + under[3] * (1.0 - alpha),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only transcription of the official handler, integer milliseconds and
    /// per-byte arithmetic included (`extrans/wave.cpp:120-261`).
    mod reference {
        /// `tTVPWaveTransHandler::StartProcess` state.
        #[derive(Clone, Copy, Debug)]
        pub struct State {
            pub cur_h: i32,
            pub omega: f64,
            pub rad_start: f64,
            pub blend_ratio: u32,
            pub bg: u32,
        }

        pub struct Setup {
            pub time: i64,
            pub maxh: i32,
            pub maxomega: f64,
            pub wavetype: i32,
            pub bg1: u32,
            pub bg2: u32,
            pub height: i32,
        }

        /// `StartProcess` (`wave.cpp:120-166`).
        pub fn start_process(setup: &Setup, tick: i64) -> State {
            let mut cur_time = tick;
            let mut t = tick;
            if cur_time > setup.time {
                cur_time = setup.time;
            }
            let half_time = setup.time / 2;
            if t >= half_time {
                t = setup.time - t;
            }
            if t < 0 {
                t = 0;
            }
            let tt = ((std::f64::consts::PI / 2.0) * t as f64 / half_time as f64).sin();
            let cur_h = (tt * setup.maxh as f64) as i32;
            let omega = match setup.wavetype {
                0 => setup.maxomega * tt,
                1 => setup.maxomega * (setup.time - cur_time) as f64 / setup.time as f64,
                2 => setup.maxomega * cur_time as f64 / setup.time as f64,
                other => unreachable!("the reference's switch has no arm for wavetype {other}"),
            };
            let rad_start = -omega * (setup.height / 2) as f64;
            let blend_ratio = (cur_time * 255 / setup.time) as u32;
            State {
                cur_h,
                omega,
                rad_start,
                blend_ratio,
                bg: blend(setup.bg1, setup.bg2, blend_ratio),
            }
        }

        /// `Blend(a, b, opa)` (`common.h:18-31`): per byte
        /// `a + ((b - a) * opa >> 8)`.
        pub fn blend(a: u32, b: u32, opa: u32) -> u32 {
            let byte = |shift: u32| -> u32 {
                let av = ((a >> shift) & 0xff) as i32;
                let bv = ((b >> shift) & 0xff) as i32;
                (((av + (((bv - av) * opa as i32) >> 8)) as u32) & 0xff) << shift
            };
            byte(0) | byte(8) | byte(16) | byte(24)
        }

        /// `Process` (`wave.cpp:173-261`): the destination bitmap's value for one
        /// pixel, `src1`/`src2` the bitmaps the reference reads at `x - d`.
        pub fn process_pixel(state: &State, x: i32, y: i32, width: i32, src1: u32, src2: u32) -> u32 {
            let rad = y as f64 * state.omega + state.rad_start;
            let d = (rad.sin() * state.cur_h as f64) as i32;
            let source_x = x - d;
            if source_x < 0 || source_x >= width {
                return state.bg;
            }
            blend(src1, src2, state.blend_ratio)
        }
    }

    const WIDTH: i32 = 64;
    const HEIGHT: i32 = 16;
    const FRAME_WIDTH: f32 = WIDTH as f32;
    const FRAME_HEIGHT: f32 = HEIGHT as f32;

    /// The destination layer's bitmap (`tTVPDivisibleData::Src1`, opaque).
    fn dest_pixel(x: i32, y: i32) -> [u8; 4] {
        [
            (x * 27) as u8,
            (y * 41) as u8,
            (128 + x * 7) as u8,
            255,
        ]
    }

    /// The source layer's bitmap (`tTVPDivisibleData::Src2`, opaque).
    fn source_pixel(x: i32, y: i32) -> [u8; 4] {
        [
            (255 - x * 23) as u8,
            (255 - y * 31) as u8,
            (x * 11 + y * 3) as u8,
            255,
        ]
    }

    /// The scene beneath the destination layer, premultiplied (opaque).
    const UNDER: [f32; 4] = [0.2, 0.4, 0.6, 1.0];

    fn argb(pixel: [u8; 4]) -> u32 {
        ((pixel[3] as u32) << 24)
            | ((pixel[0] as u32) << 16)
            | ((pixel[1] as u32) << 8)
            | pixel[2] as u32
    }

    fn straight(color: u32) -> [f32; 4] {
        [
            ((color >> 16) & 0xff) as f32 / 255.0,
            ((color >> 8) & 0xff) as f32 / 255.0,
            (color & 0xff) as f32 / 255.0,
            ((color >> 24) & 0xff) as f32 / 255.0,
        ]
    }

    fn bytes(pixel: [f32; 4]) -> [u8; 4] {
        [
            (pixel[0] * 255.0).round() as u8,
            (pixel[1] * 255.0).round() as u8,
            (pixel[2] * 255.0).round() as u8,
            (pixel[3] * 255.0).round() as u8,
        ]
    }

    /// One frame texture tap: nearest texel, which is what the sampler returns
    /// when the shifted tap lands on a texel centre (the kernel's shifts are
    /// whole image pixels).
    fn sample(bitmap: fn(i32, i32) -> [u8; 4], uv: [f32; 2]) -> [f32; 4] {
        let x = (uv[0] * FRAME_WIDTH - 0.5).round() as i32;
        let y = (uv[1] * FRAME_HEIGHT - 0.5).round() as i32;
        if x < 0 || y < 0 || x >= WIDTH || y >= HEIGHT {
            return [0.0, 0.0, 0.0, 0.0];
        }
        let pixel = bitmap(x, y);
        [
            pixel[0] as f32 / 255.0,
            pixel[1] as f32 / 255.0,
            pixel[2] as f32 / 255.0,
            pixel[3] as f32 / 255.0,
        ]
    }

    #[allow(clippy::too_many_arguments)]
    fn frame(
        progress: f32,
        duration_millis: f32,
        maxh: f32,
        maxomega: f32,
        wavetype: f32,
        bg1: u32,
        bg2: u32,
    ) -> WaveFrame {
        WaveFrame {
            progress,
            duration_millis,
            viewport: [FRAME_WIDTH, FRAME_HEIGHT],
            scale: 1.0,
            origin: [0.0, 0.0],
            image_rect: [0.0, 0.0, FRAME_WIDTH, FRAME_HEIGHT],
            max_h: maxh,
            max_omega: maxomega,
            wave_type: wavetype,
            bg_color1: straight(bg1),
            bg_color2: straight(bg2),
            under_available: true,
        }
    }

    /// The fixed parameter sets the fidelity run uses: `time`, `maxh`,
    /// `maxomega`, `wavetype`, `bgcolor1`, `bgcolor2`.
    const PARAMETER_SETS: &[(i64, i32, f64, i32, u32, u32)] = &[
        (1000, 50, 0.2, 0, 0x0000_0000, 0x0000_0000),
        (600, 20, 0.1, 1, 0x8040_2010, 0xc0ff_8040),
        (777, 33, 0.35, 2, 0xff00_0000, 0x8000_0000),
    ];

    fn setup(time: i64, maxh: i32, maxomega: f64, wavetype: i32, bg1: u32, bg2: u32) -> reference::Setup {
        reference::Setup {
            time,
            maxh,
            maxomega,
            wavetype,
            bg1,
            bg2,
            height: HEIGHT,
        }
    }

    /// The value the engine's composite must produce for one pixel, taken from
    /// the reference handler: its destination-bitmap value where the pixel is
    /// covered, and -- where `TVPFillARGB` replaced the bitmap with the
    /// background colour -- that colour composited over the scene beneath the
    /// layer.  Returns `(value, covered)`.
    fn reference_expected(state: &reference::State, x: i32, y: i32) -> ([f32; 4], bool) {
        let d = ((y as f64 * state.omega + state.rad_start).sin() * state.cur_h as f64) as i32;
        let source_x = x - d;
        if !(0..WIDTH).contains(&source_x) {
            // `Process` returns the background colour for the strip; the sources
            // are never read there.
            let bg = reference::process_pixel(state, x, y, WIDTH, 0, 0);
            return (straight_over(straight(bg), UNDER), false);
        }
        let value = reference::process_pixel(
            state,
            x,
            y,
            WIDTH,
            argb(dest_pixel(source_x, y)),
            argb(source_pixel(source_x, y)),
        );
        (straight(value), true)
    }

    fn uv_of(x: i32, y: i32) -> [f32; 2] {
        [
            (x as f32 + 0.5) / FRAME_WIDTH,
            (y as f32 + 0.5) / FRAME_HEIGHT,
        ]
    }

    fn engine_pixel(frame: &WaveFrame, x: i32, y: i32) -> [f32; 4] {
        wave_pixel(
            frame,
            uv_of(x, y),
            &|uv| sample(dest_pixel, uv),
            &|uv| sample(source_pixel, uv),
            &|_| UNDER,
        )
    }

    /// The kernel must reproduce the reference's raster scroll when the
    /// millisecond clock is supplied.  What is left of the difference is the
    /// reference's integer arithmetic: `BlendRatio` steps in 255ths, `Blend`
    /// scales by 256ths, and every output byte is rounded to 8 bits -- at most
    /// one 8-bit step plus one ratio step per channel.
    #[test]
    fn wave_kernel_matches_the_reference_port() {
        let mut max_deviation = 0.0f32;
        let mut covered = 0usize;
        let mut strip = 0usize;
        for &(time, maxh, maxomega, wavetype, bg1, bg2) in PARAMETER_SETS {
            let setup = setup(time, maxh, maxomega, wavetype, bg1, bg2);
            for step in 0..=10 {
                let cur_time = time * step / 10;
                let frame = frame(
                    cur_time as f32 / time as f32,
                    time as f32,
                    maxh as f32,
                    maxomega as f32,
                    wavetype as f32,
                    bg1,
                    bg2,
                );
                let state = reference::start_process(&setup, cur_time);
                for y in 0..HEIGHT {
                    for x in 0..WIDTH {
                        let engine = engine_pixel(&frame, x, y);
                        let (expected, is_covered) = reference_expected(&state, x, y);
                        if is_covered {
                            covered += 1;
                        } else {
                            strip += 1;
                        }
                        for channel in 0..4 {
                            max_deviation =
                                max_deviation.max((engine[channel] - expected[channel]).abs());
                        }
                    }
                }
            }
        }
        assert!(covered > 0 && strip > 0, "the run must exercise both regions");
        println!(
            "wave kernel vs reference: max per-channel deviation {max_deviation:.5} \
             ({covered} covered pixels, {strip} strip pixels)"
        );
        // The bound is the measured maximum (1/256, one integer-blend step), so
        // a regression of a single 8-bit step fails this instead of hiding under
        // a loose ceiling.
        assert!(
            max_deviation <= 1.0 / 255.0,
            "max per-channel deviation {max_deviation} exceeds one 8-bit step"
        );
    }

    /// The reference clamps `time` to at least 2 ms before building the handler
    /// (`if(time < 2) time = 2;`, `wave.cpp:336`); without that `HalfTime =
    /// Time / 2` would be zero and every row's phase a division by zero.  `1 ms`
    /// is the case that reaches the clamp, and it must behave exactly like the
    /// reference's clamped 2 ms run.
    #[test]
    fn wave_clock_clamps_durations_below_two_milliseconds() {
        let one_ms = frame(0.5, 1.0, 50.0, 0.2, 0.0, 0, 0);
        let two_ms = frame(0.5, 2.0, 50.0, 0.2, 0.0, 0, 0);
        assert_eq!(one_ms.state(), two_ms.state());
        let state = one_ms.state();
        assert!(
            state.cur_h.is_finite()
                && state.omega.is_finite()
                && state.rad_start.is_finite()
                && state.blend_ratio.is_finite()
                && state.bg_color.iter().all(|channel| channel.is_finite()),
            "the clamped clock must stay finite: {state:?}"
        );

        // Every phase of a 1 ms clock produces finite pixels...
        for step in 0..=4 {
            let kernel_frame = frame(step as f32 / 4.0, 1.0, 50.0, 0.2, 0.0, 0, 0);
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    for channel in engine_pixel(&kernel_frame, x, y) {
                        assert!(channel.is_finite(), "1 ms clock, pixel ({x}, {y})");
                    }
                }
            }
        }

        // ...and matches the reference running its own clamped 2 ms clock.  The
        // kernel's `progress = 0.5` is `CurTime = 1` of the clamped 2 ms.
        let setup = setup(2, 50, 0.2, 0, 0, 0);
        let state = reference::start_process(&setup, 1);
        let kernel_frame = frame(0.5, 1.0, 50.0, 0.2, 0.0, 0, 0);
        let mut max_deviation = 0.0f32;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let engine = engine_pixel(&kernel_frame, x, y);
                let (expected, _) = reference_expected(&state, x, y);
                for channel in 0..4 {
                    max_deviation = max_deviation.max((engine[channel] - expected[channel]).abs());
                }
            }
        }
        assert!(
            max_deviation <= 1.0 / 255.0,
            "1 ms must render as the reference's clamped 2 ms; deviation {max_deviation}"
        );
    }

    /// Without the caller's millisecond clock the kernel derives `CurTime` from
    /// `progress`, which is the same curve minus the reference's integer
    /// millisecond quantization.  This pins how far that can move a pixel.
    #[test]
    fn wave_kernel_without_the_clock_stays_within_a_pixel() {
        let (time, maxh, maxomega, wavetype, bg1, bg2) =
            (1000, 50, 0.2, 0, 0x8040_2010, 0xc0ff_8040);
        let setup = setup(time, maxh, maxomega, wavetype, bg1, bg2);
        let mut max_deviation = 0.0f32;
        for step in 0..=10 {
            let cur_time = time * step / 10;
            let frame = frame(
                cur_time as f32 / time as f32,
                0.0,
                maxh as f32,
                maxomega as f32,
                wavetype as f32,
                bg1,
                bg2,
            );
            let state = reference::start_process(&setup, cur_time);
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let engine = engine_pixel(&frame, x, y);
                    let (expected, _) = reference_expected(&state, x, y);
                    for channel in 0..4 {
                        max_deviation =
                            max_deviation.max((engine[channel] - expected[channel]).abs());
                    }
                }
            }
        }
        println!("wave kernel without the ms clock: max deviation {max_deviation:.5}");
        assert!(
            max_deviation <= 0.012,
            "max deviation {max_deviation} exceeds the progress-approximation bound"
        );
    }

    /// The frame transform and the destination rectangle reach the kernel through
    /// uniform slots 8/9 (`local = (uv*frame - origin)/scale - image.xy`,
    /// `shifted = uv - d*scale/frame.x`).  Every test above runs at scale 1 with
    /// the image at the frame origin, so this one pins the mapping with a 2x
    /// content scale, a letterbox offset and a non-zero rectangle -- a missing or
    /// misapplied term moves rows or columns and blows the deviation up.
    #[test]
    fn wave_kernel_maps_image_space_through_the_frame_transform() {
        const VIEWPORT: [f32; 2] = [256.0, 144.0];
        const SCALE: f32 = 2.0;
        const ORIGIN: [f32; 2] = [16.0, 8.0];
        const IMAGE: [f32; 4] = [30.0, 20.0, WIDTH as f32, HEIGHT as f32];

        let (time, maxh, maxomega, wavetype, bg1, bg2) =
            (1000, 40, 0.25, 0, 0x8040_2010, 0xc0ff_8040);
        let setup = setup(time, maxh, maxomega, wavetype, bg1, bg2);
        let base = WaveFrame {
            progress: 0.0,
            viewport: VIEWPORT,
            scale: SCALE,
            origin: ORIGIN,
            image_rect: IMAGE,
            ..frame(0.0, time as f32, maxh as f32, maxomega as f32, wavetype as f32, bg1, bg2)
        };
        // The frame texture holds the layer's bitmap at the image rect's physical
        // position, so a tap maps back through the same transform.
        let tap = |bitmap: fn(i32, i32) -> [u8; 4], uv: [f32; 2]| -> [f32; 4] {
            let local = [
                (uv[0] * VIEWPORT[0] - ORIGIN[0]) / SCALE - IMAGE[0],
                (uv[1] * VIEWPORT[1] - ORIGIN[1]) / SCALE - IMAGE[1],
            ];
            let x = (local[0] - 0.5).round() as i32;
            let y = (local[1] - 0.5).round() as i32;
            if !(0..WIDTH).contains(&x) || !(0..HEIGHT).contains(&y) {
                return [0.0, 0.0, 0.0, 0.0];
            }
            let pixel = bitmap(x, y);
            [
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
                pixel[3] as f32 / 255.0,
            ]
        };

        let mut max_deviation = 0.0f32;
        let mut covered = 0usize;
        for step in 0..=10 {
            let cur_time = time * step / 10;
            let kernel_frame = WaveFrame {
                progress: cur_time as f32 / time as f32,
                ..base
            };
            let state = reference::start_process(&setup, cur_time);
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    // The pixel centre of image pixel (x, y) in frame uv.
                    let uv = [
                        (ORIGIN[0] + (IMAGE[0] + x as f32 + 0.5) * SCALE) / VIEWPORT[0],
                        (ORIGIN[1] + (IMAGE[1] + y as f32 + 0.5) * SCALE) / VIEWPORT[1],
                    ];
                    let engine = wave_pixel(
                        &kernel_frame,
                        uv,
                        &|uv| tap(dest_pixel, uv),
                        &|uv| tap(source_pixel, uv),
                        &|_| UNDER,
                    );
                    let (expected, is_covered) = reference_expected(&state, x, y);
                    if is_covered {
                        covered += 1;
                    }
                    for channel in 0..4 {
                        max_deviation =
                            max_deviation.max((engine[channel] - expected[channel]).abs());
                    }
                }
            }
        }
        println!(
            "wave kernel through a 2x letterboxed transform: max deviation {max_deviation:.5} \
             ({covered} covered pixels)"
        );
        assert!(covered > 0, "the run must cover part of the image");
        assert!(
            max_deviation <= 1.0 / 255.0,
            "max deviation {max_deviation} exceeds one 8-bit step"
        );
    }

    /// The strip is the reference's `TVPFillARGB` written into the destination
    /// bitmap: a transparent background shows the scene beneath the layer, an
    /// opaque one replaces it.
    #[test]
    fn wave_strip_composites_the_background_over_the_under_face() {
        // `progress = 0.5` puts the amplitude at its peak, so the strip is at
        // its widest and both cases are reachable.
        let transparent = frame(0.5, 1000.0, 50.0, 0.2, 0.0, 0x0000_0000, 0x0000_0000);
        let state = transparent.state();
        assert_eq!(state.bg_color, [0.0, 0.0, 0.0, 0.0]);
        let mut saw_strip = false;
        for y in 0..HEIGHT {
            let shift = transparent.row_shift(&state, y as f32);
            let strip_column = if shift > 0.0 { Some(0) } else { None };
            let strip_column = strip_column.or((shift < 0.0).then_some(WIDTH - 1));
            let Some(x) = strip_column else {
                continue;
            };
            let pixel = engine_pixel(&transparent, x, y);
            assert_eq!(
                bytes(pixel),
                bytes(UNDER),
                "transparent background must show the under face at ({x}, {y})"
            );
            saw_strip = true;
        }
        assert!(saw_strip, "the parameter set must produce a visible strip");

        let opaque = frame(0.5, 1000.0, 50.0, 0.2, 0.0, 0xffff_0000, 0xffff_0000);
        let state = opaque.state();
        let shift = opaque.row_shift(&state, 0.0);
        let x = if shift < 0.0 { WIDTH - 1 } else { 0 };
        let pixel = engine_pixel(&opaque, x, 0);
        assert_eq!(bytes(pixel), [255, 0, 0, 255]);
    }

    /// A covered pixel lerps the two bitmaps the way `TVPConstAlphaBlend_SD`
    /// does, at the `BlendRatio` the clock has reached.
    #[test]
    fn wave_covered_pixel_lerps_the_shifted_bitmaps() {
        let setup = setup(1000, 50, 0.2, 0, 0, 0);
        let cur_time = 250;
        let frame = frame(0.25, 1000.0, 50.0, 0.2, 0.0, 0, 0);
        let state = reference::start_process(&setup, cur_time);
        let mut compared = 0usize;
        for y in 0..HEIGHT {
            let d = ((y as f64 * state.omega + state.rad_start).sin() * state.cur_h as f64) as i32;
            for x in 0..WIDTH {
                let source_x = x - d;
                if !(0..WIDTH).contains(&source_x) {
                    continue;
                }
                let pixel = engine_pixel(&frame, x, y);
                let expected = straight(reference::blend(
                    argb(dest_pixel(source_x, y)),
                    argb(source_pixel(source_x, y)),
                    state.blend_ratio,
                ));
                for channel in 0..4 {
                    assert!(
                        (pixel[channel] - expected[channel]).abs() <= 0.01,
                        "channel {channel} at ({x}, {y})"
                    );
                }
                compared += 1;
            }
        }
        assert!(compared > 100, "the parameter set must cover most of the image");
    }

    /// The constants the sync test checks against the shader text: the shader
    /// spells `3.14159265359` out, which is the same `f32` as `PI`.
    #[test]
    fn wave_constants_are_the_ones_the_shader_carries() {
        assert_eq!(WAVE_PI, std::f32::consts::PI);
        assert_eq!(WAVE_RATIO_STEPS, 255.0);
        assert_eq!(WAVE_RATIO_DIVISOR, 256.0);
    }
}
