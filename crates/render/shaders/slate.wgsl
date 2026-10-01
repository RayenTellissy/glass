// The offline media slate's background: the viewer ground, three soft colour glows seen
// through a dot screen, and the glass card (drop shadow, frosted fill, top highlight,
// edge) with the offline icon. Opaque linear light; the card's text is composited on top
// by the text passes. `render::slate::reference` is the CPU version of this shader.

struct SlateParams {
    // width, height, scale (design px to canvas px), dot cell
    size: vec4<f32>,
    // dot hole radius, dot screen's average cover, card tint amount, glass gradient stop
    dots: vec4<f32>,
    // card x0, y0, x1, y1
    card: vec4<f32>,
    // corner radius, edge width, shadow y offset, shadow sigma
    card_style: vec4<f32>,
    // icon mask x, y, width, height in canvas pixels
    icon: vec4<f32>,
    bg0: vec4<f32>,
    bg1: vec4<f32>,
    glass_top: vec4<f32>,
    glass_bottom: vec4<f32>,
    glass_edge: vec4<f32>,
    glass_highlight: vec4<f32>,
    glass_shadow: vec4<f32>,
    icon_color: vec4<f32>,
    // glows bottom to top: colour, then centre and radii as fractions of the frame
    glow0_color: vec4<f32>,
    glow0: vec4<f32>,
    glow1_color: vec4<f32>,
    glow1: vec4<f32>,
    glow2_color: vec4<f32>,
    glow2: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: SlateParams;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var icon_mask: texture_2d<f32>;

// Abramowitz and Stegun 7.1.26 (error under 1.5e-7).
fn erf1(x: f32) -> f32 {
    let s = sign(x);
    let a = abs(x);
    let t = 1.0 / (1.0 + 0.3275911 * a);
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * exp(-a * a);
    return s * y;
}

// A rectangle blurred by a Gaussian of standard deviation `sigma`.
fn blurred_rect(p: vec2<f32>, r: vec4<f32>, sigma: f32) -> f32 {
    let k = 1.0 / (sigma * 1.41421356);
    let x = erf1((p.x - r.x) * k) - erf1((p.x - r.z) * k);
    let y = erf1((p.y - r.y) * k) - erf1((p.y - r.w) * k);
    return 0.25 * x * y;
}

// Pixel coverage of a rounded rectangle (x0, y0, x1, y1) with corner radius `radius`.
fn rounded_cover(p: vec2<f32>, r: vec4<f32>, radius: f32) -> f32 {
    let centre = (r.xy + r.zw) * 0.5;
    let half = (r.zw - r.xy) * 0.5;
    let q = abs(p - centre) - half + vec2<f32>(radius);
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
    return clamp(0.5 - d, 0.0, 1.0);
}

fn glow(dst: vec3<f32>, uv: vec2<f32>, color: vec4<f32>, g: vec4<f32>) -> vec3<f32> {
    let t = length((uv - g.xy) / g.zw);
    let a = clamp(1.0 - t, 0.0, 1.0) * color.a;
    return mix(dst, color.rgb, a);
}

@fragment
fn fs_slate(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy;
    let size = params.size.xy;
    let bg0 = params.bg0.rgb;

    var glows = bg0;
    let uv = p / size;
    glows = glow(glows, uv, params.glow0_color, params.glow0);
    glows = glow(glows, uv, params.glow1_color, params.glow1);
    glows = glow(glows, uv, params.glow2_color, params.glow2);

    let cell = params.size.w;
    let q = p - (floor(p / cell) + 0.5) * cell;
    let hole = clamp(params.dots.x - length(q) + 0.5, 0.0, 1.0);
    var c = mix(glows, bg0, 1.0 - hole);

    let card = params.card;
    let radius = params.card_style.x;
    let edge = params.card_style.y;
    let cover = rounded_cover(p, card, radius);

    let shadow_rect = card + vec4<f32>(0.0, params.card_style.z, 0.0, params.card_style.z);
    let shadow = params.glass_shadow.a * blurred_rect(p, shadow_rect, params.card_style.w);
    c = mix(c, params.glass_shadow.rgb, shadow * (1.0 - cover));

    // The frosted fill: the dot screen blurred to its average, tinted, then the glass
    // gradient (premultiplied, top colour to bottom colour over the first part).
    let backdrop = mix(glows, bg0, params.dots.y);
    var fill = mix(backdrop, params.bg1.rgb, params.dots.z);
    let v = clamp((p.y - card.y) / (card.w - card.y) / params.dots.w, 0.0, 1.0);
    let top = vec4<f32>(params.glass_top.rgb * params.glass_top.a, params.glass_top.a);
    let bottom = vec4<f32>(params.glass_bottom.rgb * params.glass_bottom.a, params.glass_bottom.a);
    let glass = mix(top, bottom, v);
    fill = fill * (1.0 - glass.a) + glass.rgb;
    c = mix(c, fill, cover);

    let inner = card + vec4<f32>(edge, edge, -edge, -edge);
    let inner_radius = max(radius - edge, 0.0);
    let inner_cover = rounded_cover(p, inner, inner_radius);
    let shifted = inner + vec4<f32>(0.0, params.size.z, 0.0, params.size.z);
    let highlight = inner_cover * (1.0 - rounded_cover(p, shifted, inner_radius));
    c = mix(c, params.glass_highlight.rgb, params.glass_highlight.a * highlight);
    c = mix(c, params.glass_edge.rgb, params.glass_edge.a * clamp(cover - inner_cover, 0.0, 1.0));

    let ip = vec2<i32>(floor(p - params.icon.xy));
    if (ip.x >= 0 && ip.y >= 0 && f32(ip.x) < params.icon.z && f32(ip.y) < params.icon.w) {
        let m = textureLoad(icon_mask, ip, 0).r;
        c = mix(c, params.icon_color.rgb, params.icon_color.a * m);
    }
    return vec4<f32>(c, 1.0);
}
