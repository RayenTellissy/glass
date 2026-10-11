//! The mixer graph against sums worked out independently: fader gain, the pan law, mute
//! and solo, sends and bus routing, automation, meters and the clip guard; and the real-time
//! shell's graph swaps, pausing and underruns.

use std::f64::consts::{FRAC_PI_4, PI, SQRT_2};

use audio::mixer::{
    CONTROL_FRAMES, Feed, Graph, GraphError, GraphSpec, Lane, Layout, MasterSpec, Mixer,
    MixerConfig, Param, Point, Route, SendSpec, StripRef, StripSpec, SwapAt, meter,
};

const RATE: u32 = 48_000;

fn spec(tracks: Vec<StripSpec>, buses: Vec<StripSpec>) -> GraphSpec {
    GraphSpec {
        sample_rate: RATE,
        layout: Layout::Stereo,
        max_block: 256,
        tracks,
        buses,
        master: MasterSpec::default(),
    }
}

/// Track `t`'s sample on channel `c` at frame `k`: a different tone per track and channel.
fn tone(t: usize, c: usize, k: usize) -> f32 {
    let hz = 220.0 * (t + 1) as f64 + 37.0 * c as f64;
    (0.3 * (2.0 * PI * hz * k as f64 / f64::from(RATE) + c as f64).sin()) as f32
}

/// Interleaved feed frames for `tracks` stereo tracks.
fn feed(tracks: usize, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * tracks * 2);
    for k in 0..frames {
        for t in 0..tracks {
            for c in 0..2 {
                out.push(tone(t, c, k));
            }
        }
    }
    out
}

fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// The pan law worked out from its definition.
fn law(pan: f64) -> [f64; 2] {
    let theta = (pan + 1.0) * FRAC_PI_4;
    [SQRT_2 * theta.cos(), SQRT_2 * theta.sin()]
}

fn mix(spec: &GraphSpec, frames: usize, position: i64) -> Vec<f32> {
    let mut graph = Graph::build(spec).unwrap();
    let input = feed(spec.tracks.len(), frames);
    let mut out = vec![0.0; frames * 2];
    graph.mix(position, &input, &mut out);
    out
}

fn assert_close(out: &[f32], expected: impl Fn(usize, usize) -> f64, tolerance: f64, what: &str) {
    let mut worst = 0.0f64;
    for (i, &s) in out.iter().enumerate() {
        let (k, c) = (i / 2, i % 2);
        worst = worst.max((f64::from(s) - expected(k, c)).abs());
    }
    assert!(worst <= tolerance, "{what}: worst error {worst:e}");
}

#[test]
fn faders_and_the_pan_law_sum_exactly() {
    let pans = [0.0, -0.5, 1.0, 0.3];
    let faders = [-6.0, 0.0, 3.0, -12.5];
    let tracks = (0..4)
        .map(|t| StripSpec {
            fader_db: faders[t],
            pan: pans[t],
            ..StripSpec::new(t as u64)
        })
        .collect();
    let spec = spec(tracks, Vec::new());
    let out = mix(&spec, 4000, 0);
    assert_close(
        &out,
        |k, c| {
            (0..4)
                .map(|t| f64::from(tone(t, c, k)) * db(faders[t]) * law(pans[t])[c])
                .sum()
        },
        1e-6,
        "faders and pan",
    );
    // Centre is unity and the law keeps constant power.
    assert_eq!(law(0.0).map(|g| (g * 1e9).round() / 1e9), [1.0, 1.0]);
    for pan in [-1.0, -0.4, 0.0, 0.7, 1.0] {
        let (l, r) = audio::mixer::pan_law(pan);
        assert!((f64::from(l * l + r * r) - 2.0).abs() < 1e-6);
    }
}

