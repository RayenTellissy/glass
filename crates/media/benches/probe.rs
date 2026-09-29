//! Probe cost per file, which bounds project open (budget: under 1 s for the whole
//! project). Run with `cargo bench -p media --bench probe`; needs the generated fixtures
//! (`tests/fixtures/generate.sh`) and does nothing without them.

use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};

/// One file per probe path: MP4 index (1080p and 4K H.264), MOV with PCM audio,
/// Matroska packet scan, MOV with a tmcd track and a display matrix, a still image.
const FILES: &[&str] = &[
    "h264_1080p30.mp4",
    "h264_4k30.mp4",
    "prores422_1080p23976.mov",
    "vp9_720p30.webm",
    "rotated_timecode_h264_640x360_25.mov",
    "still_1080p.png",
];

fn probe(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    if !dir.join("MANIFEST.json").is_file() {
        eprintln!("skipped: no fixtures; run tests/fixtures/generate.sh");
        return;
    }
    media::init();
    let mut group = c.benchmark_group("probe");
    for file in FILES {
        let path = dir.join(file);
        group.bench_function(*file, |b| {
            b.iter(|| black_box(media::probe(&path).expect("probe")))
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    targets = probe
}
criterion_main!(benches);
