// Checker wipe: the frame is cut into columns x rows cells coloured like a
// checkerboard. In the first half the cells of one colour wipe in the direction, each
// from its start to its end; in the second half the others do. A cell holds the
// interval [start, start + e] of B along the direction; coverage is that mask filtered
// by the edge's kernel (1 px box, or the softness, capped at the smaller cell side)
// along and across the direction, summed over the cell and its nearer neighbours, so
// every cell edge is anti-aliased and full cells join without a seam. The border
// follows each travelling edge on A's side, narrowed near the start and the end of a
// cell's wipe. Parameters: columns, rows, softness and border width in output pixels,
// border colour.

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

// Coverage at `x` of the interval [start, start + e] of a cell starting `x` before
// (`at_start` is edge(x)).
fn reached(x: f32, at_start: f32, e: f32, softness: f32) -> f32 {
    return at_start - edge(x - e, softness);
}

// B's coverage, and its outer edge with the border, at `a` along and `b` across the
// direction in cells of `ca` by `cb` whose first half (parity 0) has reached `e0` and
// second half `e1`. Sums the cell and its neighbours on the nearer side along and
// across, and the diagonal one between; the kernel is at most a cell wide, so the far
// neighbours add exactly nothing, and the row and its neighbour share the kernel's
// weight (w and 1 - w). The diagonal neighbour has the cell's parity, the other two
// the opposite one.
fn cells(a: f32, b: f32, ca: f32, cb: f32, e0: f32, e1: f32, border: f32, softness: f32) -> vec2<f32> {
    let ka = floor(a / ca);
    let kb = floor(b / cb);
    let x = a - ka * ca;
    let y = b - kb * cb;
    let odd = ((i32(ka) + i32(kb)) & 1) == 1;
    // This cell's parity's reach, and the other's.
    let e = select(e0, e1, odd);
    let f = select(e1, e0, odd);
    let eo = e + min(border, min(e, ca - e));
    let fo = f + min(border, min(f, ca - f));
    // Half the kernel or more from the cell's sides, the neighbours add exactly nothing
    // (most pixels).
    let half_kernel = 0.5 * max(softness, 1.0);
    let inside = vec2<f32>(x, y);
    if all(inside >= vec2<f32>(half_kernel)) && all(inside <= vec2<f32>(ca, cb) - half_kernel) {
        let inner = 1.0 - edge(x - e, softness);
        let outer = 1.0 - edge(x - eo, softness);
        return clamp(vec2<f32>(inner, outer), vec2<f32>(0.0), vec2<f32>(1.0));
    }
    let xn = x - select(ca, -ca, x < 0.5 * ca);
    let yn = y - select(cb, -cb, y < 0.5 * cb);
    let w = edge(y, softness) - edge(y - cb, softness);
    let s = edge(x, softness);
    let sn = edge(xn, softness);
    let inner = w * (reached(x, s, e, softness) + reached(xn, sn, f, softness))
        + (1.0 - w) * (reached(x, s, f, softness) + reached(xn, sn, e, softness));
    let outer = w * (reached(x, s, eo, softness) + reached(xn, sn, fo, softness))
        + (1.0 - w) * (reached(x, s, fo, softness) + reached(xn, sn, eo, softness));
    return clamp(vec2<f32>(inner, outer), vec2<f32>(0.0), vec2<f32>(1.0));
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
    let grid = max(round(vec2<f32>(tr.p[0].x, tr.p[1].x)), vec2<f32>(1.0));
    let cell = tr.size / grid;
    let border = max(tr.p[3].x, 0.0);
    let softness = min(tr.p[2].x, min(cell.x, cell.y));
    let p = in.pos.xy;
    let flip = tr.size - p;
    var a = select(p.x, flip.x, tr.dir.x < 0.0);
    var b = p.y;
    var ca = cell.x;
    var cb = cell.y;
    if abs(tr.dir.y) > 0.5 {
        a = select(p.y, flip.y, tr.dir.y < 0.0);
        b = p.x;
        ca = cell.y;
        cb = cell.x;
    }
    let e0 = ca * clamp(2.0 * t, 0.0, 1.0);
    let e1 = ca * clamp(2.0 * t - 1.0, 0.0, 1.0);
    let c = cells(a, b, ca, cb, e0, e1, border, softness);
    return wipe(in, c.x, c.y, tr.p[4]);
}
