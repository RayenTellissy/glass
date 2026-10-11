// Liquid: A melts into B through a flowing distortion. Two fields of fractal noise
// drifting with the progress push A one way and B the other, up to the amount (a
// fraction of the frame's width) half way (an envelope of sin(pi t)); each pixel turns
// from A to B at its own time, set by a third, so B pools in and spreads. The fields
// are smooth, so they are worked out at a quarter of the resolution (`flow`, pass 0:
// the push in red and green, the pixel's timing in blue) and read filtered. The pattern
// is the same for the same seed. Parameters: seed, amount.

const PI: f32 = 3.141592653589793;

@fragment
fn flow(in: VsOut) -> @location(0) vec4<f32> {
    let key = seed_key(tr.p[0].x);
    let q = in.uv * tr.size / tr.size.y * 2.5;
    let drift = vec2<f32>(0.8, -0.6) * tr.progress;
    let push = vec2<f32>(
        fbm(q + drift, key ^ 0x11u, 3u),
        fbm(q - drift + 7.3, key ^ 0x12u, 3u),
    ) - 0.5;
    // Spread from the noise's usual 0.2..0.8 over 0..1.
    let when = clamp((fbm(q * 0.8 + 3.1, key ^ 0x13u, 3u) - 0.2) / 0.6, 0.0, 1.0);
    return vec4<f32>(push, when, 1.0);
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
    let f = textureSampleLevel(pass0, samp, in.uv, 0.0);
    let amount = max(tr.p[1].x, 0.0) * sin(PI * clamp(t, 0.0, 1.0));
    // Push in uv: the frame's width scales both axes.
    let push = 2.0 * f.xy * amount * vec2<f32>(1.0, tr.size.x / tr.size.y);
    // Each pixel's turn lasts half the transition, starting between 0 and 1/2.
    let start = 0.5 * clamp(f.z, 0.0, 1.0);
    let w = smoothstep(start, start + 0.5, t);
    let a = sample_a(in.uv + push * (1.0 - w));
    let b = sample_b(in.uv - push * w);
    return a * (1.0 - w) + b * w;
}
