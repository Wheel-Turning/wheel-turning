//! Stored records for the rights ledger (Spec 08, Rights lifecycle): drafted requests, replies,
//! statements and the owner's mandate decisions. A record is kept under its SHA-256, so storing
//! the same bytes twice keeps one copy and the key proves what was recorded.
//!
//! Records go to the records bucket (`infra/terraform/records/`) when `WT_RECORDS_BUCKET` is set
//! ([`S3Records`], Spec 11 PB1.2), and to a local directory otherwise ([`LocalRecords`]). The key
//! is `records/<sha256>` in both. The bucket keeps every version under an Object Lock legal hold,
//! so records survive takedown, which deletes transcripts but never the record of what was
//! agreed.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::s3::{AwsBucket, Bucket, S3Error};

/// Why a record could not be stored.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// The file or directory could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A statement or draft was empty, which records nothing.
    #[error("{0} is empty")]
    Empty(PathBuf),
    /// The bucket refused.
    #[error(transparent)]
    S3(#[from] S3Error),
}

/// Where records are kept. Every implementation stores a record under its SHA-256 and never
/// replaces or deletes one.
pub trait Records: Send + Sync {
    /// Stores `bytes` and returns their key. Storing identical bytes again is a no-op.
    fn store(&self, bytes: &[u8]) -> Result<StoredRecord, RecordError>;
    /// The bytes recorded under a SHA-256, if any.
    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, RecordError>;
    /// The SHA-256 of every stored record, sorted.
    fn list(&self) -> Result<Vec<String>, RecordError>;

    /// Reads a file a person supplied, such as a pasted email, and stores it. An empty file is
    /// refused.
    fn store_file(&self, file: &Path) -> Result<StoredRecord, RecordError> {
        let bytes = std::fs::read(file).map_err(|source| RecordError::Io {
            path: file.to_owned(),
            source,
        })?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Err(RecordError::Empty(file.to_owned()));
        }
        self.store(&bytes)
    }
}

impl<T: Records + ?Sized> Records for Box<T> {
    fn store(&self, bytes: &[u8]) -> Result<StoredRecord, RecordError> {
        (**self).store(bytes)
    }
    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, RecordError> {
        (**self).get(sha256)
    }
    fn list(&self) -> Result<Vec<String>, RecordError> {
        (**self).list()
    }
}

/// The records store a command uses: the records bucket when `bucket` is set, else `local_dir`.
pub fn open(bucket: Option<&str>, local_dir: &Path) -> Result<Box<dyn Records>, RecordError> {
    Ok(match bucket {
        Some(name) => Box::new(S3Records::new(AwsBucket::connect(name, None)?)),
        None => Box::new(LocalRecords::new(local_dir)),
    })
}

/// The key prefix records live under, in the bucket and in `evidence_ref` and its kin.
pub const PREFIX: &str = "records/";

/// Whether `name` is a lowercase hex SHA-256, the only name a record has.
fn is_sha256(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A stored record: its key and the SHA-256 of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRecord {
    /// `records/<sha256>`, stored as `evidence_ref`, `draft_ref` or `reply_ref`.
    pub key: String,
    /// Lowercase hex SHA-256 of the bytes.
    pub sha256: String,
}

/// The lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Records kept in a local directory.
#[derive(Debug, Clone)]
pub struct LocalRecords {
    /// The directory records are stored in.
    dir: PathBuf,
}

impl LocalRecords {
    /// Records under `dir`, created on first store.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl Records for LocalRecords {
    fn store(&self, bytes: &[u8]) -> Result<StoredRecord, RecordError> {
        let sha256 = sha256_hex(bytes);
        let path = self.dir.join(&sha256);
        if !path.exists() {
            std::fs::create_dir_all(&self.dir).map_err(|source| RecordError::Io {
                path: self.dir.clone(),
                source,
            })?;
            std::fs::write(&path, bytes).map_err(|source| RecordError::Io {
                path: path.clone(),
                source,
            })?;
        }
        Ok(StoredRecord {
            key: format!("{PREFIX}{sha256}"),
            sha256,
        })
    }

    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, RecordError> {
        if !is_sha256(sha256) {
            return Ok(None);
        }
        let path = self.dir.join(sha256);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(RecordError::Io { path, source }),
        }
    }

    fn list(&self) -> Result<Vec<String>, RecordError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(RecordError::Io {
                    path: self.dir.clone(),
                    source,
                });
            }
        };
        let mut out: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| is_sha256(n))
            .collect();
        out.sort();
        Ok(out)
    }
}

/// Records in the records bucket under `records/<sha256>`, each written with a legal hold. The
/// bucket's policy refuses a write without one, and nothing here deletes.
#[derive(Debug)]
pub struct S3Records<B = AwsBucket> {
    /// The records bucket.
    bucket: B,
}

impl<B: Bucket> S3Records<B> {
    /// Records in `bucket`.
    pub fn new(bucket: B) -> Self {
        Self { bucket }
    }

    /// The bucket underneath.
    pub fn bucket(&self) -> &B {
        &self.bucket
    }
}

impl<B: Bucket> Records for S3Records<B> {
    fn store(&self, bytes: &[u8]) -> Result<StoredRecord, RecordError> {
        let sha256 = sha256_hex(bytes);
        let key = format!("{PREFIX}{sha256}");
        // Checked first, so storing the same bytes again adds no version to the locked bucket.
        if !self.bucket.exists(&key)? {
            self.bucket.put(&key, bytes, true)?;
        }
        Ok(StoredRecord { key, sha256 })
    }

    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, RecordError> {
        if !is_sha256(sha256) {
            return Ok(None);
        }
        Ok(self.bucket.get(&format!("{PREFIX}{sha256}"))?)
    }

    fn list(&self) -> Result<Vec<String>, RecordError> {
        let mut out: Vec<String> = crate::s3::list_all(&self.bucket, PREFIX)?
            .into_iter()
            .filter_map(|k| k.strip_prefix(PREFIX).map(str::to_owned))
            .filter(|n| is_sha256(n))
            .collect();
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_kept_once_under_its_hash() {
        let dir = std::env::temp_dir().join(format!("wt-records-{}", uuid::Uuid::new_v4()));
        let records = LocalRecords::new(&dir);
        let first = records
            .store(b"We decline.")
            .unwrap_or_else(|e| panic!("{e}"));
        let again = records
            .store(b"We decline.")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first, again);
        assert_eq!(first.key, format!("records/{}", first.sha256));
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let empty = dir.join("empty.txt");
        std::fs::write(&empty, " \n").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            records.store_file(&empty),
            Err(RecordError::Empty(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
