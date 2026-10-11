// Linear wipe: a straight edge crosses the picture in the chosen direction (the
// diagonals at 45 degrees), B behind it and A ahead. The edge starts just outside the
// first corner and ends just past the last, with room for its softness and border, so
// the picture changes continuously from A and into B. Parameters: softness and border
// width in output pixels, border colour.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    if t <= 0.0 {
        return here_a(in);
    }
    if t >= 1.0 {
        return here_b(in);
    }
    let softness = tr.p[0].x;
    let border = max(tr.p[1].x, 0.0);
    let margin = 0.5 * max(softness, 1.0) + border;
    // The frame's extent along the direction of travel.
    let span = tr.dir * tr.size;
    let first = min(span.x, 0.0) + min(span.y, 0.0) - margin;
    let last = max(span.x, 0.0) + max(span.y, 0.0) + margin;
    let d = first * (1.0 - t) + last * t - dot(in.pos.xy, tr.dir);
    return wipe(in, edge(d, softness), edge(d + border, softness), tr.p[2]);
}
