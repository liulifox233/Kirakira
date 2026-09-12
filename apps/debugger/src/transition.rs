//! CPU mirror of `crates/krkr-render/src/transition.wgsl`.
//!
//! A frame taken while a transition runs is not the sum of its layer draw
//! lists: the windowed shell renders the live tree, then each
//! `FrameOutput::transitions` entry composites its `frozen` / `under` /
//! `source` faces through that method's kernel inside the destination
//! rectangle (`krkr-render::render_transition_to_view`).  A headless `shot`
//! built from `draw_commands` alone therefore showed the incoming face at its
//! pre-transition state and could not verify a transition visually.
//!
//! This module transcribes the shader's kernels and its uniform layout so
//! `snapshot::composite_frame_output` can run the same composite on the CPU.
//! `transition.wgsl` is the source of truth: the function names, the uniform
//! slots and the arithmetic follow it, and a change there has to be mirrored
//! here (the renderer already keeps the `wave` kernel honest with a CPU mirror
//! of its own, `krkr-render/src/wave.rs`; this port covers every kernel).
//!
//! Deviations the software path keeps, all shared with the rest of
//! `krkr-debug`'s compositor:
//!
//! * face textures are tapped with a bilinear clamp-to-edge kernel, matching
//!   the window's `Linear` sampler, but the tap happens on sRGB bytes where
//!   the GPU mixes in linear space;
//! * the frame is composited at its logical size, so the uniform transform is
//!   identity (scale 1, no letterbox offset) and the destination rectangle
//!   maps to whole pixels the way `physical_rect` floors and ceils it;
//! * `DrawCommand::Text` is not rasterised (the software compositor has no
//!   text path).

use krkr_core::{FrameTransition, TransitionMethod};

/// The `transition.wgsl` uniform block, slot by slot.
///
/// The accessors below are the shader's (`progress()`, `viewport_size()`,
/// `image_rect()`, ...), so the kernel bodies can be read against the WGSL
/// side by side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Uniforms {
    pub data: [[f32; 4]; 12],
}

impl Uniforms {
    /// Builds the block for one transition the way the renderer's
    /// `transition_uniforms` does at a 1:1 transform (the debugger's frame is
    /// its content size, so there is no letterbox offset and no scaling).
    pub fn for_transition(
        transition: &FrameTransition,
        width: f32,
        height: f32,
        under_available: bool,
    ) -> Self {
        let params = &transition.params;
        // `Turn` and `RotateSwap` are the kernels that read `bgcolor`
        // (`transition_uniforms` in krkr-render, the same selection).
        let primary_bg_color = if matches!(
            params.method,
            TransitionMethod::Turn | TransitionMethod::RotateSwap
        ) {
            params.bg_color
        } else {
            params.bg_color1
        };
        // `tTVPDivisibleData::Dest` (`LayerIntf.cpp:6513-6540`) in logical
        // frame pixels; without measurable geometry the composite covers the
        // whole frame, which is the content size here.
        let image_rect = transition
            .dest_rect
            .filter(|rect| rect.width > 0.0 && rect.height > 0.0)
            .map(|rect| [rect.x, rect.y, rect.width, rect.height])
            .unwrap_or([0.0, 0.0, width.max(1.0), height.max(1.0)]);
        Self {
            data: [
                [
                    transition.progress.clamp(0.0, 1.0),
                    params.method.as_code(),
                    if transition.rule_texture_id.is_some() {
                        1.0
                    } else {
                        0.0
                    },
                    0.0,
                ],
                [
                    width.max(1.0),
                    height.max(1.0),
                    params.vague.max(0.0),
                    params.scroll_from as u8 as f32,
                ],
                [
                    params.scroll_stay as u8 as f32,
                    params.wave_type,
                    params.max_h.max(0.0),
                    params.max_omega.max(0.0),
                ],
                color_uniform(primary_bg_color),
                color_uniform(params.bg_color2),
                [
                    params.max_size.max(1.0),
                    params.factor.max(0.0),
                    params.accel,
                    params.twist,
                ],
                [
                    params.twist_accel,
                    params.center_x,
                    params.center_y,
                    params.ripple_width.max(1.0),
                ],
                [
                    params.roundness.max(0.01),
                    params.speed.max(0.01),
                    params.max_drift.max(0.0),
                    0.0,
                ],
                image_rect,
                [1.0, 0.0, 0.0, if under_available { 1.0 } else { 0.0 }],
                [params.duration_millis.max(0.0), 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
            ],
        }
    }

    fn progress(&self) -> f32 {
        self.data[0][0].clamp(0.0, 1.0)
    }

    fn viewport_size(&self) -> [f32; 2] {
        [self.data[1][0].max(1.0), self.data[1][1].max(1.0)]
    }

    fn image_rect(&self) -> [f32; 4] {
        self.data[8]
    }

    fn transform_scale(&self) -> f32 {
        self.data[9][0].max(1.0e-6)
    }

    fn under_available(&self) -> bool {
        self.data[9][3] >= 0.5
    }

    fn duration_millis(&self) -> f32 {
        self.data[10][0].max(0.0)
    }
}

fn color_uniform(color: krkr_core::Color) -> [f32; 4] {
    [color.r, color.g, color.b, color.a]
}

/// One face texture: the RGBA bytes the renderer's offscreen pass produced
/// for a `FrameTransition` face, straight alpha.
#[derive(Clone, Copy)]
pub struct Face<'a> {
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
}

