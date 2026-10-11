//! Accurate-seek latency: `FrameReader::seek_exact` to random frames (keyframe seek, then
//! decode forward to the frame), software and VideoToolbox, on the 1080p and 4K H.264
//! fixtures (30-frame GOPs) and the 720p60 fixture with 120-frame open GOPs and B-frames.
//! Feeds the < 50 ms scrub budget. Besides criterion's estimate, each case prints the
//! median, p95 and max of 200 timed random seeks. Run alone with
//! `cargo bench -p media --bench seek`; needs the generated fixtures.

use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use media::{
    DecoderOptions, FrameReader, GopIndex, HwPreference, SourceTimeline, StreamSelect, Timestamp,
};

const CASES: &[&str] = &[
    "h264_1080p30.mp4",
    "h264_4k30.mp4",
    "h264_bframes_gop120_720p60.mp4",
];

/// A fixed pseudo-random sequence, so every run seeks to the same frames.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % below
    }
}

fn frame_times(path: &Path) -> Vec<Timestamp> {
    let (timeline, _) = SourceTimeline::scan(path, StreamSelect::BestVideo).expect("timeline");
    (0..timeline.frame_count())
        .filter_map(|k| timeline.frame_pts(k))
        .collect()
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let at = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[at]
}

fn seek(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    if !dir.join("MANIFEST.json").is_file() {
        eprintln!("skipped: no fixtures; run tests/fixtures/generate.sh");
        return;
    }
    let mut group = c.benchmark_group("seek_exact");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(8));
    for &file in CASES {
        let path = dir.join(file);
        let times = frame_times(&path);
        let (gop, _) = GopIndex::build(&path, StreamSelect::BestVideo).expect("gop index");
        let gop = Arc::new(gop);
        for hw in [HwPreference::Off, HwPreference::Require] {
            let label = if hw == HwPreference::Off {
                "software"
            } else {
                "hardware"
            };
            let open = || {
                FrameReader::open_with(&path, StreamSelect::BestVideo, DecoderOptions::with_hw(hw))
                    .map(|r| r.with_gop_index(gop.clone()))
            };
            let Ok(mut reader) = open() else {
                eprintln!("{file} {label}: skipped (no hardware decoder)");
                continue;
            };
            let mut rng = Lcg(0x5eed);
            let mut samples = Vec::with_capacity(200);
            for _ in 0..200 {
                let target = times[rng.next(times.len() as u64) as usize];
                let start = Instant::now();
                let frame = reader.seek_exact(target).expect("seek").expect("frame");
                samples.push(start.elapsed());
                assert_eq!(frame.pts(), Some(target));
            }
            samples.sort_unstable();
            eprintln!(
                "{file} {label}: 200 random seeks, median {:.1} ms, p95 {:.1} ms, max {:.1} ms",
                percentile(&samples, 0.5).as_secs_f64() * 1e3,
                percentile(&samples, 0.95).as_secs_f64() * 1e3,
                samples[samples.len() - 1].as_secs_f64() * 1e3,
            );
            group.bench_function(format!("{file}/{label}"), |b| {
                b.iter(|| {
                    let target = times[rng.next(times.len() as u64) as usize];
                    black_box(reader.seek_exact(target).expect("seek"));
                })
            });
        }
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(Duration::from_secs(1));
    targets = seek
}
criterion_main!(benches);
