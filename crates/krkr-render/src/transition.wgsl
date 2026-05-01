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
    progress: f32,
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

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>(input.position, 0.0, 1.0);
    output.tex_coord = input.tex_coord;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let old_color = textureSample(old_image, old_sampler, input.tex_coord);
    let new_color = textureSample(new_image, new_sampler, input.tex_coord);
    return mix(old_color, new_color, clamp(uniforms.progress, 0.0, 1.0));
}
