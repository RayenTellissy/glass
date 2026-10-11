//! Offline media: which of a project's source files can be used right now, why the others
//! cannot, and what stands in for them.
//!
//! [`check_project`] looks at every [`MediaRef`]: a path that is gone, a file that cannot
//! be opened (or a folder that cannot be listed) and, with [`Verify::Content`], a file
//! whose content hash no longer matches the one recorded at import are offline, each with
//! an [`OfflineReason`]. Nothing in the project changes: availability is a separate map
//! the engine and UI read, so an offline clip keeps its place, trims and effects, and the
//! project loads, edits, saves and undoes exactly as before. Relinking ([`super::relink`])
//! points the reference at a new path, after which a new check brings it back online.
//!
//! In place of an offline clip's pictures the compositor imports a
//! [`render::slate`] frame labelled by [`slate_label`], at the clip's size, for every
//! frame of the clip's range ([`offline_clips`]).
//!
//! Checking reads the disk (a stat and an open per file, plus up to 2.5 MiB per file for
//! the content hash), so it runs off the UI thread.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::path::Path;

use media::ContentHash;
use project::{ItemId, MediaId, MediaRef, Project, SequenceId, TimeRange};
use render::slate::SlateLabel;

/// How far [`check_media`] looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Verify {
    /// The path exists and opens for reading.
    Quick,
    /// Also that the content hash matches the one recorded at import (when one was).
    #[default]
    Content,
}

/// Why a source file cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfflineReason {
    /// Nothing is at the path any more.
    Missing,
    /// Something is there but cannot be read (permissions, a disconnected network
    /// share, a folder where a file was); `error` is the system's message.
    Unreadable { error: String },
    /// The file reads, but its content is not what was imported (replaced or
    /// re-exported under the same name).
    Changed { expected: String, found: String },
}

impl OfflineReason {
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::Missing => "media.offline.reason.missing",
            Self::Unreadable { .. } => "media.offline.reason.unreadable",
            Self::Changed { .. } => "media.offline.reason.changed",
        }
    }

    /// The reason in the current locale, naming `path`.
    pub fn message(&self, path: &str) -> String {
        match self {
            Self::Unreadable { error } => {
                assets::i18n::text_with(self.i18n_key(), &[("path", &path), ("error", error)])
            }
            _ => assets::i18n::text_with(self.i18n_key(), &[("path", &path)]),
        }
    }
}

impl fmt::Display for OfflineReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("missing"),
            Self::Unreadable { error } => write!(f, "unreadable: {error}"),
            Self::Changed { expected, found } => {
                write!(f, "content changed: expected {expected}, found {found}")
            }
        }
    }
}

/// Whether one source file can be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Online,
    Offline(OfflineReason),
}

impl Availability {
    pub fn is_offline(&self) -> bool {
        matches!(self, Self::Offline(_))
    }

    pub fn reason(&self) -> Option<&OfflineReason> {
        match self {
            Self::Online => None,
            Self::Offline(reason) => Some(reason),
        }
    }
}

/// Checks one source file.
pub fn check_media(media: &MediaRef, verify: Verify) -> Availability {
    check_path(Path::new(&media.path), &media.content_hash, verify)
}

fn check_path(path: &Path, content_hash: &str, verify: Verify) -> Availability {
    let unreadable = |e: std::io::Error| {
        Availability::Offline(OfflineReason::Unreadable {
            error: e.to_string(),
        })
    };
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Availability::Offline(OfflineReason::Missing);
        }
        Err(e) => return unreadable(e),
    };
    // An image sequence is a folder; anything else must be a file that opens.
    let opened = if meta.is_dir() {
        std::fs::read_dir(path).map(drop)
    } else {
        File::open(path).map(drop)
    };
    if let Err(e) = opened {
        return unreadable(e);
    }
    if verify == Verify::Quick || content_hash.is_empty() {
        return Availability::Online;
    }
    let hash = if meta.is_dir() {
        ContentHash::of_folder(path)
    } else {
        ContentHash::of_file(path)
    };
    match hash {
        Ok(hash) if hash.to_hex().eq_ignore_ascii_case(content_hash) => Availability::Online,
        Ok(hash) => Availability::Offline(OfflineReason::Changed {
            expected: content_hash.to_owned(),
            found: hash.to_hex(),
        }),
        Err(e) => unreadable(e),
    }
}

/// The availability of every source a project refers to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaAvailability {
    status: BTreeMap<MediaId, Availability>,
}

impl MediaAvailability {
    /// A media id this map has not seen counts as online: it was added after the check.
    pub fn get(&self, id: MediaId) -> &Availability {
        self.status.get(&id).unwrap_or(&Availability::Online)
    }

    pub fn is_offline(&self, id: MediaId) -> bool {
        self.get(id).is_offline()
    }

    /// Offline sources in id order, with their reasons.
    pub fn offline(&self) -> impl Iterator<Item = (MediaId, &OfflineReason)> {
        self.status
            .iter()
            .filter_map(|(id, a)| a.reason().map(|r| (*id, r)))
    }

    pub fn offline_count(&self) -> usize {
        self.offline().count()
    }

    /// Records a new result for one source (after a relink or a file-system event).
    pub fn set(&mut self, id: MediaId, availability: Availability) {
        self.status.insert(id, availability);
    }
}

/// Checks every source of `project`. Does not change the project.
pub fn check_project(project: &Project, verify: Verify) -> MediaAvailability {
    let status = project
        .media()
        .iter()
        .map(|m| {
            let availability = check_media(m, verify);
            if let Availability::Offline(reason) = &availability {
                tracing::info!(media = m.id.0, path = %m.path, %reason, "media offline");
            }
            (m.id, availability)
        })
        .collect();
    MediaAvailability { status }
}

/// A clip whose source is offline: the slate stands in for it over `range`.
#[derive(Clone, Debug, PartialEq)]
pub struct OfflineClip {
    pub sequence: SequenceId,
    pub item: ItemId,
    pub media: MediaId,
    /// The clip's place on the timeline, in sequence time: the slate's duration.
    pub range: TimeRange,
}

/// Every clip in `project` whose source is offline, in sequence, track and time order:
/// the offline state the timeline shows (Phase 6 draws it with `clip/offline`).
pub fn offline_clips(project: &Project, availability: &MediaAvailability) -> Vec<OfflineClip> {
    let mut out = Vec::new();
    for sequence in project.sequences() {
        for track in &sequence.tracks {
            for item in &track.items {
                if let Some(media) = item.clip().and_then(|c| c.source.media())
                    && availability.is_offline(media)
                {
                    out.push(OfflineClip {
                        sequence: sequence.id,
                        item: item.id,
                        media,
                        range: item.range,
                    });
                }
            }
        }
    }
    out
}

/// What the slate for `media` says: "Media offline", the file name (the folder name for
/// an image sequence) and, when the reel or card is known, where it was last seen, then
/// that edits are kept.
pub fn slate_label(media: &MediaRef, reel: Option<&str>) -> SlateLabel {
    let path = Path::new(&media.path);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| media.path.clone());
    let detail = match reel.filter(|r| !r.is_empty()) {
        Some(reel) => {
            assets::i18n::text_with("media.offline.slate.detail_reel", &[("reel", &reel)])
        }
        None => assets::i18n::text("media.offline.slate.detail"),
    };
    SlateLabel {
        title: assets::i18n::text("media.offline.slate.title"),
        file_name,
        detail,
    }
}
