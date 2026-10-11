//! Every picture fixture in tests/fixtures/generated/MANIFEST.json (video, still,
//! animation and image sequence) decodes through `media::VideoSource` into GPU textures
//! the way the timeline renderer does it: software frames through
//! `render::upload::upload_frame`, and, where `HwPreference::Auto` picks a hardware
//! decoder, surfaces through `render::interop::SurfaceImporter`. The first frame and one
//! from the middle are converted on the GPU, read back and compared with swscale's
//! conversion of the software decode of the same frame. Fixtures this machine could not
//! generate are not in the manifest and are reported as skipped. Skips without fixtures
//! or a GPU with 16-bit normalised textures.

mod common;

use std::path::{Path, PathBuf};

use common::{Gpu, convert, diff, gpu, read_rgba8};
use media::{
    DecoderOptions, HwPreference, Rational, SourceOptions, Timestamp, VideoFrame, VideoSource,
};
use render::upload::upload_frame;
use render::wgpu;
use render::yuv::{YuvParams, YuvPlanes};

/// Fixtures that need an encoder or tool the pinned FFmpeg build or this machine may not
/// have; generate.sh leaves them out of the manifest when it cannot make them.
const OPTIONAL: [&str; 5] = [
    "vp9_720p30.webm",
    "sync_vp9_opus.webm",
    "av1_1080p30.mp4",
    "anim_vfr_160x120.webp",
    "still_marked_640x360.webp",
];

fn generated() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated")
}

/// A sequence entry names a `%04d` pattern; it is present when its folder is.
fn present(path: &Path) -> bool {
    if path.to_string_lossy().contains('%') {
        path.parent().is_some_and(Path::is_dir)
    } else {
        path.exists()
    }
}

fn open(path: &Path, hw: HwPreference) -> VideoSource {
    VideoSource::open(
        path,
        SourceOptions {
            decoder: DecoderOptions::with_hw(hw),
            ..SourceOptions::new(Rational::new(30, 1))
        },
    )
    .unwrap_or_else(|e| panic!("{}: open: {e}", path.display()))
}

/// The first frame and the frame at `mid` (when there is one).
fn frames(source: &mut VideoSource, mid: Option<Timestamp>) -> Vec<(&'static str, VideoFrame)> {
    let mut out = vec![(
        "first",
        source.next_frame().expect("decode").expect("a frame"),
    )];
    if let Some(mid) = mid {
        out.push((
            "mid",
            source.seek_exact(mid).expect("seek").expect("a frame"),
        ));
    }
    out
}

/// GPU conversion of a software frame through the upload path, after checking the plane
/// textures' sizes and formats against the frame.
fn software_to_rgba8(gpu: &Gpu, label: &str, frame: &VideoFrame) -> Vec<u8> {
    let uploaded = upload_frame(&gpu.device, &gpu.queue, frame)
        .unwrap_or_else(|e| panic!("{label}: upload: {e}"));
    let format = frame.format();
    assert_eq!(
        uploaded.textures.len(),
        format.plane_count(),
        "{label}: one texture per plane of {}",
        format.name()
    );
    let (sx, sy) = format.chroma_shift();
    let want_format = if format.bit_depth() > 8 {
        wgpu::TextureFormat::R16Unorm
    } else {
        wgpu::TextureFormat::R8Unorm
    };
    for (i, texture) in uploaded.textures.iter().enumerate() {
        let (w, h) = if i == 0 || format.is_rgb() {
            (frame.width(), frame.height())
        } else {
            (
                frame.width().div_ceil(1 << sx),
                frame.height().div_ceil(1 << sy),
            )
        };
        assert_eq!(
            (texture.width(), texture.height()),
            (w, h),
            "{label}: plane {i} texture size"
        );
        if i == 0 || uploaded.params.layout.planar {
            assert_eq!(texture.format(), want_format, "{label}: plane {i} format");
        }
    }
    let out = convert(
        gpu,
        uploaded.planes(),
        &uploaded.params.encoded(),
        (frame.width(), frame.height()),
        wgpu::TextureFormat::Rgba16Float,
    );
    assert_eq!((out.width(), out.height()), (frame.width(), frame.height()));
    read_rgba8(gpu, &out)
}

/// GPU conversion of a hardware frame through the zero-copy import, after checking the
/// imported textures against the surface.
fn hardware_to_rgba8(gpu: &Gpu, label: &str, frame: &VideoFrame) -> Vec<u8> {
    let surface = frame.hardware().expect("a hardware frame");
    let mut importer = render::interop::SurfaceImporter::default();
    let imported = importer
        .import(&gpu.device, surface)
        .unwrap_or_else(|e| panic!("{label}: import: {e}"));
    let (luma_format, chroma_format) = render::interop::plane_formats(surface.format());
    assert_eq!(imported.luma_texture.format(), luma_format, "{label}: luma");
    assert_eq!(
        imported.chroma_texture.format(),
        chroma_format,
        "{label}: chroma"
    );
    assert_eq!(
        (
            imported.luma_texture.width(),
            imported.luma_texture.height()
        ),
        (surface.width(), surface.height()),
        "{label}: luma size"
    );
    let size = (surface.width(), surface.height());
    let out = convert(
        gpu,
        YuvPlanes::Biplanar {
            luma: &imported.luma,
            chroma: &imported.chroma,
        },
        &YuvParams::for_frame(surface).encoded(),
        size,
        wgpu::TextureFormat::Rgba16Float,
    );
    read_rgba8(gpu, &out)
}

