//! The offline media slate: the background pass against its CPU reference, the card's
//! texts landing in their boxes in their colours, the icon, the design geometry, and the
//! cache (one render per size and label, imported by every frame). The font is pinned to
//! Arial; tests skip when it (or a GPU adapter) is missing.

mod common;

use std::path::Path;

use render::graph::{Graph, TexturePool};
use render::nodes::{CompositeNode, Layer};
use render::renderer::Renderer;
use render::slate::{self, SlateCache, SlateLabel, SlateStyle};
use render::text::{FontFamily, TextEngine};
use render::wgpu;

const ARIAL: &str = "/System/Library/Fonts/Supplemental/Arial.ttf";

/// `f16` storage (about 1e-3 relative below 1) plus the shader's `f32` maths.
const TOLERANCE: f64 = 2e-3;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    pool: TexturePool,
    engine: TextEngine,
}

fn gpu() -> Option<Gpu> {
    if !Path::new(ARIAL).exists() {
        eprintln!("skipped: {ARIAL} not found");
        return None;
    }
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
        engine: TextEngine::with_font_files(&[Path::new(ARIAL)]).expect("Arial loads"),
    })
}

fn cache() -> SlateCache {
    let arial = FontFamily::Named("Arial".to_owned());
    SlateCache::new(SlateStyle {
        ui_font: arial.clone(),
        mono_font: arial,
        ..SlateStyle::default()
    })
}

fn label(file_name: &str) -> SlateLabel {
    SlateLabel {
        title: "Media offline".to_owned(),
        file_name: file_name.to_owned(),
        detail: "Last seen on A014. Edits are kept.".to_owned(),
    }
}

fn inside(b: [f32; 4], x: usize, y: usize, grow: f32) -> bool {
    let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
    x >= b[0] - grow && x <= b[2] + grow && y >= b[1] - grow && y <= b[3] + grow
}

fn luma(c: [f64; 3]) -> f64 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

#[test]
fn background_matches_the_reference_and_texts_land_in_their_boxes() {
    let Some(mut gpu) = gpu() else { return };
    let mut cache = cache();
    for size in [(640, 360), (1920, 1080), (1080, 1920)] {
        let slate = cache.prepare(
            &gpu.renderer,
            &mut gpu.engine,
            &mut gpu.pool,
            size,
            &label("A014_C011.mov"),
        );
        let pixels = common::read(&gpu.device, &gpu.queue, slate.texture());
        let params = slate.params();
        let geo = slate.geometry();
        let texts = [geo.title, geo.file_name, geo.detail];
        let pal = &params.palette;
        let colours = [pal.title, pal.file_name, pal.detail];

        let mut worst = 0.0f64;
        let mut brightest = [0.0f64; 3];
        let mut touched = [0usize; 3];
        for y in 0..size.1 as usize {
            for x in 0..size.0 as usize {
                let gpu_px = pixels[y * size.0 as usize + x];
                assert!((gpu_px[3] - 1.0).abs() < 1e-3, "opaque at {x},{y}");
                let got = [gpu_px[0], gpu_px[1], gpu_px[2]].map(f64::from);
                let want = slate::reference(params, [x as f64 + 0.5, y as f64 + 0.5]);
                let in_text = texts.iter().position(|b| inside(*b, x, y, 3.0));
                match in_text {
                    None => {
                        for i in 0..3 {
                            let err = (got[i] - want[i]).abs();
                            worst = worst.max(err);
                            assert!(
                                err <= TOLERANCE * want[i].abs().max(1.0),
                                "{size:?} at {x},{y}: {got:?} vs {want:?}"
                            );
                        }
                    }
                    Some(t) => {
                        if (luma(got) - luma(want)).abs() > 0.05 {
                            touched[t] += 1;
                        }
                        brightest[t] = brightest[t].max(luma(got));
                    }
                }
            }
        }
        eprintln!(
            "{size:?}: background max error {worst:.5}, text pixels {touched:?}, brightest {brightest:.3?}"
        );
        let s = geo.scale;
        for t in 0..3 {
            // Every text drew, in its own colour (full-coverage stem pixels at 12 and 11
            // px get within a few percent once scaled up).
            assert!(
                touched[t] as f32 > 20.0 * s * s,
                "{size:?} text {t} drew {touched:?}"
            );
            let want = luma(
                colours[t].map(f64::from)[..3]
                    .try_into()
                    .unwrap_or([0.0; 3]),
            );
            let tol = if s >= 2.0 { 0.03 } else { 0.25 };
            assert!(
                (brightest[t] - want).abs() <= tol * want,
                "{size:?} text {t}: brightest {} vs colour {want}",
                brightest[t]
            );
        }
    }
    assert_eq!(cache.renders(), 3);
}

