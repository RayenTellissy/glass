// Whip pan: a push that starts slowly, crosses the cut fast and settles (a quintic
// smootherstep), blurred along the direction by a box as long as the blur parameter's
// fraction of the frame at the fastest point, shorter in proportion to the speed
// elsewhere. The blur averages the strip A and B form edge to edge, each extended
// beyond its far edge by the clamping sampler, so nothing transparent is ever averaged
// in and a flat picture keeps its level; below a pixel of blur it is a plain push.
// Lengths are fractions of the frame, so the look does not depend on the resolution.
//
// Up to `samples` pixels of blur, `fs` takes one tap a pixel. A longer blur is made at
// a reduced resolution along the direction (across it the resolution is kept): `strip`
// lays out the strip, two frames over a quarter of the output's extent (at least 256
// texels), each texel a box over its cell; three `smear` passes each average up to
// `samples` taps, spaced by that factor more each time, so together they are one evenly
// spaced comb of up to samples^3 taps as long as the blur; `fs` reads the last one,
// filtered, mixing it in from `samples` to twice that many pixels of blur. Parameters:
// blur, samples.

// The motion at the current progress.
struct Whip {
    // How far the frames have moved, in frames.
    travel: f32,
    // One frame's step along the direction, in pixels, and its length.
    span: vec2<f32>,
    extent: f32,
    // The blur's length in output pixels.
    blur: f32,
    samples: f32,
    // Where A ends and B begins behind it, along the direction (`dot(q, tr.dir)` with
    // `q` in A's pixels).
    seam: f32,
    // How much of the result comes from the reduced passes.
    reduced: f32,
}

fn whip() -> Whip {
    var w: Whip;
    let t = tr.progress;
    w.travel = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    // The travel's speed relative to its peak at the middle.
    let speed = 16.0 * t * t * (1.0 - t) * (1.0 - t);
    w.span = tr.dir * tr.size;
    w.extent = abs(w.span.x + w.span.y);
    w.blur = max(tr.p[0].x, 0.0) * speed * w.extent;
    w.samples = max(round(tr.p[1].x), 2.0);
    w.seam = min(w.span.x, 0.0) + min(w.span.y, 0.0);
    w.reduced = clamp(w.blur / w.samples - 1.0, 0.0, 1.0);
    return w;
}

// The reduced passes' texel along the direction, in output pixels, for targets of
// `texels`: the strip's two frames over their extent.
fn cell_of(w: Whip, texels: vec2<f32>) -> f32 {
    return 2.0 * w.extent / dot(texels, abs(tr.dir));
}

// The point of A's pixels at `s` along the direction and `y` across.
fn strip_point(s: f32, y: f32) -> vec2<f32> {
    let axis = abs(tr.dir);
    return axis * (s * dot(tr.dir, axis)) + axis.yx * y;
}

// The strip filtered at `s` along the direction and the pixel centre `y` across: A from
// the seam on, B behind it, each clamped beyond its far edge; across the seam, as a
// bilinear read would, its edge pixel's share from each frame.
fn strip_at(w: Whip, s: f32, y: f32) -> vec4<f32> {
    let in_a = clamp(s - w.seam + 0.5, 0.0, 1.0);
    var sum = vec4<f32>(0.0);
    if in_a > 0.0 {
        sum += sample_a(strip_point(max(s, w.seam + 0.5), y) / tr.size) * in_a;
    }
    if in_a < 1.0 {
        let q = strip_point(min(s, w.seam - 0.5), y);
        sum += sample_b((q + w.span) / tr.size) * (1.0 - in_a);
    }
    return sum;
}

// Each smear's taps: enough for a comb of three smears at most a texel apart over
// `length` texels, and at most `samples`.
fn smear_taps(length: f32, samples: f32) -> f32 {
    return clamp(ceil(pow(length, 1.0 / 3.0) - 1e-3), 1.0, samples);
}

