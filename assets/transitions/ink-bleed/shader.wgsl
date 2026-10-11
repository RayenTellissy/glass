// Ink bleed: B spreads over A like ink soaking into paper from three drops the seed
// places, along tendrils of warped fractal noise, with a soft edge (the softness, in
// the field's units: a fraction of two fifths of the diagonal) never narrower than one
// output pixel, and B a little darker along the wet edge. The field is the distance to
// the nearest drop (0 there, 1 from two fifths of the diagonal on) four fifths of it,
// plus a fifth of the noise, so it lies in 0..1, and the threshold sweeps from half the
// softness below it to half above: what is covered stays covered. Parameters: seed,
// softness.

fn field(p: vec2<f32>, key: u32) -> f32 {
    // Two fifths of the diagonal: past it the edge follows the noise alone.
    let reach = 0.4 * length(tr.size);
    var near = 1.0;
    for (var i = 0; i < 3; i += 1) {
        let drop = (0.15 + 0.7 * vec2<f32>(hash_cell(vec2<i32>(i, 0), key), hash_cell(vec2<i32>(i, 1), key))) * tr.size;
        near = min(near, length(p - drop) / reach);
    }
    let q = p / tr.size.y * 4.0;
    let warp = vec2<f32>(value_noise(q, key ^ 0x3au), value_noise(q + 5.2, key ^ 0x3bu)) - 0.5;
    let n = fbm(q + 2.5 * warp, key ^ 0x3cu, 4u);
    // The noise added on top makes the edge ragged.
    return 0.8 * near + 0.2 * n;
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
    let soft = clamp(tr.p[1].x, 0.0, 1.0);
    let key = seed_key(tr.p[0].x);
    let threshold = mix(-0.5 * soft - 0.01, 1.0 + 0.5 * soft + 0.01, t);
    let c = field_edge(threshold - field(in.pos.xy, key), soft);
    let b = here_b(in);
    let wet = 1.0 - 0.6 * c * (1.0 - c);
    return here_a(in) * (1.0 - c) + vec4<f32>(b.rgb * wet, b.a) * c;
}
