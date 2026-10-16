//! Object storage for transcripts and staging files (Spec 04): `transcripts/<talk_id>/v<n>.json`
//! and `citations/<talk_id>.json`. They go to the transcripts bucket when `WT_TRANSCRIPT_BUCKET`
//! is set ([`S3Objects`], Spec 11 PB1.2) and to a local directory under `./data` otherwise
//! ([`LocalObjects`], Spec 01). Audio is never stored here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::s3::{AwsBucket, Bucket, S3Error};

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
    /// Objects remain under a prefix after deleting it.
    #[error("objects remain under {0} after deletion")]
    Remains(String),
    /// The bucket refused.
    #[error(transparent)]
    S3(#[from] S3Error),
}

/// A place for objects by key.
pub trait ObjectStore: Send + Sync {
    /// Stores `bytes` under `key`, replacing what was there.
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), ObjectError>;
    /// The object under `key`, if any.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ObjectError>;
    /// Deletes every object whose key starts with `prefix` and returns how many.
    fn delete_prefix(&self, prefix: &str) -> Result<usize, ObjectError>;
    /// The keys under `prefix`, sorted. A prefix ending in `/` names a directory; anything else
    /// names one object, as in [`ObjectStore::delete_prefix`]. An empty prefix lists everything.
    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError>;
    /// How many stored versions, delete markers included, remain under `prefix`. A store without
    /// versions holds one per key, so the default counts [`ObjectStore::list`].
    fn versions(&self, prefix: &str) -> Result<usize, ObjectError> {
        Ok(self.list(prefix)?.len())
    }
}

impl<T: ObjectStore + ?Sized> ObjectStore for Arc<T> {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), ObjectError> {
        (**self).put(key, bytes)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ObjectError> {
        (**self).get(key)
    }
    fn delete_prefix(&self, prefix: &str) -> Result<usize, ObjectError> {
        (**self).delete_prefix(prefix)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        (**self).list(prefix)
    }
    fn versions(&self, prefix: &str) -> Result<usize, ObjectError> {
        (**self).versions(prefix)
    }
}

/// The store a command uses: the transcripts bucket when `bucket` is set, else `local_root`.
pub fn open(bucket: Option<&str>, local_root: &Path) -> Result<Arc<dyn ObjectStore>, ObjectError> {
    Ok(match bucket {
        Some(name) => Arc::new(S3Objects::new(AwsBucket::connect(name, None)?)),
        None => Arc::new(LocalObjects::new(local_root)),
    })
}

/// Whether `key` is valid: no empty, `.` or `..` segment, backslash or colon, so no key reaches
/// outside a local root and both stores accept the same keys.
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains(['\\', ':']))
}

/// Whether `key` falls under `prefix` as [`ObjectStore::list`] defines it: everything for an
/// empty prefix, a directory for one ending in `/`, else the one object or the directory of
/// that name.
fn under(key: &str, prefix: &str) -> bool {
    if prefix.is_empty() || prefix.ends_with('/') {
        key.starts_with(prefix)
    } else {
        key == prefix || key.strip_prefix(prefix).is_some_and(|r| r.starts_with('/'))
    }
}

/// Objects as files under a directory.
#[derive(Debug, Clone)]
pub struct LocalObjects {
    /// The directory every key lives under.
    root: PathBuf,
}

impl LocalObjects {
    /// A store rooted at `root`, created on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The file for a key under the root. A key with an empty, `.` or `..` segment, a backslash or
    /// a colon is refused, so no key reaches outside the root.
    fn path(&self, key: &str) -> Result<PathBuf, ObjectError> {
        if valid_key(key) {
            Ok(key.split('/').fold(self.root.clone(), |acc, p| acc.join(p)))
        } else {
            Err(ObjectError::BadKey(key.to_owned()))
        }
    }
}

/// Wraps an I/O error with the path it concerns.
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
        // Written beside the target and renamed over it, so a reader sees the old object or the
        // new one, never half of it (October 11, 2026: jobs read transcripts mid-write).
        let mut tmp = path.clone().into_os_string();
        tmp.push(format!(".tmp-{}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, bytes).map_err(io(&tmp))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io(&path)(e)
        })
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

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        let trimmed = prefix.trim_end_matches('/');
        let path = if trimmed.is_empty() {
            self.root.clone()
        } else {
            self.path(trimmed)?
        };
        let mut keys: Vec<String> = if path.is_dir() {
            walk(&path).iter().filter_map(|p| self.key_of(p)).collect()
        } else if path.is_file() {
            self.key_of(&path).into_iter().collect()
        } else {
            Vec::new()
        };
        keys.sort();
        Ok(keys)
    }
}

impl LocalObjects {
    /// The key of a file under the root, with `/` separators on every platform.
    fn key_of(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        Some(parts.join("/"))
    }
}

/// Every file under a directory, recursively; an unreadable directory counts as empty.
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

