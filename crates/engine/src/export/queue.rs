//! [`ExportQueue`]: jobs in submission order, one at a time on `glass-export`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use render::readback::ReadbackConfig;
use render::wgpu;

use super::events::{EventKind, ExportEvent, JobId};
use super::job::ExportJob;
use super::run;
use crate::lookahead::{LookaheadConfig, MediaDecoderOptions};
use crate::threads::{self, Role, ThreadRegistry};

/// Decoding, read-back and reporting settings shared by every job of a queue.
#[derive(Clone, Debug)]
pub struct ExportQueueConfig {
    /// How the look-ahead opens sources (hardware decode by default).
    pub decoder: MediaDecoderOptions,
    /// `glass-decode-<n>` workers of each job's look-ahead (at least 2).
    pub decode_workers: usize,
    /// Decoded frames each look-ahead lane holds; `None` for the look-ahead's default at
    /// the sequence's canvas.
    pub depth: Option<usize>,
    /// Read-back buffers in flight (lossless): how far GPU render runs ahead of encode.
    pub readback_slots: usize,
    /// Read-back frames queued for the encoder.
    pub encode_queue: usize,
    /// The shortest time between two progress events of a job.
    pub progress_interval: Duration,
    pub registry: Arc<ThreadRegistry>,
}

impl Default for ExportQueueConfig {
    fn default() -> Self {
        Self {
            decoder: MediaDecoderOptions::default(),
            decode_workers: LookaheadConfig::default().decode_workers,
            depth: None,
            readback_slots: ReadbackConfig::EXPORT.slots,
            encode_queue: 2,
            progress_interval: Duration::from_millis(100),
            registry: threads::global().clone(),
        }
    }
}

/// Pause and cancel for one job, shared by the queue handle and the job's threads.
#[derive(Debug, Default)]
pub(crate) struct JobControl {
    paused: Mutex<bool>,
    changed: Condvar,
    cancelled: AtomicBool,
    /// `glass-export` gave up on the job: the render thread stops too.
    render_stopped: AtomicBool,
}

impl JobControl {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.paused.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        *self.lock() = paused;
        self.changed.notify_all();
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        // Taking the lock orders the flag before any waiter's recheck.
        drop(self.lock());
        self.changed.notify_all();
    }

    pub(crate) fn stop_render(&self) {
        self.render_stopped.store(true, Ordering::SeqCst);
        drop(self.lock());
        self.changed.notify_all();
    }

    pub(crate) fn render_stopped(&self) -> bool {
        self.render_stopped.load(Ordering::SeqCst)
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub(crate) fn is_paused(&self) -> bool {
        *self.lock()
    }

    /// Returns at once while running; while paused, waits (marked idle) for resume or
    /// cancel. `false` once cancelled.
    pub(crate) fn wait_running(&self) -> bool {
        let mut paused = self.lock();
        while *paused && !self.is_cancelled() {
            paused = threads::idle(|| {
                self.changed
                    .wait(paused)
                    .unwrap_or_else(PoisonError::into_inner)
            });
        }
        !self.is_cancelled()
    }
}

struct Queued {
    id: JobId,
    job: ExportJob,
    control: Arc<JobControl>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    queued: VecDeque<Queued>,
    running: Option<(JobId, Arc<JobControl>)>,
    /// `glass-export` is running (it exits once the queue is empty).
    worker: bool,
    handle: Option<JoinHandle<()>>,
    closed: bool,
}

pub(crate) struct Shared {
    state: Mutex<State>,
    pub(crate) events: Sender<ExportEvent>,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) config: ExportQueueConfig,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn emit(&self, job: JobId, kind: EventKind) {
        let _ = self.events.send(ExportEvent { job, kind });
    }
}

/// Export jobs run one at a time, in submission order, on a `glass-export` thread that
/// starts with the first job and ends when the queue is empty. Dropping the queue
/// cancels every job and waits for the thread.
///
/// Jobs do not run in parallel: they would share the GPU, the hardware encoder and the
/// CPU cores x264 and x265 already use, and finish no sooner overall.
pub struct ExportQueue {
    shared: Arc<Shared>,
}

