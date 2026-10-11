//! Burned-in subtitles: which cue shows at a time, the layout's size and place in the
//! frame, and the cue drawn over a frame on the GPU. The font is pinned to Arial, which
//! every macOS ships; tests skip when it (or a GPU adapter) is missing. The visual
//! regression case `subtitle_burn_in` holds the look.

mod common;

use std::path::Path;

use media::SubtitleCue;
use render::graph::{Graph, TexturePool};
use render::renderer::Renderer;
use render::subtitle::{self, SubtitleStyle};
use render::text::{FontFamily, GlyphAtlas, TextEngine};
use render::wgpu;

const ARIAL: &str = "/System/Library/Fonts/Supplemental/Arial.ttf";

fn engine() -> Option<TextEngine> {
    if !Path::new(ARIAL).exists() {
        eprintln!("skipped: {ARIAL} not found");
        return None;
    }
    Some(TextEngine::with_font_files(&[Path::new(ARIAL)]).expect("Arial loads"))
}

fn style() -> SubtitleStyle {
    SubtitleStyle {
        family: FontFamily::Named("Arial".to_owned()),
        ..Default::default()
    }
}

fn cue(start_ms: i64, end_ms: i64, text: &str) -> SubtitleCue {
    SubtitleCue {
        start_ms,
        end_ms,
        text: text.to_string(),
    }
}

#[test]
fn the_cue_at_a_time_starts_at_or_before_it_and_ends_after_it() {
    let cues = [
        cue(0, 1000, "a"),
        cue(1000, 1500, "b"),
        cue(2000, 3000, "c"),
    ];
    let at = |t| subtitle::cue_at(&cues, t).map(|c| c.text.as_str());
    assert_eq!(at(-1), None);
    assert_eq!(at(0), Some("a"));
    assert_eq!(at(999), Some("a"));
    assert_eq!(at(1000), Some("b"));
    assert_eq!(at(1500), None);
    assert_eq!(at(1999), None);
    assert_eq!(at(2999), Some("c"));
    assert_eq!(at(3000), None);
    assert_eq!(subtitle::cue_at(&[], 0), None);
}

/// Each line's horizontal extent in canvas pixels, from its glyphs' advances.
fn line_extents(burn: &subtitle::BurnIn) -> Vec<(f32, f32)> {
    burn.layout
        .lines
        .iter()
        .map(|line| {
            let glyphs = &burn.layout.glyphs[line.glyphs.clone()];
            let left = glyphs.iter().map(|g| g.glyph.x).fold(f32::MAX, f32::min);
            let right = glyphs
                .iter()
                .map(|g| g.glyph.x + g.glyph.w)
                .fold(f32::MIN, f32::max);
            (burn.paint.origin[0] + left, burn.paint.origin[0] + right)
        })
        .collect()
}

#[test]
fn cues_sit_centred_on_the_bottom_of_the_title_safe_area() {
    let Some(mut engine) = engine() else { return };
    let style = style();
    for (w, h) in [(1920, 1080), (3840, 2160), (1080, 1920)] {
        let format = style.format((w, h));
        assert_eq!(format.size, h as f32 * 0.05);
        let burn = style.lay_out(&mut engine, "Hello, subtitles", (w, h));
        assert_eq!(burn.layout.lines.len(), 1);
        let bottom = burn.paint.origin[1] + burn.layout.height;
        assert!(
            (bottom - h as f32 * 0.95).abs() < 0.01,
            "{bottom} at {w}x{h}"
        );
        let (left, right) = line_extents(&burn)[0];
        let centre = (left + right) / 2.0;
        assert!((centre - w as f32 / 2.0).abs() < 1.0, "{centre} at {w}x{h}");
        let outline = burn.paint.outline.expect("outline");
        assert!((outline.width - format.size * 0.08).abs() < 1e-4);
        assert!(burn.paint.shadow.is_some());
        assert_eq!(burn.paint.fill, [1.0; 4]);
    }
}

#[test]
fn long_cues_wrap_inside_the_safe_width_and_keep_forced_breaks() {
    let Some(mut engine) = engine() else { return };
    let style = style();
    let (w, h) = (1920, 1080);
    let text = "A subtitle long enough that it cannot fit on one line of a full HD frame at \
                this size, so it wraps\nthen a forced line";
    let burn = style.lay_out(&mut engine, text, (w, h));
    assert!(
        burn.layout.lines.len() >= 3,
        "{} lines",
        burn.layout.lines.len()
    );
    for (left, right) in line_extents(&burn) {
        assert!(left >= w as f32 * 0.05 - 0.5, "{left}");
        assert!(right <= w as f32 * 0.95 + 0.5, "{right}");
        assert!(((left + right) / 2.0 - w as f32 / 2.0).abs() < 1.0);
    }
    let bottom = burn.paint.origin[1] + burn.layout.height;
    assert!((bottom - h as f32 * 0.95).abs() < 0.01);
}

#[test]
fn the_cue_is_drawn_over_the_frame() {
    let Some(mut engine) = engine() else { return };
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let size = (960, 540);
    let renderer = Renderer::new(&device, &queue);
    let mut pool = TexturePool::new();
    let mut atlas = GlyphAtlas::new(&device, &queue);
    let grey = [0.2, 0.2, 0.2, 1.0];
    let background = common::rgba16f(&device, &queue, size, &vec![grey; 960 * 540]);
    let background = background.create_view(&Default::default());
    let target = common::blank(&device, wgpu::TextureFormat::Rgba16Float, size);
    let target_view = target.create_view(&Default::default());

    let burn = style().lay_out(&mut engine, "HHHH", size);
    atlas.begin_frame();
    let prepared = atlas.prepare(&mut engine, &burn.layout, &burn.paint, None);
    let mut g = Graph::new();
    let frame = g.import("frame", &background);
    let out = subtitle::add_burn_in(&mut g, frame, &atlas, &prepared, size);
    g.export(out, &target_view);
    g.compile()
        .expect("compiles")
        .execute(&renderer, &mut pool, None);
    let pixels = common::read(&device, &queue, &target);
    let at = |x: usize, y: usize| pixels[y * 960 + x];

    // The top half is the frame untouched.
    for y in 0..270 {
        for x in (0..960).step_by(7) {
            let p = at(x, y);
            assert!(
                (p[0] - 0.2).abs() < 0.01 && (p[3] - 1.0).abs() < 1e-3,
                "{p:?}"
            );
        }
    }
    // The text's rows near the bottom centre hold white fill and dark outline.
    let rows = (burn.paint.origin[1] as usize)..(513usize);
    let (mut white, mut dark) = (0, 0);
    for y in rows {
        for x in 420..540 {
            let p = at(x, y);
            if p[0] > 0.9 {
                white += 1;
            }
            if p[0] < 0.05 {
                dark += 1;
            }
        }
    }
    assert!(white > 10 && dark > 10, "white {white}, dark {dark}");
}
