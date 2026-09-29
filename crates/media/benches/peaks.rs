//! Waveform peaks: `analysis::peaks::generate` on 60 s of synthetic stereo audio written
//! as 16-bit WAV at 48 kHz (no resampling) and 44.1 kHz (resampled to 48 kHz), and on the
//! AAC track of the 1080p fixture. Prints the time per hour of audio (extrapolated from
//! the 60 s runs) and the peak file size per hour. Run alone with
//! `cargo bench -p media --bench peaks`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use media::analysis::Unattended;
use media::analysis::peaks::{PeakSettings, generate};

const SECONDS: u32 = 60;

fn write_wav(path: &Path, rate: u32) {
    let frames = (rate * SECONDS) as usize;
    let data_len = (frames * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    let mut noise = 0x1234_5678u32;
    for i in 0..frames {
        let t = i as f64 / f64::from(rate);
        let tone = 0.4 * (2.0 * std::f64::consts::PI * 440.0 * t).sin();
        noise ^= noise << 13;
        noise ^= noise >> 17;
        noise ^= noise << 5;
        let hiss = (f64::from(noise) / f64::from(u32::MAX) - 0.5) * 0.2;
        for v in [tone, hiss] {
            out.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
        }
    }
    std::fs::write(path, out).expect("write wav");
}

fn peaks(c: &mut Criterion) {
    let scratch = std::env::temp_dir().join(format!("glass-bench-peaks-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch");
    let mut cases: Vec<(String, PathBuf, f64)> = Vec::new();
    for rate in [48_000, 44_100] {
        let path = scratch.join(format!("stereo_{rate}.wav"));
        write_wav(&path, rate);
        cases.push((format!("wav_s16_stereo_{rate}"), path, f64::from(SECONDS)));
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/generated/h264_1080p30.mp4");
    if fixture.is_file() {
        cases.push(("aac_stereo_48000_3s".to_string(), fixture, 3.0));
    }
    let settings = PeakSettings::default();
    let mut group = c.benchmark_group("peaks");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for (name, path, seconds) in &cases {
        let mut times = Vec::new();
        let mut bytes = 0;
        for _ in 0..5 {
            let start = Instant::now();
            let peaks = generate(path, &settings, &Unattended).expect("peaks");
            times.push(start.elapsed());
            bytes = peaks.to_bytes().len();
        }
        times.sort();
        let median = times[times.len() / 2].as_secs_f64();
        let per_hour = 3600.0 / seconds;
        println!(
            "{name}: {:.1} ms for {seconds} s, {:.1} s per hour of audio ({:.0}x realtime), peak file {:.2} MB per hour",
            median * 1e3,
            median * per_hour,
            seconds / median,
            bytes as f64 * per_hour / 1e6,
        );
        group.bench_function(name.as_str(), |b| {
            b.iter(|| generate(path, &settings, &Unattended).expect("peaks"));
        });
    }
    group.finish();
    let _ = std::fs::remove_dir_all(&scratch);
}

criterion_group!(benches, peaks);
criterion_main!(benches);
