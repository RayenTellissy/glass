//! Bounded lock-free frame queues between decode, render and present.
//!
//! Two shapes, one behaviour:
//!
//! - [`spsc`]: one producer, one consumer, on an `rtrb` ring. Used for render → present
//!   (the render thread is the only producer) and for any single decoder feeding render.
//!   The consumer can [`peek`](SpscConsumer::peek) and pick the latest due frame
//!   ([`SpscConsumer::pop_latest_due`]).
//! - [`mpsc`]: many producers, one consumer, on crossbeam's bounded `ArrayQueue`. Used
//!   for decode → render, where each `glass-decode-<n>` worker holds a clone of the
//!   producer. Order is kept per producer, not across producers: the consumer matches
//!   frames to render requests by their [`FrameStamp`].
//!
//! Every item is a [`Stamped`] frame: a [`FrameStamp`] (generation, sequence frame
//! index, presentation time) plus the frame itself.
//!
//! **Generations and flush.** A queue is tied to a shared [`Generation`] counter, one per
//! playback pipeline. A seek or an edit calls [`Generation::advance`]; from then on
//! producers get [`PushError::Stale`] for frames stamped with an older generation, and
//! the consumer drops older frames as it meets them (on its own thread, counted in
//! `stale_dropped`). Push and pop take no lock; `advance` wakes producers blocked on a
//! full queue so they return `Stale` at once instead of waiting for space.
//!
//! **Back-pressure.**
//! - Decode → render: decoders call [`MpscProducer::push`], which parks while the queue
//!   is full. A full queue means look-ahead is deep enough; the decoder sleeps until
//!   render takes a frame, the generation moves on (it returns `Stale`) or render goes
//!   away (`Disconnected`). Render never waits for decode except in
//!   [`pop`](MpscConsumer::pop)/[`pop_timeout`](MpscConsumer::pop_timeout).
//! - Render → present: render calls [`SpscProducer::try_push`] only, never a blocking
//!   push, because render must not wait on the UI. On `Full` it keeps the frame and
//!   offers it again on its next tick. The presenter calls
//!   [`SpscConsumer::pop_latest_due`] once per display refresh: every frame due by the
//!   clock is taken, only the latest is returned and the earlier ones are dropped as
//!   late (`skipped`). When nothing is due the presenter keeps showing the frame it
//!   already holds.
//!
//! **Waiting.** A side that waits parks its thread (a [`Parker`] on a std mutex and
//! condition variable), it never spins:
//! it announces that it is waiting, checks the queue once more and sleeps; the other side
//! looks at that flag after each push or pop (behind a `SeqCst` fence on both sides) and
//! unparks it. Parking and unparking are the only slow path; the producers' unparkers sit
//! in a short list behind a mutex that is locked only when a producer is waiting.
//!
//! **Allocation.** Creating a queue or a producer allocates, and initialises its parker's
//! mutex and condition variable (the standard library allocates them on first use); after
//! that, push, pop, waiting, waking and `advance` allocate nothing
//! (`tests/frame_queue_alloc.rs`).

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use crossbeam_queue::ArrayQueue;
use media::{HwFrame, Timestamp, VideoFrame};
use render::wgpu;

/// Where a frame belongs: the pipeline generation it was made for, its frame index on the
/// sequence, and its presentation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameStamp {
    pub generation: u64,
    pub index: i64,
    pub pts: Timestamp,
}

/// A frame and its stamp: what the queues carry.
#[derive(Clone, Debug)]
pub struct Stamped<T> {
    pub stamp: FrameStamp,
    pub frame: T,
}

impl<T> Stamped<T> {
    pub fn new(stamp: FrameStamp, frame: T) -> Self {
        Self { stamp, frame }
    }
}

/// A decoded video frame on its way from the decode pool to render: CPU planes, or a
/// hardware surface with no CPU copy.
#[derive(Debug)]
pub enum DecodedFrame {
    Cpu(VideoFrame),
    Hw(HwFrame),
}

/// A rendered frame on its way from render to present.
pub type RenderedFrame = wgpu::Texture;

/// The decode pool's side of the decode → render queue (clone one per decoder).
pub type DecodeSender = MpscProducer<DecodedFrame>;
/// Render's side of the decode → render queue.
pub type DecodeReceiver = MpscConsumer<DecodedFrame>;
/// Render's side of the render → present queue.
pub type PresentSender = SpscProducer<RenderedFrame>;
/// The presenter's side of the render → present queue.
pub type PresentReceiver = SpscConsumer<RenderedFrame>;

