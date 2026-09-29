//! Times three of egui-wgpu's profiling scopes on the UI thread.
//!
//! egui-wgpu 0.36 adds the time spent in `get_current_texture`, `Queue::submit`
//! and `present` to its `vsync_sec`, and eframe subtracts that from the frame
//! time it reports as `cpu_usage`. Acquire and present are mostly waits, but
//! `Queue::submit` is real CPU work on the UI thread (command encoding for
//! Metal), so the judged metric adds it back. With `profiling`'s tracing
//! backend each scope is a tracing span; this subscriber enables only those
//! three spans from the `egui_wgpu` crate and ignores everything else.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};

#[derive(Clone, Copy)]
pub enum Kind {
    Acquire = 0,
    Submit = 1,
    Present = 2,
}

static TOTAL_NS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

thread_local! {
    static STARTED: [Cell<Option<Instant>>; 3] = const { [Cell::new(None), Cell::new(None), Cell::new(None)] };
}

fn kind(meta: &Metadata<'_>) -> Option<Kind> {
    if !meta.is_span()
        || !meta
            .module_path()
            .is_some_and(|m| m.starts_with("egui_wgpu"))
    {
        return None;
    }
    match meta.name() {
        "get_current_texture" => Some(Kind::Acquire),
        "Queue::submit" => Some(Kind::Submit),
        "present" => Some(Kind::Present),
        _ => None,
    }
}

fn kind_of_id(id: &Id) -> Option<usize> {
    let k = id.into_u64().checked_sub(1)? as usize;
    (k < 3).then_some(k)
}

struct SpanTimer;

impl Subscriber for SpanTimer {
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        if kind(meta).is_some() {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        kind(meta).is_some()
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        // Only enabled callsites reach here; the id encodes which one.
        let k = kind(span.metadata()).map_or(3, |k| k as u64);
        Id::from_u64(k + 1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, _: &Event<'_>) {}

    fn enter(&self, id: &Id) {
        if let Some(k) = kind_of_id(id) {
            STARTED.with(|s| s[k].set(Some(Instant::now())));
        }
    }

    fn exit(&self, id: &Id) {
        if let Some(k) = kind_of_id(id)
            && let Some(start) = STARTED.with(|s| s[k].take())
        {
            TOTAL_NS[k].fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
    }
}

pub fn install() -> bool {
    tracing::subscriber::set_global_default(SpanTimer).is_ok()
}

/// Returns the time spent in each scope since the last call, in milliseconds.
pub fn take_ms(k: Kind) -> f32 {
    TOTAL_NS[k as usize].swap(0, Ordering::Relaxed) as f32 / 1e6
}
