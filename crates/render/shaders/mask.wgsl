// Layer masks: shapes' coverage unioned, feathered by a separable Gaussian (at 1/2 or
// 1/4 resolution for wide feathers: averaged down, blurred, read back bilinearly),
// inverted, combined with the masks before it (union = max, intersection = min), then
// multiplied into the layer's premultiplied colour. The CPU reference is `render::mask`.

struct MaskParams {
    // Bilinear fetches per blurred pixel; 0 reads the source unblurred.
    fetches: u32,
    invert: u32,
    // 0 the first mask (nothing to combine with), 1 union, 2 intersection.
    combine: u32,
    // Layer pixels per blur texel: 1, 2 or 4.
    scale: u32,
    // 1 / the size of the texture read (fs_down, blurs), or of the blur texture times the
    // scale (fs_combine reading it back).
    texel: vec2<f32>,
    // The feather's Gaussian taps paired into fetches, two per entry: offset in pixels
    // (between the pair's texels, by their weights) and the pair's normalised weight.
    taps: array<vec4<f32>, 128>,
}

@group(0) @binding(0) var<uniform> params: MaskParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src: texture_2d<f32>;
@group(0) @binding(3) var other: texture_2d<f32>;

// The sampler clamps to the edge, so coverage beyond the layer repeats its border.
fn blur(p: vec2<f32>, dir: vec2<f32>) -> f32 {
    var sum = 0.0;
    for (var i = 0u; i < params.fetches; i = i + 1u) {
        let entry = params.taps[i / 2u];
        let tap = select(entry.xy, entry.zw, (i & 1u) == 1u);
        sum = sum + tap.y * textureSampleLevel(src, samp, (p + dir * tap.x) * params.texel, 0.0).r;
    }
    return sum;
}

// The larger of two coverages: shapes of one mask.
@fragment
fn fs_union(in: VsOut) -> @location(0) vec4<f32> {
    let pos = vec2<i32>(in.pos.xy);
    let v = max(textureLoad(src, pos, 0).r, textureLoad(other, pos, 0).r);
    return vec4<f32>(v, 0.0, 0.0, 1.0);
}

@fragment
fn fs_blur_h(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(blur(in.pos.xy, vec2<f32>(1.0, 0.0)), 0.0, 0.0, 1.0);
}

@fragment
fn fs_blur_v(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(blur(in.pos.xy, vec2<f32>(0.0, 1.0)), 0.0, 0.0, 1.0);
}

// The average of the scale x scale layer pixels under this blur texel, from bilinear
// fetches between groups of 2 x 2.
@fragment
fn fs_down(in: VsOut) -> @location(0) vec4<f32> {
    let n = params.scale / 2u;
    let origin = (in.pos.xy - 0.5) * f32(params.scale);
    var sum = 0.0;
    for (var j = 0u; j < n; j = j + 1u) {
        for (var k = 0u; k < n; k = k + 1u) {
            let at = origin + vec2<f32>(f32(2u * j + 1u), f32(2u * k + 1u));
            sum = sum + textureSampleLevel(src, samp, at * params.texel, 0.0).r;
        }
    }
    return vec4<f32>(sum / f32(n * n), 0.0, 0.0, 1.0);
}

// The vertical blur (or the source as it is, or the low-resolution blur read back),
// inverted if asked, combined with the masks so far in `other`.
@fragment
fn fs_combine(in: VsOut) -> @location(0) vec4<f32> {
    let pos = vec2<i32>(in.pos.xy);
    var v = 0.0;
    if (params.scale > 1u) {
        v = textureSampleLevel(src, samp, in.pos.xy * params.texel, 0.0).r;
    } else if (params.fetches > 0u) {
        v = blur(in.pos.xy, vec2<f32>(0.0, 1.0));
    } else {
        v = textureLoad(src, pos, 0).r;
    }
    v = clamp(v, 0.0, 1.0);
    if (params.invert != 0u) {
        v = 1.0 - v;
    }
    switch params.combine {
        case 1u: { v = max(textureLoad(other, pos, 0).r, v); }
        case 2u: { v = min(textureLoad(other, pos, 0).r, v); }
        default: {}
    }
    return vec4<f32>(v, 0.0, 0.0, 1.0);
}

// The layer (premultiplied) scaled by the mask.
@fragment
fn fs_apply(in: VsOut) -> @location(0) vec4<f32> {
    let pos = vec2<i32>(in.pos.xy);
    return textureLoad(src, pos, 0) * textureLoad(other, pos, 0).r;
}

// No masks: the layer as it is.
@fragment
fn fs_copy(in: VsOut) -> @location(0) vec4<f32> {
    return textureLoad(src, vec2<i32>(in.pos.xy), 0);
}
