//! The offline mix of a sequence (`engine::mix`) against sums worked out from the tone
//! files' own samples: clip gain, fader, pan, the project's track mute and solo, sends
//! and a bus, automation from project curves, and the default `mix_range`.

mod mix_common;

use std::f64::consts::{FRAC_PI_4, SQRT_2};

use engine::audio::mixer::{Layout, Route, SendSpec};
use engine::mix::{
    BusMix, MixFormat, MixSettings, OfflineMix, TrackMix, lane_from_curve, mix_range,
    mix_range_with,
};
use engine::project::params::ParamValue;
use engine::project::{ClipParam, Curve, Interpolation, Key, KeyTarget, Time, TimeRange};
use mix_common::{Place, Tone, build, samples, temp_dir};

const RATE: f64 = 48_000.0;

fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

fn law(pan: f64) -> [f64; 2] {
    let theta = (pan + 1.0) * FRAC_PI_4;
    [SQRT_2 * theta.cos(), SQRT_2 * theta.sin()]
}

fn tones() -> Vec<Tone> {
    vec![
        Tone {
            rate: 48_000,
            seconds: 2.0,
            channels: vec![(440.0, 0.3), (660.0, 0.2)],
        },
        Tone {
            rate: 48_000,
            seconds: 2.0,
            channels: vec![(550.0, 0.25)],
        },
        Tone {
            rate: 48_000,
            seconds: 2.0,
            channels: vec![(1000.0, 0.1), (1000.0, 0.1)],
        },
    ]
}

/// The tone's sample at timeline frame `k` of a clip starting at `start` seconds from
/// `source` seconds, as the mix layout sees it (mono copied to both channels), or 0
/// outside the clip.
fn clip_sample(tone: &Tone, k: usize, c: usize, start: f64, length: f64, source: f64) -> f64 {
    let (start, end) = ((start * RATE) as usize, ((start + length) * RATE) as usize);
    if k < start || k >= end {
        return 0.0;
    }
    let at = k - start + (source * RATE) as usize;
    let channel = if tone.channels.len() == 1 { 0 } else { c };
    f64::from(tone.sample(channel, at))
}

fn worst(out: &[f32], expected: impl Fn(usize, usize) -> f64) -> f64 {
    out.iter()
        .enumerate()
        .map(|(i, &s)| (f64::from(s) - expected(i / 2, i % 2)).abs())
        .fold(0.0, f64::max)
}

