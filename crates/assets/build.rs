//! Embeds the design fonts from `assets/fonts/` when a maintainer has added them (see
//! `assets/fonts/README.md`). Missing files embed nothing, and the loader falls back to
//! a system font at run time.
//!
//! Also embeds every bundled transition folder under `assets/transitions/` (its
//! `transition.json` and `shader.wgsl`, either possibly missing), so release builds carry
//! them without the folder.

use std::fmt::Write as _;
use std::path::PathBuf;

const FONTS: [&str; 4] = [
    "Geist-Regular.ttf",
    "Geist-Medium.ttf",
    "GeistMono-Regular.ttf",
    "GeistMono-Medium.ttf",
];

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dir = manifest.join("../../assets/fonts");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut code = String::from("pub(crate) const EMBEDDED: [Option<&[u8]>; 4] = [\n");
    for name in FONTS {
        let path = dir.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        match path.canonicalize() {
            Ok(path) if path.is_file() => {
                let _ = writeln!(
                    code,
                    "    Some(include_bytes!({:?})),",
                    path.display().to_string()
                );
            }
            _ => code.push_str("    None,\n"),
        }
    }
    code.push_str("];\n");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("out dir"));
    std::fs::write(out_dir.join("embedded_fonts.rs"), code).expect("write embedded_fonts.rs");
    std::fs::write(
        out_dir.join("embedded_transitions.rs"),
        transitions(&manifest.join("../../assets/transitions")),
    )
    .expect("write embedded_transitions.rs");
}

/// `EMBEDDED`: one entry per folder under `dir`, sorted by folder name.
fn transitions(dir: &std::path::Path) -> String {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut folders: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    folders.sort();
    let file = |folder: &PathBuf, name: &str| {
        let path = folder.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        match path.canonicalize() {
            Ok(path) if path.is_file() => {
                format!("Some(include_str!({:?}))", path.display().to_string())
            }
            _ => "None".to_owned(),
        }
    };
    let mut code = String::from("pub const EMBEDDED: &[Embedded] = &[\n");
    for folder in &folders {
        let name = folder
            .file_name()
            .and_then(|n| n.to_str())
            .expect("transition folder names are UTF-8");
        let _ = writeln!(
            code,
            "    Embedded {{ folder: {name:?}, manifest: {}, shader: {} }},",
            file(folder, "transition.json"),
            file(folder, "shader.wgsl"),
        );
    }
    code.push_str("];\n");
    code
}