#[test]
fn mute_and_solo_silence_the_right_tracks() {
    let strip = |id: u64, mute: bool, solo: bool| StripSpec {
        mute,
        solo,
        ..StripSpec::new(id)
    };
    // No solo: the muted track is silent.
    let out = mix(
        &spec(
            vec![strip(0, false, false), strip(1, true, false)],
            Vec::new(),
        ),
        2000,
        0,
    );
    assert_close(&out, |k, c| f64::from(tone(0, c, k)), 1e-6, "mute");
    // A solo silences the others; a muted solo stays silent.
    let out = mix(
        &spec(
            vec![
                strip(0, false, false),
                strip(1, false, true),
                strip(2, true, true),
            ],
            Vec::new(),
        ),
        2000,
        0,
    );
    assert_close(&out, |k, c| f64::from(tone(1, c, k)), 1e-6, "solo");
}

#[test]
fn sends_and_buses_route_as_specified() {
    // Track 0: to the master at -3 dB, panned left, with a post-fader send to bus 0 at -6.
    // Track 1: out to bus 1 at -20 dB, with a pre-fader send to bus 0 at 0 dB.
    // Bus 0: -3 dB, out to bus 1. Bus 1: panned 0.5, to the master. Master at -1 dB.
    let tracks = vec![
        StripSpec {
            fader_db: -3.0,
            pan: -0.6,
            sends: vec![SendSpec {
                bus: 0,
                level_db: -6.0,
                pre_fader: false,
            }],
            ..StripSpec::new(10)
        },
        StripSpec {
            fader_db: -20.0,
            output: Route::Bus(1),
            sends: vec![SendSpec {
                bus: 0,
                level_db: 0.0,
                pre_fader: true,
            }],
            ..StripSpec::new(11)
        },
    ];
    let buses = vec![
        StripSpec {
            fader_db: -3.0,
            output: Route::Bus(1),
            ..StripSpec::new(100)
        },
        StripSpec {
            pan: 0.5,
            ..StripSpec::new(101)
        },
    ];
    let mut spec = spec(tracks, buses);
    spec.master.fader_db = -1.0;
    let out = mix(&spec, 3000, 0);
    assert_close(
        &out,
        |k, c| {
            let x0 = f64::from(tone(0, c, k));
            let x1 = f64::from(tone(1, c, k));
            let t0 = x0 * db(-3.0) * law(-0.6)[c];
            let t1 = x1 * db(-20.0);
            let bus0 = (t0 * db(-6.0) + x1) * db(-3.0);
            let bus1 = (bus0 + t1) * law(0.5)[c];
            (t0 + bus1) * db(-1.0)
        },
        1e-6,
        "sends and buses",
    );
}

#[test]
fn routing_loops_and_unknown_buses_are_refused() {
    let to = |output| StripSpec {
        output,
        ..StripSpec::new(1)
    };
    let looped = spec(Vec::new(), vec![to(Route::Bus(0))]);
    assert_eq!(
        Graph::build(&looped).err(),
        Some(GraphError::BusOrder { bus: 0, to: 0 })
    );
    let unknown = spec(vec![to(Route::Bus(3))], Vec::new());
    assert_eq!(
        Graph::build(&unknown).err(),
        Some(GraphError::UnknownBus { strip: 1, bus: 3 })
    );
}

