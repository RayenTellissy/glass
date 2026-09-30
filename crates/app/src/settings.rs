//! Per-user folders and the settings the app reads before the UI exists.
//!
//! Until the Settings dialog exists, settings live in `settings.json` in the user config
//! folder and can be overridden by environment variables (CONTRIBUTING.md, "Crash
//! reports").

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Overrides the user config folder (settings).
pub const CONFIG_DIR_ENV: &str = "GLASS_CONFIG_DIR";
/// Overrides the user data folder (crash reports, later models and asset packs).
pub const DATA_DIR_ENV: &str = "GLASS_DATA_DIR";
/// Overrides the user cache folder (autosaves until projects have their own cache folder).
pub const CACHE_DIR_ENV: &str = "GLASS_CACHE_DIR";
/// `1`/`on`/`true` or `0`/`off`/`false`: overrides the crash report setting.
pub const CRASH_REPORTS_ENV: &str = "GLASS_CRASH_REPORTS";

/// The folder name under the platform's config and data folders.
const APP_DIR: &str = "Glass";

/// Where per-user files go.
#[derive(Clone, Debug)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    /// The platform folders (`~/Library/Application Support/Glass` on macOS,
    /// `%APPDATA%\Glass` on Windows, `$XDG_CONFIG_HOME/Glass` and `$XDG_DATA_HOME/Glass`
    /// on Linux), and the cache folder (`~/Library/Caches/Glass`, `%LOCALAPPDATA%\Glass`,
    /// `$XDG_CACHE_HOME/Glass`), unless the environment overrides them.
    pub fn from_env() -> Self {
        let pick = |env: &str, base: Option<PathBuf>| {
            std::env::var_os(env)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| base.map(|b| b.join(APP_DIR)))
                .unwrap_or_else(|| std::env::temp_dir().join(APP_DIR))
        };
        Self {
            config: pick(CONFIG_DIR_ENV, dirs::config_dir()),
            data: pick(DATA_DIR_ENV, dirs::data_dir()),
            cache: pick(CACHE_DIR_ENV, dirs::cache_dir()),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    /// Where crash reports are written. Nothing reads them but the user.
    pub fn crashes(&self) -> PathBuf {
        self.data.join("crashes")
    }
}

/// The settings read at startup. Every field defaults to off.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Write a minidump and a text report to the data folder's `crashes/` when Glass
    /// crashes. Off unless the user turns it on. Reports are never sent anywhere.
    pub crash_reports: bool,
}

impl Settings {
    /// Reads `settings.json` (a missing file means defaults; an unreadable one is logged
    /// and means defaults), then applies environment overrides.
    pub fn load(paths: &Paths) -> Self {
        let mut settings = Self::read_file(&paths.settings_file());
        if let Some(on) = std::env::var(CRASH_REPORTS_ENV)
            .ok()
            .and_then(|v| parse_switch(&v))
        {
            settings.crash_reports = on;
        }
        settings
    }

    fn read_file(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!("{}: {error}; using default settings", path.display());
                Self::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!("{}: {error}; using default settings", path.display());
                Self::default()
            }
        }
    }
}

fn parse_switch(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => Some(true),
        "0" | "off" | "false" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_reports_default_off() {
        assert!(!Settings::default().crash_reports);
        let missing = Settings::read_file(Path::new("/nonexistent/glass/settings.json"));
        assert!(!missing.crash_reports);
    }

    #[test]
    fn settings_file_turns_crash_reports_on() {
        let settings: Settings =
            serde_json::from_str(r#"{"crash_reports": true, "later": 1}"#).expect("parse");
        assert!(settings.crash_reports);
    }

    #[test]
    fn switch_values() {
        assert_eq!(parse_switch("ON"), Some(true));
        assert_eq!(parse_switch("0"), Some(false));
        assert_eq!(parse_switch("maybe"), None);
    }
}
