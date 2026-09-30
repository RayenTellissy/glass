//! Text: layout metrics, bidi, the glyph atlas across frames (reuse, growth, eviction),
//! GPU glyph coverage against a CPU reference, outline and shadow, the per-glyph transform
//! hook, and GPU time for a full-screen 4K title. The font is pinned to Arial, which every
//! macOS ships; tests skip when it (or a GPU adapter) is missing.

mod common;

use std::path::Path;

use render::graph::{Graph, TexturePool};
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::text::{
    self, FontFamily, GlyphAtlas, GlyphCoverageNode, GlyphInfo, GlyphTransform, Outline, Shadow,
    TextAlign, TextEngine, TextFormat, TextLayout, TextPaint,
};
use render::wgpu;

const ARIAL: &str = "/System/Library/Fonts/Supplemental/Arial.ttf";

/// Coverage is 8-bit in the atlas and `f16` in the target: one 8-bit level plus `f16`
/// rounding at 1.0.
const COVERAGE_TOLERANCE: f32 = 1.0 / 255.0 + 1.0 / 2048.0;

fn engine() -> Option<TextEngine> {
    if !Path::new(ARIAL).exists() {
        eprintln!("skipped: {ARIAL} not found");
        return None;
    }
    Some(TextEngine::with_font_files(&[Path::new(ARIAL)]).expect("Arial loads"))
}

fn arial(size: f32) -> TextFormat {
    TextFormat {
        family: FontFamily::Named("Arial".to_owned()),
        size,
        ..Default::default()
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    pool: TexturePool,
}

fn gpu() -> Option<Gpu> {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return None;
    };
    let renderer = Renderer::new(&device, &queue);
    Some(Gpu {
        device,
        queue,
        renderer,
        pool: TexturePool::new(),
    })
}

impl Gpu {
    /// The painted text on a transparent `size` canvas.
    fn paint(
        &mut self,
        atlas: &GlyphAtlas,
        prepared: &text::PreparedText,
        size: (u32, u32),
    ) -> Vec<[f32; 4]> {
        let target = common::blank(&self.device, wgpu::TextureFormat::Rgba16Float, size);
        let view = target.create_view(&Default::default());
        let mut g = Graph::new();
        let port = text::add_to_graph(&mut g, atlas, prepared, size);
        g.export(port, &view);
        g.compile()
            .expect("compiles")
            .execute(&self.renderer, &mut self.pool, None);
        common::read(&self.device, &self.queue, &target)
    }

    /// The raw coverage texture (fill, outline, union).
    fn coverage(
        &mut self,
        atlas: &GlyphAtlas,
        prepared: &text::PreparedText,
        size: (u32, u32),
    ) -> Vec<[f32; 4]> {
        let target = common::blank(&self.device, wgpu::TextureFormat::Rgba16Float, size);
        let view = target.create_view(&Default::default());
        let mut g = Graph::new();
        let node = g.add(GlyphCoverageNode::new(atlas, prepared, size), &[]);
        g.export(node.out(0), &view);
        g.compile()
            .expect("compiles")
            .execute(&self.renderer, &mut self.pool, None);
        common::read(&self.device, &self.queue, &target)
    }
}

/// Coverage-weighted centroid of channel `c`.
fn centroid(pixels: &[[f32; 4]], width: u32, c: usize) -> [f32; 2] {
    let (mut sx, mut sy, mut sum) = (0.0f64, 0.0f64, 0.0f64);
    for (i, px) in pixels.iter().enumerate() {
        let v = f64::from(px[c]);
        sx += v * (i as u32 % width) as f64;
        sy += v * (i as u32 / width) as f64;
        sum += v;
    }
    assert!(sum > 0.0, "channel {c} is empty");
    [(sx / sum) as f32, (sy / sum) as f32]
}

