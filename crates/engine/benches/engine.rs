//! Engine micro-benchmarks. Run with `cargo bench -p engine --bench "*"`; see
//! CONTRIBUTING.md for short local runs and baselines.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use engine::logging::{DEFAULT_RING_CAPACITY, LogFilter, LogRing, RingLayer};
use tracing::dispatcher::{self, Dispatch};
use tracing_subscriber::layer::SubscriberExt as _;

fn enabled_features(c: &mut Criterion) {
    c.bench_function("enabled_features", |b| {
        b.iter(|| black_box(engine::enabled_features()))
    });
}

fn log_ring(c: &mut Criterion) {
    let ring = Arc::new(LogRing::new(DEFAULT_RING_CAPACITY));
    let dispatch = Dispatch::new(tracing_subscriber::registry().with(RingLayer::new(ring.clone())));
    let mut group = c.benchmark_group("log_ring");
    dispatcher::with_default(&dispatch, || {
        group.bench_function("record", |b| {
            b.iter(
                || tracing::info!(target: "engine::bench", frame = black_box(42u64), "frame done"),
            )
        });
    });
    // The ring is full after the warm-up, as it is in a long session.
    group.bench_function("snapshot_full", |b| {
        b.iter(|| black_box(ring.snapshot(&LogFilter::default())))
    });
    let warnings = LogFilter {
        min_level: Some(tracing::Level::WARN),
        target: None,
    };
    group.bench_function("snapshot_filtered_empty", |b| {
        b.iter(|| black_box(ring.snapshot(&warnings)))
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    targets = enabled_features, log_ring
}
criterion_main!(benches);
