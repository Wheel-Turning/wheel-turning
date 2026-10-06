//! Speech recognition (Spec 04, Transcription): the `Transcriber` trait, the whisper.cpp
//! implementation and a scripted fake for tests.
//!
//! whisper.cpp runs as a subprocess, the `whisper-cli` build measured in `docs/asr-benchmark.md`,
//! rather than linked through `whisper-rs` (decision of October 5, 2026). Its raw output holds
//! every speaker's words, so it is written only into the talk's scratch directory and read back
//! from there.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Deserialize;
use wt_core::scratch::{ScratchDir, wav_duration_ms};
use wt_core::transcript::Word;

/// Recognition errors.
#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    /// whisper-cli could not be started.
    #[error("starting {binary}: {message}")]
    Start {
        /// The executable.
        binary: String,
        /// What went wrong.
        message: String,
    },
    /// whisper-cli ran and failed.
    #[error("whisper-cli failed: {0}")]
    Failed(String),
    /// Its output could not be read.
    #[error("reading the recognizer's output: {0}")]
    Output(String),
    /// Scratch space refused.
    #[error(transparent)]
    Scratch(#[from] wt_core::scratch::ScratchError),
}

/// What steers recognition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsrHints {
    /// Canonical glossary terms joined by commas, the initial prompt.
    pub glossary_prompt: String,
    /// Spoken language, `en`.
    pub language: String,
}

/// Every word recognized, from every speaker. It stays in scratch.
#[derive(Debug, Clone, PartialEq)]
pub struct RawTranscript {
    /// The model's name, recorded with the transcript.
    pub model: String,
    /// Length of the audio.
    pub audio_ms: i64,
    /// The words, in order.
    pub words: Vec<Word>,
}

/// A speech recognizer.
#[async_trait]
pub trait Transcriber: Send + Sync {
    /// Transcribes `audio`, a WAV file in `scratch`.
    async fn transcribe(
        &self,
        scratch: &ScratchDir,
        audio: &Path,
        hints: &AsrHints,
    ) -> Result<RawTranscript, AsrError>;
}

/// The glossary prompt: canonical terms joined by commas, cut to about 200 tokens. whisper.cpp
/// takes at most half its text context as a prompt, and a token is roughly four characters of
/// English.
pub fn glossary_prompt<'a>(terms: impl IntoIterator<Item = &'a str>) -> String {
    const MAX_CHARS: usize = 800;
    let mut out = String::new();
    for term in terms {
        let piece = if out.is_empty() {
            term.to_owned()
        } else {
            format!(", {term}")
        };
        if out.len() + piece.len() > MAX_CHARS {
            break;
        }
        out.push_str(&piece);
    }
    out
}

/// `[asr]` in `config/pipeline.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AsrSettings {
    /// Spoken language.
    pub language: String,
    /// Beam size.
    pub beam_size: u32,
    /// Whether to use the GPU.
    pub use_gpu: bool,
    /// CPU threads.
    pub threads: u32,
}

impl Default for AsrSettings {
    fn default() -> Self {
        Self {
            language: "en".into(),
            beam_size: 5,
            use_gpu: true,
            threads: 8,
        }
    }
}

impl WhisperCli {
    /// The transcriber for `[asr]` with the executable from `WT_WHISPER_CLI` and the model from
    /// `WT_WHISPER_MODEL`.
    pub fn new(binary: PathBuf, model: PathBuf, settings: &AsrSettings) -> Self {
        Self {
            binary,
            model,
            threads: settings.threads,
            beam_size: settings.beam_size,
            use_gpu: settings.use_gpu,
        }
    }
}

/// The whisper.cpp transcriber, with the paths from `WT_WHISPER_CLI` and `WT_WHISPER_MODEL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhisperCli {
    /// The `whisper-cli` executable.
    pub binary: PathBuf,
    /// The ggml model file.
    pub model: PathBuf,
    /// CPU threads.
    pub threads: u32,
    /// Beam size.
    pub beam_size: u32,
    /// Whether to use the GPU.
    pub use_gpu: bool,
}

impl WhisperCli {
    /// The model's name as recorded in the transcript: `ggml-large-v3.bin` is
    /// `whisper-large-v3`.
    pub fn model_name(&self) -> String {
        model_name(&self.model)
    }
}

/// `ggml-large-v3.bin` gives `whisper-large-v3`, and `ggml-distil-large-v3.bin` gives
/// `whisper-distil-large-v3`.
pub fn model_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    format!("whisper-{}", stem.strip_prefix("ggml-").unwrap_or(stem))
}

#[derive(Debug, Deserialize)]
struct CliOutput {
    transcription: Vec<CliSegment>,
}

#[derive(Debug, Deserialize)]
struct CliSegment {
    offsets: CliOffsets,
    text: String,
    #[serde(default)]
    tokens: Vec<CliToken>,
}

#[derive(Debug, Deserialize)]
struct CliOffsets {
    from: i64,
    to: i64,
}

#[derive(Debug, Deserialize)]
struct CliToken {
    text: String,
    #[serde(default)]
    p: f32,
}

fn special(text: &str) -> bool {
    text.trim_start().starts_with("[_")
}

