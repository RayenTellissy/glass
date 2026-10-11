//! Timeline playback end to end: a sequence built in code from the generated fixtures
//! (two video tracks, four clips, a cross dissolve, a transformed and faded clip and a
//! clip at 2x) plays 3 s at 1080p30 headless through the look-ahead, the timeline
//! renderer and the frame cache, paced by the master clock: no frame is late, the frames
//! equal single-frame renders of the same timeline time from exactly decoded layers, and
//! the dissolve's midpoint equals the CPU mix of its two sides. The scrubber's exact
//! frames go through the same renderer and cache index. GPU checks skip without an
//! adapter, media checks without the fixtures.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio::Clock;
use engine::lookahead::{GopIndexes, Layer, SourceRef};
use engine::quality::QualityLevel;
use engine::scrub::{
    Decoded, ManualClock, ScrubConfig, ScrubDecoder, Scrubber, SoftwareDecoderOptions,
    SoftwareScrubDecoder,
};
use engine::timeline::{
    Presented, PresentedFrame, RendererConfig, TimelineConfig, TimelinePlan, TimelinePlayback,
    TimelineRenderer, TrackFrame,
};
use media::{DecoderOptions, HwPreference, VideoFrame};
use project::{
    ClipSpec, Project, Rational, SequenceId, SpeedKeep, Time, TimeRange, TrackKind, Transform,
    TransitionSpec,
};
use render::frame_cache::{FrameCacheConfig, SpillFormat};
use render::wgpu;

const WAIT: Duration = Duration::from_secs(20);
const FPS: i64 = 30;

fn at(frames: i64) -> Time {
    Time::from_frames(frames, Rational::new(FPS as i32, 1)).expect("time")
}

fn range(start: i64, end: i64) -> TimeRange {
    TimeRange::between(at(start), at(end)).expect("range")
}

/// Where the clips sit, in frames: A and B (start, end, source start) on V1 with a
/// centred dissolve of `dissolve` frames on their cut, C and D on V2.
struct Layout {
    length: i64,
    a: (i64, i64),
    b: (i64, i64, i64),
    c: (i64, i64),
    d: (i64, i64),
    dissolve: i64,
}

const LAYOUT_3S: Layout = Layout {
    length: 90,
    a: (0, 50),
    // Start, end, source start (frames): one second in, 5 frames of head handle.
    b: (50, 90, 30),
    c: (10, 40),
    d: (60, 90),
    dissolve: 10,
};

/// For the 2 s 4K fixture: the source must last as long as B and D need.
const LAYOUT_2S: Layout = Layout {
    length: 60,
    a: (0, 30),
    b: (30, 60, 15),
    c: (6, 24),
    d: (36, 60),
    dissolve: 10,
};

/// V1: A then B with a centred cross dissolve; V2 above: C scaled to half, turned,
/// moved up left and at 60% opacity, and D scaled to half, moved down right, at 2x.
fn timeline(
    main: &Path,
    other: &Path,
    canvas: (u32, u32),
    layout: &Layout,
) -> (Project, SequenceId) {
    let p = Project::new("timeline-playback", "timeline playback");
    let (p, m1) = p.add_media("main", main.to_string_lossy()).expect("media");
    let (p, m2) = p
        .add_media("other", other.to_string_lossy())
        .expect("media");
    let (p, seq) = p
        .add_sequence("main", Rational::new(FPS as i32, 1), 48_000)
        .expect("sequence");
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).expect("V2");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("V1");
    let clip = |media, (start, end): (i64, i64), source: i64| ClipSpec {
        media,
        range: range(start, end),
        source_start: at(source),
    };
    let (p, a) = p.insert_clip(v1, clip(m1, layout.a, 0)).expect("A");
    let (p, b) = p
        .insert_clip(v1, clip(m1, (layout.b.0, layout.b.1), layout.b.2))
        .expect("B");
    let (p, c) = p.insert_clip(v2, clip(m2, layout.c, 0)).expect("C");
    let (p, d) = p.insert_clip(v2, clip(m1, layout.d, 0)).expect("D");
    let (p, _) = p
        .add_transition(TransitionSpec::new(Some(a), Some(b), at(layout.dissolve)))
        .expect("dissolve");
    let (w, h) = (f64::from(canvas.0), f64::from(canvas.1));
    let p = p
        .set_clip_transform(
            c,
            Transform {
                position: [-0.2 * w, -0.18 * h],
                scale: [0.5, 0.5],
                rotation: 10.0,
                anchor: [0.0, 0.0],
            },
        )
        .expect("C transform");
    let p = p.set_clip_opacity(c, 0.6).expect("C opacity");
    let p = p
        .set_clip_transform(
            d,
            Transform {
                position: [0.2 * w, 0.18 * h],
                scale: [0.5, 0.5],
                ..Transform::IDENTITY
            },
        )
        .expect("D transform");
    let p = p
        .set_clip_speed(d, Rational::new(2, 1), SpeedKeep::Timeline)
        .expect("D speed");
    (p, seq)
}

