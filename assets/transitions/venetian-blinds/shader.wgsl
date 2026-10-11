// Venetian blinds: the frame is cut across the direction into `count` equal slats, and
// in each an edge travels in the direction from the slat's start to its end, B behind
// it. A slat holds the interval [start, start + e] of B; its coverage is the interval
// filtered by the edge's kernel (1 px box, or the softness, capped at the slat's
// width), summed over the slat and its nearer neighbour, so the edges between slats are
// anti-aliased too and the slats join without a seam once full. The border follows
// each travelling edge on A's side, narrowed near the start and the end of a slat so it
// appears and leaves without a jump. Parameters: count, softness and border width in
// output pixels, border colour.

// Coverage at `x` (0..h within its slat) of B's intervals [k h, k h + e] in this slat
// and the neighbour on the nearer side; the kernel is at most a slat wide, so the far
// neighbour adds exactly nothing.
fn slats(x: f32, h: f32, e: f32, softness: f32) -> f32 {
    let near = x - select(h, -h, x < 0.5 * h);
    let c = edge(x, softness) - edge(x - e, softness) + edge(near, softness) - edge(near - e, softness);
    return clamp(c, 0.0, 1.0);
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
    let count = max(round(tr.p[0].x), 1.0);
    let border = max(tr.p[2].x, 0.0);
    // Position along the direction of travel, 0 at the frame's first edge.
    let span = tr.dir * tr.size;
    let along = dot(in.pos.xy, tr.dir) - min(span.x, 0.0) - min(span.y, 0.0);
    let h = abs(span.x + span.y) / count;
    let x = along - floor(along / h) * h;
    let softness = min(tr.p[1].x, h);
    let e = h * t;
    let outer = e + min(border, min(e, h - e));
    return wipe(in, slats(x, h, e, softness), slats(x, h, outer, softness), tr.p[3]);
}