/// The decode → render queue.
pub fn decode_queue(capacity: usize, generation: &Generation) -> (DecodeSender, DecodeReceiver) {
    mpsc(capacity, generation)
}

/// The render → present queue.
pub fn present_queue(capacity: usize, generation: &Generation) -> (PresentSender, PresentReceiver) {
    spsc(capacity, generation)
}

/// The shared generation counter of one playback pipeline. Cheap to clone; every clone
/// is the same counter.
#[derive(Clone)]
pub struct Generation(Arc<GenerationInner>);

struct GenerationInner {
    value: AtomicU64,
    /// The queues to wake on `advance`. Locked only by `advance` and queue creation.
    queues: Mutex<Vec<Weak<Shared>>>,
}

impl Generation {
    /// A counter starting at generation 0.
    pub fn new() -> Self {
        Self(Arc::new(GenerationInner {
            value: AtomicU64::new(0),
            queues: Mutex::new(Vec::new()),
        }))
    }

    /// The current generation. Frames stamped with an older one are stale.
    pub fn current(&self) -> u64 {
        self.0.value.load(Ordering::Acquire)
    }

    /// Starts a new generation (a seek or an edit) and returns it. Frames of earlier
    /// generations are refused by producers and dropped by consumers from now on, and
    /// producers blocked on a full queue wake up with [`PushError::Stale`].
    pub fn advance(&self) -> u64 {
        let new = self.0.value.fetch_add(1, Ordering::SeqCst) + 1;
        lock(&self.0.queues).retain(|queue| match queue.upgrade() {
            Some(shared) => {
                shared.wake_producers();
                true
            }
            None => false,
        });
        new
    }

    fn attach(&self, shared: &Arc<Shared>) {
        let mut queues = lock(&self.0.queues);
        queues.retain(|queue| queue.strong_count() > 0);
        queues.push(Arc::downgrade(shared));
    }
}

impl Default for Generation {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Generation").field(&self.current()).finish()
    }
}

/// Why a push did not take the frame; every variant hands the frame back.
pub enum PushError<T> {
    /// The queue is full (`try_push` only).
    Full(Stamped<T>),
    /// The frame's generation is older than the current one: drop it and move on.
    Stale(Stamped<T>),
    /// The consumer is gone.
    Disconnected(Stamped<T>),
    /// The queue stayed full until the deadline (`push_timeout` only).
    Timeout(Stamped<T>),
}

impl<T> PushError<T> {
    pub fn into_inner(self) -> Stamped<T> {
        match self {
            Self::Full(item)
            | Self::Stale(item)
            | Self::Disconnected(item)
            | Self::Timeout(item) => item,
        }
    }
}

impl<T> fmt::Debug for PushError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, item) = match self {
            Self::Full(item) => ("Full", item),
            Self::Stale(item) => ("Stale", item),
            Self::Disconnected(item) => ("Disconnected", item),
            Self::Timeout(item) => ("Timeout", item),
        };
        f.debug_tuple(name).field(&item.stamp).finish()
    }
}

/// Why a pop returned no frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopError {
    /// Nothing is queued (`try_pop` only).
    Empty,
    /// Nothing arrived before the deadline (`pop_timeout` only).
    Timeout,
    /// Every producer is gone and the queue is empty.
    Disconnected,
}

/// What [`SpscConsumer::pop_latest_due`] returns: the latest due frame and how many
/// earlier due frames were dropped for it.
#[derive(Debug)]
pub struct Due<T> {
    pub frame: Stamped<T>,
    pub skipped: usize,
}

/// The waiting and liveness state both sides of a queue share. Independent of the frame
/// type, so a [`Generation`] can hold queues of any type.
struct Shared {
    generation: Generation,
    consumer: Unparker,
    consumer_waiting: AtomicBool,
    consumer_alive: AtomicBool,
    producers: AtomicUsize,
    producers_waiting: AtomicUsize,
    next_producer: AtomicU64,
    /// Each live producer's unparker, by producer id.
    producer_unparkers: Mutex<Vec<(u64, Unparker)>>,
}

