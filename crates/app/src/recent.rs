//! Recently opened projects, newest first, with the cache folder each one autosaves in,
//! kept in `recent.json` in the data folder. The launch scan reads every listed cache
//! folder as well as the per-user one, so an unclean exit in a project that had a file
//! is offered for recovery at the next launch (Decision 55's follow-up), and a restored
//! autosave knows which file it belongs to.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many projects the list keeps.
pub const KEEP: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProject {
    /// The project file.
    pub path: PathBuf,
    /// The cache folder its autosave lives in (`<cache>/autosave/<uid>`).
    pub cache: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recent {
    pub projects: Vec<RecentProject>,
}

impl Recent {
    /// Reads the list; a missing or unreadable file is an empty list (logged).
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!("{}: {error}; starting a new recent list", path.display());
                Self::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!("{}: {error}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(self).expect("the list always serialises"),
        )?;
        std::fs::rename(&tmp, path)
    }

    /// Puts `project` first, dropping an older entry for the same file and any beyond
    /// [`KEEP`].
    pub fn opened(&mut self, project: RecentProject) {
        self.projects.retain(|p| p.path != project.path);
        self.projects.insert(0, project);
        self.projects.truncate(KEEP);
    }

    /// The project file whose autosave lives in `autosave_dir`, if listed.
    pub fn file_for_autosave(&self, autosave_dir: &Path) -> Option<&Path> {
        self.projects
            .iter()
            .find(|p| autosave_dir.starts_with(p.cache.join("autosave")))
            .map(|p| p.path.as_path())
    }

    /// The distinct cache folders, in list order.
    pub fn caches(&self) -> Vec<&Path> {
        let mut out: Vec<&Path> = Vec::new();
        for p in &self.projects {
            if !out.contains(&p.cache.as_path()) {
                out.push(&p.cache);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, cache: &str) -> RecentProject {
        RecentProject {
            path: path.into(),
            cache: cache.into(),
        }
    }

    #[test]
    fn newest_first_without_duplicates() {
        let mut recent = Recent::default();
        recent.opened(entry("/a.proj", "/a/.glass-cache"));
        recent.opened(entry("/b.proj", "/a/.glass-cache"));
        recent.opened(entry("/a.proj", "/a/.glass-cache"));
        assert_eq!(recent.projects[0].path, Path::new("/a.proj"));
        assert_eq!(recent.projects.len(), 2);
        assert_eq!(recent.caches(), [Path::new("/a/.glass-cache")]);
        for i in 0..30 {
            recent.opened(entry(&format!("/{i}.proj"), "/c"));
        }
        assert_eq!(recent.projects.len(), KEEP);
        assert_eq!(
            recent.file_for_autosave(Path::new("/c/autosave/uid-1")),
            Some(Path::new("/29.proj"))
        );
    }

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("glass-recent-{}", std::process::id()));
        let path = dir.join("recent.json");
        let mut recent = Recent::default();
        recent.opened(entry("/x.proj", "/x"));
        recent.save(&path).expect("save");
        assert_eq!(Recent::load(&path), recent);
        std::fs::write(&path, "nonsense").expect("write");
        assert_eq!(Recent::load(&path), Recent::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
