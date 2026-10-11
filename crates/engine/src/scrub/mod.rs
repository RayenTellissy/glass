//! Scrub mode: while the user drags the playhead, show the nearest cached frame at once
//! and the exact frame when the pointer pauses.
//!
//! ```text
//!          request(frame) ─▶ latest request (ArrayQueue of 1) + unpark ─▶ glass-scrub
//!   UI  ◀─ Shown (nearest cached frame, from CachedFrames::try_nearest)       │ decode (software,
//!       ◀─ poll_ready() ◀── Ready (ArrayQueue of 1) ◀── render into the cache ┘ one decoder per source)
//! ```
//!
//! **Immediate answer.** [`Scrubber::request`] runs on the UI thread and never waits: it
//! hands the request to the worker through a one-slot lock-free queue (a newer request
//! replaces an unread one), unparks it, and answers from [`CachedFrames`], the index of
//! timeline frames resident in the rendered-frame cache, with a `try_read` only. The
//! answer is the requested frame when cached, else the nearest cached frame by timeline
//! distance, preferring the side the pointer moves toward ([`nearest`]); when nothing is
//! cached, or a writer holds the index that instant, the frame already on screen stays.
//! The UI shows the answered frame from the cache.
//!
//! **Exact frame after a pause.** The worker (`glass-scrub`) waits on the [`ScrubClock`]
//! until the latest request is [`ScrubConfig::pause`] old (40 ms), then decodes that
//! timeline frame exactly: every segment of the [`DecodePlan`] covering it, through a
//! [`ScrubDecoder`] per source, opened on first use and kept open until the scrub ends
//! ([`SoftwareScrubDecoder`] for files, software only). [`ScrubRender`] renders the
//! layers into the frame cache, the frame joins [`CachedFrames`], and a [`Ready`] goes to
//! the UI ([`Scrubber::poll_ready`]). A frame already cached is published without
//! decoding. Repeating the last requested frame is not a new request (it does not
//! restart the pause).
//!
//! **Supersede.** A newer request cancels the exact decode in flight: decoders check the
//! cancel flag between GOP-walk steps and between layers, and a frame decoded for an old
//! request is neither rendered nor published. A `Ready` that arrives after a newer request
//! is dropped by `poll_ready`.
//!
//! **Fast moves.** While the pointer moves (requests closer together than the pause),
//! the worker spends its time on frames cheap to decode: the first timeline frame showing
//! each source keyframe within [`ScrubConfig::prefetch_window`] frames ahead of the
//! pointer, in the direction of motion, nearest first, at most
//! [`ScrubConfig::prefetch_per_move`] per request and each frame tried once per scrub,
//! skipping cached frames and frames covered by more than one segment. Each is one decoded
//! frame (no GOP walk), rendered into the cache so later requests find a near frame. A
//! prefetch is cancelled when the pause runs out, so the exact frame comes first.
//!
//! **End.** [`Scrubber::finish`] (or dropping the handle) stops the worker, cancelling
//! any decode in flight, joins it and closes every decoder, and returns where the pointer
//! was so playback can seek there.

mod cached;
mod clock;
mod decoder;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{JoinHandle, Thread};
use std::time::Duration;

use crossbeam_queue::ArrayQueue;
use media::{MediaError, Rational, Timestamp};

use crate::lookahead::{DecodePlan, Layer, PlanSegment, SourceId, SourceRef};
use crate::threads::{self, Role, ThreadRegistry};

pub use cached::{Busy, CachedFrames, Direction, nearest};
pub use clock::{ManualClock, MonotonicClock, ScrubClock};
pub use decoder::{SoftwareDecoderOptions, SoftwareScrubDecoder};

/// How long the pointer must rest before the exact frame is decoded.
pub const DEFAULT_PAUSE: Duration = Duration::from_millis(40);

/// The result of one exact decode.
#[derive(Debug)]
pub enum Decoded<F> {
    /// The frame on screen at the requested time, and its presentation time.
    Frame(Timestamp, F),
    /// The source has no frames.
    Empty,
    /// The cancel flag was seen before the frame was reached.
    Cancelled,
}