/// Bounding box (min x, min y, max x, max y) of pixels where channel `c` exceeds 0.5.
fn extent(pixels: &[[f32; 4]], width: u32, c: usize) -> [u32; 4] {
    let mut b = [u32::MAX, u32::MAX, 0, 0];
    for (i, px) in pixels.iter().enumerate() {
        if px[c] > 0.5 {
            let (x, y) = (i as u32 % width, i as u32 / width);
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
    }
    b
}

#[test]
fn layout_metrics_are_sane() {
    let Some(mut engine) = engine() else { return };
    let layout = engine.layout("Hello", &arial(100.0));
    assert_eq!(layout.glyphs.len(), 5);
    assert_eq!(layout.lines.len(), 1);
    let line = &layout.lines[0];
    assert_eq!(line.top, 0.0);
    assert!((line.height - 120.0).abs() < 1e-3, "{line:?}");
    assert!(
        line.baseline > 80.0 && line.baseline < 115.0,
        "baseline {}",
        line.baseline
    );
    // Arial's advances for "Hello" sum to 2.278 em.
    assert!((layout.width - 227.8).abs() < 3.0, "width {}", layout.width);
    let advances: f32 = layout.glyphs.iter().map(|g| g.glyph.w).sum();
    assert!((advances - layout.width).abs() < 0.5);
    assert!(
        layout
            .glyphs
            .windows(2)
            .all(|w| w[1].glyph.x > w[0].glyph.x)
    );
    assert!(
        layout
            .glyphs
            .iter()
            .all(|g| g.glyph.glyph_id != 0 && !g.rtl())
    );

    let wrapped = engine.layout(
        "Hello world, the quick brown fox",
        &TextFormat {
            max_width: Some(300.0),
            ..arial(100.0)
        },
    );
    assert!(wrapped.lines.len() >= 3, "{:?}", wrapped.lines);
    for pair in wrapped.lines.windows(2) {
        assert!((pair[1].baseline - pair[0].baseline - 120.0).abs() < 1e-3);
    }
    assert!(wrapped.lines.iter().all(|l| l.width <= 300.0));
    assert!((wrapped.height - 120.0 * wrapped.lines.len() as f32).abs() < 1e-3);

    let centred = engine.layout(
        "Hello",
        &TextFormat {
            max_width: Some(1000.0),
            align: TextAlign::Center,
            ..arial(100.0)
        },
    );
    let left = centred.glyphs[0].glyph.x;
    assert!(
        (left - (1000.0 - centred.width) / 2.0).abs() < 1.0,
        "left {left}"
    );
}

#[test]
fn right_to_left_text_is_laid_out_and_rendered() {
    let Some(mut engine) = engine() else { return };
    // "Shalom olam" in Hebrew.
    let hebrew = "שלום עולם";
    let layout = engine.layout(hebrew, &arial(64.0));
    assert_eq!(layout.lines.len(), 1);
    assert!(layout.lines[0].rtl, "a Hebrew paragraph is right to left");
    assert!(
        layout
            .glyphs
            .iter()
            .all(|g| g.rtl() && g.glyph.glyph_id != 0)
    );
    // Right to left: later characters sit further left.
    let mut by_x: Vec<(f32, usize)> = layout
        .glyphs
        .iter()
        .map(|g| (g.glyph.x, g.cluster().start))
        .collect();
    by_x.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert!(by_x.windows(2).all(|w| w[0].1 > w[1].1), "{by_x:?}");

    // Mixed text keeps the Latin run left to right inside a left-to-right paragraph.
    let mixed = engine.layout("abc שלום", &arial(64.0));
    assert!(!mixed.lines[0].rtl);
    assert!(!mixed.glyphs[0].rtl() && mixed.glyphs.last().is_some_and(|g| g.rtl()));

    let Some(mut gpu) = gpu() else { return };
    let mut atlas = GlyphAtlas::new(&gpu.device, &gpu.queue);
    let paint = TextPaint {
        origin: [8.0, 4.0],
        ..Default::default()
    };
    let prepared = atlas.prepare(&mut engine, &layout, &paint, None);
    let size = (layout.width as u32 + 16, 90);
    let out = gpu.paint(&atlas, &prepared, size);
    let covered = out.iter().filter(|p| p[3] > 0.5).count();
    assert!(covered > 500, "{covered} pixels covered");
    // Hebrew letters hang from the top line: the right half has ink as well as the left.
    let right = out
        .iter()
        .enumerate()
        .filter(|(i, p)| (*i as u32 % size.0) > size.0 / 2 && p[3] > 0.5)
        .count();
    assert!(right > covered / 4);
}

/// The CPU reference: every glyph's fill mask placed at its pixel origin, overlaps
/// merged by maximum.
fn cpu_coverage(
    engine: &mut TextEngine,
    layout: &TextLayout,
    origin: [f32; 2],
    (w, h): (u32, u32),
) -> Vec<f32> {
    let mut out = vec![0.0f32; (w * h) as usize];
    for laid in &layout.glyphs {
        let line = &layout.lines[laid.line];
        let p = laid
            .glyph
            .physical((origin[0], origin[1] + line.baseline), 1.0);
        let Some(mask) = engine.rasterize(p.cache_key, 0.0) else {
            continue;
        };
        for row in 0..mask.height as i32 {
            for col in 0..mask.width as i32 {
                let (x, y) = (p.x + mask.left + col, p.y - mask.top + row);
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    continue;
                }
                let v = f32::from(mask.data[(row * mask.width as i32 + col) as usize]) / 255.0;
                let px = &mut out[(y as u32 * w + x as u32) as usize];
                *px = px.max(v);
            }
        }
    }
    out
}

