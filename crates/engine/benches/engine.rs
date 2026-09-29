//! Engine micro-benchmarks. Run with `cargo bench -p engine`; see CONTRIBUTING.md for
//! short local runs and baselines.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};

fn enabled_features(c: &mut Criterion) {
    c.bench_function("enabled_features", |b| {
        b.iter(|| black_box(engine::enabled_features()))
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    targets = enabled_features
}
criterion_main!(benches);
