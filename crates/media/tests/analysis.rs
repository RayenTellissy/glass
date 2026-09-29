//! Scene cut, silence, beat and loudness detection on the generated fixtures. Skips
//! without fixtures (`tests/fixtures/generate.sh`).

mod common;

use media::analysis::Unattended;
use media::analysis::scenes::{self, CutKind, SceneSettings};

fn entry(file: &str) -> Option<serde_json::Value> {
    common::manifest()?
        .into_iter()
        .find(|e| e["file"].as_str() == Some(file))
}

fn numbers(value: &serde_json::Value) -> Vec<u64> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_u64().expect("number"))
        .collect()
}

#[test]
fn scene_cuts_land_on_the_exact_frames() {
    let file = "scenes_cuts_640x360_30.mp4";
    let (Some(path), Some(entry)) = (common::fixture(file), entry(file)) else {
        return;
    };
    let hard = numbers(&entry["hard_cuts"]);
    let fade = entry["fade_through_black"].as_u64().expect("fade");
    for sensitivity in [0.0, 0.5, 1.0] {
        let settings = SceneSettings {
            sensitivity,
            ..SceneSettings::default()
        };
        let found = scenes::detect(&path, &settings, &Unattended).expect("detect");
        assert_eq!(found.frames, 360);
        assert!((found.fps - 30.0).abs() < 1e-6);
        let hard_found: Vec<u64> = found
            .cuts
            .iter()
            .filter(|c| c.kind == CutKind::Hard)
            .map(|c| c.frame)
            .collect();
        assert_eq!(hard_found, hard, "sensitivity {sensitivity}: {found:?}");
        let fades: Vec<u64> = found
            .cuts
            .iter()
            .filter(|c| c.kind == CutKind::Fade)
            .map(|c| c.frame)
            .collect();
        assert_eq!(fades.len(), 1, "sensitivity {sensitivity}: {fades:?}");
        assert!(fades[0].abs_diff(fade) <= 1, "fade at {}", fades[0]);
        for cut in &found.cuts {
            assert_eq!(cut.time_us, cut.frame as i64 * 1_000_000 / 30);
        }
    }
}

#[test]
fn continuous_motion_has_no_cuts() {
    for file in [
        "h264_1080p30.mp4",
        "h264_bframes_gop120_720p60.mp4",
        "vp9_720p30.webm",
        "hevc_10bit_1080p25.mp4",
    ] {
        let Some(path) = common::fixture(file) else {
            return;
        };
        let found = scenes::detect(&path, &SceneSettings::default(), &Unattended).expect("detect");
        assert!(found.frames > 30, "{file}");
        assert!(found.cuts.is_empty(), "{file}: {:?}", found.cuts);
    }
}
