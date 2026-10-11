//! One job, start to finish: `glass-export` prepares it, encodes and verifies; a
//! `glass-export-render` thread renders and reads back its frames (see the
//! [module docs](super)).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use media::analysis::{Control, Stopped};
use media::export::verify::{self, Expected, VerifyError};
use media::export::{Container, ExportSession, Picture, SubtitleSettings};
use media::{Primaries, SubtitleCue, VideoFrame};
use project::subtitle_export::{self, Cue};
use project::{Time, TimeRange};
use render::colour::WorkingSpace;
use render::nodes::OutputTransfer;
use render::readback::{Backpressure, ReadbackConfig, ReadbackFrame, ReadbackRing};
use render::subtitle::{self, BurnIn, SubtitleStyle};
use render::text::{GlyphAtlas, TextEngine};
use render::wgpu;

use super::events::{EventKind, ExportFailure, Finished, JobId, Notification, Progress, Stage};
use super::job::{ExportJob, SubtitleExport, output_encoding, sidecar_path};
use super::queue::{JobControl, Shared};
use crate::audio::mixer::Layout;
use crate::lookahead::{
    FramesError, GopIndexes, Layer, Lookahead, LookaheadConfig, LookaheadFrames, MediaDecoder,
    MediaDecoderOptions, default_depth,
};
use crate::mix::{MixFormat, MixSettings, OfflineMix, sample_of};
use crate::threads::{self, Role};
use crate::timeline::{FrameOutput, RendererConfig, TimelinePlan, TimelineRenderer};

/// How often the render thread looks for a cancel while it waits for the look-ahead.
const LOOKAHEAD_POLL: Duration = Duration::from_millis(50);

enum Stop {
    Cancelled,
    Failed(ExportFailure),
}

impl From<ExportFailure> for Stop {
    fn from(failure: ExportFailure) -> Self {
        Self::Failed(failure)
    }
}

/// Runs `job` on the calling thread (`glass-export`) and reports its outcome.
pub(super) fn run_job(shared: &Shared, id: JobId, job: ExportJob, control: &Arc<JobControl>) {
    let name = job.display_name();
    let mut runner = Runner {
        shared,
        id,
        control,
        progress: Reporter::new(shared, id),
    };
    let kind = match runner.run(job) {
        Ok(finished) => EventKind::Finished(Box::new(finished)),
        Err(Stop::Cancelled) => EventKind::Cancelled,
        Err(Stop::Failed(failure)) => {
            tracing::warn!(job = id.0, key = failure.key, args = ?failure.args, "export failed");
            EventKind::Failed {
                failure,
                notification: Notification::failed(&name),
            }
        }
    };
    shared.emit(id, kind);
}

/// `glass-export` could not be started for `job`.
pub(super) fn fail_to_start(shared: &Shared, id: JobId, job: &ExportJob, error: &std::io::Error) {
    shared.emit(
        id,
        EventKind::Failed {
            failure: ExportFailure::new("export.error.thread").with("detail", error),
            notification: Notification::failed(&job.display_name()),
        },
    );
}

/// Running time with paused stretches left out.
struct RunTime {
    started: Instant,
    paused: Duration,
    paused_since: Option<Instant>,
}

impl RunTime {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            paused: Duration::ZERO,
            paused_since: None,
        }
    }

    fn pause(&mut self) {
        self.paused_since.get_or_insert_with(Instant::now);
    }

    fn resume(&mut self) {
        if let Some(since) = self.paused_since.take() {
            self.paused += since.elapsed();
        }
    }

    fn elapsed(&self) -> Duration {
        let now = self.paused_since.unwrap_or_else(Instant::now);
        now.duration_since(self.started).saturating_sub(self.paused)
    }
}

/// Throttled progress events.
struct Reporter<'a> {
    shared: &'a Shared,
    id: JobId,
    stage: Stage,
    done: u64,
    total: u64,
    time: RunTime,
    last: Option<Instant>,
}

impl<'a> Reporter<'a> {
    fn new(shared: &'a Shared, id: JobId) -> Self {
        Self {
            shared,
            id,
            stage: Stage::Preparing,
            done: 0,
            total: 0,
            time: RunTime::new(),
            last: None,
        }
    }

    fn fps(&self) -> f64 {
        let seconds = self.time.elapsed().as_secs_f64();
        if seconds > 0.0 {
            self.done as f64 / seconds
        } else {
            0.0
        }
    }