impl<'a> Face<'a> {
    pub const EMPTY: Face<'static> = Face {
        pixels: &[],
        width: 0,
        height: 0,
    };

    pub fn new(pixels: &'a [u8], width: u32, height: u32) -> Self {
        Self {
            pixels,
            width,
            height,
        }
    }

    /// `textureSample` through the window's sampler: bilinear, clamp-to-edge,
    /// uv in frame space.
    pub fn sample(&self, uv: [f32; 2]) -> [f32; 4] {
        if self.width == 0 || self.height == 0 {
            return [0.0; 4];
        }
        let x = uv[0] * self.width as f32 - 0.5;
        let y = uv[1] * self.height as f32 - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = x - x0;
        let fy = y - y0;
        let x0 = self.texel_index(x0, self.width);
        let x1 = self.texel_index(x0 as f32 + 1.0, self.width);
        let y0i = self.texel_index(y0, self.height);
        let y1 = self.texel_index(y0 + 1.0, self.height);
        let top = mix4(self.texel(x0, y0i), self.texel(x1, y0i), fx);
        let bottom = mix4(self.texel(x0, y1), self.texel(x1, y1), fx);
        mix4(top, bottom, fy)
    }

    fn texel_index(&self, index: f32, size: u32) -> u32 {
        index.max(0.0).min(size as f32 - 1.0) as u32
    }

    fn texel(&self, x: u32, y: u32) -> [f32; 4] {
        let index = ((y * self.width + x) * 4) as usize;
        let pixel = &self.pixels[index..index + 4];
        [
            pixel[0] as f32 / 255.0,
            pixel[1] as f32 / 255.0,
            pixel[2] as f32 / 255.0,
            pixel[3] as f32 / 255.0,
        ]
    }
}

/// The four binds one kernel composite reads (`TransitionFaceViews` plus the
/// rule texture).  Sampling a face outside its bounds is the caller's job:
/// the shader binds the old face where the rule is absent and the incoming
/// face where the under pass was not rendered.
#[derive(Clone, Copy)]
pub struct Faces<'a> {
    pub old: Face<'a>,
    pub new: Face<'a>,
    pub under: Face<'a>,
    pub rule: Face<'a>,
}

