//! Thumbnail strips: `analysis::thumbnails::generate` on the 1080p and 4K H.264 fixtures
//! (3 s and 2 s, 30-frame GOPs), all three levels, GOP index cached as in the app. The
//! 1 s level (level 0) is the finest and the last written, so this is the time to have
//! level 0 on disk; each case also prints the time until the coarse level (30 s) is on
//! disk, the time per further 1 s thumbnail and that rate extrapolated to an hour of
//! footage.
//! Run alone with `cargo bench -p media --bench thumbnails`; needs the generated fixtures.

use std::cell::Cell;
use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use media::analysis::thumbnails::{ThumbnailSettings, generate};
use media::analysis::{Control, Stopped};
use media::{CacheDir, SourceFile};

const CASES: &[&str] = &["h264_1080p30.mp4", "h264_4k30.mp4"];

/// Records when the first (coarsest) level lands.
struct Timer {
    start: Instant,
    coarse: Cell<Option<Duration>>,
}

impl Control for Timer {
    fn checkpoint(&self) -> Result<(), Stopped> {
        Ok(())
    }

    fn progress(&self, _: f32) {}

    fn partial(&self, _: u32) {
        if self.coarse.get().is_none() {
            self.coarse.set(Some(self.start.elapsed()));
        }
    }
}

fn thumbnails(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    if !dir.join("MANIFEST.json").is_file() {
        eprintln!("skipped: no fixtures; run tests/fixtures/generate.sh");
        return;
    }
    let scratch = std::env::temp_dir().join(format!("glass-bench-thumbs-{}", std::process::id()));
    let cache = CacheDir::new(&scratch);
    let settings = ThumbnailSettings::default();
    let mut group = c.benchmark_group("thumbnails");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(8));
    for &file in CASES {
        let source = SourceFile::open(dir.join(file)).expect("hash");
        let entry = cache.entry(&source.hash, "thumbs", &settings.params(), 1);
        // Warm the GOP index into the cache folder, then time runs.
        let strips = generate(
            &source.path,
            &settings,
            &entry,
            Some(&cache),
            &media::analysis::Unattended,
        )
        .expect("thumbnails");
        let count = strips.levels[0].len();
        let mut totals = Vec::new();
        let mut coarse = Vec::new();
        for _ in 0..20 {
            let timer = Timer {
                start: Instant::now(),
                coarse: Cell::new(None),
            };
            generate(&source.path, &settings, &entry, Some(&cache), &timer).expect("thumbnails");
            totals.push(timer.start.elapsed());
            coarse.push(timer.coarse.get().unwrap_or_default());
        }
        totals.sort();
        coarse.sort();
        let median = totals[totals.len() / 2];
        let first = coarse[coarse.len() / 2];
        // Setup (open, probe, timeline, GOP index from the cache) plus the first
        // thumbnail is the coarse level's time; the rest is per-thumbnail work.
        let per_thumb = (median.saturating_sub(first)) / (count.max(2) - 1) as u32;
        println!(
            "{file}: all levels (level 0 = 1 s, {count} thumbnails) median {:.1} ms, coarse level on disk after {:.1} ms, then {:.1} ms per further 1 s thumbnail, about {:.0} s per hour of footage",
            median.as_secs_f64() * 1e3,
            first.as_secs_f64() * 1e3,
            per_thumb.as_secs_f64() * 1e3,
            per_thumb.as_secs_f64() * 3600.0,
        );
        group.bench_function(file, |b| {
            b.iter(|| {
                generate(
                    &source.path,
                    &settings,
                    &entry,
                    Some(&cache),
                    &media::analysis::Unattended,
                )
                .expect("thumbnails")
            });
        });
    }
    group.finish();
    let _ = std::fs::remove_dir_all(&scratch);
}

criterion_group!(benches, thumbnails);
criterion_main!(benches);
