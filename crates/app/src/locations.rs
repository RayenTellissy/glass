//! Where Glass keeps its own files: the one resolver for the settings, data and cache
//! folders, in normal and portable mode.
//!
//! Each folder is the first writable candidate of:
//!
//! 1. its environment override (`GLASS_CONFIG_DIR`, `GLASS_DATA_DIR`, `GLASS_CACHE_DIR`),
//! 2. in portable mode, `Settings/`, `Data/` or `Cache/` inside the [`PORTABLE_DIR`]
//!    folder next to the executable,
//! 3. the platform folder (`~/Library/Application Support/Glass` and
//!    `~/Library/Caches/Glass` on macOS, `%APPDATA%\Glass` and `%LOCALAPPDATA%\Glass` on
//!    Windows, `$XDG_CONFIG_HOME/Glass`, `$XDG_DATA_HOME/Glass` and
//!    `$XDG_CACHE_HOME/Glass` on Linux),
//! 4. `Glass` in the temporary folder.
//!
//! Portable mode is on when a folder named [`PORTABLE_DIR`] sits next to the executable;
//! on macOS that means next to `Glass.app`, not inside `Contents/MacOS`. A candidate that
//! cannot be created or written (a read-only stick, a locked folder) is skipped with a
//! logged warning, so Glass still starts and keeps its files in the next one.

use std::path::{Path, PathBuf};

/// Overrides the config folder (settings).
pub const CONFIG_DIR_ENV: &str = "GLASS_CONFIG_DIR";
/// Overrides the data folder (crash reports, later models and asset packs).
pub const DATA_DIR_ENV: &str = "GLASS_DATA_DIR";
/// Overrides the cache folder (autosaves until projects have their own cache folder).
pub const CACHE_DIR_ENV: &str = "GLASS_CACHE_DIR";

/// The folder next to the executable that turns portable mode on and holds its files.
pub const PORTABLE_DIR: &str = "Glass Data";

/// The folder name under the platform's folders and the temporary folder.
const APP_DIR: &str = "Glass";

/// One of the folders Glass keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Config,
    Data,
    Cache,
}

impl Kind {
    const ALL: [Self; 3] = [Self::Config, Self::Data, Self::Cache];

    fn env(self) -> &'static str {
        match self {
            Self::Config => CONFIG_DIR_ENV,
            Self::Data => DATA_DIR_ENV,
            Self::Cache => CACHE_DIR_ENV,
        }
    }

    /// The subfolder of [`PORTABLE_DIR`] in portable mode.
    fn portable_name(self) -> &'static str {
        match self {
            Self::Config => "Settings",
            Self::Data => "Data",
            Self::Cache => "Cache",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Data => "data",
            Self::Cache => "cache",
        }
    }
}

/// What the resolver reads from the system, so tests can supply their own.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    /// The running executable.
    pub exe: Option<PathBuf>,
    /// Environment overrides, in [`Kind`] order (config, data, cache).
    pub overrides: [Option<PathBuf>; 3],
    /// The platform's base folders, in [`Kind`] order; `Glass` is appended.
    pub platform: [Option<PathBuf>; 3],
    /// The temporary folder; `Glass` is appended.
    pub temp: PathBuf,
}

impl Inputs {
    pub fn from_env() -> Self {
        let env = |kind: Kind| {
            std::env::var_os(kind.env())
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            exe: std::env::current_exe().ok(),
            overrides: Kind::ALL.map(env),
            platform: [dirs::config_dir(), dirs::data_dir(), dirs::cache_dir()],
            temp: std::env::temp_dir(),
        }
    }
}

/// A candidate folder that was skipped because it could not be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fallback {
    pub kind: Kind,
    pub skipped: PathBuf,
    pub reason: String,
}

impl Fallback {
    /// The warning logged for it.
    pub fn message(&self) -> String {
        format!(
            "{} folder {} is not writable ({}); using the next location",
            self.kind.name(),
            self.skipped.display(),
            self.reason
        )
    }
}

/// Where Glass keeps its files for this session.
#[derive(Clone, Debug)]
pub struct Locations {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    /// The [`PORTABLE_DIR`] folder when portable mode is on.
    pub portable: Option<PathBuf>,
    /// Candidates skipped on the way, each already logged as a warning.
    pub fallbacks: Vec<Fallback>,
}