    fn emit(&mut self) {
        let fps = self.fps();
        let eta = (self.done > 0 && fps > 0.0)
            .then(|| Duration::from_secs_f64(self.total.saturating_sub(self.done) as f64 / fps));
        self.last = Some(Instant::now());
        self.shared.emit(
            self.id,
            EventKind::Progress(Progress {
                stage: self.stage,
                frames_done: self.done,
                total_frames: self.total,
                fps,
                eta,
                elapsed: self.time.elapsed(),
            }),
        );
    }

    fn stage(&mut self, stage: Stage) {
        self.stage = stage;
        self.emit();
    }

    fn frame_done(&mut self) {
        self.done += 1;
        let due = self
            .last
            .is_none_or(|t| t.elapsed() >= self.shared.config.progress_interval);
        if due || self.done == self.total {
            self.emit();
        }
    }
}

/// Lets `verify` stop on cancel and wait while paused.
struct JobVerifyControl<'a>(&'a JobControl);

impl Control for JobVerifyControl<'_> {
    fn checkpoint(&self) -> Result<(), Stopped> {
        if self.0.wait_running() {
            Ok(())
        } else {
            Err(Stopped)
        }
    }

    fn progress(&self, _: f32) {}

    fn partial(&self, _: u32) {}
}

/// The temporary files of a job, deleted unless the job commits them.
struct Outputs {
    /// What the session writes: a hidden file next to the output, or for an image
    /// sequence a hidden folder holding the frames under their final names.
    temp: PathBuf,
    folder: Option<PathBuf>,
    output: PathBuf,
    sidecar: Option<(PathBuf, PathBuf)>,
    committed: bool,
}

impl Outputs {
    fn new(id: JobId, output: &Path, sequence: bool) -> Self {
        let parent = match output.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let name = output
            .file_name()
            .map_or_else(|| "export".into(), |n| n.to_string_lossy().into_owned());
        let tag = format!("{}-{}", std::process::id(), id.0);
        if sequence {
            // The name may be a printf pattern; the folder's must not be.
            let folder = parent.join(format!(".{}.{tag}.part", name.replace('%', "")));
            Self {
                temp: folder.join(&name),
                folder: Some(folder),
                output: output.to_path_buf(),
                sidecar: None,
                committed: false,
            }
        } else {
            Self {
                temp: parent.join(format!(".{name}.{tag}.part")),
                folder: None,
                output: output.to_path_buf(),
                sidecar: None,
                committed: false,
            }
        }
    }

    fn with_sidecar(&mut self, id: JobId, path: PathBuf) {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{}-{}.part", std::process::id(), id.0));
        let mut hidden = std::ffi::OsString::from(".");
        hidden.push(name);
        let temp = path.with_file_name(hidden);
        self.sidecar = Some((temp, path));
    }

    /// Moves everything to its final path. Returns the files written.
    fn commit(&mut self, written: &[PathBuf]) -> Result<Vec<PathBuf>, ExportFailure> {
        let rename = |from: &Path, to: &Path| {
            std::fs::rename(from, to).map_err(|e| {
                ExportFailure::new("export.error.rename")
                    .with("path", to.display())
                    .with("detail", e)
            })
        };
        let mut files = Vec::new();
        if let Some((temp, path)) = &self.sidecar {
            rename(temp, path)?;
            files.push(path.clone());
        }
        let moved = match &self.folder {
            None => rename(&self.temp, &self.output).map(|()| vec![self.output.clone()]),
            Some(folder) => {
                let parent = folder.parent().unwrap_or(Path::new("."));
                let mut moved = Vec::with_capacity(written.len());
                let mut result = Ok(());
                for file in written {
                    let to = parent.join(file.file_name().unwrap_or_default());
                    if let Err(e) = rename(file, &to) {
                        result = Err(e);
                        break;
                    }
                    moved.push(to);
                }
                match result {
                    Ok(()) => Ok(moved),
                    Err(e) => {
                        for file in &moved {
                            let _ = std::fs::remove_file(file);
                        }
                        Err(e)
                    }
                }
            }
        };
        match moved {
            Ok(moved) => {
                self.committed = true;
                if let Some(folder) = &self.folder {
                    let _ = std::fs::remove_dir_all(folder);
                }
                files.splice(0..0, moved);
                Ok(files)
            }
            Err(e) => {
                if let Some((_, path)) = &self.sidecar {
                    let _ = std::fs::remove_file(path);
                }
                Err(e)
            }
        }
    }
}

