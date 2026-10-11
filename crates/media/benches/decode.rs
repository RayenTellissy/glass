//! Software decode throughput, reported as frames per second (criterion's elements per
//! second). Run with `cargo bench -p media --bench decode`, alone on the machine; needs the
//! generated fixtures (`tests/fixtures/generate.sh`) and does nothing without them.

use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use media::{DecoderOptions, FrameReader, StreamSelect};

/// (fixture, frames, decoder threads; 0 = one per core)
const CASES: &[(&str, u64, usize)] = &[
    ("h264_4k30.mp4", 60, 0),
    ("h264_4k30.mp4", 60, 1),
    ("h264_1080p30.mp4", 90, 0),
    ("hevc_10bit_1080p25.mp4", 50, 0),
    ("prores422_1080p23976.mov", 48, 0),
];

fn decode(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    if !dir.join("MANIFEST.json").is_file() {
        eprintln!("skipped: no fixtures; run tests/fixtures/generate.sh");
        return;
    }
    let mut group = c.benchmark_group("decode");
    group.sample_size(10);
    for &(file, frames, threads) in CASES {
        let path = dir.join(file);
        let options = DecoderOptions {
            threads,
            ..Default::default()
        };
        let name = format!(
            "{file}/{}",
            if threads == 0 {
                "auto threads".to_string()
            } else {
                format!("{threads} thread")
            }
        );
        group.throughput(Throughput::Elements(frames));
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut reader =
                    FrameReader::open_with(&path, StreamSelect::BestVideo, options).expect("open");
                let mut n = 0;
                while let Some(frame) = reader.next_frame().expect("decode") {
                    black_box(&frame);
                    n += 1;
                }
                assert_eq!(n, frames);
            })
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(4));
    targets = decode
}
criterion_main!(benches);
