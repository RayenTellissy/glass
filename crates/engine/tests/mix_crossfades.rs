//! Sample-accurate editing and crossfades in the offline mix (`engine::mix`): transitions
//! on audio tracks as crossfades (equal power, linear, S-curve) and fades from or to
//! nothing, cuts on exact samples, constant-speed clips across their edges, declick
//! ramps, and nested sequences mixed as their parent clip's source. Expected samples are
//! worked out from the files' own samples and the fade formulas, written out here.

mod mix_common;

use std::f64::consts::{FRAC_PI_2, PI};
use std::path::PathBuf;

use engine::audio::mixer::Layout;
use engine::media::{AudioOptions, AudioReader};
use engine::mix::{MixFormat, MixSettings, OfflineMix, mix_range, mix_range_with};
use engine::project::audio_transition::{self, EQUAL_POWER, LINEAR, S_CURVE};
use engine::project::{
    Alignment, ItemId, Project, Rational, Resolution, SequenceFormat, SequenceId, SpeedKeep, Time,
    TimeRange, TrackKind, TransitionSpec,
};
use engine::render::transition::Easing;
use mix_common::{Place, Tone, build, temp_dir, write_wav};

const RATE: u32 = 48_000;

/// `n` samples at 48 kHz.
fn at(n: i64) -> Time {
    Time::from_samples(n, RATE).unwrap()
}

/// Seconds as 48 kHz samples.
fn n(seconds: f64) -> i64 {
    (seconds * f64::from(RATE)).round() as i64
}

fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Uniform white noise in -amplitude..amplitude, one plane per channel, from `seed`.
fn noise(seed: u64, frames: usize, channels: usize, amplitude: f32) -> Vec<Vec<f32>> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move || {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let x = state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        (x as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    };
    (0..channels)
        .map(|_| (0..frames).map(|_| next() * amplitude).collect())
        .collect()
}

/// The fade formulas of a transition on an audio track, per sample at its centre over
/// `from..to`: `(outgoing, incoming)` gains.
fn fade(curve: &str, from: i64, to: i64, k: i64, easing: Easing) -> (f64, f64) {
    if k < from {
        return (1.0, 0.0);
    }
    if k >= to {
        return (0.0, 1.0);
    }
    let q = easing.apply(((k - from) as f64 + 0.5) / (to - from) as f64);
    match curve {
        LINEAR => (1.0 - q, q),
        S_CURVE => {
            let s = (q * FRAC_PI_2).sin();
            (1.0 - s * s, s * s)
        }
        _ => ((q * FRAC_PI_2).cos(), (q * FRAC_PI_2).sin()),
    }
}

fn stereo() -> MixFormat {
    MixFormat {
        sample_rate: RATE,
        layout: Layout::Stereo,
    }
}

fn mix(project: &Project, sequence: SequenceId, frames: i64) -> Vec<f32> {
    mix_range(project, sequence, TimeRange::new(Time::ZERO, at(frames))).unwrap()
}

/// The largest difference between `out` (stereo) and `expected(frame, channel)`.
fn worst(out: &[f32], expected: impl Fn(i64, usize) -> f64) -> f64 {
    out.iter()
        .enumerate()
        .map(|(i, &s)| (f64::from(s) - expected((i / 2) as i64, i % 2)).abs())
        .fold(0.0, f64::max)
}

fn add(project: &Project, spec: TransitionSpec) -> Project {
    project.add_transition(spec).unwrap().0
}

fn files(dir: &std::path::Path, planes: &[Vec<Vec<f32>>]) -> Vec<PathBuf> {
    planes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let path = dir.join(format!("source{i}.wav"));
            write_wav(&path, RATE, p);
            path
        })
        .collect()
}