// The strip, a box over each texel's cell, where `fs` can reach it: the part on screen
// with half the blur and the smears' filtering either side. Elsewhere, and while the
// reduced passes are not used, nothing is read.
@fragment
fn strip(in: VsOut) -> @location(0) vec4<f32> {
    let w = whip();
    if w.reduced <= 0.0 {
        return vec4<f32>(0.0);
    }
    let axis = abs(tr.dir);
    let cell = cell_of(w, tr.pass_size);
    let s = w.seam - w.extent + dot(in.pos.xy, axis) * cell;
    let shown = w.seam - w.travel * w.extent;
    let reach = 0.5 * w.blur + 6.0 * cell;
    if s < shown - reach || s > shown + w.extent + reach {
        return vec4<f32>(0.0);
    }
    // Taps two pixels apart, each a filtered read of two.
    let n = max(ceil(0.5 * cell), 1.0);
    let y = dot(in.pos.xy, axis.yx);
    let first = s + (0.5 / n - 0.5) * cell;
    let last = s + (0.5 - 0.5 / n) * cell;
    let step = tr.dir * (cell / n) / tr.size;
    if first - 0.5 >= w.seam {
        return sum_taps(tex_a, strip_point(first, y) / tr.size, step, n) / n;
    }
    if last + 0.5 <= w.seam {
        return sum_taps(tex_b, (strip_point(first, y) + w.span) / tr.size, step, n) / n;
    }
    var sum = vec4<f32>(0.0);
    for (var i = 0.0; i < n; i += 1.0) {
        sum += strip_at(w, first + i * cell / n, y);
    }
    return sum / n;
}

// One of three smears of the pass before (pass 1 smears the strip): `n` taps along the
// direction, n^(k - 1) times the comb's spacing apart in smear k.
@fragment
fn smear(in: VsOut) -> @location(0) vec4<f32> {
    let w = whip();
    if w.reduced <= 0.0 {
        return vec4<f32>(0.0);
    }
    let axis = abs(tr.dir);
    let cell = cell_of(w, tr.pass_size);
    // The comb's length in texels: the blur less the strip's own box.
    let comb = max(w.blur - cell, 0.0) / cell;
    let n = smear_taps(comb, w.samples);
    let spacing = comb / (n * n * n) * pow(n, f32(tr.pass_index) - 1.0);
    let uv = (in.pos.xy - axis * (0.5 * (n - 1.0) * spacing)) / tr.pass_size;
    let step = axis * spacing / tr.pass_size;
    switch tr.pass_index {
        case 1u: {
            return sum_taps(pass0, uv, step, n) / n;
        }
        case 2u: {
            return sum_taps(pass1, uv, step, n) / n;
        }
        default: {
            return sum_taps(pass2, uv, step, n) / n;
        }
    }
}

// The blur at A's point `q` from one tap of the strip a pixel.
fn dense(w: Whip, q: vec2<f32>) -> vec4<f32> {
    let n = max(ceil(w.blur), 2.0);
    let step = w.blur / n;
    // Tap i lies `first - i * step` along the direction in A; the taps from `k` on are
    // behind the seam, in B (edge to edge behind A).
    let first = dot(q, tr.dir) + 0.5 * w.blur - 0.5 * step;
    let k = u32(clamp(floor((first - w.seam) / step) + 1.0, 0.0, n));
    let start = (q + (0.5 * w.blur - 0.5 * step) * tr.dir) / tr.size;
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

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    if t <= 0.0 {
        return here_a(in);
    }
    if t >= 1.0 {
        return here_b(in);
    }
    let w = whip();
    let q = in.pos.xy - w.travel * w.span;
    if w.blur <= 1.0 {
        return frame_a(q, 1.0) + frame_b(q + w.span, 1.0);
    }
    var direct = vec4<f32>(0.0);
    if w.reduced < 1.0 {
        direct = dense(w, q);
    }
    var reduced = vec4<f32>(0.0);
    if w.reduced > 0.0 {
        // The last smear at this point of the strip.
        let axis = abs(tr.dir);
        let texels = vec2<f32>(textureDimensions(pass3));
        let u = (dot(q, tr.dir) - (w.seam - w.extent)) / cell_of(w, texels);
        let uv = (axis * u + axis.yx * dot(in.pos.xy, axis.yx)) / texels;
        reduced = textureSampleLevel(pass3, samp, uv, 0.0);
    }
    return mix(direct, reduced, w.reduced);
}