impl Locations {
    /// Resolves the folders for this process (see the module docs).
    pub fn from_env() -> Self {
        let locations = Self::resolve(&Inputs::from_env());
        match &locations.portable {
            Some(root) => tracing::info!("portable mode: files in {}", root.display()),
            None => tracing::debug!("files in {}", locations.data.display()),
        }
        locations
    }

    /// Picks and creates each folder from `inputs`, logging every skipped candidate.
    pub fn resolve(inputs: &Inputs) -> Self {
        let portable = inputs
            .exe
            .as_deref()
            .and_then(portable_root)
            .filter(|root| root.is_dir());
        let mut fallbacks = Vec::new();
        let [config, data, cache] = Kind::ALL.map(|kind| {
            let i = kind as usize;
            let mut candidates: Vec<PathBuf> = Vec::new();
            candidates.extend(inputs.overrides[i].clone());
            candidates.extend(portable.as_ref().map(|r| r.join(kind.portable_name())));
            candidates.extend(inputs.platform[i].as_ref().map(|b| b.join(APP_DIR)));
            candidates.push(inputs.temp.join(APP_DIR));
            pick(kind, candidates, &mut fallbacks)
        });
        Self {
            config,
            data,
            cache,
            portable,
            fallbacks,
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

/// The [`PORTABLE_DIR`] path for `exe`, whether or not it exists: next to the executable,
/// or next to the `.app` bundle when the executable is its `Contents/MacOS/` binary.
pub fn portable_root(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let base = app_bundle(dir).and_then(Path::parent).unwrap_or(dir);
    Some(base.join(PORTABLE_DIR))
}

/// The `.app` bundle when `dir` is its `Contents/MacOS` folder.
fn app_bundle(dir: &Path) -> Option<&Path> {
    let contents = dir.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = dir.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"));
    is_bundle.then_some(bundle)
}

/// The first candidate that can be created and written; the last one when none can.
fn pick(kind: Kind, candidates: Vec<PathBuf>, fallbacks: &mut Vec<Fallback>) -> PathBuf {
    let count = candidates.len();
    for (n, path) in candidates.into_iter().enumerate() {
        match ensure_writable(&path) {
            Ok(()) => return path,
            Err(error) => {
                let fallback = Fallback {
                    kind,
                    skipped: path.clone(),
                    reason: error.to_string(),
                };
                tracing::warn!("{}", fallback.message());
                fallbacks.push(fallback);
                if n + 1 == count {
                    return path;
                }
            }
        }
    }
    unreachable!("the temporary folder is always a candidate")
}

/// Creates `dir` and checks that a file can be written in it.
fn ensure_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".glass-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("glass-locations-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn inputs(root: &Path, exe: PathBuf) -> Inputs {
        Inputs {
            exe: Some(exe),
            overrides: [None, None, None],
            platform: ["config", "data", "cache"].map(|k| Some(root.join("platform").join(k))),
            temp: root.join("temp"),
        }
    }

    #[test]
    fn standard_locations_without_the_portable_folder() {
        let root = fresh("standard");
        let locations = Locations::resolve(&inputs(&root, root.join("bin/glass")));
        assert_eq!(locations.portable, None);
        assert_eq!(locations.config, root.join("platform/config/Glass"));
        assert_eq!(locations.data, root.join("platform/data/Glass"));
        assert_eq!(locations.cache, root.join("platform/cache/Glass"));
        assert!(locations.cache.is_dir());
        assert!(locations.fallbacks.is_empty());
        assert_eq!(
            locations.settings_file(),
            root.join("platform/config/Glass/settings.json")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn portable_folder_next_to_the_executable() {
        let root = fresh("portable");
        let portable = root.join("bin").join(PORTABLE_DIR);
        std::fs::create_dir_all(&portable).expect("portable folder");
        let locations = Locations::resolve(&inputs(&root, root.join("bin/glass.exe")));
        assert_eq!(locations.portable.as_deref(), Some(portable.as_path()));
        assert_eq!(locations.config, portable.join("Settings"));
        assert_eq!(locations.data, portable.join("Data"));
        assert_eq!(locations.cache, portable.join("Cache"));
        assert_eq!(locations.crashes(), portable.join("Data/crashes"));
        assert!(locations.fallbacks.is_empty());
        assert!(!root.join("platform").exists(), "normal folders untouched");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn portable_folder_sits_next_to_the_app_bundle() {
        let exe = Path::new("/Volumes/Stick/Glass.app/Contents/MacOS/glass");
        assert_eq!(
            portable_root(exe),
            Some(PathBuf::from("/Volumes/Stick").join(PORTABLE_DIR))
        );
        let plain = Path::new("/opt/glass/Contents/MacOS/glass");
        assert_eq!(
            portable_root(plain),
            Some(PathBuf::from("/opt/glass/Contents/MacOS").join(PORTABLE_DIR))
        );

        let root = fresh("bundle");
        let exe = root.join("Glass.app/Contents/MacOS/glass");
        // Inside the bundle does not count.
        std::fs::create_dir_all(exe.with_file_name(PORTABLE_DIR)).expect("inner folder");
        assert_eq!(
            Locations::resolve(&inputs(&root, exe.clone())).portable,
            None
        );
        std::fs::create_dir_all(root.join(PORTABLE_DIR)).expect("portable folder");
        let locations = Locations::resolve(&inputs(&root, exe));
        assert_eq!(locations.config, root.join(PORTABLE_DIR).join("Settings"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn environment_overrides_win_in_both_modes() {
        let root = fresh("override");
        std::fs::create_dir_all(root.join("bin").join(PORTABLE_DIR)).expect("portable folder");
        let mut inputs = inputs(&root, root.join("bin/glass"));
        inputs.overrides[Kind::Cache as usize] = Some(root.join("my-cache"));
        let locations = Locations::resolve(&inputs);
        assert_eq!(locations.cache, root.join("my-cache"));
        assert_eq!(
            locations.config,
            root.join("bin").join(PORTABLE_DIR).join("Settings")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Makes `dir` read-only; false when this user can write it anyway (root).
    #[cfg(unix)]
    fn read_only(dir: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        ensure_writable(&dir.join("probe")).is_err()
    }

    #[cfg(unix)]
    fn writable(dir: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_portable_folder_falls_back_with_a_warning() {
        let root = fresh("read-only-portable");
        let portable = root.join("bin").join(PORTABLE_DIR);
        std::fs::create_dir_all(&portable).expect("portable folder");
        if !read_only(&portable) {
            writable(&portable);
            eprintln!("skipped: permissions are not enforced for this user");
            return;
        }
        let locations = Locations::resolve(&inputs(&root, root.join("bin/glass")));
        writable(&portable);
        assert_eq!(locations.portable.as_deref(), Some(portable.as_path()));
        assert_eq!(locations.config, root.join("platform/config/Glass"));
        assert_eq!(locations.data, root.join("platform/data/Glass"));
        assert_eq!(locations.cache, root.join("platform/cache/Glass"));
        let skipped: Vec<_> = locations
            .fallbacks
            .iter()
            .map(|f| f.skipped.clone())
            .collect();
        assert_eq!(
            skipped,
            ["Settings", "Data", "Cache"].map(|n| portable.join(n))
        );
        let message = locations.fallbacks[0].message();
        assert!(message.starts_with("config folder "), "{message}");
        assert!(message.contains("not writable"), "{message}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_platform_folder_falls_back_to_temp() {
        let root = fresh("read-only-platform");
        let base = root.join("platform");
        std::fs::create_dir_all(&base).expect("platform folder");
        if !read_only(&base) {
            writable(&base);
            eprintln!("skipped: permissions are not enforced for this user");
            return;
        }
        let locations = Locations::resolve(&inputs(&root, root.join("bin/glass")));
        writable(&base);
        assert_eq!(locations.portable, None);
        assert_eq!(locations.data, root.join("temp/Glass"));
        assert_eq!(locations.fallbacks.len(), 3);
        assert_eq!(locations.fallbacks[1].kind, Kind::Data);
        assert_eq!(locations.fallbacks[1].skipped, base.join("data/Glass"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
