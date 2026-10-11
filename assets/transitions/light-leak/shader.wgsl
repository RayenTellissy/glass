// Light leak: three warm glows sweep across the frame in the direction of travel,
// strongest half way (an envelope of sin(pi t)^2), hiding a dissolve from A to B over
// the middle half. Each glow is a soft gaussian blob of the colour (or a paler, yellower
// version of it), broken up by noise, its size, path and timing from the seed. The
// light is added in linear light and capped per channel like the additive dissolve, at
// the larger of the alpha and the picture's own value: SDR stays within white and HDR
// highlights keep their value but gain nothing. The approach to the cap is soft (an
// exponential knee), so bright glows do not band. Parameters: seed, colour, intensity.

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
    let w = smoothstep(0.25, 0.75, t);
    let base = here_a(in) * (1.0 - w) + here_b(in) * w;
    let key = seed_key(tr.p[0].x);
    let s = sin(PI * clamp(t, 0.0, 1.0));
    let strength = max(tr.p[2].x, 0.0) * s * s;
    // Positions in units of the frame's height, from the centre.
    let unit = tr.size.y;
    let p = (in.pos.xy - 0.5 * tr.size) / unit;
    let half_extent = 0.5 * abs(dot(tr.dir, tr.size)) / unit;
    let across = vec2<f32>(-tr.dir.y, tr.dir.x);
    let colour = max(tr.p[1].rgb, vec3<f32>(0.0));
    var light = vec3<f32>(0.0);
    for (var i = 0; i < 3; i += 1) {
        let h = vec3<f32>(
            hash_cell(vec2<i32>(i, 0), key),
            hash_cell(vec2<i32>(i, 1), key),
            hash_cell(vec2<i32>(i, 2), key),
        );
        let radius = 0.3 + 0.4 * h.x;
        // Each crosses from beyond one edge to beyond the other, a little early or late.
        let along = mix(-half_extent - radius, half_extent + radius, clamp(t + 0.3 * (h.y - 0.5), 0.0, 1.0));
        let centre = tr.dir * along + across * (h.z - 0.5) * 0.9;
        let d = p - centre;
        let g = exp(-dot(d, d) / (radius * radius));
        light += mix(colour, 0.5 * colour + vec3<f32>(0.5, 0.4, 0.2), h.z) * g;
    }
    let breakup = 0.6 + 0.8 * fbm(p * 2.5, key ^ 0x1eu, 3u);
    let add = light * breakup * strength * base.a;
    let cap = max(vec3<f32>(base.a), base.rgb);
    let room = max(cap - base.rgb, vec3<f32>(0.0));
    let rgb = base.rgb + room * (1.0 - exp(-add / max(room, vec3<f32>(1e-6))));
    return vec4<f32>(rgb, base.a);
}
