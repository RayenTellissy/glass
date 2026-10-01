//! The frame queues between decode, render and present: order, capacity, flush by
//! generation, waiting and waking, the present policy, and producer/consumer stress.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use engine::frame_queue::{
    FrameStamp, Generation, MpscConsumer, MpscProducer, PopError, PushError, SpscConsumer,
    SpscProducer, Stamped, mpsc, spsc,
};
use media::{Rational, Timestamp};

const RATE: Rational = Rational::new(1, 24);

fn frame(generation: u64, index: i64) -> Stamped<u64> {
    Stamped::new(
        FrameStamp {
            generation,
            index,
            pts: Timestamp::new(index, RATE),
        },
        index as u64 * 10,
    )
}

/// The same checks run against both shapes through this small adapter.
trait Queue {
    fn try_push(&mut self, item: Stamped<u64>) -> Result<(), PushError<u64>>;
    fn try_pop(&mut self) -> Result<Stamped<u64>, PopError>;
}

impl Queue for (SpscProducer<u64>, SpscConsumer<u64>) {
    fn try_push(&mut self, item: Stamped<u64>) -> Result<(), PushError<u64>> {
        self.0.try_push(item)
    }
    fn try_pop(&mut self) -> Result<Stamped<u64>, PopError> {
        self.1.try_pop()
    }
}

impl Queue for (MpscProducer<u64>, MpscConsumer<u64>) {
    fn try_push(&mut self, item: Stamped<u64>) -> Result<(), PushError<u64>> {
        self.0.try_push(item)
    }
    fn try_pop(&mut self) -> Result<Stamped<u64>, PopError> {
        self.1.try_pop()
    }
}

fn both(capacity: usize, generation: &Generation) -> [Box<dyn Queue>; 2] {
    [
        Box::new(spsc::<u64>(capacity, generation)),
        Box::new(mpsc::<u64>(capacity, generation)),
    ]
}

#[test]
fn frames_come_out_in_push_order() {
    let generation = Generation::new();
    for mut queue in both(4, &generation) {
        for round in 0..10 {
            for i in 0..3 {
                queue.try_push(frame(0, round * 3 + i)).expect("push");
            }
            for i in 0..3 {
                let item = queue.try_pop().expect("pop");
                assert_eq!(item.stamp.index, round * 3 + i);
                assert_eq!(item.stamp.pts, Timestamp::new(round * 3 + i, RATE));
                assert_eq!(item.frame, (round * 3 + i) as u64 * 10);
            }
        }
        assert_eq!(queue.try_pop().unwrap_err(), PopError::Empty);
    }
}

#[test]
fn a_full_queue_refuses_and_hands_the_frame_back() {
    let generation = Generation::new();
    for mut queue in both(3, &generation) {
        for i in 0..3 {
            queue.try_push(frame(0, i)).expect("push");
        }
        match queue.try_push(frame(0, 3)) {
            Err(PushError::Full(item)) => assert_eq!(item.stamp.index, 3),
            other => panic!("expected Full, got {other:?}"),
        }
        queue.try_pop().expect("pop");
        queue.try_push(frame(0, 3)).expect("room after a pop");
    }
    let (tx, rx) = spsc::<u64>(5, &generation);
    assert_eq!((tx.capacity(), rx.capacity(), tx.slots()), (5, 5, 5));
    let (tx, rx) = mpsc::<u64>(5, &generation);
    assert_eq!((tx.capacity(), rx.capacity()), (5, 5));
}

#[test]
fn advancing_the_generation_flushes_queued_frames_and_refuses_old_ones() {
    let generation = Generation::new();
    for mut queue in both(8, &generation) {
        let old = generation.current();
        for i in 0..5 {
            queue.try_push(frame(old, i)).expect("push");
        }
        let new = generation.advance();
        assert_eq!(new, old + 1);
        match queue.try_push(frame(old, 5)) {
            Err(PushError::Stale(item)) => assert_eq!(item.stamp.index, 5),
            other => panic!("expected Stale, got {other:?}"),
        }
        queue.try_push(frame(new, 100)).expect("push current");
        let item = queue.try_pop().expect("current frame");
        assert_eq!((item.stamp.generation, item.stamp.index), (new, 100));
        assert_eq!(queue.try_pop().unwrap_err(), PopError::Empty);
    }
}

#[test]
fn consumers_count_the_stale_frames_they_drop() {
    let generation = Generation::new();
    let (mut tx, mut rx) = spsc::<u64>(8, &generation);
    for i in 0..4 {
        tx.try_push(frame(0, i)).unwrap();
    }
    generation.advance();
    tx.try_push(frame(1, 9)).unwrap();
    assert_eq!(rx.len(), 5);
    assert_eq!(rx.drain_stale(), 4);
    assert_eq!(rx.peek().map(|item| item.stamp.index), Some(9));
    assert_eq!(rx.stale_dropped(), 4);

    let (mut tx, mut rx) = mpsc::<u64>(8, &generation);
    for i in 0..4 {
        tx.try_push(frame(1, i)).unwrap();
    }
    generation.advance();
    assert_eq!(rx.try_pop().unwrap_err(), PopError::Empty);
    assert_eq!(rx.stale_dropped(), 4);
}