#[test]
fn gpu_coverage_matches_the_cpu_reference() {
    let Some(mut engine) = engine() else { return };
    let Some(mut gpu) = gpu() else { return };
    let mut atlas = GlyphAtlas::new(&gpu.device, &gpu.queue);
    let size = (420, 100);
    // Fractional origin: glyphs land on subpixel bins, and "Av" kerns into overlap.
    let origin = [10.3, 5.7];
    let layout = engine.layout("Glass Avg fi", &arial(64.0));
    let paint = TextPaint {
        origin,
        ..Default::default()
    };
    let prepared = atlas.prepare(&mut engine, &layout, &paint, None);
    let gpu_cov = gpu.coverage(&atlas, &prepared, size);
    let cpu_cov = cpu_coverage(&mut engine, &layout, origin, size);

    let mut max_diff = 0.0f32;
    let mut sum_diff = 0.0f64;
    for (g, c) in gpu_cov.iter().zip(&cpu_cov) {
        let d = (g[0] - c).abs();
        max_diff = max_diff.max(d);
        sum_diff += f64::from(d);
        assert_eq!(g[1], 0.0, "no outline was asked for");
        assert!((g[2] - g[0]).abs() < 1e-6, "the union equals the fill");
    }
    let mean = sum_diff / cpu_cov.len() as f64;
    eprintln!("GPU vs CPU coverage: max {max_diff:.5}, mean {mean:.6}");
    assert!(max_diff <= COVERAGE_TOLERANCE, "max diff {max_diff}");
    assert!(cpu_cov.iter().filter(|&&v| v > 0.5).count() > 2000);

    // Painted in a colour: premultiplied, and the coverage scales the fill colour.
    let paint = TextPaint {
        origin,
        fill: [0.5, 0.25, 1.0, 0.8],
        ..Default::default()
    };
    let prepared = atlas.prepare(&mut engine, &layout, &paint, None);
    let painted = gpu.paint(&atlas, &prepared, size);
    for (p, c) in painted.iter().zip(&cpu_cov) {
        let a = 0.8 * c;
        assert!((p[3] - a).abs() <= COVERAGE_TOLERANCE, "{p:?} vs {a}");
        assert!((p[0] - 0.5 * a).abs() <= COVERAGE_TOLERANCE);
        assert!((p[2] - a).abs() <= COVERAGE_TOLERANCE);
    }
}