/// The scrubber's decoder for one source, kept open while the scrub lasts.
pub trait ScrubDecoder: Send + 'static {
    type Frame: Send + Sync + 'static;

    /// The frame on screen at source time `at` (the last one presented at or before it,
    /// the first for an earlier `at`). Checks `cancel` whenever it can stop early and
    /// returns [`Decoded::Cancelled`] once it returns true.
    fn decode_exact(
        &mut self,
        at: Timestamp,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Decoded<Self::Frame>, MediaError>;

    /// Appends the presentation times of the keyframes from `from` to `to` (inclusive):
    /// the frames that decode without a GOP walk. Sources without keyframes (pictures)
    /// append nothing.
    fn keyframes(&mut self, from: Timestamp, to: Timestamp, out: &mut Vec<Timestamp>);
}

/// Renders a scrubbed timeline frame into the rendered-frame cache. Called on
/// `glass-scrub`; an implementation backed by the GPU forwards the layers to
/// `glass-render` and returns once the frame is cached.
pub trait ScrubRender<F>: Send + 'static {
    /// Renders timeline frame `frame` from `layers` (bottom first; empty in a gap).
    fn render(&mut self, frame: i64, layers: &[Layer<F>]);
}

/// Timing and prefetch limits of a scrub.
#[derive(Clone, Debug)]
pub struct ScrubConfig {
    /// How long the pointer rests before the exact frame is decoded.
    pub pause: Duration,
    /// One timeline frame's duration.
    pub frame_duration: Rational,
    /// How much farther the cached frame ahead of the pointer may be than the one behind
    /// and still be shown ([`nearest`]).
    pub ahead_bias: f64,
    /// Timeline frames ahead of the pointer searched for cheap frames while it moves.
    pub prefetch_window: i64,
    /// Cheap frames decoded per request at most; 0 turns prefetching off.
    pub prefetch_per_move: usize,
    pub registry: Arc<ThreadRegistry>,
}

impl Default for ScrubConfig {
    fn default() -> Self {
        Self {
            pause: DEFAULT_PAUSE,
            frame_duration: Rational::new(1, 30),
            ahead_bias: 2.0,
            prefetch_window: 90,
            prefetch_per_move: 2,
            registry: threads::global().clone(),
        }
    }
}

/// The frame the UI shows for a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    pub frame: i64,
    /// Whether it is the requested frame.
    pub exact: bool,
}

/// The exact frame for a request is in the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ready {
    pub frame: i64,
    /// The request it answers (counts requests from 1).
    pub request: u64,
}

/// Counters for tests, benchmarks and the debug panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrubStats {
    /// Requests (repeats of the last frame not counted).
    pub requests: u64,
    /// Requests answered with the requested frame from the cache.
    pub exact_hits: u64,
    /// Requests answered with another cached frame.
    pub nearest_hits: u64,
    /// Requests answered with the frame already on screen, or nothing.
    pub held: u64,
    /// Exact frames decoded and rendered after a pause.
    pub exact_decodes: u64,
    /// Exact frames published without decoding (already cached).
    pub exact_from_cache: u64,
    /// `Ready` messages sent.
    pub published: u64,
    /// Exact decodes dropped because a newer request came.
    pub superseded: u64,
    /// Cheap frames decoded and rendered while moving.
    pub prefetched: u64,
    /// Cheap frames abandoned when the pause ran out.
    pub prefetch_cancelled: u64,
    /// Sources that failed to open, and decode errors.
    pub decode_errors: u64,
}

/// What the UI hands back to playback when the scrub ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrubEnd {
    /// The last requested frame: where playback continues.
    pub frame: Option<i64>,
    /// The frame on screen.
    pub shown: Option<Shown>,
    pub stats: ScrubStats,
}

#[derive(Clone, Copy, Debug)]
struct Request {
    frame: i64,
    at: Duration,
    seq: u64,
    direction: Direction,
}

