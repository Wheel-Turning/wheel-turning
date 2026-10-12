//! Sensitive questions (Spec 05): [`Sensitivity::classify`] decides, before retrieval, whether
//! a question needs a pointer to help. It never affects ranking.
//!
//! Three layers, and the highest level any of them returns wins:
//!
//! 1. Rules: a phrase list per level in `config/sensitive.toml`, matched on word boundaries
//!    without regard to case.
//! 2. Embedding: the query vector against the exemplar sentences in
//!    `config/sensitive_exemplars.toml`, with a cosine threshold per level.
//! 3. Model: only when the first two disagree, through [`ModelCheck`], which the web app backs
//!    with a Haiku-class call that the spend cap may refuse. It can raise the level and never
//!    lower it.
//!
//! A failed layer cannot lower the result. A down embedder leaves the rules to decide alone and
//! logs `sensitivity_degraded`. Question text is never logged.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use wt_ml::{Embedder, MlError};

/// How serious a question looks, in increasing order: the derived `Ord` is the order "the
/// highest level wins" uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Level {
    /// Nothing sensitive.
    #[default]
    None,
    /// Symptoms, medicines, doses, diagnoses or a health emergency: a banner, no summary line.
    Medical,
    /// Despair, hopelessness, grief or thoughts of death without stated intent: a banner, no
    /// summary line and no giving panel.
    Distress,
    /// Stated intent, a plan or means to harm oneself or someone else, or an emergency in
    /// progress: the full notice page, no retrieval, nothing stored but counts.
    Acute,
}

impl Level {
    /// The value stored in `query_log.sensitive_category`.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::None => "none",
            Level::Medical => "medical",
            Level::Distress => "distress",
            Level::Acute => "acute",
        }
    }

    /// Parses a stored or model-returned category; anything else is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Level::None),
            "medical" => Some(Level::Medical),
            "distress" => Some(Level::Distress),
            "acute" => Some(Level::Acute),
            _ => None,
        }
    }

    /// Whether the summary line is shown (Spec 05's table): only for `none`.
    pub fn shows_summary(self) -> bool {
        self == Level::None
    }

    /// Whether the giving panel is shown: not for `distress` or `acute`.
    pub fn shows_giving(self) -> bool {
        self <= Level::Medical
    }
}

/// `config/sensitive.toml`. The default holds no rules and flags nothing; it exists only as a
/// fallback for test sites, and the tests parse the shipped file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensitiveConfig {
    /// The fixed query behind the acute page's "Show teachings on facing difficult feelings".
    pub curated_query: String,
    /// `[thresholds]`.
    pub thresholds: Thresholds,
    /// `[model]`.
    #[serde(default)]
    pub model: ModelSettings,
    /// `[rules]`.
    pub rules: RuleLists,
}

/// `[thresholds]`: the cosine similarity to the closest exemplar at which the embedding layer
/// returns each level.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    /// For `acute`.
    pub acute_sim: f32,
    /// For `distress`.
    pub distress_sim: f32,
    /// For `medical`.
    pub medical_sim: f32,
}

/// `[model]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettings {
    /// Whether the model layer may run at all.
    pub enabled: bool,
    /// Seconds before the model call is abandoned.
    pub timeout_s: u64,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_s: 5,
        }
    }
}

/// `[rules]`: phrases per level.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleLists {
    /// Acute phrases.
    pub acute: Vec<String>,
    /// Distress phrases.
    pub distress: Vec<String>,
    /// Medical phrases.
    pub medical: Vec<String>,
}

/// `config/sensitive_exemplars.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exemplars {
    /// Whether the owner and, if possible, a clinician have reviewed the sentences.
    #[serde(default)]
    pub reviewed: bool,
    /// Acute sentences.
    pub acute: Vec<String>,
    /// Distress sentences.
    pub distress: Vec<String>,
    /// Medical sentences.
    pub medical: Vec<String>,
}

/// Why the files could not be used.
#[derive(Debug, thiserror::Error)]
pub enum SensitiveError {
    /// A file could not be read. Unlike `retrieval.toml`, these have no defaults: a server
    /// without its rules must not start.
    #[error("reading {path}: {source}")]
    Read {
        /// The file.
        path: String,
        /// The cause.
        source: std::io::Error,
    },
    /// A file is not valid TOML for these settings.
    #[error("parsing {path}: {source}")]
    Parse {
        /// The file.
        path: String,
        /// The cause.
        source: toml::de::Error,
    },
    /// A phrase or exemplar list is unusable.
    #[error("{0}")]
    Invalid(String),
    /// The exemplars could not be embedded.
    #[error("embedding the exemplars: {0}")]
    Embed(MlError),
}

fn load_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, SensitiveError> {
    let text = std::fs::read_to_string(path).map_err(|source| SensitiveError::Read {
        path: path.display().to_string(),
        source,
    })?;
    parse_toml(&text, &path.display().to_string())
}

fn parse_toml<T: serde::de::DeserializeOwned>(text: &str, path: &str) -> Result<T, SensitiveError> {
    toml::from_str(text).map_err(|source| SensitiveError::Parse {
        path: path.to_owned(),
        source,
    })
}