impl Shared {
    fn new(generation: &Generation, consumer: Unparker) -> Arc<Self> {
        let shared = Arc::new(Self {
            generation: generation.clone(),
            consumer,
            consumer_waiting: AtomicBool::new(false),
            consumer_alive: AtomicBool::new(true),
            producers: AtomicUsize::new(0),
            producers_waiting: AtomicUsize::new(0),
            next_producer: AtomicU64::new(0),
            producer_unparkers: Mutex::new(Vec::new()),
        });
        generation.attach(&shared);
        shared
    }

    fn wake_producers(&self) {
        for (_, unparker) in lock(&self.producer_unparkers).iter() {
            unparker.unpark();
        }
    }

    /// After a successful push: wake the consumer if it is waiting. The fence pairs with
    /// the one in `ConsumerSide::pop_with`, so either the consumer sees the frame or this
    /// sees the flag.
    fn after_push(&self) {
        fence(Ordering::SeqCst);
        if self.consumer_waiting.load(Ordering::Relaxed) {
            self.consumer.unpark();
        }
    }

    /// After a pop freed a slot: wake producers waiting for space (pairs with the fence
    /// in `ProducerSide::push_with`).
    fn after_pop(&self) {
        fence(Ordering::SeqCst);
        if self.producers_waiting.load(Ordering::Relaxed) > 0 {
            self.wake_producers();
        }
    }
}

/// Puts one thread to sleep until its [`Unparker`] is called: a wake-up token behind a
/// mutex and condition variable. An `unpark` before `park` is kept, so the next `park`
/// returns at once.
///
/// The standard library allocates a mutex and a condition variable on their first use
/// (on macOS), so `new` uses both once and later waits and wakes allocate nothing.
struct Parker {
    inner: Arc<ParkerInner>,
}

struct ParkerInner {
    notified: Mutex<bool>,
    wake: Condvar,
}

#[derive(Clone)]
struct Unparker {
    inner: Arc<ParkerInner>,
}

impl Parker {
    fn new() -> Self {
        let inner = Arc::new(ParkerInner {
            notified: Mutex::new(false),
            wake: Condvar::new(),
        });
        let notified = lock(&inner.notified);
        drop(
            inner
                .wake
                .wait_timeout(notified, Duration::ZERO)
                .unwrap_or_else(PoisonError::into_inner),
        );
        inner.wake.notify_one();
        Self { inner }
    }

    fn unparker(&self) -> Unparker {
        Unparker {
            inner: self.inner.clone(),
        }
    }

