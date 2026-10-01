// Vector shapes (render::shape). Outlines become exact box-filtered coverage by
// signed-area accumulation: each edge draws a quad from itself to the right edge of its
// outline's box in the accumulation atlas, adding, per pixel, the edge's height within
// the pixel's row times the mean fraction of the pixel lying right of the edge (additive
// blending). The sum is the integral of the winding number over the pixel; the fill rule
// turns it into coverage. Paint then samples the atlas and draws source-over, in order,
// premultiplied linear light. Rust mirror: render::shape::node (Item, Seg, Stop layout).

struct Globals {
    canvas: vec2<f32>,
    atlas: vec2<f32>,
    rule: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

// One edge, in its outline's box coordinates (pixels from the box's top left).
struct Seg {
    p: vec4<f32>,
    item: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

struct Item {
    // Box on the canvas: x, y, width, height (whole pixels).
    rect: vec4<f32>,
    // Box origin in the atlas.
    atlas: vec2<f32>,
    // Fill rule in the low 8 bits, atlas binding (0 to 3) above.
    rule: u32,
    // 0 solid, 1 linear gradient, 2 radial gradient.
    kind: u32,
    // Canvas to paint space: x' = m.x x + m.z y + t.x, y' = m.y x + m.w y + t.y.
    m: vec4<f32>,
    t: vec2<f32>,
    // First stop and stop count.
    stops: vec2<u32>,
    // Linear: start.xy, end.xy. Radial: center.xy, radius.
    grad: vec4<f32>,
    // Solid colour, premultiplied.
    color: vec4<f32>,
}

struct Stop {
    // Premultiplied.
    color: vec4<f32>,
    offset: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<storage, read> segs: array<Seg>;
@group(0) @binding(2) var<storage, read> items: array<Item>;
@group(0) @binding(3) var<storage, read> stops: array<Stop>;
@group(0) @binding(4) var acc0: texture_2d<f32>;
@group(0) @binding(5) var acc1: texture_2d<f32>;
@group(0) @binding(6) var acc2: texture_2d<f32>;
@group(0) @binding(7) var acc3: texture_2d<f32>;

fn load_acc(slot: u32, texel: vec2<i32>) -> f32 {
    switch slot {
        case 1u: { return textureLoad(acc1, texel, 0).r; }
        case 2u: { return textureLoad(acc2, texel, 0).r; }
        case 3u: { return textureLoad(acc3, texel, 0).r; }
        default: { return textureLoad(acc0, texel, 0).r; }
    }
}

// Corner of a two-triangle quad for vertex 0..5: (0,0) (1,0) (0,1) (0,1) (1,0) (1,1).
fn quad_corner(vi: u32) -> vec2<f32> {
    return vec2<f32>(f32((0x32u >> vi) & 1u), f32((0x2Cu >> vi) & 1u));
}

fn to_clip(p: vec2<f32>, size: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x / size.x * 2.0 - 1.0, 1.0 - p.y / size.y * 2.0, 0.0, 1.0);
}

struct AccOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) seg: vec4<f32>,
    @location(1) @interpolate(flat) origin: vec2<f32>,
}