#[test]
fn the_result_does_not_depend_on_block_size_or_position() {
    let lane = Lane::new(vec![
        Point::linear(0, -30.0),
        Point::linear(3000, 0.0),
        Point::hold(5000, -6.0),
        Point::linear(6100, 2.0),
    ])
    .unwrap();
    let tracks = vec![
        StripSpec {
            fader_lane: Some(lane),
            pan: 0.2,
            ..StripSpec::new(0)
        },
        StripSpec::new(1),
    ];
    let spec = spec(tracks, Vec::new());
    let frames = 8000;
    let input = feed(2, frames);
    let reference = mix(&spec, frames, 0);
    for block in [1, 31, 32, 37, 256] {
        let mut graph = Graph::build(&spec).unwrap();
        let mut out = vec![0.0; frames * 2];
        let mut done = 0;
        while done < frames {
            let n = block.min(frames - done);
            graph
                .input_mut(n)
                .copy_from_slice(&input[done * 4..(done + n) * 4]);
            graph.process(done as i64, n, &mut out[done * 2..(done + n) * 2]);
            done += n;
        }
        assert_close(
            &out,
            |k, c| f64::from(reference[k * 2 + c]),
            1e-6,
            &format!("blocks of {block}"),
        );
    }
    // Starting mid-timeline gives the same samples as getting there from zero.
    let start = 1234;
    let mut graph = Graph::build(&spec).unwrap();
    let mut out = vec![0.0; (frames - start) * 2];
    graph.mix(start as i64, &input[start * 4..], &mut out);
    assert_close(
        &out,
        |k, c| f64::from(reference[(start + k) * 2 + c]),
        1e-6,
        "started at 1234",
    );
}

#[test]
fn automation_ramps_are_sample_accurate_within_the_control_ramp() {
    // A fader ramp from -40 dB to 0 dB over a second on a DC feed of 0.5, then a hold
    // step of the pan, and a master fader ramp.
    let fader = Lane::new(vec![Point::linear(0, -40.0), Point::linear(48_000, 0.0)]).unwrap();
    let pan = Lane::new(vec![Point::hold(0, 0.0), Point::hold(30_016, -1.0)]).unwrap();
    let master = Lane::new(vec![
        Point::linear(10_016, 0.0),
        Point::linear(40_000, -12.0),
    ])
    .unwrap();
    let mut spec = spec(
        vec![StripSpec {
            fader_lane: Some(fader.clone()),
            pan_lane: Some(pan.clone()),
            ..StripSpec::new(0)
        }],
        Vec::new(),
    );
    spec.master.fader_lane = Some(master.clone());
    let frames = 50_000;
    let mut graph = Graph::build(&spec).unwrap();
    let input = vec![0.5f32; frames * 2];
    let mut out = vec![0.0; frames * 2];
    graph.mix(0, &input, &mut out);

    let exact = |k: usize, c: usize| {
        let f = k as i64;
        0.5 * db(fader.value_at(f)) * law(pan.value_at(f))[c] * db(master.value_at(f))
    };
    let mut worst = 0.0f64;
    // A control point (938 x 32), so the hold step completes on its frame.
    let step = 30_016;
    for k in 0..frames {
        // The hold step ramps over the control segment that ends at it.
        if (step - CONTROL_FRAMES..step).contains(&k) {
            continue;
        }
        for c in 0..2 {
            worst = worst.max((f64::from(out[k * 2 + c]) - exact(k, c)).abs());
        }
    }
    // Linear ramps of the gain between control points 32 frames apart: the chord of
    // 10^(dB/20) over 32 frames is within about 1e-6 of the curve here. (Every lane point
    // sits on a control point; a kink between two is rounded off within that segment.)
    assert!(worst < 2e-6, "worst error {worst:e}");
    // The step itself lands by its frame.
    assert!((f64::from(out[step * 2 + 1]) - exact(step, 1)).abs() < 1e-6);
}

