//! User-facing strings. Every string the UI shows is a key in `assets/i18n/<locale>.json`,
//! looked up with [`t!`](crate::t). English is embedded in the binary; other locales load
//! from JSON with [`Locale::from_json`] and become current with [`set_locale`].
//!
//! A lookup never panics. A key missing from the current locale falls back to English;
//! a key missing from English too shows as `⟦key⟧` and logs one warning per key.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{self, Display, Write as _};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use serde::Deserialize;

/// The English strings, embedded at build time.
pub const EN_JSON: &str = include_str!("../../../assets/i18n/en.json");

/// Locales bundled with the binary: `(code, JSON)`.
pub const BUNDLED: &[(&str, &str)] = &[("en", EN_JSON)];

/// Text direction of a locale. Right-to-left layout arrives in Phase 6; the flag is read
/// from each locale file now so the layout code has one place to ask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Ltr,
    Rtl,
}

/// One locale's strings.
#[derive(Clone, Debug)]
pub struct Locale {
    code: String,
    name: String,
    direction: Direction,
    strings: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocaleFile {
    locale: String,
    name: String,
    #[serde(default)]
    direction: Direction,
    strings: BTreeMap<String, String>,
}

/// A locale file that could not be used.
#[derive(Debug)]
pub enum LocaleError {
    Json(serde_json::Error),
    /// A key that is not lowercase dotted segments (`area.thing`, `[a-z0-9_]`).
    BadKey(String),
    /// A string with an unbalanced `{` or `}` or an empty placeholder.
    BadPlaceholder {
        key: String,
    },
}

impl Display for LocaleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "locale file is not valid: {error}"),
            Self::BadKey(key) => write!(
                f,
                "key {key:?} is not lowercase dotted segments such as \"area.thing\""
            ),
            Self::BadPlaceholder { key } => write!(f, "string {key:?} has a malformed placeholder"),
        }
    }
}

impl std::error::Error for LocaleError {}

impl Locale {
    /// Parses a locale file (the format is described in `assets/i18n/README.md`).
    pub fn from_json(json: &str) -> Result<Self, LocaleError> {
        let file: LocaleFile = serde_json::from_str(json).map_err(LocaleError::Json)?;
        for (key, value) in &file.strings {
            if !is_valid_key(key) {
                return Err(LocaleError::BadKey(key.clone()));
            }
            if placeholders(value).is_none() {
                return Err(LocaleError::BadPlaceholder { key: key.clone() });
            }
        }
        Ok(Self {
            code: file.locale,
            name: file.name,
            direction: file.direction,
            strings: file.strings.into_iter().collect(),
        })
    }

    /// English, from the embedded file. If that file were ever broken (a test guards
    /// it), this logs the error and returns an empty locale rather than panicking.
    pub fn english() -> Self {
        Self::from_json(EN_JSON).unwrap_or_else(|error| {
            tracing::error!("embedded en.json: {error}");
            Self {
                code: "en".into(),
                name: "English".into(),
                direction: Direction::Ltr,
                strings: HashMap::new(),
            }
        })
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    /// The locale's own name for itself, for the language picker.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn is_rtl(&self) -> bool {
        self.direction == Direction::Rtl
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.strings.get(key).map(String::as_str)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.strings.keys().map(String::as_str)
    }
}

