// Iris: a shape centred in the frame opens from nothing until B fills the frame, or
// closes from beyond the frame until A is gone. Shapes: a circle, a diamond (a square
// at 45 degrees), a rectangle of the frame's aspect, and a five-pointed star pointing
// up. Each is a signed distance in output pixels from its outline, for a size that
// starts (open) or ends (close) a margin below zero, where the shape has shrunk to a
// point and keeps receding, and ends (open) or starts (close) where the farthest
// corner is inside it by the margin, the softness's half width plus the border.
// Parameters: shape, mode, softness and border width in output pixels, border colour.

const SQRT_HALF: f32 = 0.7071067811865476;
// The star's inner corners lie at this fraction of its outer radius.
const STAR_INNER: f32 = 0.45;

fn box_distance(q: vec2<f32>, half_size: vec2<f32>) -> f32 {
    let d = abs(q) - half_size;
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);
}

// A five-pointed star of outer radius `r` with its inner corners at `r * STAR_INNER`,
// pointing towards -y (the exact distance by folding into one edge). Bounded below by
// the circle through its points, which also keeps it right as r reaches 0.
fn star_distance(q: vec2<f32>, r: f32) -> f32 {
    let k1 = vec2<f32>(0.809016994375, -0.587785252292);
    let k2 = vec2<f32>(-k1.x, k1.y);
    var p = vec2<f32>(abs(q.x), -q.y);
    p -= 2.0 * max(dot(k1, p), 0.0) * k1;
    p -= 2.0 * max(dot(k2, p), 0.0) * k2;
    p.x = abs(p.x);
    p.y -= r;
    let ba = STAR_INNER * vec2<f32>(-k1.y, k1.x) - vec2<f32>(0.0, 1.0);
    let h = clamp(dot(p, ba) / dot(ba, ba), 0.0, r);
    let d = length(p - ba * h) * sign(p.y * ba.x - p.x * ba.y);
    return max(d, length(q) - r);
}

// Signed distance from the shape of size `s` (negative inside); below zero the point
// shape is grown outwards by -s.
fn shape_distance(shape: f32, q: vec2<f32>, s: f32) -> f32 {
    let r = max(s, 0.0);
    var d = length(q) - r;
    if shape >= 2.5 {
        d = star_distance(q, r);
    } else if shape >= 1.5 {
        d = box_distance(q, vec2<f32>(r, r * tr.size.y / tr.size.x));
    } else if shape >= 0.5 {
        d = (abs(q.x) + abs(q.y)) * SQRT_HALF - r;
    }
    return d - min(s, 0.0);
}

// The size at which the shape holds every corner of the frame `margin` inside.
fn full_size(shape: f32, margin: f32) -> f32 {
    let half_size = 0.5 * tr.size;
    if shape >= 2.5 {
        return (length(half_size) + margin) / STAR_INNER;
    } else if shape >= 1.5 {
        return max(half_size.x + margin, (half_size.y + margin) * tr.size.x / tr.size.y);
    } else if shape >= 0.5 {
        return (half_size.x + half_size.y) * SQRT_HALF + margin;
    }
    return length(half_size) + margin;
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
    let shape = tr.p[0].x;
    let close = tr.p[1].x >= 0.5;
    let softness = tr.p[2].x;
    let border = max(tr.p[3].x, 0.0);
    let margin = 0.5 * max(softness, 1.0) + border;
    let q = in.pos.xy - 0.5 * tr.size;
    let full = full_size(shape, margin);
    if close {
        // B outside the shape, the border inside it on A's side.
        let d = shape_distance(shape, q, full * (1.0 - t) - margin * t);
        return wipe(in, edge(d, softness), edge(d + border, softness), tr.p[4]);
    }
    let d = -shape_distance(shape, q, -margin * (1.0 - t) + full * t);
    return wipe(in, edge(d, softness), edge(d + border, softness), tr.p[4]);
}