/// Objects in an S3 bucket: the transcripts bucket through [`AwsBucket`], or the in-process
/// [`crate::s3::FakeBucket`] in tests. Keys and prefixes mean what they mean for
/// [`LocalObjects`].
#[derive(Debug)]
pub struct S3Objects<B = AwsBucket> {
    /// The bucket the objects live in.
    bucket: B,
}

impl<B: Bucket> S3Objects<B> {
    /// A store over `bucket`.
    pub fn new(bucket: B) -> Self {
        Self { bucket }
    }

    /// The bucket underneath.
    pub fn bucket(&self) -> &B {
        &self.bucket
    }

    /// The S3 listing prefix for a store prefix, refusing one that is not a valid key.
    fn listing_prefix<'p>(&self, prefix: &'p str) -> Result<&'p str, ObjectError> {
        let trimmed = prefix.trim_end_matches('/');
        if prefix.is_empty() || valid_key(trimmed) {
            Ok(prefix)
        } else {
            Err(ObjectError::BadKey(prefix.to_owned()))
        }
    }

    /// Every version and delete marker under `prefix`.
    fn all_versions(&self, prefix: &str) -> Result<Vec<crate::s3::Version>, ObjectError> {
        let listing = self.listing_prefix(prefix)?;
        Ok(crate::s3::versions_all(&self.bucket, listing)?
            .into_iter()
            .filter(|v| under(&v.key, prefix))
            .collect())
    }
}

impl<B: Bucket> ObjectStore for S3Objects<B> {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), ObjectError> {
        if !valid_key(key) {
            return Err(ObjectError::BadKey(key.to_owned()));
        }
        Ok(self.bucket.put(key, bytes, false)?)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ObjectError> {
        if !valid_key(key) {
            return Err(ObjectError::BadKey(key.to_owned()));
        }
        Ok(self.bucket.get(key)?)
    }

    /// Deletes every version and delete marker under the prefix, not only the current objects,
    /// so a takedown leaves nothing in a versioned bucket. Returns how many current objects there
    /// were, as the local store does, and fails if any version remains afterwards.
    fn delete_prefix(&self, prefix: &str) -> Result<usize, ObjectError> {
        if prefix.trim_end_matches('/').is_empty() {
            return Err(ObjectError::BadKey(prefix.to_owned()));
        }
        let current = self.list(prefix)?.len();
        let versions = self.all_versions(prefix)?;
        for chunk in versions.chunks(1000) {
            self.bucket.delete_versions(chunk)?;
        }
        if self.all_versions(prefix)?.is_empty() {
            Ok(current)
        } else {
            Err(ObjectError::Remains(prefix.to_owned()))
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        let listing = self.listing_prefix(prefix)?;
        let mut keys: Vec<String> = crate::s3::list_all(&self.bucket, listing)?
            .into_iter()
            .filter(|k| under(k, prefix))
            .collect();
        keys.sort();
        Ok(keys)
    }

    fn versions(&self, prefix: &str) -> Result<usize, ObjectError> {
        Ok(self.all_versions(prefix)?.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_replaces_an_object_whole_and_leaves_no_temporary_file() {
        let dir = std::env::temp_dir().join(format!("wt-objects-{}", uuid::Uuid::new_v4()));
        let store = LocalObjects::new(&dir);
        store
            .put("transcripts/t1/v1.json", b"{\"long\": \"first version\"}")
            .unwrap_or_else(|e| panic!("{e}"));
        store
            .put("transcripts/t1/v1.json", b"{}")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            store.get("transcripts/t1/v1.json").unwrap_or_default(),
            Some(b"{}".to_vec())
        );
        let names: Vec<_> = std::fs::read_dir(dir.join("transcripts/t1"))
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert_eq!(names, vec![std::ffi::OsString::from("v1.json")]);
    }

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
        store
            .put("citations/t1.json", b"{}")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            store.list("transcripts/t1/").unwrap_or_default(),
            ["transcripts/t1/v1.json", "transcripts/t1/v2.json"]
        );
        assert_eq!(
            store.list("citations/t1.json").unwrap_or_default(),
            ["citations/t1.json"]
        );
        assert_eq!(store.list("").unwrap_or_default().len(), 3);
        assert!(store.list("transcripts/t9/").unwrap_or_default().is_empty());
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

    #[test]
    fn a_prefix_without_a_slash_names_one_object_or_its_directory() {
        assert!(under("citations/t1.json", "citations/t1.json"));
        assert!(!under("citations/t1.json.bak", "citations/t1.json"));
        assert!(under("transcripts/t1/v1.json", "transcripts/t1"));
        assert!(!under("transcripts/t10/v1.json", "transcripts/t1"));
        assert!(!under("transcripts/t10/v1.json", "transcripts/t1/"));
        assert!(under("anything", ""));
    }
}
