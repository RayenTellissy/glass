//! The background export queue end to end: a generated sequence (two clips of the 1080p30
//! fixture with a cross dissolve, and a subtitle track) exported with the YouTube 1080p
//! preset through x264: the verification report is clean, decoded frames equal the
//! timeline renderer's frames within codec tolerance, pause and resume keep every frame,
//! cancel leaves nothing behind, queued jobs run in order, progress only grows, burned-in
//! subtitles reach the pixels and sidecars and embedded cues are written. GPU checks skip
//! without an adapter, media checks without the fixtures.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use engine::export::{
    EventKind, ExportEvent, ExportJob, ExportQueue, ExportQueueConfig, Finished, JobId,
    NotificationKind, SidecarFormat, Stage, SubtitleExport, output_encoding,
};
use engine::lookahead::{GopIndexes, Layer};
use engine::scrub::{Decoded, ScrubDecoder, SoftwareDecoderOptions, SoftwareScrubDecoder};
use engine::threads::{ThreadRegistry, ThreadState};
use engine::timeline::{FrameOutput, RendererConfig, TimelinePlan, TimelineRenderer};
use media::export::presets::Preset;
use media::export::{ColourTags, EncoderPreference};
use media::{DecoderOptions, Frame, FrameReader, HwPreference, StreamSelect, VideoFrame};
use project::{
    ClipSpec, Project, Rational, Resolution, SequenceFormat, SequenceId, Time, TimeRange, TrackId,
    TrackKind, TransitionSpec,
};
use render::subtitle::SubtitleStyle;
use render::wgpu;

const WAIT: Duration = Duration::from_secs(120);
const FPS: i64 = 30;
const ARIAL: &str = "/System/Library/Fonts/Supplemental/Arial.ttf";

fn at(frames: i64) -> Time {
    Time::from_frames(frames, Rational::new(FPS as i32, 1)).expect("time")
}

fn range(start: i64, end: i64) -> TimeRange {
    TimeRange::between(at(start), at(end)).expect("range")
}

struct Sequence {
    project: Project,
    seq: SequenceId,
    subtitles: TrackId,
}

