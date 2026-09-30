//! Scene cut, silence, beat and loudness detection on the generated fixtures. Skips
//! without fixtures (`tests/fixtures/generate.sh`).

mod common;

use std::cell::Cell;

use media::analysis::loudness::{self, LoudnessSettings};
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

/// Integrated loudness, loudness range and true peak against FFmpeg's `ebur128` filter
/// (MANIFEST, one decimal, so its own measurement is within 0.05 of what it prints):
/// integrated within 0.1 LU, true peak within 0.3 dB (FFmpeg oversamples to 192 kHz,
/// libebur128 4x with a 49-tap filter; the 44.1 kHz file is measured at its own rate
/// while FFmpeg's filter resamples it to 48 kHz first), loudness range within 0.5 LU:
/// FFmpeg's filter bins short-term values into a histogram and its percentiles on the
/// program's level steps land 0.3 to 0.4 LU above libebur128's, which passes EBU Tech
/// 3342 (unit tests).
#[test]
fn loudness_matches_ffmpeg_ebur128() {
    for file in [
        "loudness_program_48k_stereo.wav",
        "loudness_program_44k1_stereo.flac",
        "loudness_51_48k.wav",
    ] {
        let (Some(path), Some(entry)) = (common::fixture(file), entry(file)) else {
            return;
        };
        let reference = &entry["ebur128"];
        let want = |k: &str| reference[k].as_f64().expect("reference");
        let found = loudness::measure(&path, &LoudnessSettings, &Unattended).expect("measure");
        let integrated = found.integrated_lufs.expect("integrated");
        let tp = found.true_peak_dbtp.expect("true peak");
        println!(
            "{file}: I {integrated:.3} LUFS (FFmpeg {}), LRA {:.3} LU (FFmpeg {}), TP {tp:.3} dBTP (FFmpeg {}), max S {:.2}, max M {:.2}",
            want("integrated"),
            found.range_lu,
            want("lra"),
            want("true_peak"),
            found.max_short_term_lufs.unwrap_or(f64::NAN),
            found.max_momentary_lufs.unwrap_or(f64::NAN),
        );
        let rate = entry["audio"]["sample_rate"].as_u64().expect("rate") as u32;
        let channels = entry["audio"]["channels"].as_u64().expect("channels") as usize;
        assert_eq!((found.sample_rate, found.channels), (rate, channels.min(6)));
        assert!(
            (integrated - want("integrated")).abs() <= 0.1,
            "{file}: I {integrated}"
        );
        assert!(
            (found.range_lu - want("lra")).abs() <= 0.5,
            "{file}: LRA {}",
            found.range_lu
        );
        assert!((tp - want("true_peak")).abs() <= 0.3, "{file}: TP {tp}");
        let seconds = entry["duration"].as_f64().expect("duration");
        let points = (seconds * 10.0).round() as usize;
        assert_eq!(found.short_term.len(), points, "{file}");
        assert_eq!(found.momentary.len(), points, "{file}");
        assert!(found.short_term[29..].iter().all(Option::is_some), "{file}");
    }
}

/// The program fixture's levels change every 8 s (noise gain 0.4, 0.08, 0.2, 0.6): the
/// short-term curve settles to steps whose differences are those gains in dB.
#[test]
fn loudness_curve_follows_the_program() {
    let Some(path) = common::fixture("loudness_program_48k_stereo.wav") else {
        return;
    };
    let found = loudness::measure(&path, &LoudnessSettings, &Unattended).expect("measure");
    // Windows ending at 7.5, 15.5 and 23.5 s lie inside one level (the tone at 12-14 s
    // is inside the second window, so it is skipped there: 11.5 s ends before it).
    let at = |s: f64| f64::from(found.short_term[(s * 10.0) as usize - 1].expect("level"));
    let db = |g: f64| 20.0 * g.log10();
    let steps = [
        (at(11.5) - at(7.5), db(0.08 / 0.4)),
        (at(23.5) - at(11.5), db(0.2 / 0.08)),
        (at(29.5) - at(23.5), db(0.6 / 0.2)),
    ];
    for (got, want) in steps {
        assert!(
            (got - want).abs() < 0.5,
            "step {got:.2} dB, expected {want:.2}"
        );
    }
}

#[test]
fn loudness_reports_progress_and_stops_when_cancelled() {
    let Some(path) = common::fixture("loudness_program_48k_stereo.wav") else {
        return;
    };
    let control = StopAfter {
        left: Cell::new(60),
        progress: Cell::new(0.0),
    };
    let result = loudness::measure(&path, &LoudnessSettings, &control);
    assert!(matches!(result, Err(AnalysisError::Stopped)), "{result:?}");
    let progress = control.progress.get();
    assert!(progress > 0.2 && progress < 0.6, "progress {progress}");
}