/// The tolerances of `colour_conversion.rs`: video within a mean of 0.05 and a maximum
/// of 2 levels (`software_frames_match_swscale_with_their_siting`, `hardware_decode.rs`),
/// pictures within 1 level (`stills_convert_like_swscale`; a 16-bit solid can sit on a
/// rounding edge at every pixel).
fn check(label: &str, video: bool, gpu_rgba: &[u8], reference: &[u8]) {
    let d = diff(gpu_rgba, reference);
    eprintln!(
        "{label}: GPU vs swscale mean {:.4}, p99 {}, max {}",
        d.mean, d.p99, d.max
    );
    if video {
        assert!(d.mean < 0.05, "{label}: mean difference {}", d.mean);
        assert!(d.max <= 2, "{label}: max difference {}", d.max);
    } else {
        assert!(d.max <= 1, "{label}: max difference {}", d.max);
    }
}

#[test]
fn every_picture_fixture_decodes_to_a_texture() {
    let Ok(text) = std::fs::read_to_string(generated().join("MANIFEST.json")) else {
        eprintln!("skipped: no fixture manifest; run tests/fixtures/generate.sh to create it");
        return;
    };
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter with 16-bit normalised textures");
        return;
    };
    let manifest: serde_json::Value = serde_json::from_str(&text).expect("manifest is JSON");
    let entries = manifest["files"].as_array().expect("files");
    let listed: Vec<&str> = entries.iter().filter_map(|e| e["file"].as_str()).collect();
    for file in OPTIONAL {
        if !listed.contains(&file) {
            eprintln!("skipped: {file} not generated on this machine (see generate.sh notes)");
        }
    }

    let (mut present_count, mut covered, mut hardware) = (0, Vec::new(), Vec::new());
    for entry in entries {
        let kind = entry["kind"].as_str().expect("kind");
        if !matches!(
            kind,
            "video" | "image" | "image_sequence" | "still" | "animation" | "sequence"
        ) {
            continue;
        }
        let file = entry["file"].as_str().expect("file");
        let path = generated().join(file);
        if !present(&path) {
            eprintln!("skipped: {file} is in the manifest but missing; rerun generate.sh");
            continue;
        }
        present_count += 1;
        let size = (
            entry["width"].as_u64().expect("width") as u32,
            entry["height"].as_u64().expect("height") as u32,
        );
        let frame_count = entry["frames"].as_u64();
        let mid = match (entry["duration"].as_f64(), frame_count) {
            (_, Some(1)) => None,
            (Some(duration), _) => Some(duration / 2.0),
            (None, Some(n)) => Some(n as f64 / 30.0 / 2.0),
            (None, None) => None,
        }
        .map(|s| Timestamp::from_seconds(s, Rational::new(1, 1000)));

        let mut software = open(&path, HwPreference::Off);
        let mut references = Vec::new();
        for (which, frame) in frames(&mut software, mid) {
            let label = format!("{file} {which} ({}, software)", frame.format());
            assert_eq!(
                (frame.width(), frame.height()),
                size,
                "{label}: frame size against the manifest"
            );
            let reference = frame.to_rgba8().expect("swscale");
            check(
                &label,
                kind == "video",
                &software_to_rgba8(&gpu, &label, &frame),
                &reference.data,
            );
            references.push(reference.data);
        }

        if kind == "video" {
            let mut auto = open(&path, HwPreference::Auto);
            if auto.decode_path().is_some_and(|p| p.is_hardware()) {
                for ((which, frame), reference) in
                    frames(&mut auto, mid).into_iter().zip(&references)
                {
                    let label = format!("{file} {which} (hardware)");
                    assert!(frame.is_hardware(), "{label}: a hardware surface");
                    let rgba = hardware_to_rgba8(&gpu, &label, &frame);
                    check(&label, true, &rgba, reference);
                }
                hardware.push(file);
            }
        }
        covered.push(file);
    }

    eprintln!(
        "covered {} fixtures ({} also in hardware): {covered:?}; hardware: {hardware:?}",
        covered.len(),
        hardware.len()
    );
    assert!(present_count > 0, "the manifest lists picture fixtures");
    assert_eq!(
        covered.len(),
        present_count,
        "every present fixture covered"
    );
}