#[test]
fn outline_and_shadow_are_drawn() {
    let Some(mut engine) = engine() else { return };
    let Some(mut gpu) = gpu() else { return };
    let mut atlas = GlyphAtlas::new(&gpu.device, &gpu.queue);
    let size = (320, 200);
    let layout = engine.layout("OH", &arial(110.0));
    let red = [1.0, 0.0, 0.0, 1.0];
    let base = TextPaint {
        origin: [30.0, 20.0],
        fill: red,
        ..Default::default()
    };
    let prepared = atlas.prepare(&mut engine, &layout, &base, None);
    let plain = gpu.paint(&atlas, &prepared, size);
    let is_blue = |p: &[f32; 4]| p[2] > 0.9 && p[0] < 0.05;
    let is_green = |p: &[f32; 4]| p[1] > 0.3 && p[0] < 0.05 && p[2] < 0.05;
    assert!(!plain.iter().any(is_blue) && !plain.iter().any(is_green));

    let outlined = TextPaint {
        outline: Some(Outline {
            width: 4.0,
            color: [0.0, 0.0, 1.0, 1.0],
        }),
        ..base.clone()
    };
    let prepared = atlas.prepare(&mut engine, &layout, &outlined, None);
    let out = gpu.paint(&atlas, &prepared, size);
    let blue = out.iter().filter(|p| is_blue(p)).count();
    assert!(blue > 1000, "{blue} outline pixels");
    // The outline lies outside the fill: no pure-outline pixel is inside the plain fill.
    assert!(
        out.iter()
            .zip(&plain)
            .all(|(o, p)| !(is_blue(o) && p[3] > 0.5))
    );
    // It grows the silhouette by about its width on each side.
    let fill_box = extent(&plain, size.0, 3);
    let outline_box = extent(&out, size.0, 3);
    for (i, sign) in [(0, -1i32), (1, -1), (2, 1), (3, 1)] {
        let grown = (outline_box[i] as i32 - fill_box[i] as i32) * sign;
        assert!(
            (3..=5).contains(&grown),
            "edge {i} grew {grown}: {fill_box:?} {outline_box:?}"
        );
    }

    let shadowed = TextPaint {
        shadow: Some(Shadow {
            offset: [24.0, 16.0],
            blur: 2.0,
            color: [0.0, 1.0, 0.0, 1.0],
        }),
        ..outlined
    };
    let prepared = atlas.prepare(&mut engine, &layout, &shadowed, None);
    let out = gpu.paint(&atlas, &prepared, size);
    let green: Vec<[f32; 4]> = out
        .iter()
        .map(|p| {
            if is_green(p) {
                [0.0, 1.0, 0.0, 1.0]
            } else {
                [0.0; 4]
            }
        })
        .collect();
    assert!(green.iter().filter(|p| p[1] > 0.0).count() > 1000);
    // The visible shadow sits down and to the right of the text.
    let text_c = centroid(&plain, size.0, 3);
    let shadow_c = centroid(&green, size.0, 1);
    assert!(
        shadow_c[0] > text_c[0] + 8.0 && shadow_c[1] > text_c[1] + 5.0,
        "{text_c:?} {shadow_c:?}"
    );
    // Fill and outline stay on top; everything is valid premultiplied colour.
    assert!(
        out.iter()
            .all(|p| p[3] <= 1.0 + 1e-3 && p[0].max(p[1]).max(p[2]) <= p[3] + 1e-3)
    );
    assert!(
        out.iter()
            .zip(&plain)
            .all(|(o, p)| p[3] < 0.99 || o[0] > 0.99)
    );
}

