//! Diarization and window labeling (Spec 04, Diarization and segmentation). The default
//! `Diarizer` posts the scratch WAV to the sidecar in `sidecars/diarize/`, which runs pyannote and
//! keeps nothing. The `WindowLabeler` for ambiguous windows is the language model in production.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Deserialize;

use crate::segmenter::{Label, Turn, Window};

/// Diarization errors.
#[derive(Debug, thiserror::Error)]
pub enum DiarizeError {
    /// The audio could not be read.
    #[error("reading {path}: {message}")]
    Read {
        /// The file.
        path: String,
        /// What went wrong.
        message: String,
    },
    /// The sidecar could not be reached or answered with an error.
    #[error("the diarization sidecar at {url}: {message}")]
    Sidecar {
        /// Its address.
        url: String,
        /// What went wrong.
        message: String,
    },
}

/// Splits a recording into speaker turns.
#[async_trait]
pub trait Diarizer: Send + Sync {
    /// The turns of `audio`, a WAV file in scratch, labeled `S1`, `S2`, ...
    async fn diarize(&self, audio: &Path) -> Result<Vec<Turn>, DiarizeError>;
}

/// The sidecar at `WT_DIARIZE_URL`.
#[derive(Debug, Clone)]
pub struct SidecarDiarizer {
    url: String,
    client: reqwest::Client,
}

impl SidecarDiarizer {
    /// A client for the sidecar at `url`, such as `http://127.0.0.1:8090`. Diarizing an hour
    /// on a CPU takes minutes, so the timeout is generous. No connection is kept idle: the
    /// sidecar closes one after a few seconds, and reusing it fails the next request.
    pub fn new(url: &str) -> Result<Self, DiarizeError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3 * 60 * 60))
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|e| DiarizeError::Sidecar {
                url: url.to_owned(),
                message: e.to_string(),
            })?;
        Ok(Self {
            url: url.trim_end_matches('/').to_owned(),
            client,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SidecarResponse {
    turns: Vec<Turn>,
}

#[async_trait]
impl Diarizer for SidecarDiarizer {
    async fn diarize(&self, audio: &Path) -> Result<Vec<Turn>, DiarizeError> {
        let bytes = tokio::fs::read(audio)
            .await
            .map_err(|e| DiarizeError::Read {
                path: audio.display().to_string(),
                message: e.to_string(),
            })?;
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name("audio")
            .mime_str("audio/wav")
            .map_err(|e| self.error(e))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let response = self
            .client
            .post(format!("{}/diarize", self.url))
            .multipart(form)
            .send()
            .await
            .map_err(|e| self.error(e.without_url()))?;
        if !response.status().is_success() {
            return Err(DiarizeError::Sidecar {
                url: self.url.clone(),
                message: format!("answered {}", response.status().as_u16()),
            });
        }
        let body: SidecarResponse = response
            .json()
            .await
            .map_err(|e| self.error(e.without_url()))?;
        Ok(body.turns)
    }
}

impl SidecarDiarizer {
    fn error(&self, e: impl std::fmt::Display) -> DiarizeError {
        DiarizeError::Sidecar {
            url: self.url.clone(),
            message: e.to_string(),
        }
    }
}

/// A diarizer that returns scripted turns, for tests.
#[derive(Debug, Default)]
pub struct FakeDiarizer {
    turns: Mutex<Vec<Turn>>,
}

impl FakeDiarizer {
    /// Returns `turns`.
    pub fn new(turns: Vec<Turn>) -> Self {
        Self {
            turns: Mutex::new(turns),
        }
    }
}

#[async_trait]
impl Diarizer for FakeDiarizer {
    async fn diarize(&self, audio: &Path) -> Result<Vec<Turn>, DiarizeError> {
        if !audio.exists() {
            return Err(DiarizeError::Read {
                path: audio.display().to_string(),
                message: "missing".into(),
            });
        }
        Ok(self.turns.lock().map(|t| t.clone()).unwrap_or_default())
    }
}

/// Labeling errors.
#[derive(Debug, thiserror::Error)]
pub enum LabelError {
    /// The labeler failed or answered badly.
    #[error("labeling ambiguous windows: {0}")]
    Failed(String),
    /// The language-model spend cap is reached; the job waits for the next month.
    #[error("labeling ambiguous windows: {0}")]
    CapReached(String),
}

/// Labels the windows the rules leave ambiguous. Each window's text holds only the teacher's
/// words.
#[async_trait]
pub trait WindowLabeler: Send + Sync {
    /// A label for every window given, keyed by its index.
    async fn label(&self, windows: &[Window]) -> Result<BTreeMap<usize, Label>, LabelError>;

    /// Whether its labels are a rule's stand-in for the model's, which the transcript and the
    /// sample report record.
    fn is_fallback(&self) -> bool {
        false
    }
}

/// Labels the windows the rules leave ambiguous without any model, for real talks while no
/// model may see them (owner's decision of October 7, 2026): a window holding the teacher's
/// words is `talk`, any other is `other`. The talk is kept rather than lost, and the count of
/// windows labeled this way is recorded so they can be checked.
#[derive(Debug, Clone, Copy, Default)]
pub struct RuleFallbackLabeler;

#[async_trait]
impl WindowLabeler for RuleFallbackLabeler {
    async fn label(&self, windows: &[Window]) -> Result<BTreeMap<usize, Label>, LabelError> {
        Ok(windows
            .iter()
            .map(|w| {
                let label = if w.text.split_whitespace().next().is_some() {
                    Label::Talk
                } else {
                    Label::Other
                };
                (w.index, label)
            })
            .collect())
    }

    fn is_fallback(&self) -> bool {
        true
    }
}

/// A labeler that gives every window one label, for tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedLabeler(pub Label);

#[async_trait]
impl WindowLabeler for FixedLabeler {
    async fn label(&self, windows: &[Window]) -> Result<BTreeMap<usize, Label>, LabelError> {
        Ok(windows.iter().map(|w| (w.index, self.0)).collect())
    }
}

#[cfg(test)]
mod fallback_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn the_fallback_keeps_teacher_words_as_talk_and_says_it_is_a_fallback() {
        let window = |index, text: &str| Window {
            index,
            start_ms: 0,
            end_ms: 60_000,
            text: text.into(),
        };
        let labels = RuleFallbackLabeler
            .label(&[window(3, "we sit together"), window(7, "  ")])
            .await
            .unwrap();
        assert_eq!(labels[&3], Label::Talk);
        assert_eq!(labels[&7], Label::Other);
        assert!(RuleFallbackLabeler.is_fallback());
        assert!(!FixedLabeler(Label::Talk).is_fallback());
    }
}
