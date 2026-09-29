//! Source rules for UI code (`crates/ui/src` and `crates/app/src`), run in CI's check
//! job and by `cargo test -p app --test ui_source -- --nocapture`:
//!
//! - every `t!("key")` exists in `assets/i18n/en.json` and passes the string's named
//!   placeholders; keys nothing uses are printed as warnings;
//! - no literal user-facing text goes to an egui text API (`label`, `button`,
//!   `RichText::new`, `on_hover_text`, ...) unless wrapped in `t!()` or marked with
//!   `// allow-literal-text: <reason>` for debug-only strings;
//! - no literal colour (`Color32::from_rgb`, `Color32::RED`, `"#F2F2F2"`, ...) appears
//!   outside the theme module (`crates/ui/src/theme.rs`) unless marked with
//!   `// allow-literal-colour: <reason>`.

#[path = "support/source_scan.rs"]
mod source_scan;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use source_scan::{
    ALLOW_COLOUR, ALLOW_TEXT, Violation, check_colours, check_keys, check_text, lex, unused_keys,
};

const SCANNED: &[&str] = &["ui/src", "app/src"];

/// The one module that turns token values into colours.
const THEME_MODULE: &str = "crates/ui/src/theme.rs";

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn scanned_files() -> Vec<(String, String)> {
    let root = crates_dir();
    let mut files = Vec::new();
    for dir in SCANNED {
        rust_files(&root.join(dir), &mut files);
    }
    files.sort();
    assert!(!files.is_empty(), "no UI sources found under {root:?}");
    files
        .into_iter()
        .map(|path| {
            let src = std::fs::read_to_string(&path).expect("read source");
            let name = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .display()
                .to_string();
            (format!("crates/{name}"), src)
        })
        .collect()
}

fn english() -> BTreeMap<String, String> {
    let path = crates_dir().join("../assets/i18n/en.json");
    let json = std::fs::read_to_string(&path).expect("read en.json");
    let value: serde_json::Value = serde_json::from_str(&json).expect("en.json is JSON");
    value["strings"]
        .as_object()
        .expect("en.json has a strings object")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().expect("string value").to_owned()))
        .collect()
}

