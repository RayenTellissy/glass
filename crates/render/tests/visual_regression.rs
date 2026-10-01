//! Visual regression tests: every case in `visual::cases::all()` (a short reference
//! sequence built with the render graph) rendered at each of its frame indices and
//! compared with `tests/visual/goldens/<name>/frame_<index>.png`.
//!
//! - `GLASS_UPDATE_SNAPSHOTS=1` rewrites the goldens.
//! - On a mismatch the test writes `frame_<index>.actual.png` and `.diff.png` (differing
//!   pixels in magenta) next to the golden; both are git-ignored.
//! - Without a GPU the test prints SKIPPED and returns, unless `GLASS_REQUIRE_GPU=1`. A
//!   software adapter compares with `visual::SOFTWARE` instead of `visual::GPU`.
//! - Goldens are macOS-only (texts are set in the Arial macOS ships): elsewhere the test
//!   prints SKIPPED.
//!
//! See CONTRIBUTING.md, "Visual regression tests".

mod common;
mod visual;

use std::path::{Path, PathBuf};

use image::ImageEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use visual::compare::{self, Comparison, Image};
use visual::{CANVAS, Studio};

const UPDATE_ENV: &str = "GLASS_UPDATE_SNAPSHOTS";
const REQUIRE_GPU_ENV: &str = "GLASS_REQUIRE_GPU";
const DIFF_MARKER: [u8; 4] = [255, 0, 255, 255];

fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/visual/goldens")
}

fn env_is(name: &str) -> bool {
    std::env::var(name).as_deref() == Ok("1")
}

#[test]
fn every_golden_folder_has_a_case() {
    let cases = visual::cases::all();
    let problems = visual::registry_problems(&cases);
    assert!(
        problems.is_empty(),
        "visual regression registry: {problems:#?}"
    );

    let mut stale = Vec::new();
    if let Ok(entries) = std::fs::read_dir(goldens_dir()) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(case) = cases.iter().find(|c| c.name == name) else {
                if entry.path().is_dir() {
                    stale.push(name);
                }
                continue;
            };
            for file in std::fs::read_dir(entry.path()).into_iter().flatten() {
                let file = file.expect("read golden folder").file_name();
                let file = file.to_string_lossy();
                let golden = !file.ends_with(".actual.png") && !file.ends_with(".diff.png");
                if golden
                    && !case
                        .frames
                        .iter()
                        .any(|&f| visual::golden_file_name(f) == *file)
                {
                    stale.push(format!("{name}/{file}"));
                }
            }
        }
    }
    stale.sort();
    assert!(
        stale.is_empty(),
        "goldens under crates/render/tests/visual/goldens with no case or frame; delete them: \
         {stale:?}"
    );
}

#[test]
fn reference_sequences_match_their_goldens() {
    let update = env_is(UPDATE_ENV);
    if !Path::new(visual::ARIAL).exists() {
        assert!(
            !cfg!(target_os = "macos"),
            "visual regression: {} is missing",
            visual::ARIAL
        );
        eprintln!(
            "SKIPPED visual regression: the goldens use {}, which only macOS ships",
            visual::ARIAL
        );
        return;
    }
    let Some(mut studio) = Studio::new() else {
        assert!(
            !env_is(REQUIRE_GPU_ENV),
            "visual regression: no GPU adapter and {REQUIRE_GPU_ENV}=1"
        );
        eprintln!("SKIPPED visual regression: no GPU adapter");
        return;
    };
    if studio.software && env_is(REQUIRE_GPU_ENV) {
        panic!(
            "visual regression: {} is a software adapter and {REQUIRE_GPU_ENV}=1",
            studio.adapter
        );
    }
    let tolerance = if studio.software {
        visual::SOFTWARE
    } else {
        visual::GPU
    };
    eprintln!(
        "visual regression on {} with {tolerance:?}{}",
        studio.adapter,
        if update { ", updating goldens" } else { "" }
    );

    let (width, height) = CANVAS;
    let mut failures = Vec::new();
    let mut compared = 0;
    for case in visual::cases::all() {
        let dir = goldens_dir().join(case.name);
        for &frame in case.frames {
            let scene = (case.scene)(frame);
            let rgba = studio.render(&scene, frame);
            let golden = dir.join(visual::golden_file_name(frame));
            let actual_path = golden.with_extension("actual.png");
            let diff_path = golden.with_extension("diff.png");
            let _ = std::fs::remove_file(&actual_path);
            let _ = std::fs::remove_file(&diff_path);
            if update {
                std::fs::create_dir_all(&dir).expect("create case folder");
                write_png(&golden, width, height, &rgba);
                continue;
            }
            compared += 1;
            let label = format!("{}/{}", case.name, visual::golden_file_name(frame));
            let Some((golden_width, golden_height, expected)) = read_png(&golden) else {
                write_png(&actual_path, width, height, &rgba);
                failures.push(format!(
                    "{label}: no golden; run with {UPDATE_ENV}=1 to create it"
                ));
                continue;
            };
            // Goldens hold no alpha; the scenes are opaque.
            let actual: Vec<u8> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|px| [px[0], px[1], px[2], 255])
                .collect();
            let result = compare::compare(
                Image {
                    width: golden_width,
                    height: golden_height,
                    rgba: &expected,
                },
                Image {
                    width,
                    height,
                    rgba: &actual,
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
            write_png(&actual_path, width, height, &rgba);
            if let Comparison::Mismatch { diff, .. } = &result {
                write_png(&diff_path, golden_width, golden_height, diff);
            }
            failures.push(format!("{label}: {result}"));
        }
    }
    if update {
        eprintln!(
            "visual regression: goldens written to {}",
            goldens_dir().display()
        );
        return;
    }
    eprintln!(
        "visual regression: {compared} frames compared, {} differ",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "rendered frames differ from their goldens (see the .actual.png and .diff.png files \
         next to each golden; if the change is intended, rerun with {UPDATE_ENV}=1 and review \
         the new goldens):\n  {}",
        failures.join("\n  ")
    );
    if env_is(REQUIRE_GPU_ENV) {
        assert!(compared > 0, "no frame was compared");
    }
}

/// Writes RGB (the scenes are opaque), at the best PNG compression.
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