/// `fs_main`: one composite pixel through the method's kernel.
///
/// `uv` is frame space with the origin at the top-left, `[0, 1]` across the
/// frame, sampled at pixel centres by the caller.
pub fn kernel_pixel(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let method = uniforms.data[0][1];
    if method < 0.5 {
        transition_crossfade(uniforms, uv, faces)
    } else if method < 1.5 {
        transition_universal(uniforms, uv, faces)
    } else if method < 2.5 {
        transition_scroll(uniforms, uv, faces)
    } else if method < 3.5 {
        transition_wave(uniforms, uv, faces)
    } else if method < 4.5 {
        transition_mosaic(uniforms, uv, faces)
    } else if method < 5.5 {
        transition_turn(uniforms, uv, faces)
    } else if method < 6.5 {
        transition_rotatezoom(uniforms, uv, faces)
    } else if method < 7.5 {
        transition_rotatevanish(uniforms, uv, faces)
    } else if method < 8.5 {
        transition_rotateswap(uniforms, uv, faces)
    } else {
        transition_ripple(uniforms, uv, faces)
    }
}

fn in_bounds(uv: [f32; 2]) -> bool {
    uv[0] >= 0.0 && uv[1] >= 0.0 && uv[0] <= 1.0 && uv[1] <= 1.0
}

fn sample_old(faces: &Faces<'_>, uv: [f32; 2], bg: [f32; 4]) -> [f32; 4] {
    if !in_bounds(uv) {
        return bg;
    }
    faces.old.sample(uv)
}

fn sample_new(faces: &Faces<'_>, uv: [f32; 2], bg: [f32; 4]) -> [f32; 4] {
    if !in_bounds(uv) {
        return bg;
    }
    faces.new.sample(uv)
}

fn acceleration(t: f32, accel: f32) -> f32 {
    let x = t.clamp(0.0, 1.0);
    if accel >= 0.01 {
        return x.powf(accel);
    }
    if accel <= -0.01 {
        return 1.0 - (1.0 - x).powf(-accel);
    }
    x
}

fn scroll_direction(origin: f32) -> [f32; 2] {
    if origin >= 2.5 {
        return [0.0, 1.0];
    }
    if origin >= 1.5 {
        return [1.0, 0.0];
    }
    if origin >= 0.5 {
        return [0.0, -1.0];
    }
    [-1.0, 0.0]
}

fn transition_crossfade(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    mix4(
        faces.old.sample(uv),
        faces.new.sample(uv),
        uniforms.progress(),
    )
}

fn transition_universal(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let old_color = faces.old.sample(uv);
    let new_color = faces.new.sample(uv);
    let mut rule_value = uv[0];
    if uniforms.data[0][2] > 0.5 {
        let screen = uniforms.viewport_size();
        let rule_dims = [
            faces.rule.width.max(1) as f32,
            faces.rule.height.max(1) as f32,
        ];
        let mut rule_uv = [
            uv[0] * screen[0] / rule_dims[0],
            uv[1] * screen[1] / rule_dims[1],
        ];
        if rule_dims[0] < screen[0] {
            rule_uv[0] = rule_uv[0].fract();
        }
        if rule_dims[1] < screen[1] {
            rule_uv[1] = rule_uv[1].fract();
        }
        rule_uv = [rule_uv[0].clamp(0.0, 0.9999), rule_uv[1].clamp(0.0, 0.9999)];
        let rule_color = faces.rule.sample(rule_uv);
        rule_value = rule_color[0] * 0.299 + rule_color[1] * 0.587 + rule_color[2] * 0.114;
    }
    let vague = (uniforms.data[1][2] / 255.0).max(1.0 / 255.0);
    let phase = uniforms.progress() * (1.0 + vague);
    let amount = ((phase - rule_value) / vague).clamp(0.0, 1.0);
    mix4(old_color, new_color, amount)
}

