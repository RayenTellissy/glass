// Paper tear: A tears away along a ragged edge travelling in the direction, B behind
// it. The edge is offset across its length by fractal noise (up to the roughness, a
// fraction of the frame's shorter side, either way); A's torn lip shows the paper's
// pale core, brightest at the very edge (a highlight) and fibrous, and the lifted paper
// casts a shadow onto B just behind the edge. The lip and the paper take A's coverage,
// and the shadow darkens B's colour only, so coverage is A's until the edge passes and
// B's after. The edge starts and ends far enough outside the frame that nothing of the
// lip or shadow shows at the ends; it is anti-aliased over one output pixel.
// Parameters: seed, roughness.

// The paper's core, linear light.
const PAPER: vec3<f32> = vec3<f32>(0.82, 0.79, 0.72);

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
    let short = min(tr.size.x, tr.size.y);
    let dir = normalize(tr.dir);
    let across = vec2<f32>(-dir.y, dir.x);
    let d = in.pos.xy - 0.5 * tr.size;
    let along = dot(d, dir);
    let v = dot(d, across) / short;
    let rough = clamp(tr.p[1].x, 0.0, 1.0) * short;
    let lip = max(0.012 * short, 1.5);
    let shadow = max(0.03 * short, 2.0);
    let half_extent = 0.5 * dot(abs(dir), tr.size);
    let margin = rough + lip + shadow + 2.0;
    let front = mix(-half_extent - margin, half_extent + margin, t);
    let jag = rough * (2.0 * fbm(vec2<f32>(v * 8.0, 0.5), key, 5u) - 1.0);
    // Pixels past the edge (B's side positive).
    let s = front - along - jag;
    // The lip's width varies along the edge, like torn fibres.
    let fibre = value_noise(vec2<f32>(v * 90.0, 3.5), key ^ 0x77u);
    let width = lip * (0.4 + 0.9 * fibre);
    // One pixel's anti-aliasing from the edge's screen slope, the same for both.
    let px = max(fwidth(s), 1e-6);
    let b_cover = edge(s / px, 0.0);
    let torn = edge((s + width) / px, 0.0);
    let a = here_a(in);
    let b = here_b(in);
    // The highlight: the core brighter towards the edge.
    let bright = mix(0.8, 1.15, clamp(1.0 + s / width, 0.0, 1.0));
    let core = vec4<f32>(min(PAPER * bright, vec3<f32>(1.0)) * a.a, a.a);
    let dark = 0.55 * (1.0 - smoothstep(0.0, shadow, s));
    let shaded = vec4<f32>(b.rgb * (1.0 - dark), b.a);
    return a * (1.0 - torn) + core * (torn - b_cover) + shaded * b_cover;
}
