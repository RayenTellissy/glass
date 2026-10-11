//! The bundled transitions in `assets/transitions/`: one folder per transition holding
//! `transition.json` (its manifest) and `shader.wgsl`. `render::transition` parses and
//! validates them; this module only carries the files, embedded at build time, and the
//! folder's place in the source tree (what debug builds read and watch instead).

use std::path::{Path, PathBuf};

/// One folder under `assets/transitions/` as it was at build time. A file that was
/// missing is `None`, so the registry can report it rather than the build failing.
#[derive(Clone, Copy, Debug)]
pub struct Embedded {
    pub folder: &'static str,
    pub manifest: Option<&'static str>,
    pub shader: Option<&'static str>,
}

include!(concat!(env!("OUT_DIR"), "/embedded_transitions.rs"));

/// The manifest file name inside a transition folder.
pub const MANIFEST: &str = "transition.json";

/// The shader file name inside a transition folder.
pub const SHADER: &str = "shader.wgsl";

/// `assets/transitions/` in the source tree.
pub fn source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/transitions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_folder_is_embedded_with_both_files() {
        let mut on_disk: Vec<String> = std::fs::read_dir(source_dir())
            .expect("assets/transitions exists")
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        on_disk.sort();
        let embedded: Vec<&str> = EMBEDDED.iter().map(|e| e.folder).collect();
        assert_eq!(embedded, on_disk);
        for e in EMBEDDED {
            assert!(e.manifest.is_some(), "{}/{MANIFEST} is missing", e.folder);
            assert!(e.shader.is_some(), "{}/{SHADER} is missing", e.folder);
        }
    }
}
