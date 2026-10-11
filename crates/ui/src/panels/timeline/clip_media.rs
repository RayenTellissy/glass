//! What the clip renderer knows about each source's analysis results: thumbnail strips,
//! their atlases as textures, and waveform peaks, keyed by [`MediaId`].
//!
//! The renderer never waits and never reads a file. It asks [`ClipMedia`] for what it
//! needs; whatever is there is drawn, and whatever is missing is drawn as a placeholder
//! and becomes a [`MediaRequest`] in the store's queue (once per thing, however many
//! frames ask). Whoever owns the store drains the queue once a frame:
//!
//! - [`AnalysisFeed::pump`] submits thumbnail and peak requests to the analysis pool
//!   (`engine::analysis`) as [`ThumbnailJob`]s and [`PeaksJob`]s at
//!   [`Priority::Visible`], polls the handles with `try_result` and moves finished
//!   results into the store. A result already on disk comes back on the pool's next
//!   pass without being recomputed.
//! - Atlas requests come back out of [`AnalysisFeed::pump`]: decoding a JPEG atlas
//!   ([`ThumbnailStrips::load_atlas`]) is file I/O, so the caller does it off the UI
//!   thread and hands the image to [`ClipMedia::insert_atlas`], which uploads it as a
//!   texture.
//!
//! Atlases are keyed by level and chunk (`AtlasKey`), so a lookup allocates nothing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};
use engine::analysis::{AnalysisPool, JobError, JobHandle, PeaksJob, Priority, ThumbnailJob};
use engine::media::RgbaImage;
use engine::media::SourceFile;
use engine::media::analysis::peaks::{PeakSettings, Peaks};
use engine::media::analysis::thumbnails::{ThumbnailSettings, ThumbnailStrips};
use project::MediaId;

/// One atlas of a source's thumbnail strips: index into
/// [`ThumbnailStrips::levels`] and the chunk (atlas file) within that level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AtlasKey {
    pub level: u32,
    pub chunk: u32,
}

/// Something the renderer needed and the store did not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaRequest {
    /// The thumbnail strips' index of a source.
    Thumbnails(MediaId),
    /// One atlas of a source's strips (its file name is in the strips' index).
    Atlas { media: MediaId, atlas: AtlasKey },
    /// The waveform peaks of a source.
    Peaks(MediaId),
}

/// An uploaded atlas.
struct Atlas {
    texture: TextureHandle,
    size: [u32; 2],
}

#[derive(Default)]
struct Entry {
    strips: Option<Arc<ThumbnailStrips>>,
    atlases: HashMap<AtlasKey, Atlas>,
    peaks: Option<Arc<Peaks>>,
}

/// The analysis results the timeline has on the UI thread, and the queue of what it
/// still wants.
#[derive(Default)]
pub struct ClipMedia {
    entries: HashMap<MediaId, Entry>,
    /// Everything ever queued, so a request is made once (until [`ClipMedia::forget`]).
    requested: HashSet<MediaRequest>,
    queue: Vec<MediaRequest>,
}

impl ClipMedia {
    pub fn new() -> Self {
        Self::default()
    }

    fn request(&mut self, request: MediaRequest) {
        if self.requested.insert(request) {
            self.queue.push(request);
        }
    }

    /// `media`'s thumbnail strips, or `None` (and a request) while they are not here.
    pub fn strips(&mut self, media: MediaId) -> Option<Arc<ThumbnailStrips>> {
        let strips = self.entries.get(&media).and_then(|e| e.strips.clone());
        if strips.is_none() {
            self.request(MediaRequest::Thumbnails(media));
        }
        strips
    }

    /// An atlas of `media`'s strips as a texture with its size in pixels, or `None` (and
    /// a request) while it is not loaded.
    pub fn atlas(&mut self, media: MediaId, atlas: AtlasKey) -> Option<(TextureId, [u32; 2])> {
        let found = self
            .entries
            .get(&media)
            .and_then(|e| e.atlases.get(&atlas))
            .map(|a| (a.texture.id(), a.size));
        if found.is_none() {
            self.request(MediaRequest::Atlas { media, atlas });
        }
        found
    }

    /// `media`'s waveform peaks, or `None` (and a request) while they are not here.
    pub fn peaks(&mut self, media: MediaId) -> Option<Arc<Peaks>> {
        let peaks = self.entries.get(&media).and_then(|e| e.peaks.clone());
        if peaks.is_none() {
            self.request(MediaRequest::Peaks(media));
        }
        peaks
    }

    pub fn insert_strips(&mut self, media: MediaId, strips: Arc<ThumbnailStrips>) {
        let entry = self.entries.entry(media).or_default();
        // New strips may lay their atlases out differently.
        entry.atlases.clear();
        entry.strips = Some(strips);
        self.requested
            .retain(|r| !matches!(r, MediaRequest::Atlas { media: m, .. } if *m == media));
    }

    /// Uploads a decoded atlas of `media`'s strips.
    pub fn insert_atlas(
        &mut self,
        ctx: &egui::Context,
        media: MediaId,
        atlas: AtlasKey,
        image: &RgbaImage,
    ) {
        let size = [image.width as usize, image.height as usize];
        if image.data.len() != size[0] * size[1] * 4 {
            return;
        }
        let pixels = ColorImage::from_rgba_unmultiplied(size, &image.data);
        let name = format!(
            "glass.clip_thumbs.{}.{}.{}",
            media.0, atlas.level, atlas.chunk
        );
        let texture = ctx.load_texture(name, pixels, TextureOptions::LINEAR);
        self.entries.entry(media).or_default().atlases.insert(
            atlas,
            Atlas {
                texture,
                size: [image.width, image.height],
            },
        );
    }

