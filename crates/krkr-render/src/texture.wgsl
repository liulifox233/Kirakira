struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) tint: vec4<f32>,
    @location(3) force_opaque: f32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) tint: vec4<f32>,
    @location(2) force_opaque: f32,
};

@group(0) @binding(0)
var image: texture_2d<f32>;

@group(0) @binding(1)
var image_sampler: sampler;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>(input.position, 0.0, 1.0);
    output.tex_coord = input.tex_coord;
    output.tint = input.tint;
    output.force_opaque = input.force_opaque;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    var color = textureSample(image, image_sampler, input.tex_coord) * input.tint;
    // Official `TVPCopyOpaqueImage` (`color_opaque_functor`): `0xff000000 | src`.
    // Layer opacity still comes from the tint alpha.
    if (input.force_opaque > 0.5) {
        color.a = input.tint.a;
    }
    return color;
}
