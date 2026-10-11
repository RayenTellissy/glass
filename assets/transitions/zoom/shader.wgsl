// Zoom: in, the view flies into A (scaled from 1 to the zoom factor about the centre)
// while B comes from the distance (from one over the factor to 1) and fades in over
// it; out, A shrinks away (from 1 to one over the factor) and fades out over B, which
// settles from the factor to 1. Scales change exponentially, at a steady apparent
// speed; what the smaller frame leaves uncovered shows the larger one, which always
// covers the frame. The shrinking frame (A out, B in) is read through a pyramid of
// halvings (`reduce`, passes 0 to 3, down to a sixteenth) between the two levels
// nearest its footprint (one pair for the whole pass, chosen before the per-pixel work
// so each read names its texture), so strong zooms do not alias. An optional radial blur averages
// the picture along the line to the centre, over a scale range proportional to the
// zoom's speed and largest half way: up to `samples` pixels of blur by one tap a pixel
// here in `fs`; a longer one at a quarter of the resolution (at least 256 texels a
// side), where `small` draws the zoom over the margin the blur reaches into and three
// `smear` passes each average up to `samples` taps along the line, at scales spaced by
// that factor more each time (one comb of up to samples^3 taps), and `fs` reads the
// last, mixing it in from `samples` to twice that many pixels of blur. Parameters: mode,
// zoom factor, blur, samples.

// The first smear's pass index (after the pyramid and `small`, pass 4); the last,
// which `fs` reads, is pass 7.
const FIRST_SMEAR: u32 = 5u;

// The zoom at the current progress.
struct Zoom {
    out: bool,
    // The blur's range of scales (natural log).
    spread: f32,
    samples: f32,
    // How far B has faded in over A (in), or A out over B (out).
    fade: f32,
    scale_a: f32,
    scale_b: f32,
    // The scale about the centre the reduced passes cover: the frame and as far beyond
    // as the blur reaches.
    margin: f32,
    // The longest blur in output pixels (in the corners).
    longest: f32,
}

fn zoom() -> Zoom {
    var z: Zoom;
    let t = tr.progress;
    z.out = tr.p[0].x >= 0.5;
    let range = log(max(tr.p[1].x, 1.0));
    z.spread = max(max(tr.p[2].x, 0.0) * range * 4.0 * t * (1.0 - t), 0.0);
    z.samples = max(round(tr.p[3].x), 1.0);
    z.fade = smoothstep(0.0, 1.0, t);
    if z.out {
        z.scale_a = exp(-range * t);
        z.scale_b = exp(range * (1.0 - t));
    } else {
        z.scale_a = exp(range * t);
        z.scale_b = exp(range * (t - 1.0));
    }
    z.margin = exp(0.5 * z.spread);
    z.longest = length(0.5 * tr.size) * z.spread;
    return z;
}

// How much of the result comes from the reduced passes for `blur` pixels of blur.
fn reduced_share(z: Zoom, blur: f32) -> f32 {
    return clamp(blur / z.samples - 1.0, 0.0, 1.0);
}

// The shrinking frame's scale.
fn shrink_scale(z: Zoom) -> f32 {
    return select(z.scale_b, z.scale_a, z.out);
}

// The pyramid level (0 the frame itself) for `px` output pixels of the shrinking frame.
fn level_for(z: Zoom, px: f32) -> f32 {
    return clamp(log2(max(px / shrink_scale(z), 1.0)), 0.0, 4.0);
}

// A frame scaled by `scale` about the centre at the output point `p`, for an output
// pixel `px` pixels wide, read from `lo` and `hi` mixed by `f` (the same texture twice
// with `f` 0 for the growing frame): weighted by its coverage, transparent outside.
fn scaled(
    lo: texture_2d<f32>,
    hi: texture_2d<f32>,
    f: f32,
    p: vec2<f32>,
    scale: f32,
    px: f32,
) -> vec4<f32> {
    let c = 0.5 * tr.size;
    let q = c + (p - c) / scale;
    let cover = frame_coverage(q, px / scale);
    if cover <= 0.0 {
        return vec4<f32>(0.0);
    }
    let uv = q / tr.size;
    var v = textureSampleLevel(lo, samp, uv, 0.0);
    if f > 0.0 {
        v = mix(v, textureSampleLevel(hi, samp, uv, 0.0), f);
    }
    return v * cover;
}

// The unblurred zoom at `p` for an output pixel `px` pixels wide, from the shrinking
// frame's levels `lo` and `hi` mixed by `f` and the growing frame `g`.
fn picture(
    z: Zoom,
    lo: texture_2d<f32>,
    hi: texture_2d<f32>,
    f: f32,
    g: texture_2d<f32>,
    p: vec2<f32>,
    px: f32,
) -> vec4<f32> {
    let m = scaled(lo, hi, f, p, shrink_scale(z), px);
    if z.out {
        return over(m * (1.0 - z.fade), scaled(g, g, 0.0, p, z.scale_b, px));
    }
    return over(m * z.fade, scaled(g, g, 0.0, p, z.scale_a, px));
}

// Halves the shrinking frame (pass 0) or the level before: a texel's centre lies
// between four of the level above, so one filtered read is their mean.
@fragment
fn reduce(in: VsOut) -> @location(0) vec4<f32> {
    switch tr.pass_index {
        case 0u: {
            if tr.p[0].x >= 0.5 {
                return sample_a(in.uv);
            }
            return sample_b(in.uv);
        }
        case 1u: {
            return textureSampleLevel(pass0, samp, in.uv, 0.0);
        }
        case 2u: {
            return textureSampleLevel(pass1, samp, in.uv, 0.0);
        }
        default: {
            return textureSampleLevel(pass2, samp, in.uv, 0.0);
        }
    }
}