fn report(problems: &[(String, Violation)]) -> String {
    problems
        .iter()
        .map(|(file, v)| format!("  {file}:{}: {}", v.line, v.message))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ui_text_uses_existing_i18n_keys_and_no_literals() {
    let strings = english();
    let mut problems = Vec::new();
    let mut all_uses = Vec::new();
    for (file, src) in scanned_files() {
        let (uses, violations) = check_text(&src);
        problems.extend(violations.into_iter().map(|v| (file.clone(), v)));
        problems.extend(
            check_keys(&uses, &strings)
                .into_iter()
                .map(|v| (file.clone(), v)),
        );
        all_uses.extend(uses);
    }
    for key in unused_keys(&strings, &all_uses) {
        eprintln!("warning: i18n key {key:?} in assets/i18n/en.json is not used by UI code yet");
    }
    assert!(
        problems.is_empty(),
        "UI text rules failed:\n{}",
        report(&problems)
    );
}

#[test]
fn ui_code_has_no_literal_colours_outside_the_theme() {
    let mut problems = Vec::new();
    let mut theme_seen = false;
    for (file, src) in scanned_files() {
        if file == THEME_MODULE {
            theme_seen = true;
            continue;
        }
        problems.extend(check_colours(&src).into_iter().map(|v| (file.clone(), v)));
    }
    assert!(theme_seen, "{THEME_MODULE} moved; update THEME_MODULE");
    assert!(
        problems.is_empty(),
        "UI colour rules failed:\n{}",
        report(&problems)
    );
}

// Fixture snippets proving the checks catch what they should.

fn text_violations(src: &str) -> Vec<usize> {
    check_text(src).1.into_iter().map(|v| v.line).collect()
}

#[test]
fn literal_text_in_egui_calls_is_caught() {
    let src = r#"
fn panel(ui: &mut egui::Ui) {
    ui.label("Render queue");
    ui.heading(t!("export.title"));
    let _ = ui.button(egui::RichText::new("Export"));
    ui.add(egui::Button::new("Cancel")).on_hover_text("Stops the export");
    ui.label(format!("{count} clips selected"));
    ui.label(format!("{} · {}", a, b));
    ui.label(format!("{}{}", t!("a.b"), "x"));
}
"#;
    assert_eq!(text_violations(src), vec![3, 5, 6, 6, 7, 9]);
}

#[test]
fn allow_comment_needs_a_reason_and_covers_its_line_and_the_next() {
    let src = format!(
        r#"
fn debug(ui: &mut egui::Ui) {{
    ui.label("fps"); // {ALLOW_TEXT} debug overlay, never shown to users
    // {ALLOW_TEXT} debug overlay
    ui.label("gpu");
    ui.label("no reason"); // {ALLOW_TEXT}
    ui.label("far away");
}}
"#
    );
    assert_eq!(text_violations(&src), vec![6, 7]);
}

#[test]
fn closures_tests_and_non_text_calls_are_not_flagged() {
    let src = r##"
fn tabs(ui: &mut egui::Ui) {
    ui.collapsing(t!("inspector.transform"), |ui| {
        tracing::info!("opened the transform section");
        ui.label("Scale");
    });
    egui::Panel::top("top-bar").show(ui, |_| {});
    let id = egui::Id::new("timeline");
    let c = '"';
    let s = r#"ui.label("inside a raw string")"#;
}

#[cfg(test)]
mod tests {
    fn fixture(ui: &mut egui::Ui) {
        ui.label("fixture text");
    }
}
"##;
    assert_eq!(text_violations(src), vec![5]);
}

#[test]
fn keys_are_extracted_and_checked_against_english() {
    let src = r#"
fn f(ui: &mut egui::Ui, n: usize, total: usize) {
    ui.label(t!("export.progress", done = n, total = total));
    ui.label(t!("export.title"));
    ui.label(t!("export.missing"));
    ui.label(t!("export.progress", done = n));
    ui.label(t!(KEY));
}
"#;
    let (uses, violations) = check_text(src);
    assert_eq!(
        violations.iter().map(|v| v.line).collect::<Vec<_>>(),
        vec![7],
        "a non-literal key cannot be checked"
    );
    assert_eq!(uses.len(), 4);
    assert_eq!(
        uses[0].args.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["done", "total"]
    );

    let strings: BTreeMap<String, String> = [
        ("export.progress", "{done} of {total}"),
        ("export.title", "Export"),
        ("export.unused", "Never shown"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let lines: Vec<usize> = check_keys(&uses, &strings)
        .into_iter()
        .map(|v| v.line)
        .collect();
    assert_eq!(lines, vec![5, 6], "missing key, then missing placeholder");
    assert_eq!(unused_keys(&strings, &uses), vec!["export.unused"]);
}

#[test]
fn lexer_handles_strings_chars_lifetimes_and_comments() {
    let lexed = lex(
        "fn f<'a>(x: &'a str) -> char { /* \"no\" */ let _ = b\"bytes\"; let _ = br##\"r\"#\"##; '\\'' }",
    );
    let strings: Vec<_> = lexed
        .tokens
        .iter()
        .filter_map(|t| match &t.tok {
            source_scan::Tok::Str(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(strings, vec!["bytes", "r\"#"]);
    assert_eq!(lexed.comments.len(), 1);
}

#[test]
fn literal_colours_are_caught() {
    let src = format!(
        r##"
fn paint(p: &egui::Painter, theme: &Theme) {{
    p.rect_filled(r, 4.0, egui::Color32::from_rgb(76, 127, 234));
    p.rect_filled(r, 4.0, Color32::RED);
    p.rect_filled(r, 4.0, Color32::from_hex("#4C7FEA").unwrap_or_default());
    let accent = "#F2F2F2";
    let soft = egui::Rgba::from_rgba_unmultiplied(1.0, 1.0, 1.0, 0.1);
    let c = ecolor::hex_color!("#123456");
    p.rect_filled(r, 4.0, theme.color(Role::ClipVideo));
    p.rect_filled(r, 4.0, Color32::TRANSPARENT);
    let issue = "#123"; // hex-looking but three digits is still a colour
    let debug = Color32::from_gray(40); // {ALLOW_COLOUR} frame-time overlay, developers only
    let id = "#tag-name";
}}

#[cfg(test)]
mod tests {{
    const RED: Color32 = Color32::from_rgb(255, 0, 0);
}}
"##
    );
    let lines: Vec<usize> = check_colours(&src).into_iter().map(|v| v.line).collect();
    // Line 5 holds both the constructor and the hex string; line 8 the macro and its
    // string.
    assert_eq!(lines, vec![3, 4, 5, 5, 6, 7, 8, 8, 11]);
}
