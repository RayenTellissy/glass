// Roll: A and B turn together like the two halves of a wheel about the middle of a
// frame edge, half a turn clockwise. A rolls away in the direction and B, starting
// folded over the edge out of sight, rolls in behind it; they meet along the edge's
// line through the pivot. The pivot is on the edge a quarter turn clockwise from the
// direction (the bottom edge travelling right, the left edge travelling down). Areas
// neither frame covers on the way are transparent black.

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
    let side = vec2<f32>(-tr.dir.y, tr.dir.x);
    let pivot = 0.5 * tr.size + side * dot(abs(side), 0.5 * tr.size);
    let d = in.pos.xy - pivot;
    let angle = PI * t;
    return frame_a(pivot + rotate(d, -angle), 1.0) + frame_b(pivot + rotate(d, PI - angle), 1.0);
}
