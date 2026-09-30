//! The importer: scans and probes on its own `glass-import` thread and reports to the UI
//! through a channel the UI drains once per frame, so dropping a card or a folder of a
//! thousand files never stalls a frame. Each clip is announced as soon as it is probed
//! (a few milliseconds per file), so the pool fills in while the import runs.
//!
//! An import is two steps, and each is one [`Importer`]:
//!
//! 1. [`Importer::scan`] walks the dropped paths into an [`ImportPlan`] (directory
//!    listings and sizes only), for the folder import dialog or a direct drop.
//! 2. [`Importer::import`] probes every new clip in a plan, hashes it (the analysis
//!    cache key, also used to skip files already in the project under another path),
//!    reads its metadata (sidecars, container tags, EXIF) and hands out [`ImportedClip`]s.
//!
//! Both check a cancel flag between files. [`AfterImport`] hooks run on the import
//! thread for each clip; that is where scene detection attaches when a clip should be
//! split into scenes on the way in.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use media::{ContentHash, MediaInfo, StreamDetail};
use project::media_pool::{Facts, Metadata};

use super::classify::MediaClass;
use super::scan::{self, ImportPlan, ImportSource, ScanOptions};
use super::sidecar;
use super::tags;

/// Import settings, from the import dialog.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub scan: ScanOptions,
    /// Create a bin per folder (and per card) instead of putting every clip in the
    /// target bin.
    pub keep_folders_as_bins: bool,
    /// Leave out clips already in the project (by path in the plan, by content hash
    /// here).
    pub skip_existing: bool,
    /// Ask the [`AfterImport`] hooks to split long clips into scenes.
    pub split_into_scenes: bool,
    /// Content hashes of the project's media, for `skip_existing`.
    pub known_hashes: HashSet<ContentHash>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            scan: ScanOptions::default(),
            keep_folders_as_bins: true,
            skip_existing: true,
            split_into_scenes: false,
            known_hashes: HashSet::new(),
        }
    }
}

/// One probed clip, ready for the media pool.
#[derive(Clone, Debug)]
pub struct ImportedClip {
    pub source: ImportSource,
    pub hash: ContentHash,
    pub facts: Facts,
    pub metadata: Metadata,
    /// The probe of the first file (`None` for stills FFmpeg does not read, such as
    /// HEIC on some builds; their facts come from the picture header).
    pub info: Option<MediaInfo>,
}

/// Why a clip was not imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// Its content is already in the project (under another path, or twice in the
    /// drop).
    Duplicate,
    /// It could not be read; the message says why.
    Failed(String),
}

/// Progress and results, in order.
#[derive(Clone, Debug)]
pub enum ImportEvent {
    /// The scan finished.
    Planned(Arc<ImportPlan>),
    /// Work on a clip started.
    Started {
        index: usize,
        total: usize,
        name: String,
        file_name: String,
        size_bytes: u64,
        bytes_done: u64,
        bytes_total: u64,
    },
    Imported(Box<ImportedClip>),
    Skipped {
        name: String,
        reason: SkipReason,
    },
    Finished(ImportSummary),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub imported: usize,
    pub duplicates: usize,
    pub failed: usize,
    pub elapsed: Duration,
}

/// Work to run on the import thread for each imported clip. Scene detection registers
/// here to split clips when [`ImportOptions::split_into_scenes`] is on; proxy and
/// analysis scheduling can too.
pub trait AfterImport: Send + Sync {
    fn clip_imported(&self, clip: &ImportedClip, options: &ImportOptions);
}

/// Splits clips into scenes on the way in: when [`ImportOptions::split_into_scenes`] is
/// on, each imported video clip gets a scene cut job ([`crate::analysis::SceneJob`]) on
/// the analysis pool at background priority. The cuts land in the analysis cache, where
/// the split-into-scenes dialog reads them.
pub struct SceneDetectionOnImport {
    pool: Arc<crate::analysis::AnalysisPool>,
    settings: media::analysis::scenes::SceneSettings,
}

impl SceneDetectionOnImport {
    pub fn new(pool: Arc<crate::analysis::AnalysisPool>) -> Self {
        Self {
            pool,
            settings: media::analysis::scenes::SceneSettings::default(),
        }
    }
}

impl AfterImport for SceneDetectionOnImport {
    fn clip_imported(&self, clip: &ImportedClip, options: &ImportOptions) {
        if !options.split_into_scenes || clip.source.class != MediaClass::Video {
            return;
        }
        let job = crate::analysis::SceneJob {
            source: media::SourceFile {
                path: clip.source.primary().to_path_buf(),
                hash: clip.hash,
            },
            settings: self.settings,
        };
        drop(self.pool.submit(job, crate::analysis::Priority::Background));
    }
}

/// A scan or import running on its own thread.
pub struct Importer {
    cancel: Arc<AtomicBool>,
    events: Receiver<ImportEvent>,
    thread: Option<JoinHandle<()>>,
}