#[derive(Default)]
struct Counters {
    exact_decodes: AtomicU64,
    exact_from_cache: AtomicU64,
    published: AtomicU64,
    superseded: AtomicU64,
    prefetched: AtomicU64,
    prefetch_cancelled: AtomicU64,
    decode_errors: AtomicU64,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

struct Shared {
    requests: ArrayQueue<Request>,
    ready: ArrayQueue<Ready>,
    stop: AtomicBool,
    /// When the latest request was made, in nanoseconds of the scrub clock.
    last_request: AtomicU64,
    counters: Counters,
}

impl Shared {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// A running scrub, owned by the UI thread. Dropping it stops and joins the worker.
pub struct Scrubber {
    shared: Arc<Shared>,
    /// The worker thread, unparked for each request.
    worker: Thread,
    clock: Arc<dyn ScrubClock>,
    cached: CachedFrames,
    ahead_bias: f64,
    /// The last requested frame and the direction of motion at it.
    last: Option<(i64, Direction)>,
    shown: Option<Shown>,
    seq: u64,
    requests: u64,
    exact_hits: u64,
    nearest_hits: u64,
    held: u64,
    thread: Option<JoinHandle<()>>,
}

impl Scrubber {
    /// Starts `glass-scrub` for `plan`. `cached` is the index the frame cache's owner
    /// keeps; `make_decoder` opens a source's decoder on the worker the first time the
    /// scrub needs it; `shown` is the frame on screen when scrubbing starts.
    pub fn start<P, D, R, M>(
        plan: P,
        config: ScrubConfig,
        cached: CachedFrames,
        clock: Arc<dyn ScrubClock>,
        make_decoder: M,
        render: R,
        shown: Option<i64>,
    ) -> std::io::Result<Self>
    where
        P: DecodePlan,
        D: ScrubDecoder,
        R: ScrubRender<D::Frame>,
        M: FnMut(&SourceRef) -> Result<D, MediaError> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            requests: ArrayQueue::new(1),
            ready: ArrayQueue::new(1),
            stop: AtomicBool::new(false),
            last_request: AtomicU64::new(0),
            counters: Counters::default(),
        });
        let worker = Worker {
            plan,
            make_decoder,
            decoders: HashMap::new(),
            render,
            clock: clock.clone(),
            shared: shared.clone(),
            cached: cached.clone(),
            pause: config.pause,
            frame_duration: config.frame_duration,
            prefetch_window: config.prefetch_window.max(0),
            prefetch_per_move: config.prefetch_per_move,
            latest: None,
            finished: None,
            prefetch: (0, 0),
            tried: HashSet::new(),
            keyframes: Vec::new(),
        };
        let thread = config
            .registry
            .spawn(Role::Scrub, None, move || worker.run())?;
        Ok(Self {
            shared,
            worker: thread.thread().clone(),
            clock,
            cached,
            ahead_bias: config.ahead_bias,
            last: None,
            shown: shown.map(|frame| Shown {
                frame,
                exact: false,
            }),
            seq: 0,
            requests: 0,
            exact_hits: 0,
            nearest_hits: 0,
            held: 0,
            thread: Some(thread),
        })
    }

    /// The pointer is at timeline frame `frame`: returns the frame to show now (see the
    /// module docs). Never waits.
    pub fn request(&mut self, frame: i64) -> Option<Shown> {
        let direction = match self.last {
            Some((last, _)) => match Direction::of_move(last, frame) {
                Direction::Still => return self.shown,
                moved => moved,
            },
            None => Direction::Still,
        };
        let at = self.clock.now();
        self.seq += 1;
        self.requests += 1;
        self.last = Some((frame, direction));
        self.shared.last_request.store(nanos(at), Ordering::Release);
        self.shared.requests.force_push(Request {
            frame,
            at,
            seq: self.seq,
            direction,
        });
        self.worker.unpark();

        let pick = self
            .cached
            .try_nearest(frame, direction, self.ahead_bias)
            .ok()
            .flatten();
        self.shown = match pick {
            Some(pick) => {
                if pick == frame {
                    self.exact_hits += 1;
                } else {
                    self.nearest_hits += 1;
                }
                Some(Shown {
                    frame: pick,
                    exact: pick == frame,
                })
            }
            None => {
                self.held += 1;
                self.shown.map(|shown| Shown {
                    frame: shown.frame,
                    exact: shown.frame == frame,
                })
            }
        };
        self.shown
    }

    /// The exact frame for the latest request, once it is in the cache; `None` before
    /// that, and for frames of older requests.
    pub fn poll_ready(&mut self) -> Option<Ready> {
        while let Some(ready) = self.shared.ready.pop() {
            if ready.request == self.seq {
                self.shown = Some(Shown {
                    frame: ready.frame,
                    exact: true,
                });
                return Some(ready);
            }
        }
        None
    }

    /// The frame on screen.
    pub fn shown(&self) -> Option<Shown> {
        self.shown
    }

    pub fn stats(&self) -> ScrubStats {
        let c = &self.shared.counters;
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        ScrubStats {
            requests: self.requests,
            exact_hits: self.exact_hits,
            nearest_hits: self.nearest_hits,
            held: self.held,
            exact_decodes: load(&c.exact_decodes),
            exact_from_cache: load(&c.exact_from_cache),
            published: load(&c.published),
            superseded: load(&c.superseded),
            prefetched: load(&c.prefetched),
            prefetch_cancelled: load(&c.prefetch_cancelled),
            decode_errors: load(&c.decode_errors),
        }
    }

    /// Ends the scrub: stops the worker (cancelling a decode in flight), closes its
    /// decoders and returns where playback continues.
    pub fn finish(mut self) -> ScrubEnd {
        self.stop();
        ScrubEnd {
            frame: self.last.map(|(frame, _)| frame),
            shown: self.shown,
            stats: self.stats(),
        }
    }

    fn stop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.shared.stop.store(true, Ordering::Release);
        self.worker.unpark();
        let _ = thread.join();
    }
}

