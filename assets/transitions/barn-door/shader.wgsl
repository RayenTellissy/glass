// Barn door: A splits along the frame's centre line, vertical (the doors slide left and
// right) or horizontal (up and down). Open: the doors part and B grows from the centre
// line. Close: B comes in from both edges and meets at the centre line. The edges start
// and end a margin outside, the softness's half width plus the border. Parameters:
// split, mode, softness and border width in output pixels, border colour.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    if t <= 0.0 {
        return here_a(in);
    }
    if t >= 1.0 {
        return here_b(in);
    }
    let vertical = tr.p[0].x < 0.5;
    let close = tr.p[1].x >= 0.5;
    let softness = tr.p[2].x;
    let border = max(tr.p[3].x, 0.0);
    let margin = 0.5 * max(softness, 1.0) + border;
    let q = abs(in.pos.xy - 0.5 * tr.size);
    let x = select(q.y, q.x, vertical);
    let half_size = 0.5 * select(tr.size.y, tr.size.x, vertical);
    var d: f32;
    if close {
        d = x - ((half_size + margin) * (1.0 - t) - margin * t);
    } else {
        d = -margin * (1.0 - t) + (half_size + margin) * t - x;
    }
    return wipe(in, edge(d, softness), edge(d + border, softness), tr.p[4]);
}
