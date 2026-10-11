// Dip to colour: A fades to the colour (parameter 0, straight linear RGBA, premultiplied
// here), reached at the midpoint (parameter 1, default half way), then B fades in.

// A fades to `c` (premultiplied) until the midpoint, then `c` fades to B. Each half is a
// weighted sum, clamped so an overshooting ease holds the colour, A or B rather than
// extrapolating; progress 0 is exactly A and progress 1 exactly B.
fn dip(in: VsOut, c: vec4<f32>, midpoint: f32) -> vec4<f32> {
    let m = clamp(midpoint, 0.05, 0.95);
    let t = tr.progress;
    if t <= m {
        let s = clamp(t / m, 0.0, 1.0);
        return here_a(in) * (1.0 - s) + c * s;
    }
    let s = clamp((t - m) / (1.0 - m), 0.0, 1.0);
    return c * (1.0 - s) + here_b(in) * s;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = tr.p[0];
    return dip(in, vec4<f32>(c.rgb * c.a, c.a), tr.p[1].x);
}