fn software_config(canvas: (u32, u32)) -> TimelineConfig {
    let mut config = TimelineConfig::new(canvas);
    // Software decode on both sides, so playback and the single-frame renders decode
    // the same pictures (and hardware seeks stay out of this test).
    config.decoder.decoder = DecoderOptions::with_hw(HwPreference::Off);
    config.decode_workers = 4;
    config
}

fn pop(presented: &mut Presented) -> PresentedFrame {
    presented
        .pop_timeout(WAIT)
        .unwrap_or_else(|e| panic!("no presented frame: {e:?}"))
        .frame
}

/// Every layer of timeline frame `t`, decoded exactly in software as the scrubber does.
fn exact_layers(plan: &TimelinePlan, t: i64) -> Vec<Layer<VideoFrame>> {
    let gops = GopIndexes::new();
    let mut layers = Vec::new();
    for segment in plan.segments().segments() {
        if !segment.timeline.contains(&t) {
            continue;
        }
        let mut decoder = SoftwareScrubDecoder::open(
            &segment.source,
            &gops,
            SoftwareDecoderOptions {
                project_rate: media::Rational::new(FPS as i32, 1),
                ..Default::default()
            },
        )
        .expect("open");
        let at = segment.source_time_at(t, plan.frame_duration());
        let (pts, frame) = match decoder.decode_exact(at, &|| false).expect("decode") {
            Decoded::Frame(pts, frame) => (pts, frame),
            other => panic!("frame {t}: {other:?}"),
        };
        layers.push(Layer {
            layer: segment.layer,
            segment: segment.hash,
            source: segment.source.id,
            source_pts: Some(pts),
            frame: Some(Arc::new(frame)),
        });
    }
    layers
}

fn single_renderer(gpu: &common::Gpu, canvas: (u32, u32)) -> TimelineRenderer {
    let mut config = RendererConfig::new(canvas);
    config.cache = Some(FrameCacheConfig {
        vram_ceiling: 1 << 30,
        ram_ceiling: 1 << 30,
        spill_format: SpillFormat::Native,
        spill_slots: FrameCacheConfig::SPILL_SLOTS,
    });
    TimelineRenderer::new(&gpu.device, &gpu.queue, config)
}

fn wait(gpu: &common::Gpu, submission: wgpu::SubmissionIndex) {
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .expect("poll");
}

/// Timeline frame `t` rendered alone at `quality` by a fresh renderer.
fn single_frame(
    gpu: &common::Gpu,
    plan: &TimelinePlan,
    canvas: (u32, u32),
    t: i64,
    quality: QualityLevel,
) -> Vec<u8> {
    let mut renderer = single_renderer(gpu, canvas);
    let rendered = renderer.render(plan, t, &exact_layers(plan, t), quality);
    assert!(!rendered.from_cache);
    wait(gpu, rendered.submission.expect("rendered"));
    common::read_rgba8(gpu, &rendered.texture)
}