fn transition_scroll(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let p = uniforms.progress();
    let dir = scroll_direction(uniforms.data[1][3]);
    let stay = uniforms.data[2][0];
    let new_disp = [dir[0] * (1.0 - p), dir[1] * (1.0 - p)];
    let old_disp = [-dir[0] * p, -dir[1] * p];
    let new_uv = [uv[0] - new_disp[0], uv[1] - new_disp[1]];
    let old_uv = [uv[0] - old_disp[0], uv[1] - old_disp[1]];
    let transparent = [0.0; 4];

    if stay >= 1.5 {
        if in_bounds(old_uv) {
            return sample_old(faces, old_uv, transparent);
        }
        return faces.new.sample(uv);
    }

    if stay >= 0.5 {
        if in_bounds(new_uv) {
            return sample_new(faces, new_uv, transparent);
        }
        return faces.old.sample(uv);
    }

    if in_bounds(new_uv) {
        return sample_new(faces, new_uv, transparent);
    }
    if in_bounds(old_uv) {
        return sample_old(faces, old_uv, transparent);
    }
    transparent
}

/// The transition handler's clock: `0` means the caller supplied no duration
/// and the phase comes from `progress` alone (`transition.wgsl`).
fn wave_blend_ratio(timed: bool, cur_time: f32, total: f32, p: f32) -> f32 {
    if !timed {
        return p;
    }
    (cur_time * 255.0 / total).floor() / 256.0
}

/// `wave` (`extrans/wave.cpp:16-265`): raster scroll with an animated
/// background strip.  The kernel's deviations are recorded at
/// `transition.wgsl::transition_wave` (millisecond clock rebuilt from
/// `progress` and `duration_millis`, float form of the integer blend, the
/// scene beneath read at the shifted position).
fn transition_wave(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let p = uniforms.progress();
    let frame = uniforms.viewport_size();
    let image = uniforms.image_rect();
    let scale = uniforms.transform_scale();
    let origin = [uniforms.data[9][1], uniforms.data[9][2]];

    let local = [
        (uv[0] * frame[0] - origin[0]) / scale - image[0],
        (uv[1] * frame[1] - origin[1]) / scale - image[1],
    ];

    let time = uniforms.duration_millis();
    let timed = time > 0.0;
    let total = if timed { time.max(2.0) } else { 1.0 };
    let cur_time = if timed { p * total } else { p };
    let half = if timed { (total * 0.5).floor() } else { 0.5 };
    let mut t = cur_time.clamp(0.0, total);
    if t >= half {
        t = total - t;
    }
    let ramp = (std::f32::consts::PI * 0.5 * t / half).sin();
    let cur_h = (ramp * uniforms.data[2][2]).trunc();
    let wave_type = uniforms.data[2][1].trunc();
    let max_omega = uniforms.data[2][3];
    let mut omega = max_omega * ramp;
    if wave_type == 1.0 {
        omega = max_omega * (total - cur_time) / total;
    }
    if wave_type == 2.0 {
        omega = max_omega * cur_time / total;
    }
    let rad = local[1].floor() * omega - omega * (image[3] * 0.5).floor();
    let shift = (rad.sin() * cur_h).trunc();
    let ratio = wave_blend_ratio(timed, cur_time, total, p);

    let source_x = local[0] - shift;
    if source_x < 0.0 || source_x >= image[2] {
        let bg = mix4(uniforms.data[3], uniforms.data[4], ratio);
        if !uniforms.under_available() {
            return bg;
        }
        let under = faces.under.sample(uv);
        return [
            bg[0] * bg[3] + under[0] * (1.0 - bg[3]),
            bg[1] * bg[3] + under[1] * (1.0 - bg[3]),
            bg[2] * bg[3] + under[2] * (1.0 - bg[3]),
            bg[3] + under[3] * (1.0 - bg[3]),
        ];
    }

    let shifted = [uv[0] - shift * scale / frame[0], uv[1]];
    mix4(faces.old.sample(shifted), faces.new.sample(shifted), ratio)
}

