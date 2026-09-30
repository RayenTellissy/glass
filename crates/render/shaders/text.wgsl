// Text shadow and colour passes. The coverage input holds fill coverage in red, outline
// coverage in green and their union in blue (see text_glyph.wgsl).

// One layout for every pass; each reads the fields it needs. Colours are linear light
// with straight alpha.
struct TextParams {
    fill: vec4<f32>,
    outline: vec4<f32>,
    shadow: vec4<f32>,
    // Shadow offset in pixels (applied by the horizontal pass only), then the target size.
    offset: vec2<f32>,
    size: vec2<f32>,
    // Gaussian standard deviation in pixels, kernel radius in pixels.
    sigma: f32,
    radius: f32,
}

@group(0) @binding(0) var<uniform> params: TextParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src: texture_2d<f32>;
@group(0) @binding(3) var shadow_tex: texture_2d<f32>;

fn gauss(x: f32) -> f32 {
    return exp(-0.5 * x * x / max(params.sigma * params.sigma, 1e-6));
}

// Channel `c` of `src` at pixel position `p`, filtered, zero outside the texture.
fn load_at(p: vec2<f32>, c: u32) -> f32 {
    if (p.x < 0.0 || p.y < 0.0 || p.x > params.size.x || p.y > params.size.y) {
        return 0.0;
    }
    return textureSampleLevel(src, samp, p / params.size, 0.0)[c];
}

// A Gaussian of `radius` taps each side along `dir` around the texel centre `p`, two taps
// per fetch: bilinear filtering between neighbouring texels weights them as the kernel
// does.
fn blur(p: vec2<f32>, dir: vec2<f32>, c: u32) -> f32 {
    let r = i32(params.radius);
    var sum = 0.0;
    var total = 0.0;
    for (var k = -r; k <= r; k = k + 2) {
        let w1 = gauss(f32(k));
        var w2 = 0.0;
        if (k + 1 <= r) {
            w2 = gauss(f32(k + 1));
        }
        let w = w1 + w2;
        sum = sum + w * load_at(p + dir * (f32(k) + w2 / w), c);
        total = total + w;
    }
    return sum / total;
}

// The horizontal pass shifts by the whole pixels of the offset, so every fetch is
// texel-aligned; the fraction is applied when the shadow is composited.
@fragment
fn fs_shadow_h(in: VsOut) -> @location(0) vec4<f32> {
    let v = blur(in.pos.xy - floor(params.offset), vec2<f32>(1.0, 0.0), 2u);
    return vec4<f32>(v, 0.0, 0.0, 0.0);
}

@fragment
fn fs_shadow_v(in: VsOut) -> @location(0) vec4<f32> {
    let v = blur(in.pos.xy, vec2<f32>(0.0, 1.0), 0u);
    return vec4<f32>(v, 0.0, 0.0, 0.0);
}

fn premultiply(c: vec4<f32>, coverage: f32) -> vec4<f32> {
    let a = c.a * coverage;
    return vec4<f32>(c.rgb * a, a);
}

@fragment
fn fs_paint(in: VsOut) -> @location(0) vec4<f32> {
    let cov = textureLoad(src, vec2<i32>(in.pos.xy), 0);
    let fill = premultiply(params.fill, cov.r);
    let outline = premultiply(params.outline, cov.g);
    return fill + (1.0 - fill.a) * outline;
}

@fragment
fn fs_paint_shadow(in: VsOut) -> @location(0) vec4<f32> {
    let pos = vec2<i32>(in.pos.xy);
    let cov = textureLoad(src, pos, 0);
    let fill = premultiply(params.fill, cov.r);
    let outline = premultiply(params.outline, cov.g);
    let at = (in.pos.xy - fract(params.offset)) / params.size;
    let behind = premultiply(params.shadow, textureSampleLevel(shadow_tex, samp, at, 0.0).r);
    return fill + (1.0 - fill.a) * (outline + (1.0 - outline.a) * behind);
}
