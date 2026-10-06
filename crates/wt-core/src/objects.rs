//! Object storage for transcripts and staging files (Spec 04): `transcripts/<talk_id>/v<n>.json`
//! and `citations/<talk_id>.json`. The transcripts bucket takes these once the project has an S3
//! client; until then, and whenever `WT_TRANSCRIPT_BUCKET` is unset, they live in a local
//! directory under `./data` (Spec 01). Audio is never stored here.

use std::path::{Path, PathBuf};

/// Why an object could not be stored or read.
#[derive(Debug, thiserror::Error)]
pub enum ObjectError {
    /// A key tried to leave the store.
    #[error("{0:?} is not a valid object key")]
    BadKey(String),
    /// The file system refused.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// A place for objects by key.
pub trait ObjectStore: Send + Sync {
    /// Stores `bytes` under `key`, replacing what was there.
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), ObjectError>;
    /// The object under `key`, if any.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ObjectError>;
    /// Deletes every object whose key starts with `prefix` and returns how many.
    fn delete_prefix(&self, prefix: &str) -> Result<usize, ObjectError>;
}

/// Objects as files under a directory.
#[derive(Debug, Clone)]
pub struct LocalObjects {
    root: PathBuf,
}

impl LocalObjects {
    /// A store rooted at `root`, created on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, key: &str) -> Result<PathBuf, ObjectError> {
        let parts: Vec<&str> = key.split('/').collect();
        let valid = !key.is_empty()
            && parts
                .iter()
                .all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.contains(['\\', ':']));
        if valid {
            Ok(parts.iter().fold(self.root.clone(), |acc, p| acc.join(p)))
        } else {
            Err(ObjectError::BadKey(key.to_owned()))
        }
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> ObjectError + '_ {
    move |source| ObjectError::Io {
        path: path.to_owned(),
        source,
    }
}

impl ObjectStore for LocalObjects {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), ObjectError> {
        let path = self.path(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io(parent))?;
        }
        std::fs::write(&path, bytes).map_err(io(&path))
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ObjectError> {
        let path = self.path(key)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(&path)(e)),
        }
    }

    fn delete_prefix(&self, prefix: &str) -> Result<usize, ObjectError> {
        // A prefix ending in `/` names a directory; anything else, a file and its siblings
        // that share the stem are not matched, which keeps deletion exact.
        let trimmed = prefix.trim_end_matches('/');
        let path = self.path(trimmed)?;
        if path.is_dir() {
            let count = walk(&path).len();
            std::fs::remove_dir_all(&path).map_err(io(&path))?;
            Ok(count)
        } else if path.is_file() {
            std::fs::remove_file(&path).map_err(io(&path))?;
            Ok(1)
        } else {
            Ok(0)
        }
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_and_delete_by_prefix() {
        let root = std::env::temp_dir().join(format!("wt-objects-{}", uuid::Uuid::new_v4()));
        let store = LocalObjects::new(&root);
        store
            .put("transcripts/t1/v1.json", b"{}")
            .unwrap_or_else(|e| panic!("{e}"));
        store
            .put("transcripts/t1/v2.json", b"{}")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            store.get("transcripts/t1/v1.json").unwrap_or_default(),
            Some(b"{}".to_vec())
        );
        assert_eq!(
            store.get("transcripts/t2/v1.json").unwrap_or_default(),
            None
        );
        assert_eq!(
            store.delete_prefix("transcripts/t1/").unwrap_or_default(),
            2
        );
        assert_eq!(
            store.get("transcripts/t1/v1.json").unwrap_or_default(),
            None
        );
        for bad in ["", "../x", "a//b", "a/../b", "c:\\x"] {
            assert!(store.put(bad, b"x").is_err(), "{bad:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
