//! Proxy encode speed: `analysis::proxy::generate` on the 4K and 1080p H.264 fixtures
//! (2 s and 3 s) for each format and encoder: ProRes Proxy through VideoToolbox and
//! `prores_ks`, H.264 through VideoToolbox and x264. Each case prints the median of 5
//! runs (decode, scale, encode, mux, concatenate, from a warm GOP index and timeline) as
//! seconds, frames per second and times realtime. The clips are short, so the fixed cost
//! of opening the file and the encoder weighs in; frames per second is the steady rate.
//! Run alone with `cargo bench -p media --bench proxy`; needs the generated fixtures.

use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use media::analysis::Unattended;
use media::analysis::proxy::{ProxyEncoder, ProxyFormat, ProxySettings, cache_entry, generate};
use media::{CacheDir, SourceFile};

const CASES: &[&str] = &["h264_4k30.mp4", "h264_1080p30.mp4"];

const ENCODERS: &[(ProxyFormat, ProxyEncoder, &str)] = &[
    (
        ProxyFormat::ProResProxy,
        ProxyEncoder::Hardware,
        "prores_videotoolbox",
    ),
    (
        ProxyFormat::ProResProxy,
        ProxyEncoder::Software,
        "prores_ks",
    ),
    (
        ProxyFormat::H264,
        ProxyEncoder::Hardware,
        "h264_videotoolbox",
    ),
    (ProxyFormat::H264, ProxyEncoder::Software, "libx264"),
];

fn proxy(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    if !dir.join("MANIFEST.json").is_file() {
        eprintln!("skipped: no fixtures; run tests/fixtures/generate.sh");
        return;
    }
    let scratch = std::env::temp_dir().join(format!("glass-proxy-bench-{}", std::process::id()));
    let mut group = c.benchmark_group("proxy");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for &file in CASES {
        let path = dir.join(file);
        let source = SourceFile::open(&path).expect("hash");
        let cache = CacheDir::new(&scratch);
        for &(format, encoder, name) in ENCODERS {
            let settings = ProxySettings {
                format,
                encoder,
                ..ProxySettings::default()
            };
            let entry = cache_entry(&cache, &source, &settings);
            let run = || {
                let _ = std::fs::remove_dir_all(entry.dir());
                let start = Instant::now();
                let proxy = generate(&path, &settings, &entry, Some(&cache), &Unattended);
                proxy.map(|p| (start.elapsed(), p))
            };
            let mut times = Vec::new();
            let mut last = None;
            for _ in 0..5 {
                match run() {
                    Ok((t, p)) => {
                        times.push(t);
                        last = Some(p);
                    }
                    Err(error) => {
                        eprintln!("{file} {name}: skipped ({error})");
                        break;
                    }
                }
            }
            let Some(p) = last else { continue };
            times.sort_unstable();
            let median = times[times.len() / 2].as_secs_f64();
            let seconds = p.duration_us as f64 / 1e6;
            eprintln!(
                "{file} {name} ({}): {}x{}, {} frames in {:.3} s median: {:.0} fps, {:.1}x realtime, {} bytes",
                p.encoder,
                p.width,
                p.height,
                p.frames,
                median,
                p.frames as f64 / median,
                seconds / median,
                p.bytes
            );
            if file == CASES[0] {
                group.bench_function(format!("{file}/{name}"), |b| {
                    b.iter(|| run().expect("proxy"));
                });
            }
        }
    }
    group.finish();
    let _ = std::fs::remove_dir_all(&scratch);
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(Duration::from_secs(1));
    targets = proxy
}
criterion_main!(benches);