    /// Sleeps until unparked, or returns at once if an unpark is pending.
    fn park(&self) {
        let mut notified = lock(&self.inner.notified);
        while !*notified {
            notified = self
                .inner
                .wake
                .wait(notified)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *notified = false;
    }

    /// [`park`](Self::park) that also returns at `deadline`.
    fn park_deadline(&self, deadline: Instant) {
        let mut notified = lock(&self.inner.notified);
        while !*notified {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            notified = self
                .inner
                .wake
                .wait_timeout(notified, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *notified = false;
    }
}

impl Unparker {
    /// Wakes the parked thread, or makes its next `park` return at once.
    fn unpark(&self) {
        *lock(&self.inner.notified) = true;
        self.inner.wake.notify_one();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The ring-independent half of a producer.
struct ProducerSide {
    shared: Arc<Shared>,
    parker: Parker,
    id: u64,
}

impl ProducerSide {
    fn new(shared: Arc<Shared>) -> Self {
        let parker = Parker::new();
        let id = shared.next_producer.fetch_add(1, Ordering::Relaxed);
        lock(&shared.producer_unparkers).push((id, parker.unparker()));
        shared.producers.fetch_add(1, Ordering::AcqRel);
        Self { shared, parker, id }
    }

    fn try_push_with<T>(
        &self,
        item: Stamped<T>,
        raw: &mut impl FnMut(Stamped<T>) -> Result<(), Stamped<T>>,
    ) -> Result<(), PushError<T>> {
        if !self.shared.consumer_alive.load(Ordering::Acquire) {
            return Err(PushError::Disconnected(item));
        }
        if item.stamp.generation < self.shared.generation.current() {
            return Err(PushError::Stale(item));
        }
        match raw(item) {
            Ok(()) => {
                self.shared.after_push();
                Ok(())
            }
            Err(item) => Err(PushError::Full(item)),
        }
    }

    fn push_with<T>(
        &self,
        mut item: Stamped<T>,
        deadline: Option<Instant>,
        raw: &mut impl FnMut(Stamped<T>) -> Result<(), Stamped<T>>,
    ) -> Result<(), PushError<T>> {
        loop {
            match self.try_push_with(item, raw) {
                Err(PushError::Full(back)) => item = back,
                other => return other,
            }
            // Announce the wait, then look again before sleeping, so a pop between the
            // failed push and the park is not missed.
            self.shared
                .producers_waiting
                .fetch_add(1, Ordering::Relaxed);
            fence(Ordering::SeqCst);
            let outcome = match self.try_push_with(item, raw) {
                Err(PushError::Full(back)) => match deadline {
                    None => {
                        self.parker.park();
                        Err(back)
                    }
                    Some(deadline) if Instant::now() < deadline => {
                        self.parker.park_deadline(deadline);
                        Err(back)
                    }
                    Some(_) => Ok(Err(PushError::Timeout(back))),
                },
                other => Ok(other),
            };
            self.shared
                .producers_waiting
                .fetch_sub(1, Ordering::Relaxed);
            match outcome {
                Ok(result) => return result,
                Err(back) => item = back,
            }
        }
    }
}

impl Drop for ProducerSide {
    fn drop(&mut self) {
        lock(&self.shared.producer_unparkers).retain(|(id, _)| *id != self.id);
        if self.shared.producers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.consumer.unpark();
        }
    }
}

/// The ring-independent half of a consumer.
struct ConsumerSide {
    shared: Arc<Shared>,
    parker: Parker,
    stale_dropped: u64,
}

impl ConsumerSide {
    fn is_stale(&self, stamp: &FrameStamp) -> bool {
        stamp.generation < self.shared.generation.current()
    }

    fn try_pop_with<T>(
        &mut self,
        raw: &mut impl FnMut() -> Option<Stamped<T>>,
    ) -> Result<Stamped<T>, PopError> {
        loop {
            let item = match raw() {
                Some(item) => item,
                None => {
                    if self.shared.producers.load(Ordering::Acquire) != 0 {
                        return Err(PopError::Empty);
                    }
                    // The last producer may have pushed just before leaving.
                    match raw() {
                        Some(item) => item,
                        None => return Err(PopError::Disconnected),
                    }
                }
            };
            self.shared.after_pop();
            if self.is_stale(&item.stamp) {
                self.stale_dropped += 1;
                continue;
            }
            return Ok(item);
        }
    }

    fn pop_with<T>(
        &mut self,
        deadline: Option<Instant>,
        raw: &mut impl FnMut() -> Option<Stamped<T>>,
    ) -> Result<Stamped<T>, PopError> {
        loop {
            match self.try_pop_with(raw) {
                Err(PopError::Empty) => {}
                other => return other,
            }
            self.shared.consumer_waiting.store(true, Ordering::Relaxed);
            fence(Ordering::SeqCst);
            let outcome = match self.try_pop_with(raw) {
                Err(PopError::Empty) => match deadline {
                    None => {
                        self.parker.park();
                        None
                    }
                    Some(deadline) if Instant::now() < deadline => {
                        self.parker.park_deadline(deadline);
                        None
                    }
                    Some(_) => Some(Err(PopError::Timeout)),
                },
                other => Some(other),
            };
            self.shared.consumer_waiting.store(false, Ordering::Relaxed);
            if let Some(result) = outcome {
                return result;
            }
        }
    }
}

impl Drop for ConsumerSide {
    fn drop(&mut self) {
        self.shared.consumer_alive.store(false, Ordering::Release);
        self.shared.wake_producers();
    }
}

fn consumer_side(generation: &Generation) -> ConsumerSide {
    let parker = Parker::new();
    let shared = Shared::new(generation, parker.unparker());
    ConsumerSide {
        shared,
        parker,
        stale_dropped: 0,
    }
}

/// A single-producer single-consumer frame queue holding up to `capacity` frames.
///
/// # Panics
///
/// If `capacity` is 0.
pub fn spsc<T: Send>(
    capacity: usize,
    generation: &Generation,
) -> (SpscProducer<T>, SpscConsumer<T>) {
    assert!(
        capacity > 0,
        "a frame queue needs room for at least one frame"
    );
    let (tx, rx) = rtrb::RingBuffer::new(capacity);
    let consumer = consumer_side(generation);
    let producer = ProducerSide::new(consumer.shared.clone());
    (
        SpscProducer {
            ring: tx,
            side: producer,
        },
        SpscConsumer {
            ring: rx,
            side: consumer,
        },
    )
}

/// A multi-producer single-consumer frame queue holding up to `capacity` frames. Clone
/// the producer for each decoder.
///
/// # Panics
///
/// If `capacity` is 0.
pub fn mpsc<T: Send>(
    capacity: usize,
    generation: &Generation,
) -> (MpscProducer<T>, MpscConsumer<T>) {
    assert!(
        capacity > 0,
        "a frame queue needs room for at least one frame"
    );
    let ring = Arc::new(ArrayQueue::new(capacity));
    let consumer = consumer_side(generation);
    let producer = ProducerSide::new(consumer.shared.clone());
    (
        MpscProducer {
            ring: ring.clone(),
            side: producer,
        },
        MpscConsumer {
            ring,
            side: consumer,
        },
    )
}

/// The producer methods both queue shapes share, over each shape's `parts`.
macro_rules! producer_methods {
    () => {
        /// Pushes without waiting: [`PushError::Full`] when the queue is full, `Stale`
        /// for an old generation, `Disconnected` when the consumer is gone.
        pub fn try_push(&mut self, item: Stamped<T>) -> Result<(), PushError<T>> {
            let (side, mut raw) = self.parts();
            side.try_push_with(item, &mut raw)
        }

        /// Pushes, parking while the queue is full. Returns `Stale` as soon as the
        /// generation moves past the frame's, and `Disconnected` when the consumer goes.
        pub fn push(&mut self, item: Stamped<T>) -> Result<(), PushError<T>> {
            let (side, mut raw) = self.parts();
            side.push_with(item, None, &mut raw)
        }

        /// [`push`](Self::push) that gives up with [`PushError::Timeout`] after `timeout`.
        pub fn push_timeout(
            &mut self,
            item: Stamped<T>,
            timeout: Duration,
        ) -> Result<(), PushError<T>> {
            let deadline = Instant::now() + timeout;
            let (side, mut raw) = self.parts();
            side.push_with(item, Some(deadline), &mut raw)
        }

        /// The pipeline's current generation; stamp new frames with it.
        pub fn generation(&self) -> u64 {
            self.side.shared.generation.current()
        }

        /// Whether work for `generation` is out of date, so a decoder can abandon it
        /// before decoding.
        pub fn is_stale(&self, generation: u64) -> bool {
            generation < self.generation()
        }

        /// Whether the consumer is gone.
        pub fn is_disconnected(&self) -> bool {
            !self.side.shared.consumer_alive.load(Ordering::Acquire)
        }
    };
}

/// The consumer methods both queue shapes share, over each shape's `parts`.
macro_rules! consumer_methods {
    () => {
        /// Pops the oldest current-generation frame without waiting, dropping stale
        /// frames on the way.
        pub fn try_pop(&mut self) -> Result<Stamped<T>, PopError> {
            let (side, mut raw) = self.parts();
            side.try_pop_with(&mut raw)
        }

        /// Pops, parking until a frame arrives or every producer is gone.
        pub fn pop(&mut self) -> Result<Stamped<T>, PopError> {
            let (side, mut raw) = self.parts();
            side.pop_with(None, &mut raw)
        }

        /// [`pop`](Self::pop) that gives up with [`PopError::Timeout`] after `timeout`.
        pub fn pop_timeout(&mut self, timeout: Duration) -> Result<Stamped<T>, PopError> {
            let deadline = Instant::now() + timeout;
            let (side, mut raw) = self.parts();
            side.pop_with(Some(deadline), &mut raw)
        }

        /// Stale frames dropped by this consumer so far.
        pub fn stale_dropped(&self) -> u64 {
            self.side.stale_dropped
        }

        /// The pipeline's current generation.
        pub fn generation(&self) -> u64 {
            self.side.shared.generation.current()
        }

        /// Whether every producer is gone (frames may still be queued).
        pub fn is_disconnected(&self) -> bool {
            self.side.shared.producers.load(Ordering::Acquire) == 0
        }
    };
}

/// The producer of an [`spsc`] queue.
pub struct SpscProducer<T> {
    ring: rtrb::Producer<Stamped<T>>,
    side: ProducerSide,
}

impl<T> SpscProducer<T> {
    producer_methods!();

    fn parts(
        &mut self,
    ) -> (
        &ProducerSide,
        impl FnMut(Stamped<T>) -> Result<(), Stamped<T>> + '_,
    ) {
        let ring = &mut self.ring;
        (&self.side, move |item| {
            ring.push(item).map_err(|rtrb::PushError::Full(item)| item)
        })
    }

    /// Free slots.
    pub fn slots(&self) -> usize {
        self.ring.slots()
    }

    pub fn capacity(&self) -> usize {
        self.ring.buffer().capacity()
    }
}

/// The consumer of an [`spsc`] queue.
pub struct SpscConsumer<T> {
    ring: rtrb::Consumer<Stamped<T>>,
    side: ConsumerSide,
}

impl<T> SpscConsumer<T> {
    consumer_methods!();

    fn parts(&mut self) -> (&mut ConsumerSide, impl FnMut() -> Option<Stamped<T>> + '_) {
        let ring = &mut self.ring;
        (&mut self.side, move || ring.pop().ok())
    }

    /// Pops and drops every queued frame of an older generation, stopping at the first
    /// current one. Returns how many were dropped.
    pub fn drain_stale(&mut self) -> usize {
        let mut dropped = 0;
        while let Ok(item) = self.ring.peek() {
            if !self.side.is_stale(&item.stamp) {
                break;
            }
            drop(self.ring.pop());
            self.side.shared.after_pop();
            dropped += 1;
        }
        self.side.stale_dropped += dropped as u64;
        dropped
    }

    /// The oldest queued frame without taking it, after dropping stale frames ahead of
    /// it.
    pub fn peek(&mut self) -> Option<&Stamped<T>> {
        self.drain_stale();
        self.ring.peek().ok()
    }

    /// The present policy: takes every queued frame for which `due` holds (in queue
    /// order, stopping at the first that is not due yet), returns the latest and drops
    /// the earlier ones as late. `None` means nothing is due: keep showing the frame
    /// already on screen.
    pub fn pop_latest_due(&mut self, mut due: impl FnMut(&FrameStamp) -> bool) -> Option<Due<T>> {
        let mut latest: Option<Stamped<T>> = None;
        let mut skipped = 0;
        loop {
            self.drain_stale();
            match self.ring.peek() {
                Ok(item) if due(&item.stamp) => {}
                _ => break,
            }
            let Ok(item) = self.ring.pop() else {
                break;
            };
            self.side.shared.after_pop();
            if latest.replace(item).is_some() {
                skipped += 1;
            }
        }
        latest.map(|frame| Due { frame, skipped })
    }

    /// Queued frames, stale ones included.
    pub fn len(&self) -> usize {
        self.ring.slots()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.ring.buffer().capacity()
    }
}

/// A producer of an [`mpsc`] queue. Clone it for each decoder.
pub struct MpscProducer<T> {
    ring: Arc<ArrayQueue<Stamped<T>>>,
    side: ProducerSide,
}

impl<T> MpscProducer<T> {
    producer_methods!();

    fn parts(
        &mut self,
    ) -> (
        &ProducerSide,
        impl FnMut(Stamped<T>) -> Result<(), Stamped<T>> + '_,
    ) {
        let ring = &self.ring;
        (&self.side, move |item| ring.push(item))
    }

    pub fn capacity(&self) -> usize {
        self.ring.capacity()
    }
}

impl<T> Clone for MpscProducer<T> {
    fn clone(&self) -> Self {
        Self {
            ring: self.ring.clone(),
            side: ProducerSide::new(self.side.shared.clone()),
        }
    }
}

/// The consumer of an [`mpsc`] queue.
pub struct MpscConsumer<T> {
    ring: Arc<ArrayQueue<Stamped<T>>>,
    side: ConsumerSide,
}

impl<T> MpscConsumer<T> {
    consumer_methods!();

    fn parts(&mut self) -> (&mut ConsumerSide, impl FnMut() -> Option<Stamped<T>> + '_) {
        let ring = &self.ring;
        (&mut self.side, move || ring.pop())
    }

    /// Queued frames, stale ones included.
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.ring.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn _assert_send<T: Send>() {}
    const _: () = {
        _assert_send::<DecodeSender>();
        _assert_send::<DecodeReceiver>();
        _assert_send::<PresentSender>();
        _assert_send::<PresentReceiver>();
    };
}
