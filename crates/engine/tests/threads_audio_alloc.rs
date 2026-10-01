//! The audio callback's registry hook allocates nothing, from the first callback on,
//! while another thread reads snapshots. A counting global allocator that counts only on
//! the thread that asks, so this test lives alone in its binary.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use engine::threads::{Role, ThreadRegistry, ThreadState};

/// The counting allocator: the only unsafe code of these tests, kept in an `ffi` module
/// as the workspace rule asks.
#[allow(unsafe_code)]
mod ffi {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Counting;

    thread_local! {
        /// Allocations on this thread while counting, or `None` when not counting. A
        /// `const` thread-local of a `Copy` type needs no allocation or destructor.
        pub static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
    }

    fn count() {
        let _ = ALLOCATIONS.try_with(|n| {
            if let Some(count) = n.get() {
                n.set(Some(count + 1));
            }
        });
    }

    // SAFETY: forwards to the system allocator unchanged; only counts calls.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count();
            // SAFETY: same contract as the caller's.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: same contract as the caller's.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count();
            // SAFETY: same contract as the caller's.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;
}

use ffi::ALLOCATIONS;

const CALLBACKS: u64 = 20_000;

/// A plain thread stands in for the backend's callback thread: it was not spawned
/// through the registry, and it counts its own allocations from before its first
/// callback to after it drops the last clone of the hook. Meanwhile the main thread
/// takes snapshots, which must always see a consistent entry.
#[test]
fn the_audio_callback_hook_does_not_allocate() {
    let registry = ThreadRegistry::new();
    let hook = registry.register_audio_callback();
    let listed = registry.snapshot();
    assert_eq!(listed.len(), 1);
    assert_eq!(&*listed[0].name, "glass-audio");
    assert_eq!(listed[0].role, Role::AudioCallback);
    assert_eq!(listed[0].state, ThreadState::Starting);
    assert_eq!(listed[0].os_id, None);

    let started = Arc::new(Barrier::new(2));
    let running = Arc::new(AtomicBool::new(true));
    let callback = thread::spawn({
        let started = started.clone();
        let running = running.clone();
        let hook = hook.clone();
        move || {
            let mut samples = [0.0f32; 512];
            started.wait();
            ALLOCATIONS.set(Some(0));
            for n in 0..CALLBACKS {
                hook.run(|| {
                    // A callback body: fill a block.
                    for (i, sample) in samples.iter_mut().enumerate() {
                        *sample = ((n as usize + i) % 64) as f32 / 64.0;
                    }
                });
            }
            let id = hook.id();
            drop(hook);
            let allocations = ALLOCATIONS.get();
            ALLOCATIONS.set(None);
            running.store(false, Ordering::Release);
            (allocations, id, samples[0])
        }
    });

    started.wait();
    let mut snapshots = 0;
    let mut last_wakeups = 0;
    while running.load(Ordering::Acquire) {
        if let Some(info) = registry.snapshot().first() {
            // Totals never go backwards and the state is always a callback state.
            assert!(info.wakeups >= last_wakeups);
            last_wakeups = info.wakeups;
            assert!(matches!(
                info.state,
                ThreadState::Starting | ThreadState::Busy | ThreadState::Idle | ThreadState::Exited
            ));
            snapshots += 1;
        }
        thread::yield_now();
    }
    let (allocations, id, _) = callback.join().expect("callback thread");
    assert_eq!(
        allocations,
        Some(0),
        "the hook allocated on the callback thread"
    );
    assert!(snapshots > 0);

    // The main thread still holds a clone: the entry stays, with the callback's totals.
    let listed = registry.snapshot();
    assert_eq!(listed.len(), 1);
    let info = &listed[0];
    assert_eq!(info.id, id);
    assert_eq!(info.wakeups, CALLBACKS);
    assert_eq!(info.state, ThreadState::Idle);
    assert!(info.os_id.is_some());
    assert!(info.busy > std::time::Duration::ZERO);
    assert!(info.last_active.is_some());

    // The last clone gone, the next snapshot prunes the entry.
    drop(hook);
    assert!(registry.snapshot().is_empty());
}