/// Two seconds at 30 fps on a `canvas`: A (source 0) then B (from source frame
/// `b_source`) on V1 with a centred 10-frame cross dissolve on their cut at 1 s, and two
/// subtitle cues.
fn sequence(media: &Path, canvas: (u32, u32), b_source: i64) -> Sequence {
    let p = Project::new("export-queue", "export queue");
    let (p, m) = p.add_media("main", media.to_string_lossy()).expect("media");
    let (p, seq) = p
        .add_sequence_with(
            "main",
            SequenceFormat {
                resolution: Resolution::new(canvas.0, canvas.1),
                frame_rate: Rational::new(FPS as i32, 1),
                ..SequenceFormat::default()
            },
        )
        .expect("sequence");
    let (p, subtitles) = p
        .add_track(seq, TrackKind::Subtitle, "Subtitles", None)
        .expect("subtitle track");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("V1");
    let clip = |start, end, source| ClipSpec {
        media: m,
        range: range(start, end),
        source_start: at(source),
    };
    let (p, a) = p.insert_clip(v1, clip(0, 30, 0)).expect("A");
    let (p, b) = p.insert_clip(v1, clip(30, 60, b_source)).expect("B");
    let (p, _) = p
        .add_transition(TransitionSpec::new(Some(a), Some(b), at(10)))
        .expect("dissolve");
    let (p, _) = p
        .insert_subtitle(subtitles, range(5, 25), "Burned into the picture")
        .expect("cue 1");
    let (p, _) = p
        .insert_subtitle(subtitles, range(35, 55), "A second cue\non two lines")
        .expect("cue 2");
    Sequence {
        project: p,
        seq,
        subtitles,
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("glass-export-queue-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch folder");
    dir
}

/// File names in `dir`, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("listing")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

struct Setup {
    gpu: common::Gpu,
    media: PathBuf,
}

fn setup(media: &str) -> Option<Setup> {
    let Some(gpu) = common::gpu() else {
        eprintln!("skipped: no GPU adapter with 16-bit normalised textures");
        return None;
    };
    Some(Setup {
        gpu,
        media: common::fixture(media)?,
    })
}

/// Software decode, so the export's layers and the reference's are the same pictures.
fn config() -> ExportQueueConfig {
    let mut config = ExportQueueConfig::default();
    config.decoder.decoder = DecoderOptions::with_hw(HwPreference::Off);
    config.decode_workers = 4;
    config
}

fn youtube_job(s: &Sequence, output: &Path, preference: EncoderPreference) -> ExportJob {
    let preset = Preset::built_in("youtube_1080p").expect("preset");
    let mut job = ExportJob::from_preset(s.project.clone(), s.seq, preset, output).expect("job");
    job.settings
        .video
        .as_mut()
        .expect("video")
        .encoder_preference = preference;
    job
}

/// Events until `done` says so, failing after [`WAIT`].
fn collect(rx: &Receiver<ExportEvent>, done: impl Fn(&[ExportEvent]) -> bool) -> Vec<ExportEvent> {
    let started = Instant::now();
    let mut events = Vec::new();
    while !done(&events) {
        let left = WAIT.saturating_sub(started.elapsed());
        match rx.recv_timeout(left) {
            Ok(event) => events.push(event),
            Err(e) => panic!("no export event ({e:?}) after {events:#?}"),
        }
    }
    events
}

fn ended(events: &[ExportEvent], job: JobId) -> bool {
    events.iter().any(|e| {
        e.job == job
            && matches!(
                e.kind,
                EventKind::Finished(_) | EventKind::Cancelled | EventKind::Failed { .. }
            )
    })
}

fn finished(events: &[ExportEvent], job: JobId) -> &Finished {
    for e in events {
        if e.job != job {
            continue;
        }
        match &e.kind {
            EventKind::Finished(f) => return f,
            EventKind::Failed { failure, .. } => panic!("job {job:?} failed: {failure:?}"),
            EventKind::Cancelled => panic!("job {job:?} was cancelled"),
            _ => {}
        }
    }
    panic!("job {job:?} did not finish")
}

/// Progress of `job` only grows, and the last one is every frame.
fn check_progress(events: &[ExportEvent], job: JobId, total: u64) {
    let progress: Vec<_> = events
        .iter()
        .filter(|e| e.job == job)
        .filter_map(|e| match &e.kind {
            EventKind::Progress(p) => Some(*p),
            _ => None,
        })
        .collect();
    assert!(!progress.is_empty());
    for pair in progress.windows(2) {
        assert!(
            pair[1].frames_done >= pair[0].frames_done,
            "progress went back: {pair:?}"
        );
        assert!(pair[1].elapsed >= pair[0].elapsed, "{pair:?}");
    }
    let rendering: Vec<_> = progress
        .iter()
        .filter(|p| p.stage == Stage::Rendering)
        .collect();
    let last = rendering.last().expect("rendering progress");
    assert_eq!((last.frames_done, last.total_frames), (total, total));
    assert!((last.fraction() - 1.0).abs() < 1e-12);
    let stages: Vec<Stage> = progress
        .iter()
        .map(|p| p.stage)
        .fold(Vec::new(), |mut v, s| {
            if v.last() != Some(&s) {
                v.push(s);
            }
            v
        });
    assert_eq!(
        stages,
        [
            Stage::Preparing,
            Stage::Rendering,
            Stage::Finishing,
            Stage::Verifying
        ]
    );
}

fn decode_all(path: &Path) -> Vec<VideoFrame> {
    let mut reader = FrameReader::open_with(
        path,
        StreamSelect::BestVideo,
        DecoderOptions::with_hw(HwPreference::Off),
    )
    .expect("reader");
    let mut frames = Vec::new();
    while let Some(frame) = reader.next_frame().expect("decode") {
        if let Frame::Video(v) = frame {
            frames.push(v);
        }
    }
    frames
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

/// Timeline frame `t` as the export delivers it (BT.709, BT.1886, 8-bit), rendered alone
/// by a fresh renderer and read back as RGBA.
fn reference_frame(gpu: &common::Gpu, plan: &TimelinePlan, t: i64) -> Vec<u8> {
    let mut renderer =
        TimelineRenderer::new(&gpu.device, &gpu.queue, RendererConfig::new(plan.canvas()));
    let (primaries, transfer) = output_encoding(ColourTags::Bt709);
    let output = FrameOutput {
        size: plan.canvas(),
        primaries,
        transfer,
        format: wgpu::TextureFormat::Rgba8Unorm,
        burn_in: None,
    };
    let (texture, submission) = renderer.render_output(plan, t, &exact_layers(plan, t), &output);
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .expect("poll");
    common::read_rgba8(gpu, &texture)
}

/// PSNR in dB of the decoded luma plane against BT.709 limited-range luma of `rgba`.
fn luma_psnr(frame: &VideoFrame, rgba: &[u8]) -> f64 {
    let plane = frame.plane(0).expect("luma");
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let mut sum = 0.0f64;
    for y in 0..h {
        for x in 0..w {
            let p = &rgba[(y * w + x) * 4..][..3];
            let luma =
                0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
            let expected = 16.0 + luma * 219.0 / 255.0;
            let d = f64::from(plane.data[y * plane.stride + x]) - expected;
            sum += d * d;
        }
    }
    let mse = sum / (w * h) as f64;
    10.0 * (255.0f64 * 255.0 / mse.max(1e-12)).log10()
}

#[test]
fn a_dissolve_exports_clean_and_matches_the_renderer() {
    let Some(f) = setup("h264_1080p30.mp4") else {
        return;
    };
    let s = sequence(&f.media, (1920, 1080), 45);
    let dir = scratch("dissolve");
    let output = dir.join("dissolve.mp4");
    let (queue, rx) = ExportQueue::new(&f.gpu.device, &f.gpu.queue, config());
    let id = queue.submit(youtube_job(&s, &output, EncoderPreference::Software));
    let events = collect(&rx, |e| ended(e, id));
    assert!(matches!(events[0].kind, EventKind::Queued { ahead: 0 }));
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, EventKind::Started { total_frames: 60 }))
    );
    check_progress(&events, id, 60);
    let done = finished(&events, id);

    assert_eq!(done.path, output);
    assert_eq!(done.files, std::slice::from_ref(&output));
    assert_eq!(done.frames, 60);
    assert_eq!(done.video_encoder.as_deref(), Some("libx264"));
    assert!(done.silent_audio);
    assert_eq!(done.offline_media, 0);
    let report = done.report.as_ref().expect("verified");
    assert!(report.findings.is_empty(), "{:#?}", report.findings);
    assert_eq!(report.measured.frames, Some(60));
    assert_eq!(done.notification.kind, NotificationKind::Finished);
    assert_eq!(
        done.notification.args,
        [("name", "dissolve.mp4".to_owned())]
    );
    assert_eq!(listing(&dir), ["dissolve.mp4"]);
    println!(
        "1080p30 export (YouTube 1080p, x264, software decode): {} frames in {:.2} s, {:.1} fps",
        done.frames,
        done.elapsed.as_secs_f64(),
        done.fps
    );

    let plan = TimelinePlan::build(&s.project, s.seq).expect("plan");
    let decoded = decode_all(&output);
    assert_eq!(decoded.len(), 60);
    // A, the dissolve's start, middle and end, and B.
    for t in [3, 25, 30, 34, 50] {
        let reference = reference_frame(&f.gpu, &plan, t);
        let psnr = luma_psnr(&decoded[t as usize], &reference);
        println!("frame {t}: luma PSNR {psnr:.1} dB");
        assert!(psnr > 40.0, "frame {t}: luma PSNR {psnr:.1} dB");
    }
    // The dissolve is visible: its middle differs from both of its sides.
    let middle = reference_frame(&f.gpu, &plan, 30);
    for side in [24, 36] {
        let other = reference_frame(&f.gpu, &plan, side);
        assert!(common::diff(&middle, &other).mean > 1.0);
    }
    drop(queue);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pause_and_resume_keep_every_frame() {
    let Some(f) = setup("h264_1080p30.mp4") else {
        return;
    };
    let s = sequence(&f.media, (1920, 1080), 45);
    let dir = scratch("pause");
    let output = dir.join("paused.mp4");
    let registry = ThreadRegistry::new();
    let mut config = config();
    config.progress_interval = Duration::ZERO;
    config.registry = registry.clone();
    let (queue, rx) = ExportQueue::new(&f.gpu.device, &f.gpu.queue, config);
    let id = queue.submit(youtube_job(&s, &output, EncoderPreference::Software));
    let mut events = collect(&rx, |e| {
        e.iter().any(|e| {
            matches!(&e.kind, EventKind::Progress(p) if p.stage == Stage::Rendering && p.frames_done >= 5)
        })
    });
    assert!(queue.pause(id));
    assert!(queue.is_paused(id));
    events.extend(collect(&rx, |e| {
        e.iter().any(|e| matches!(e.kind, EventKind::Paused { .. }))
    }));
    let paused_at = events
        .iter()
        .find_map(|e| match e.kind {
            EventKind::Paused { frames_done } => Some(frames_done),
            _ => None,
        })
        .expect("paused");
    assert!(paused_at < 60);
    // Nothing moves while paused; the pipeline's threads wait, named and listed.
    std::thread::sleep(Duration::from_millis(500));
    let during: Vec<ExportEvent> = rx.try_iter().collect();
    assert!(during.is_empty(), "{during:#?}");
    let names = |registry: &ThreadRegistry| {
        let mut names: Vec<String> = registry
            .snapshot()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        names(&registry),
        [
            "glass-decode-0",
            "glass-decode-1",
            "glass-decode-2",
            "glass-decode-3",
            "glass-export",
            "glass-export-render",
            "glass-sched",
        ]
    );
    for t in registry.snapshot() {
        assert_ne!(t.state, ThreadState::Busy, "{} works while paused", t.name);
    }
    assert!(
        listing(&dir).iter().any(|n| n.starts_with(".paused.mp4.")),
        "the partial file is hidden next to the output: {:?}",
        listing(&dir)
    );
    assert!(!output.exists());
    assert!(queue.resume(id));
    events.extend(collect(&rx, |e| ended(e, id)));
    assert!(events.iter().any(|e| matches!(e.kind, EventKind::Resumed)));
    check_progress(&events, id, 60);
    let done = finished(&events, id);
    assert_eq!(done.frames, 60);
    let report = done.report.as_ref().expect("verified");
    assert!(report.findings.is_empty(), "{:#?}", report.findings);
    assert_eq!(report.measured.frames, Some(60));
    assert_eq!(decode_all(&output).len(), 60);
    assert_eq!(listing(&dir), ["paused.mp4"]);
    // `glass-export` leaves once the queue is empty.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !registry.is_empty() {
        assert!(Instant::now() < deadline, "{:?}", names(&registry));
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(queue);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cancel_deletes_the_partial_file() {
    let Some(f) = setup("h264_1080p30.mp4") else {
        return;
    };
    let s = sequence(&f.media, (1920, 1080), 45);
    let dir = scratch("cancel");
    let output = dir.join("cancelled.mp4");
    let mut config = config();
    config.progress_interval = Duration::ZERO;
    let (queue, rx) = ExportQueue::new(&f.gpu.device, &f.gpu.queue, config);
    let job = youtube_job(&s, &output, EncoderPreference::Software).with_subtitles(
        SubtitleExport::Sidecar {
            track: s.subtitles,
            format: SidecarFormat::Vtt,
        },
    );
    let id = queue.submit(job);
    collect(&rx, |e| {
        e.iter().any(|e| {
            matches!(&e.kind, EventKind::Progress(p) if p.stage == Stage::Rendering && p.frames_done >= 5)
        })
    });
    assert!(queue.pause(id));
    collect(&rx, |e| {
        e.iter().any(|e| matches!(e.kind, EventKind::Paused { .. }))
    });
    let partial = listing(&dir);
    assert_eq!(partial.len(), 1, "{partial:?}");
    assert!(partial[0].starts_with(".cancelled.mp4."), "{partial:?}");
    assert_eq!(queue.running(), Some(id));
    assert!(queue.cancel(id));
    let events = collect(&rx, |e| ended(e, id));
    assert!(matches!(
        events.last().map(|e| &e.kind),
        Some(EventKind::Cancelled)
    ));
    assert!(listing(&dir).is_empty(), "{:?}", listing(&dir));
    assert!(!output.exists());
    assert!(!queue.cancel(id));
    // A cancel during the render (not paused) leaves nothing either.
    let output = dir.join("again.mp4");
    let id = queue.submit(youtube_job(&s, &output, EncoderPreference::Software));
    collect(&rx, |e| {
        e.iter().any(|e| {
            matches!(&e.kind, EventKind::Progress(p) if p.stage == Stage::Rendering && p.frames_done >= 3)
        })
    });
    assert!(queue.cancel(id));
    let events = collect(&rx, |e| ended(e, id));
    assert!(matches!(
        events.last().map(|e| &e.kind),
        Some(EventKind::Cancelled)
    ));
    assert!(listing(&dir).is_empty(), "{:?}", listing(&dir));
    drop(queue);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn queued_jobs_run_in_order() {
    let Some(f) = setup("h264_1080p30.mp4") else {
        return;
    };
    let s = sequence(&f.media, (1920, 1080), 45);
    let dir = scratch("order");
    let (queue, rx) = ExportQueue::new(&f.gpu.device, &f.gpu.queue, config());
    let first = queue.submit(
        youtube_job(&s, &dir.join("first.mp4"), EncoderPreference::Software).with_range(0..20),
    );
    let second = queue.submit(
        youtube_job(&s, &dir.join("second.mp4"), EncoderPreference::Software).with_range(20..45),
    );
    let third = queue.submit(
        youtube_job(&s, &dir.join("third.mp4"), EncoderPreference::Software).with_range(45..60),
    );
    assert!(queue.cancel(third), "a queued job leaves the queue");
    let events = collect(&rx, |e| {
        ended(e, first) && ended(e, second) && ended(e, third)
    });
    let order: Vec<(JobId, &'static str)> = events
        .iter()
        .filter_map(|e| {
            let what = match e.kind {
                EventKind::Queued { .. } => "queued",
                EventKind::Started { .. } => "started",
                EventKind::Finished(_) => "finished",
                EventKind::Cancelled => "cancelled",
                _ => return None,
            };
            Some((e.job, what))
        })
        .collect();
    assert_eq!(
        order,
        [
            (first, "queued"),
            (second, "queued"),
            (third, "queued"),
            (third, "cancelled"),
            (first, "started"),
            (first, "finished"),
            (second, "started"),
            (second, "finished"),
        ]
    );
    assert!(matches!(events[1].kind, EventKind::Queued { ahead: 1 }));
    check_progress(&events, first, 20);
    check_progress(&events, second, 25);
    for (id, frames) in [(first, 20), (second, 25)] {
        let done = finished(&events, id);
        assert_eq!(done.frames, frames);
        let report = done.report.as_ref().expect("verified");
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }
    assert_eq!(listing(&dir), ["first.mp4", "second.mp4"]);
    drop(queue);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn burned_in_subtitles_reach_the_pixels() {
    let Some(f) = setup("h264_1080p30.mp4") else {
        return;
    };
    if !Path::new(ARIAL).exists() {
        eprintln!("skipped: {ARIAL} missing");
        return;
    }
    let s = sequence(&f.media, (1920, 1080), 45);
    let dir = scratch("burn");
    let (queue, rx) = ExportQueue::new(&f.gpu.device, &f.gpu.queue, config());
    let plain = dir.join("plain.mp4");
    let burned = dir.join("burned.mp4");
    let a = queue.submit(youtube_job(&s, &plain, EncoderPreference::Software).with_range(0..30));
    let b = queue.submit(
        youtube_job(&s, &burned, EncoderPreference::Software)
            .with_range(0..30)
            .with_subtitles(SubtitleExport::BurnedIn {
                track: s.subtitles,
                style: SubtitleStyle::default(),
                fonts: vec![PathBuf::from(ARIAL)],
            }),
    );
    let events = collect(&rx, |e| ended(e, a) && ended(e, b));
    let report = |id| finished(&events, id).report.as_ref().expect("verified");
    assert!(report(a).findings.is_empty());
    assert!(report(b).findings.is_empty());

    let plain = decode_all(&plain);
    let burned = decode_all(&burned);
    assert_eq!((plain.len(), burned.len()), (30, 30));
    // Mean absolute luma difference per row band: the cue (frames 5 to 24) sits in the
    // bottom of the title-safe area.
    let band = |a: &VideoFrame, b: &VideoFrame, rows: std::ops::Range<usize>| {
        let (pa, pb) = (a.plane(0).expect("luma"), b.plane(0).expect("luma"));
        let w = a.width() as usize;
        let mut sum = 0u64;
        for y in rows.clone() {
            for x in 0..w {
                sum += u64::from(pa.data[y * pa.stride + x].abs_diff(pb.data[y * pb.stride + x]));
            }
        }
        sum as f64 / (rows.len() * w) as f64
    };
    let bottom = 1080 * 80 / 100..1080 * 95 / 100;
    let top = 0..1080 * 60 / 100;
    for t in [5, 15, 24] {
        let text = band(&plain[t], &burned[t], bottom.clone());
        let rest = band(&plain[t], &burned[t], top.clone());
        println!("frame {t}: subtitle band {text:.2}, picture above {rest:.2} levels");
        assert!(text > 3.0, "frame {t}: no subtitle in the band ({text:.2})");
        assert!(
            rest < 1.0,
            "frame {t}: the picture above changed ({rest:.2})"
        );
    }
    for t in [0, 4, 26] {
        let text = band(&plain[t], &burned[t], bottom.clone());
        assert!(text < 1.0, "frame {t} has no cue ({text:.2})");
    }
    drop(queue);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sidecar_and_embedded_subtitles_are_written() {
    let Some(f) = setup("h264_1080p30.mp4") else {
        return;
    };
    let s = sequence(&f.media, (1920, 1080), 45);
    let dir = scratch("subtitles");
    let (queue, rx) = ExportQueue::new(&f.gpu.device, &f.gpu.queue, config());
    let side = dir.join("side.mp4");
    let embedded = dir.join("embedded.mp4");
    let a = queue.submit(
        youtube_job(&s, &side, EncoderPreference::Software)
            .with_range(10..60)
            .with_subtitles(SubtitleExport::Sidecar {
                track: s.subtitles,
                format: SidecarFormat::Srt,
            }),
    );
    let b = queue.submit(
        youtube_job(&s, &embedded, EncoderPreference::Software)
            .with_range(10..60)
            .with_subtitles(SubtitleExport::Embedded {
                track: s.subtitles,
                language: "eng".into(),
                title: None,
            }),
    );
    let events = collect(&rx, |e| ended(e, a) && ended(e, b));
    let mut done: Vec<Box<Finished>> = Vec::new();
    for e in events {
        match e.kind {
            EventKind::Finished(f) => done.push(f),
            EventKind::Failed { failure, .. } => panic!("{failure:?}"),
            _ => {}
        }
    }
    assert_eq!(done.len(), 2);
    let srt = dir.join("side.srt");
    assert_eq!(done[0].sidecar.as_deref(), Some(srt.as_path()));
    assert_eq!(done[0].files, [side.clone(), srt.clone()]);
    let track = s
        .project
        .sequence(s.seq)
        .and_then(|q| q.track(s.subtitles))
        .expect("track");
    let cues = project::subtitle_export::track_cues(track, Some(range(10, 60))).expect("cues");
    assert_eq!(cues.len(), 2);
    // Relative to the range: the first cue is cut at its start.
    assert_eq!((cues[0].start_ms, cues[0].end_ms), (0, 500));
    let text = std::fs::read_to_string(&srt).expect("sidecar");
    assert_eq!(text, project::subtitle_export::to_srt(&cues));
    assert!(text.starts_with("1\n00:00:00,000 --> 00:00:00,500\nBurned into the picture\n"));

    let read = media::read_subtitle_cues(&embedded, 2).expect("embedded cues");
    let expected: Vec<(i64, i64, String)> = cues
        .iter()
        .map(|c| (c.start_ms, c.end_ms, c.text.clone()))
        .collect();
    let found: Vec<(i64, i64, String)> = read
        .iter()
        .map(|c| (c.start_ms, c.end_ms, c.text.clone()))
        .collect();
    assert_eq!(found, expected);
    for d in &done {
        let report = d.report.as_ref().expect("verified");
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }
    assert_eq!(listing(&dir), ["embedded.mp4", "side.mp4", "side.srt"]);
    drop(queue);
    let _ = std::fs::remove_dir_all(dir);
}

/// Export throughput at 1080p and 4K with x264 and with VideoToolbox (`Auto`); run alone:
/// `cargo test -p engine --test export_queue -- --ignored --nocapture`.
#[test]
#[ignore = "timing; run alone"]
fn export_throughput() {
    for (fixture, canvas, preset) in [
        ("h264_1080p30.mp4", (1920, 1080), "youtube_1080p"),
        ("h264_4k30.mp4", (3840, 2160), "youtube_4k"),
    ] {
        let Some(f) = setup(fixture) else {
            return;
        };
        // The 4K fixture lasts 2 s: B starts at its first second.
        let s = sequence(&f.media, canvas, if canvas.0 > 1920 { 30 } else { 45 });
        let dir = scratch("throughput");
        let config = ExportQueueConfig {
            decode_workers: 4,
            ..ExportQueueConfig::default()
        };
        let (queue, rx) = ExportQueue::new(&f.gpu.device, &f.gpu.queue, config);
        for preference in [EncoderPreference::Software, EncoderPreference::Auto] {
            let preset = Preset::built_in(preset).expect("preset");
            let output = dir.join(format!("{}-{preference:?}.mp4", preset.id));
            let mut job =
                ExportJob::from_preset(s.project.clone(), s.seq, preset, output).expect("job");
            let video = job.settings.video.as_mut().expect("video");
            video.encoder_preference = preference;
            if preference == EncoderPreference::Auto {
                // VideoToolbox H.264 with B-frames fails in the muxer ("writing a packet:
                // Invalid argument", a `media` follow-up), so it is timed without them.
                video.max_b_frames = Some(0);
            }
            let id = queue.submit(job);
            let events = collect(&rx, |e| ended(e, id));
            let outcome = events.iter().find_map(|e| match &e.kind {
                EventKind::Finished(f) => Some(Ok(f)),
                EventKind::Failed { failure, .. } => Some(Err(failure)),
                _ => None,
            });
            let size = format!("{}x{} {} {preference:?}", canvas.0, canvas.1, preset.id);
            match outcome {
                Some(Ok(done)) => println!(
                    "{size}: {} frames in {:.2} s, {:.1} fps ({:?})",
                    done.frames,
                    done.elapsed.as_secs_f64(),
                    done.fps,
                    done.video_encoder
                ),
                // Reported, not failed: hardware encoders vary by machine.
                Some(Err(failure)) if preference == EncoderPreference::Auto => {
                    println!("{size}: {failure:?}")
                }
                other => panic!("{size}: {other:?}"),
            }
        }
        drop(queue);
        let _ = std::fs::remove_dir_all(dir);
    }
}
