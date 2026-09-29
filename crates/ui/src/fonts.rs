//! Font families for egui, from [`assets::fonts`]: Geist and Geist Mono when bundled,
//! otherwise a system UI font (see `assets/fonts/README.md`).
//!
//! Families: `Proportional` (400), `Monospace` (mono 400), and the named families
//! [`MEDIUM`] (500) and [`MONO_MEDIUM`] (mono 500). Each falls back to the others so
//! every family renders when only one font file was found.

use std::sync::OnceLock;

use assets::fonts::{Face, Fonts};
use egui::{FontData, FontDefinitions, FontFamily};

/// The named family for Geist 500 (titles, display, emphasis).
pub const MEDIUM: &str = "medium";
/// The named family for Geist Mono 500.
pub const MONO_MEDIUM: &str = "mono-medium";

pub fn medium() -> FontFamily {
    FontFamily::Name(MEDIUM.into())
}

pub fn mono_medium() -> FontFamily {
    FontFamily::Name(MONO_MEDIUM.into())
}

/// The faces, loaded once per process: reading a system font is file I/O, so it happens
/// at startup, never per frame.
pub fn loaded() -> &'static Fonts {
    static LOADED: OnceLock<Fonts> = OnceLock::new();
    LOADED.get_or_init(assets::fonts::load)
}

/// egui font definitions for `fonts`, or `None` when no face was found.
pub fn definitions(fonts: &'static Fonts) -> Option<FontDefinitions> {
    if fonts.is_empty() {
        return None;
    }
    let mut defs = FontDefinitions::empty();
    let mut key_of = |face: Face| -> Option<String> {
        let file = fonts.get(face)?;
        // Faces that share a file (a system fallback, or Medium standing in for
        // Regular) share one entry, so the bytes are parsed once.
        let key = file.origin.to_string();
        defs.font_data.entry(key.clone()).or_insert_with(|| {
            let data = match &file.bytes {
                std::borrow::Cow::Borrowed(bytes) => FontData::from_static(bytes),
                std::borrow::Cow::Owned(bytes) => FontData::from_static(bytes.as_slice()),
            };
            data.into()
        });
        Some(key)
    };
    let regular = key_of(Face::Regular);
    let medium_key = key_of(Face::Medium);
    let mono = key_of(Face::MonoRegular);
    let mono_medium_key = key_of(Face::MonoMedium);

    let chain = |first: &[&Option<String>]| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for key in first.iter().copied().flatten() {
            if !out.contains(key) {
                out.push(key.clone());
            }
        }
        out
    };
    defs.families
        .insert(FontFamily::Proportional, chain(&[&regular, &mono]));
    defs.families
        .insert(FontFamily::Monospace, chain(&[&mono, &regular]));
    defs.families
        .insert(medium(), chain(&[&medium_key, &regular, &mono]));
    defs.families
        .insert(mono_medium(), chain(&[&mono_medium_key, &mono, &regular]));
    Some(defs)
}

/// Installs the fonts into `ctx` once; later calls do nothing. Returns whether any font
/// was found (without one egui cannot lay out text).
pub fn install(ctx: &egui::Context) -> bool {
    let id = egui::Id::new("glass.fonts.installed");
    if let Some(installed) = ctx.data(|d| d.get_temp::<bool>(id)) {
        return installed;
    }
    let installed = match definitions(loaded()) {
        Some(defs) => {
            ctx.set_fonts(defs);
            true
        }
        None => false,
    };
    ctx.data_mut(|d| d.insert_temp(id, installed));
    installed
}
