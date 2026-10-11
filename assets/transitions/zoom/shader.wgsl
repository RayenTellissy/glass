// Zoom: in, the view flies into A (scaled from 1 to the zoom factor about the centre)
// while B comes from the distance (from one over the factor to 1) and fades in over
// it; out, A shrinks away (from 1 to one over the factor) and fades out over B, which
// settles from the factor to 1. Scales change exponentially, at a steady apparent
// speed; what the smaller frame leaves uncovered shows the larger one, which always
// covers the frame. An optional radial blur averages taps along the line to the centre,
// over a scale range proportional to the zoom's speed and largest half way. Parameters:
// mode, zoom factor, blur, samples.

// A (or B) scaled by `scale` about the centre, blurred over `spread` (natural log of
// the scale) with up to `most` taps: about one a pixel of blur at this distance.
fn zoomed(b: bool, p: vec2<f32>, scale: f32, spread: f32, most: f32) -> vec4<f32> {
    let c = 0.5 * tr.size;
    let d = p - c;
    let n = clamp(ceil(length(d) * spread), 1.0, most);
    // The taps' scales step by a constant ratio from the smallest.
    let step = exp(-spread / n);
    var shrink = exp(0.5 * spread * (1.0 - 1.0 / n)) / scale;
    var sum = vec4<f32>(0.0);
    for (var i = 0u; i < u32(n); i++) {
        let q = c + d * shrink;
        if b {
            sum += frame_b(q, shrink);
        } else {
            sum += frame_a(q, shrink);
        }
        shrink *= step;
    }
    return sum / n;
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
    let out = tr.p[0].x >= 0.5;
    let range = log(max(tr.p[1].x, 1.0));
    let spread = max(tr.p[2].x, 0.0) * range * 4.0 * t * (1.0 - t);
    let most = max(round(tr.p[3].x), 1.0);
    let p = in.pos.xy;
    let fade = smoothstep(0.0, 1.0, t);
    if out {
        let a = zoomed(false, p, exp(-range * t), spread, most);
        let b = zoomed(true, p, exp(range * (1.0 - t)), spread, most);
        return over(a * (1.0 - fade), b);
    }
    let a = zoomed(false, p, exp(range * t), spread, most);
    let b = zoomed(true, p, exp(range * (t - 1.0)), spread, most);
    return over(b * fade, a);
}
