// Slide: in, B slides in over A from the side behind the direction while A stays; out,
// A slides off ahead of the direction and uncovers B, which stays. The moving frame is
// laid over the still one (premultiplied over). Parameter: mode.

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
    if tr.p[0].x >= 0.5 {
        return over(frame_a(in.pos.xy - t * span, 1.0), here_b(in));
    }
    return over(frame_b(in.pos.xy + (1.0 - t) * span, 1.0), here_a(in));
}
