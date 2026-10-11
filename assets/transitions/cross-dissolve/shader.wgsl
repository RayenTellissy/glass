// Cross dissolve: A fades out as B fades in, a per-pixel mix of the premultiplied
// pictures by the eased progress. Written as a weighted sum rather than `mix` so
// progress 0 is exactly A and progress 1 exactly B.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = tr.progress;
    return here_a(in) * (1.0 - t) + here_b(in) * t;
}
