// Film burn: A burns away from two points the seed places, the front ragged with noise
// that coarsens away from them, revealing B. Just ahead of the front A chars dark; at
// the front a band glows, emissive light added to the picture (the glow colour times
// the glow strength, so above 1 is HDR light), fading into B behind it. The field is
// the distance to the nearer point (0 there, 1 from half the diagonal on) scaled by
// 0.35 to 1 of fractal noise, under a square root, so it lies in 0..1, and the
// threshold sweeps from below the char band to past the glow band: what has burnt never
// unburns. The front is anti-aliased over one output pixel. Parameters: seed, glow,
// glow colour.

// How far ahead of the front A chars, and behind it the glow reaches, in field units.
const CHAR: f32 = 0.06;
const GLOW: f32 = 0.05;

// The field at the output point `p` in pixels: 0 at a burn's start, 1 at most.
fn field(p: vec2<f32>, key: u32) -> f32 {
    // Half the diagonal: past it the burn front is the noise alone.
    let reach = 0.5 * length(tr.size);
    var near = 1.0;
    for (var i = 0; i < 2; i += 1) {
        let spot = (0.2 + 0.6 * vec2<f32>(hash_cell(vec2<i32>(i, 0), key), hash_cell(vec2<i32>(i, 1), key))) * tr.size;
        near = min(near, length(p - spot) / reach);
    }
    let n = fbm(p / tr.size.y * 3.0, key ^ 0xb1u, 4u);
    // The square root spreads the many small values, so the burn starts slowly and
    // takes most of the transition to finish.
    return sqrt(near * (0.35 + 0.65 * n));
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
    let key = seed_key(tr.p[0].x);
    let threshold = mix(-CHAR - 0.01, 1.0 + GLOW + 0.01, t);
    let s = threshold - field(in.pos.xy, key);
    let burnt = field_edge(s, 0.0);
    let char = smoothstep(-CHAR, 0.0, s);
    let a = here_a(in);
    let charred = vec4<f32>(a.rgb * (1.0 - 0.9 * char * char), a.a);
    let picture = charred * (1.0 - burnt) + here_b(in) * burnt;
    // The glow: light added at the front in proportion to the coverage, gone a band
    // behind it. It adds no coverage of its own.
    let g = burnt * (1.0 - smoothstep(0.0, GLOW, s));
    let glow = max(tr.p[2].rgb, vec3<f32>(0.0)) * max(tr.p[1].x, 0.0) * g * picture.a;
    return vec4<f32>(picture.rgb + glow, picture.a);
}
