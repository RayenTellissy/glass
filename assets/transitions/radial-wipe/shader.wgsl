// Radial wipe: B grows as a circle from the centre point (a fraction of the frame, so
// (0.5, 0.5) is the middle; it may lie outside the frame) until it covers the farthest
// corner. The radius starts just short of the nearest point of the frame and ends past
// the farthest corner, with room for the softness and border. Parameters: centre,
// softness and border width in output pixels, border colour.

// Coverage of B at a point `d` output pixels inside B's edge (negative outside): a 1 px
// box that anti-aliases a hard edge, widened to `softness` pixels and eased towards a
// smoothstep as the softness passes 1 px. Non-decreasing in d, 0 from half the width
// outside the edge and 1 from half the width inside.
fn edge(d: f32, softness: f32) -> f32 {
    let s = max(softness, 0.0);
    let x = clamp(d / max(s, 1.0) + 0.5, 0.0, 1.0);
    return mix(x, x * x * (3.0 - 2.0 * x), clamp(s - 1.0, 0.0, 1.0));
}

// A outside `outer`, the border colour (straight linear RGBA, premultiplied here)
// between `outer` and `inner`, B inside `inner` (outer >= inner). A weighted sum, so
// coverage 0 is exactly A and 1 exactly B.
fn wipe(in: VsOut, inner: f32, outer: f32, border: vec4<f32>) -> vec4<f32> {
    let c = vec4<f32>(border.rgb * border.a, border.a);
    return here_a(in) * (1.0 - outer) + c * (outer - inner) + here_b(in) * inner;
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
    let softness = tr.p[1].x;
    let border = max(tr.p[2].x, 0.0);
    let margin = 0.5 * max(softness, 1.0) + border;
    let c = tr.p[0].xy * tr.size;
    let near = length(c - clamp(c, vec2<f32>(0.0), tr.size)) - margin;
    let far = length(max(abs(c), abs(tr.size - c))) + margin;
    let d = near * (1.0 - t) + far * t - length(in.pos.xy - c);
    return wipe(in, edge(d, softness), edge(d + border, softness), tr.p[3]);
}
