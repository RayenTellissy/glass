// Layer compositing: up to eight layers over an optional background in one pass. Each
// layer is resampled through its transform (canvas pixel centre to layer point, nearest,
// bilinear or Catmull-Rom bicubic), limited to its own rectangle with antialiased edges,
// and blended with its mode on premultiplied linear light (W3C Compositing Level 1). The
// node picks the entry point for the most expensive filter its layers use.
// render::transform and render::blend hold the formulas, and nodes::Layer::reference is
// the CPU version of layer_colour. Per-layer fields extend `Layer` here and
// `Layer::gpu_words` together.

const MAX_LAYERS: u32 = 8u;

struct Layer {
    // Texture size in pixels.
    size: vec2<f32>,
    blend: u32,
    // 0 nearest, 1 bilinear, 2 bicubic.
    resample: u32,
    // Canvas pixel to layer point: p = (a x + c y + tx, b x + d y + ty) with m = (a, b, c,
    // d) and t = (tx, ty, footprint x, footprint y); the footprint is how many layer
    // pixels one canvas pixel spans along each axis.
    m: vec4<f32>,
    t: vec4<f32>,
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

// Catmull-Rom (B = 0, C = 0.5) with nine bilinear taps: the two middle weights of each
// axis are positive, so one bilinear fetch between those texels at offset w2 / (w1 + w2)
// returns their weighted sum. The sampler clamps to the edge, like the CPU reference.
fn bicubic(t: texture_2d<f32>, p: vec2<f32>, size: vec2<f32>) -> vec4<f32> {
    let tc = p - 0.5;
    let f0 = floor(tc);
    let f = tc - f0;
    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let t0 = (f0 - 0.5) / size;
    let t12 = (f0 + 0.5 + w2 / w12) / size;
    let t3 = (f0 + 2.5) / size;
    var c = vec4<f32>(0.0);
    c += textureSampleLevel(t, samp, vec2<f32>(t0.x, t0.y), 0.0) * (w0.x * w0.y);
    c += textureSampleLevel(t, samp, vec2<f32>(t12.x, t0.y), 0.0) * (w12.x * w0.y);
    c += textureSampleLevel(t, samp, vec2<f32>(t3.x, t0.y), 0.0) * (w3.x * w0.y);
    c += textureSampleLevel(t, samp, vec2<f32>(t0.x, t12.y), 0.0) * (w0.x * w12.y);
    c += textureSampleLevel(t, samp, vec2<f32>(t12.x, t12.y), 0.0) * (w12.x * w12.y);
    c += textureSampleLevel(t, samp, vec2<f32>(t3.x, t12.y), 0.0) * (w3.x * w12.y);
    c += textureSampleLevel(t, samp, vec2<f32>(t0.x, t3.y), 0.0) * (w0.x * w3.y);
    c += textureSampleLevel(t, samp, vec2<f32>(t12.x, t3.y), 0.0) * (w12.x * w3.y);
    c += textureSampleLevel(t, samp, vec2<f32>(t3.x, t3.y), 0.0) * (w3.x * w3.y);
    // The negative lobes can overshoot: keep alpha a coverage and colour non-negative.
    return vec4<f32>(max(c.rgb, vec3<f32>(0.0)), clamp(c.a, 0.0, 1.0));
}

fn filtered(t: texture_2d<f32>, p: vec2<f32>, size: vec2<f32>, resample: u32, ceiling: u32) -> vec4<f32> {
    switch min(resample, ceiling) {
        case 0u: {
            let i = clamp(vec2<i32>(floor(p)), vec2<i32>(0), vec2<i32>(size) - 1);
            return textureLoad(t, i, 0);
        }
        case 1u: { return textureSampleLevel(t, samp, p / size, 0.0); }
        default: { return bicubic(t, p, size); }
    }
}

fn fetch(i: u32, p: vec2<f32>, ceiling: u32) -> vec4<f32> {
    let size = params.layers[i].size;
    let resample = params.layers[i].resample;
    switch i {
        case 0u: { return filtered(layer0, p, size, resample, ceiling); }
        case 1u: { return filtered(layer1, p, size, resample, ceiling); }
        case 2u: { return filtered(layer2, p, size, resample, ceiling); }
        case 3u: { return filtered(layer3, p, size, resample, ceiling); }
        case 4u: { return filtered(layer4, p, size, resample, ceiling); }
        case 5u: { return filtered(layer5, p, size, resample, ceiling); }
        case 6u: { return filtered(layer6, p, size, resample, ceiling); }
        default: { return filtered(layer7, p, size, resample, ceiling); }
    }
}

// Coverage of one hard edge for a pixel centre `d` layer pixels inside it, box-filtered
// over the pixel's footprint.
fn edge(d: f32, footprint: f32) -> f32 {
    return clamp((d + 0.5 * footprint) / footprint, 0.0, 1.0);
}

// The layer's premultiplied colour at canvas point q.
fn layer_colour(i: u32, q: vec2<f32>, ceiling: u32) -> vec4<f32> {
    let layer = params.layers[i];
    let p = vec2<f32>(
        layer.m.x * q.x + layer.m.z * q.y + layer.t.x,
        layer.m.y * q.x + layer.m.w * q.y + layer.t.y,
    );
    let fp = layer.t.zw;
    let coverage = edge(p.x, fp.x) * edge(layer.size.x - p.x, fp.x)
        * edge(p.y, fp.y) * edge(layer.size.y - p.y, fp.y);
    if coverage <= 0.0 {
        return vec4<f32>(0.0);
    }
    return fetch(i, p, ceiling) * coverage;
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

// Every layer's filter is capped at `ceiling`, a constant in each entry point, so the
// compiler drops the unused filters: a pass that carries the bicubic code runs with fewer
// threads in flight, which costs untransformed layers about half their time again.
fn run(q: vec2<f32>, ceiling: u32) -> vec4<f32> {
    var dst = vec4<f32>(0.0);
    if params.has_background != 0u {
        dst = textureLoad(background, vec2<i32>(q), 0);
    }
    for (var i = 0u; i < min(params.count, MAX_LAYERS); i++) {
        dst = composite(params.layers[i].blend, layer_colour(i, q, ceiling), dst);
    }
    return dst;
}

@fragment
fn fs_nearest(in: VsOut) -> @location(0) vec4<f32> {
    return run(in.pos.xy, 0u);
}

@fragment
fn fs_bilinear(in: VsOut) -> @location(0) vec4<f32> {
    return run(in.pos.xy, 1u);
}

@fragment
fn fs_bicubic(in: VsOut) -> @location(0) vec4<f32> {
    return run(in.pos.xy, 2u);
}
