//! UI snapshot cases: every panel, dialog and parameter widget draws itself here with
//! realistic sample content, and `crates/ui/tests/ui_snapshots.rs` renders each case
//! headless in every theme at 1x and 2x and compares it with a golden PNG under
//! `crates/ui/tests/snapshots/<name>/<theme>@<scale>x.png` (see CONTRIBUTING.md,
//! "UI snapshots").
//!
//! Adding a panel means adding a case to [`cases`]: a test fails when a module under
//! `src/panels/`, `src/dialogs/` or `src/params/` has none ([`uncovered_modules`]).

mod cases;
mod clip_cases;
pub mod compare;
pub mod fonts;
mod pool_cases;
mod relink_cases;
mod timeline_cases;
mod track_header_cases;
mod workspace_cases;

use std::path::Path;

use egui::Vec2;

pub use crate::theme::ThemeKind;

/// The folders whose modules must each have at least one case.
pub const COVERED_DIRS: [&str; 3] = ["panels", "dialogs", "params"];

/// The scales every case is rendered at.
pub const SCALES: [u32; 2] = [1, 2];

/// One piece of UI to snapshot.
#[derive(Clone, Copy)]
pub struct SnapshotCase {
    /// The golden folder name: lowercase ASCII letters, digits and `_`, unique.
    pub name: &'static str,
    /// The module the case covers, as a path under `crates/ui/src` without `.rs`
    /// (`"panels/media_pool"`, `"theme"`).
    pub module: &'static str,
    /// The rendered size in points; the golden is this size times the scale, so keep it
    /// cropped to the piece.
    pub size: Vec2,
    /// Draws the piece into the root `Ui` of a frame, with the theme already applied.
    pub draw: fn(&mut egui::Ui),
}

impl std::fmt::Debug for SnapshotCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotCase")
            .field("name", &self.name)
            .field("module", &self.module)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// Every registered case. New panels, dialogs and parameter widgets add theirs here.
pub fn cases() -> Vec<SnapshotCase> {
    cases::all()
}

/// The golden file name for a theme and scale, `dark@2x.png`.
pub fn golden_file_name(theme: ThemeKind, scale: u32) -> String {
    format!("{}@{scale}x.png", theme.name())
}

/// Modules under [`COVERED_DIRS`] in the source tree at `src_root` (`crates/ui/src`)
/// that no case in `cases` covers. A module is a `.rs` file other than `mod.rs`, or a
/// folder (its submodules are covered by the folder's cases).
pub fn uncovered_modules(src_root: &Path, cases: &[SnapshotCase]) -> Vec<String> {
    let mut missing = Vec::new();
    for dir in COVERED_DIRS {
        for module in modules_in(&src_root.join(dir)) {
            let path = format!("{dir}/{module}");
            let covered = cases
                .iter()
                .any(|case| case.module == path || case.module.starts_with(&format!("{path}/")));
            if !covered {
                missing.push(path);
            }
        }
    }
    missing
}

fn modules_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut modules: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.to_owned();
            if path.is_dir() {
                Some(name)
            } else {
                let stem = name.strip_suffix(".rs")?;
                (stem != "mod").then(|| stem.to_owned())
            }
        })
        .collect();
    modules.sort();
    modules
}

/// Problems with the registry itself: bad or duplicate names, modules that do not exist
/// under `src_root`, empty sizes.
pub fn registry_problems(src_root: &Path, cases: &[SnapshotCase]) -> Vec<String> {
    let mut problems = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let valid_name = !case.name.is_empty()
            && case
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !valid_name {
            problems.push(format!(
                "case {:?}: names use lowercase letters, digits and _",
                case.name
            ));
        }
        if cases[..i].iter().any(|other| other.name == case.name) {
            problems.push(format!("case {:?} is registered twice", case.name));
        }
        let module = src_root.join(case.module);
        if !module.with_extension("rs").is_file() && !module.is_dir() {
            problems.push(format!(
                "case {:?}: module {:?} not found under crates/ui/src",
                case.name, case.module
            ));
        }
        if !(case.size.x >= 1.0 && case.size.y >= 1.0) {
            problems.push(format!("case {:?}: empty size", case.name));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &'static str, module: &'static str) -> SnapshotCase {
        SnapshotCase {
            name,
            module,
            size: egui::vec2(10.0, 10.0),
            draw: |_| {},
        }
    }

    #[test]
    fn golden_names() {
        assert_eq!(golden_file_name(ThemeKind::Dark, 1), "dark@1x.png");
        assert_eq!(
            golden_file_name(ThemeKind::HighContrast, 2),
            "high-contrast@2x.png"
        );
    }

    #[test]
    fn modules_without_cases_are_found() {
        let root = std::env::temp_dir().join(format!("glass-snapshot-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("panels/timeline")).expect("dirs");
        std::fs::create_dir_all(root.join("dialogs")).expect("dirs");
        for file in [
            "panels/mod.rs",
            "panels/media_pool.rs",
            "panels/inspector.rs",
            "panels/timeline/mod.rs",
            "panels/timeline/ruler.rs",
            "dialogs/recovery.rs",
            "dialogs/notes.txt",
        ] {
            std::fs::write(root.join(file), "").expect("write");
        }
        let cases = [
            case("media_pool", "panels/media_pool"),
            case("timeline_ruler", "panels/timeline/ruler"),
        ];
        let missing = uncovered_modules(&root, &cases);
        let problems = registry_problems(&root, &[case("Bad-Name", "panels/gone")]);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(missing, ["panels/inspector", "dialogs/recovery"]);
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn the_registry_is_well_formed() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let cases = cases();
        assert!(!cases.is_empty());
        let problems = registry_problems(&src, &cases);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