impl Drop for Outputs {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        match &self.folder {
            Some(folder) => {
                let _ = std::fs::remove_dir_all(folder);
            }
            None => {
                let _ = std::fs::remove_file(&self.temp);
            }
        }
        if let Some((temp, _)) = &self.sidecar {
            let _ = std::fs::remove_file(temp);
        }
    }
}

/// Export frame `k` to timeline frames and times, and samples of silence.
#[derive(Clone, Copy, Debug)]
struct FrameMap {
    start: i64,
    end: i64,
    /// Seconds per timeline frame.
    timeline: (i64, i64),
    /// Seconds per output frame.
    output: (i64, i64),
}

impl FrameMap {
    fn frames(&self) -> u64 {
        let (tn, td) = (i128::from(self.timeline.0), i128::from(self.timeline.1));
        let (on, od) = (i128::from(self.output.0), i128::from(self.output.1));
        let len = i128::from(self.end - self.start);
        // len * tn/td seconds over on/od seconds per frame, to the nearest frame.
        let num = len * tn * od;
        let den = td * on;
        u64::try_from((2 * num + den) / (2 * den))
            .unwrap_or(0)
            .max(1)
    }

    /// The timeline frame showing at the start of output frame `k`.
    fn timeline_frame(&self, k: u64) -> i64 {
        let (tn, td) = (i128::from(self.timeline.0), i128::from(self.timeline.1));
        let (on, od) = (i128::from(self.output.0), i128::from(self.output.1));
        let offset = i128::from(k) * on * td / (od * tn);
        (self.start + i64::try_from(offset).unwrap_or(i64::MAX - self.start)).min(self.end - 1)
    }

    /// Output frame `k`'s time from the range start, in whole milliseconds (nearest).
    fn millis(&self, k: u64) -> i64 {
        let (on, od) = (i128::from(self.output.0), i128::from(self.output.1));
        let num = i128::from(k) * on * 1000;
        i64::try_from((2 * num + od) / (2 * od)).unwrap_or(i64::MAX)
    }

    /// Audio samples at `rate` up to the end of output frame `k`.
    fn samples_through(&self, k: u64, rate: u32) -> u64 {
        let (on, od) = (i128::from(self.output.0), i128::from(self.output.1));
        let num = i128::from(k + 1) * on * i128::from(rate);
        u64::try_from((2 * num + od) / (2 * od)).unwrap_or(0)
    }

    fn seconds(&self, frames: u64) -> f64 {
        frames as f64 * self.output.0 as f64 / self.output.1 as f64
    }
}

struct Runner<'a> {
    shared: &'a Shared,
    id: JobId,
    control: &'a Arc<JobControl>,
    progress: Reporter<'a>,
}

