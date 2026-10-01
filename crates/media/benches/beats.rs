//! Beat detection: `analysis::beats::detect` on the 20 s beat fixtures (mono 48 kHz
//! 16-bit FLAC) and on the 10 s silence fixture as WAV and AAC (a different decode
//! path; its beats mean nothing). Prints the speed against realtime. Run alone with
//! `cargo bench -p media --bench beats`.

use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use media::analysis::Unattended;
use media::analysis::beats::{BeatSettings, detect};

fn beats(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    let mut group = c.benchmark_group("beats");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));
    let settings = BeatSettings::default();
    for (file, seconds) in [
        ("beats_128_house.flac", 20.0),
        ("beats_120_clicks.flac", 20.0),
        ("silence_48k_stereo.wav", 10.0),
        ("silence_48k_stereo.m4a", 10.0),
    ] {
        let path = dir.join(file);
        if !path.is_file() {
            println!("skipped: {file} missing; run tests/fixtures/generate.sh");
            continue;
        }
        let mut times = Vec::new();
        let mut result = None;
        for _ in 0..9 {
            let start = Instant::now();
            result = Some(detect(&path, &settings, &Unattended).expect("detect"));
            times.push(start.elapsed());
        }
        times.sort();
        let median = times[times.len() / 2].as_secs_f64();
        let result = result.expect("ran");
        println!(
            "{file}: {seconds:.1} s of audio in {:.2} ms, {:.0}x realtime, {:.2} BPM",
            median * 1e3,
            seconds / median,
            result.bpm,
        );
        group.bench_function(file, |b| {
            b.iter(|| detect(&path, &settings, &Unattended).expect("detect"));
        });
    }
    group.finish();
}

criterion_group!(benches, beats);
criterion_main!(benches);