impl Importer {
    /// Scans `paths` into a plan; ends with [`ImportEvent::Planned`] or `Cancelled`.
    pub fn scan(paths: Vec<PathBuf>, options: ScanOptions) -> std::io::Result<Self> {
        Self::spawn(
            move |tx, cancel| match scan::scan(&paths, &options, cancel) {
                Ok(plan) => {
                    let _ = tx.send(ImportEvent::Planned(Arc::new(plan)));
                }
                Err(scan::Cancelled) => {
                    let _ = tx.send(ImportEvent::Cancelled);
                }
            },
        )
    }

    /// Imports the new clips of `plan`.
    pub fn import(
        plan: Arc<ImportPlan>,
        options: ImportOptions,
        hooks: Vec<Arc<dyn AfterImport>>,
    ) -> std::io::Result<Self> {
        Self::spawn(move |tx, cancel| run_import(&plan, &options, &hooks, tx, cancel))
    }

    /// Scans and then imports everything that was dropped, the path for a drop onto a
    /// bin (no dialog). Sends `Planned` first.
    pub fn scan_and_import(
        paths: Vec<PathBuf>,
        options: ImportOptions,
        hooks: Vec<Arc<dyn AfterImport>>,
    ) -> std::io::Result<Self> {
        Self::spawn(
            move |tx, cancel| match scan::scan(&paths, &options.scan, cancel) {
                Ok(plan) => {
                    let plan = Arc::new(plan);
                    let _ = tx.send(ImportEvent::Planned(plan.clone()));
                    run_import(&plan, &options, &hooks, tx, cancel);
                }
                Err(scan::Cancelled) => {
                    let _ = tx.send(ImportEvent::Cancelled);
                }
            },
        )
    }

    fn spawn(
        work: impl FnOnce(&Sender<ImportEvent>, &AtomicBool) + Send + 'static,
    ) -> std::io::Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, events) = mpsc::channel();
        let flag = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("glass-import".into())
            .spawn(move || work(&tx, &flag))?;
        Ok(Self {
            cancel,
            events,
            thread: Some(thread),
        })
    }

    /// Events so far, without waiting.
    pub fn poll(&self) -> Vec<ImportEvent> {
        self.events.try_iter().collect()
    }

    /// Stops at the next file; `Cancelled` follows.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Blocks until the thread ends and returns every remaining event (tests and
    /// command-line tools; never on the UI thread).
    pub fn wait(mut self) -> Vec<ImportEvent> {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.events.try_iter().collect()
    }
}

impl Drop for Importer {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn run_import(
    plan: &ImportPlan,
    options: &ImportOptions,
    hooks: &[Arc<dyn AfterImport>],
    tx: &Sender<ImportEvent>,
    cancel: &AtomicBool,
) {
    let start = Instant::now();
    let sources: Vec<&ImportSource> = plan
        .sources
        .iter()
        .filter(|s| !(options.skip_existing && s.already_in_project))
        .collect();
    let bytes_total: u64 = sources.iter().map(|s| s.size_bytes).sum();
    let mut bytes_done = 0;
    let mut seen: HashSet<ContentHash> = HashSet::new();
    let mut summary = ImportSummary::default();
    for (index, source) in sources.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(ImportEvent::Cancelled);
            return;
        }
        let _ = tx.send(ImportEvent::Started {
            index,
            total: sources.len(),
            name: source.name.clone(),
            file_name: file_name(source.primary()),
            size_bytes: source.size_bytes,
            bytes_done,
            bytes_total,
        });
        bytes_done += source.size_bytes;
        match import_one(source) {
            Ok(clip) => {
                let duplicate = !seen.insert(clip.hash)
                    || (options.skip_existing && options.known_hashes.contains(&clip.hash));
                if duplicate {
                    summary.duplicates += 1;
                    let _ = tx.send(ImportEvent::Skipped {
                        name: source.name.clone(),
                        reason: SkipReason::Duplicate,
                    });
                    continue;
                }
                for hook in hooks {
                    hook.clip_imported(&clip, options);
                }
                summary.imported += 1;
                let _ = tx.send(ImportEvent::Imported(Box::new(clip)));
            }
            Err(message) => {
                tracing::warn!(clip = %source.name, %message, "import: clip not readable");
                summary.failed += 1;
                let _ = tx.send(ImportEvent::Skipped {
                    name: source.name.clone(),
                    reason: SkipReason::Failed(message),
                });
            }
        }
    }
    summary.elapsed = start.elapsed();
    let _ = tx.send(ImportEvent::Finished(summary));
}