fn checksum(pixels: &[u8]) -> u64 {
    // FNV-1a.
    pixels.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn srgb_decode(v: u8) -> f64 {
    let v = f64::from(v) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_encode(v: f64) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let e = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (e * 255.0).round() as u8
}

struct Fixtures {
    gpu: common::Gpu,
    main: std::path::PathBuf,
    other: std::path::PathBuf,
}

fn fixtures(main: &str, other: &str) -> Option<Fixtures> {
    let Some(gpu) = common::gpu() else {
        eprintln!("skipped: no GPU adapter with 16-bit normalised textures");
        return None;
    };
    Some(Fixtures {
        gpu,
        main: common::fixture(main)?,
        other: common::fixture(other)?,
    })
}

/// Percentiles of a list of durations, in milliseconds.
fn millis(mut values: Vec<Duration>, p: f64) -> f64 {
    values.sort();
    let i = ((values.len() as f64 - 1.0) * p).round() as usize;
    values.get(i).map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

#[test]
fn three_seconds_at_1080p30_play_on_time_and_match_single_frame_renders() {
    let Some(f) = fixtures("h264_1080p30.mp4", "prores422_1080p23976.mov") else {
        return;
    };
    let canvas = (1920, 1080);
    let (project, seq) = timeline(&f.main, &f.other, canvas, &LAYOUT_3S);
    let plan = Arc::new(TimelinePlan::build(&project, seq).expect("plan"));
    assert_eq!(plan.end(), LAYOUT_3S.length);
    assert_eq!(plan.segments().segments().len(), 4);
    let clock = Clock::system(48_000);
    let (playback, mut presented) = TimelinePlayback::start(
        plan.clone(),
        &f.gpu.device,
        &f.gpu.queue,
        clock.clone(),
        software_config(canvas),
    )
    .expect("start");

    const CHECKED: [i64; 9] = [0, 12, 30, 44, 47, 50, 53, 70, 89];
    let mut kept: HashMap<i64, (wgpu::Texture, QualityLevel)> = HashMap::new();
    // Paused at the start, the first frame is presented at once.
    let first = pop(&mut presented);
    assert_eq!((first.frame, first.due), (0, None));
    kept.insert(0, (first.texture, first.quality));

    playback.play();
    let started = Instant::now();
    let mut frames = Vec::new();
    let mut costs = Vec::new();
    let mut renders = Vec::new();
    while frames.last() != Some(&(LAYOUT_3S.length - 1)) {
        assert!(started.elapsed() < WAIT, "playback stalled at {frames:?}");
        let frame = pop(&mut presented);
        // Shown like a display would: held until it is due.
        let due = frame.due.expect("a playing frame has a deadline");
        let ahead = due - clock.now();
        if ahead > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(ahead));
        }
        assert!(
            !frame.late,
            "frame {} finished after its deadline",
            frame.frame
        );
        if frame.frame > 1 {
            costs.push(frame.decode_wait + frame.render);
        }
        renders.push(frame.render);
        if CHECKED.contains(&frame.frame) {
            kept.insert(frame.frame, (frame.texture, frame.quality));
        }
        frames.push(frame.frame);
    }
    let elapsed = started.elapsed();
    let stats = playback.stats();
    let quality = playback.quality();
    let lookahead = playback.lookahead_stats();
    let (render_stats, cache) = playback.render_stats();
    let interval_ms = 1000.0 / FPS as f64;
    eprintln!(
        "1080p30: {} frames in {:.2} s, late {}, dropped {}, level {:?} (misses {}), \
         render mean {:.2} ms p99 {:.2} ms max {:.2} ms, decode wait + render p50 {:.2} ms \
         p99 {:.2} ms (headroom at p99 {:.0}% of {interval_ms:.1} ms), look-ahead seeks {} \
         (max {:?}), decode errors {}, renderer {render_stats:?}, cache inserts {}",
        frames.len(),
        elapsed.as_secs_f64(),
        stats.late,
        stats.dropped,
        quality.level,
        quality.misses,
        stats.render_total.as_secs_f64() * 1000.0 / stats.presented.max(1) as f64,
        millis(renders.clone(), 0.99),
        stats.render_max.as_secs_f64() * 1000.0,
        millis(costs.clone(), 0.5),
        millis(costs.clone(), 0.99),
        100.0 * (1.0 - millis(costs.clone(), 0.99) / interval_ms),
        lookahead.seeks,
        lookahead.seek_max,
        lookahead.decode_errors,
        cache.inserts,
    );
    assert_eq!(frames, (1..LAYOUT_3S.length).collect::<Vec<_>>());
    assert_eq!(stats.late, 0);
    assert_eq!(stats.dropped, 0);
    assert_eq!(stats.presented, LAYOUT_3S.length as u64 - 1);
    assert_eq!(lookahead.decode_errors, 0);
    assert_eq!(render_stats.missing_pictures, 0);
    assert_eq!(render_stats.upload_errors, 0);
    assert!(
        elapsed.as_secs_f64() < 3.0 + 0.5,
        "paced playback took {elapsed:?}"
    );

    // Each checked frame equals the same timeline time rendered alone.
    let mut midpoint = None;
    let mut sums = Vec::new();
    for t in CHECKED {
        let (texture, quality) = &kept[&t];
        let played = common::read_rgba8(&f.gpu, texture);
        let alone = single_frame(&f.gpu, &plan, canvas, t, *quality);
        assert_eq!(
            checksum(&played),
            checksum(&alone),
            "frame {t} at {quality:?} differs from its single-frame render"
        );
        sums.push(checksum(&played));
        if t == 50 {
            midpoint = Some(played);
        }
    }
    sums.sort_unstable();
    sums.dedup();
    assert_eq!(
        sums.len(),
        CHECKED.len(),
        "every checked frame shows a different picture"
    );
    playback.shutdown();

    // The dissolve's midpoint is the linear-light mix of its two sides at 0.5: render the
    // frame's description with the progress at 0 (exactly A) and 1 (exactly B).
    let desc = plan.frame(50);
    let [TrackFrame::Transition { transition, linear }] = desc.tracks[..] else {
        panic!("frame 50 is not only the dissolve: {desc:?}");
    };
    assert_eq!(linear, 0.5);
    let layers = exact_layers(&plan, 50);
    let mut renderer = single_renderer(&f.gpu, canvas);
    let mut side = |progress: f64| {
        let mut desc = desc.clone();
        desc.tracks[0] = TrackFrame::Transition {
            transition,
            linear: progress,
        };
        let (texture, submission) = renderer.render_desc(&plan, &desc, &layers, QualityLevel::Full);
        wait(&f.gpu, submission);
        common::read_rgba8(&f.gpu, &texture)
    };
    let a = side(0.0);
    let b = side(1.0);
    assert_ne!(
        checksum(&a),
        checksum(&b),
        "the two sides show different pictures"
    );
    let expected: Vec<u8> = a
        .iter()
        .zip(&b)
        .enumerate()
        .map(|(i, (a, b))| {
            if i % 4 == 3 {
                255
            } else {
                srgb_encode(0.5 * srgb_decode(*a) + 0.5 * srgb_decode(*b))
            }
        })
        .collect();
    let midpoint = midpoint.expect("frame 50 checked");
    let diff = common::diff(&expected, &midpoint);
    eprintln!(
        "dissolve midpoint vs CPU mix: mean {:.3}, p99 {}, max {}",
        diff.mean, diff.p99, diff.max
    );
    // Both sides and the midpoint are dithered to 8 bits independently.
    assert!(diff.max <= 3 && diff.p99 <= 2 && diff.mean < 0.75);
}