#[test]
fn the_present_policy_takes_the_latest_due_frame_and_skips_late_ones() {
    let generation = Generation::new();
    let (mut tx, mut rx) = spsc::<u64>(8, &generation);
    for i in 0..6 {
        tx.try_push(frame(0, i)).unwrap();
    }
    // Clock at frame 3: frames 0 to 3 are due, 3 is shown, 0 to 2 were late.
    let due = rx
        .pop_latest_due(|stamp| stamp.index <= 3)
        .expect("due frame");
    assert_eq!((due.frame.stamp.index, due.skipped), (3, 3));
    // Clock still at 3: nothing new is due, the presenter holds frame 3.
    assert!(rx.pop_latest_due(|stamp| stamp.index <= 3).is_none());
    // Clock at 4: exactly one frame, none skipped.
    let due = rx.pop_latest_due(|stamp| stamp.index <= 4).unwrap();
    assert_eq!((due.frame.stamp.index, due.skipped), (4, 0));
    // A seek: frame 5 is stale and never shown, even when due.
    generation.advance();
    assert!(rx.pop_latest_due(|_| true).is_none());
    assert_eq!(rx.stale_dropped(), 1);
    assert!(rx.is_empty());
}

#[test]
fn a_waiting_consumer_wakes_for_a_frame_and_times_out_without_one() {
    let generation = Generation::new();
    let (mut tx, mut rx) = spsc::<u64>(2, &generation);
    let start = Instant::now();
    assert_eq!(
        rx.pop_timeout(Duration::from_millis(30)).unwrap_err(),
        PopError::Timeout
    );
    assert!(start.elapsed() >= Duration::from_millis(30));

    let pusher = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        tx.try_push(frame(0, 7)).unwrap();
        tx
    });
    assert_eq!(rx.pop().unwrap().stamp.index, 7);
    drop(pusher.join().unwrap());
    assert_eq!(rx.pop().unwrap_err(), PopError::Disconnected);
    assert!(rx.is_disconnected());

    let (tx, mut rx) = mpsc::<u64>(2, &generation);
    let tx2 = tx.clone();
    let leaver = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        drop(tx);
        thread::sleep(Duration::from_millis(20));
        drop(tx2);
    });
    // Only the last producer leaving disconnects.
    assert_eq!(rx.pop().unwrap_err(), PopError::Disconnected);
    leaver.join().unwrap();
}

#[test]
fn frames_pushed_just_before_the_last_producer_leaves_are_still_delivered() {
    let generation = Generation::new();
    let (mut tx, mut rx) = mpsc::<u64>(4, &generation);
    tx.try_push(frame(0, 1)).unwrap();
    drop(tx);
    assert_eq!(rx.try_pop().unwrap().stamp.index, 1);
    assert_eq!(rx.try_pop().unwrap_err(), PopError::Disconnected);
}

#[test]
fn a_blocked_producer_waits_for_room_then_pushes() {
    let generation = Generation::new();
    let (mut tx, mut rx) = mpsc::<u64>(1, &generation);
    tx.try_push(frame(0, 0)).unwrap();
    let pushed = Arc::new(AtomicBool::new(false));
    let flag = pushed.clone();
    let producer = thread::spawn(move || {
        tx.push(frame(0, 1)).unwrap();
        flag.store(true, Ordering::SeqCst);
        match tx.push_timeout(frame(0, 2), Duration::from_millis(30)) {
            Err(PushError::Timeout(item)) => assert_eq!(item.stamp.index, 2),
            other => panic!("expected Timeout, got {other:?}"),
        }
    });
    thread::sleep(Duration::from_millis(30));
    assert!(
        !pushed.load(Ordering::SeqCst),
        "push should wait while full"
    );
    assert_eq!(rx.pop().unwrap().stamp.index, 0);
    producer.join().unwrap();
    assert!(pushed.load(Ordering::SeqCst));
    assert_eq!(rx.try_pop().unwrap().stamp.index, 1);
}

#[test]
fn a_seek_releases_producers_blocked_on_a_full_queue() {
    let generation = Generation::new();
    let (mut tx, rx) = spsc::<u64>(1, &generation);
    let (mut mtx, mrx) = mpsc::<u64>(1, &generation);
    tx.try_push(frame(0, 0)).unwrap();
    mtx.try_push(frame(0, 0)).unwrap();
    let mut blocked = Vec::new();
    blocked.push(thread::spawn(move || {
        matches!(tx.push(frame(0, 1)), Err(PushError::Stale(_)))
    }));
    for _ in 0..3 {
        let mut tx = mtx.clone();
        blocked.push(thread::spawn(move || {
            matches!(tx.push(frame(0, 1)), Err(PushError::Stale(_)))
        }));
    }
    thread::sleep(Duration::from_millis(30));
    let start = Instant::now();
    generation.advance();
    for thread in blocked {
        assert!(thread.join().unwrap(), "a blocked push should return Stale");
    }
    assert!(start.elapsed() < Duration::from_secs(1));
    drop((rx, mrx, mtx));
}

