//! The callback path allocates nothing: copying from the ring, underruns, channel mapping,
//! drift correction and publishing the clock, plus reading the clock and feeding the ring.
//! A counting global allocator, so this test lives alone in its binary.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use audio::{OfflineOutput, OutputConfig};

/// The counting allocator: the only unsafe code of these tests, kept in an `ffi` module
/// as the workspace rule asks.
#[allow(unsafe_code)]
mod ffi {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    pub struct Counting;

    pub static COUNTING: AtomicBool = AtomicBool::new(false);
    pub static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

    // SAFETY: forwards to the system allocator unchanged; only counts calls.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if COUNTING.load(Ordering::Relaxed) {
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
            // SAFETY: same contract as the caller's.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: same contract as the caller's.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if COUNTING.load(Ordering::Relaxed) {
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
            // SAFETY: same contract as the caller's.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;
}

use ffi::{ALLOCATIONS, COUNTING};

#[test]
fn the_callback_path_does_not_allocate() {
    let config = OutputConfig {
        sample_rate: Some(48_000),
        channels: 2,
        ring_frames: 4096,
        buffer_frames: None,
    };
    let (mut timed, mut timed_feeder, timed_clock) = OfflineOutput::with_device_timing(&config);
    let (mut manual, mut manual_feeder, manual_clock) = OfflineOutput::new(&config);
    let left = vec![0.1f32; 700];
    let right = vec![0.2f32; 700];
    let interleaved = vec![0.3f32; 2 * 300];
    let mut out = vec![0.0f32; 2 * 512];
    let base = Instant::now();
    let latency = Duration::from_millis(12);

    // Everything is allocated up front; nothing needs warming.
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut sum = 0.0;
    for i in 0..2_000u32 {
        let now = base + Duration::from_micros(u64::from(i) * 10_667);
        // Every third callback finds the ring short: underruns take the same path.
        if i % 3 != 0 {
            timed_feeder.push_planar(&[&left, &right]);
            manual_feeder.push_interleaved(&interleaved);
        }
        timed.render_at(&mut out, now, latency);
        manual.render(&mut out);
        manual.advance(700);
        sum += timed_clock.at(now) + timed_clock.now() + manual_clock.now();
        sum += timed_clock.rate_ratio() + timed_clock.underruns() as f64;
    }
    COUNTING.store(false, Ordering::Relaxed);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    assert!(sum > 0.0);
    assert!(timed_clock.underruns() > 0 && manual_clock.underruns() > 0);
    assert_eq!(
        allocations, 0,
        "{allocations} allocations in 2000 callbacks"
    );
}
