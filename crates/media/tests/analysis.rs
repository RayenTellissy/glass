//! Scene cut, silence, beat and loudness detection on the generated fixtures. Skips
//! without fixtures (`tests/fixtures/generate.sh`).

mod common;

use std::cell::Cell;

use media::analysis::scenes::{self, CutKind, SceneSettings};
use media::analysis::silence::{self, SilenceSettings};
use media::analysis::{AnalysisError, Control, Stopped, Unattended};

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

fn pairs(value: &serde_json::Value) -> Vec<(f64, f64)> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|p| (p[0].as_f64().expect("start"), p[1].as_f64().expect("end")))
        .collect()
}

/// The worst edge error in seconds, after checking the range count.
fn worst_edge(got: &[(f64, f64)], expected: &[(f64, f64)]) -> f64 {
    assert_eq!(got.len(), expected.len(), "{got:?} vs {expected:?}");
    got.iter()
        .zip(expected)
        .map(|(g, e)| (g.0 - e.0).abs().max((g.1 - e.1).abs()))
        .fold(0.0, f64::max)
}

/// Edges against sound land within the 1 ms hop; against the -30 dBFS tone a window may
/// hold 2 ms of it before it rises 3 dB over the -40 dBFS threshold (hysteresis).
#[test]
fn silences_match_the_fixture_within_3_ms_in_pcm_and_10_ms_in_aac() {
    for (file, tolerance) in [
        ("silence_48k_stereo.wav", 0.003),
        ("silence_48k_stereo.m4a", 0.010),
    ] {
        let (Some(path), Some(entry)) = (common::fixture(file), entry(file)) else {
            return;
        };
        let expected = pairs(&entry["silences"]);
        let found =
            silence::detect(&path, &SilenceSettings::default(), &Unattended).expect("detect");
        assert_eq!(
            (found.first_sample, found.end_sample),
            (0, 480_000),
            "{file}"
        );
        let worst = worst_edge(&found.seconds(), &expected);
        println!("{file}: worst silence edge error {:.2} ms", worst * 1e3);
        assert!(worst <= tolerance, "{file}: {:?}", found.seconds());
    }
}

#[test]
fn silence_threshold_and_minimum_length_change_the_result() {
    let Some(path) = common::fixture("silence_48k_stereo.wav") else {
        return;
    };
    let run = |settings: SilenceSettings| {
        silence::detect(&path, &settings, &Unattended)
            .expect("detect")
            .seconds()
    };
    // At -25 dBFS the -30 dBFS tone is quiet too: 5.02 s to the end is one silence. A
    // window holding up to 2 ms of the -15 dBFS tone is still below -25 dBFS, so the edges
    // reach that much further into the sound.
    let loose = run(SilenceSettings {
        threshold_db: -25.0,
        ..SilenceSettings::default()
    });
    let expected = [(0.0, 0.7), (1.5, 2.3), (3.5, 5.0), (5.02, 10.0)];
    assert!(worst_edge(&loose, &expected) <= 0.003, "{loose:?}");
    // Below the -61 dBFS noise floor nothing is silent.
    let strict = run(SilenceSettings {
        threshold_db: -70.0,
        ..SilenceSettings::default()
    });
    assert!(strict.is_empty(), "{strict:?}");
    // At a 1 s minimum only 3.5-5.0 and 7.2-10.0 remain; at 250 ms 2.6-2.9 joins.
    let long = run(SilenceSettings {
        min_ms: 1000,
        ..SilenceSettings::default()
    });
    assert!(
        worst_edge(&long, &[(3.5, 5.0), (7.2, 10.0)]) <= 0.002,
        "{long:?}"
    );
    let short = run(SilenceSettings {
        min_ms: 250,
        ..SilenceSettings::default()
    });
    let expected = [
        (0.0, 0.7),
        (1.5, 2.3),
        (2.6, 2.9),
        (3.5, 5.0),
        (5.02, 6.0),
        (7.2, 10.0),
    ];
    assert!(worst_edge(&short, &expected) <= 0.003, "{short:?}");
}

/// Stops at the `n`th checkpoint and records the progress it was told.
struct StopAfter {
    left: Cell<u32>,
    progress: Cell<f32>,
}

impl Control for StopAfter {
    fn checkpoint(&self) -> Result<(), Stopped> {
        let left = self.left.get();
        if left == 0 {
            return Err(Stopped);
        }
        self.left.set(left - 1);
        Ok(())
    }

    fn progress(&self, fraction: f32) {
        assert!(fraction >= self.progress.get(), "progress went back");
        self.progress.set(fraction);
    }

    fn partial(&self, _: u32) {}
}

#[test]
fn silence_detection_reports_progress_and_stops_when_cancelled() {
    let Some(path) = common::fixture("silence_48k_stereo.wav") else {
        return;
    };
    let control = StopAfter {
        left: Cell::new(20),
        progress: Cell::new(0.0),
    };
    let result = silence::detect(&path, &SilenceSettings::default(), &control);
    assert!(matches!(result, Err(AnalysisError::Stopped)), "{result:?}");
    let progress = control.progress.get();
    assert!(progress > 0.2 && progress < 0.5, "progress {progress}");
}
