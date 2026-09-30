// Layer compositing: up to eight layers over an optional background in one pass, each
// blended with its mode on premultiplied linear light (W3C Compositing Level 1; formulas
// in render::blend, which is the CPU reference). A layer smaller than the canvas sits at
// its top left and is transparent outside itself. Per-layer fields beyond the blend mode
// (opacity, transform, crop) extend `Layer` here and `CompositeNode::uniform` together.

const MAX_LAYERS: u32 = 8u;

struct Layer {
    size: vec2<f32>,
    blend: u32,
    _pad: u32,
}

struct CompositeParams {
    count: u32,
    has_background: u32,
    _pad0: u32,
    _pad1: u32,
    layers: array<Layer, 8>,
}

@group(0) @binding(0) var<uniform> params: CompositeParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var background: texture_2d<f32>;
@group(0) @binding(3) var layer0: texture_2d<f32>;
@group(0) @binding(4) var layer1: texture_2d<f32>;
@group(0) @binding(5) var layer2: texture_2d<f32>;
@group(0) @binding(6) var layer3: texture_2d<f32>;
@group(0) @binding(7) var layer4: texture_2d<f32>;
@group(0) @binding(8) var layer5: texture_2d<f32>;
@group(0) @binding(9) var layer6: texture_2d<f32>;
@group(0) @binding(10) var layer7: texture_2d<f32>;

fn load(t: texture_2d<f32>, p: vec2<f32>, size: vec2<f32>) -> vec4<f32> {
    if any(p < vec2<f32>(0.0)) || any(p >= size) {
        return vec4<f32>(0.0);
    }
    return textureLoad(t, vec2<i32>(p), 0);
}

fn fetch(i: u32, p: vec2<f32>) -> vec4<f32> {
    let size = params.layers[i].size;
    switch i {
        case 0u: { return load(layer0, p, size); }
        case 1u: { return load(layer1, p, size); }
        case 2u: { return load(layer2, p, size); }
        case 3u: { return load(layer3, p, size); }
        case 4u: { return load(layer4, p, size); }
        case 5u: { return load(layer5, p, size); }
        case 6u: { return load(layer6, p, size); }
        default: { return load(layer7, p, size); }
    }
}

fn screen(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    let above = (cb > vec3<f32>(1.0)) | (cs > vec3<f32>(1.0));
    return select(cb + cs - cb * cs, max(cb, cs), above);
}

fn hard_light(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    return select(screen(cb, 2.0 * cs - 1.0), cb * 2.0 * cs, cs <= vec3<f32>(0.5));
}

fn blend_fn(mode: u32, cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: { return cb * cs; }
        case 2u: { return screen(cb, cs); }
        case 3u: { return hard_light(cs, cb); }
        case 4u: { return cb + cs; }
        case 5u: { return max(cb - cs, vec3<f32>(0.0)); }
        case 6u: { return abs(cb - cs); }
        case 7u: {
            let d = select(sqrt(max(cb, vec3<f32>(0.0))), ((16.0 * cb - 12.0) * cb + 4.0) * cb, cb <= vec3<f32>(0.25));
            let low = cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
            let high = cb + (2.0 * cs - 1.0) * (d - cb);
            return select(high, low, cs <= vec3<f32>(0.5));
        }
        case 8u: { return hard_light(cb, cs); }
        case 9u: {
            let cap = max(cb, vec3<f32>(1.0));
            let q = min(cap, cb / max(1.0 - cs, vec3<f32>(1e-6)));
            return select(select(q, cap, cs >= vec3<f32>(1.0)), vec3<f32>(0.0), cb <= vec3<f32>(0.0));
        }
        case 10u: {
            let q = 1.0 - min(vec3<f32>(1.0), (1.0 - cb) / max(cs, vec3<f32>(1e-6)));
            return select(select(q, vec3<f32>(0.0), cs <= vec3<f32>(0.0)), cb, cb >= vec3<f32>(1.0));
        }
        default: { return cs; }
    }
}

// Premultiplied source over premultiplied backdrop with a separable blend function.
fn composite(mode: u32, src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    let a_s = src.a;
    let a_b = dst.a;
    if a_s <= 0.0 {
        return dst;
    }
    let cs = src.rgb / a_s;
    let cb = select(vec3<f32>(0.0), dst.rgb / a_b, a_b > 0.0);
    let rgb = src.rgb * (1.0 - a_b) + dst.rgb * (1.0 - a_s) + a_s * a_b * blend_fn(mode, cb, cs);
    return vec4<f32>(rgb, a_s + a_b * (1.0 - a_s));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let q = in.pos.xy;
    var dst = vec4<f32>(0.0);
    if params.has_background != 0u {
        dst = textureLoad(background, vec2<i32>(q), 0);
    }
    for (var i = 0u; i < min(params.count, MAX_LAYERS); i++) {
        let layer = params.layers[i];
        let src = fetch(i, floor(q));
        dst = composite(layer.blend, src, dst);
    }
    return dst;
}
