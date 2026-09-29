//! The project cache folder and the content hash that keys per-file analysis in it.
//!
//! **Content hash** ([`ContentHash::of_file`]): BLAKE3 over a version tag, the file size
//! (u64, little-endian), the first 1 MiB, eight 64 KiB blocks at evenly spaced offsets,
//! and the last 1 MiB (a file of 2 MiB plus one block or less is hashed whole). It reads at most 2.5 MiB
//! whatever the file's size, so hashing a 50 GB camera file costs a few milliseconds, and
//! it depends only on bytes, not on the path or the modification time, so moving,
//! renaming, copying or relinking a file keeps its cache. Two different media files of
//! the same size that agree in all sampled blocks are the accepted collision risk:
//! compressed video differs everywhere after an edit, and headers (first block) and
//! indexes (MP4 `moov` at either end) are always sampled.
//!
//! **Layout** (see docs/ARCHITECTURE.md, Cache design): `<cache>/analysis/<hash hex>/`
//! holds the analyses of one source. The GOP index is `gop-video.json` (or `gop-<stream>.json`);
//! everything else goes through [`CacheEntry`]: `<kind>-<params hash>-v<version>.<suffix>`,
//! so a change of settings or format version never reads an old result. Files are
//! written to a temporary name and renamed, so a reader never sees a partial file.
//! Everything under `analysis/` is regenerable.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Bumped when the sampling changes, so old cache entries stop matching.
const CONTENT_HASH_VERSION: &[u8] = b"glass-content-hash-v1";
const EDGE: u64 = 1 << 20;
const BLOCK: u64 = 64 << 10;
const BLOCKS: u64 = 8;

/// A file's identity by content (see the module docs for the scheme).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    pub fn of_file(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        let mut hasher = blake3::Hasher::new();
        hasher.update(CONTENT_HASH_VERSION);
        hasher.update(&size.to_le_bytes());
        if size <= 2 * EDGE + BLOCK {
            std::io::copy(&mut file, &mut hasher)?;
        } else {
            let mut buffer = vec![0u8; EDGE as usize];
            let mut sample = |file: &mut File, at: u64, len: u64| -> std::io::Result<()> {
                file.seek(SeekFrom::Start(at))?;
                let chunk = &mut buffer[..len as usize];
                file.read_exact(chunk)?;
                hasher.update(chunk);
                Ok(())
            };
            sample(&mut file, 0, EDGE)?;
            let span = size - 2 * EDGE - BLOCK;
            for i in 0..BLOCKS {
                sample(&mut file, EDGE + span * i / (BLOCKS - 1), BLOCK)?;
            }
            sample(&mut file, size - EDGE, EDGE)?;
        }
        Ok(Self(*hasher.finalize().as_bytes()))
    }

    /// Lower-case hexadecimal, 64 characters.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A project cache folder (`.glass-cache/` beside the project file by default).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheDir {
    root: PathBuf,
}

