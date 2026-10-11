// Film dissolve: the pictures are mixed after a display-like encoding rather than in
// linear light, the way an optical printer's dissolve between two exposures reads. Each
// straight (unpremultiplied) colour channel x is encoded as sign(x) * |x|^(1/2.4), the
// inverse of the BT.1886 power, mixed by progress with alpha, and decoded with the 2.4
// power. Half way between black and white is 0.5^2.4 = 0.19 in linear light instead of
// the cross dissolve's 0.5, so the dissolve dips through the darker picture. Values
// above 1 and negative ones (wide gamut) follow the same odd power curve. A pixel with
// zero alpha is encoded as if opaque, so additive light without coverage is kept.
// Progress 0 and 1 return A and B untouched, as an encode-decode round trip is not
// exact in floating point.

const GAMMA: f32 = 2.4;

fn power(x: vec3<f32>, y: f32) -> vec3<f32> {
    let m = abs(x);
    return sign(x) * select(pow(m, vec3<f32>(y)), vec3<f32>(0.0), m == vec3<f32>(0.0));
}

// Premultiplied linear to premultiplied encoded.
fn encode(c: vec4<f32>) -> vec4<f32> {
    let a = select(c.a, 1.0, c.a <= 0.0);
    return vec4<f32>(power(c.rgb / a, 1.0 / GAMMA) * a, c.a);
}

// Premultiplied encoded to premultiplied linear.
fn decode(e: vec4<f32>) -> vec4<f32> {
    let a = select(e.a, 1.0, e.a <= 0.0);
    return vec4<f32>(power(e.rgb / a, GAMMA) * a, e.a);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    if t <= 0.0 {
        return here_a(in);
    }
    if t >= 1.0 {
        return here_b(in);
    }
    return decode(encode(here_a(in)) * (1.0 - t) + encode(here_b(in)) * t);
}