// `small` from the shrinking frame `m` (level 0), the growing frame `g` and `level`.
fn small_from(
    z: Zoom,
    m: texture_2d<f32>,
    g: texture_2d<f32>,
    level: f32,
    p: vec2<f32>,
    px: f32,
) -> vec4<f32> {
    let below = min(floor(level), 3.0);
    let f = level - below;
    switch u32(below) {
        case 0u: {
            return small_taps(z, m, pass0, f, g, p, px);
        }
        case 1u: {
            return small_taps(z, pass0, pass1, f, g, p, px);
        }
        case 2u: {
            return small_taps(z, pass1, pass2, f, g, p, px);
        }
        default: {
            return small_taps(z, pass2, pass3, f, g, p, px);
        }
    }
}

// The mean of four reads of the picture a quarter of a texel `px` wide apart.
fn small_taps(
    z: Zoom,
    lo: texture_2d<f32>,
    hi: texture_2d<f32>,
    f: f32,
    g: texture_2d<f32>,
    p: vec2<f32>,
    px: f32,
) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var i = 0u; i < 4u; i++) {
        let o = (vec2<f32>(f32(i & 1u), f32(i >> 1u)) - 0.5) * 0.5 * px;
        sum += picture(z, lo, hi, f, g, p + o, 0.5 * px);
    }
    return 0.25 * sum;
}

// The unblurred zoom over the margin, each texel the mean of four reads a quarter of it
// apart. Nothing while the reduced passes are not used.
@fragment
fn small(in: VsOut) -> @location(0) vec4<f32> {
    let z = zoom();
    if reduced_share(z, z.longest) <= 0.0 {
        return vec4<f32>(0.0);
    }
    let c = 0.5 * tr.size;
    let p = c + (in.uv * tr.size - c) * z.margin;
    let px = tr.size.x / tr.pass_size.x * z.margin;
    let level = level_for(z, 0.5 * px);
    if z.out {
        return small_from(z, tex_a, tex_b, level, p, px);
    }
    return small_from(z, tex_b, tex_a, level, p, px);
}

// One of three smears of the pass before (the first smears `small`): `n` taps along the
// line to the centre, at scales whose logs are n^k times the comb's spacing apart in
// smear k (from 0). The comb's taps are at most a texel apart in the corners.
@fragment
fn smear(in: VsOut) -> @location(0) vec4<f32> {
    let z = zoom();
    if reduced_share(z, z.longest) <= 0.0 {
        return vec4<f32>(0.0);
    }
    let texel = tr.size.x / tr.pass_size.x * z.margin;
    let n = clamp(ceil(pow(z.longest / texel, 1.0 / 3.0) - 1e-3), 1.0, z.samples);
    let spacing = z.spread / (n * n * n) * pow(n, f32(tr.pass_index - FIRST_SMEAR));
    switch tr.pass_index - FIRST_SMEAR {
        case 0u: {
            return radial(pass4, in.uv, n, spacing);
        }
        case 1u: {
            return radial(pass5, in.uv, n, spacing);
        }
        default: {
            return radial(pass6, in.uv, n, spacing);
        }
    }
}

// The mean of `n` reads of `t` along the line from uv to the centre, at scales whose
// logs are `spacing` apart.
fn radial(t: texture_2d<f32>, uv: vec2<f32>, n: f32, spacing: f32) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var i = 0.0; i < n; i += 1.0) {
        let k = exp((i - 0.5 * (n - 1.0)) * spacing);
        sum += textureSampleLevel(t, samp, 0.5 + (uv - 0.5) * k, 0.0);
    }
    return sum / n;
}

// `fs` from the shrinking frame `m` (level 0) and the growing frame `g`.
fn result(z: Zoom, m: texture_2d<f32>, g: texture_2d<f32>, p: vec2<f32>) -> vec4<f32> {
    let level = level_for(z, 1.0);
    let below = min(floor(level), 3.0);
    let f = level - below;
    switch u32(below) {
        case 0u: {
            return blurred(z, m, pass0, f, g, p);
        }
        case 1u: {
            return blurred(z, pass0, pass1, f, g, p);
        }
        case 2u: {
            return blurred(z, pass1, pass2, f, g, p);
        }
        default: {
            return blurred(z, pass2, pass3, f, g, p);
        }
    }
}

// The blurred zoom at the output point `p`: taps here up to `samples` pixels of blur,
// the last smear beyond twice that, mixed between.
fn blurred(
    z: Zoom,
    lo: texture_2d<f32>,
    hi: texture_2d<f32>,
    f: f32,
    g: texture_2d<f32>,
    p: vec2<f32>,
) -> vec4<f32> {
    let c = 0.5 * tr.size;
    let d = p - c;
    let blur = length(d) * z.spread;
    let share = reduced_share(z, blur);
    var direct = vec4<f32>(0.0);
    if share < 1.0 {
        // Taps whose scales step by a constant ratio, about one a pixel of blur.
        let n = max(ceil(blur), 1.0);
        let step = exp(-z.spread / n);
        var k = exp(0.5 * z.spread * (1.0 - 1.0 / n));
        for (var i = 0.0; i < n; i += 1.0) {
            direct += picture(z, lo, hi, f, g, c + d * k, k);
            k *= step;
        }
        direct /= n;
    }
    var reduced = vec4<f32>(0.0);
    if share > 0.0 {
        reduced = textureSampleLevel(pass7, samp, (c + d / z.margin) / tr.size, 0.0);
    }
    return mix(direct, reduced, share);
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
    let z = zoom();
    if z.out {
        return result(z, tex_a, tex_b, in.pos.xy);
    }
    return result(z, tex_b, tex_a, in.pos.xy);
}
