//! Stored records for the rights ledger (Spec 08, Rights lifecycle): drafted requests, replies,
//! statements and the owner's mandate decisions. A record is kept under its SHA-256, so storing
//! the same bytes twice keeps one copy and the key proves what was recorded.
//!
//! Records go to a local directory for now. The records bucket (`infra/terraform/records/`)
//! takes them once the project has an S3 client; the key format stays the same.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

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
    dir: PathBuf,
}

impl LocalRecords {
    /// Records under `dir`, created on first store.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Stores `bytes` and returns their key. Storing identical bytes again is a no-op.
    pub fn store(&self, bytes: &[u8]) -> Result<StoredRecord, RecordError> {
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
            key: format!("records/{sha256}"),
            sha256,
        })
    }

    /// Reads a file a person supplied, such as a pasted email, and stores it. An empty file is
    /// refused.
    pub fn store_file(&self, file: &Path) -> Result<StoredRecord, RecordError> {
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
