// Shared by every transition shader (`assets/transitions/<id>/shader.wgsl`): the
// library compiles common.wgsl, this file, then the transition's shader, which defines
// `fs` and the entry points of the extra passes its manifest lists. A and B are
// premultiplied linear light in the working space; the output is the size of A. The
// uniform is built by `render::transition::TransitionNode`; every pass sees the same
// one but for `pass_index` and `pass_size`.

struct TransitionUniform {
    // Eased progress, 0 at the start (all A) to 1 at the end (all B). A cubic-bezier
    // easing may overshoot slightly outside 0..1.
    progress: f32,
    // Progress before easing, 0..1.
    linear: f32,
    // `render::transition::Direction` index: 0 left, 1 right, 2 up, 3 down, 4 up-left,
    // 5 up-right, 6 down-left, 7 down-right; 0 for a transition without directions.
    direction: u32,
    // The pass being drawn: the index of one of the manifest's extra passes, or their
    // count (0 for a transition without any) in `fs`.
    pass_index: u32,
    // The direction as a unit vector in uv (x right, y down); zero without directions.
    dir: vec2<f32>,
    // Output size in pixels.
    size: vec2<f32>,
    // The size in pixels of the target being drawn: the pass's own, `size` in `fs`.
    pass_size: vec2<f32>,
    // The schema's parameters in manifest order, one vec4 each: numbers and angles
    // (radians) in x, booleans as 0 or 1, choices as the option's index, points in xy,
    // colours as straight-alpha linear RGBA.
    p: array<vec4<f32>, 8>,
}

@group(0) @binding(0) var<uniform> tr: TransitionUniform;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var tex_a: texture_2d<f32>;
@group(0) @binding(3) var tex_b: texture_2d<f32>;
// The targets of the manifest's extra passes, in order. A pass reads only those before
// it; the others (its own included) are a 1x1 placeholder while it draws.
@group(0) @binding(4) var pass0: texture_2d<f32>;
@group(0) @binding(5) var pass1: texture_2d<f32>;
@group(0) @binding(6) var pass2: texture_2d<f32>;
@group(0) @binding(7) var pass3: texture_2d<f32>;
@group(0) @binding(8) var pass4: texture_2d<f32>;
@group(0) @binding(9) var pass5: texture_2d<f32>;
@group(0) @binding(10) var pass6: texture_2d<f32>;
@group(0) @binding(11) var pass7: texture_2d<f32>;

// A filtered at uv (clamped to the edge).
fn sample_a(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_a, samp, uv, 0.0);
}

// B filtered at uv (clamped to the edge).
fn sample_b(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_b, samp, uv, 0.0);
}

// The target of pass `i` filtered at uv (clamped to the edge). A switch over the
// targets, which the GPU's compiler may flatten into a read of each: in a loop, read the
// target itself (`pass0`, ..., or through a `texture_2d<f32>` parameter, `sum_taps`).
fn sample_pass(i: u32, uv: vec2<f32>) -> vec4<f32> {
    switch i {
        case 0u: { return textureSampleLevel(pass0, samp, uv, 0.0); }
        case 1u: { return textureSampleLevel(pass1, samp, uv, 0.0); }
        case 2u: { return textureSampleLevel(pass2, samp, uv, 0.0); }
        case 3u: { return textureSampleLevel(pass3, samp, uv, 0.0); }
        case 4u: { return textureSampleLevel(pass4, samp, uv, 0.0); }
        case 5u: { return textureSampleLevel(pass5, samp, uv, 0.0); }
        case 6u: { return textureSampleLevel(pass6, samp, uv, 0.0); }
        default: { return textureSampleLevel(pass7, samp, uv, 0.0); }
    }
}

// The sum of `n` filtered reads of `t` from uv in steps of `step` (in uv).
fn sum_taps(t: texture_2d<f32>, uv: vec2<f32>, step: vec2<f32>, n: f32) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var i = 0.0; i < n; i += 1.0) {
        sum += textureSampleLevel(t, samp, uv + i * step, 0.0);
    }
    return sum;
}

// The size in pixels of pass `i`'s target (1 by 1 while it is not readable).
fn size_of_pass(i: u32) -> vec2<f32> {
    switch i {
        case 0u: { return vec2<f32>(textureDimensions(pass0)); }
        case 1u: { return vec2<f32>(textureDimensions(pass1)); }
        case 2u: { return vec2<f32>(textureDimensions(pass2)); }
        case 3u: { return vec2<f32>(textureDimensions(pass3)); }
        case 4u: { return vec2<f32>(textureDimensions(pass4)); }
        case 5u: { return vec2<f32>(textureDimensions(pass5)); }
        case 6u: { return vec2<f32>(textureDimensions(pass6)); }
        default: { return vec2<f32>(textureDimensions(pass7)); }
    }
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

// Procedural noise, shared by the stylised transitions. Integer hashing only, so a seed
// gives the same pattern on every GPU and every run.

// A well mixed 32-bit hash (PCG output permutation).
fn hash_u32(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

// The seed parameter `x` (a whole number stored as a float) as a hash key.
fn seed_key(x: f32) -> u32 {
    return hash_u32(u32(max(round(x), 0.0)) ^ 0x9e3779b9u);
}

// A value in 0..1 (never 1) for the lattice cell `c` under the key `key`.
fn hash_cell(c: vec2<i32>, key: u32) -> f32 {
    let h = hash_u32(bitcast<u32>(c.x) ^ hash_u32(bitcast<u32>(c.y) ^ key));
    return f32(h >> 8u) / 16777216.0;
}

// Value noise in 0..1 at `p` (lattice cells of size 1): the cells' hashes blended by a
// quintic fade, so it is smooth with continuous slopes.
fn value_noise(p: vec2<f32>, key: u32) -> f32 {
    let i = vec2<i32>(floor(p));
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = hash_cell(i, key);
    let b = hash_cell(i + vec2<i32>(1, 0), key);
    let c = hash_cell(i + vec2<i32>(0, 1), key);
    let d = hash_cell(i + vec2<i32>(1, 1), key);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// `octaves` of value noise, each twice the frequency and half the weight of the one
// before and keyed apart, normalised to 0..1.
fn fbm(p: vec2<f32>, key: u32, octaves: u32) -> f32 {
    var sum = 0.0;
    var weight = 1.0;
    var total = 0.0;
    var q = p;
    for (var i = 0u; i < octaves; i += 1u) {
        sum += weight * value_noise(q, hash_u32(key + i));
        total += weight;
        weight *= 0.5;
        // Turned a little each octave so the lattices do not line up.
        q = vec2<f32>(1.6 * q.x - 1.2 * q.y, 1.2 * q.x + 1.6 * q.y) + 17.0;
    }
    return sum / total;
}

// Coverage of B where `s` is how far a field lies past its threshold (positive on B's
// side, in the field's own units): a ramp `softness` units wide (in the same units),
// never narrower than one output pixel (from the field's screen slope), eased to a
// smoothstep as it widens. Non-decreasing in s; 0 from half the width below and 1 from
// half the width above. Call it in uniform control flow (it takes derivatives).
fn field_edge(s: f32, softness: f32) -> f32 {
    let px = max(fwidth(s), 1e-6);
    return edge(s / px, max(softness, 0.0) / px);
}
