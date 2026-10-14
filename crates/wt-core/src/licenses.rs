//! The model license record, `docs/model-licenses.md` (Spec 04, Model license record). A stage
//! refuses to start when its configured model is missing from the record or has no recorded
//! revision.
//!
//! A model is listed when its name appears in backticks in a row of one of the record's tables.
//! It has a revision when that row holds a 40-character commit hash or a 64-character file hash,
//! or when the row is in the "Language models" section, where an API model's id is its
//! revision.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Why a model may not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LicenseError {
    /// The record could not be read.
    #[error("reading the model license record {path}: {message}")]
    Read {
        /// Its path.
        path: PathBuf,
        /// The error.
        message: String,
    },
    /// The model is not in the record.
    #[error("model {model} is not listed in {path}; record its license and revision first")]
    Unlisted {
        /// The model.
        model: String,
        /// The record.
        path: PathBuf,
    },
    /// The model is listed without a revision.
    #[error("model {model} is listed in {path} without a revision; record the one downloaded")]
    NoRevision {
        /// The model.
        model: String,
        /// The record.
        path: PathBuf,
    },
}

/// The parsed record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseRecord {
    /// Where the record was read from, for error messages.
    path: PathBuf,
    /// Each listed name and whether some row gives it a revision.
    models: BTreeMap<String, bool>,
}

/// Whether a word is a revision hash, 40 or 64 hex digits. A row with one pins its models'
/// revision, and a ticked hash is not a model name.
fn is_hash(word: &str) -> bool {
    matches!(word.len(), 40 | 64) && word.chars().all(|c| c.is_ascii_hexdigit())
}

impl LicenseRecord {
    /// Parses the record's Markdown.
    pub fn parse(text: &str, path: impl Into<PathBuf>) -> Self {
        let mut models: BTreeMap<String, bool> = BTreeMap::new();
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(h) = line.strip_prefix("## ") {
                section = h.trim().to_owned();
                continue;
            }
            if !line.starts_with('|') || line.starts_with("| ---") || line.starts_with("|---") {
                continue;
            }
            let ticked: Vec<&str> = line.split('`').skip(1).step_by(2).collect();
            let revision = section == "Language models"
                || line
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .any(is_hash);
            for name in ticked {
                let name = name.trim();
                if name.is_empty() || is_hash(name) {
                    continue;
                }
                let entry = models.entry(name.to_owned()).or_insert(false);
                *entry |= revision;
            }
        }
        Self {
            path: path.into(),
            models,
        }
    }

    /// Reads and parses the record at `path`.
    pub fn load(path: &Path) -> Result<Self, LicenseError> {
        let text = std::fs::read_to_string(path).map_err(|e| LicenseError::Read {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
        Ok(Self::parse(&text, path))
    }

    /// Ok when `model` is listed with a revision.
    pub fn require(&self, model: &str) -> Result<(), LicenseError> {
        match self.models.get(model) {
            Some(true) => Ok(()),
            Some(false) => Err(LicenseError::NoRevision {
                model: model.to_owned(),
                path: self.path.clone(),
            }),
            None => Err(LicenseError::Unlisted {
                model: model.to_owned(),
                path: self.path.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: &str = "\
# Model licenses

## Speech recognition

| Model | Loaded as | License | Source and revision |
| --- | --- | --- | --- |
| Whisper large-v3 | `ggml-large-v3.bin` | Apache-2.0 | `openai/whisper-large-v3` at `06f233fe06e710322aca913c1bc4249a0d71fce1` |
| Something | `ggml-tiny.bin` | MIT | not recorded |

## Language models

| Model id | Stages | Added |
| --- | --- | --- |
| `claude-haiku-4-5` | extract | October 6, 2026 |
";

    #[test]
    fn listed_models_need_a_revision() {
        let r = LicenseRecord::parse(RECORD, "docs/model-licenses.md");
        assert_eq!(r.require("ggml-large-v3.bin"), Ok(()));
        assert_eq!(r.require("claude-haiku-4-5"), Ok(()));
        assert!(matches!(
            r.require("ggml-tiny.bin"),
            Err(LicenseError::NoRevision { .. })
        ));
        let err = r.require("ggml-medium.bin").unwrap_err().to_string();
        assert!(
            err.contains("ggml-medium.bin") && err.contains("not listed"),
            "{err}"
        );
    }
}
