//! Font files for the UI: Geist and Geist Mono embedded from `assets/fonts/` when they
//! are there at build time, otherwise a system UI font read at run time (never bundled).
//! See `assets/fonts/README.md`.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/embedded_fonts.rs"));

/// The four faces the design uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// Geist 400.
    Regular,
    /// Geist 500.
    Medium,
    /// Geist Mono 400.
    MonoRegular,
    /// Geist Mono 500.
    MonoMedium,
}

impl Face {
    pub const ALL: [Face; 4] = [
        Face::Regular,
        Face::Medium,
        Face::MonoRegular,
        Face::MonoMedium,
    ];

    /// The file name in `assets/fonts/`.
    pub fn file_name(self) -> &'static str {
        match self {
            Face::Regular => "Geist-Regular.ttf",
            Face::Medium => "Geist-Medium.ttf",
            Face::MonoRegular => "GeistMono-Regular.ttf",
            Face::MonoMedium => "GeistMono-Medium.ttf",
        }
    }

    pub fn is_mono(self) -> bool {
        matches!(self, Face::MonoRegular | Face::MonoMedium)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Where a face's bytes came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Embedded from `assets/fonts/` at build time.
    Bundled(&'static str),
    /// Read from the system at run time.
    System(PathBuf),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Bundled(name) => write!(f, "bundled {name}"),
            Origin::System(path) => write!(f, "system {}", path.display()),
        }
    }
}

/// One face's font file.
#[derive(Clone, Debug)]
pub struct FontFile {
    pub bytes: Cow<'static, [u8]>,
    pub origin: Origin,
}

/// The loaded faces. A face is `None` only when neither a bundled nor a system font
/// could be found, in which case the UI cannot draw text.
#[derive(Clone, Debug, Default)]
pub struct Fonts {
    faces: [Option<FontFile>; 4],
}

impl Fonts {
    pub fn get(&self, face: Face) -> Option<&FontFile> {
        self.faces[face.index()].as_ref()
    }

    /// True when every face comes from the bundled design fonts.
    pub fn all_bundled(&self) -> bool {
        self.faces.iter().all(|f| {
            matches!(
                f,
                Some(FontFile {
                    origin: Origin::Bundled(_),
                    ..
                })
            )
        })
    }

    pub fn is_empty(&self) -> bool {
        self.faces.iter().all(Option::is_none)
    }

