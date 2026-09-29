//! Scene cut detection: `analysis::scenes::detect` on the 1080p and 4K H.264 fixtures and
//! the 640x360 cut fixture, with the job's two decoder threads and with one per core.
//! Prints frames per second and the speed against realtime. Run alone with
//! `cargo bench -p media --bench scenes`.

use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use media::analysis::Unattended;
use media::analysis::scenes::{SceneSettings, detect};

fn scenes(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    let mut group = c.benchmark_group("scenes");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for file in [
        "h264_1080p30.mp4",
        "h264_4k30.mp4",
        "scenes_cuts_640x360_30.mp4",
    ] {
        let path = dir.join(file);
        if !path.is_file() {
            println!("skipped: {file} missing; run tests/fixtures/generate.sh");
            continue;
        }
        for threads in [2, 0] {
            let settings = SceneSettings {
                decode_threads: threads,
                ..SceneSettings::default()
            };
            let mut times = Vec::new();
            let mut result = None;
            for _ in 0..5 {
                let start = Instant::now();
                result = Some(detect(&path, &settings, &Unattended).expect("detect"));
                times.push(start.elapsed());
            }
            times.sort();
            let median = times[times.len() / 2].as_secs_f64();
            let result = result.expect("ran");
            let seconds = result.frames as f64 / result.fps;
            let label = if threads == 0 { "all cores" } else { "2 threads" };
            println!(
                "{file} ({label}): {} frames in {:.1} ms, {:.0} fps, {:.1}x realtime",
                result.frames,
                median * 1e3,
                result.frames as f64 / median,
                seconds / median,
            );
            group.bench_function(format!("{file}/{threads}"), |b| {
                b.iter(|| detect(&path, &settings, &Unattended).expect("detect"));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, scenes);
criterion_main!(benches);