#[test]
fn geometry_follows_the_design() {
    let Some(mut gpu) = gpu() else { return };
    let mut cache = cache();
    let slate = cache.prepare(
        &gpu.renderer,
        &mut gpu.engine,
        &mut gpu.pool,
        (640, 360),
        &label("A014_C011.mov"),
    );
    let geo = slate.geometry();
    assert_eq!(geo.scale, 1.0);
    // 248 wide, 16 + 16 + 2 + 14 + 2 + 14 + 16 plus the 1 px edge top and bottom.
    assert_eq!(geo.card, [196.0, 139.0, 444.0, 221.0]);
    assert_eq!(geo.icon, [geo.icon[0], 156.0, geo.icon[0] + 16.0, 172.0]);
    assert_eq!(geo.title[0], geo.icon[0] + 24.0);
    assert_eq!(geo.title[1], 156.0);
    assert_eq!(geo.file_name[1], 174.0);
    assert_eq!(geo.detail[1], 190.0);
    // The icon and title row, the file name and the detail are centred on the card.
    let centre = 320.0;
    let row = (geo.icon[0] + geo.title[2]) * 0.5;
    assert!((row - centre).abs() <= 0.5, "row centre {row}");
    assert!(((geo.file_name[0] + geo.file_name[2]) * 0.5 - centre).abs() < 0.01);
    assert!(((geo.detail[0] + geo.detail[2]) * 0.5 - centre).abs() < 0.01);
    assert_eq!(geo.file_name_shown, "A014_C011.mov");

    // The icon's fully covered pixels are state/error.
    let pixels = common::read(&gpu.device, &gpu.queue, slate.texture());
    let icon = &slate.params().icon;
    let solid = icon
        .data
        .iter()
        .position(|&m| m == 255)
        .expect("the icon has solid pixels");
    let x = icon.left as usize + solid % icon.width as usize;
    let y = icon.top as usize + solid / icon.width as usize;
    assert!(inside(geo.icon, x, y, 1.0));
    let px = pixels[y * 640 + x];
    let want = slate.params().palette.icon;
    for i in 0..3 {
        assert!(
            (px[i] - want[i]).abs() < 2e-3,
            "icon pixel {px:?} vs {want:?}"
        );
    }

    // A long name is shortened in the middle to fit the card.
    let long = "A014_C011_20260930_R3DX_WIDE_MASTER_GRADE_v12_final_final.mov";
    let slate = cache.prepare(
        &gpu.renderer,
        &mut gpu.engine,
        &mut gpu.pool,
        (640, 360),
        &label(long),
    );
    let geo = slate.geometry();
    assert!(geo.file_name_shown.contains('…'), "{}", geo.file_name_shown);
    assert!(geo.file_name_shown.ends_with(".mov"));
    assert!(geo.file_name[2] - geo.file_name[0] <= 214.0);
    assert!(geo.file_name[0] >= geo.card[0] + 17.0);
}

#[test]
fn slates_render_once_per_size_and_label_and_frames_import_them() {
    let Some(mut gpu) = gpu() else { return };
    let mut cache = cache();
    let a = label("A014_C011.mov");
    let b = label("B002_C004.mov");
    for _ in 0..3 {
        cache.prepare(
            &gpu.renderer,
            &mut gpu.engine,
            &mut gpu.pool,
            (320, 180),
            &a,
        );
    }
    assert_eq!(cache.renders(), 1);
    cache.prepare(
        &gpu.renderer,
        &mut gpu.engine,
        &mut gpu.pool,
        (320, 180),
        &b,
    );
    cache.prepare(
        &gpu.renderer,
        &mut gpu.engine,
        &mut gpu.pool,
        (640, 360),
        &a,
    );
    assert_eq!((cache.renders(), cache.len()), (3, 3));
    assert!(cache.get((320, 180), &a).is_some());
    assert!(cache.get((321, 180), &a).is_none());

    // Frames: the slate as a layer of the frame's composite, 30 frames, no new renders.
    let slate_pixels = common::read(
        &gpu.device,
        &gpu.queue,
        cache.get((320, 180), &a).expect("prepared").texture(),
    );
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, (320, 180));
    let view = target.create_view(&Default::default());
    for _ in 0..30 {
        let mut g = Graph::new();
        let port = cache
            .add_to_graph(&mut g, (320, 180), &a)
            .expect("prepared");
        let out = g.add(
            CompositeNode::new((320, 180), false, vec![Layer::default()]),
            &[port],
        );
        g.export(out.out(0), &view);
        g.compile()
            .expect("compiles")
            .execute(&gpu.renderer, &mut gpu.pool, None);
    }
    assert_eq!(cache.renders(), 3);
    let frame = common::read(&gpu.device, &gpu.queue, &target);
    assert_eq!(frame, slate_pixels);

    cache.retain(|_, l| *l != b);
    assert_eq!(cache.len(), 2);
    let mut g = Graph::new();
    assert!(cache.add_to_graph(&mut g, (320, 180), &b).is_none());
}
