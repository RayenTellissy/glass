// Pixelate: the picture coarsens into square blocks that grow from one pixel to the
// block size (a fraction of the frame's width) half way and shrink back, switching
// from A to B over the middle tenth while the blocks are largest. Block sizes are whole
// pixels on a grid anchored at the frame's centre, so their edges stay on pixel
// boundaries; each block is flat, the mean of the picture under it, read from a pyramid
// of halvings (`reduce`, passes 0 to 3, down to a sixteenth) at the level where the
// block spans two to four texels, by up to 4 x 4 filtered reads. Parameters: block size.

// The share of B at the current progress.
fn b_share() -> f32 {
    return smoothstep(0.45, 0.55, tr.progress);
}

// The dissolved picture at uv, filtered.
fn picture(uv: vec2<f32>) -> vec4<f32> {
    let w = b_share();
    if w <= 0.0 {
        return sample_a(uv);
    }
    if w >= 1.0 {
        return sample_b(uv);
    }
    return mix(sample_a(uv), sample_b(uv), w);
}

// Halves the dissolved picture (pass 0) or the level before: a texel's centre lies
// between four of the level above, so one filtered read is their mean.
@fragment
fn reduce(in: VsOut) -> @location(0) vec4<f32> {
    switch tr.pass_index {
        case 0u: {
            return picture(in.uv);
        }
        case 1u: {
            return textureSampleLevel(pass0, samp, in.uv, 0.0);
        }
        case 2u: {
            return textureSampleLevel(pass1, samp, in.uv, 0.0);
        }
        default: {
            return textureSampleLevel(pass2, samp, in.uv, 0.0);
        }
    }
}

// The mean of n x n reads of `t` spread evenly over the block from `lo` to `hi` (uv).
fn block_mean(t: texture_2d<f32>, lo: vec2<f32>, hi: vec2<f32>, n: f32) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var j = 0.0; j < n; j += 1.0) {
        for (var i = 0.0; i < n; i += 1.0) {
            let uv = mix(lo, hi, (vec2<f32>(i, j) + 0.5) / n);
            sum += textureSampleLevel(t, samp, uv, 0.0);
        }
    }
    return sum / (n * n);
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
    let peak = smoothstep(0.0, 1.0, 1.0 - abs(2.0 * clamp(t, 0.0, 1.0) - 1.0));
    let largest = max(tr.p[0].x * tr.size.x, 1.0);
    let s = max(round(1.0 + (largest - 1.0) * peak), 1.0);
    if s <= 1.0 {
        let w = b_share();
        return here_a(in) * (1.0 - w) + here_b(in) * w;
    }
    // The block under this pixel, cut at the frame's edges.
    let c = floor(0.5 * tr.size);
    let cell = floor((in.pos.xy - c) / s);
    let lo = max(c + cell * s, vec2<f32>(0.0)) / tr.size;
    let hi = min(c + (cell + 1.0) * s, tr.size) / tr.size;
    // The level whose texels are s/4 to s/2 pixels, and reads about two texels apart.
    let level = u32(clamp(floor(log2(s * 0.5)), 0.0, 4.0));
    let n = clamp(ceil(s / exp2(f32(level)) * 0.5), 2.0, 4.0);
    switch level {
        case 0u: {
            let w = b_share();
            if w <= 0.0 {
                return block_mean(tex_a, lo, hi, n);
            }
            if w >= 1.0 {
                return block_mean(tex_b, lo, hi, n);
            }
            return mix(block_mean(tex_a, lo, hi, n), block_mean(tex_b, lo, hi, n), w);
        }
        case 1u: {
            return block_mean(pass0, lo, hi, n);
        }
        case 2u: {
            return block_mean(pass1, lo, hi, n);
        }
        case 3u: {
            return block_mean(pass2, lo, hi, n);
        }
        default: {
            return block_mean(pass3, lo, hi, n);
        }
    }
}
