//! What a file is, judged by its name alone: the import scan and the drop target run on
//! paths before anything is opened, so this never touches the disk.

use std::path::Path;

/// The kind of media a file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MediaClass {
    Video,
    Audio,
    Still,
    /// A numbered run of stills imported as one clip.
    Sequence,
}

/// A file that looks like media Glass cannot read, so the user is told rather than the
/// file silently left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Unreadable {
    /// Photoshop documents (`.psd`, `.psb`).
    Photoshop,
    /// Camera raw stills and raw video (`.dng`, `.cr3`, `.braw`, `.r3d`, ...).
    CameraRaw,
    /// Other design or document formats people drop by mistake (`.ai`, `.pdf`).
    Document,
}

/// What a path is by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Media(MediaClass),
    Unreadable(Unreadable),
    /// Files cameras and operating systems leave next to media (sidecars, thumbnails,
    /// indexes, `.DS_Store`): never imported and never reported.
    Junk,
    /// Anything else (text files, archives): left out without a warning.
    Other,
}

const VIDEO: &[&str] = &[
    "mov", "mp4", "m4v", "mts", "m2ts", "mxf", "mkv", "webm", "avi", "mpg", "mpeg", "ts", "3gp",
    "wmv", "flv", "vob", "insv",
];
const AUDIO: &[&str] = &[
    "wav", "wave", "bwf", "aif", "aiff", "aifc", "mp3", "m4a", "aac", "flac", "ogg", "oga", "opus",
    "caf", "wma",
];
const STILL: &[&str] = &[
    "png", "jpg", "jpeg", "tif", "tiff", "bmp", "gif", "webp", "heic", "heif", "avif",
];
const PHOTOSHOP: &[&str] = &["psd", "psb"];
const RAW: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef",
    "srw", "x3f", "iiq", "3fr", "braw", "r3d", "ari", "crm",
];
const DOCUMENT: &[&str] = &["ai", "pdf", "sketch", "indd", "xd", "afdesign", "afphoto"];
/// Sidecars, proxies made by the camera, thumbnails and index files.
const JUNK: &[&str] = &[
    "xml", "srt", "xmp", "thm", "lrv", "lrf", "bim", "cpi", "mpl", "bdm", "smi", "ppn", "cpc",
    "kmp", "ind", "bin", "dat", "db", "ini", "log", "lnk", "cif", "rsv", "sav", "txt",
];
const JUNK_NAMES: &[&str] = &[".ds_store", "thumbs.db", "desktop.ini", "icon\r"];

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Extensions of every file Glass imports, for the file picker's filter.
pub fn importable_extensions() -> Vec<&'static str> {
    VIDEO.iter().chain(AUDIO).chain(STILL).copied().collect()
}

/// Hidden files, AppleDouble `._` files and the operating system's folder litter.
pub fn is_hidden(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
        n.starts_with('.') || JUNK_NAMES.contains(&n.to_ascii_lowercase().as_str())
    })
}

/// Classifies a file by its name.
pub fn classify(path: &Path) -> FileKind {
    if is_hidden(path) {
        return FileKind::Junk;
    }
    let Some(ext) = extension(path) else {
        return FileKind::Other;
    };
    let ext = ext.as_str();
    if VIDEO.contains(&ext) {
        FileKind::Media(MediaClass::Video)
    } else if AUDIO.contains(&ext) {
        FileKind::Media(MediaClass::Audio)
    } else if STILL.contains(&ext) {
        FileKind::Media(MediaClass::Still)
    } else if PHOTOSHOP.contains(&ext) {
        FileKind::Unreadable(Unreadable::Photoshop)
    } else if RAW.contains(&ext) {
        FileKind::Unreadable(Unreadable::CameraRaw)
    } else if DOCUMENT.contains(&ext) {
        FileKind::Unreadable(Unreadable::Document)
    } else if JUNK.contains(&ext) {
        FileKind::Junk
    } else {
        FileKind::Other
    }
}

/// Counts for a drop in progress, by name only (no disk access on the UI thread).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DropSummary {
    pub video: usize,
    pub audio: usize,
    pub still: usize,
    /// Paths without an extension, taken to be folders or camera cards.
    pub folders: usize,
    pub unreadable: Vec<(Unreadable, usize)>,
    /// Files that will be left out without a warning.
    pub other: usize,
}

impl DropSummary {
    pub fn of(paths: &[impl AsRef<Path>]) -> Self {
        let mut summary = Self::default();
        for path in paths {
            let path = path.as_ref();
            match classify(path) {
                FileKind::Media(MediaClass::Video) => summary.video += 1,
                FileKind::Media(MediaClass::Audio) => summary.audio += 1,
                FileKind::Media(MediaClass::Still | MediaClass::Sequence) => summary.still += 1,
                FileKind::Unreadable(kind) => {
                    match summary.unreadable.iter_mut().find(|(k, _)| *k == kind) {
                        Some((_, n)) => *n += 1,
                        None => summary.unreadable.push((kind, 1)),
                    }
                }
                FileKind::Other if path.extension().is_none() => summary.folders += 1,
                FileKind::Junk | FileKind::Other => summary.other += 1,
            }
        }
        summary.unreadable.sort();
        summary
    }

    /// Files and folders that will be imported.
    pub fn importable(&self) -> usize {
        self.video + self.audio + self.still + self.folders
    }

    pub fn unreadable_total(&self) -> usize {
        self.unreadable.iter().map(|(_, n)| n).sum()
    }

    /// Everything dropped that is not junk.
    pub fn total(&self) -> usize {
        self.importable() + self.unreadable_total()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_classify_without_touching_the_disk() {
        let kind = |p: &str| classify(Path::new(p));
        assert_eq!(kind("/a/C0001.MP4"), FileKind::Media(MediaClass::Video));
        assert_eq!(kind("/a/00001.MTS"), FileKind::Media(MediaClass::Video));
        assert_eq!(kind("/a/take.WAV"), FileKind::Media(MediaClass::Audio));
        assert_eq!(kind("/a/IMG_0001.HEIC"), FileKind::Media(MediaClass::Still));
        assert_eq!(
            kind("/a/poster.psd"),
            FileKind::Unreadable(Unreadable::Photoshop)
        );
        assert_eq!(
            kind("/a/IMG_0001.CR3"),
            FileKind::Unreadable(Unreadable::CameraRaw)
        );
        assert_eq!(kind("/a/C0001M01.XML"), FileKind::Junk);
        assert_eq!(kind("/a/DJI_0001.SRT"), FileKind::Junk);
        assert_eq!(kind("/a/._C0001.MP4"), FileKind::Junk);
        assert_eq!(kind("/a/.DS_Store"), FileKind::Junk);
        assert_eq!(kind("/a/notes.md"), FileKind::Other);
    }

    #[test]
    fn drop_summary_counts_by_kind() {
        let summary = DropSummary::of(&[
            "a.mov",
            "b.mp4",
            "c.wav",
            "d.png",
            "e.psd",
            "f.psd",
            "Card B",
            ".DS_Store",
        ]);
        assert_eq!(summary.video, 2);
        assert_eq!(summary.audio, 1);
        assert_eq!(summary.still, 1);
        assert_eq!(summary.folders, 1);
        assert_eq!(summary.unreadable, vec![(Unreadable::Photoshop, 2)]);
        assert_eq!(summary.importable(), 5);
        assert_eq!(summary.total(), 7);
    }
}