impl Runner<'_> {
    fn checkpoint(&mut self) -> Result<(), Stop> {
        if self.control.is_paused() && !self.control.is_cancelled() {
            self.progress.time.pause();
            self.shared.emit(
                self.id,
                EventKind::Paused {
                    frames_done: self.progress.done,
                },
            );
            let running = self.control.wait_running();
            self.progress.time.resume();
            if running {
                self.shared.emit(self.id, EventKind::Resumed);
            }
        }
        if self.control.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        Ok(())
    }

    fn run(&mut self, job: ExportJob) -> Result<Finished, Stop> {
        self.progress.stage(Stage::Preparing);
        self.checkpoint()?;
        let project = &job.project;
        let sequence = project
            .sequence(job.sequence)
            .ok_or_else(|| ExportFailure::new("export.error.sequence"))?;
        let plan = TimelinePlan::build(project, job.sequence)
            .map_err(|e| ExportFailure::new("export.error.plan").with("detail", e))?;
        let plan = Arc::new(plan);
        let rate = sequence.frame_rate;
        // An audio-only export may run to the end of the audio, past the last picture.
        let mut length = plan.end();
        if job.settings.video.is_none() {
            length = length.max(
                audio_end(sequence, rate)
                    .map_err(|e| ExportFailure::new("export.error.plan").with("detail", e))?,
            );
        }
        let range = job.range.clone().unwrap_or(0..length);
        if range.start < 0 || range.end > length || range.start >= range.end {
            return Err(ExportFailure::new("export.error.range")
                .with("start", range.start)
                .with("end", range.end)
                .with("length", length)
                .into());
        }
        let time_range = (|| -> Result<TimeRange, project::TimeError> {
            TimeRange::between(
                Time::from_frames(range.start, rate)?,
                Time::from_frames(range.end, rate)?,
            )
        })()
        .map_err(|e| ExportFailure::new("export.error.plan").with("detail", e))?;

        let mut settings = job.settings.clone();
        let cues = match job.subtitles.track() {
            Some(track) => {
                let track = sequence
                    .track(track)
                    .ok_or_else(|| ExportFailure::new("export.error.subtitle_track"))?;
                subtitle_export::track_cues(track, Some(time_range))
                    .map_err(|e| ExportFailure::new("export.error.plan").with("detail", e))?
            }
            None => Vec::new(),
        };
        if let SubtitleExport::Embedded {
            language, title, ..
        } = &job.subtitles
        {
            if !settings.container.accepts_subtitles() {
                return Err(ExportFailure::new("export.error.subtitle_container")
                    .with("container", settings.container.name())
                    .into());
            }
            let mut stream = SubtitleSettings::new(language.clone());
            stream.title = title.clone();
            settings.subtitles.push(stream);
        }

        let tl = plan.frame_duration();
        let map = match &settings.video {
            Some(v) => FrameMap {
                start: range.start,
                end: range.end,
                timeline: (i64::from(tl.num), i64::from(tl.den)),
                output: (i64::from(v.frame_rate.den), i64::from(v.frame_rate.num)),
            },
            None => FrameMap {
                start: range.start,
                end: range.end,
                timeline: (i64::from(tl.num), i64::from(tl.den)),
                output: (i64::from(tl.num), i64::from(tl.den)),
            },
        };
        let total = map.frames();
        self.progress.total = total;

        let sequence_output = settings.container == Container::ImageSequence;
        let mut outputs = Outputs::new(self.id, &job.output, sequence_output);
        if let SubtitleExport::Sidecar { format, .. } = &job.subtitles {
            outputs.with_sidecar(self.id, sidecar_path(&job.output, *format));
        }
        if let Some(folder) = &outputs.folder {
            std::fs::create_dir_all(folder).map_err(|e| {
                ExportFailure::new("export.error.write")
                    .with("path", folder.display())
                    .with("detail", e)
            })?;
        }
        let mut session = ExportSession::create(&outputs.temp, &settings)
            .map_err(|e| ExportFailure::from_export(&e, true))?;
        if matches!(job.subtitles, SubtitleExport::Embedded { .. }) {
            let stream = settings.subtitles.len() - 1;
            for cue in &cues {
                session
                    .push_subtitle(stream, &media_cue(cue))
                    .map_err(|e| ExportFailure::from_export(&e, false))?;
            }
        }
        self.shared.emit(
            self.id,
            EventKind::Started {
                total_frames: total,
            },
        );
        self.progress.stage(Stage::Rendering);

        let mut audio = match &settings.audio {
            Some(a) => Some(
                MixedAudio::new(project, job.sequence, time_range, a.sample_rate, a.channels)
                    .map_err(|e| ExportFailure::new("export.error.plan").with("detail", e))?,
            ),
            None => None,
        };
        let silent_audio = audio.as_ref().is_some_and(|a| a.silent);
        match &settings.video {
            Some(video) => {
                let depth = session.pixel_format().map_or(8, |f| f.bit_depth());
                let format = readback_format(&self.shared.device, depth);
                let (primaries, transfer) = output_encoding(video.colour);
                let burn = match &job.subtitles {
                    SubtitleExport::BurnedIn { style, fonts, .. } => Some(BurnSetup {
                        cues: cues.iter().map(media_cue).collect(),
                        style: style.clone(),
                        fonts: fonts.clone(),
                    }),
                    _ => None,
                };
                let working = WorkingSpace::for_project(project.settings().colour_space.is_hdr());
                let target = Target {
                    size: (video.width, video.height),
                    primaries,
                    transfer,
                    format,
                };
                self.encode_video(&plan, map, target, working, burn, &mut session, &mut audio)?;
            }
            None => self.encode_audio_only(map, &mut session, &mut audio)?,
        }

        self.progress.stage(Stage::Finishing);
        let summary = session
            .finish()
            .map_err(|e| ExportFailure::from_export(&e, false))?;
        let fps = self.progress.fps();
        self.checkpoint()?;

        self.progress.stage(Stage::Verifying);
        let mut expected = Expected::new(&settings, total, map.seconds(total));
        if let Some(preset) = job.preset {
            expected = expected.with_preset(preset);
        }
        if silent_audio {
            // No audio clip plays in the range: silence cannot meet a loudness target and
            // there is nothing to measure.
            expected.loudness = None;
        }
        let report = match verify::verify(&outputs.temp, &expected, &JobVerifyControl(self.control))
        {
            Ok(report) => Some(report),
            Err(VerifyError::Stopped) => return Err(Stop::Cancelled),
            Err(VerifyError::ImageSequence) => None,
        };
        self.checkpoint()?;

        if let (Some((temp, path)), SubtitleExport::Sidecar { format, .. }) =
            (&outputs.sidecar, &job.subtitles)
        {
            let text = match format {
                super::SidecarFormat::Srt => subtitle_export::to_srt(&cues),
                super::SidecarFormat::Vtt => subtitle_export::to_vtt(&cues),
            };
            std::fs::write(temp, text).map_err(|e| {
                ExportFailure::new("export.error.write")
                    .with("path", path.display())
                    .with("detail", e)
            })?;
        }
        let files = outputs.commit(&summary.files)?;
        let findings = report.as_ref().map_or(0, |r| r.findings.len());
        let path = files.first().cloned().unwrap_or_else(|| job.output.clone());
        let elapsed = self.progress.time.elapsed();
        Ok(Finished {
            path,
            sidecar: outputs.sidecar.as_ref().map(|(_, p)| p.clone()),
            files,
            frames: self.progress.done,
            elapsed,
            fps,
            report,
            video_encoder: summary.video_encoder,
            fallbacks: summary.fallbacks,
            offline_media: plan.missing_media(),
            silent_audio,
            notification: Notification::finished(&job.display_name(), findings),
        })
    }

    /// Pushes the mixed audio up to the end of output frame `k`.
    fn push_audio(
        &self,
        session: &mut ExportSession,
        map: &FrameMap,
        k: u64,
        audio: &mut Option<MixedAudio>,
    ) -> Result<(), Stop> {
        let Some(audio) = audio else {
            return Ok(());
        };
        let through = map.samples_through(k, audio.rate);
        let count = usize::try_from(through.saturating_sub(audio.pushed)).unwrap_or(0);
        if count == 0 {
            return Ok(());
        }
        audio.mix.render_planar(count, &mut audio.planes);
        // Channels the mix layout lacks (a count other than 1, 2 or 6) get silence.
        for plane in audio.planes.iter_mut().skip(audio.mix.channels()) {
            plane.clear();
            plane.resize(count, 0.0);
        }
        let planes: Vec<&[f32]> = audio.planes[..audio.channels]
            .iter()
            .map(Vec::as_slice)
            .collect();
        session
            .push_audio(&planes)
            .map_err(|e| ExportFailure::from_export(&e, false))?;
        audio.pushed = through;
        Ok(())
    }

    fn encode_audio_only(
        &mut self,
        map: FrameMap,
        session: &mut ExportSession,
        audio: &mut Option<MixedAudio>,
    ) -> Result<(), Stop> {
        for k in 0..self.progress.total {
            self.checkpoint()?;
            self.push_audio(session, &map, k, audio)?;
            self.progress.frame_done();
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_video(
        &mut self,
        plan: &Arc<TimelinePlan>,
        map: FrameMap,
        target: Target,
        working: WorkingSpace,
        burn: Option<BurnSetup>,
        session: &mut ExportSession,
        audio: &mut Option<MixedAudio>,
    ) -> Result<(), Stop> {
        let config = &self.shared.config;
        let canvas = plan.canvas();
        let rate = plan.rate();
        let options = MediaDecoderOptions {
            project_rate: media::Rational::new(rate.num, rate.den),
            ..config.decoder
        };
        let gops = GopIndexes::new();
        let (lookahead, frames) = Lookahead::start(
            plan.segments().clone(),
            LookaheadConfig {
                decode_workers: config.decode_workers,
                depth: config
                    .depth
                    .unwrap_or_else(|| default_depth(canvas.0, canvas.1, 8)),
                frame_duration: plan.frame_duration(),
                registry: config.registry.clone(),
            },
            |_| MediaDecoder::new(options, gops.clone()),
        )
        .map_err(|e| ExportFailure::new("export.error.thread").with("detail", e))?;
        if map.start > 0 {
            lookahead.seek(map.start);
        }
        let (tx, rx) = mpsc::sync_channel(config.encode_queue.max(1));
        let (back_tx, back_rx) = mpsc::channel();
        let render = RenderThread {
            plan: plan.clone(),
            frames,
            map,
            total: self.progress.total,
            target,
            working,
            burn,
            device: self.shared.device.clone(),
            queue: self.shared.queue.clone(),
            slots: config.readback_slots.max(1),
            control: self.control.clone(),
            tx,
            back: back_rx,
        };
        let thread = match config
            .registry
            .spawn(Role::ExportRender, None, move || render.run())
        {
            Ok(thread) => thread,
            Err(e) => {
                lookahead.shutdown();
                return Err(ExportFailure::new("export.error.thread")
                    .with("detail", e)
                    .into());
            }
        };

        let result = self.encode_frames(&rx, &back_tx, map, target, session, audio);
        // Unblocks the render thread if it waits to send, then waits for it.
        drop(rx);
        if result.is_err() {
            self.control.stop_render();
        }
        let _ = thread.join();
        lookahead.shutdown();
        result
    }

    fn encode_frames(
        &mut self,
        rx: &Receiver<Result<ReadbackFrame, ExportFailure>>,
        back: &mpsc::Sender<ReadbackFrame>,
        map: FrameMap,
        target: Target,
        session: &mut ExportSession,
        audio: &mut Option<MixedAudio>,
    ) -> Result<(), Stop> {
        let total = self.progress.total;
        for k in 0..total {
            self.checkpoint()?;
            let frame = match threads::idle(|| rx.recv()) {
                Ok(Ok(frame)) => frame,
                Ok(Err(failure)) => return Err(failure.into()),
                Err(_) => {
                    self.checkpoint()?;
                    return Err(ExportFailure::new("export.error.render")
                        .with(
                            "detail",
                            format!("rendering stopped after {k} of {total} frames"),
                        )
                        .into());
                }
            };
            debug_assert_eq!(frame.timestamp, k);
            self.checkpoint()?;
            let (w, h) = target.size;
            let picture = match target.format {
                wgpu::TextureFormat::Rgba16Unorm => {
                    Picture::rgba16(w, h, &frame.data, w as usize * 8)
                }
                wgpu::TextureFormat::Rgb10a2Unorm => {
                    Picture::rgb10a2(w, h, &frame.data, w as usize * 4)
                }
                _ => Picture::rgba8(w, h, &frame.data, w as usize * 4),
            };
            session
                .push_picture(&picture)
                .map_err(|e| ExportFailure::from_export(&e, false))?;
            let _ = back.send(frame);
            self.push_audio(session, &map, k, audio)?;
            self.progress.frame_done();
        }
        Ok(())
    }
}

/// Timeline frames until the end of the last item on an audio track, rounded up.
fn audio_end(
    sequence: &project::Sequence,
    rate: project::Rational,
) -> Result<i64, project::TimeError> {
    let mut end = 0;
    for track in sequence
        .tracks
        .iter()
        .filter(|t| t.kind == project::TrackKind::Audio)
    {
        let time = track.end()?;
        let frames = time.frames_floor(rate);
        end = end.max(if Time::from_frames(frames, rate)? < time {
            frames + 1
        } else {
            frames
        });
    }
    Ok(end)
}

/// The export's audio: the sequence mix (`engine::mix`, every track at its defaults) at the
/// settings' rate and channels, from the range's first sample.
struct MixedAudio {
    mix: OfflineMix,
    planes: Vec<Vec<f32>>,
    rate: u32,
    channels: usize,
    /// Samples pushed so far.
    pushed: u64,
    /// No audio clip plays in the range.
    silent: bool,
}

impl MixedAudio {
    fn new(
        project: &project::Project,
        sequence: project::SequenceId,
        range: TimeRange,
        rate: u32,
        channels: usize,
    ) -> Result<Self, crate::mix::MixError> {
        let format = MixFormat {
            sample_rate: rate,
            layout: Layout::for_channels(channels),
        };
        let start = sample_of(range.start, rate)?;
        let end = sample_of(range.end()?, rate)?;
        let mix = OfflineMix::new(project, sequence, &MixSettings::default(), format, start)?;
        Ok(Self {
            silent: !mix.plays_between(start, end),
            planes: vec![Vec::new(); format.channels().max(channels)],
            mix,
            rate,
            channels,
            pushed: 0,
        })
    }
}

/// The 8-bit, 10-bit or 16-bit read-back format for an encoder of `depth` bits.
fn readback_format(device: &wgpu::Device, depth: u8) -> wgpu::TextureFormat {
    let unorm16 = device
        .features()
        .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
    match depth {
        0..=8 => wgpu::TextureFormat::Rgba8Unorm,
        9..=10 => wgpu::TextureFormat::Rgb10a2Unorm,
        _ if unorm16 => wgpu::TextureFormat::Rgba16Unorm,
        _ => wgpu::TextureFormat::Rgb10a2Unorm,
    }
}

fn media_cue(cue: &Cue) -> SubtitleCue {
    SubtitleCue {
        start_ms: cue.start_ms,
        end_ms: cue.end_ms,
        text: cue.text.clone(),
    }
}

/// The delivered picture.
#[derive(Clone, Copy, Debug)]
struct Target {
    size: (u32, u32),
    primaries: Primaries,
    transfer: OutputTransfer,
    format: wgpu::TextureFormat,
}

struct BurnSetup {
    cues: Vec<SubtitleCue>,
    style: SubtitleStyle,
    fonts: Vec<PathBuf>,
}

/// `glass-export-render`: the job's frames from the look-ahead through the timeline
/// renderer into the read-back ring, then to `glass-export`.
struct RenderThread {
    plan: Arc<TimelinePlan>,
    frames: LookaheadFrames<VideoFrame>,
    map: FrameMap,
    total: u64,
    target: Target,
    working: WorkingSpace,
    burn: Option<BurnSetup>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    slots: usize,
    control: Arc<JobControl>,
    tx: SyncSender<Result<ReadbackFrame, ExportFailure>>,
    back: Receiver<ReadbackFrame>,
}

/// The read-back ring and the submissions whose frames it holds, oldest first.
struct InFlight {
    ring: ReadbackRing,
    submissions: VecDeque<wgpu::SubmissionIndex>,
}

impl RenderThread {
    fn run(mut self) {
        let tx = self.tx.clone();
        if let Err(Some(failure)) = self.render() {
            let _ = tx.send(Err(failure));
        }
    }

    fn render(&mut self) -> Result<(), Option<ExportFailure>> {
        let mut renderer = TimelineRenderer::new(
            &self.device,
            &self.queue,
            RendererConfig {
                working: self.working,
                ..RendererConfig::new(self.plan.canvas())
            },
        );
        let mut flight = InFlight {
            ring: ReadbackRing::new(ReadbackConfig {
                slots: self.slots,
                policy: Backpressure::Lossless,
            }),
            submissions: VecDeque::with_capacity(self.slots),
        };
        let (w, h) = self.target.size;
        flight
            .ring
            .reserve(&self.device, w, h, self.target.format)
            .map_err(|e| Some(ExportFailure::new("export.error.render").with("detail", e)))?;
        let mut text = match &self.burn {
            Some(burn) => Some(BurnText::new(&self.device, &self.queue, burn)?),
            None => None,
        };
        let mut layers: Vec<Layer<VideoFrame>> = Vec::new();
        let mut holding: Option<i64> = None;
        for k in 0..self.total {
            if !self.control.wait_running() || self.control.render_stopped() {
                return Err(None);
            }
            let t = self.map.timeline_frame(k);
            while holding.is_none_or(|h| h < t) {
                match self.frames.next_into_timeout(&mut layers, LOOKAHEAD_POLL) {
                    Ok(stamp) => holding = Some(stamp.index),
                    Err(FramesError::Timeout) => {
                        if self.control.is_cancelled() || self.control.render_stopped() {
                            return Err(None);
                        }
                    }
                    // Past the last segment: nothing to composite.
                    Err(FramesError::Finished) => {
                        layers.clear();
                        holding = Some(t);
                    }
                    Err(FramesError::Disconnected) => {
                        return Err(Some(
                            ExportFailure::new("export.error.render")
                                .with("detail", "the decoders stopped"),
                        ));
                    }
                }
            }
            let prepared = match (&mut text, &self.burn) {
                (Some(text), Some(burn)) => {
                    text.prepare(burn, self.map.millis(k), self.target.size)
                }
                _ => None,
            };
            let output = FrameOutput {
                size: self.target.size,
                primaries: self.target.primaries,
                transfer: self.target.transfer,
                format: self.target.format,
                burn_in: match (&text, &prepared) {
                    (Some(text), Some(prepared)) => Some((&text.atlas, prepared)),
                    _ => None,
                },
            };
            let (texture, _) = renderer.render_output(&self.plan, t, &layers, &output);
            self.record(&mut flight, &texture, k)?;
            self.deliver_ready(&mut flight)?;
        }
        while flight.ring.in_flight() > 0 {
            self.recycle(&mut flight);
            match flight.ring.wait(&self.device) {
                Ok(Some(frame)) => {
                    flight.submissions.pop_front();
                    self.send(frame)?;
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(Some(
                        ExportFailure::new("export.error.render").with("detail", e),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Copies `texture` into a free read-back buffer, first handing finished frames on
    /// while every buffer is taken.
    fn record(
        &self,
        flight: &mut InFlight,
        texture: &wgpu::Texture,
        k: u64,
    ) -> Result<(), Option<ExportFailure>> {
        loop {
            self.recycle(flight);
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("export readback"),
                });
            match flight.ring.record(&self.device, &mut encoder, texture, k) {
                Ok(_) => {
                    let submission = self.queue.submit([encoder.finish()]);
                    flight.submissions.push_back(submission);
                    return Ok(());
                }
                Err(render::readback::SubmitError::Full) => {
                    if let Some(oldest) = flight.submissions.front() {
                        let _ = self.device.poll(wgpu::PollType::Wait {
                            submission_index: Some(oldest.clone()),
                            timeout: None,
                        });
                    }
                    if !self.deliver_ready(flight)? {
                        // Mapped after the poll: wait for it outright.
                        match flight.ring.wait(&self.device) {
                            Ok(Some(frame)) => {
                                flight.submissions.pop_front();
                                self.send(frame)?;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                return Err(Some(
                                    ExportFailure::new("export.error.render").with("detail", e),
                                ));
                            }
                        }
                    }
                }
                Err(e) => {
                    return Err(Some(
                        ExportFailure::new("export.error.render").with("detail", e),
                    ));
                }
            }
        }
    }

    /// Hands every finished frame, oldest first, to `glass-export`. Returns whether any
    /// was.
    fn deliver_ready(&self, flight: &mut InFlight) -> Result<bool, Option<ExportFailure>> {
        let mut any = false;
        let mut polled = false;
        loop {
            self.recycle(flight);
            let next = if polled {
                flight.ring.try_take()
            } else {
                polled = true;
                flight.ring.poll(&self.device)
            };
            match next {
                Ok(Some(frame)) => {
                    flight.submissions.pop_front();
                    self.send(frame)?;
                    any = true;
                }
                Ok(None) => return Ok(any),
                Err(e) => {
                    return Err(Some(
                        ExportFailure::new("export.error.render").with("detail", e),
                    ));
                }
            }
        }
    }

    fn recycle(&self, flight: &mut InFlight) {
        while let Ok(frame) = self.back.try_recv() {
            flight.ring.recycle(frame);
        }
    }

    fn send(&self, frame: ReadbackFrame) -> Result<(), Option<ExportFailure>> {
        threads::blocked(|| self.tx.send(Ok(frame))).map_err(|_| None::<ExportFailure>)
    }
}

/// The burned-in subtitle's text engine, atlas and laid-out cues.
struct BurnText {
    engine: TextEngine,
    atlas: GlyphAtlas,
    laid_out: HashMap<usize, BurnIn>,
}

impl BurnText {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        burn: &BurnSetup,
    ) -> Result<Self, Option<ExportFailure>> {
        let engine = if burn.fonts.is_empty() {
            TextEngine::system()
        } else {
            let paths: Vec<&Path> = burn.fonts.iter().map(PathBuf::as_path).collect();
            TextEngine::with_font_files(&paths)
                .map_err(|e| Some(ExportFailure::new("export.error.render").with("detail", e)))?
        };
        Ok(Self {
            engine,
            atlas: GlyphAtlas::new(device, queue),
            laid_out: HashMap::new(),
        })
    }

    /// The cue showing at `ms`, prepared for this frame.
    fn prepare(
        &mut self,
        burn: &BurnSetup,
        ms: i64,
        size: (u32, u32),
    ) -> Option<render::text::PreparedText> {
        let cue = subtitle::cue_at(&burn.cues, ms)?;
        let index = burn.cues.partition_point(|c| c.start_ms < cue.start_ms);
        let engine = &mut self.engine;
        let laid = self
            .laid_out
            .entry(index)
            .or_insert_with(|| burn.style.lay_out(engine, &cue.text, size));
        self.atlas.begin_frame();
        Some(
            self.atlas
                .prepare(&mut self.engine, &laid.layout, &laid.paint, None),
        )
    }
}
