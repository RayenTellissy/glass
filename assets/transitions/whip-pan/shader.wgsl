// Whip pan: a push that starts slowly, crosses the cut fast and settles (a quintic
// smootherstep), blurred along the direction by a box as long as the blur parameter's
// fraction of the frame at the fastest point, shorter in proportion to the speed
// elsewhere. The blur averages up to `samples` evenly spaced filtered taps of the strip
// A and B form edge to edge, each extended beyond its far edge by the clamping sampler,
// so nothing transparent is ever averaged in and a flat picture keeps its level; below
// a pixel of blur it is a plain push. With fewer taps than pixels of blur, each row
// across the direction staggers its taps, so the gaps show as streaks along the motion
// rather than ghost copies. Lengths are fractions of the frame, so the look does not
// depend on the resolution. Parameters: blur, samples.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    if t <= 0.0 {
        return here_a(in);
    }
    if t >= 1.0 {
        return here_b(in);
    }
    let travel = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    // The travel's speed relative to its peak at the middle.
    let speed = 16.0 * t * t * (1.0 - t) * (1.0 - t);
    let span = tr.dir * tr.size;
    let q = in.pos.xy - travel * span;
    let blur = max(tr.p[0].x, 0.0) * speed * abs(span.x + span.y);
    if blur <= 1.0 {
        return frame_a(q, 1.0) + frame_b(q + span, 1.0);
    }
    let n = clamp(ceil(blur), 2.0, max(round(tr.p[1].x), 2.0));
    let step = blur / n;
    // Taps more than a pixel apart are staggered by up to a step per row across the
    // direction (per pixel would scatter the reads and cost several times as much).
    let row = dot(floor(in.pos.xy), abs(vec2<f32>(tr.dir.y, tr.dir.x)));
    let stagger = jitter(row) * clamp(step - 1.0, 0.0, 1.0) * step;
    let p = q + stagger * tr.dir;
    // Tap i lies `first - i * step` along the direction in A; the taps from `k` on are
    // behind the seam, in B (edge to edge behind A).
    let first = dot(p, tr.dir) + 0.5 * blur - 0.5 * step;
    let seam = min(span.x, 0.0) + min(span.y, 0.0);
    let k = u32(clamp(floor((first - seam) / step) + 1.0, 0.0, n));
    let start = (p + (0.5 * blur - 0.5 * step) * tr.dir) / tr.size;
    let stride = -step * tr.dir / tr.size;
    var sum = vec4<f32>(0.0);
    for (var i = 0u; i < k; i++) {
        sum += sample_a(start + f32(i) * stride);
    }
    let behind = start + tr.dir;
    for (var i = k; i < u32(n); i++) {
        sum += sample_b(behind + f32(i) * stride);
    }
    return sum / n;
}
