// Spin: A turns about the centre by the angle, clockwise or anticlockwise, while B
// turns the same way from the angle back to rest, dissolving from A to B in the middle
// half. Each is scaled just enough that its turned frame covers the picture, times a
// zoom that swells to the zoom parameter half way, so no edge ever shows. Parameters:
// rotation, angle, zoom.

const PI: f32 = 3.141592653589793;

// A (or B) turned by `angle` about the centre, scaled to cover the frame times `zoom`.
fn turned(b: bool, p: vec2<f32>, angle: f32, zoom: f32) -> vec4<f32> {
    let aspect = max(tr.size.x / tr.size.y, tr.size.y / tr.size.x);
    let scale = (abs(cos(angle)) + aspect * abs(sin(angle))) * zoom;
    let c = 0.5 * tr.size;
    let q = c + rotate(p - c, -angle) / scale;
    if b {
        return frame_b(q, 1.0 / scale);
    }
    return frame_a(q, 1.0 / scale);
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
    let turn = select(1.0, -1.0, tr.p[0].x >= 0.5) * tr.p[1].x;
    let zoom = mix(1.0, max(tr.p[2].x, 1.0), sin(PI * t));
    let w = smoothstep(0.25, 0.75, t);
    var out = vec4<f32>(0.0);
    if w < 1.0 {
        out += turned(false, in.pos.xy, turn * t, zoom) * (1.0 - w);
    }
    if w > 0.0 {
        out += turned(true, in.pos.xy, turn * (t - 1.0), zoom) * w;
    }
    return out;
}
