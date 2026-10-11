// Push: B comes in from the side behind the direction and pushes A out ahead of it,
// both moving together by the frame's width (or height), edge to edge.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    if t <= 0.0 {
        return here_a(in);
    }
    if t >= 1.0 {
        return here_b(in);
    }
    let span = tr.dir * tr.size;
    let q = in.pos.xy - t * span;
    return frame_a(q, 1.0) + frame_b(q + span, 1.0);
}