@vertex
fn vs_accumulate(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> AccOut {
    let s = segs[ii];
    let item = items[s.item];
    let size = item.rect.zw;
    let lo = vec2<f32>(
        clamp(floor(min(s.p.x, s.p.z)), 0.0, size.x),
        clamp(floor(min(s.p.y, s.p.w)), 0.0, size.y),
    );
    let hi = vec2<f32>(size.x, clamp(ceil(max(s.p.y, s.p.w)), 0.0, size.y));
    var local = mix(lo, hi, quad_corner(vi));
    if hi.y <= lo.y || hi.x <= lo.x {
        local = lo;
    }
    var o: AccOut;
    o.pos = to_clip(item.atlas + local, globals.atlas);
    o.seg = s.p;
    o.origin = item.atlas;
    return o;
}

// Mean over x in [xl, xr] of clamp(c - x, 0, 1): how much of the cell [c - 1, c] lies
// right of an edge crossing it anywhere in that range.
fn mean_right(xl: f32, xr: f32, c: f32) -> f32 {
    let w = xr - xl;
    if w < 1e-3 {
        return clamp(c - 0.5 * (xl + xr), 0.0, 1.0);
    }
    let a = clamp(c - 1.0, xl, xr);
    let b = clamp(c, xl, xr);
    return ((a - xl) + (b - a) * (c - 0.5 * (a + b))) / w;
}

@fragment
fn fs_accumulate(in: AccOut) -> @location(0) vec4<f32> {
    let cell = floor(in.pos.xy) - in.origin;
    let p0 = in.seg.xy;
    let p1 = in.seg.zw;
    let ya = clamp(p0.y, cell.y, cell.y + 1.0);
    let yb = clamp(p1.y, cell.y, cell.y + 1.0);
    let dy = yb - ya;
    if dy == 0.0 {
        return vec4<f32>(0.0);
    }
    let k = (p1.x - p0.x) / (p1.y - p0.y);
    let xa = p0.x + (ya - p0.y) * k;
    let xb = p0.x + (yb - p0.y) * k;
    let m = mean_right(min(xa, xb), max(xa, xb), cell.x + 1.0);
    return vec4<f32>(dy * m, 0.0, 0.0, 0.0);
}

// Coverage from the accumulated winding integral: non-zero clamps its magnitude, even-odd
// folds it into a triangle wave of period 2.
fn coverage(a: f32, rule: u32) -> f32 {
    if rule == 1u {
        return 1.0 - abs(1.0 - fract(a * 0.5) * 2.0);
    }
    return min(abs(a), 1.0);
}

fn gradient(first: u32, count: u32, t: f32) -> vec4<f32> {
    if count == 0u {
        return vec4<f32>(0.0);
    }
    if t < stops[first].offset {
        return stops[first].color;
    }
    for (var i = 1u; i < count; i++) {
        let s1 = stops[first + i];
        if t < s1.offset {
            let s0 = stops[first + i - 1u];
            return mix(s0.color, s1.color, (t - s0.offset) / (s1.offset - s0.offset));
        }
    }
    return stops[first + count - 1u].color;
}

fn paint(item: Item, pos: vec2<f32>) -> vec4<f32> {
    if item.kind == 0u {
        return item.color;
    }
    let p = vec2<f32>(
        item.m.x * pos.x + item.m.z * pos.y + item.t.x,
        item.m.y * pos.x + item.m.w * pos.y + item.t.y,
    );
    var t: f32;
    if item.kind == 1u {
        let d = item.grad.zw - item.grad.xy;
        t = dot(p - item.grad.xy, d) / max(dot(d, d), 1e-20);
    } else {
        t = length(p - item.grad.xy) / max(item.grad.z, 1e-20);
    }
    return gradient(item.stops.x, item.stops.y, clamp(t, 0.0, 1.0));
}

struct PaintOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) item: u32,
}

@vertex
fn vs_paint(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> PaintOut {
    let item = items[ii];
    var o: PaintOut;
    o.pos = to_clip(item.rect.xy + item.rect.zw * quad_corner(vi), globals.canvas);
    o.item = ii;
    return o;
}

@fragment
fn fs_paint(in: PaintOut) -> @location(0) vec4<f32> {
    let item = items[in.item];
    let texel = vec2<i32>(floor(in.pos.xy) - item.rect.xy + item.atlas);
    let cov = coverage(load_acc(item.rule >> 8u, texel), item.rule & 0xffu);
    return paint(item, in.pos.xy) * cov;
}

// One outline's coverage over the whole canvas (the accumulation is canvas-sized).
@fragment
fn fs_resolve(in: VsOut) -> @location(0) vec4<f32> {
    let cov = coverage(load_acc(0u, vec2<i32>(in.pos.xy)), globals.rule);
    return vec4<f32>(cov, 0.0, 0.0, 1.0);
}