#[test]
fn scrubbed_frames_use_the_playback_renderer_and_cache_index() {
    let Some(f) = fixtures("h264_1080p30.mp4", "prores422_1080p23976.mov") else {
        return;
    };
    let canvas = (1920, 1080);
    let (project, seq) = timeline(&f.main, &f.other, canvas, &LAYOUT_3S);
    let plan = Arc::new(TimelinePlan::build(&project, seq).expect("plan"));
    let (playback, mut presented) = TimelinePlayback::start(
        plan.clone(),
        &f.gpu.device,
        &f.gpu.queue,
        Clock::system(48_000),
        software_config(canvas),
    )
    .expect("start");
    assert_eq!(pop(&mut presented).frame, 0);
    let cached = playback.cached_frames();
    assert!(
        cached.contains(0),
        "the paused start frame joined the index"
    );

    let clock = Arc::new(ManualClock::new());
    let gops = GopIndexes::new();
    let mut scrubber = Scrubber::start(
        plan.segments().clone(),
        ScrubConfig {
            frame_duration: plan.frame_duration(),
            ..Default::default()
        },
        cached.clone(),
        clock.clone(),
        move |source: &SourceRef| {
            SoftwareScrubDecoder::open(
                source,
                &gops,
                SoftwareDecoderOptions {
                    project_rate: media::Rational::new(FPS as i32, 1),
                    ..Default::default()
                },
            )
        },
        playback.scrub_render(),
        Some(0),
    )
    .expect("scrubber");
    // Nothing near 70 is cached yet: the nearest cached frame answers at once.
    let shown = scrubber.request(70).expect("shown");
    assert_eq!((shown.frame, shown.exact), (0, false));
    clock.advance(Duration::from_millis(40));
    let started = Instant::now();
    let ready = loop {
        if let Some(ready) = scrubber.poll_ready() {
            break ready;
        }
        assert!(started.elapsed() < WAIT, "no exact frame");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(ready.frame, 70);
    assert!(
        cached.contains(70),
        "the exact frame joined the playback cache index"
    );
    let end = scrubber.finish();
    assert_eq!(end.stats.exact_decodes, 1);

    playback.show(70);
    let frame = pop(&mut presented);
    assert_eq!((frame.frame, frame.from_cache), (70, true));
    let shown = common::read_rgba8(&f.gpu, &frame.texture);
    let alone = single_frame(&f.gpu, &plan, canvas, 70, QualityLevel::Full);
    assert_eq!(checksum(&shown), checksum(&alone));

    // Seeking while paused presents the new position at once.
    playback.seek(20);
    let frame = pop(&mut presented);
    assert_eq!((frame.frame, frame.due), (20, None));
    playback.shutdown();
}

/// Playback throughput with a presenter that takes frames as soon as they come: the
/// frames per second the pipeline sustains, at 1080p and 4K (software decode, debug or
/// release as built). Run with `--ignored --nocapture`.
#[test]
#[ignore = "measures throughput; run alone"]
fn throughput_at_1080p_and_4k() {
    for (main, other, canvas, layout) in [
        (
            "h264_1080p30.mp4",
            "prores422_1080p23976.mov",
            (1920, 1080),
            &LAYOUT_3S,
        ),
        ("h264_4k30.mp4", "h264_4k30.mp4", (3840, 2160), &LAYOUT_2S),
    ] {
        let Some(f) = fixtures(main, other) else {
            continue;
        };
        let (project, seq) = timeline(&f.main, &f.other, canvas, layout);
        let plan = Arc::new(TimelinePlan::build(&project, seq).expect("plan"));
        let mut config = software_config(canvas);
        config.quality = None;
        let (playback, mut presented) = TimelinePlayback::start(
            plan,
            &f.gpu.device,
            &f.gpu.queue,
            Clock::system(48_000),
            config,
        )
        .expect("start");
        assert_eq!(pop(&mut presented).frame, 0);
        // Let the look-ahead fill its lanes, as it does before the user presses play.
        std::thread::sleep(Duration::from_millis(500));
        playback.play();
        let started = Instant::now();
        let mut renders = Vec::new();
        let mut costs = Vec::new();
        let mut last = 0;
        while last != layout.length - 1 {
            let frame = pop(&mut presented);
            renders.push(frame.render);
            costs.push(frame.decode_wait + frame.render);
            last = frame.frame;
        }
        let elapsed = started.elapsed().as_secs_f64();
        let fps = renders.len() as f64 / elapsed;
        eprintln!(
            "{}x{}: {} frames in {elapsed:.2} s = {fps:.1} fps ({:.0}% headroom over 30 fps); \
             render p50 {:.2} ms p99 {:.2} ms, decode wait + render p50 {:.2} ms p99 {:.2} ms",
            canvas.0,
            canvas.1,
            renders.len(),
            100.0 * (1.0 - 30.0 / fps),
            millis(renders.clone(), 0.5),
            millis(renders, 0.99),
            millis(costs.clone(), 0.5),
            millis(costs, 0.99),
        );
        playback.shutdown();
    }
}