impl ExportQueue {
    /// A queue rendering on `device` (the app's shared device). Returns the queue and the
    /// receiver of its events, which the UI drains with `try_recv` once per frame.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: ExportQueueConfig,
    ) -> (Self, Receiver<ExportEvent>) {
        let (events, receiver) = mpsc::channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            events,
            device: device.clone(),
            queue: queue.clone(),
            config,
        });
        (Self { shared }, receiver)
    }

    /// Adds `job` at the end of the queue and returns its id; `Queued` follows at once.
    pub fn submit(&self, job: ExportJob) -> JobId {
        let mut state = self.shared.lock();
        state.next_id += 1;
        let id = JobId(state.next_id);
        let ahead = state.queued.len() + usize::from(state.running.is_some());
        state.queued.push_back(Queued {
            id,
            job,
            control: Arc::new(JobControl::default()),
        });
        self.shared.emit(id, EventKind::Queued { ahead });
        if !state.worker && !state.closed {
            let shared = self.shared.clone();
            match self
                .shared
                .config
                .registry
                .spawn(Role::Export, None, move || worker(shared))
            {
                Ok(handle) => {
                    state.worker = true;
                    // The previous worker has left its loop; dropping its handle detaches
                    // the last instructions of its exit.
                    state.handle = Some(handle);
                }
                Err(error) => {
                    let queued = state.queued.pop_back().expect("just queued");
                    drop(state);
                    run::fail_to_start(&self.shared, queued.id, &queued.job, &error);
                }
            }
        }
        id
    }

    /// Pauses a queued or running job at its next frame boundary. `false` when the job
    /// is not in the queue.
    pub fn pause(&self, id: JobId) -> bool {
        self.with_control(id, |c| c.set_paused(true))
    }

    pub fn resume(&self, id: JobId) -> bool {
        self.with_control(id, |c| c.set_paused(false))
    }

    /// Cancels a job: a queued one leaves the queue (`Cancelled` at once), a running one
    /// stops, closes and deletes its partial files. `false` when the job is not in the
    /// queue.
    pub fn cancel(&self, id: JobId) -> bool {
        let mut state = self.shared.lock();
        if let Some(i) = state.queued.iter().position(|q| q.id == id) {
            state.queued.remove(i);
            self.shared.emit(id, EventKind::Cancelled);
            return true;
        }
        match &state.running {
            Some((running, control)) if *running == id => {
                control.cancel();
                true
            }
            _ => false,
        }
    }

    /// The running job, if any.
    pub fn running(&self) -> Option<JobId> {
        self.shared.lock().running.as_ref().map(|(id, _)| *id)
    }

    /// Jobs waiting, in the order they will run.
    pub fn queued(&self) -> Vec<JobId> {
        self.shared.lock().queued.iter().map(|q| q.id).collect()
    }

    /// Whether `id` is paused (queued or running).
    pub fn is_paused(&self, id: JobId) -> bool {
        let state = self.shared.lock();
        let control = state
            .queued
            .iter()
            .find(|q| q.id == id)
            .map(|q| &q.control)
            .or(state
                .running
                .as_ref()
                .filter(|(r, _)| *r == id)
                .map(|(_, c)| c));
        control.is_some_and(|c| c.is_paused())
    }

    fn with_control(&self, id: JobId, f: impl FnOnce(&JobControl)) -> bool {
        let state = self.shared.lock();
        let control = state
            .queued
            .iter()
            .find(|q| q.id == id)
            .map(|q| &q.control)
            .or(state
                .running
                .as_ref()
                .filter(|(r, _)| *r == id)
                .map(|(_, c)| c));
        match control {
            Some(c) => {
                f(c);
                true
            }
            None => false,
        }
    }
}

impl Drop for ExportQueue {
    fn drop(&mut self) {
        let handle = {
            let mut state = self.shared.lock();
            state.closed = true;
            for q in std::mem::take(&mut state.queued) {
                self.shared.emit(q.id, EventKind::Cancelled);
            }
            if let Some((_, control)) = &state.running {
                control.cancel();
            }
            state.handle.take()
        };
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

/// `glass-export`: runs queued jobs until there are none.
fn worker(shared: Arc<Shared>) {
    loop {
        let next = {
            let mut state = shared.lock();
            match state.queued.pop_front() {
                Some(q) if !state.closed => {
                    state.running = Some((q.id, q.control.clone()));
                    q
                }
                _ => {
                    state.worker = false;
                    state.running = None;
                    return;
                }
            }
        };
        run::run_job(&shared, next.id, next.job, &next.control);
        shared.lock().running = None;
    }
}