    pub fn insert_peaks(&mut self, media: MediaId, peaks: Arc<Peaks>) {
        self.entries.entry(media).or_default().peaks = Some(peaks);
    }

    /// Drops everything known about `media` (relinked or removed), so it is asked for
    /// again when next drawn.
    pub fn forget(&mut self, media: MediaId) {
        self.entries.remove(&media);
        self.requested.retain(|r| match r {
            MediaRequest::Thumbnails(m) | MediaRequest::Peaks(m) => *m != media,
            MediaRequest::Atlas { media: m, .. } => *m != media,
        });
        self.queue.retain(|r| match r {
            MediaRequest::Thumbnails(m) | MediaRequest::Peaks(m) => *m != media,
            MediaRequest::Atlas { media: m, .. } => *m != media,
        });
    }

    /// The requests made since the last call, oldest first.
    pub fn take_requests(&mut self) -> Vec<MediaRequest> {
        std::mem::take(&mut self.queue)
    }

    /// Requests waiting to be taken.
    pub fn pending_requests(&self) -> &[MediaRequest] {
        &self.queue
    }
}

/// An atlas the caller loads off the UI thread: read `file` from the source's
/// thumbnail cache entry with [`ThumbnailStrips::load_atlas`], then hand the image to
/// [`ClipMedia::insert_atlas`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtlasLoad {
    pub media: MediaId,
    pub atlas: AtlasKey,
    /// The atlas file name in the strips' cache entry.
    pub file: String,
    pub strips: Arc<ThumbnailStrips>,
}

/// Turns a [`ClipMedia`] store's requests into analysis jobs and their results back
/// into the store. Never blocks: it submits, and polls with `try_result`.
pub struct AnalysisFeed {
    pub thumbnails: ThumbnailSettings,
    pub peaks: PeakSettings,
    pending_strips: Vec<(MediaId, JobHandle<ThumbnailStrips>)>,
    pending_peaks: Vec<(MediaId, JobHandle<Peaks>)>,
}

impl AnalysisFeed {
    pub fn new(thumbnails: ThumbnailSettings, peaks: PeakSettings) -> Self {
        Self {
            thumbnails,
            peaks,
            pending_strips: Vec::new(),
            pending_peaks: Vec::new(),
        }
    }

    /// Jobs submitted and not finished yet.
    pub fn in_flight(&self) -> usize {
        self.pending_strips.len() + self.pending_peaks.len()
    }

    /// Once a frame: submits `media`'s new thumbnail and peak requests (`source` names
    /// each source's file and content hash; a source it does not know is skipped),
    /// moves finished results into `media`, and returns the atlases to load.
    pub fn pump(
        &mut self,
        pool: &AnalysisPool,
        media: &mut ClipMedia,
        source: impl Fn(MediaId) -> Option<SourceFile>,
    ) -> Vec<AtlasLoad> {
        let mut loads = Vec::new();
        for request in media.take_requests() {
            match request {
                MediaRequest::Thumbnails(id) => {
                    if let Some(source) = source(id) {
                        let job = ThumbnailJob {
                            source,
                            settings: self.thumbnails,
                        };
                        self.pending_strips
                            .push((id, pool.submit(job, Priority::Visible)));
                    }
                }
                MediaRequest::Peaks(id) => {
                    if let Some(source) = source(id) {
                        let job = PeaksJob {
                            source,
                            settings: self.peaks,
                        };
                        self.pending_peaks
                            .push((id, pool.submit(job, Priority::Visible)));
                    }
                }
                MediaRequest::Atlas { media: id, atlas } => {
                    let strips = media.entries.get(&id).and_then(|e| e.strips.clone());
                    let file = strips.as_ref().and_then(|s| {
                        s.levels
                            .get(atlas.level as usize)?
                            .atlases
                            .get(atlas.chunk as usize)
                            .cloned()
                    });
                    if let (Some(strips), Some(file)) = (strips, file) {
                        loads.push(AtlasLoad {
                            media: id,
                            atlas,
                            file,
                            strips,
                        });
                    }
                }
            }
        }
        self.pending_strips.retain(|(id, handle)| {
            finished(handle, "thumbnails", |strips| {
                media.insert_strips(*id, strips)
            })
        });
        self.pending_peaks.retain(|(id, handle)| {
            finished(handle, "peaks", |peaks| media.insert_peaks(*id, peaks))
        });
        loads
    }
}

/// Hands a finished job's result to `done`; returns whether the job is still running.
fn finished<T: Send + Sync + 'static>(
    handle: &JobHandle<T>,
    what: &str,
    done: impl FnOnce(Arc<T>),
) -> bool {
    match handle.try_result() {
        None => true,
        Some(Ok(value)) => {
            done(value);
            false
        }
        Some(Err(JobError::Cancelled)) => false,
        Some(Err(e)) => {
            tracing::warn!("timeline {what} for {} failed: {e}", handle.key().hash);
            false
        }
    }
}
