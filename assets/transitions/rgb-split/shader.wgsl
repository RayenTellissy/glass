// RGB split: red and blue slide apart along the direction (red behind, blue ahead) by
// up to the amount (a fraction of the frame's width) half way, an envelope of
// sin(pi t), and back together, while A dissolves into B over the middle half, where
// the split is widest. Green stays in place. Parameters: amount.

const PI: f32 = 3.141592653589793;

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
    let w = smoothstep(0.25, 0.75, t);
    let o = tr.dir * max(tr.p[0].x, 0.0) * tr.size.x * sin(PI * clamp(t, 0.0, 1.0));
    let p = in.pos.xy;
    let r = picture(p - o, w);
    let g = picture(p, w);
    let b = picture(p + o, w);
    // Each channel keeps its own coverage; the most covered of the three is the alpha,
    // so no channel exceeds it.
    return vec4<f32>(r.r, g.g, b.b, max(max(r.a, g.a), b.a));
}