fn transition_mosaic(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let p = uniforms.progress();
    let screen = uniforms.viewport_size();
    let block = (1.0 + (p * std::f32::consts::PI).sin() * uniforms.data[5][0]).max(1.0);
    let block_uv = [
        ((uv[0] * screen[0] / block).floor() + 0.5) * block / screen[0],
        ((uv[1] * screen[1] / block).floor() + 0.5) * block / screen[1],
    ];
    mix4(faces.old.sample(block_uv), faces.new.sample(block_uv), p)
}

fn hash_tile(tile: [f32; 2]) -> f32 {
    // The shader's `43758.5453`; `43_758.547` is that literal's nearest `f32`.
    ((tile[0] * 12.9898 + tile[1] * 78.233).sin() * 43_758.547).fract()
}

fn transition_turn(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let p = uniforms.progress();
    let screen = uniforms.viewport_size();
    let bg = uniforms.data[3];
    let tile_count = [
        (screen[0] / 64.0).floor().max(1.0),
        (screen[1] / 64.0).floor().max(1.0),
    ];
    let tile = [
        (uv[0] * tile_count[0]).floor(),
        (uv[1] * tile_count[1]).floor(),
    ];
    let local = [
        uv[0] * tile_count[0] - tile[0],
        uv[1] * tile_count[1] - tile[1],
    ];
    let delay = hash_tile(tile) * 0.25;
    let t = ((p - delay) / (1.0 - delay).max(0.001)).clamp(0.0, 1.0);
    let width = ((t - 0.5).abs() * 2.0).max(0.04);
    if (local[0] - 0.5).abs() > width * 0.5 {
        return bg;
    }
    let corrected_local = [(local[0] - 0.5) / width + 0.5, local[1]];
    let sample_uv = [
        (tile[0] + corrected_local[0]) / tile_count[0],
        (tile[1] + corrected_local[1]) / tile_count[1],
    ];
    if t < 0.5 {
        return faces.old.sample(sample_uv);
    }
    faces.new.sample(sample_uv)
}

fn rotate_uv(
    uniforms: &Uniforms,
    uv: [f32; 2],
    center: [f32; 2],
    scale: f32,
    angle: f32,
) -> [f32; 2] {
    let screen = uniforms.viewport_size();
    let aspect_vec = [screen[0] / screen[1], 1.0];
    let mut p = [
        (uv[0] - center[0]) * aspect_vec[0],
        (uv[1] - center[1]) * aspect_vec[1],
    ];
    let c = (-angle).cos();
    let s = (-angle).sin();
    p = [p[0] * c - p[1] * s, p[0] * s + p[1] * c];
    let scale = scale.max(0.001);
    p = [p[0] / scale, p[1] / scale];
    [
        p[0] / aspect_vec[0] + center[0],
        p[1] / aspect_vec[1] + center[1],
    ]
}

fn transition_center(uniforms: &Uniforms, default_center: [f32; 2]) -> [f32; 2] {
    let screen = uniforms.viewport_size();
    if uniforms.data[6][1] >= 0.0 && uniforms.data[6][2] >= 0.0 {
        return [
            uniforms.data[6][1] / screen[0],
            uniforms.data[6][2] / screen[1],
        ];
    }
    default_center
}

fn transition_rotatezoom(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let p = uniforms.progress();
    let center = transition_center(uniforms, [0.5, 0.5]);
    let scale_t = acceleration(p, uniforms.data[5][2]);
    let twist_t = acceleration(p, uniforms.data[6][0]);
    let scale = mix(uniforms.data[5][1].max(0.001), 1.0, scale_t);
    let angle = uniforms.data[5][3] * std::f32::consts::PI * 2.0 * (1.0 - twist_t);
    let sample_uv = rotate_uv(uniforms, uv, center, scale, angle);
    let old_color = faces.old.sample(uv);
    if !in_bounds(sample_uv) {
        return old_color;
    }
    let new_color = faces.new.sample(sample_uv);
    mix4(old_color, new_color, p)
}

