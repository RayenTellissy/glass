//! Loudness: `analysis::loudness::measure` on the 30 s program fixture as 16-bit WAV at
//! 48 kHz and as FLAC at 44.1 kHz (each measured at its own rate), and on the 12 s 5.1
//! WAV. Prints the speed against realtime. Run alone with
//! `cargo bench -p media --bench loudness`.

use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use media::analysis::Unattended;
use media::analysis::loudness::{LoudnessSettings, measure};

fn loudness(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    let mut group = c.benchmark_group("loudness");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));
    for file in [
        "loudness_program_48k_stereo.wav",
        "loudness_program_44k1_stereo.flac",
        "loudness_51_48k.wav",
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
            result = Some(measure(&path, &LoudnessSettings, &Unattended).expect("measure"));
            times.push(start.elapsed());
        }
        times.sort();
        let median = times[times.len() / 2].as_secs_f64();
        let result = result.expect("ran");
        let seconds = result.short_term.len() as f64 * 0.1;
        println!(
            "{file}: {:.1} s of audio in {:.2} ms, {:.0}x realtime, I {:.1} LUFS",
            seconds,
            median * 1e3,
            seconds / median,
            result.integrated_lufs.unwrap_or(f64::NAN),
        );
        group.bench_function(file, |b| {
            b.iter(|| measure(&path, &LoudnessSettings, &Unattended).expect("measure"));
        });
    }
    group.finish();
}

criterion_group!(benches, loudness);
criterion_main!(benches);