#[test]
fn per_glyph_transform_moves_scales_rotates_and_fades_a_glyph() {
    let Some(mut engine) = engine() else { return };
    let Some(mut gpu) = gpu() else { return };
    let mut atlas = GlyphAtlas::new(&gpu.device, &gpu.queue);
    let size = (300, 300);
    let layout = engine.layout("IL", &arial(100.0));
    let paint = TextPaint {
        origin: [100.0, 100.0],
        ..Default::default()
    };
    let render =
        |gpu: &mut Gpu, atlas: &mut GlyphAtlas, engine: &mut TextEngine, t: GlyphTransform| {
            let hook = move |g: &GlyphInfo| {
                if g.index == 0 {
                    t
                } else {
                    GlyphTransform::IDENTITY
                }
            };
            let prepared = atlas.prepare(engine, &layout, &paint, Some(&hook));
            gpu.coverage(atlas, &prepared, size)
        };
    let hide_l = |g: &GlyphInfo| GlyphTransform {
        opacity: if g.index == 1 { 0.0 } else { 1.0 },
        ..Default::default()
    };
    let prepared = atlas.prepare(&mut engine, &layout, &paint, Some(&hide_l));
    let i_only = gpu.coverage(&atlas, &prepared, size);
    let base = render(&mut gpu, &mut atlas, &mut engine, GlyphTransform::IDENTITY);
    let moved = render(
        &mut gpu,
        &mut atlas,
        &mut engine,
        GlyphTransform {
            offset: [-60.0, -80.0],
            ..Default::default()
        },
    );
    // Only the "I" moves: the difference between the renders is the I, shifted.
    let diff = |a: &[[f32; 4]], b: &[[f32; 4]]| -> Vec<[f32; 4]> {
        a.iter()
            .zip(b)
            .map(|(x, y)| [(x[0] - y[0]).max(0.0), 0.0, 0.0, 0.0])
            .collect()
    };
    let i_at = centroid(&i_only, size.0, 0);
    let moved_i = centroid(&diff(&moved, &base), size.0, 0);
    assert!(
        (moved_i[0] - i_at[0] + 60.0).abs() < 0.5 && (moved_i[1] - i_at[1] + 80.0).abs() < 0.5,
        "I at {i_at:?}, moved to {moved_i:?}"
    );
    // A quarter turn makes the tall I wide.
    let hook = |g: &GlyphInfo| GlyphTransform {
        rotation: std::f32::consts::FRAC_PI_2,
        opacity: if g.index == 0 { 1.0 } else { 0.0 },
        ..Default::default()
    };
    let prepared = atlas.prepare(&mut engine, &layout, &paint, Some(&hook));
    let turned = gpu.coverage(&atlas, &prepared, size);
    let [x0, y0, x1, y1] = extent(&i_only, size.0, 0);
    let [tx0, ty0, tx1, ty1] = extent(&turned, size.0, 0);
    assert!(y1 - y0 > 3 * (x1 - x0), "upright I is tall");
    assert!((tx1 - tx0) as i32 - (y1 - y0) as i32 <= 2 && (tx1 - tx0) + 2 >= y1 - y0);
    assert!((ty1 - ty0) as i32 - (x1 - x0) as i32 <= 2 && (ty1 - ty0) + 2 >= x1 - x0);

    // Doubling the scale quadruples the ink; half opacity halves it.
    let ink = |c: &[[f32; 4]]| c.iter().map(|p| f64::from(p[0])).sum::<f64>();
    for (scale, opacity, expected, tolerance) in [(2.0, 1.0, 4.0, 0.2), (1.0, 0.5, 0.5, 0.01)] {
        let hook = move |g: &GlyphInfo| GlyphTransform {
            scale: [scale, scale],
            opacity: if g.index == 0 { opacity } else { 0.0 },
            ..Default::default()
        };
        let prepared = atlas.prepare(&mut engine, &layout, &paint, Some(&hook));
        let ratio = ink(&gpu.coverage(&atlas, &prepared, size)) / ink(&i_only);
        assert!(
            (ratio - expected).abs() < tolerance,
            "scale {scale}, opacity {opacity}: ink ratio {ratio}"
        );
    }
}

