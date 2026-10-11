// Radial wipe: B grows as a circle from the centre point (a fraction of the frame, so
// (0.5, 0.5) is the middle; it may lie outside the frame) until it covers the farthest
// corner. The radius starts just short of the nearest point of the frame and ends past
// the farthest corner, with room for the softness and border. Parameters: centre,
// softness and border width in output pixels, border colour.

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
