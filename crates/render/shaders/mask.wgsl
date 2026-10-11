// Layer masks: shapes' coverage unioned, feathered by a separable Gaussian, inverted,
// combined with the masks before it (union = max, intersection = min), then multiplied
// into the layer's premultiplied colour. The CPU reference is `render::mask`.

struct MaskParams {
    // Kernel half-width in whole pixels; 0 reads the source unblurred.
    radius: u32,
    // 1 / (2 sigma^2) of the Gaussian.
    falloff: f32,
    invert: u32,
    // 0 the first mask (nothing to combine with), 1 union, 2 intersection.
    combine: u32,
    // 1 / texture size.
    texel: vec2<f32>,
}

@group(0) @binding(0) var<uniform> params: MaskParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src: texture_2d<f32>;
@group(0) @binding(3) var other: texture_2d<f32>;

fn gauss(k: f32) -> f32 {
    return exp(-k * k * params.falloff);
}

// Taps k and k + 1 share one bilinear fetch between their texels, weighted by both.
// The sampler clamps to the edge, so coverage beyond the layer repeats its border.
fn blur(p: vec2<f32>, dir: vec2<f32>) -> f32 {
    let r = i32(params.radius);
    var sum = 0.0;
    var total = 0.0;
    for (var k = -r; k <= r; k = k + 2) {
        let w1 = gauss(f32(k));
        var w2 = 0.0;
        if (k + 1 <= r) {
            w2 = gauss(f32(k + 1));
        }
        let w = w1 + w2;
        let at = (p + dir * (f32(k) + w2 / w)) * params.texel;
        sum = sum + w * textureSampleLevel(src, samp, at, 0.0).r;
        total = total + w;
    }
    return sum / total;
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

// The vertical blur (or the source as it is), inverted if asked, combined with the masks
// so far in `other`.
@fragment
fn fs_combine(in: VsOut) -> @location(0) vec4<f32> {
    let pos = vec2<i32>(in.pos.xy);
    var v = textureLoad(src, pos, 0).r;
    if (params.radius > 0u) {
        v = blur(in.pos.xy, vec2<f32>(0.0, 1.0));
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
