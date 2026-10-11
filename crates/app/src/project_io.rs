//! Opening and saving project files.
//!
//! A project file may name media relative to its own folder (the reference project
//! does, Decision 66), so opening resolves every relative path against the file's
//! folder before anything reads it; files that are not there stay in the project as
//! offline media and are listed in [`Opened::missing`]. Saving writes media inside the
//! project's folder relative to it again, so a project folder moved as a whole (a
//! portable stick, another machine) still finds its media, and writes the file through
//! a temporary file so a crash mid-write leaves the old one.

use std::path::{Path, PathBuf};

use engine::project::{Project, Relink, schema};

/// A project read from disk with its media paths resolved.
#[derive(Debug)]
pub struct Opened {
    pub project: Project,
    /// Media files not found where the project says they are.
    pub missing: Vec<PathBuf>,
}

/// Why a project could not be opened or saved; the text is for the error line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoError(pub String);

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The project file's folder, the base for relative media paths.
fn base(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new("."))
}

/// Reads `path` and points every relative media path at the file beside it.
pub fn open(path: &Path) -> Result<Opened, IoError> {
    let bytes = std::fs::read(path).map_err(|e| IoError(e.to_string()))?;
    let project = schema::load(&bytes).map_err(|e| IoError(e.to_string()))?;
    let dir = base(path);
    let mut relinks = Vec::new();
    let mut missing = Vec::new();
    for media in project.media().iter() {
        let file = Path::new(&media.path);
        let resolved = if file.is_relative() {
            dir.join(file)
        } else {
            file.to_path_buf()
        };
        if !resolved.exists() {
            missing.push(resolved.clone());
        }
        if file.is_relative() {
            relinks.push(Relink {
                media: media.id,
                path: resolved.to_string_lossy().into_owned(),
                content_hash: media.content_hash.clone(),
                duration: None,
            });
        }
    }
    let project = if relinks.is_empty() {
        project
    } else {
        project
            .relink_media(&relinks)
            .map_err(|e| IoError(e.to_string()))?
    };
    Ok(Opened { project, missing })
}

/// `project` as it is written to `path`: media inside the file's folder relative to it.
pub fn for_file(project: &Project, path: &Path) -> Result<Project, IoError> {
    let dir = base(path);
    let relinks: Vec<Relink> = project
        .media()
        .iter()
        .filter_map(|media| {
            let relative = Path::new(&media.path).strip_prefix(dir).ok()?;
            (!relative.as_os_str().is_empty()).then(|| Relink {
                media: media.id,
                path: relative.to_string_lossy().into_owned(),
                content_hash: media.content_hash.clone(),
                duration: None,
            })
        })
        .collect();
    if relinks.is_empty() {
        return Ok(project.clone());
    }
    project
        .relink_media(&relinks)
        .map_err(|e| IoError(e.to_string()))
}

/// Writes `project` to `path` (see the module docs).
pub fn save(project: &Project, path: &Path) -> Result<(), IoError> {
    let bytes = schema::save(&for_file(project, path)?);
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| IoError(e.to_string()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, bytes).map_err(|e| IoError(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| IoError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("glass-app-io-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("footage")).expect("dir");
        dir
    }

    #[test]
    fn media_beside_the_file_round_trips_relative() {
        let dir = temp("relative");
        let clip = dir.join("footage/a.mov");
        std::fs::write(&clip, b"x").expect("clip");
        let elsewhere = "/somewhere/else/b.mov";
        let (p, _) = Project::new("uid-io", "IO")
            .add_media("hash-a", clip.to_string_lossy())
            .expect("media");
        let (p, _) = p.add_media("hash-b", elsewhere).expect("media");
        let file = dir.join("film.proj");
        save(&p, &file).expect("save");

        let written = schema::load(&std::fs::read(&file).expect("read")).expect("load");
        let paths: Vec<String> = written.media().iter().map(|m| m.path.clone()).collect();
        assert_eq!(paths, ["footage/a.mov", elsewhere]);

        let opened = open(&file).expect("open");
        let paths: Vec<String> = opened
            .project
            .media()
            .iter()
            .map(|m| m.path.clone())
            .collect();
        assert_eq!(
            paths,
            [clip.to_string_lossy().into_owned(), elsewhere.into()]
        );
        assert_eq!(opened.missing, [PathBuf::from(elsewhere)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_files_are_errors() {
        let dir = temp("bad");
        assert!(open(&dir.join("none.proj")).is_err());
        std::fs::write(dir.join("bad.proj"), b"{").expect("write");
        assert!(open(&dir.join("bad.proj")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
