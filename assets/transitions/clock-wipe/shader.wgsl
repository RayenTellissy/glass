// Clock wipe: a hand turns once around the centre point (a fraction of the frame) from
// the start angle (0 is twelve o'clock), clockwise or anticlockwise, and B fills the
// angle it has swept. Distances to the hand are arc lengths in output pixels, measured
// at a radius of at least twice the edge's half width so the softness stays bounded
// near the centre. The hand starts and ends that half width (in angle) beyond the start
// ray, so its softness and border appear and leave without a jump. The start ray is a
// fixed edge, anti-aliased but not softened, and the border follows the hand only.
// Parameters: start angle, rotation, centre, softness and border width in output
// pixels, border colour.

const TAU: f32 = 6.283185307179586;

// B's coverage with the hand at `hand` radians from the start ray: a pixel near the
// ray (`ahead` > 0) is split by it, the part after the ray swept at the start of the
// turn (angle `theta`) and the rest at its end (`theta` + one turn).
fn swept(theta: f32, split: f32, reach: f32, hand: f32, grow: f32, softness: f32) -> f32 {
    var early = 0.0;
    var late = 0.0;
    if split > 0.0 {
        early = edge((hand - theta) * reach + grow, softness);
    }
    if split < 1.0 {
        late = edge((hand - theta - TAU) * reach + grow, softness);
    }
    return split * early + (1.0 - split) * late;
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
    let softness = tr.p[3].x;
    let border = max(tr.p[4].x, 0.0);
    let margin = 0.5 * max(softness, 1.0) + border;
    let start = tr.p[0].x;
    let turn = select(1.0, -1.0, tr.p[1].x >= 0.5);
    // Along the start ray, and across it towards the sweep (y is down, so clockwise
    // from up is towards +x).
    let along = vec2<f32>(sin(start), -cos(start));
    let across = turn * vec2<f32>(cos(start), sin(start));
    let q = in.pos.xy - tr.p[2].xy * tr.size;
    let ahead = dot(q, along);
    let side = dot(q, across);
    // -pi..pi from the start ray, positive in the sweep.
    let theta = atan2(side, ahead);
    var split = select(0.0, 1.0, theta >= 0.0);
    if ahead > 0.0 {
        split = clamp(side + 0.5, 0.0, 1.0);
    }
    let reach = max(length(q), 2.0 * margin);
    let lead = margin / reach;
    let hand = -lead * (1.0 - t) + (TAU + lead) * t;
    let inner = swept(theta, split, reach, hand, 0.0, softness);
    let outer = swept(theta, split, reach, hand, border, softness);
    return wipe(in, inner, outer, tr.p[5]);
}
