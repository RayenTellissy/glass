//! UI snapshot tests: every case in `ui::snapshot::cases()` rendered headless in every
//! theme at 1x and 2x and compared with `tests/snapshots/<name>/<theme>@<scale>x.png`.
//!
//! - `GLASS_UPDATE_SNAPSHOTS=1` rewrites the goldens (and `tests/snapshots/FONTS.txt`).
//! - On a mismatch the test writes `<theme>@<scale>x.actual.png` and `.diff.png` (differing
//!   pixels in magenta) next to the golden; both are git-ignored.
//! - Without a GPU the test prints SKIPPED and returns, unless `GLASS_REQUIRE_GPU=1`. A
//!   software adapter compares with `Tolerance::SOFTWARE` instead of `Tolerance::GPU`.
//! - Goldens are macOS-only until the Geist fonts are bundled: elsewhere the test prints
//!   SKIPPED (see `ui::snapshot::fonts`).
//!
//! See CONTRIBUTING.md, "UI snapshots".

use std::path::{Path, PathBuf};

use image::ImageEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use ui::headless::{Capture, HeadlessUi, REQUIRE_GPU_ENV, Viewport};
use ui::snapshot::compare::{self, Comparison, Image, Tolerance};
use ui::snapshot::{self, SCALES, SnapshotCase, ThemeKind};

const UPDATE_ENV: &str = "GLASS_UPDATE_SNAPSHOTS";
const FONTS_FILE: &str = "FONTS.txt";
const DIFF_MARKER: [u8; 4] = [255, 0, 255, 255];
/// Frames drawn before the capture: egui settles some layouts on the second frame.
const SETTLE_FRAMES: usize = 3;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn goldens_dir() -> PathBuf {
    manifest_dir().join("tests/snapshots")
}

fn env_is(name: &str) -> bool {
    std::env::var(name).as_deref() == Ok("1")
}

#[test]
fn every_panel_dialog_and_param_module_has_a_snapshot_case() {
    let cases = snapshot::cases();
    let src = manifest_dir().join("src");
    let problems = snapshot::registry_problems(&src, &cases);
    assert!(problems.is_empty(), "snapshot registry: {problems:#?}");

    let missing = snapshot::uncovered_modules(&src, &cases);
    assert!(
        missing.is_empty(),
        "these UI modules have no snapshot case; add one to crates/ui/src/snapshot/cases.rs \
         and run with {UPDATE_ENV}=1 to write its goldens: {missing:#?}"
    );

    let mut stale = Vec::new();
    if let Ok(entries) = std::fs::read_dir(goldens_dir()) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() && !cases.iter().any(|c| c.name == name) {
                stale.push(name);
            }
        }
    }
    stale.sort();
    assert!(
        stale.is_empty(),
        "golden folders under crates/ui/tests/snapshots with no case; delete them: {stale:?}"
    );
}

