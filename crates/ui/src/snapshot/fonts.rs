//! The fonts snapshots render with. The runtime fallback picks the best system UI font,
//! which differs between machines and macOS releases, so goldens pin fonts instead:
//! the bundled Geist faces once they are in `assets/fonts/`, until then Arial and
//! Courier New from `/System/Library/Fonts/Supplemental/`, which every macOS ships and
//! which have not changed across recent releases. [`PinnedFonts::fingerprint`] is
//! stored next to the goldens so a font change is named when snapshots stop matching.

use std::path::Path;

use assets::fonts::{Face, Origin};
use egui::{FontData, FontDefinitions, FontFamily};

const SYSTEM_SANS: &str = "/System/Library/Fonts/Supplemental/Arial.ttf";
const SYSTEM_MONO: &str = "/System/Library/Fonts/Supplemental/Courier New.ttf";

/// Font definitions for snapshots and a short description of the files behind them.
pub struct PinnedFonts {
    pub definitions: FontDefinitions,
    pub fingerprint: String,
}

/// Why the pinned fonts are unavailable; snapshots skip rather than render with
/// whatever font the machine has.
#[derive(Debug)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The bundled Geist faces when all four are bundled, else the pinned macOS files.
pub fn pinned() -> Result<PinnedFonts, Unavailable> {
    let loaded = crate::fonts::loaded();
    if loaded.all_bundled() {
        let mut files = Vec::new();
        for face in Face::ALL {
            if let Some(file) = loaded.get(face) {
                files.push((face, file.bytes.to_vec(), file.origin.clone()));
            }
        }
        let fingerprint = files
            .iter()
            .map(|(_, bytes, origin)| describe(origin, bytes))
            .collect::<Vec<_>>()
            .join("\n");
        let data = |face: Face| {
            files
                .iter()
                .find(|(f, _, _)| *f == face)
                .map(|(_, bytes, _)| bytes.clone())
                .unwrap_or_default()
        };
        return Ok(PinnedFonts {
            definitions: definitions(
                data(Face::Regular),
                data(Face::Medium),
                data(Face::MonoRegular),
                data(Face::MonoMedium),
            ),
            fingerprint,
        });
    }
    if !cfg!(target_os = "macos") {
        return Err(Unavailable(
            "snapshot goldens are macOS-only until the Geist fonts are bundled".into(),
        ));
    }
    let read = |path: &str| {
        std::fs::read(path).map_err(|e| Unavailable(format!("cannot read {path}: {e}")))
    };
    let sans = read(SYSTEM_SANS)?;
    let mono = read(SYSTEM_MONO)?;
    let fingerprint = [
        describe(&Origin::System(SYSTEM_SANS.into()), &sans),
        describe(&Origin::System(SYSTEM_MONO.into()), &mono),
    ]
    .join("\n");
    Ok(PinnedFonts {
        definitions: definitions(sans.clone(), sans, mono.clone(), mono),
        fingerprint,
    })
}

fn describe(origin: &Origin, bytes: &[u8]) -> String {
    let name = match origin {
        Origin::Bundled(name) => (*name).to_owned(),
        Origin::System(path) => Path::new(path).file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        ),
    };
    format!("{name} {} bytes fnv1a {:016x}", bytes.len(), fnv1a(bytes))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The same four families [`crate::fonts::definitions`] builds, from given bytes.
fn definitions(
    regular: Vec<u8>,
    medium: Vec<u8>,
    mono: Vec<u8>,
    mono_medium: Vec<u8>,
) -> FontDefinitions {
    let mut defs = FontDefinitions::empty();
    let mut keys = Vec::new();
    for (key, bytes) in [
        ("regular", regular),
        ("medium", medium),
        ("mono", mono),
        ("mono-medium", mono_medium),
    ] {
        defs.font_data
            .insert(key.to_owned(), FontData::from_owned(bytes).into());
        keys.push(key.to_owned());
    }
    let chain = |order: [usize; 4]| order.iter().map(|&i| keys[i].clone()).collect::<Vec<_>>();
    defs.families
        .insert(FontFamily::Proportional, chain([0, 2, 1, 3]));
    defs.families
        .insert(FontFamily::Monospace, chain([2, 0, 3, 1]));
    defs.families
        .insert(crate::fonts::medium(), chain([1, 0, 2, 3]));
    defs.families
        .insert(crate::fonts::mono_medium(), chain([3, 2, 0, 1]));
    defs
}

/// Installs `fonts` into `ctx`, replacing whatever [`crate::fonts::install`] chose.
pub fn install(ctx: &egui::Context, fonts: &PinnedFonts) {
    crate::fonts::install_definitions(ctx, fonts.definitions.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_matches_the_reference_vectors() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