#[test]
fn atlas_reuses_glyphs_across_frames_and_evicts_when_full() {
    let Some(mut engine) = engine() else { return };
    let Some(mut gpu) = gpu() else { return };
    let size = (700, 120);
    let paint = TextPaint {
        origin: [10.0, 10.0],
        outline: Some(Outline {
            width: 2.0,
            color: [0.0, 0.0, 1.0, 1.0],
        }),
        ..Default::default()
    };
    let first = engine.layout("ABCDEFGHIJ", &arial(60.0));
    let second = engine.layout("KLMNOPQRST", &arial(60.0));

    let mut atlas = GlyphAtlas::new(&gpu.device, &gpu.queue);
    atlas.begin_frame();
    let prepared = atlas.prepare(&mut engine, &first, &paint, None);
    let frame1 = gpu.paint(&atlas, &prepared, size);
    let after1 = atlas.stats();
    assert_eq!(after1.misses, 20, "ten fills and ten outlines");
    assert_eq!(after1.hits, 0);
    atlas.begin_frame();
    let prepared = atlas.prepare(&mut engine, &first, &paint, None);
    let frame2 = gpu.paint(&atlas, &prepared, size);
    let after2 = atlas.stats();
    assert_eq!(
        after2.misses, after1.misses,
        "the second frame rasterises nothing"
    );
    assert_eq!(after2.hits, 20);
    assert_eq!(after2.grows, 0);
    assert_eq!(frame1, frame2);

    // A small atlas grows, then evicts the first frame's glyphs to fit the second's.
    let mut small = GlyphAtlas::with_sizes(&gpu.device, &gpu.queue, 64, 256);
    small.begin_frame();
    let _ = small.prepare(&mut engine, &first, &paint, None);
    assert!(small.stats().grows >= 2, "{:?}", small.stats());
    assert_eq!(small.size(), 256);
    small.begin_frame();
    let prepared = small.prepare(&mut engine, &second, &paint, None);
    let stats = small.stats();
    assert!(stats.evictions > 0 && stats.repacks > 0, "{stats:?}");
    assert_eq!(stats.overflowed, 0);
    let from_small = gpu.paint(&small, &prepared, size);
    atlas.begin_frame();
    let prepared = atlas.prepare(&mut engine, &second, &paint, None);
    let from_large = gpu.paint(&atlas, &prepared, size);
    assert_eq!(from_small, from_large, "repacked glyphs draw the same");

    // Returning to the first text re-rasterises only what was evicted.
    small.begin_frame();
    let misses = small.stats().misses;
    let _ = small.prepare(&mut engine, &first, &paint, None);
    assert!(small.stats().misses > misses);
}

#[test]
fn full_screen_4k_title_gpu_time() {
    let Some(mut engine) = engine() else { return };
    let Some(mut gpu) = gpu() else { return };
    let size = (3840, 2160);
    let mut atlas = GlyphAtlas::new(&gpu.device, &gpu.queue);
    let layout = engine.layout(
        "GLASS\nTITLE",
        &TextFormat {
            line_height: 1.0,
            align: TextAlign::Center,
            max_width: Some(3840.0),
            ..arial(900.0)
        },
    );
    assert!(
        layout.width > 2800.0,
        "the title spans the frame: {}",
        layout.width
    );
    let paint = TextPaint {
        origin: [0.0, 180.0],
        fill: [1.0, 1.0, 1.0, 1.0],
        outline: Some(Outline {
            width: 12.0,
            color: [0.0, 0.0, 0.0, 1.0],
        }),
        shadow: Some(Shadow {
            offset: [16.0, 16.0],
            blur: 8.0,
            color: [0.0, 0.0, 0.0, 0.6],
        }),
    };
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, size);
    let view = target.create_view(&Default::default());
    let mut profiler = GpuProfiler::new(&gpu.device, &gpu.queue, 16);
    let mut totals = Vec::new();
    let mut passes = Vec::new();
    for _ in 0..12 {
        atlas.begin_frame();
        let prepared = atlas.prepare(&mut engine, &layout, &paint, None);
        let mut g = Graph::new();
        let port = text::add_to_graph(&mut g, &atlas, &prepared, size);
        g.export(port, &view);
        g.compile()
            .expect("compiles")
            .execute(&gpu.renderer, &mut gpu.pool, Some(&mut profiler));
        if let Some(t) = profiler.wait(&gpu.device) {
            totals.extend(t.total_ms);
            passes = t.passes;
        }
    }
    let out = common::read(&gpu.device, &gpu.queue, &target);
    assert!(
        out.iter().filter(|p| p[3] > 0.5).count() > 1_000_000,
        "the title covers the frame"
    );
    if totals.is_empty() {
        eprintln!("timestamps not supported: timing skipped");
        return;
    }
    // The first frames rasterise the glyphs and build pipelines; report the steady state.
    let mut steady = totals[2..].to_vec();
    steady.sort_by(f64::total_cmp);
    eprintln!(
        "4K title GPU time: median {:.3} ms, min {:.3} ms; passes {:?}",
        steady[steady.len() / 2],
        steady[0],
        passes
            .iter()
            .map(|p| format!("{} {:.3}", p.label, p.gpu_ms.unwrap_or(f64::NAN)))
            .collect::<Vec<_>>()
    );
}