#[test]
fn meters_read_known_signals() {
    let period = meter::publish_period(RATE);
    assert_eq!(period, 800);
    // 600 Hz at 48 kHz: 80 samples a cycle, ten cycles a meter period; amplitude 0.5,
    // then 0.1 from one second on, until past the hold time.
    let frames = 5 * RATE as usize / 2 + 4 * period;
    let mut input = Vec::with_capacity(frames * 2);
    for k in 0..frames {
        let amplitude = if k < RATE as usize { 0.5 } else { 0.1 };
        let s = (amplitude * (2.0 * PI * 600.0 * k as f64 / f64::from(RATE)).sin()) as f32;
        input.extend([s, s]);
    }
    let spec = spec(vec![StripSpec::new(7)], Vec::new());
    let mut graph = Graph::build(&spec).unwrap();
    let bank = graph.meters();
    let mut out = vec![0.0; frames * 2];
    // Up to just under one second: loud periods only.
    let loud = RATE as usize;
    graph.mix(0, &input[..loud * 2], &mut out[..loud * 2]);
    assert_eq!(bank.publications(), (loud / period) as u64);
    let track = bank.track(7).unwrap();
    for c in 0..2 {
        assert!((track.peak[c] - 0.5).abs() < 1e-6, "{track:?}");
        assert!(
            (f64::from(track.rms[c]) - 0.5 / SQRT_2).abs() < 1e-6,
            "{track:?}"
        );
        assert!((track.hold[c] - 0.5).abs() < 1e-6);
    }
    assert_eq!(bank.master(), track);

    // Quiet for less than the hold time: the hold stays at the loud peak.
    let mid = loud + RATE as usize / 2;
    graph.mix(
        loud as i64,
        &input[loud * 2..mid * 2],
        &mut out[loud * 2..mid * 2],
    );
    let track = bank.track(7).unwrap();
    assert!((track.peak[0] - 0.1).abs() < 1e-6);
    assert!((f64::from(track.rms[0]) - 0.1 / SQRT_2).abs() < 1e-6);
    assert!((track.hold[0] - 0.5).abs() < 1e-6);
    // After 1.5 s the hold falls to the current peak.
    graph.mix(mid as i64, &input[mid * 2..], &mut out[mid * 2..]);
    let track = bank.track(7).unwrap();
    assert!((track.hold[0] - 0.1).abs() < 1e-6, "{track:?}");
    assert_eq!(bank.clip().overs, 0);

    // Paused: the meters fall to zero.
    graph.idle(period);
    assert_eq!(bank.track(7).unwrap().peak[0], 0.0);
}

#[test]
fn the_clip_guard_reports_overs_and_soft_clips_only_when_asked() {
    let hot = StripSpec {
        fader_db: 6.0,
        ..StripSpec::new(0)
    };
    let frames = 4800;
    let input = vec![0.8f32; frames * 2];
    for soft_clip in [false, true] {
        let mut spec = spec(vec![hot.clone()], Vec::new());
        spec.master.soft_clip = soft_clip;
        let mut graph = Graph::build(&spec).unwrap();
        let bank = graph.meters();
        let mut out = vec![0.0; frames * 2];
        graph.mix(0, &input, &mut out);
        let expected = 0.8 * db(6.0);
        let clip = bank.clip();
        assert_eq!(clip.overs, (frames * 2) as u64);
        assert!((f64::from(clip.max_peak) - expected).abs() < 1e-5);
        let max = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if soft_clip {
            assert!(max < 1.0 && max > 0.95, "soft clipped to {max}");
        } else {
            assert!((f64::from(max) - expected).abs() < 1e-5, "unclipped {max}");
        }
        bank.reset_clip();
        assert_eq!(bank.clip().overs, 0);
    }
    // Below the knee the soft clipper changes nothing.
    assert_eq!(audio::mixer::soft_clip(0.5), 0.5);
    assert_eq!(audio::mixer::soft_clip(-0.89), -0.89);
}

/// A one-track graph and a feed holding `value` for `frames` frames from `start`.
fn constant_graph(value: f32, start: i64, frames: usize) -> (Graph, audio::mixer::FeedConsumer) {
    let graph = Graph::build(&spec(vec![StripSpec::new(0)], Vec::new())).unwrap();
    let (mut feed, consumer) = Feed::new(graph.width(), frames, start);
    assert_eq!(feed.push(&vec![value; frames * 2]), frames);
    (graph, consumer)
}

