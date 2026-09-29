//! Frame-time monitor costs: recording runs every UI frame, stats run when the debug
//! panel is open.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use ui::frame_time::FrameTimeMonitor;

fn frame_time(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_time");
    let mut monitor = FrameTimeMonitor::default();
    let mut ns = 0u64;
    group.bench_function("record", |b| {
        b.iter(|| {
            ns = (ns + 7_919) % 9_000_000;
            monitor.record(black_box(Duration::from_nanos(ns)));
        })
    });
    group.bench_function("stats_full_window", |b| {
        b.iter(|| black_box(monitor.stats()))
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    targets = frame_time
}
criterion_main!(benches);
