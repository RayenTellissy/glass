//! Text benchmarks: shaping and layout of a paragraph, and preparing it against a warm
//! glyph atlas. Throughput is in glyphs. Run with `cargo bench -p render --bench text`;
//! skips when Arial (every macOS ships it) or a GPU adapter is missing.

use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use render::text::{FontFamily, GlyphAtlas, TextEngine, TextFormat, TextPaint};
use render::wgpu;

const ARIAL: &str = "/System/Library/Fonts/Supplemental/Arial.ttf";

const PARAGRAPH: &str = "Glass is a video editor for people who want to make something \
    good without fighting their tools. Cut, trim and arrange clips on a timeline, add \
    titles and captions, correct colour, mix audio and export for any screen. Every frame \
    is rendered on the GPU in linear light, so blends, blurs and scaling look right, and \
    the preview keeps up while you work.";

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&Default::default())).ok()
}

fn text(c: &mut Criterion) {
    if !Path::new(ARIAL).exists() {
        eprintln!("skipped: {ARIAL} not found");
        return;
    }
    let mut engine = TextEngine::with_font_files(&[Path::new(ARIAL)]).expect("Arial loads");
    let format = TextFormat {
        family: FontFamily::Named("Arial".to_owned()),
        size: 48.0,
        max_width: Some(1600.0),
        ..Default::default()
    };
    let layout = engine.layout(PARAGRAPH, &format);
    let glyphs = layout.glyphs.len() as u64;

    let mut group = c.benchmark_group("text");
    group
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .throughput(Throughput::Elements(glyphs));
    group.bench_function("layout_paragraph", |b| {
        b.iter(|| black_box(engine.layout(black_box(PARAGRAPH), &format)))
    });
    if let Some((device, queue)) = device() {
        let mut atlas = GlyphAtlas::new(&device, &queue);
        let paint = TextPaint::default();
        let _ = atlas.prepare(&mut engine, &layout, &paint, None);
        group.bench_function("prepare_paragraph_warm", |b| {
            b.iter(|| {
                atlas.begin_frame();
                black_box(atlas.prepare(&mut engine, &layout, &paint, None))
            })
        });
    }
    group.finish();
}

criterion_group!(benches, text);
criterion_main!(benches);