#[test]
fn the_mixer_swaps_graphs_where_asked_and_counts_underruns() {
    let (mut mixer, mut control) = Mixer::new(MixerConfig::default());
    let mut out = vec![0.0f32; 2 * 100];
    // No graph yet: silence.
    assert_eq!(mixer.render(&mut out), 0);
    assert!(out.iter().all(|&s| s == 0.0));

    let (a, feed_a) = constant_graph(0.25, 0, 1000);
    let first = control.swap(a, feed_a, SwapAt::Now).unwrap();
    // Paused: silence, and the position stays.
    mixer.render(&mut out);
    assert!(out.iter().all(|&s| s == 0.0));
    assert_eq!(mixer.status().position(), 0);
    assert_eq!(mixer.status().generation(), first);

    control.play().unwrap();
    mixer.render(&mut out);
    assert!(out.iter().all(|&s| s == 0.25));
    assert_eq!(mixer.status().position(), 100);

    // An edit takes over at frame 250, inside the next callback.
    let (b, feed_b) = constant_graph(0.5, 250, 1000);
    let second = control.swap(b, feed_b, SwapAt::Continue).unwrap();
    let mut out = vec![0.0f32; 2 * 300];
    mixer.render(&mut out);
    for (k, frame) in out.chunks(2).enumerate() {
        let expected = if 100 + k < 250 { 0.25 } else { 0.5 };
        assert_eq!(frame, [expected, expected], "frame {}", 100 + k);
    }
    assert_eq!(mixer.status().position(), 400);
    assert_eq!(mixer.status().generation(), second);
    // The first graph came back to be dropped here.
    assert_eq!(control.collect(), 1);

    // A fader change ramps in by the next control point.
    control.set(StripRef::Track(0), Param::Fader(-6.0)).unwrap();
    let mut out = vec![0.0f32; 2 * 64];
    mixer.render(&mut out);
    let last = out[out.len() - 1];
    assert!((f64::from(last) - 0.5 * db(-6.0)).abs() < 1e-6, "{last}");

    // The feed runs dry at frame 1250: silence, counted, and the position moves on.
    let before = mixer.status().underruns();
    let mut out = vec![0.0f32; 2 * 1000];
    let silent = mixer.render(&mut out);
    let position = 400 + 64 + 1000;
    assert_eq!(silent, position - 1250);
    assert_eq!(mixer.status().underruns(), before + 1);
    assert_eq!(mixer.status().position(), position as i64);

    // A seek: the new feed plays at once from its start.
    let (c, feed_c) = constant_graph(0.125, 10_000, 500);
    control.swap(c, feed_c, SwapAt::Now).unwrap();
    let mut out = vec![0.0f32; 2 * 100];
    assert_eq!(mixer.render(&mut out), 0);
    assert!(out.iter().all(|&s| s == 0.125));
    assert_eq!(mixer.status().position(), 10_100);
    assert_eq!(control.collect(), 1);
    assert_eq!(mixer.status().leaked(), 0);
}

#[test]
fn a_late_edit_drops_the_frames_already_played() {
    let (mut mixer, mut control) = Mixer::new(MixerConfig::default());
    let (a, feed_a) = constant_graph(0.25, 0, 1000);
    control.swap(a, feed_a, SwapAt::Now).unwrap();
    control.play().unwrap();
    let mut out = vec![0.0f32; 2 * 300];
    mixer.render(&mut out);
    // The new feed starts at 200, but the live graph is already at 300: its first 100
    // frames are dropped so it stays in step with the timeline.
    let graph = Graph::build(&spec(vec![StripSpec::new(0)], Vec::new())).unwrap();
    let (mut feed, consumer) = Feed::new(graph.width(), 1000, 200);
    let ramp: Vec<f32> = (0..400).flat_map(|k| [k as f32, k as f32]).collect();
    feed.push(&ramp);
    control.swap(graph, consumer, SwapAt::Continue).unwrap();
    let mut out = vec![0.0f32; 2 * 10];
    assert_eq!(mixer.render(&mut out), 0);
    assert_eq!(out[0], 100.0);
    assert_eq!(out[18], 109.0);
}
