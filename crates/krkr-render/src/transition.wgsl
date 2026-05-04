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
    data: array<vec4<f32>, 8>,
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
        let screen = viewport_size();
        let rule_dims = vec2<f32>(textureDimensions(rule_image));
        var rule_uv = uv * screen / max(rule_dims, vec2<f32>(1.0, 1.0));
        if (rule_dims.x < screen.x) {
            rule_uv.x = fract(rule_uv.x);
        }
        if (rule_dims.y < screen.y) {
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

fn transition_wave(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let screen = viewport_size();
    let pi = 3.14159265359;
    var envelope = sin(p * pi);
    let wave_type = uniforms.data[2].y;
    if (wave_type >= 1.5) {
        envelope = p;
    } else if (wave_type >= 0.5) {
        envelope = 1.0 - p;
    }
    let max_h = uniforms.data[2].z;
    let max_omega = uniforms.data[2].w;
    let offset = sin(uv.y * screen.y * max_omega + p * pi * 4.0) * envelope * max_h / screen.x;
    let bg = mix(uniforms.data[3], uniforms.data[4], p);
    let old_color = sample_old(uv + vec2<f32>(offset * (1.0 - p), 0.0), bg);
    let new_color = sample_new(uv - vec2<f32>(offset * p, 0.0), bg);
    return mix(old_color, new_color, p);
}

fn transition_mosaic(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let screen = viewport_size();
    let pi = 3.14159265359;
    let block = max(1.0, 1.0 + sin(p * pi) * uniforms.data[5].x);
    let block_uv = (floor(uv * screen / block) + vec2<f32>(0.5, 0.5)) * block / screen;
    return mix(textureSample(old_image, old_sampler, block_uv), textureSample(new_image, new_sampler, block_uv), p);
}

fn hash_tile(tile: vec2<f32>) -> f32 {
    return fract(sin(dot(tile, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

fn transition_turn(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let screen = viewport_size();
    let bg = uniforms.data[3];
    let tile_count = max(floor(screen / 64.0), vec2<f32>(1.0, 1.0));
    let tile = floor(uv * tile_count);
    let local = fract(uv * tile_count);
    let delay = hash_tile(tile) * 0.25;
    let t = clamp((p - delay) / max(1.0 - delay, 0.001), 0.0, 1.0);
    let width = max(abs(t - 0.5) * 2.0, 0.04);
    if (abs(local.x - 0.5) > width * 0.5) {
        return bg;
    }
    let corrected_local = vec2<f32>((local.x - 0.5) / width + 0.5, local.y);
    let sample_uv = (tile + corrected_local) / tile_count;
    if (t < 0.5) {
        return textureSample(old_image, old_sampler, sample_uv);
    }
    return textureSample(new_image, new_sampler, sample_uv);
}

fn rotate_uv(uv: vec2<f32>, center: vec2<f32>, scale: f32, angle: f32) -> vec2<f32> {
    let screen = viewport_size();
    let aspect_vec = vec2<f32>(screen.x / screen.y, 1.0);
    var p = (uv - center) * aspect_vec;
    let c = cos(-angle);
    let s = sin(-angle);
    p = vec2<f32>(p.x * c - p.y * s, p.x * s + p.y * c) / max(scale, 0.001);
    return p / aspect_vec + center;
}

fn transition_center(default_center: vec2<f32>) -> vec2<f32> {
    let screen = viewport_size();
    if (uniforms.data[6].y >= 0.0 && uniforms.data[6].z >= 0.0) {
        return uniforms.data[6].yz / screen;
    }
    return default_center;
}

fn transition_rotatezoom(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let pi = 3.14159265359;
    let center = transition_center(vec2<f32>(0.5, 0.5));
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
    let center = transition_center(vec2<f32>(0.5, 0.5));
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
    let old_uv = rotate_uv(uv, vec2<f32>(0.5, 0.5), mix(1.0, 0.25, p), twist * p);
    let new_uv = rotate_uv(uv, vec2<f32>(0.5, 0.5), mix(0.25, 1.0, p), twist * (p - 1.0));
    let old_color = sample_old(old_uv, bg);
    let new_color = sample_new(new_uv, bg);
    return mix(old_color, new_color, p);
}

fn transition_ripple(uv: vec2<f32>) -> vec4<f32> {
    let p = progress();
    let screen = viewport_size();
    let roundness = max(uniforms.data[7].x, 0.01);
    let aspect_vec = vec2<f32>(screen.x / screen.y / roundness, roundness);
    let center = transition_center(vec2<f32>(0.5, 0.5));
    let delta = (uv - center) * aspect_vec;
    let dist = length(delta);
    let corner = max(center, vec2<f32>(1.0, 1.0) - center) * aspect_vec;
    let max_dist = length(corner);
    let width = uniforms.data[6].w / min(screen.x, screen.y);
    let front = p * (max_dist + width * uniforms.data[7].y);
    let reveal = 1.0 - smoothstep(front - width, front + width, dist);
    let dir = normalize(delta + vec2<f32>(0.0001, 0.0)) / aspect_vec;
    let wave = sin((dist - front) * uniforms.data[7].y * 24.0);
    let envelope = exp(-abs(dist - front) / max(width * 3.0, 0.001)) * (1.0 - p);
    let drift = wave * envelope * uniforms.data[7].z / min(screen.x, screen.y);
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