impl SensitiveConfig {
    /// Loads `sensitive.toml`.
    pub fn load(path: &Path) -> Result<Self, SensitiveError> {
        load_toml(path)
    }

    /// Parses the file's text.
    pub fn parse(text: &str) -> Result<Self, SensitiveError> {
        parse_toml(text, "sensitive.toml")
    }
}

impl Exemplars {
    /// Loads `sensitive_exemplars.toml`.
    pub fn load(path: &Path) -> Result<Self, SensitiveError> {
        load_toml(path)
    }

    /// Parses the file's text.
    pub fn parse(text: &str) -> Result<Self, SensitiveError> {
        parse_toml(text, "sensitive_exemplars.toml")
    }

    /// Each sentence with its level.
    pub fn sentences(&self) -> Vec<(Level, &str)> {
        let mut out = Vec::new();
        for (level, list) in [
            (Level::Acute, &self.acute),
            (Level::Distress, &self.distress),
            (Level::Medical, &self.medical),
        ] {
            out.extend(list.iter().map(|s| (level, s.as_str())));
        }
        out
    }
}

/// The model layer. The web app implements it with a Haiku-class call under the spend cap;
/// tests use fakes. Any error simply leaves the first two layers' result standing.
#[async_trait]
pub trait ModelCheck: Send + Sync {
    /// The level the model gives `question`.
    async fn level(&self, question: &str) -> Result<Level, String>;
}

/// What each layer said, for the structured log. Never holds question text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Layers {
    /// The rules.
    pub rules: Level,
    /// The embedding check; `None` when it did not run or failed.
    pub embedding: Option<Level>,
    /// The model check; `None` when it did not run or failed.
    pub model: Option<Level>,
}

/// The outcome of [`Sensitivity::classify`].
#[derive(Debug, Clone)]
pub struct Classified {
    /// The highest level any layer returned.
    pub level: Level,
    /// The embedder was down, so the rules decided alone.
    pub degraded: bool,
    /// What each layer said.
    pub layers: Layers,
    /// The question's vector, when the embedding layer ran, so retrieval can reuse it.
    pub vector: Option<Vec<f32>>,
}

/// The loaded rules and embedded exemplars.
pub struct Sensitivity {
    rules: Vec<(Level, Vec<String>)>,
    exemplars: Vec<(Level, Vec<f32>)>,
    thresholds: Thresholds,
    model: ModelSettings,
    curated_query: String,
}

impl Sensitivity {
    /// Checks both files and embeds every exemplar once. Fails on an empty list, an empty
    /// phrase, an empty curated query or an exemplar the embedder cannot embed.
    pub async fn prepare(
        config: &SensitiveConfig,
        exemplars: &Exemplars,
        embedder: &dyn Embedder,
    ) -> Result<Self, SensitiveError> {
        for (level, phrases, sentences) in [
            (Level::Acute, &config.rules.acute, &exemplars.acute),
            (Level::Distress, &config.rules.distress, &exemplars.distress),
            (Level::Medical, &config.rules.medical, &exemplars.medical),
        ] {
            if phrases.is_empty() || phrases.iter().any(|p| normalize(p).is_empty()) {
                return Err(SensitiveError::Invalid(format!(
                    "the {} rule list is empty or holds an empty phrase",
                    level.as_str()
                )));
            }
            if sentences.is_empty() {
                return Err(SensitiveError::Invalid(format!(
                    "no {} exemplars",
                    level.as_str()
                )));
            }
        }
        if config.curated_query.trim().is_empty() {
            return Err(SensitiveError::Invalid("curated_query is empty".into()));
        }
        let (levels, texts): (Vec<Level>, Vec<String>) = exemplars
            .sentences()
            .into_iter()
            .map(|(l, t)| (l, t.to_owned()))
            .unzip();
        let vectors = embedder
            .embed(&texts)
            .await
            .map_err(SensitiveError::Embed)?;
        if vectors.len() != texts.len() {
            return Err(SensitiveError::Embed(MlError::Output(
                "wrong number of exemplar vectors".into(),
            )));
        }
        if !exemplars.reviewed {
            tracing::warn!(
                "config/sensitive_exemplars.toml is not reviewed: the exemplars are placeholders"
            );
        }
        Ok(Self::from_vectors(
            config,
            levels.into_iter().zip(vectors).collect(),
        ))
    }

    /// Builds the classifier from exemplar vectors already computed, without the checks
    /// [`Sensitivity::prepare`] makes.
    pub fn from_vectors(config: &SensitiveConfig, exemplars: Vec<(Level, Vec<f32>)>) -> Self {
        let rules = [
            (Level::Acute, &config.rules.acute),
            (Level::Distress, &config.rules.distress),
            (Level::Medical, &config.rules.medical),
        ]
        .into_iter()
        .map(|(level, list)| {
            let phrases = list
                .iter()
                .map(|p| normalize(p))
                .filter(|p| !p.is_empty())
                .collect();
            (level, phrases)
        })
        .collect();
        Self {
            rules,
            exemplars,
            thresholds: config.thresholds,
            model: config.model,
            curated_query: config.curated_query.clone(),
        }
    }

