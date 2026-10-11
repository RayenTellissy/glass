// Swirl: a vortex about the centre twists A and B alike, by the angle at the centre
// falling smoothly to nothing at the radius (a fraction of half the frame's shorter
// side, so at 100% the vortex is the largest circle inside the frame), growing to its
// peak half way and unwinding after, while A dissolves into B in the middle half.
// Beyond 100%, what the twist pulls in from outside the frame is transparent black.
// Parameters: rotation, angle, radius.

const PI: f32 = 3.141592653589793;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    if t <= 0.0 {
        return here_a(in);
    }
    if t >= 1.0 {
        return here_b(in);
    }
    let turn = select(1.0, -1.0, tr.p[0].x >= 0.5) * tr.p[1].x * sin(PI * t);
    let radius = max(tr.p[2].x, 0.0) * 0.5 * min(tr.size.x, tr.size.y);
    let c = 0.5 * tr.size;
    let d = in.pos.xy - c;
    let k = max(1.0 - length(d) / max(radius, 1e-3), 0.0);
    let q = c + rotate(d, -turn * k * k);
    let w = smoothstep(0.25, 0.75, t);
    if tr.p[2].x <= 1.0 {
        // The twist stays inside the frame.
        return sample_a(q / tr.size) * (1.0 - w) + sample_b(q / tr.size) * w;
    }
    return frame_a(q, 1.0) * (1.0 - w) + frame_b(q, 1.0) * w;
}
