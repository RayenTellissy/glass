//! Embeds the design fonts from `assets/fonts/` when a maintainer has added them (see
//! `assets/fonts/README.md`). Missing files embed nothing, and the loader falls back to
//! a system font at run time.

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

    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("out dir")).join("embedded_fonts.rs");
    std::fs::write(out, code).expect("write embedded_fonts.rs");
}
