//! The storage adapter (Spec 03, Source adapters; Seattle Soto Zen sources item 2c): files a
//! sangha gave the owner, such as a folder, a drive download or a bulk export, under a storage
//! source's `root`.
//!
//! Listing gives each file's key (its path under the root), size, modified time and length from
//! `ffprobe`, which reads only the container's header. A file's contents are read only by the
//! media fetch, which copies it into the talk's scratch directory (`crate::media`). The files
//! are the owner's: nothing here or in the pipeline deletes, moves or changes them, and the owner
//! deletes them after the run (`docs/runbook.md`, When SSZ's files arrive).
//!
//! A file is known by its fingerprint, the SHA-256 of its key, size and modified time, which is
//! what a match is keyed by (CLAUDE.md rule 8): hashing the contents would read every file at
//! listing time, which the spec reserves for the fetch.

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use wt_core::records::sha256_hex;

/// Errors from listing or resolving storage files. Paths are the owner's local files, never a
/// secret.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The root or a file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The path.
        path: String,
        /// Why.
        source: std::io::Error,
    },
    /// `ffprobe` could not be started.
    #[error("cannot run {binary}: {source}; install ffmpeg, which includes ffprobe")]
    Probe {
        /// The binary.
        binary: String,
        /// Why.
        source: std::io::Error,
    },
    /// A key that is not a plain relative path under the root.
    #[error("storage key {0:?} is not a file under the storage root")]
    BadKey(String),
}

/// One file under a storage root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageFile {
    /// Its path under the root, with `/` between parts.
    pub key: String,
    /// Size in bytes.
    pub bytes: u64,
    /// Modified time, seconds since 1970.
    pub modified_s: i64,
    /// Length in whole seconds, when `ffprobe` could read it.
    pub duration_s: Option<i32>,
}

impl StorageFile {
    /// The file's fingerprint: SHA-256 of its key, size and modified time.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.key, self.bytes, self.modified_s)
    }

    /// The file's name without its folders or extension.
    pub fn stem(&self) -> &str {
        let name = self.key.rsplit('/').next().unwrap_or_default();
        match name.rfind('.') {
            Some(dot) if dot > 0 => &name[..dot],
            _ => name,
        }
    }
}

/// SHA-256 of a file's key, size and modified time.
pub fn fingerprint(key: &str, bytes: u64, modified_s: i64) -> String {
    sha256_hex(format!("{key}\n{bytes}\n{modified_s}").as_bytes())
}

/// Reads a media file's length. `ffprobe` in production; tests use a fake.
pub trait Prober: Send + Sync {
    /// The file's length in seconds, or `None` when it has no readable length, such as a file
    /// that is not media. An error only when the prober itself cannot run.
    fn duration_s(&self, path: &Path) -> Result<Option<f64>, StorageError>;
}

/// `ffprobe`, which reads the container's header and none of the audio.
#[derive(Debug, Clone)]
pub struct Ffprobe {
    /// The binary, `ffprobe` on the path by default.
    pub binary: PathBuf,
}

impl Default for Ffprobe {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("ffprobe"),
        }
    }
}

impl Prober for Ffprobe {
    fn duration_s(&self, path: &Path) -> Result<Option<f64>, StorageError> {
        let out = Command::new(&self.binary)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(path)
            .output()
            .map_err(|source| StorageError::Probe {
                binary: self.binary.display().to_string(),
                source,
            })?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|d| d.is_finite() && *d > 0.0))
    }
}