    /// One line naming each face's source, for logs and test output.
    pub fn summary(&self) -> String {
        Face::ALL
            .iter()
            .map(|&face| match self.get(face) {
                Some(file) => format!("{face:?}: {}", file.origin),
                None => format!("{face:?}: none"),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Loads the four faces (see the module docs for the order) and logs where they came from.
pub fn load() -> Fonts {
    let fonts = load_from(&EMBEDDED, system_candidates());
    if fonts.all_bundled() {
        tracing::info!("fonts: bundled Geist and Geist Mono");
    } else if fonts.is_empty() {
        tracing::warn!("fonts: no bundled or system font found; the UI cannot draw text");
    } else {
        tracing::info!("fonts: system fallback ({})", fonts.summary());
    }
    fonts
}

fn load_from(embedded: &[Option<&'static [u8]>; 4], system: SystemCandidates) -> Fonts {
    let bundled = |face: Face| {
        embedded[face.index()].map(|bytes| FontFile {
            bytes: Cow::Borrowed(bytes),
            origin: Origin::Bundled(face.file_name()),
        })
    };
    let system_font = |paths: &[PathBuf]| {
        paths.iter().find_map(|path| {
            std::fs::read(path).ok().map(|bytes| FontFile {
                bytes: Cow::Owned(bytes),
                origin: Origin::System(path.clone()),
            })
        })
    };

    let regular = bundled(Face::Regular).or_else(|| system_font(&system.proportional));
    let medium = bundled(Face::Medium).or_else(|| regular.clone());
    let mono_regular = bundled(Face::MonoRegular)
        .or_else(|| system_font(&system.mono))
        .or_else(|| regular.clone());
    let mono_medium = bundled(Face::MonoMedium).or_else(|| mono_regular.clone());
    Fonts {
        faces: [regular, medium, mono_regular, mono_medium],
    }
}

struct SystemCandidates {
    proportional: Vec<PathBuf>,
    mono: Vec<PathBuf>,
}

/// Standard system UI font locations, best first. Only `.ttf` files: egui reads the
/// first face of a collection, which is not always the regular one.
fn system_candidates() -> SystemCandidates {
    let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();
    if cfg!(target_os = "macos") {
        SystemCandidates {
            proportional: paths(&[
                "/System/Library/Fonts/SFNS.ttf",
                "/System/Library/Fonts/Geneva.ttf",
                "/System/Library/Fonts/Supplemental/Arial.ttf",
            ]),
            mono: paths(&[
                "/System/Library/Fonts/SFNSMono.ttf",
                "/System/Library/Fonts/Monaco.ttf",
                "/System/Library/Fonts/Supplemental/Courier New.ttf",
            ]),
        }
    } else if cfg!(windows) {
        let root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
        let fonts = root.join("Fonts");
        let at = |names: &[&str]| names.iter().map(|n| fonts.join(n)).collect::<Vec<_>>();
        SystemCandidates {
            proportional: at(&["segoeui.ttf", "arial.ttf"]),
            mono: at(&["consola.ttf", "cour.ttf"]),
        }
    } else {
        let dirs = ["/usr/share/fonts", "/usr/local/share/fonts"];
        let find = |names: &[&str]| {
            names
                .iter()
                .flat_map(|name| {
                    dirs.iter()
                        .flat_map(move |dir| find_file(Path::new(dir), name, 4))
                })
                .collect::<Vec<_>>()
        };
        SystemCandidates {
            proportional: find(&[
                "DejaVuSans.ttf",
                "NotoSans-Regular.ttf",
                "LiberationSans-Regular.ttf",
            ]),
            mono: find(&[
                "DejaVuSansMono.ttf",
                "NotoSansMono-Regular.ttf",
                "LiberationMono-Regular.ttf",
            ]),
        }
    }
}

/// Finds `name` under `dir`, at most `depth` folders deep.
fn find_file(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    let mut subdirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    subdirs
        .into_iter()
        .find_map(|sub| find_file(&sub, name, depth - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    static FAKE: &[u8] = b"not a real font";

    fn none() -> SystemCandidates {
        SystemCandidates {
            proportional: vec![],
            mono: vec![],
        }
    }

    #[test]
    fn bundled_faces_win_and_medium_falls_back_to_regular() {
        let fonts = load_from(&[Some(FAKE), None, Some(FAKE), None], none());
        assert_eq!(
            fonts.get(Face::Medium).map(|f| &f.origin),
            Some(&Origin::Bundled("Geist-Regular.ttf"))
        );
        assert_eq!(
            fonts.get(Face::MonoMedium).map(|f| &f.origin),
            Some(&Origin::Bundled("GeistMono-Regular.ttf"))
        );
        assert!(fonts.all_bundled());
    }

    #[test]
    fn nothing_found_means_no_faces() {
        let fonts = load_from(&[None; 4], none());
        assert!(fonts.is_empty());
        assert!(!fonts.all_bundled());
    }

    #[test]
    fn system_fallback_is_read_and_mono_falls_back_to_proportional() {
        let dir = std::env::temp_dir().join(format!("glass-fonts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let sans = dir.join("Sans.ttf");
        std::fs::write(&sans, FAKE).expect("write");
        let fonts = load_from(
            &[None; 4],
            SystemCandidates {
                proportional: vec![dir.join("Missing.ttf"), sans.clone()],
                mono: vec![dir.join("MissingMono.ttf")],
            },
        );
        let _ = std::fs::remove_dir_all(&dir);
        for face in Face::ALL {
            assert_eq!(
                fonts.get(face).map(|f| &f.origin),
                Some(&Origin::System(sans.clone())),
                "{face:?}"
            );
        }
    }
}