/// Reads whisper-cli's `--output-json-full` with one word per segment (`-ml 1 -sow`). Special
/// tokens and empty segments are skipped, and a word's probability is the mean of its tokens'.
pub fn parse_cli_json(json: &str) -> Result<Vec<Word>, AsrError> {
    let out: CliOutput = serde_json::from_str(json).map_err(|e| AsrError::Output(e.to_string()))?;
    let clamp = |ms: i64| i32::try_from(ms.max(0)).unwrap_or(i32::MAX);
    Ok(out
        .transcription
        .into_iter()
        .filter(|s| !s.text.trim().is_empty() && !special(&s.text))
        .map(|s| {
            let probs: Vec<f32> = s
                .tokens
                .iter()
                .filter(|t| !special(&t.text))
                .map(|t| t.p)
                .collect();
            let prob = if probs.is_empty() {
                0.0
            } else {
                probs.iter().sum::<f32>() / probs.len() as f32
            };
            let start = clamp(s.offsets.from);
            Word::new(s.text.trim(), start, clamp(s.offsets.to).max(start), prob)
        })
        .collect())
}

#[async_trait]
impl Transcriber for WhisperCli {
    async fn transcribe(
        &self,
        scratch: &ScratchDir,
        audio: &Path,
        hints: &AsrHints,
    ) -> Result<RawTranscript, AsrError> {
        let stem = scratch.file("asr")?;
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("-m")
            .arg(&self.model)
            .arg("-f")
            .arg(audio)
            .args(["-l", &hints.language])
            .args(["-t", &self.threads.to_string()])
            .args(["-bs", &self.beam_size.to_string()])
            .args(["-ml", "1", "-sow", "-ojf", "-np"])
            .arg("-of")
            .arg(&stem);
        if !hints.glossary_prompt.is_empty() {
            cmd.arg("--prompt").arg(&hints.glossary_prompt);
        }
        if !self.use_gpu {
            cmd.arg("-ng");
        }
        let out = cmd.output().await.map_err(|e| AsrError::Start {
            binary: self.binary.display().to_string(),
            message: e.to_string(),
        })?;
        if !out.status.success() {
            // The last line of stderr; it names the problem and holds no transcript text.
            let line = String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("no message")
                .to_owned();
            return Err(AsrError::Failed(line));
        }
        let json_path = scratch.file("asr.json")?;
        let json =
            std::fs::read_to_string(&json_path).map_err(|e| AsrError::Output(e.to_string()))?;
        Ok(RawTranscript {
            model: self.model_name(),
            audio_ms: wav_duration_ms(audio)?,
            words: parse_cli_json(&json)?,
        })
    }
}

/// A transcriber that returns a scripted word list, for tests.
#[derive(Debug, Default)]
pub struct FakeTranscriber {
    words: Mutex<Vec<Word>>,
    /// The audio length it reports.
    pub audio_ms: i64,
    /// When set, every call fails with this message.
    pub fail_with: Option<String>,
}

impl FakeTranscriber {
    /// Returns `words` for a recording of `audio_ms`.
    pub fn new(words: Vec<Word>, audio_ms: i64) -> Self {
        Self {
            words: Mutex::new(words),
            audio_ms,
            fail_with: None,
        }
    }
}

#[async_trait]
impl Transcriber for FakeTranscriber {
    async fn transcribe(
        &self,
        _scratch: &ScratchDir,
        audio: &Path,
        _hints: &AsrHints,
    ) -> Result<RawTranscript, AsrError> {
        if let Some(message) = &self.fail_with {
            return Err(AsrError::Failed(message.clone()));
        }
        if !audio.exists() {
            return Err(AsrError::Output(format!("{} is missing", audio.display())));
        }
        Ok(RawTranscript {
            model: "fake-asr".into(),
            audio_ms: self.audio_ms,
            words: self.words.lock().map(|w| w.clone()).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// whisper-cli's full JSON, with invented words.
    const FIXTURE: &str = r#"{
      "systeminfo": "fixture", "model": {"type": "large"}, "params": {"language": "en"},
      "result": {"language": "en"},
      "transcription": [
        {"offsets": {"from": 0, "to": 220}, "text": "",
         "tokens": [{"text": "[_BEG_]", "p": 0.3, "offsets": {"from": 0, "to": 0}}]},
        {"offsets": {"from": 220, "to": 510}, "text": " Sitting",
         "tokens": [{"text": " Sit", "p": 0.9}, {"text": "ting", "p": 0.7}]},
        {"offsets": {"from": 510, "to": 640}, "text": " is",
         "tokens": [{"text": " is", "p": 0.95}]},
        {"offsets": {"from": 640, "to": 900}, "text": " enough.",
         "tokens": [{"text": " enough", "p": 0.8}, {"text": ".", "p": 0.6},
                    {"text": "[_TT_45]", "p": 0.1}]}
      ]
    }"#;

    #[test]
    fn the_cli_json_becomes_words() {
        let words = parse_cli_json(FIXTURE).unwrap_or_else(|e| panic!("{e}"));
        let texts: Vec<_> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Sitting", "is", "enough."]);
        assert_eq!((words[0].start_ms, words[0].end_ms), (220, 510));
        assert!((words[0].prob - 0.8).abs() < 1e-6, "mean of its tokens");
        assert!(
            (words[2].prob - 0.7).abs() < 1e-6,
            "special tokens do not count"
        );
        assert!(parse_cli_json("not json").is_err());
    }

    #[test]
    fn model_names_and_the_prompt() {
        assert_eq!(
            model_name(Path::new("/m/ggml-large-v3.bin")),
            "whisper-large-v3"
        );
        assert_eq!(
            model_name(Path::new("ggml-distil-large-v3.bin")),
            "whisper-distil-large-v3"
        );
        assert_eq!(
            glossary_prompt(["Dogen", "shikantaza"]),
            "Dogen, shikantaza"
        );
        let long: Vec<String> = (0..500).map(|i| format!("term{i}")).collect();
        assert!(glossary_prompt(long.iter().map(String::as_str)).len() <= 800);
    }
}
