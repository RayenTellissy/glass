// VHS: a worn tape cuts from A to B. Towards the middle (an envelope of sin(pi t) times
// the amount) rows wobble sideways, a tracking band rolls down the frame tearing its
// rows further and filling them with snow, colour bleeds to the right of the detail
// (luma kept in place, chroma smeared), scanlines darken every few rows (smooth, at
// least three pixels a line) and grain flickers; the picture dissolves from A to B over
// the middle fifth. Grain and wobble change 30 times over the transition and are the
// same for the same seed. Parameters: seed, amount.

const PI: f32 = 3.141592653589793;
const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

// The dissolve at the output point `p` in pixels, filtered (clamped to the edge).
fn picture(p: vec2<f32>, w: f32) -> vec4<f32> {
    let uv = p / tr.size;
    if w <= 0.0 {
        return sample_a(uv);
    }
    if w >= 1.0 {
        return sample_b(uv);
    }
    return mix(sample_a(uv), sample_b(uv), w);
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
    let e = clamp(tr.p[1].x, 0.0, 1.0) * sin(PI * clamp(t, 0.0, 1.0));
    let w = smoothstep(0.4, 0.6, t);
    let seed = seed_key(tr.p[0].x);
    let frame = clamp(floor(tr.linear * 30.0), 0.0, 29.0);
    let key = hash_u32(seed + u32(frame));
    let y = in.pos.y / tr.size.y;

    // Wobble: a slow wave down the frame, drifting with the frames.
    var shift = (value_noise(vec2<f32>(y * 5.0, frame * 0.35), seed) - 0.5) * 0.012;
    // The tracking band: an eighth of the height rolling down once and a half.
    let band = fract(1.5 * tr.linear + hash_cell(vec2<i32>(0, 0), seed));
    let d = abs(fract(y - band + 0.5) - 0.5);
    let tracking = 1.0 - smoothstep(0.0, 0.0625, d);
    shift += tracking * (value_noise(vec2<f32>(in.pos.y * 0.25, frame), key) - 0.3) * 0.06;
    let p = in.pos.xy + vec2<f32>(shift * e * tr.size.x, 0.0);

    // Luma here, chroma smeared to the right over a 250th of the width.
    let here = picture(p, w);
    let bleed = e * tr.size.x / 250.0;
    var chroma = vec3<f32>(0.0);
    for (var i = 1.0; i <= 3.0; i += 1.0) {
        let c = picture(p - vec2<f32>(i * bleed, 0.0), w);
        chroma += c.rgb - dot(c.rgb, LUMA);
    }
    var rgb = dot(here.rgb, LUMA) + mix(here.rgb - dot(here.rgb, LUMA), chroma / 3.0, e);

    // Scanlines: a 240th of the height a line, but at least 3 px.
    let period = max(tr.size.y / 240.0, 3.0);
    rgb *= 1.0 - 0.3 * e * (0.5 + 0.5 * cos(2.0 * PI * in.pos.y / period));

    // Grain, and snow in the tracking band, added in proportion to the coverage; never
    // below black (or the value already there when it is negative).
    let grain = hash_cell(vec2<i32>(in.pos.xy), key) - 0.5;
    let snow = step(0.93, hash_cell(vec2<i32>(i32(in.pos.x * 0.5), i32(in.pos.y)), key ^ 0x2du));
    let noise = e * (0.1 * grain + 0.8 * tracking * snow);
    rgb = max(rgb + noise * here.a, min(rgb, vec3<f32>(0.0)));
    return vec4<f32>(rgb, here.a);
}