#[test]
fn the_mix_of_tones_is_their_exact_sum() {
    let dir = temp_dir("sum");
    let tones = tones();
    let files: Vec<_> = tones
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let path = dir.join(format!("tone{i}.wav"));
            t.write(&path);
            path
        })
        .collect();
    let built = build(
        &files,
        &[
            vec![Place {
                media: 0,
                start: 0.0,
                length: 1.0,
                source_start: 0.0,
            }],
            vec![Place {
                media: 1,
                start: 0.25,
                length: 1.0,
                source_start: 0.5,
            }],
            vec![Place {
                media: 2,
                start: 0.0,
                length: 1.25,
                source_start: 0.0,
            }],
        ],
    );
    let clip0 = built.clips[0][0];
    let project = built.project.set_clip_gain(clip0, -6.0).unwrap();
    let [a1, a2, a3] = built.tracks[..] else {
        panic!("three tracks")
    };

    // A1: -3 dB, panned left, post-fader send to the bus at -6. A2: to the bus, panned
    // right. A3: muted in the project. The bus at -2 dB to the master, at -0.5 dB.
    let project = project.set_track_muted(a3, true).unwrap();
    let mut settings = MixSettings::default();
    settings.tracks.insert(
        a1,
        TrackMix {
            fader_db: -3.0,
            pan: -0.5,
            sends: vec![SendSpec {
                bus: 0,
                level_db: -6.0,
                pre_fader: false,
            }],
            ..TrackMix::default()
        },
    );
    settings.tracks.insert(
        a2,
        TrackMix {
            pan: 0.5,
            output: Route::Bus(0),
            ..TrackMix::default()
        },
    );
    settings.buses.push(BusMix {
        fader_db: -2.0,
        ..BusMix::new(1)
    });
    settings.master.fader_db = -0.5;
    let format = MixFormat {
        sample_rate: 48_000,
        layout: Layout::Stereo,
    };
    let range = TimeRange::new(Time::ZERO, samples(1.5));
    let out = mix_range_with(&project, built.sequence, range, &settings, format).unwrap();
    assert_eq!(out.len(), 72_000 * 2);
    let error = worst(&out, |k, c| {
        let x0 = clip_sample(&tones[0], k, c, 0.0, 1.0, 0.0) * db(-6.0);
        let x1 = clip_sample(&tones[1], k, c, 0.25, 1.0, 0.5);
        let t0 = x0 * db(-3.0) * law(-0.5)[c];
        let t1 = x1 * law(0.5)[c];
        let bus = (t0 * db(-6.0) + t1) * db(-2.0);
        (t0 + bus) * db(-0.5)
    });
    assert!(error <= 1e-6, "worst error {error:e}");

    // Solo A3 (unmuted): only it plays.
    let soloed = project.set_track_muted(a3, false).unwrap();
    let soloed = soloed.set_track_solo(a3, true).unwrap();
    let out = mix_range_with(&soloed, built.sequence, range, &settings, format).unwrap();
    let error = worst(&out, |k, c| {
        clip_sample(&tones[2], k, c, 0.0, 1.25, 0.0) * db(-0.5)
    });
    assert!(error <= 1e-6, "solo: worst error {error:e}");

    // The defaults: every track at unity to the master, the clip gain still applies.
    let project = project.set_track_muted(a3, false).unwrap();
    let out = mix_range(&project, built.sequence, range).unwrap();
    let error = worst(&out, |k, c| {
        clip_sample(&tones[0], k, c, 0.0, 1.0, 0.0) * db(-6.0)
            + clip_sample(&tones[1], k, c, 0.25, 1.0, 0.5)
            + clip_sample(&tones[2], k, c, 0.0, 1.25, 0.0)
    });
    assert!(error <= 1e-6, "defaults: worst error {error:e}");

    // Rendering in odd pieces from a later start gives the same samples.
    let mut mix = OfflineMix::new(&project, built.sequence, &settings, format, 30_000).unwrap();
    let mut pieces = vec![0.0f32; 20_000 * 2];
    for chunk in [1usize, 999, 1600, 17_400] {
        let at = (mix.position() - 30_000) as usize * 2;
        mix.render(&mut pieces[at..at + chunk * 2]);
    }
    let whole = mix_range_with(
        &project,
        built.sequence,
        TimeRange::new(samples(0.625), samples(20_000.0 / RATE)),
        &settings,
        format,
    )
    .unwrap();
    assert_eq!(whole.len(), pieces.len());
    let error = worst(&pieces, |k, c| f64::from(whole[k * 2 + c]));
    assert!(error <= 1e-7, "pieces: worst error {error:e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Mute and solo are project track switches (schema v9): the default `mix_range` hears
/// them with no mixer settings at all, and undoing them brings the track back.
#[test]
fn project_mute_and_solo_change_the_default_mix() {
    let dir = temp_dir("switches");
    let tones = tones();
    let files: Vec<_> = tones[..2]
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let path = dir.join(format!("tone{i}.wav"));
            t.write(&path);
            path
        })
        .collect();
    let place = |media| {
        vec![Place {
            media,
            start: 0.0,
            length: 1.0,
            source_start: 0.0,
        }]
    };
    let built = build(&files, &[place(0), place(1)]);
    let a1 = built.tracks[0];
    let range = TimeRange::new(Time::ZERO, samples(1.0));
    let x0 = |k, c| clip_sample(&tones[0], k, c, 0.0, 1.0, 0.0);
    let x1 = |k, c| clip_sample(&tones[1], k, c, 0.0, 1.0, 0.0);

    let both = mix_range(&built.project, built.sequence, range).unwrap();
    let error = worst(&both, |k, c| x0(k, c) + x1(k, c));
    assert!(error <= 1e-6, "both: worst error {error:e}");

    let muted = built.project.set_track_muted(a1, true).unwrap();
    let out = mix_range(&muted, built.sequence, range).unwrap();
    let error = worst(&out, x1);
    assert!(error <= 1e-6, "A1 muted: worst error {error:e}");

    let soloed = built.project.set_track_solo(a1, true).unwrap();
    let out = mix_range(&soloed, built.sequence, range).unwrap();
    let error = worst(&out, x0);
    assert!(error <= 1e-6, "A1 soloed: worst error {error:e}");

    // A muted solo track is silent, and so is every other track.
    let out = mix_range(
        &soloed.set_track_muted(a1, true).unwrap(),
        built.sequence,
        range,
    )
    .unwrap();
    assert!(out.iter().all(|&s| s == 0.0), "muted solo");

    let back = soloed.set_track_solo(a1, false).unwrap();
    assert_eq!(mix_range(&back, built.sequence, range).unwrap(), both);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clip_gain_and_track_fader_automation_follow_their_curves() {
    let dir = temp_dir("automation");
    let tone = Tone {
        rate: 48_000,
        seconds: 2.0,
        channels: vec![(300.0, 0.4), (300.0, 0.4)],
    };
    let path = dir.join("tone.wav");
    tone.write(&path);
    let built = build(
        std::slice::from_ref(&path),
        &[vec![Place {
            media: 0,
            start: 0.5,
            length: 1.0,
            source_start: 0.25,
        }]],
    );
    let clip = built.clips[0][0];
    // Clip gain: -12 dB at source 0.25 s rising linearly to 0 dB at source 1.25 s (keys
    // in source time).
    let target = || KeyTarget::Clip {
        item: clip,
        param: ClipParam::GainDb,
    };
    let project = built.project.set_clip_gain(clip, -12.0).unwrap();
    let project = project.animate_param(target(), samples(0.25)).unwrap();
    let project = project
        .set_key(target(), samples(1.25), ParamValue::Float(0.0))
        .unwrap();
    // Track fader: an eased (bezier) fall from 0 to -20 dB over 0.5..1.5 s of sequence
    // time, flattened by `lane_from_curve`.
    let fader = Curve {
        keys: vec![
            Key {
                interpolation: Interpolation::ease(1),
                ..Key::new(samples(0.5), ParamValue::Float(0.0))
            },
            Key::new(samples(1.5), ParamValue::Float(-20.0)),
        ],
    };
    let mut settings = MixSettings::default();
    settings.track_mut(built.tracks[0]).fader_curve = Some(fader.clone());
    let format = MixFormat {
        sample_rate: 48_000,
        layout: Layout::Stereo,
    };
    let range = TimeRange::new(Time::ZERO, samples(2.0));
    let out = mix_range_with(&project, built.sequence, range, &settings, format).unwrap();

    let error = worst(&out, |k, c| {
        let t = k as f64 / RATE;
        let x = clip_sample(&tone, k, c, 0.5, 1.0, 0.25);
        let clip_db = -12.0 + 12.0 * (t - 0.5).clamp(0.0, 1.0);
        let at = Time::from_samples(k as i64, 48_000).unwrap();
        let fader_db = fader.value_at(at).and_then(|v| v.as_f64()).unwrap();
        x * db(clip_db) * db(fader_db)
    });
    // Gains ramp linearly between control points 32 frames apart, and the bezier is
    // flattened every 5 ms: both stay well within 1e-4 of the curve here.
    assert!(error <= 1e-4, "worst error {error:e}");

    // Linear and hold keys map onto lane points one to one.
    let curve = Curve {
        keys: vec![
            Key::new(samples(0.0), ParamValue::Float(-6.0)),
            Key {
                interpolation: Interpolation::Hold,
                ..Key::new(samples(1.0), ParamValue::Float(0.0))
            },
            Key::new(samples(2.0), ParamValue::Float(-3.0)),
        ],
    };
    let lane = lane_from_curve(&curve, 48_000).unwrap();
    assert_eq!(lane.points().len(), 3);
    assert_eq!(lane.value_at(24_000), -3.0);
    assert_eq!(lane.value_at(72_000), 0.0);
    assert_eq!(lane.value_at(96_000), -3.0);
    let _ = std::fs::remove_dir_all(&dir);
}
