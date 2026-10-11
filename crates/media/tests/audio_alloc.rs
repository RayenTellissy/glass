//! `AudioReader::read` allocates nothing on the Rust heap once warmed up (FFmpeg's own
//! frame pool is outside Rust's allocator and reuses its buffers). A counting global
//! allocator, so this test lives alone in its binary.

mod common;

use std::sync::atomic::Ordering;

use media::{AudioOptions, AudioReader, ChannelLayout};

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
fn reads_do_not_allocate_after_warm_up() {
    for (file, rate) in [
        ("h264_1080p30.mp4", 44_100),
        ("h264_1080p30.mp4", 48_000),
        ("sync_vp9_opus.webm", 44_100),
        ("audio_44k1_mono.mp3", 48_000),
        ("audio_51_48k.wav", 44_100),
    ] {
        let Some(path) = common::fixture(file) else {
            continue;
        };
        let options = AudioOptions {
            sample_rate: rate,
            layout: ChannelLayout::Stereo,
            ..AudioOptions::default()
        };
        let mut reader = AudioReader::open(&path, options).expect("open");
        let mut left = vec![0.0f32; 512];
        let mut right = vec![0.0f32; 512];
        // Warm-up: the first chunks size the frame buffers.
        for _ in 0..8 {
            reader.read(&mut [&mut left, &mut right]).expect("read");
        }
        ALLOCATIONS.store(0, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Relaxed);
        let mut frames = 0;
        for _ in 0..60 {
            frames += reader.read(&mut [&mut left, &mut right]).expect("read");
        }
        COUNTING.store(false, Ordering::Relaxed);
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        eprintln!("{file} at {rate} Hz: {frames} frames read, {allocations} allocations");
        assert_eq!(allocations, 0, "{file} at {rate} Hz");
    }
}