    /// The fixed query behind the acute page's link.
    pub fn curated_query(&self) -> &str {
        &self.curated_query
    }

    /// Layer 1 alone: the highest level whose phrase list matches.
    pub fn rules_level(&self, question: &str) -> Level {
        let text = format!(" {} ", normalize(question));
        self.rules
            .iter()
            .filter(|(_, phrases)| phrases.iter().any(|p| text.contains(&format!(" {p} "))))
            .map(|(level, _)| *level)
            .max()
            .unwrap_or(Level::None)
    }

    /// Layer 2 alone: the highest level whose closest exemplar is at or above its threshold.
    pub fn embedding_level(&self, vector: &[f32]) -> Level {
        self.similarities(vector)
            .into_iter()
            .filter(|(level, sim)| *sim >= self.threshold(*level))
            .map(|(level, _)| level)
            .max()
            .unwrap_or(Level::None)
    }

    /// The similarity of `vector` to the closest exemplar of each level, acute first: what the
    /// thresholds are calibrated against.
    pub fn similarities(&self, vector: &[f32]) -> [(Level, f32); 3] {
        let mut out = [
            (Level::Acute, f32::NEG_INFINITY),
            (Level::Distress, f32::NEG_INFINITY),
            (Level::Medical, f32::NEG_INFINITY),
        ];
        for (level, e) in &self.exemplars {
            let sim = cosine(vector, e);
            if let Some(slot) = out.iter_mut().find(|(l, _)| l == level) {
                slot.1 = slot.1.max(sim);
            }
        }
        out
    }

    fn threshold(&self, level: Level) -> f32 {
        match level {
            Level::Acute => self.thresholds.acute_sim,
            Level::Distress => self.thresholds.distress_sim,
            Level::Medical => self.thresholds.medical_sim,
            Level::None => f32::INFINITY,
        }
    }

    /// Classifies a normalized question. Rules first; an acute match ends there, since nothing
    /// can raise it. Then the embedding check, and the model only when the two disagree.
    pub async fn classify(
        &self,
        question: &str,
        embedder: &dyn Embedder,
        model: Option<&dyn ModelCheck>,
    ) -> Classified {
        let rules = self.rules_level(question);
        let mut layers = Layers {
            rules,
            ..Layers::default()
        };
        if rules == Level::Acute {
            return Classified {
                level: rules,
                degraded: false,
                layers,
                vector: None,
            };
        }
        let vector = match embedder.embed(&[question.to_owned()]).await {
            Ok(v) => v.into_iter().next().filter(|v| !v.is_empty()),
            Err(e) => {
                tracing::warn!(error = %e, "embedding the question for the sensitivity check");
                None
            }
        };
        let Some(vector) = vector else {
            tracing::warn!(
                sensitivity_degraded = true,
                rules = rules.as_str(),
                "sensitivity_degraded: the embedder is down, so the rules decide alone"
            );
            return Classified {
                level: rules,
                degraded: true,
                layers,
                vector: None,
            };
        };
        let embedding = self.embedding_level(&vector);
        layers.embedding = Some(embedding);
        let mut level = rules.max(embedding);
        if let Some(m) = model.filter(|_| rules != embedding && self.model.enabled) {
            let limit = Duration::from_secs(self.model.timeout_s);
            match tokio::time::timeout(limit, m.level(question)).await {
                Ok(Ok(l)) => {
                    layers.model = Some(l);
                    level = level.max(l);
                }
                Ok(Err(e)) => tracing::warn!(error = %e, "the sensitivity model check failed"),
                Err(_) => tracing::warn!("the sensitivity model check timed out"),
            }
        }
        Classified {
            level,
            degraded: false,
            layers,
            vector: Some(vector),
        }
    }
}

/// Lowercases, drops apostrophes and turns every other non-alphanumeric run into one space, so
/// phrases match on word boundaries.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = true;
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2019}' | '\u{2018}' | '`') {
            continue;
        }
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_owned()
}

/// Cosine similarity; the dot product of unit vectors, normalized anyway in case one is not.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::NEG_INFINITY;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn levels_order_from_none_to_acute() {
        assert!(Level::None < Level::Medical);
        assert!(Level::Medical < Level::Distress);
        assert!(Level::Distress < Level::Acute);
        assert!(Level::Medical.shows_giving() && !Level::Distress.shows_giving());
        assert!(!Level::Medical.shows_summary() && Level::None.shows_summary());
        for l in [Level::None, Level::Medical, Level::Distress, Level::Acute] {
            assert_eq!(Level::parse(l.as_str()), Some(l));
        }
    }

    #[test]
    fn normalize_drops_apostrophes_and_punctuation() {
        assert_eq!(normalize("I CAN'T go on..."), "i cant go on");
        assert_eq!(normalize("can’t—breathe!"), "cant breathe");
        assert_eq!(normalize("  self-harm "), "self harm");
    }
}
