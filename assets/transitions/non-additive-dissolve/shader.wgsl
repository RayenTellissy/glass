// Non-additive dissolve: per channel, the larger of A and B, each weighted as in the
// additive dissolve (A full until half way then ramping down, B ramping up to full by
// half way). B's brighter areas show through first and A's darker areas go first; half
// way the picture is the per-channel maximum of the two. Progress 0 and 1 return A and B
// untouched, since a maximum against zero would lift negative (wide gamut) values.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    if t <= 0.0 {
        return here_a(in);
    }
    if t >= 1.0 {
        return here_b(in);
    }
    let a = here_a(in) * clamp(2.0 * (1.0 - t), 0.0, 1.0);
    let b = here_b(in) * clamp(2.0 * t, 0.0, 1.0);
    return max(a, b);
}
