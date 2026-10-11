// Glitch: the picture breaks into horizontal bands of blocks that jump sideways by
// whole pixels and tear their red and blue apart, more of them and further towards the
// middle (an envelope of sin(pi t) times the amount). The pattern changes 24 times over
// the transition. Untouched bands cut from A to B half way; each block of a broken band
// shows B with a chance equal to the progress, so B flickers in early and A lingers
// late. Offsets are whole pixels, so block edges stay on pixel boundaries and nothing
// is filtered. The pattern is the same for the same seed. Parameters: seed, amount.

const PI: f32 = 3.141592653589793;

// A or B (premultiplied) at the output point `p` in pixels.
fn source(b: bool, p: vec2<f32>) -> vec4<f32> {
    let uv = p / tr.size;
    if b {
        return sample_b(uv);
    }
    return sample_a(uv);
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
    let e = clamp(tr.p[1].x, 0.0, 1.0) * sin(PI * clamp(t, 0.0, 1.0));
    let frame = u32(clamp(floor(tr.linear * 24.0), 0.0, 23.0));
    let key = hash_u32(seed_key(tr.p[0].x) + frame);
    // Bands a 24th of the height (whole pixels, at least 2).
    let unit = max(floor(tr.size.y / 24.0), 2.0);
    let band = floor(in.pos.y / unit);
    let broken = hash_cell(vec2<i32>(0, i32(band)), key) < 0.75 * e;
    var shift = 0.0;
    var split = round(0.004 * e * tr.size.x);
    var b = t >= 0.5;
    if broken {
        // Blocks 2 to 12 units wide along the band, each its own source.
        let width = unit * floor(2.0 + 10.0 * hash_cell(vec2<i32>(1, i32(band)), key));
        let block = floor(in.pos.x / width);
        let cell = vec2<i32>(i32(block), i32(band));
        shift = round((hash_cell(cell, key ^ 0x51u) - 0.5) * 0.3 * e * tr.size.x);
        split = round((hash_cell(cell, key ^ 0xa3u) - 0.5) * 0.06 * e * tr.size.x);
        b = hash_cell(cell, key ^ 0x7fu) < t;
    }
    let p = in.pos.xy + vec2<f32>(shift, 0.0);
    let r = source(b, p + vec2<f32>(split, 0.0));
    let g = source(b, p);
    let bl = source(b, p - vec2<f32>(split, 0.0));
    // Each channel keeps its own coverage; the most covered of the three is the alpha,
    // so no channel exceeds it.
    return vec4<f32>(r.r, g.g, bl.b, max(max(r.a, g.a), bl.a));
}
