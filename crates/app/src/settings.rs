//! The settings the app reads before the UI exists.
//!
//! Until the Settings dialog exists, settings live in `settings.json` in the config
//! folder ([`crate::locations`]) and can be overridden by environment variables
//! (CONTRIBUTING.md, "Crash reports").

use std::path::Path;

use serde::Deserialize;

use crate::locations::Locations;

/// `1`/`on`/`true` or `0`/`off`/`false`: overrides the crash report setting.
pub const CRASH_REPORTS_ENV: &str = "GLASS_CRASH_REPORTS";

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
    pub fn load(locations: &Locations) -> Self {
        let mut settings = Self::read_file(&locations.settings_file());
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
