// A LUT applied in the working space: alpha divided out, into the LUT's input space
// (gamut matrix, then transfer), the 1D shaper (linear interpolation) and the 3D table
// (tetrahedral interpolation from four texel loads, so the weights are full f32 rather
// than the filtering hardware's fixed-point ones), back to the working space, mixed with
// the input by the intensity, alpha multiplied back in. The CPU reference is
// `render::lut::apply_in_working_space`.

struct LutParams {
    into_r: vec4<f32>,
    into_g: vec4<f32>,
    into_b: vec4<f32>,
    back_r: vec4<f32>,
    back_g: vec4<f32>,
    back_b: vec4<f32>,
    // Domain minimum and 1 / (max - min) per channel.
    shaper_min: vec4<f32>,
    shaper_scale: vec4<f32>,
    cube_min: vec4<f32>,
    cube_scale: vec4<f32>,
    // 0 Rec.709 (inverse BT.1886), 1 sRGB, 2 linear.
    space: u32,
    // Entries, 0 when the table is absent.
    shaper_size: u32,
    cube_size: u32,
    intensity: f32,
}

@group(0) @binding(0) var<uniform> params: LutParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src: texture_2d<f32>;
// One entry per texel, rows of 4096.
@group(0) @binding(3) var shaper: texture_2d<f32>;
// Red along x, green along y, blue along z.
@group(0) @binding(4) var cube: texture_3d<f32>;

fn srgb_encode(v: f32) -> f32 {
    if v <= 0.0031308 {
        return 12.92 * v;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn srgb_decode(e: f32) -> f32 {
    if e <= 0.04045 {
        return e / 12.92;
    }
    return pow((e + 0.055) / 1.055, 2.4);
}

fn encode(l: vec3<f32>) -> vec3<f32> {
    let c = max(l, vec3<f32>(0.0));
    switch params.space {
        case 0u: { return pow(c, vec3<f32>(1.0 / 2.4)); }
        case 1u: { return vec3<f32>(srgb_encode(c.r), srgb_encode(c.g), srgb_encode(c.b)); }
        default: { return c; }
    }
}

fn decode(e_in: vec3<f32>) -> vec3<f32> {
    let e = max(e_in, vec3<f32>(0.0));
    switch params.space {
        case 0u: { return pow(e, vec3<f32>(2.4)); }
        case 1u: { return vec3<f32>(srgb_decode(e.r), srgb_decode(e.g), srgb_decode(e.b)); }
        default: { return e; }
    }
}

fn shaper_at(i: u32) -> vec3<f32> {
    return textureLoad(shaper, vec2<i32>(i32(i % 4096u), i32(i / 4096u)), 0).rgb;
}

fn apply_shaper(v: vec3<f32>) -> vec3<f32> {
    let n = params.shaper_size;
    let t = clamp((v - params.shaper_min.xyz) * params.shaper_scale.xyz, vec3<f32>(0.0), vec3<f32>(1.0));
    let p = t * f32(n - 1u);
    let i = min(vec3<u32>(floor(p)), vec3<u32>(n - 2u));
    let f = p - vec3<f32>(i);
    let r = mix(shaper_at(i.r).r, shaper_at(i.r + 1u).r, f.r);
    let g = mix(shaper_at(i.g).g, shaper_at(i.g + 1u).g, f.g);
    let b = mix(shaper_at(i.b).b, shaper_at(i.b + 1u).b, f.b);
    return vec3<f32>(r, g, b);
}

fn cube_at(i: vec3<i32>) -> vec3<f32> {
    return textureLoad(cube, i, 0).rgb;
}

// The grid cube around the input split into six tetrahedra along its black-white
// diagonal; the input is a weighted sum of the four corners of the one it falls in.
fn apply_cube(v: vec3<f32>) -> vec3<f32> {
    let n = params.cube_size;
    let t = clamp((v - params.cube_min.xyz) * params.cube_scale.xyz, vec3<f32>(0.0), vec3<f32>(1.0));
    let p = t * f32(n - 1u);
    let base = min(floor(p), vec3<f32>(f32(n - 2u)));
    let f = p - base;
    let i = vec3<i32>(base);
    let c000 = cube_at(i);
    let c111 = cube_at(i + vec3<i32>(1, 1, 1));
    var d1: vec3<i32>;
    var d2: vec3<i32>;
    var w: vec3<f32>;
    if f.r >= f.g {
        if f.g >= f.b {
            d1 = vec3<i32>(1, 0, 0); d2 = vec3<i32>(1, 1, 0); w = f.rgb;
        } else if f.r >= f.b {
            d1 = vec3<i32>(1, 0, 0); d2 = vec3<i32>(1, 0, 1); w = f.rbg;
        } else {
            d1 = vec3<i32>(0, 0, 1); d2 = vec3<i32>(1, 0, 1); w = f.brg;
        }
    } else if f.b >= f.g {
        d1 = vec3<i32>(0, 0, 1); d2 = vec3<i32>(0, 1, 1); w = f.bgr;
    } else if f.b >= f.r {
        d1 = vec3<i32>(0, 1, 0); d2 = vec3<i32>(0, 1, 1); w = f.gbr;
    } else {
        d1 = vec3<i32>(0, 1, 0); d2 = vec3<i32>(1, 1, 0); w = f.grb;
    }
    let c1 = cube_at(i + d1);
    let c2 = cube_at(i + d2);
    return c000 + w.x * (c1 - c000) + w.y * (c2 - c1) + w.z * (c111 - c2);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(in.pos.xy), 0);
    let a = clamp(c.a, 0.0, 1.0);
    if params.intensity <= 0.0 || a <= 0.0 {
        return c;
    }
    let straight = c.rgb / a;
    let lin = vec3<f32>(dot(params.into_r.xyz, straight), dot(params.into_g.xyz, straight), dot(params.into_b.xyz, straight));
    var e = encode(lin);
    if params.shaper_size > 0u {
        e = apply_shaper(e);
    }
    if params.cube_size > 0u {
        e = apply_cube(e);
    }
    let d = decode(e);
    let graded = vec3<f32>(dot(params.back_r.xyz, d), dot(params.back_g.xyz, d), dot(params.back_b.xyz, d));
    return vec4<f32>(c.rgb + params.intensity * (graded * a - c.rgb), c.a);
}