/// Lists the files under `root` whose extension is one of `extensions` (lower case), in key
/// order. Hidden files and folders, whose names start with a dot, are skipped, and so is any
/// name that is not valid Unicode. Symbolic links are not followed.
pub fn list(
    root: &Path,
    extensions: &[String],
    prober: &dyn Prober,
) -> Result<Vec<StorageFile>, StorageError> {
    let mut files = Vec::new();
    let mut folders = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = folders.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|source| StorageError::Read {
            path: dir.display().to_string(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| StorageError::Read {
                path: dir.display().to_string(),
                source,
            })?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                tracing::warn!(path = %entry.path().display(), "storage file name is not Unicode; skipped");
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let key = format!("{prefix}{name}");
            let kind = entry.file_type().map_err(|source| StorageError::Read {
                path: entry.path().display().to_string(),
                source,
            })?;
            if kind.is_dir() {
                folders.push((entry.path(), format!("{key}/")));
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let wanted = Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.contains(&e.to_ascii_lowercase()));
            if !wanted {
                continue;
            }
            let meta = entry.metadata().map_err(|source| StorageError::Read {
                path: entry.path().display().to_string(),
                source,
            })?;
            let modified_s = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            let duration_s = prober
                .duration_s(&entry.path())?
                .map(|d| d.round().min(f64::from(i32::MAX)) as i32);
            if duration_s.is_none() {
                tracing::warn!(%key, "ffprobe read no length for this storage file");
            }
            files.push(StorageFile {
                key,
                bytes: meta.len(),
                modified_s,
                duration_s,
            });
        }
    }
    files.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(files)
}

/// The path of the file `key` names under `root`. A key must be a plain relative path: no
/// drive, no leading `/`, no `..`, and the file it reaches, symbolic links followed, must still
/// be under the root.
pub fn resolve(root: &Path, key: &str) -> Result<PathBuf, StorageError> {
    let bad = || StorageError::BadKey(key.to_owned());
    if key.is_empty() || key.contains('\\') {
        return Err(bad());
    }
    let mut path = root.to_path_buf();
    for part in Path::new(key).components() {
        match part {
            Component::Normal(p) => path.push(p),
            _ => return Err(bad()),
        }
    }
    let read = |p: &Path| {
        p.canonicalize().map_err(|source| StorageError::Read {
            path: p.display().to_string(),
            source,
        })
    };
    if !read(&path)?.starts_with(read(root)?) {
        return Err(bad());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedProber;

    impl Prober for FixedProber {
        fn duration_s(&self, path: &Path) -> Result<Option<f64>, StorageError> {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            Ok((!name.contains("broken")).then_some(2400.4))
        }
    }

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("wt-storage-{}", uuid_like()));
        std::fs::create_dir_all(root.join("2021/inner")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        for (name, body) in [
            ("2021/2021-11-07 Fixture Talk.MP4", "aa"),
            ("2021/inner/broken.m4a", "b"),
            ("notes.txt", "c"),
            (".hidden/secret.mp3", "d"),
            ("top.mp3", "eeee"),
        ] {
            std::fs::write(root.join(name), body).unwrap();
        }
        root
    }

    fn uuid_like() -> String {
        format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn listing_finds_media_files_by_key_with_their_size_and_length() {
        let root = temp_root();
        let exts: Vec<String> = ["mp4", "m4a", "mp3"].map(String::from).to_vec();
        let files = list(&root, &exts, &FixedProber).unwrap();
        let keys: Vec<&str> = files.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "2021/2021-11-07 Fixture Talk.MP4",
                "2021/inner/broken.m4a",
                "top.mp3"
            ]
        );
        assert_eq!(files[0].bytes, 2);
        assert_eq!(files[0].duration_s, Some(2400));
        assert_eq!(files[1].duration_s, None);
        assert_eq!(files[0].stem(), "2021-11-07 Fixture Talk");
        assert_ne!(files[0].fingerprint(), files[2].fingerprint());
        assert_eq!(
            files[0].fingerprint(),
            fingerprint(&files[0].key, 2, files[0].modified_s)
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_key_cannot_leave_the_root() {
        let root = temp_root();
        assert!(resolve(&root, "top.mp3").unwrap().ends_with("top.mp3"));
        assert!(resolve(&root, "2021/inner/broken.m4a").is_ok());
        for bad in [
            "",
            "../top.mp3",
            "/etc/passwd",
            "2021/../../x",
            "2021\\x",
            "C:/x",
        ] {
            assert!(resolve(&root, bad).is_err(), "{bad} accepted");
        }
        assert!(matches!(
            resolve(&root, "missing.mp3"),
            Err(StorageError::Read { .. })
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
