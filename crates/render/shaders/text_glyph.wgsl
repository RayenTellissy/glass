// Glyph quads from the atlas into a text coverage texture: fill coverage in red, outline
// coverage in green, their union in blue. Blending takes the maximum, so overlapping
// glyphs merge instead of adding up.

struct GlyphParams {
    // Target size in pixels, atlas size in pixels.
    canvas: vec2<f32>,
    atlas: vec2<f32>,
}

@group(0) @binding(0) var<uniform> params: GlyphParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var atlas: texture_2d<f32>;

struct Instance {
    // Quad top-left and size in canvas pixels, before the transform.
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    // Quad top-left in atlas pixels (the quad and atlas rectangles have the same size).
    @location(2) uv: vec2<f32>,
    @location(3) pivot: vec2<f32>,
    @location(4) offset: vec2<f32>,
    @location(5) scale: vec2<f32>,
    // rotation (radians, clockwise on screen), opacity, channel (0 fill, 1 outline).
    @location(6) rot_opacity_channel: vec3<f32>,
}

struct GlyphOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) weight: vec3<f32>,
}

@vertex
fn vs_glyph(@builtin(vertex_index) v: u32, inst: Instance) -> GlyphOut {
    // Two triangles: corners (0,0) (1,0) (0,1) (1,0) (1,1) (0,1).
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let corner = corners[v];
    let local = inst.pos + corner * inst.size - inst.pivot;
    let r = inst.rot_opacity_channel.x;
    let c = cos(r);
    let s = sin(r);
    let scaled = local * inst.scale;
    let p = inst.pivot + vec2<f32>(c * scaled.x - s * scaled.y, s * scaled.x + c * scaled.y) + inst.offset;
    var o: GlyphOut;
    o.pos = vec4<f32>(p.x / params.canvas.x * 2.0 - 1.0, 1.0 - p.y / params.canvas.y * 2.0, 0.0, 1.0);
    o.uv = (inst.uv + corner * inst.size) / params.atlas;
    let opacity = inst.rot_opacity_channel.y;
    if (inst.rot_opacity_channel.z < 0.5) {
        o.weight = vec3<f32>(opacity, 0.0, opacity);
    } else {
        o.weight = vec3<f32>(0.0, opacity, opacity);
    }
    return o;
}

@fragment
fn fs_glyph(in: GlyphOut) -> @location(0) vec4<f32> {
    let coverage = textureSampleLevel(atlas, samp, in.uv, 0.0).r;
    return vec4<f32>(in.weight * coverage, 0.0);
}
