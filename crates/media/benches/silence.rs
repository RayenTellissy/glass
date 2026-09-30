//! Silence detection: `analysis::silence::detect` on the 10 s silence fixture as 16-bit
//! WAV and as AAC in MP4, at the project rate (48 kHz, no resampling). Prints the speed
//! against realtime. Run alone with `cargo bench -p media --bench silence`.

use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use media::analysis::Unattended;
use media::analysis::silence::{SilenceSettings, detect};

fn silence(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    let mut group = c.benchmark_group("silence");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(5));
    let settings = SilenceSettings::default();
    for file in ["silence_48k_stereo.wav", "silence_48k_stereo.m4a"] {
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
        let seconds =
            (result.end_sample - result.first_sample) as f64 / f64::from(result.sample_rate);
        println!(
            "{file}: {:.1} s of audio in {:.2} ms, {:.0}x realtime, {} silences",
            seconds,
            median * 1e3,
            seconds / median,
            result.ranges.len(),
        );
        group.bench_function(file, |b| {
            b.iter(|| detect(&path, &settings, &Unattended).expect("detect"));
        });
    }
    group.finish();
}

criterion_group!(benches, silence);
criterion_main!(benches);