#[test]
fn equal_power_crossfades_keep_the_power_of_uncorrelated_noise() {
    let dir = temp_dir("xfade-noise");
    let amplitude = 0.25f32;
    let sources = [
        noise(1, n(3.0) as usize, 2, amplitude),
        noise(2, n(3.0) as usize, 2, amplitude),
    ];
    let paths = files(&dir, &sources);
    // A over 0..1.5 s from its start, B over 1.5..3 s from 1 s in (a 1 s head handle); a
    // 1 s crossfade centred on the cut spans 1..2 s.
    let built = build(
        &paths,
        &[vec![
            Place {
                media: 0,
                start: 0.0,
                length: 1.5,
                source_start: 0.0,
            },
            Place {
                media: 1,
                start: 1.5,
                length: 1.5,
                source_start: 1.0,
            },
        ]],
    );
    let (a, b) = (built.clips[0][0], built.clips[0][1]);
    let (from, to) = (n(1.0), n(2.0));
    let nominal = f64::from(amplitude).powi(2) / 3.0;
    // Power in 100 ms windows over 0.25..2.75 s, in dB against the noise's own power.
    let windows = |out: &[f32]| -> Vec<f64> {
        (n(0.25)..n(2.75))
            .step_by(4800)
            .map(|w| {
                let samples = &out[w as usize * 2..(w as usize + 4800) * 2];
                let power = samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>()
                    / samples.len() as f64;
                10.0 * (power / nominal).log10()
            })
            .collect()
    };
    let sources = &sources;
    let expected = |curve: &'static str| {
        move |k: i64, c: usize| {
            let (g_out, g_in) = fade(curve, from, to, k, Easing::Linear);
            let x = if k < to {
                f64::from(sources[0][c][k as usize])
            } else {
                0.0
            };
            let y = if k >= from {
                f64::from(sources[1][c][(k - n(1.5) + n(1.0)) as usize])
            } else {
                0.0
            };
            x * g_out + y * g_in
        }
    };

    // The default transition kind (a picture dissolve's id) and the crossfade's own
    // equal-power curve play the same.
    for spec in [
        TransitionSpec::new(Some(a), Some(b), at(n(1.0))),
        audio_transition::crossfade_spec(Some(a), Some(b), at(n(1.0)), EQUAL_POWER),
    ] {
        let project = add(&built.project, spec);
        let out = mix(&project, built.sequence, n(3.0));
        let error = worst(&out, expected(EQUAL_POWER));
        assert!(error <= 1e-6, "equal power: worst error {error:e}");
        let levels = windows(&out);
        let off = levels.iter().fold(0.0f64, |m, l| m.max(l.abs()));
        assert!(off <= 0.5, "equal power: window levels {levels:.2?} dB");
    }

    // A linear crossfade of the same noise dips about 3 dB in the middle.
    let project = add(
        &built.project,
        audio_transition::crossfade_spec(Some(a), Some(b), at(n(1.0)), LINEAR),
    );
    let out = mix(&project, built.sequence, n(3.0));
    let error = worst(&out, expected(LINEAR));
    assert!(error <= 1e-6, "linear: worst error {error:e}");
    let levels = windows(&out);
    let middle = levels[levels.len() / 2];
    assert!(
        (-3.5..=-2.5).contains(&middle),
        "linear: middle window {middle:.2} dB"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn constant_gain_crossfades_keep_the_level_of_the_same_signal() {
    let dir = temp_dir("xfade-same");
    let tone = Tone {
        rate: RATE,
        seconds: 4.0,
        channels: vec![(440.0, 0.5), (660.0, 0.4)],
    };
    let path = dir.join("tone.wav");
    tone.write(&path);
    // One recording cut in two at 2 s: across the cut both sides read the same samples.
    let built = build(
        std::slice::from_ref(&path),
        &[vec![
            Place {
                media: 0,
                start: 0.0,
                length: 2.0,
                source_start: 0.0,
            },
            Place {
                media: 0,
                start: 2.0,
                length: 2.0,
                source_start: 2.0,
            },
        ]],
    );
    let (a, b) = (built.clips[0][0], built.clips[0][1]);
    let plain = |k: i64, c: usize| f64::from(tone.sample(c, k as usize));
    for curve in [LINEAR, S_CURVE] {
        let spec = audio_transition::crossfade_spec(Some(a), Some(b), at(n(0.5)), curve);
        let project = add(&built.project, spec);
        let out = mix(&project, built.sequence, n(4.0));
        let error = worst(&out, plain);
        assert!(error <= 1e-6, "{curve}: worst error {error:e}");
    }
    // Equal power on the same signal rises by up to 3 dB at the cut.
    let project = add(
        &built.project,
        audio_transition::crossfade_spec(Some(a), Some(b), at(n(0.5)), EQUAL_POWER),
    );
    let out = mix(&project, built.sequence, n(4.0));
    let k = n(2.0) as usize;
    let rise = (k - 200..k + 200)
        .map(|k| f64::from(out[k * 2]).abs() / plain(k as i64, 0).abs().max(1e-3))
        .fold(0.0, f64::max);
    assert!(
        rise > 1.35,
        "equal power on the same signal: peak ratio {rise}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An index-coded source: sample `k` holds `sign × (k + 1) × 1e-6`, so a mixed sample
/// names the file and the source sample it came from.
fn coded(sign: f32, frames: usize) -> Vec<Vec<f32>> {
    let plane: Vec<f32> = (0..frames).map(|k| sign * (k + 1) as f32 * 1e-6).collect();
    vec![plane.clone(), plane]
}

/// The (sign, source sample) a coded sample names; (0, 0) for silence.
fn decode(s: f32) -> (i32, i64) {
    if s == 0.0 {
        return (0, 0);
    }
    let sign = if s > 0.0 { 1 } else { -1 };
    (sign, (f64::from(s.abs()) / 1e-6).round() as i64 - 1)
}

#[test]
fn cuts_play_every_sample_once_on_the_exact_sample() {
    let dir = temp_dir("xfade-cuts");
    let mut impulse = vec![vec![0.0f32; 48_000]; 2];
    impulse[0][1000] = 0.5;
    impulse[1][1000] = 0.5;
    let paths = files(&dir, &[coded(1.0, 96_000), coded(-1.0, 96_000), impulse]);
    // X over 0..24001 from source sample 123, Y over 24001..54321 from source sample 4567,
    // then the impulse file from its sample 1000 (the impulse) at 54321: odd samples, off
    // every frame of the 30 fps grid.
    let (cut1, cut2, end) = (24_001i64, 54_321i64, 60_000i64);
    let secs = |n: i64| n as f64 / f64::from(RATE);
    let place = |media: usize, start: i64, end: i64, source: i64| Place {
        media,
        start: secs(start),
        length: secs(end - start),
        source_start: secs(source),
    };
    let built = build(
        &paths,
        &[vec![
            place(0, 0, cut1, 123),
            place(1, cut1, cut2, 4567),
            place(2, cut2, end, 1000),
        ]],
    );
    let out = mix(&built.project, built.sequence, end + 100);
    for k in 0..end + 100 {
        let (l, r) = (out[k as usize * 2], out[k as usize * 2 + 1]);
        assert_eq!(decode(l), decode(r), "frame {k}");
        if k < cut1 {
            assert_eq!(decode(l), (1, 123 + k), "frame {k}");
        } else if k < cut2 {
            assert_eq!(decode(l), (-1, 4567 + k - cut1), "frame {k}");
        } else if k == cut2 {
            assert!((l - 0.5).abs() < 1e-7, "the impulse lands on the cut: {l}");
        } else {
            assert_eq!(l, 0.0, "frame {k} after the impulse");
        }
    }

    // At another mix rate both clips of a cut round it to the same sample: two constant
    // sources meet there, with nothing doubled or dropped.
    let dc = |v: f32| vec![vec![v; 96_000]; 2];
    let paths = files(&dir, &[dc(0.5), dc(0.25)]);
    let built = build(
        &paths,
        &[vec![
            place(0, 0, cut1, 24_000),
            place(1, cut1, 48_000, 24_000),
        ]],
    );
    let (p, sequence) = (built.project, built.sequence);
    let format = MixFormat {
        sample_rate: 44_100,
        layout: Layout::Stereo,
    };
    let out = mix_range_with(
        &p,
        sequence,
        TimeRange::new(Time::ZERO, Time::seconds(1)),
        &MixSettings::default(),
        format,
    )
    .unwrap();
    // 24001 / 48000 s is sample 22050.92 at 44.1 kHz: the cut is sample 22051.
    let cut = 22_051usize;
    for k in 1000..cut {
        assert!((out[k * 2] - 0.5).abs() < 1e-3, "frame {k}: {}", out[k * 2]);
    }
    for k in cut..43_000 {
        assert!(
            (out[k * 2] - 0.25).abs() < 1e-3,
            "frame {k}: {}",
            out[k * 2]
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn transitions_next_to_gaps_fade_from_and_to_nothing() {
    let dir = temp_dir("xfade-gaps");
    let tone = Tone {
        rate: RATE,
        seconds: 4.0,
        channels: vec![(300.0, 0.5), (450.0, 0.5)],
    };
    let path = dir.join("tone.wav");
    tone.write(&path);
    // The clip plays 0.5..2.5 s from 1 s in: a 1 s head handle, and the file runs on.
    let built = build(
        std::slice::from_ref(&path),
        &[vec![Place {
            media: 0,
            start: 0.5,
            length: 2.0,
            source_start: 1.0,
        }]],
    );
    let clip = built.clips[0][0];
    // In from nothing: 0.25 s centred on the clip's start (half over the gap before it),
    // eased in. Out to nothing: 0.5 s starting at its end, an S-curve over the gap after.
    let mut fade_in = TransitionSpec::new(None, Some(clip), at(n(0.25)));
    fade_in.easing = "ease-in".to_owned();
    let mut fade_out = audio_transition::crossfade_spec(Some(clip), None, at(n(0.5)), S_CURVE);
    fade_out.alignment = Alignment::Start;
    let project = add(&add(&built.project, fade_in), fade_out);
    let out = mix(&project, built.sequence, n(3.5));
    let error = worst(&out, |k, c| {
        if k < n(0.375) || k >= n(3.0) {
            return 0.0;
        }
        let gain = fade(EQUAL_POWER, n(0.375), n(0.625), k, Easing::EaseIn).1
            * fade(S_CURVE, n(2.5), n(3.0), k, Easing::Linear).0;
        f64::from(tone.sample(c, (k - n(0.5) + n(1.0)) as usize)) * gain
    });
    assert!(error <= 1e-6, "worst error {error:e}");
    // Silent outside the fades, and the fades start and end on their samples.
    assert!(out[..n(0.375) as usize * 2].iter().all(|&s| s == 0.0));
    assert!(out[n(3.0) as usize * 2..].iter().all(|&s| s == 0.0));
    assert!(out[n(0.375) as usize * 2..n(0.375) as usize * 2 + 2] != [0.0, 0.0]);
    assert!(out[n(3.0) as usize * 2 - 2..n(3.0) as usize * 2] != [0.0, 0.0]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_speed_two_clip_plays_on_across_its_edges() {
    let dir = temp_dir("xfade-speed");
    let tone = Tone {
        rate: RATE,
        seconds: 6.0,
        channels: vec![(220.0, 0.5), (330.0, 0.5)],
    };
    let path = dir.join("tone.wav");
    tone.write(&path);
    let speed = |project: Project, items: &[ItemId]| {
        items.iter().fold(project, |p, &item| {
            p.set_clip_speed(item, Rational::new(2, 1), SpeedKeep::Timeline)
                .unwrap()
        })
    };
    // One clip over 0.5..2 s from 0.3 s plus a sample (half a sample of the 24 kHz reader
    // a 2x clip uses), and the same clip cut in two one sample after 1.25 s.
    let first = n(0.3) + 1;
    let whole = build(
        std::slice::from_ref(&path),
        &[vec![Place {
            media: 0,
            start: 0.5,
            length: 1.5,
            source_start: first as f64 / f64::from(RATE),
        }]],
    );
    let whole_project = speed(whole.project.clone(), &whole.clips[0]);
    let cut = n(1.25) + 1;
    let second = first + 2 * (cut - n(0.5));
    let split = build(
        std::slice::from_ref(&path),
        &[vec![
            Place {
                media: 0,
                start: 0.5,
                length: (cut - n(0.5)) as f64 / f64::from(RATE),
                source_start: first as f64 / f64::from(RATE),
            },
            Place {
                media: 0,
                start: cut as f64 / f64::from(RATE),
                length: (n(2.0) - cut) as f64 / f64::from(RATE),
                source_start: second as f64 / f64::from(RATE),
            },
        ]],
    );
    let split_project = speed(split.project.clone(), &split.clips[0]);
    let a = mix(&whole_project, whole.sequence, n(2.5));
    let b = mix(&split_project, split.sequence, n(2.5));
    let seam = worst(&b, |k, c| f64::from(a[k as usize * 2 + c]));
    assert!(seam <= 1e-6, "the cut changed the samples by {seam:e}");

    // The clip is the 24 kHz reader's own output from the source start rounded to its
    // grid (halves later), sample for sample, its first and last samples included: the
    // edges add nothing of their own.
    let reader_first = (first + 1) / 2;
    let mut reader = AudioReader::open(
        &path,
        AudioOptions {
            sample_rate: 24_000,
            ..AudioOptions::default()
        },
    )
    .unwrap();
    reader.seek_to_sample(reader_first as u64).unwrap();
    let len = (n(2.0) - n(0.5)) as usize;
    let mut planes = vec![vec![0.0f32; len]; 2];
    {
        let (l, r) = planes.split_at_mut(1);
        assert_eq!(
            reader.read(&mut [&mut l[0][..], &mut r[0][..]]).unwrap(),
            len
        );
    }
    let direct = worst(&a, |k, c| {
        let i = k - n(0.5);
        if (0..len as i64).contains(&i) {
            f64::from(planes[c][i as usize])
        } else {
            0.0
        }
    });
    assert!(direct <= 1e-6, "against the reader: worst error {direct:e}");

    // And the tone at twice the speed within the resampler's tolerance everywhere, at the
    // reader's own alignment (the nearest of a sample either way).
    let fit = |lag: i64| {
        worst(&a, |k, c| {
            if !(n(0.5)..n(2.0)).contains(&k) {
                return 0.0;
            }
            let (hz, amplitude) = tone.channels[c];
            let j = reader_first + (k - n(0.5)) + lag;
            amplitude * (2.0 * PI * hz * j as f64 / 24_000.0).sin()
        })
    };
    let (lag, error) = (-1..=1)
        .map(|lag| (lag, fit(lag)))
        .min_by(|x, y| x.1.total_cmp(&y.1))
        .unwrap();
    println!("speed 2: against the reader {direct:e}; against the tone {error:e} at lag {lag}");
    assert!(
        error <= 1e-3,
        "against the tone: worst error {error:e} at lag {lag}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn declick_ramps_only_hard_cuts_and_only_when_asked() {
    let dir = temp_dir("xfade-declick");
    let paths = files(
        &dir,
        &[
            vec![vec![0.5f32; 96_000]; 2],
            vec![vec![-0.5f32; 96_000]; 2],
        ],
    );
    // DC +0.5 over 0..0.5 s and on over 0.5..1 s from where it stopped (a seamless
    // continuation), then DC -0.5 over 1..1.5 s, then a gap.
    let built = build(
        &paths,
        &[vec![
            Place {
                media: 0,
                start: 0.0,
                length: 0.5,
                source_start: 0.0,
            },
            Place {
                media: 0,
                start: 0.5,
                length: 0.5,
                source_start: 0.5,
            },
            Place {
                media: 1,
                start: 1.0,
                length: 0.5,
                source_start: 0.0,
            },
        ]],
    );
    let range = TimeRange::new(Time::ZERO, at(n(2.0)));
    let off = mix_range(&built.project, built.sequence, range).unwrap();
    for k in 0..n(2.0) {
        let want = if k < n(1.0) {
            0.5
        } else if k < n(1.5) {
            -0.5
        } else {
            0.0
        };
        let got = f64::from(off[k as usize * 2]);
        assert!((got - want).abs() <= 1e-7, "declick off, frame {k}: {got}");
    }
    let settings = MixSettings {
        declick: true,
        ..MixSettings::default()
    };
    let on = mix_range_with(&built.project, built.sequence, range, &settings, stereo()).unwrap();
    let ramp = 48i64;
    let s_in = |k: i64, from: i64| {
        let p = ((k - from) as f64 + 0.5) / ramp as f64;
        (p * FRAC_PI_2).sin().powi(2)
    };
    let error = worst(&on, |k, _| {
        let level = if k < n(1.0) {
            0.5
        } else if k < n(1.5) {
            -0.5
        } else {
            return 0.0;
        };
        // Hard edges at 0 (from silence), 1 s (between the sources) and 1.5 s (into the
        // gap); none at 0.5 s, where the source continues.
        let edges = [0, n(1.0)];
        let mut gain = 1.0;
        for start in edges {
            if (start..start + ramp).contains(&k) {
                gain *= s_in(k, start);
            }
        }
        for end in [n(1.0), n(1.5)] {
            if (end - ramp..end).contains(&k) {
                gain *= 1.0 - s_in(k, end - ramp);
            }
        }
        level * gain
    });
    assert!(error <= 1e-6, "declick on: worst error {error:e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 48 kHz stereo sequence named `name` in `p`.
fn sequence(p: Project, name: &str) -> (Project, SequenceId) {
    p.add_sequence_with(
        name,
        SequenceFormat {
            resolution: Resolution::new(1920, 1080),
            frame_rate: Rational::new(30, 1),
            ..SequenceFormat::default()
        },
    )
    .unwrap()
}

#[test]
fn a_nested_sequence_plays_its_own_mix() {
    let dir = temp_dir("xfade-nested");
    let tones = [
        Tone {
            rate: RATE,
            seconds: 3.0,
            channels: vec![(440.0, 0.3), (550.0, 0.3)],
        },
        Tone {
            rate: RATE,
            seconds: 3.0,
            channels: vec![(330.0, 0.4)],
        },
    ];
    let mut paths: Vec<PathBuf> = tones
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let path = dir.join(format!("tone{i}.wav"));
            t.write(&path);
            path
        })
        .collect();
    paths.extend(files(&dir, &[noise(7, n(3.0) as usize, 2, 0.2)]));
    // The inner sequence: A1 the stereo tone over 0..1 s crossfading (linear, 0.2 s) into
    // the mono tone over 1..2 s from 0.5 s in; A2 noise at -6 dB over 0.3..1.7 s fading
    // in from nothing over its first 0.1 s.
    let built = build(
        &paths,
        &[
            vec![
                Place {
                    media: 0,
                    start: 0.0,
                    length: 1.0,
                    source_start: 0.0,
                },
                Place {
                    media: 1,
                    start: 1.0,
                    length: 1.0,
                    source_start: 0.5,
                },
            ],
            vec![Place {
                media: 2,
                start: 0.3,
                length: 1.4,
                source_start: 0.0,
            }],
        ],
    );
    let inner = built.sequence;
    let (a, b, noise_clip) = (built.clips[0][0], built.clips[0][1], built.clips[1][0]);
    let mut p = add(
        &built.project,
        audio_transition::crossfade_spec(Some(a), Some(b), at(n(0.2)), LINEAR),
    );
    let mut fade_in = TransitionSpec::new(None, Some(noise_clip), at(n(0.1)));
    fade_in.alignment = Alignment::Start;
    p = add(&p, fade_in);
    p = p.set_clip_gain(noise_clip, -6.0).unwrap();

    // The parent: the inner sequence over 0.5..2 s from its 0.25 s, at -3 dB, fading out
    // to nothing over 0.2 s after its end (inner time 1.75..1.95 s).
    let (next, parent) = sequence(p, "Parent");
    let (next, parent_track) = next
        .add_track(parent, TrackKind::Audio, "A1", None)
        .unwrap();
    let (next, nested) = next
        .insert_sequence_clip(
            parent_track,
            inner,
            TimeRange::new(at(n(0.5)), at(n(1.5))),
            at(n(0.25)),
        )
        .unwrap();
    let next = next.set_clip_gain(nested, -3.0).unwrap();
    let mut fade_out = TransitionSpec::new(Some(nested), None, at(n(0.2)));
    fade_out.alignment = Alignment::Start;
    let next = add(&next, fade_out);
    // And an outer sequence nesting the parent's first 2 s from 1 s on.
    let (next, outer) = sequence(next, "Outer");
    let (next, outer_track) = next.add_track(outer, TrackKind::Audio, "A1", None).unwrap();
    let (project, _) = next
        .insert_sequence_clip(
            outer_track,
            parent,
            TimeRange::new(at(n(1.0)), at(n(2.0))),
            Time::ZERO,
        )
        .unwrap();

    let inner_mix = mix(&project, inner, n(3.0));
    assert!(inner_mix.iter().any(|&s| s != 0.0));
    let parent_mix = mix(&project, parent, n(3.0));
    let error = worst(&parent_mix, |k, c| {
        if !(n(0.5)..n(2.2)).contains(&k) {
            return 0.0;
        }
        let gain = db(-3.0) * fade(EQUAL_POWER, n(2.0), n(2.2), k, Easing::Linear).0;
        let j = k - n(0.5) + n(0.25);
        f64::from(inner_mix[j as usize * 2 + c]) * gain
    });
    assert!(error <= 1e-6, "parent: worst error {error:e}");
    let outer_mix = mix(&project, outer, n(3.5));
    let error = worst(&outer_mix, |k, c| {
        let j = k - n(1.0);
        if !(0..n(2.0)).contains(&j) {
            return 0.0;
        }
        f64::from(parent_mix[j as usize * 2 + c])
    });
    assert!(error <= 1e-6, "outer: worst error {error:e}");

    // The nested sequence's own track switches apply: muting its noise track leaves the
    // tones, in the inner mix and through the nesting alike.
    let muted = project.set_track_muted(built.tracks[1], true).unwrap();
    let inner_muted = mix(&muted, inner, n(3.0));
    let error = worst(&inner_muted, |k, c| {
        f64::from(inner_mix[k as usize * 2 + c])
    });
    assert!(error > 0.05, "muting the noise changed nothing ({error})");
    let parent_muted = mix(&muted, parent, n(3.0));
    let error = worst(&parent_muted, |k, c| {
        if !(n(0.5)..n(2.2)).contains(&k) {
            return 0.0;
        }
        let gain = db(-3.0) * fade(EQUAL_POWER, n(2.0), n(2.2), k, Easing::Linear).0;
        let j = k - n(0.5) + n(0.25);
        f64::from(inner_muted[j as usize * 2 + c]) * gain
    });
    assert!(
        error <= 1e-6,
        "parent with a muted inner track: worst error {error:e}"
    );

    let offline = OfflineMix::new(&project, outer, &MixSettings::default(), stereo(), 0).unwrap();
    let stats = offline.stats();
    assert_eq!(stats.nested_clips, 2, "{stats:?}");
    assert_eq!(stats.silent(), 0, "{stats:?}");
    assert_eq!(stats.clips, 5, "{stats:?}");
    assert!(offline.plays_between(n(1.5), n(1.6)));
    assert!(!offline.plays_between(0, n(1.0)));
    let _ = std::fs::remove_dir_all(&dir);
}