impl CacheDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default cache folder of a project: `.glass-cache/` next to the project file.
    pub fn for_project(project_file: impl AsRef<Path>) -> Self {
        let dir = project_file
            .as_ref()
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self::new(dir.join(".glass-cache"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The analysis folder of one source file.
    pub fn analysis_dir(&self, hash: &ContentHash) -> PathBuf {
        self.root.join("analysis").join(hash.to_hex())
    }

    /// Path of one analysis file of a source.
    pub fn analysis_file(&self, hash: &ContentHash, name: &str) -> PathBuf {
        self.analysis_dir(hash).join(name)
    }

    /// Reads a JSON analysis file; `Ok(None)` when it does not exist or no longer parses
    /// (an older format), so the caller rebuilds it.
    pub fn read_json<T: DeserializeOwned>(
        &self,
        hash: &ContentHash,
        name: &str,
    ) -> std::io::Result<Option<T>> {
        let path = self.analysis_file(hash, name);
        let text = match std::fs::read(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        match serde_json::from_slice(&text) {
            Ok(value) => Ok(Some(value)),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "ignoring an unreadable cache file");
                Ok(None)
            }
        }
    }

    /// Writes a JSON analysis file atomically (temporary name, then rename).
    pub fn write_json<T: Serialize>(
        &self,
        hash: &ContentHash,
        name: &str,
        value: &T,
    ) -> std::io::Result<PathBuf> {
        let data = serde_json::to_vec(value).map_err(std::io::Error::other)?;
        write_atomic(&self.analysis_dir(hash), name, &data)
    }

    /// The files of one analysis result: `kind` names the analysis, `params` its settings
    /// (hashed into the name, so different settings live side by side) and `version` its
    /// format (bumping it makes older files miss). See [`CacheEntry`].
    pub fn entry(&self, hash: &ContentHash, kind: &str, params: &str, version: u32) -> CacheEntry {
        self.entry_hashed(hash, kind, &params_hash(params), version)
    }

    /// [`Self::entry`] with the parameters already hashed ([`params_hash`]).
    pub fn entry_hashed(
        &self,
        hash: &ContentHash,
        kind: &str,
        params_hash: &str,
        version: u32,
    ) -> CacheEntry {
        CacheEntry {
            dir: self.analysis_dir(hash),
            kind: kind.to_string(),
            stem: format!("{kind}-{params_hash}-v{version}"),
        }
    }
}

/// The first 8 bytes of BLAKE3 over an analysis's canonical parameter string, as hex.
pub fn params_hash(params: &str) -> String {
    blake3::hash(params.as_bytes()).as_bytes()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A source file and its [`ContentHash`], which callers compute once (off the UI
/// thread: hashing reads up to 2.5 MiB) and pass to analyses.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceFile {
    pub path: PathBuf,
    pub hash: ContentHash,
}

impl SourceFile {
    /// Hashes `path`.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let hash = ContentHash::of_file(&path)?;
        Ok(Self { path, hash })
    }
}

/// One analysis result on disk: `<cache>/analysis/<hash>/<kind>-<params hash>-v<version>`
/// followed by `.` and an extension for the main file (`.json`, `.bin`), or by `.` and a
/// suffix for side files (thumbnail atlases). The main file is written last, so its
/// presence means the result is complete. Every write is atomic (temporary name, then
/// rename).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheEntry {
    dir: PathBuf,
    kind: String,
    stem: String,
}

