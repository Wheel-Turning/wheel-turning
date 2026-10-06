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
    /// A file in scratch could not be written or read.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The converter could not be started or failed.
    #[error("converting {input} to WAV failed: {message}")]
    Convert {
        /// The source file.
        input: PathBuf,
        /// What went wrong, without file contents.
        message: String,
    },
    /// A WAV file is not 16-bit PCM in the expected layout.
    #[error("{0} is not a 16-bit PCM WAV file")]
    NotWav(PathBuf),
}

/// The audio file the pipeline transcribes and diarizes: mono, 16 kHz, 16-bit PCM.
pub const WAV_NAME: &str = "audio.wav";

/// Turns a fetched media file into the WAV the pipeline reads. The fetched file is deleted as
/// soon as the WAV exists (Spec 04, Media handling, rule 2).
pub trait Converter: Send + Sync {
    /// Writes `dir`'s WAV from `input`, a file in `dir`, and returns the WAV's path.
    fn to_wav(&self, dir: &ScratchDir, input: &Path) -> Result<PathBuf, ScratchError>;
}

/// The converter that runs `ffmpeg -vn -ac 1 -ar 16000`.
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    /// The ffmpeg executable, `ffmpeg` on the path by default.
    pub binary: PathBuf,
}

impl Default for Ffmpeg {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("ffmpeg"),
        }
    }
}

impl Converter for Ffmpeg {
    fn to_wav(&self, dir: &ScratchDir, input: &Path) -> Result<PathBuf, ScratchError> {
        let wav = dir.file(WAV_NAME)?;
        if !input.starts_with(dir.path()) {
            return Err(ScratchError::BadName(input.display().to_string()));
        }
        let out = std::process::Command::new(&self.binary)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(input)
            .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
            .arg(&wav)
            .output()
            .map_err(|e| ScratchError::Convert {
                input: input.to_owned(),
                message: e.to_string(),
            })?;
        // The original goes at once, whatever happened.
        let _ = std::fs::remove_file(input);
        if !out.status.success() {
            let message = String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("ffmpeg failed")
                .to_owned();
            return Err(ScratchError::Convert {
                input: input.to_owned(),
                message,
            });
        }
        Ok(wav)
    }
}

/// The length of a 16-bit PCM WAV file in milliseconds, from its header.
pub fn wav_duration_ms(path: &Path) -> Result<i64, ScratchError> {
    use std::io::Read;
    let not_wav = || ScratchError::NotWav(path.to_owned());
    let mut file = std::fs::File::open(path).map_err(|source| ScratchError::Io {
        path: path.to_owned(),
        source,
    })?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).map_err(|_| not_wav())?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(not_wav());
    }
    let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
    loop {
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk).map_err(|_| not_wav())?;
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        if &chunk[0..4] == b"fmt " {
            let mut fmt = vec![0u8; size as usize];
            file.read_exact(&mut fmt).map_err(|_| not_wav())?;
            if fmt.len() < 16 {
                return Err(not_wav());
            }
            channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
            bits = u16::from_le_bytes([fmt[14], fmt[15]]);
        } else if &chunk[0..4] == b"data" {
            let bytes_per_ms = u64::from(rate) * u64::from(channels) * u64::from(bits / 8) / 1000;
            if bytes_per_ms == 0 {
                return Err(not_wav());
            }
            return Ok(i64::try_from(u64::from(size) / bytes_per_ms).unwrap_or(i64::MAX));
        } else {
            std::io::copy(
                &mut (&mut file).take(u64::from(size + size % 2)),
                &mut std::io::sink(),
            )
            .map_err(|_| not_wav())?;
        }
    }
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

    /// Creates a file in the directory for writing, such as a download. Every audio byte the
    /// system writes goes through here or [`Converter`].
    pub fn create_file(&self, name: &str) -> Result<std::fs::File, ScratchError> {
        let path = self.file(name)?;
        std::fs::File::create(&path).map_err(|source| ScratchError::Io { path, source })
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

    /// A mono 16-bit PCM WAV of `ms` milliseconds of silence at `rate` Hz.
    pub(crate) fn wav_bytes(rate: u32, ms: u32) -> Vec<u8> {
        let samples = rate / 1000 * ms;
        let data = samples * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data.to_le_bytes());
        out.resize(out.len() + data as usize, 0);
        out
    }

    #[test]
    fn wav_length_comes_from_the_header() {
        let root = root();
        let dir = ScratchDir::create(&root, talk()).unwrap_or_else(|e| panic!("{e}"));
        let path = dir.file("silence.wav").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&path, wav_bytes(16_000, 2_500)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(wav_duration_ms(&path).unwrap_or_default(), 2_500);
        let junk = dir.file("junk.wav").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&junk, b"not audio").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            wav_duration_ms(&junk),
            Err(ScratchError::NotWav(_))
        ));
    }

    #[test]
    fn ffmpeg_makes_mono_16k_and_deletes_the_original() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffmpeg is not installed; skipping");
            return;
        }
        let root = root();
        let dir = ScratchDir::create(&root, talk()).unwrap_or_else(|e| panic!("{e}"));
        let source = dir.file("source.media").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&source, wav_bytes(44_100, 1_000)).unwrap_or_else(|e| panic!("{e}"));
        let wav = Ffmpeg::default()
            .to_wav(&dir, &source)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!source.exists(), "the fetched file is gone");
        // Resampling can trim a few milliseconds.
        assert!((990..=1_000).contains(&wav_duration_ms(&wav).unwrap_or_default()));
        let header = std::fs::read(&wav).unwrap_or_default();
        assert_eq!(u16::from_le_bytes([header[22], header[23]]), 1, "mono");
        assert_eq!(
            u32::from_le_bytes([header[24], header[25], header[26], header[27]]),
            16_000
        );
        dir.finish().unwrap_or_else(|e| panic!("{e}"));
    }
}
