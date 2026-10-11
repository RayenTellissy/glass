// Linear RGB from one set of primaries to another: a 3x3 matrix on premultiplied
// linear light (alpha untouched).

struct GamutParams {
    r: vec4<f32>,
    g: vec4<f32>,
    b: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: GamutParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src: texture_2d<f32>;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(in.pos.xy), 0);
    return vec4<f32>(dot(params.r.xyz, c.rgb), dot(params.g.xyz, c.rgb), dot(params.b.xyz, c.rgb), c.a);
}