impl Drop for Scrubber {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Worker<P, D, R, M> {
    plan: P,
    make_decoder: M,
    /// `None` for a source that failed to open (not retried during this scrub).
    decoders: HashMap<SourceId, Option<D>>,
    render: R,
    clock: Arc<dyn ScrubClock>,
    shared: Arc<Shared>,
    cached: CachedFrames,
    pause: Duration,
    frame_duration: Rational,
    prefetch_window: i64,
    prefetch_per_move: usize,
    latest: Option<Request>,
    /// The request whose exact frame was handled.
    finished: Option<u64>,
    /// Prefetches made for a request: (request, count).
    prefetch: (u64, usize),
    /// Frames prefetched or tried this scrub.
    tried: HashSet<i64>,
    keyframes: Vec<Timestamp>,
}

impl<P, D, R, M> Worker<P, D, R, M>
where
    P: DecodePlan,
    D: ScrubDecoder,
    R: ScrubRender<D::Frame>,
    M: FnMut(&SourceRef) -> Result<D, MediaError> + Send + 'static,
{
    fn run(mut self) {
        self.clock.attach(&std::thread::current());
        while !self.shared.stopping() {
            if let Some(request) = self.shared.requests.pop() {
                self.latest = Some(request);
            }
            let Some(request) = self.latest else {
                self.park(None);
                continue;
            };
            if self.finished == Some(request.seq) {
                self.park(None);
                continue;
            }
            let due = request.at + self.pause;
            if self.clock.now() >= due {
                self.exact(request);
                self.finished = Some(request.seq);
                continue;
            }
            if self.prefetch(request) {
                continue;
            }
            self.park(Some(due));
        }
    }

    fn park(&self, deadline: Option<Duration>) {
        threads::idle(|| self.clock.park_until(deadline));
    }

    fn exact(&mut self, request: Request) {
        let counters = &self.shared.counters;
        if self.cached.contains(request.frame) {
            bump(&counters.exact_from_cache);
            self.publish(request);
            return;
        }
        let shared = self.shared.clone();
        let superseded = move || shared.stopping() || !shared.requests.is_empty();
        let Some(layers) = self.decode(request.frame, &superseded) else {
            bump(&self.shared.counters.superseded);
            return;
        };
        if superseded() {
            bump(&self.shared.counters.superseded);
            return;
        }
        self.render.render(request.frame, &layers);
        drop(layers);
        self.cached.insert(request.frame);
        bump(&self.shared.counters.exact_decodes);
        if !superseded() {
            self.publish(request);
        }
    }

    fn publish(&self, request: Request) {
        self.shared.ready.force_push(Ready {
            frame: request.frame,
            request: request.seq,
        });
        bump(&self.shared.counters.published);
    }

    /// Decodes one cheap frame ahead of a moving pointer; false when there is nothing
    /// (more) to do for this request.
    fn prefetch(&mut self, request: Request) -> bool {
        if request.direction == Direction::Still || self.prefetch_per_move == 0 {
            return false;
        }
        if self.prefetch.0 != request.seq {
            self.prefetch = (request.seq, 0);
        }
        if self.prefetch.1 >= self.prefetch_per_move {
            return false;
        }
        let Some(target) = self.prefetch_target(request) else {
            return false;
        };
        self.prefetch.1 += 1;
        self.tried.insert(target);
        let shared = self.shared.clone();
        let clock = self.clock.clone();
        let pause = nanos(self.pause);
        let pause_over = move || {
            shared.stopping()
                || nanos(clock.now())
                    >= shared
                        .last_request
                        .load(Ordering::Acquire)
                        .saturating_add(pause)
        };
        match self.decode(target, &pause_over) {
            Some(layers) => {
                self.render.render(target, &layers);
                drop(layers);
                self.cached.insert(target);
                bump(&self.shared.counters.prefetched);
            }
            None => {
                self.tried.remove(&target);
                bump(&self.shared.counters.prefetch_cancelled);
            }
        }
        true
    }

    /// The nearest frame ahead of the pointer, within the window, that shows a source
    /// keyframe first, is covered by one segment, and is neither cached nor tried.
    fn prefetch_target(&mut self, request: Request) -> Option<i64> {
        let window = self.prefetch_window;
        let (lo, hi) = match request.direction {
            Direction::Forward => (request.frame + 1, request.frame + window),
            Direction::Backward => (request.frame - window, request.frame - 1),
            Direction::Still => return None,
        };
        if lo > hi {
            return None;
        }
        let segments: Vec<PlanSegment> = self
            .plan
            .segments_from(lo)
            .take_while(|s| s.timeline.start <= hi)
            .collect();
        let mut best: Option<i64> = None;
        let mut keyframes = std::mem::take(&mut self.keyframes);
        for segment in &segments {
            let a = lo.max(segment.timeline.start);
            let b = hi.min(segment.timeline.end - 1);
            if a > b {
                continue;
            }
            let from = segment.source_time_at(a - 1, self.frame_duration);
            let to = segment.source_time_at(b, self.frame_duration);
            keyframes.clear();
            if let Some(decoder) = self.decoder(&segment.source) {
                decoder.keyframes(from, to, &mut keyframes);
            }
            for &keyframe in &keyframes {
                let t = first_frame_showing(segment, keyframe, self.frame_duration);
                if t < a || t > b || self.tried.contains(&t) {
                    continue;
                }
                let distance = (t - request.frame).abs();
                if best.is_some_and(|best| (best - request.frame).abs() <= distance) {
                    continue;
                }
                let layers = segments.iter().filter(|s| s.timeline.contains(&t)).count();
                if layers != 1 || self.cached.contains(t) {
                    continue;
                }
                best = Some(t);
            }
        }
        self.keyframes = keyframes;
        best
    }

    fn decoder(&mut self, source: &SourceRef) -> Option<&mut D> {
        let make_decoder = &mut self.make_decoder;
        let counters = &self.shared.counters;
        self.decoders
            .entry(source.id)
            .or_insert_with(|| match make_decoder(source) {
                Ok(decoder) => Some(decoder),
                Err(error) => {
                    tracing::warn!(
                        source = %source.path.display(),
                        %error,
                        "scrub: source failed to open"
                    );
                    bump(&counters.decode_errors);
                    None
                }
            })
            .as_mut()
    }

    /// Every layer of timeline frame `frame`, bottom first; `None` when `cancel` stopped
    /// it. A source that failed gives a layer with no frame (render draws the slate).
    fn decode(&mut self, frame: i64, cancel: &dyn Fn() -> bool) -> Option<Vec<Layer<D::Frame>>> {
        let mut segments: Vec<PlanSegment> = self
            .plan
            .segments_from(frame)
            .take_while(|s| s.timeline.start <= frame)
            .filter(|s| s.timeline.contains(&frame))
            .collect();
        segments.sort_by_key(|s| s.layer);
        let mut layers = Vec::with_capacity(segments.len());
        for segment in segments {
            if cancel() {
                return None;
            }
            let at = segment.source_time_at(frame, self.frame_duration);
            let decoded = self
                .decoder(&segment.source)
                .map(|decoder| decoder.decode_exact(at, cancel));
            let (source_pts, decoded) = match decoded {
                Some(Ok(Decoded::Frame(pts, decoded))) => (Some(pts), Some(Arc::new(decoded))),
                Some(Ok(Decoded::Cancelled)) => return None,
                Some(Ok(Decoded::Empty)) | None => (None, None),
                Some(Err(error)) => {
                    tracing::warn!(
                        source = %segment.source.path.display(),
                        frame,
                        %error,
                        "scrub: decode failed"
                    );
                    bump(&self.shared.counters.decode_errors);
                    (None, None)
                }
            };
            layers.push(Layer {
                layer: segment.layer,
                segment: segment.hash,
                source: segment.source.id,
                source_pts,
                frame: decoded,
                skipped: false,
            });
        }
        Some(layers)
    }
}

/// The first timeline frame of `segment` whose instant is at or after source time
/// `at`: `start + ceil((at - source_start) / step)`, exact, where `step` is the source
/// time one timeline frame advances ([`PlanSegment::source_step`]).
fn first_frame_showing(segment: &PlanSegment, at: Timestamp, frame_duration: Rational) -> i64 {
    let (an, ad) = positive(at.time_base);
    let (sn, sd) = positive(segment.source_start.time_base);
    let (fn_, fd) = positive(segment.source_step(frame_duration));
    // (at.value * an / ad - start.value * sn / sd) / (fn / fd), over a common denominator.
    let num =
        (i128::from(at.value) * an * sd - i128::from(segment.source_start.value) * sn * ad) * fd;
    let den = ad * sd * fn_;
    if den == 0 {
        return segment.timeline.start;
    }
    let offset = num.div_euclid(den) + i128::from(num.rem_euclid(den) != 0);
    let offset = i64::try_from(offset).unwrap_or(if offset < 0 { i64::MIN } else { i64::MAX });
    segment.timeline.start.saturating_add(offset)
}

fn positive(r: Rational) -> (i128, i128) {
    if r.den < 0 {
        (-i128::from(r.num), -i128::from(r.den))
    } else {
        (i128::from(r.num), i128::from(r.den))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookahead::SourceId;
    use render::frame_cache::SegmentHash;

    fn segment(start: i64, source_start: i64, time_base: Rational) -> PlanSegment {
        PlanSegment {
            source: SourceRef::new(SourceId(1), "a.mp4"),
            timeline: start..start + 100,
            source_start: Timestamp::new(source_start, time_base),
            speed: Rational::new(1, 1),
            layer: 0,
            hash: SegmentHash::from(1),
        }
    }

    #[test]
    fn first_frame_showing_inverts_source_time_at() {
        let fd = Rational::new(1, 30);
        // 25 fps source, 512 ticks of 1/12800 per frame, starting at source frame 3.
        let tb = Rational::new(1, 12800);
        let s = segment(10, 3 * 512, tb);
        for k in 3..60 {
            let at = Timestamp::new(k * 512, tb);
            let t = first_frame_showing(&s, at, fd);
            // Frame t is the first whose instant reaches source frame k.
            assert!(s.source_time_at(t, fd).cmp_time(at).is_ge(), "k {k} t {t}");
            assert!(
                s.source_time_at(t - 1, fd).cmp_time(at).is_lt(),
                "k {k} t {t}"
            );
        }
        // Same rate: source frame k shows first at start + (k - 3).
        let tb = Rational::new(1, 15360);
        let s = segment(10, 3 * 512, tb);
        assert_eq!(
            first_frame_showing(&s, Timestamp::new(33 * 512, tb), fd),
            40
        );
    }
}
