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
//! holds one file per analysis, named by kind (for example `gop-0.json` for the GOP index
//! of stream 0). Files are written to a temporary name and renamed, so a reader never
//! sees a partial file. Everything under `analysis/` is regenerable.

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
        let dir = self.analysis_dir(hash);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(name);
        let tmp = dir.join(format!(".{name}.{}.partial", std::process::id()));
        let data = serde_json::to_vec(value).map_err(std::io::Error::other)?;
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&data)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }
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
}