impl CacheEntry {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<kind>-<params hash>-v<version>`.
    pub fn stem(&self) -> &str {
        &self.stem
    }

    /// Name of the file with this suffix (`json`, `l0.c3.jpg`, ...).
    pub fn file_name(&self, suffix: &str) -> String {
        format!("{}.{suffix}", self.stem)
    }

    pub fn path(&self, suffix: &str) -> PathBuf {
        self.dir.join(self.file_name(suffix))
    }

    /// The file's bytes, or `Ok(None)` when it does not exist.
    pub fn read(&self, suffix: &str) -> std::io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(suffix)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn write(&self, suffix: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        write_atomic(&self.dir, &self.file_name(suffix), bytes)
    }

    /// Reads a JSON file; `None` when it is missing or does not parse.
    pub fn read_json<T: DeserializeOwned>(&self, suffix: &str) -> Option<T> {
        let bytes = match self.read(suffix) {
            Ok(bytes) => bytes?,
            Err(error) => {
                tracing::warn!(entry = %self.stem, %error, "unreadable cache file");
                return None;
            }
        };
        serde_json::from_slice(&bytes)
            .inspect_err(|error| {
                tracing::warn!(entry = %self.stem, %error, "ignoring an unparsable cache file");
            })
            .ok()
    }

    pub fn write_json<T: Serialize>(&self, suffix: &str, value: &T) -> std::io::Result<PathBuf> {
        let data = serde_json::to_vec(value).map_err(std::io::Error::other)?;
        self.write(suffix, &data)
    }

    /// Deletes files of the same kind written by other format versions (any parameters),
    /// which can never match again. Returns how many were removed.
    pub fn remove_other_versions(&self) -> std::io::Result<usize> {
        let Some(version) = self.stem.rsplit('-').next() else {
            return Ok(0);
        };
        let prefix = format!("{}-", self.kind);
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut removed = 0;
        for item in entries {
            let item = item?;
            let name = item.file_name();
            let Some(name) = name.to_str() else { continue };
            // <kind>-<16 hex>-v<n>.<suffix>
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Some((stem_rest, _)) = rest.split_once('.') else {
                continue;
            };
            let mut parts = stem_rest.splitn(2, '-');
            let (Some(hash), Some(other)) = (parts.next(), parts.next()) else {
                continue;
            };
            let is_ours = hash.len() == 16
                && other.starts_with('v')
                && other[1..].bytes().all(|b| b.is_ascii_digit());
            if is_ours && other != version {
                std::fs::remove_file(item.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn write_atomic(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}-{n}.partial", std::process::id()));
    {
        let mut file = File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("glass-cache-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn hash_depends_on_bytes_not_path() {
        let dir = temp_dir("hash");
        let big: Vec<u8> = (0..(5u32 << 20))
            .map(|i| (i * 7 + i / 4096) as u8)
            .collect();
        let a = dir.join("a.bin");
        let b = dir.join("moved.bin");
        std::fs::write(&a, &big).expect("write");
        std::fs::write(&b, &big).expect("write");
        let ha = ContentHash::of_file(&a).expect("hash");
        assert_eq!(ha, ContentHash::of_file(&b).expect("hash"));
        // A change in a sampled middle block changes the hash.
        let mut changed = big.clone();
        changed[(1 << 20) + 5] ^= 1;
        std::fs::write(&b, &changed).expect("write");
        assert_ne!(ha, ContentHash::of_file(&b).expect("hash"));
        // Small files are hashed whole.
        std::fs::write(&a, b"small").expect("write");
        std::fs::write(&b, b"smalL").expect("write");
        assert_ne!(
            ContentHash::of_file(&a).expect("hash"),
            ContentHash::of_file(&b).expect("hash")
        );
        assert_eq!(ha.to_hex().len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_round_trip_and_layout() {
        let dir = temp_dir("json");
        let cache = CacheDir::new(dir.join(".glass-cache"));
        let hash = ContentHash([7; 32]);
        assert_eq!(
            cache.read_json::<Vec<u32>>(&hash, "x.json").expect("read"),
            None
        );
        let path = cache
            .write_json(&hash, "x.json", &vec![1u32, 2, 3])
            .expect("write");
        assert!(path.starts_with(dir.join(".glass-cache/analysis").join(hash.to_hex())));
        assert_eq!(
            cache.read_json::<Vec<u32>>(&hash, "x.json").expect("read"),
            Some(vec![1, 2, 3])
        );
        std::fs::write(&path, b"not json").expect("write");
        assert_eq!(
            cache.read_json::<Vec<u32>>(&hash, "x.json").expect("read"),
            None
        );
        assert_eq!(
            CacheDir::for_project("/work/film.glass").root(),
            Path::new("/work/.glass-cache")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entries_are_named_by_kind_params_and_version() {
        let dir = temp_dir("entry");
        let cache = CacheDir::new(&dir);
        let hash = ContentHash([3; 32]);
        let v1 = cache.entry(&hash, "peaks", "rate=48000", 1);
        let other_params = cache.entry(&hash, "peaks", "rate=44100", 1);
        let v2 = cache.entry(&hash, "peaks", "rate=48000", 2);
        let thumbs = cache.entry(&hash, "thumbs", "h=108", 1);
        assert_ne!(v1.stem(), other_params.stem());
        assert!(v1.stem().starts_with("peaks-") && v1.stem().ends_with("-v1"));
        assert_eq!(v1.stem().len(), "peaks-".len() + 16 + "-v1".len());
        v1.write("bin", b"one").expect("write");
        v1.write("l0.c0.jpg", b"side").expect("write");
        other_params.write("bin", b"two").expect("write");
        thumbs.write("json", b"{}").expect("write");
        assert_eq!(v1.read("bin").expect("read"), Some(b"one".to_vec()));
        assert_eq!(v2.read("bin").expect("read"), None);
        // Version 2 removes both version-1 peaks results, and nothing else.
        assert_eq!(v2.remove_other_versions().expect("remove"), 3);
        assert_eq!(v1.read("bin").expect("read"), None);
        assert!(thumbs.read("json").expect("read").is_some());
        assert_eq!(v2.remove_other_versions().expect("remove"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
