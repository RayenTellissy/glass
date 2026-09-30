// The working space (linear light, 1.0 = 203-nit reference white, premultiplied) to an
// output encoding: gamut into the output primaries, alpha divided out, the transfer
// function, and triangular (TPDF) dither of one code value before the target's own
// round-to-nearest quantisation.

struct OutputParams {
    r: vec4<f32>,
    g: vec4<f32>,
    b: vec4<f32>,
    transfer: u32,
    // Code values of the target (255, 1023, 65535), 0 for float targets (no dither).
    levels: f32,
    dither: u32,
    seed: u32,
    straight_alpha: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: OutputParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src: texture_2d<f32>;

fn srgb_encode(v: f32) -> f32 {
    if v <= 0.0031308 {
        return 12.92 * v;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn bt709_oetf(v: f32) -> f32 {
    if v < 0.018 {
        return 4.5 * v;
    }
    return 1.099 * pow(v, 0.45) - 0.099;
}

fn pq_encode(nits: f32) -> f32 {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 4096.0 * 128.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 4096.0 * 32.0;
    let c3 = 2392.0 / 4096.0 * 32.0;
    let y = pow(clamp(nits / 10000.0, 0.0, 1.0), m1);
    return pow((c1 + c2 * y) / (1.0 + c3 * y), m2);
}

fn hlg_oetf(e: f32) -> f32 {
    let a = 0.17883277;
    let b = 1.0 - 4.0 * a;
    let c = 0.55991073;
    if e <= 1.0 / 12.0 {
        return sqrt(3.0 * e);
    }
    return a * log(12.0 * e - b) + c;
}

// Display light relative to 203 nits to HLG: the inverse of the BT.2100 OOTF for a
// 1000-nit display (system gamma 1.2), then the OETF.
fn hlg_encode(l: vec3<f32>) -> vec3<f32> {
    let fd = max(l, vec3<f32>(0.0)) * (203.0 / 1000.0);
    let yd = dot(fd, vec3<f32>(0.2627, 0.6780, 0.0593));
    if yd <= 0.0 {
        return vec3<f32>(0.0);
    }
    let ys = pow(yd, 1.0 / 1.2);
    let e = clamp(fd / pow(ys, 0.2), vec3<f32>(0.0), vec3<f32>(1.0));
    return vec3<f32>(hlg_oetf(e.r), hlg_oetf(e.g), hlg_oetf(e.b));
}

fn encode(l: vec3<f32>) -> vec3<f32> {
    let c = max(l, vec3<f32>(0.0));
    switch params.transfer {
        case 0u: { return vec3<f32>(srgb_encode(c.r), srgb_encode(c.g), srgb_encode(c.b)); }
        case 1u: { return pow(c, vec3<f32>(1.0 / 2.4)); }
        case 2u: { return vec3<f32>(bt709_oetf(c.r), bt709_oetf(c.g), bt709_oetf(c.b)); }
        case 3u: { return vec3<f32>(pq_encode(c.r * 203.0), pq_encode(c.g * 203.0), pq_encode(c.b * 203.0)); }
        case 4u: { return hlg_encode(c); }
        default: { return l; }
    }
}

// PCG3D (Jarzynski and Olano, "Hash Functions for GPU Rendering", 2020).
fn pcg3d(v_in: vec3<u32>) -> vec3<u32> {
    var v = v_in * 1664525u + 1013904223u;
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v = v ^ (v >> vec3<u32>(16u));
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    return v;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.pos.xy);
    let c = textureLoad(src, p, 0);
    var rgb = c.rgb;
    var a = clamp(c.a, 0.0, 1.0);
    if params.straight_alpha != 0u && a > 0.0 {
        rgb = rgb / a;
    }
    let l = vec3<f32>(dot(params.r.xyz, rgb), dot(params.g.xyz, rgb), dot(params.b.xyz, rgb));
    var e = encode(l);
    if params.transfer < 5u {
        e = clamp(e, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    if params.dither != 0u && params.levels > 0.0 {
        // Two uniform variables per channel sum to a triangular distribution over
        // (-1, 1) code values: the quantisation error becomes signal-independent noise.
        // One hash gives 16 bits for each of the six variables.
        let h = pcg3d(vec3<u32>(vec2<u32>(p), params.seed));
        let u1 = vec3<f32>(h & vec3<u32>(0xffffu)) * (1.0 / 65536.0);
        let u2 = vec3<f32>(h >> vec3<u32>(16u)) * (1.0 / 65536.0);
        let noise = (u1 + u2 - 1.0) / params.levels;
        // Keep exact black and white exact.
        let inside = step(vec3<f32>(0.5 / params.levels), e) * step(e, vec3<f32>(1.0 - 0.5 / params.levels));
        e = clamp(e + noise * inside, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return vec4<f32>(e, a);
}