/// Lowercase dotted segments: `area.thing`, each segment `[a-z0-9_]+`, at least two.
pub fn is_valid_key(key: &str) -> bool {
    let mut segments = 0;
    for segment in key.split('.') {
        if segment.is_empty()
            || !segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

/// The placeholder names in a string (`{name}`), in order, or `None` when a brace is
/// unbalanced or a placeholder is empty. `{{` and `}}` are literal braces.
pub fn placeholders(text: &str) -> Option<Vec<&str>> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(['{', '}']) {
        let (brace, after) = (rest.as_bytes()[i], &rest[i + 1..]);
        if after.as_bytes().first() == Some(&brace) {
            rest = &after[1..];
            continue;
        }
        if brace == b'}' {
            return None;
        }
        let end = after.find(['{', '}'])?;
        if after.as_bytes()[end] != b'}' || end == 0 {
            return None;
        }
        names.push(&after[..end]);
        rest = &after[end + 1..];
    }
    Some(names)
}

static ENGLISH: LazyLock<Arc<Locale>> = LazyLock::new(|| Arc::new(Locale::english()));
static CURRENT: LazyLock<RwLock<Arc<Locale>>> = LazyLock::new(|| RwLock::new(ENGLISH.clone()));
static WARNED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// Makes `locale` current for every later [`t!`](crate::t) call.
pub fn set_locale(locale: Locale) {
    let locale = Arc::new(locale);
    match CURRENT.write() {
        Ok(mut current) => *current = locale,
        Err(poisoned) => *poisoned.into_inner() = locale,
    }
}

/// The current locale (English until [`set_locale`] is called).
pub fn current() -> Arc<Locale> {
    match CURRENT.read() {
        Ok(current) => current.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Keys that were looked up but exist in no locale, for tests and the debug panel.
pub fn missing_keys() -> Vec<String> {
    let mut keys: Vec<String> = WARNED
        .lock()
        .map(|set| set.iter().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// The marker shown in place of a missing string.
pub fn missing_marker(key: &str) -> String {
    format!("⟦{key}⟧")
}

fn lookup(key: &str, apply: impl FnOnce(&str) -> String) -> String {
    let current = current();
    if let Some(text) = current.get(key).or_else(|| ENGLISH.get(key)) {
        return apply(text);
    }
    let first = WARNED
        .lock()
        .map(|mut set| set.insert(key.to_owned()))
        .unwrap_or(false);
    if first {
        tracing::warn!(key, locale = current.code(), "missing i18n key");
    }
    missing_marker(key)
}

/// The string for `key` in the current locale. Use [`t!`](crate::t) instead.
pub fn text(key: &str) -> String {
    lookup(key, str::to_owned)
}

/// The string for `key` with `{name}` placeholders filled from `args`. A placeholder
/// with no argument is left as written and logged. Use [`t!`](crate::t) instead.
pub fn text_with(key: &str, args: &[(&str, &dyn Display)]) -> String {
    lookup(key, |template| fill(key, template, args))
}

fn fill(key: &str, template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let (brace, after) = (rest.as_bytes()[i], &rest[i + 1..]);
        if after.as_bytes().first() == Some(&brace) {
            out.push(brace as char);
            rest = &after[1..];
            continue;
        }
        let Some(end) = after.find('}').filter(|_| brace == b'{') else {
            out.push(brace as char);
            rest = after;
            continue;
        };
        let name = &after[..end];
        match args.iter().find(|(arg, _)| *arg == name) {
            Some((_, value)) => {
                let _ = write!(out, "{value}");
            }
            None => {
                tracing::warn!(key, placeholder = name, "i18n placeholder has no argument");
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Looks up a user-facing string: `t!("area.key")`, or with named placeholders
/// `t!("export.progress", done = n, total = m)` for `"{done} of {total}"`.
///
/// The key must be a string literal so the source check (`crates/app/tests/ui_source.rs`)
/// can verify it exists in `assets/i18n/en.json`. Returns a `String`.
#[macro_export]
macro_rules! t {
    ($key:literal $(,)?) => {
        $crate::i18n::text($key)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::text_with(
            $key,
            &[$((::core::stringify!($name), &$value as &dyn ::core::fmt::Display)),+],
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_parses_and_every_string_is_well_formed() {
        let en = Locale::from_json(EN_JSON).expect("en.json");
        assert_eq!(en.code(), "en");
        assert_eq!(en.direction(), Direction::Ltr);
        assert!(en.keys().count() > 0);
        assert_eq!(en.get("app.name"), Some("Glass"));
    }

    #[test]
    fn keys_are_lowercase_dotted_segments() {
        for good in ["app.name", "settings.crash_reports.title", "a.b2_c"] {
            assert!(is_valid_key(good), "{good}");
        }
        for bad in [
            "app",
            "App.name",
            "app..name",
            ".app",
            "app.name ",
            "app-name.x",
        ] {
            assert!(!is_valid_key(bad), "{bad}");
        }
    }

    #[test]
    fn placeholders_are_parsed_and_braces_escape() {
        assert_eq!(
            placeholders("{done} of {total}"),
            Some(vec!["done", "total"])
        );
        assert_eq!(placeholders("{{literal}} {x}"), Some(vec!["x"]));
        assert_eq!(placeholders("no braces"), Some(vec![]));
        assert_eq!(placeholders("{open"), None);
        assert_eq!(placeholders("close}"), None);
        assert_eq!(placeholders("{}"), None);
    }

    #[test]
    fn fill_replaces_named_placeholders() {
        let n = 3;
        let args: [(&str, &dyn Display); 2] = [("done", &n), ("total", &"7")];
        assert_eq!(fill("k", "{done} of {total}", &args), "3 of 7");
        assert_eq!(fill("k", "{{x}} {done}", &args), "{x} 3");
        assert_eq!(fill("k", "{unknown} left", &args), "{unknown} left");
    }

    #[test]
    fn macro_looks_up_and_fills() {
        assert_eq!(crate::t!("app.name"), "Glass");
        let n = 2;
        // English has no such key, so the marker comes back with the key in it.
        assert_eq!(crate::t!("test.missing", count = n), "⟦test.missing⟧");
    }

    #[test]
    fn missing_keys_show_a_marker_and_are_recorded() {
        assert_eq!(text("test.not_a_key"), "⟦test.not_a_key⟧");
        assert_eq!(text("test.not_a_key"), "⟦test.not_a_key⟧");
        assert!(missing_keys().contains(&"test.not_a_key".to_owned()));
    }

    #[test]
    fn other_locales_fall_back_to_english_and_carry_direction() {
        let json = r#"{"locale":"ar","name":"العربية","direction":"rtl","strings":{}}"#;
        let ar = Locale::from_json(json).expect("parse");
        assert!(ar.is_rtl());
        assert_eq!(ar.get("app.name"), None);
        // Uses the process-wide locale, so it restores English before asserting.
        set_locale(ar);
        let fallback = text("app.name");
        set_locale(Locale::english());
        assert_eq!(fallback, "Glass");
    }

    #[test]
    fn bad_files_are_rejected() {
        let bad_key = r#"{"locale":"en","name":"E","strings":{"Bad":"x"}}"#;
        assert!(matches!(
            Locale::from_json(bad_key),
            Err(LocaleError::BadKey(_))
        ));
        let bad_text = r#"{"locale":"en","name":"E","strings":{"a.b":"{x"}}"#;
        assert!(matches!(
            Locale::from_json(bad_text),
            Err(LocaleError::BadPlaceholder { .. })
        ));
        assert!(Locale::from_json("{}").is_err());
    }
}
