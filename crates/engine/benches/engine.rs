//! Engine micro-benchmarks. Run with `cargo bench -p engine --bench "*"`; see
//! CONTRIBUTING.md for short local runs and baselines.

use std::hint::black_box;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use engine::frame_queue::{FrameStamp, Generation, Stamped, mpsc, spsc};
use engine::logging::{DEFAULT_RING_CAPACITY, LogFilter, LogRing, RingLayer};
use media::{Rational, Timestamp};
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

fn stamped(index: i64) -> Stamped<u64> {
    let pts = Timestamp::new(index, Rational::new(1, 24));
    Stamped::new(
        FrameStamp {
            generation: 0,
            index,
            pts,
        },
        index as u64,
    )
}

/// Frame queues: one push plus one pop on the same thread (the uncontended cost of an
/// operation pair), and frames per second through a 4-slot queue with the producer on
/// another thread and both sides parking when they must wait.
fn frame_queue(c: &mut Criterion) {
    let generation = Generation::new();
    let mut group = c.benchmark_group("frame_queue");
    let (mut tx, mut rx) = spsc::<u64>(8, &generation);
    group.bench_function("spsc_push_pop", |b| {
        b.iter(|| {
            tx.try_push(stamped(black_box(1))).unwrap();
            black_box(rx.try_pop().unwrap())
        })
    });
    let (mut tx, mut rx) = mpsc::<u64>(8, &generation);
    group.bench_function("mpsc_push_pop", |b| {
        b.iter(|| {
            tx.try_push(stamped(black_box(1))).unwrap();
            black_box(rx.try_pop().unwrap())
        })
    });
    group.bench_function("spsc_cross_thread", |b| {
        b.iter_custom(|frames| {
            let (mut tx, mut rx) = spsc::<u64>(4, &generation);
            let start = Instant::now();
            let producer = thread::spawn(move || {
                for i in 0..frames as i64 {
                    tx.push(stamped(i)).unwrap();
                }
            });
            while rx.pop().is_ok() {}
            producer.join().unwrap();
            start.elapsed()
        })
    });
    group.bench_function("mpsc_cross_thread_2_producers", |b| {
        b.iter_custom(|frames| {
            let (tx, mut rx) = mpsc::<u64>(4, &generation);
            let start = Instant::now();
            let producers: Vec<_> = (0..2)
                .map(|p| {
                    let mut tx = tx.clone();
                    let count = frames / 2 + (frames % 2) * (1 - p);
                    thread::spawn(move || {
                        for i in 0..count as i64 {
                            tx.push(stamped(i)).unwrap();
                        }
                    })
                })
                .collect();
            drop(tx);
            while rx.pop().is_ok() {}
            for producer in producers {
                producer.join().unwrap();
            }
            start.elapsed()
        })
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    targets = enabled_features, log_ring, frame_queue
}
criterion_main!(benches);