#[test]
fn ui_snapshots_match_their_goldens() {
    let update = env_is(UPDATE_ENV);
    let fonts = match snapshot::fonts::pinned() {
        Ok(fonts) => fonts,
        Err(reason) if !cfg!(target_os = "macos") => {
            eprintln!("SKIPPED ui snapshots: {reason}");
            return;
        }
        Err(reason) => panic!("ui snapshots: {reason}"),
    };
    let Some(mut headless) = HeadlessUi::new_or_skip("ui snapshots", Viewport::default()) else {
        return;
    };
    let tolerance = if headless.is_software() {
        Tolerance::SOFTWARE
    } else {
        Tolerance::GPU
    };
    eprintln!(
        "ui snapshots on {} with {tolerance:?}{}",
        headless.adapter(),
        if update { ", updating goldens" } else { "" }
    );

    let fonts_path = goldens_dir().join(FONTS_FILE);
    let golden_fonts = std::fs::read_to_string(&fonts_path).unwrap_or_default();
    let font_note = if golden_fonts.trim() == fonts.fingerprint.trim() {
        String::new()
    } else {
        format!(
            "\nThe fonts differ from the ones the goldens were made with.\n  goldens:\n{}\n  here:\n{}\n\
             If the new fonts are intended (for example the Geist fonts were bundled), \
             regenerate with {UPDATE_ENV}=1.",
            indent(&golden_fonts),
            indent(&fonts.fingerprint)
        )
    };
    if update {
        std::fs::create_dir_all(goldens_dir()).expect("create goldens folder");
        std::fs::write(&fonts_path, format!("{}\n", fonts.fingerprint.trim()))
            .expect("write FONTS.txt");
    }

    let mut failures = Vec::new();
    let mut compared = 0;
    for case in snapshot::cases() {
        let dir = goldens_dir().join(case.name);
        for theme in ThemeKind::ALL {
            for scale in SCALES {
                let capture = render(&mut headless, &fonts, &case, theme, scale);
                let golden = dir.join(snapshot::golden_file_name(theme, scale));
                let actual_path = golden.with_extension("actual.png");
                let diff_path = golden.with_extension("diff.png");
                let _ = std::fs::remove_file(&actual_path);
                let _ = std::fs::remove_file(&diff_path);
                if update {
                    std::fs::create_dir_all(&dir).expect("create case folder");
                    write_png(&golden, capture.width, capture.height, &capture.rgba);
                    continue;
                }
                compared += 1;
                let label = format!("{}/{}", case.name, snapshot::golden_file_name(theme, scale));
                let Some((width, height, rgba)) = read_png(&golden) else {
                    write_png(&actual_path, capture.width, capture.height, &capture.rgba);
                    failures.push(format!(
                        "{label}: no golden; run with {UPDATE_ENV}=1 to create it"
                    ));
                    continue;
                };
                let result = compare::compare(
                    Image {
                        width,
                        height,
                        rgba: &rgba,
                    },
                    Image {
                        width: capture.width,
                        height: capture.height,
                        rgba: &capture.rgba,
                    },
                    tolerance,
                    DIFF_MARKER,
                );
                if let Comparison::Match { differing, .. } = result
                    && differing > 0
                {
                    eprintln!("{label}: {result}");
                }
                if result.is_match() {
                    continue;
                }
                write_png(&actual_path, capture.width, capture.height, &capture.rgba);
                if let Comparison::Mismatch { diff, .. } = &result {
                    write_png(&diff_path, width, height, diff);
                }
                failures.push(format!("{label}: {result}"));
            }
        }
    }
    if update {
        eprintln!(
            "ui snapshots: goldens written to {}",
            goldens_dir().display()
        );
        return;
    }
    eprintln!(
        "ui snapshots: {compared} images compared, {} differ",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "UI snapshots differ from their goldens (see the .actual.png and .diff.png files next \
         to each golden; if the change is intended, rerun with {UPDATE_ENV}=1 and review the \
         new goldens):\n  {}{font_note}",
        failures.join("\n  ")
    );
    if env_is(REQUIRE_GPU_ENV) {
        assert!(compared > 0, "no snapshot was compared");
    }
}

fn render(
    headless: &mut HeadlessUi,
    fonts: &snapshot::fonts::PinnedFonts,
    case: &SnapshotCase,
    theme: ThemeKind,
    scale: u32,
) -> Capture {
    let px = |points: f32| (points * scale as f32).round() as u32;
    headless
        .set_viewport(Viewport {
            width_px: px(case.size.x),
            height_px: px(case.size.y),
            pixels_per_point: scale as f32,
        })
        .expect("resize the offscreen target");
    let ctx = headless.context().clone();
    // Each image starts from a fresh egui memory, so nothing one case or theme lays out
    // (panel sizes, animations, focus) leaks into the next.
    ctx.memory_mut(|memory| *memory = Default::default());
    snapshot::fonts::install(&ctx, fonts);
    ui::theme::set(&ctx, theme);
    let mut capture = headless
        .capture(SETTLE_FRAMES, case.draw)
        .unwrap_or_else(|e| panic!("{}: {e}", case.name));
    assert!(
        ctx.fonts(|f| f.definitions() == &fonts.definitions),
        "{}: rendered without the pinned fonts",
        case.name
    );
    // The target is opaque; egui's blending can leave alpha below 255 where nothing
    // opaque was drawn, which is not part of what the user sees.
    for px in capture.rgba.as_chunks_mut::<4>().0.iter_mut() {
        px[3] = 255;
    }
    capture
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Writes RGB (the alpha is always opaque), at the best PNG compression.
fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
    let rgb: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();
    let file = std::fs::File::create(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    PngEncoder::new_with_quality(
        std::io::BufWriter::new(file),
        CompressionType::Best,
        FilterType::Adaptive,
    )
    .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn read_png(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let image = image::open(path).ok()?.to_rgba8();
    Some((image.width(), image.height(), image.into_raw()))
}
