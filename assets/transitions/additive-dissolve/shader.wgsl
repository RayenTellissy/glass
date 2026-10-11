// Additive dissolve: A and B are added, A at full strength until half way and then
// ramping down, B ramping up to full strength by half way. In the middle both are at
// full strength, so the picture is brighter than a cross dissolve. The sum is capped per
// channel at the larger of the alpha (white in premultiplied terms) and the brighter
// weighted input: SDR pictures stay within white, and HDR highlights above 1 keep their
// value but are not pushed further (Decision 20 keeps highlights rather than clipping
// them). Progress 0 is exactly A and progress 1 exactly B.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    let a = here_a(in) * clamp(2.0 * (1.0 - t), 0.0, 1.0);
    let b = here_b(in) * clamp(2.0 * t, 0.0, 1.0);
    let alpha = min(a.a + b.a, 1.0);
    let cap = max(vec3<f32>(alpha), max(a.rgb, b.rgb));
    return vec4<f32>(min(a.rgb + b.rgb, cap), alpha);
}
