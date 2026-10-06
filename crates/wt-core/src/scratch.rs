//! Scratch space for audio (Spec 04, Media handling and scratch space). Audio exists only under
//! `WT_SCRATCH_DIR/<talk_id>/`, owned by a [`ScratchDir`] guard that deletes the directory when
//! it is dropped: on success, on a returned error and during a panic. A retry starts from
//! nothing, so audio never outlives the job that fetched it (decision of October 5, 2026).
//!
//! This module is the only code that may write audio. A guard test greps the workspace for audio
//! file extensions in write paths elsewhere.

use std::path::{Path, PathBuf};

use crate::ids::TalkId;

/// Why scratch space could not be created or removed.
#[derive(Debug, thiserror::Error)]
pub enum ScratchError {
    /// The directory could not be created, or a stale one replaced.
    #[error("creating scratch {path}: {source}")]
    Create {
        /// The directory.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The directory could not be removed.
    #[error("removing scratch {path}: {source}")]
    Remove {
        /// The directory.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The directory still exists after removal, so audio may remain.
    #[error("scratch {0} still exists after removal")]
    StillThere(PathBuf),
    /// A file name tried to leave the scratch directory.
    #[error("{0:?} is not a plain file name")]
    BadName(String),
}

/// A talk's scratch directory. Dropping the guard deletes it.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
    removed: bool,
}

impl ScratchDir {
    /// Creates `root/<talk_id>/`, empty. A directory left by an earlier attempt that crashed
    /// before its guard could run is deleted first, so nothing carries over.
    pub fn create(root: &Path, talk: TalkId) -> Result<Self, ScratchError> {
        let path = root.join(talk.as_uuid().to_string());
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(|source| ScratchError::Create {
                path: path.clone(),
                source,
            })?;
        }
        std::fs::create_dir_all(&path).map_err(|source| ScratchError::Create {
            path: path.clone(),
            source,
        })?;
        Ok(Self {
            path,
            removed: false,
        })
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A file inside the directory. Only plain names are accepted, so nothing is written
    /// outside it.
    pub fn file(&self, name: &str) -> Result<PathBuf, ScratchError> {
        let plain =
            !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':']);
        if plain {
            Ok(self.path.join(name))
        } else {
            Err(ScratchError::BadName(name.to_owned()))
        }
    }

    /// Deletes the directory and checks that it is gone. The job calls this at its end so a
    /// failure to delete is reported, not only logged.
    pub fn finish(mut self) -> Result<(), ScratchError> {
        self.removed = true;
        remove(&self.path)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if !self.removed
            && let Err(e) = remove(&self.path)
        {
            tracing::error!(error = %e, "scratch was not removed");
        }
    }
}

fn remove(path: &Path) -> Result<(), ScratchError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ScratchError::Remove {
                path: path.to_owned(),
                source,
            });
        }
    }
    if path.exists() {
        return Err(ScratchError::StillThere(path.to_owned()));
    }
    Ok(())
}

/// Talk directories under `root`, for the audit and the reaper: any that exists outside a
/// running job is audio residue.
pub fn leftovers(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    match std::fs::read_dir(root) {
        Ok(entries) => entries
            .map(|e| e.map(|e| e.path()))
            .filter(|p| p.as_ref().map_or(true, |p| p.is_dir()))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        std::env::temp_dir().join(format!("wt-scratch-{}", uuid::Uuid::new_v4()))
    }

    fn talk() -> TalkId {
        TalkId::from(uuid::Uuid::new_v4())
    }

    fn fill(dir: &ScratchDir) -> PathBuf {
        let wav = dir.file("audio.wav").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&wav, b"RIFF fixture").unwrap_or_else(|e| panic!("{e}"));
        wav
    }

    #[test]
    fn finish_removes_the_directory() {
        let root = root();
        let dir = ScratchDir::create(&root, talk()).unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().to_owned();
        fill(&dir);
        dir.finish().unwrap_or_else(|e| panic!("{e}"));
        assert!(!path.exists());
        assert!(leftovers(&root).unwrap_or_default().is_empty());
    }

    #[test]
    fn a_returned_error_removes_the_directory() {
        let root = root();
        let mut seen = None;
        let mut run = || -> Result<(), String> {
            let dir = ScratchDir::create(&root, talk()).map_err(|e| e.to_string())?;
            seen = Some(dir.path().to_owned());
            fill(&dir);
            Err("transcription failed".into())
        };
        assert!(run().is_err());
        assert!(!seen.unwrap_or_default().exists());
    }

    #[test]
    fn a_panic_removes_the_directory() {
        let root = root();
        let path = std::sync::Mutex::new(None);
        let result = std::panic::catch_unwind(|| {
            let dir = ScratchDir::create(&root, talk()).unwrap_or_else(|e| panic!("{e}"));
            *path.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.path().to_owned());
            fill(&dir);
            panic!("diarizer crashed");
        });
        assert!(result.is_err());
        let path = path.into_inner().unwrap_or_else(|e| e.into_inner());
        assert!(!path.unwrap_or_default().exists());
    }

    #[test]
    fn a_stale_directory_is_cleared_and_names_cannot_escape() {
        let root = root();
        let id = talk();
        let stale = root.join(id.as_uuid().to_string());
        std::fs::create_dir_all(&stale).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(stale.join("old.wav"), b"left by a crash").unwrap_or_else(|e| panic!("{e}"));
        let dir = ScratchDir::create(&root, id).unwrap_or_else(|e| panic!("{e}"));
        assert!(!dir.path().join("old.wav").exists(), "nothing carries over");
        for bad in ["", "..", "../x.wav", "a/b.wav", "c:\\x.wav"] {
            assert!(dir.file(bad).is_err(), "{bad:?}");
        }
        drop(dir);
        let _ = std::fs::remove_dir_all(&root);
    }
}