/// Probes, hashes and reads the sidecars of one clip.
pub fn import_one(source: &ImportSource) -> Result<ImportedClip, String> {
    let primary = source.primary();
    let hash = if source.class == MediaClass::Sequence {
        primary
            .parent()
            .map_or_else(|| ContentHash::of_file(primary), ContentHash::of_folder)
    } else {
        ContentHash::of_file(primary)
    }
    .map_err(|e| e.to_string())?;

    let (info, mut facts) = match media::probe(primary) {
        Ok(info) => {
            let facts = facts_of(&info);
            (Some(info), facts)
        }
        Err(error) if matches!(source.class, MediaClass::Still | MediaClass::Sequence) => {
            let options = media::SourceOptions::new(media::Rational::new(25, 1));
            let picture =
                media::VideoSource::open(primary, options).map_err(|_| error.to_string())?;
            let i = picture.info();
            (
                None,
                Facts {
                    width: i.width,
                    height: i.height,
                    bit_depth: i.bit_depth,
                    chroma: Some("RGB".into()),
                    ..Facts::default()
                },
            )
        }
        Err(error) => return Err(error.to_string()),
    };

    match source.class {
        MediaClass::Sequence => {
            facts.sequence_frames = Some(source.files.len() as u64);
            facts.duration_s = None;
        }
        MediaClass::Still => facts.duration_s = None,
        MediaClass::Video | MediaClass::Audio if source.files.len() > 1 => {
            // A spanned recording: its length is the sum of its files.
            let mut total = facts.duration_s.unwrap_or(0.0);
            for file in &source.files[1..] {
                let part = media::probe(file).map_err(|e| e.to_string())?;
                total += part.duration.map_or(0.0, |d| d.seconds());
            }
            facts.duration_s = Some(total);
        }
        _ => {}
    }

    let container = info.as_ref().map(tags::from_probe);
    let exif = matches!(source.class, MediaClass::Still | MediaClass::Sequence)
        .then(|| media::exif::read_exif(primary))
        .flatten()
        .map(|e| tags::from_exif(&e));
    let metadata = tags::merge(
        sidecar::read_all(&source.sidecars),
        container.as_ref(),
        exif.as_ref(),
    );
    Ok(ImportedClip {
        source: source.clone(),
        hash,
        facts,
        metadata,
        info,
    })
}

/// The pool's copy of a probe.
pub fn facts_of(info: &MediaInfo) -> Facts {
    let mut facts = Facts {
        duration_s: info.duration.map(|d| d.seconds()).filter(|s| *s > 0.0),
        container: Some(info.container.clone()),
        ..Facts::default()
    };
    if let Some((stream, video)) = info.video() {
        let (w, h) = video.display_size();
        facts.width = w;
        facts.height = h;
        facts.frame_rate = video
            .frame_rate
            .filter(|r| r.is_valid())
            .map(|r| (r.num, r.den));
        facts.video_codec = Some(stream.codec.name.to_string());
        facts.video_profile = stream.codec.profile.map(str::to_string);
        facts.bit_depth = video.bit_depth;
        facts.chroma = video.pixel_format.map(|f| chroma_of(f.name()).to_string());
        facts.primaries = Some(video.colour_space.primaries.name().to_string());
        facts.hdr = match video.colour_space.hdr {
            media::Hdr::Pq => Some("pq".into()),
            media::Hdr::Hlg => Some("hlg".into()),
            media::Hdr::None => None,
        };
    }
    if let Some((stream, audio)) = info.audio() {
        facts.audio_codec = Some(stream.codec.name.to_string());
        facts.audio_channels = audio.channels;
        facts.sample_rate = audio.sample_rate;
        facts.audio_bit_depth = audio.bit_depth;
    }
    // Some containers (WAV) report no duration but their audio stream does.
    if facts.duration_s.is_none() {
        facts.duration_s = info
            .streams
            .iter()
            .filter(|s| matches!(s.detail, StreamDetail::Video(_) | StreamDetail::Audio(_)))
            .filter_map(|s| s.duration.map(|d| d.seconds()))
            .fold(None, |a: Option<f64>, d| Some(a.map_or(d, |a| a.max(d))));
    }
    facts
}

/// Chroma subsampling from an FFmpeg pixel format name.
fn chroma_of(pixel_format: &str) -> &'static str {
    if pixel_format.starts_with("gbr")
        || pixel_format.starts_with("rgb")
        || pixel_format.starts_with("bgr")
        || pixel_format.starts_with("argb")
        || pixel_format.starts_with("abgr")
    {
        "RGB"
    } else if pixel_format.contains("444") {
        "4:4:4"
    } else if pixel_format.contains("422") || pixel_format.starts_with("p2") {
        "4:2:2"
    } else if pixel_format.contains("411") {
        "4:1:1"
    } else {
        "4:2:0"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chroma_names() {
        assert_eq!(chroma_of("yuv420p"), "4:2:0");
        assert_eq!(chroma_of("yuv422p10le"), "4:2:2");
        assert_eq!(chroma_of("p210le"), "4:2:2");
        assert_eq!(chroma_of("nv12"), "4:2:0");
        assert_eq!(chroma_of("yuv444p12le"), "4:4:4");
        assert_eq!(chroma_of("gbrp"), "RGB");
    }
}
