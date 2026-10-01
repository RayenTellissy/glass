//! Push, pop, waiting, waking and flushing allocate nothing once a frame queue is warm.
//! A counting global allocator, so this test lives alone in its binary.

use std::sync::atomic::Ordering;
use std::thread;

use engine::frame_queue::{FrameStamp, Generation, PopError, PushError, Stamped, mpsc, spsc};
use media::{Rational, Timestamp};

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

const RATE: Rational = Rational::new(1, 24);
const WARM_UP: i64 = 1_000;
const COUNTED: i64 = 20_000;

fn frame(generation: u64, index: i64) -> Stamped<[u64; 4]> {
    Stamped::new(
        FrameStamp {
            generation,
            index,
            pts: Timestamp::new(index, RATE),
        },
        [index as u64; 4],
    )
}

/// One test so nothing else runs while counting. Both shapes run a producer thread that
/// blocks on a small queue (so both sides park and wake many times), with a seek halfway
/// through the counted part; the main thread pops. Counting starts after the warm-up
/// frames and covers both threads.
#[test]
fn queue_operations_do_not_allocate_after_warm_up() {
    let generation = Generation::new();

    // Single thread first: try_push, try_pop, the present policy, advance.
    let (mut tx, mut rx) = spsc::<[u64; 4]>(4, &generation);
    let (mut mtx, mut mrx) = mpsc::<[u64; 4]>(4, &generation);
    let mut round = |counted: bool| {
        for i in 0..if counted { COUNTED } else { WARM_UP } {
            let current = generation.current();
            tx.try_push(frame(current, i)).unwrap();
            mtx.try_push(frame(current, i)).unwrap();
            assert_eq!(rx.try_pop().unwrap().stamp.index, i);
            assert_eq!(mrx.try_pop().unwrap().stamp.index, i);
            tx.try_push(frame(current, i)).unwrap();
            assert!(rx.pop_latest_due(|stamp| stamp.index <= i).is_some());
            if i % 100 == 0 {
                tx.try_push(frame(current, i)).unwrap();
                generation.advance();
                assert!(matches!(
                    tx.try_push(frame(current, i)),
                    Err(PushError::Stale(_))
                ));
                assert_eq!(rx.try_pop().unwrap_err(), PopError::Empty);
            }
        }
    };
    round(false);
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    round(true);
    COUNTING.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "single-thread operations allocated"
    );

    // Across threads, with blocking push and pop.
    let (mut tx, mut rx) = spsc::<[u64; 4]>(2, &generation);
    let (mtx, mut mrx) = mpsc::<[u64; 4]>(2, &generation);
    let mut mtx2 = mtx.clone();
    let start = generation.current();
    let producer = thread::spawn(move || {
        let mut mtx = mtx;
        for i in 0..WARM_UP + COUNTED {
            let current = generation.current();
            match tx.push(frame(current, i)) {
                Ok(()) | Err(PushError::Stale(_)) => {}
                Err(other) => panic!("unexpected {other:?}"),
            }
            let target = if i % 2 == 0 { &mut mtx } else { &mut mtx2 };
            match target.push(frame(current, i)) {
                Ok(()) | Err(PushError::Stale(_)) => {}
                Err(other) => panic!("unexpected {other:?}"),
            }
            if i == WARM_UP + COUNTED / 2 {
                generation.advance();
            }
        }
        generation
    });
    // Counting stops at the last frame, before the producer thread exits.
    let last = WARM_UP + COUNTED - 1;
    let mut popped = 0i64;
    let mut done = [false; 2];
    while done != [true, true] {
        if popped == WARM_UP {
            ALLOCATIONS.store(0, Ordering::Relaxed);
            COUNTING.store(true, Ordering::Relaxed);
        }
        if !done[0] {
            let item = rx.pop().expect("spsc frame");
            assert!(item.stamp.generation >= start);
            popped += 1;
            done[0] = item.stamp.index == last;
        }
        if !done[1] {
            done[1] = mrx.pop().expect("mpsc frame").stamp.index == last;
        }
    }
    COUNTING.store(false, Ordering::Relaxed);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(rx.pop().unwrap_err(), PopError::Disconnected);
    assert_eq!(mrx.pop().unwrap_err(), PopError::Disconnected);
    let generation = producer.join().unwrap();
    assert!(popped > WARM_UP + COUNTED / 2, "popped only {popped}");
    assert_eq!(generation.current(), start + 1);
    assert_eq!(allocations, 0, "cross-thread operations allocated");
}
