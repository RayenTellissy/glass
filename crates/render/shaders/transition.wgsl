// Shared by every transition shader (`assets/transitions/<id>/shader.wgsl`): the
// library compiles common.wgsl, this file, then the transition's shader, which defines
// `fs`. A and B are premultiplied linear light in the working space; the output is the
// size of A. The uniform is built by `render::transition::TransitionNode`.

struct TransitionUniform {
    // Eased progress, 0 at the start (all A) to 1 at the end (all B). A cubic-bezier
    // easing may overshoot slightly outside 0..1.
    progress: f32,
    // Progress before easing, 0..1.
    linear: f32,
    // `render::transition::Direction` index: 0 left, 1 right, 2 up, 3 down, 4 up-left,
    // 5 up-right, 6 down-left, 7 down-right; 0 for a transition without directions.
    direction: u32,
    _pad: u32,
    // The direction as a unit vector in uv (x right, y down); zero without directions.
    dir: vec2<f32>,
    // Output size in pixels.
    size: vec2<f32>,
    // The schema's parameters in manifest order, one vec4 each: numbers and angles
    // (radians) in x, booleans as 0 or 1, choices as the option's index, points in xy,
    // colours as straight-alpha linear RGBA.
    p: array<vec4<f32>, 8>,
}

@group(0) @binding(0) var<uniform> tr: TransitionUniform;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var tex_a: texture_2d<f32>;
@group(0) @binding(3) var tex_b: texture_2d<f32>;

// A filtered at uv (clamped to the edge).
fn sample_a(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_a, samp, uv, 0.0);
}

// B filtered at uv (clamped to the edge).
fn sample_b(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_b, samp, uv, 0.0);
}

// A's texel under this output pixel, unfiltered.
fn here_a(in: VsOut) -> vec4<f32> {
    return textureLoad(tex_a, vec2<i32>(in.pos.xy), 0);
}

// B's texel under this output pixel, unfiltered; filtered at the same uv when B is not
// the output's size.
fn here_b(in: VsOut) -> vec4<f32> {
    if all(textureDimensions(tex_b) == textureDimensions(tex_a)) {
        return textureLoad(tex_b, vec2<i32>(in.pos.xy), 0);
    }
    return sample_b(in.uv);
}

// Edge helpers shared by the wipes.

// Coverage of B at a point `d` output pixels inside B's edge (negative outside): a 1 px
// box that anti-aliases a hard edge, widened to `softness` pixels and eased towards a
// smoothstep as the softness passes 1 px. Non-decreasing in d, 0 from half the width
// outside the edge and 1 from half the width inside.
fn edge(d: f32, softness: f32) -> f32 {
    let s = max(softness, 0.0);
    let x = clamp(d / max(s, 1.0) + 0.5, 0.0, 1.0);
    return mix(x, x * x * (3.0 - 2.0 * x), clamp(s - 1.0, 0.0, 1.0));
}

// A outside `outer`, the border colour (straight linear RGBA, premultiplied here)
// between `outer` and `inner`, B inside `inner` (outer >= inner). A weighted sum, so
// coverage 0 is exactly A and 1 exactly B.
fn wipe(in: VsOut, inner: f32, outer: f32, border: vec4<f32>) -> vec4<f32> {
    let c = vec4<f32>(border.rgb * border.a, border.a);
    return here_a(in) * (1.0 - outer) + c * (outer - inner) + here_b(in) * inner;
}

// Moved frames, shared by the motion transitions. `q` is a point in a frame's own
// output pixels (0..size inside, A's pixels; B is stretched to the same size), and
// `footprint` how many of them one output pixel spans (1 for a frame moved without
// scaling).

// Coverage of a frame at `q`: 1 inside, 0 outside, its edges anti-aliased over one
// output pixel, so whatever a moved frame leaves uncovered is transparent black.
fn frame_coverage(q: vec2<f32>, footprint: f32) -> f32 {
    let c = clamp(min(q, tr.size - q) / footprint + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
    return c.x * c.y;
}

// A moved so that its point `q` lies under this output pixel: filtered (the sampler
// clamps at the edge, so nothing wraps) and weighted by its coverage.
fn frame_a(q: vec2<f32>, footprint: f32) -> vec4<f32> {
    let c = frame_coverage(q, footprint);
    if c <= 0.0 {
        return vec4<f32>(0.0);
    }
    return sample_a(q / tr.size) * c;
}

// B moved so that its point `q` lies under this output pixel, as `frame_a`.
fn frame_b(q: vec2<f32>, footprint: f32) -> vec4<f32> {
    let c = frame_coverage(q, footprint);
    if c <= 0.0 {
        return vec4<f32>(0.0);
    }
    return sample_b(q / tr.size) * c;
}

// Premultiplied `top` over `bottom`.
fn over(top: vec4<f32>, bottom: vec4<f32>) -> vec4<f32> {
    return top + bottom * (1.0 - top.a);
}

// `v` turned by `angle` radians, clockwise on screen (y is down).
fn rotate(v: vec2<f32>, angle: f32) -> vec2<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return vec2<f32>(c * v.x - s * v.y, s * v.x + c * v.y);
}

// A fixed offset in -0.5..0.5 for the whole number `i` (interleaved gradient noise), for
// staggering blur taps.
fn jitter(i: f32) -> f32 {
    return fract(52.9829189 * fract(i * 0.06711056)) - 0.5;
}