fn transition_rotatevanish(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let p = uniforms.progress();
    let center = transition_center(uniforms, [0.5, 0.5]);
    let scale_t = acceleration(p, uniforms.data[5][2]);
    let twist_t = acceleration(p, uniforms.data[6][0]);
    let scale = (1.0 - scale_t).max(0.001);
    let angle = uniforms.data[5][3] * std::f32::consts::PI * 2.0 * twist_t;
    let sample_uv = rotate_uv(uniforms, uv, center, scale, angle);
    let new_color = faces.new.sample(uv);
    if !in_bounds(sample_uv) {
        return new_color;
    }
    let old_color = faces.old.sample(sample_uv);
    mix4(old_color, new_color, p)
}

fn transition_rotateswap(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let p = uniforms.progress();
    let twist = uniforms.data[5][3] * std::f32::consts::PI * 2.0;
    let bg = uniforms.data[3];
    let old_uv = rotate_uv(uniforms, uv, [0.5, 0.5], mix(1.0, 0.25, p), twist * p);
    let new_uv = rotate_uv(
        uniforms,
        uv,
        [0.5, 0.5],
        mix(0.25, 1.0, p),
        twist * (p - 1.0),
    );
    let old_color = sample_old(faces, old_uv, bg);
    let new_color = sample_new(faces, new_uv, bg);
    mix4(old_color, new_color, p)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn transition_ripple(uniforms: &Uniforms, uv: [f32; 2], faces: &Faces<'_>) -> [f32; 4] {
    let p = uniforms.progress();
    let screen = uniforms.viewport_size();
    let roundness = uniforms.data[7][0].max(0.01);
    let aspect_vec = [screen[0] / screen[1] / roundness, roundness];
    let center = transition_center(uniforms, [0.5, 0.5]);
    let delta = [
        (uv[0] - center[0]) * aspect_vec[0],
        (uv[1] - center[1]) * aspect_vec[1],
    ];
    let dist = (delta[0] * delta[0] + delta[1] * delta[1]).sqrt();
    let corner = [
        center[0].max(1.0 - center[0]) * aspect_vec[0],
        center[1].max(1.0 - center[1]) * aspect_vec[1],
    ];
    let max_dist = (corner[0] * corner[0] + corner[1] * corner[1]).sqrt();
    let width = uniforms.data[6][3] / screen[0].min(screen[1]);
    let front = p * (max_dist + width * uniforms.data[7][1]);
    let reveal = 1.0 - smoothstep(front - width, front + width, dist);
    let dir = {
        let length = ((delta[0] + 0.0001) * (delta[0] + 0.0001) + delta[1] * delta[1]).sqrt();
        [
            (delta[0] + 0.0001) / length / aspect_vec[0],
            delta[1] / length / aspect_vec[1],
        ]
    };
    let wave = ((dist - front) * uniforms.data[7][1] * 24.0).sin();
    let envelope = (-(dist - front).abs() / (width * 3.0).max(0.001)).exp() * (1.0 - p);
    let drift = wave * envelope * uniforms.data[7][2] / screen[0].min(screen[1]);
    let sample_uv = [uv[0] + dir[0] * drift, uv[1] + dir[1] * drift];
    let old_color = sample_old(faces, sample_uv, faces.old.sample(uv));
    let new_color = sample_new(faces, sample_uv, faces.new.sample(uv));
    mix4(old_color, new_color, reveal)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn mix4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        mix(a[0], b[0], t),
        mix(a[1], b[1], t),
        mix(a[2], b[2], t),
        mix(a[3], b[3], t),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use krkr_core::{Color, FrameTransition, TransitionParams};

    const WIDTH: u32 = 2;
    const HEIGHT: u32 = 2;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter()
            .cycle()
            .take((width * height * 4) as usize)
            .copied()
            .collect()
    }

    fn transition(
        method: TransitionMethod,
        progress: f32,
        duration_millis: f32,
    ) -> FrameTransition {
        FrameTransition {
            method: method.as_name().to_string(),
            progress,
            params: TransitionParams {
                method,
                duration_millis,
                ..TransitionParams::default()
            },
            dest_rect: None,
            rule_texture_id: None,
            rule_image_upload: None,
            frozen_draw_commands: Vec::new(),
            frozen_image_uploads: Vec::new(),
            under_draw_commands: Vec::new(),
            under_image_uploads: Vec::new(),
            source_draw_commands: Vec::new(),
            source_image_uploads: Vec::new(),
        }
    }

    fn bytes(color: [f32; 4]) -> [u8; 4] {
        [
            (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            (color[3].clamp(0.0, 1.0) * 255.0).round() as u8,
        ]
    }

    /// The crossfade kernel is the reference's `TVPConstAlphaBlend_SD`: a
    /// per-channel lerp of the two faces including alpha.
    #[test]
    fn crossfade_lerps_the_faces_at_the_progress() {
        let old = solid(WIDTH, HEIGHT, [200, 40, 0, 255]);
        let new = solid(WIDTH, HEIGHT, [0, 60, 220, 255]);
        let faces = Faces {
            old: Face::new(&old, WIDTH, HEIGHT),
            new: Face::new(&new, WIDTH, HEIGHT),
            under: Face::EMPTY,
            rule: Face::EMPTY,
        };
        for (progress, expected) in [
            (0.0, [200, 40, 0, 255]),
            (0.5, [100, 50, 110, 255]),
            (1.0, [0, 60, 220, 255]),
        ] {
            let uniforms = Uniforms::for_transition(
                &transition(TransitionMethod::Crossfade, progress, 1000.0),
                WIDTH as f32,
                HEIGHT as f32,
                false,
            );
            for uv in [[0.25, 0.25], [0.75, 0.75]] {
                let pixel = kernel_pixel(&uniforms, uv, &faces);
                assert_eq!(bytes(pixel), expected, "progress {progress} at {uv:?}");
            }
        }
    }

    /// The millisecond clock reaches the kernels through uniform slot 10:
    /// `wave` at 0 / 500 / 1000 ms of a 1000 ms transition must move along its
    /// `BlendRatio` ramp instead of staying at the progress-0 frame.  `maxh`
    /// is zero so no row shifts out of its covered span and the sampled pixel
    /// is the lerp alone.
    #[test]
    fn wave_kernel_walks_the_millisecond_clock() {
        let old = solid(WIDTH, HEIGHT, [255, 0, 0, 255]);
        let new = solid(WIDTH, HEIGHT, [0, 0, 255, 255]);
        let faces = Faces {
            old: Face::new(&old, WIDTH, HEIGHT),
            new: Face::new(&new, WIDTH, HEIGHT),
            under: Face::EMPTY,
            rule: Face::EMPTY,
        };
        let mut seen = Vec::new();
        for (progress, millis) in [(0.0, 0.0), (0.5, 500.0), (1.0, 1000.0)] {
            let mut frame_transition = transition(TransitionMethod::Wave, progress, 1000.0);
            frame_transition.params.max_h = 0.0;
            let uniforms =
                Uniforms::for_transition(&frame_transition, WIDTH as f32, HEIGHT as f32, false);
            assert_eq!(uniforms.duration_millis(), 1000.0);
            let pixel = kernel_pixel(&uniforms, [0.25, 0.25], &faces);
            assert_eq!(
                pixel.iter().map(|v| v.is_finite()).collect::<Vec<_>>(),
                vec![true; 4],
                "{millis} ms must stay finite"
            );
            seen.push(bytes(pixel));
        }
        assert_eq!(seen[0], [255, 0, 0, 255], "0 ms is the frozen face");
        // `BlendRatio = CurTime * 255 / Time` steps in 255ths and `Blend`
        // scales by 256ths (`common.h:18-31`), so 500 ms of 1000 ms is
        // `mix(red, blue, 127/256)` and the completed run lands one step short
        // of the incoming face (`mix(red, blue, 255/256)`).
        assert_eq!(
            seen[1],
            [128, 0, 127, 255],
            "500 ms is mix(red, blue, 127/256)"
        );
        assert_eq!(seen[2], [1, 0, 254, 255], "1000 ms is one blend step short");
    }

    /// `wave` (`extrans/wave.cpp:203-221`): where a row's shift leaves the
    /// covered span, the kernel writes `bgcolor` over the scene beneath the
    /// destination layer instead of blending the two faces.
    #[test]
    fn wave_strip_composites_the_background_over_the_under_face() {
        let old = solid(WIDTH, HEIGHT, [255, 0, 0, 255]);
        let new = solid(WIDTH, HEIGHT, [0, 0, 255, 255]);
        let under = solid(WIDTH, HEIGHT, [0, 255, 0, 255]);
        let faces = Faces {
            old: Face::new(&old, WIDTH, HEIGHT),
            new: Face::new(&new, WIDTH, HEIGHT),
            under: Face::new(&under, WIDTH, HEIGHT),
            rule: Face::EMPTY,
        };
        // At progress 0.5 the default `maxh` (50) is at its peak, so row 0's
        // shift moves further than the whole 2-pixel image: the pixel is the
        // (transparent) `bgcolor` over the under face.
        let frame_transition = transition(TransitionMethod::Wave, 0.5, 1000.0);
        let uniforms =
            Uniforms::for_transition(&frame_transition, WIDTH as f32, HEIGHT as f32, true);
        assert_eq!(
            bytes(kernel_pixel(&uniforms, [0.25, 0.25], &faces)),
            [0, 255, 0, 255]
        );

        // An opaque background replaces the under face there.
        let mut opaque = frame_transition.clone();
        opaque.params.bg_color1 = Color::new(1.0, 0.0, 0.0, 1.0);
        opaque.params.bg_color2 = Color::new(1.0, 0.0, 0.0, 1.0);
        let uniforms = Uniforms::for_transition(&opaque, WIDTH as f32, HEIGHT as f32, true);
        assert_eq!(
            bytes(kernel_pixel(&uniforms, [0.25, 0.25], &faces)),
            [255, 0, 0, 255]
        );
    }

    /// Every method's kernel must produce finite, in-range bytes: the
    /// dispatch table is exhaustive and no geometry divides by zero at the
    /// degenerate (2x2, progress 0/0.5/1) end of the range.
    #[test]
    fn every_method_kernel_stays_finite() {
        let old = solid(WIDTH, HEIGHT, [200, 40, 0, 255]);
        let new = solid(WIDTH, HEIGHT, [0, 60, 220, 255]);
        let faces = Faces {
            old: Face::new(&old, WIDTH, HEIGHT),
            new: Face::new(&new, WIDTH, HEIGHT),
            under: Face::new(&old, WIDTH, HEIGHT),
            rule: Face::new(&old, WIDTH, HEIGHT),
        };
        for method in [
            TransitionMethod::Crossfade,
            TransitionMethod::Universal,
            TransitionMethod::Scroll,
            TransitionMethod::Wave,
            TransitionMethod::Mosaic,
            TransitionMethod::Turn,
            TransitionMethod::RotateZoom,
            TransitionMethod::RotateVanish,
            TransitionMethod::RotateSwap,
            TransitionMethod::Ripple,
        ] {
            for progress in [0.0, 0.5, 1.0] {
                let uniforms = Uniforms::for_transition(
                    &transition(method, progress, 1000.0),
                    WIDTH as f32,
                    HEIGHT as f32,
                    true,
                );
                for y in 0..HEIGHT {
                    for x in 0..WIDTH {
                        let uv = [
                            (x as f32 + 0.5) / WIDTH as f32,
                            (y as f32 + 0.5) / HEIGHT as f32,
                        ];
                        let pixel = kernel_pixel(&uniforms, uv, &faces);
                        for channel in pixel {
                            assert!(
                                channel.is_finite(),
                                "{} progress {progress} at ({x}, {y})",
                                method.as_name()
                            );
                        }
                    }
                }
            }
        }
    }
}
