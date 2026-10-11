//! Audio decode to planar f32 through `AudioReader`, reported as samples per second
//! (criterion's elements per second); divide by the output rate for the realtime factor.
//! Each iteration seeks to 0.5 s and reads to the end, so the numbers are steady-state
//! decode, conversion and resampling; opening a file (FFmpeg's stream probe, 4 to 21 ms
//! on the fixtures) is measured separately.
//! Run with `cargo bench -p media --bench audio`, alone on the machine; needs the
//! generated fixtures (`tests/fixtures/generate.sh`) and does nothing without them.

use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use media::{AudioOptions, AudioReader, ChannelLayout};

/// (fixture, output rate, seconds of audio)
const CASES: &[(&str, u32, u64)] = &[
    ("h264_1080p30.mp4", 48_000, 3),
    ("h264_1080p30.mp4", 44_100, 3),
    ("sync_vp9_opus.webm", 48_000, 3),
    ("sync_vp9_opus.webm", 44_100, 3),
    ("audio_44k1_mono.mp3", 48_000, 2),
    ("audio_48k_stereo.wav", 48_000, 3),
    ("audio_48k_stereo.wav", 44_100, 3),
];

fn audio(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    if !dir.join("MANIFEST.json").is_file() {
        eprintln!("skipped: no fixtures; run tests/fixtures/generate.sh");
        return;
    }
    let mut group = c.benchmark_group("audio");
    group.sample_size(20);
    for &(file, rate, seconds) in CASES {
        let path = dir.join(file);
        let options = AudioOptions {
            sample_rate: rate,
            layout: ChannelLayout::Stereo,
            ..AudioOptions::default()
        };
        let mut reader = AudioReader::open(&path, options).expect("open");
        let mut left = vec![0.0f32; 1024];
        let mut right = vec![0.0f32; 1024];
        let start = u64::from(rate) / 2;
        let read_to_end = |reader: &mut AudioReader, left: &mut [f32], right: &mut [f32]| {
            reader.seek_to_sample(start).expect("seek");
            let mut n = 0u64;
            loop {
                let got = reader.read(&mut [&mut *left, &mut *right]).expect("read");
                if got == 0 {
                    return n;
                }
                n += got as u64;
            }
        };
        // Audio can start after time zero (MP3's decoder delay), so count once.
        let samples = read_to_end(&mut reader, &mut left, &mut right);
        let expected = u64::from(rate) * seconds + reader.first_sample() - start;
        assert!(
            samples.abs_diff(expected) <= 1,
            "{file}: {samples} samples, expected {expected}"
        );
        group.throughput(Throughput::Elements(samples));
        group.bench_function(format!("{file}/{rate} Hz stereo"), |b| {
            b.iter(|| {
                let n = read_to_end(&mut reader, &mut left, &mut right);
                assert_eq!(n, samples, "{file}");
                black_box(&left);
            })
        });
        group.bench_function(format!("{file}/{rate} Hz open"), |b| {
            b.iter(|| black_box(AudioReader::open(&path, options).expect("open")))
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = audio
}
criterion_main!(benches);