#[test]
fn producers_blocked_on_a_full_queue_see_the_consumer_leave() {
    let generation = Generation::new();
    let (mut tx, rx) = mpsc::<u64>(1, &generation);
    tx.try_push(frame(0, 0)).unwrap();
    let producer = thread::spawn(move || {
        let result = tx.push(frame(0, 1));
        (
            matches!(result, Err(PushError::Disconnected(_))),
            tx.is_disconnected(),
        )
    });
    thread::sleep(Duration::from_millis(30));
    drop(rx);
    assert_eq!(producer.join().unwrap(), (true, true));
}

#[test]
fn spsc_stress_keeps_order_across_threads() {
    const COUNT: i64 = 200_000;
    let generation = Generation::new();
    let (mut tx, mut rx) = spsc::<u64>(4, &generation);
    let producer = thread::spawn(move || {
        for i in 0..COUNT {
            tx.push(frame(0, i)).unwrap();
        }
    });
    let mut expected = 0;
    loop {
        match rx.pop() {
            Ok(item) => {
                assert_eq!(item.stamp.index, expected);
                assert_eq!(item.frame, expected as u64 * 10);
                expected += 1;
            }
            Err(PopError::Disconnected) => break,
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
    producer.join().unwrap();
    assert_eq!(expected, COUNT);
}

/// Four decoders feed one consumer while a scheduler thread seeks at random moments.
/// Each producer's frames stay in order (so none arrives twice), generations never go
/// backwards, and every frame pushed in the last generation arrives.
#[test]
fn mpsc_stress_with_seeks_keeps_per_producer_order() {
    const PRODUCERS: usize = 4;
    const PER_PRODUCER: i64 = 50_000;
    const SEEKS: u64 = 40;
    let generation = Generation::new();
    let (tx, mut rx) = mpsc::<u64>(8, &generation);
    let seeking_done = Arc::new(AtomicBool::new(false));

    let mut producers = Vec::new();
    for p in 0..PRODUCERS {
        let mut tx = tx.clone();
        let seeking_done = seeking_done.clone();
        producers.push(thread::spawn(move || {
            let mut last_generation_pushed = 0;
            let mut pushed_in_last = 0u64;
            let mut i = 0;
            // Push PER_PRODUCER frames in the last generation; stale pushes are skipped
            // and the producer moves on with the next index.
            loop {
                let seeks_over = seeking_done.load(Ordering::SeqCst);
                let current = tx.generation();
                let index = i * PRODUCERS as i64 + p as i64;
                match tx.push(frame(current, index)) {
                    Ok(()) => {
                        last_generation_pushed = current;
                        if current == SEEKS {
                            pushed_in_last += 1;
                        }
                    }
                    Err(PushError::Stale(_)) => {}
                    Err(other) => panic!("unexpected {other:?}"),
                }
                i += 1;
                if seeks_over && i >= PER_PRODUCER {
                    break;
                }
            }
            (last_generation_pushed, pushed_in_last)
        }));
    }
    drop(tx);

    let seeker = {
        let generation = generation.clone();
        let seeking_done = seeking_done.clone();
        thread::spawn(move || {
            for _ in 0..SEEKS {
                thread::sleep(Duration::from_micros(500));
                generation.advance();
            }
            seeking_done.store(true, Ordering::SeqCst);
        })
    };

    let mut last_index = [-1i64; PRODUCERS];
    let mut last_generation = [0u64; PRODUCERS];
    let mut delivered_in_last = [0u64; PRODUCERS];
    loop {
        match rx.pop() {
            Ok(item) => {
                let p = (item.stamp.index % PRODUCERS as i64) as usize;
                assert!(
                    item.stamp.index > last_index[p],
                    "producer {p} out of order"
                );
                assert!(item.stamp.generation >= last_generation[p]);
                last_index[p] = item.stamp.index;
                last_generation[p] = item.stamp.generation;
                if item.stamp.generation == SEEKS {
                    delivered_in_last[p] += 1;
                }
            }
            Err(PopError::Disconnected) => break,
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
    seeker.join().unwrap();
    for (p, producer) in producers.into_iter().enumerate() {
        let (last, pushed_in_last) = producer.join().unwrap();
        assert_eq!(last, SEEKS);
        assert!(pushed_in_last > 0);
        assert_eq!(delivered_in_last[p], pushed_in_last, "producer {p}");
    }
    assert_eq!(generation.current(), SEEKS);
    assert!(last_generation.iter().all(|&g| g == SEEKS));
}
