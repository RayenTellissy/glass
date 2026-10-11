//! The mixing callback allocates nothing: a 16-track graph with buses, sends and
//! automation, rendered through `OfflineOutput::with_mixer` at a 256-frame buffer, with
//! setting changes, graph swaps (an edit and a seek), pausing and feeds running dry. A
//! counting global allocator, so this test lives alone in its binary.

use std::sync::atomic::Ordering;

use audio::OfflineOutput;
use audio::mixer::{
    Feed, Graph, GraphSpec, Lane, Layout, MasterSpec, Mixer, MixerConfig, Param, Point, Route,
    SendSpec, StripRef, StripSpec, SwapAt,
};

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
            if COUNTING.load(Ordering::Relaxed) {
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
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

const TRACKS: usize = 16;
const BUFFER: usize = 256;

fn spec() -> GraphSpec {
    let ramp = Lane::new(vec![
        Point::linear(0, -24.0),
        Point::linear(48_000, 0.0),
        Point::hold(96_000, -3.0),
    ])
    .unwrap();
    let tracks = (0..TRACKS as u64)
        .map(|id| StripSpec {
            fader_db: -6.0,
            pan: (id as f64 / 8.0) - 1.0,
            output: if id % 4 == 0 {
                Route::Bus(1)
            } else {
                Route::Master
            },
            sends: vec![SendSpec {
                bus: 0,
                level_db: -10.0,
                pre_fader: id % 2 == 0,
            }],
            fader_lane: (id % 3 == 0).then(|| ramp.clone()),
            ..StripSpec::new(id)
        })
        .collect();
    let buses = vec![
        StripSpec {
            output: Route::Bus(1),
            ..StripSpec::new(100)
        },
        StripSpec::new(101),
    ];
    GraphSpec {
        sample_rate: 48_000,
        layout: Layout::Stereo,
        max_block: 1024,
        tracks,
        buses,
        master: MasterSpec {
            fader_db: -1.0,
            fader_lane: None,
            soft_clip: true,
        },
    }
}

#[test]
fn the_mixing_callback_does_not_allocate() {
    let (mixer, mut control) = Mixer::new(MixerConfig::default());
    let status = control.status().clone();
    let (mut output, clock) = OfflineOutput::with_mixer(mixer);
    let graph = Graph::build(&spec()).unwrap();
    let meters = graph.meters();
    let width = graph.width();
    let (mut feed, consumer) = Feed::new(width, 8192, 0);
    control.swap(graph, consumer, SwapAt::Now).unwrap();
    control.play().unwrap();
    let chunk: Vec<f32> = (0..BUFFER * width)
        .map(|i| ((i % 97) as f32 / 97.0 - 0.5) * 0.2)
        .collect();
    let mut out = vec![0.0f32; BUFFER * 2];

    let mut allocations = 0;
    let mut sum = 0.0f32;
    for i in 0..3_000 {
        // Off the callback: feed (every seventh buffer the feed misses), commands, swaps.
        if i % 7 != 0 {
            feed.push(&chunk);
        }
        if i % 50 == 0 {
            control
                .set(
                    StripRef::Track(i as u64 % 16),
                    Param::Fader(-3.0 - (i % 5) as f64),
                )
                .unwrap();
            control
                .set(StripRef::Bus(100), Param::Mute(i % 100 == 0))
                .unwrap();
            control
                .set(StripRef::Track(3), Param::Solo(i % 200 == 0))
                .unwrap();
        }
        if i == 1000 || i == 2000 {
            let graph = Graph::build(&spec()).unwrap();
            let at = if i == 1000 {
                SwapAt::Continue
            } else {
                SwapAt::Now
            };
            let start = if i == 1000 { feed.position() } else { 500_000 };
            let (new_feed, consumer) = Feed::new(width, 8192, start);
            control.swap(graph, consumer, at).unwrap();
            feed = new_feed;
            feed.push(&chunk);
        }
        if i == 2500 {
            control.pause().unwrap();
        }
        if i == 2600 {
            control.play().unwrap();
        }

        ALLOCATIONS.store(0, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Relaxed);
        output.render(&mut out);
        COUNTING.store(false, Ordering::Relaxed);
        allocations += ALLOCATIONS.load(Ordering::Relaxed);
        sum += out[0].abs();
    }
    control.collect();
    assert!(sum > 0.0);
    assert!(
        status.underruns() > 0,
        "the dry feeds took the underrun path"
    );
    assert!(clock.underruns() > 0);
    assert_eq!(status.generation(), 3);
    assert!(meters.publications() > 0);
    assert_eq!(status.leaked(), 0);
    assert_eq!(
        allocations, 0,
        "{allocations} allocations or frees in 3000 callbacks"
    );
}
