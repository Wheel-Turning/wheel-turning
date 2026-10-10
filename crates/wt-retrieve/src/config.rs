//! `config/retrieval.toml` (Spec 05). Every key has the spec's default, so a missing file or
//! section runs with the defaults.

use std::path::Path;

use serde::Deserialize;

/// The whole file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetrievalConfig {
    /// `[retrieval]`.
    pub retrieval: SearchConfig,
    /// `[selection]`.
    pub selection: SelectionConfig,
    /// `[answer]`.
    pub answer: AnswerConfig,
}

/// `[retrieval]`: search, fusion and reranking.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    /// Full-text candidates.
    pub fts_top_k: i64,
    /// Vector candidates.
    pub vector_top_k: i64,
    /// The `k` of reciprocal rank fusion.
    pub rrf_k: f32,
    /// Fused candidates sent to the reranker.
    pub fuse_top_k: usize,
    /// `hnsw.ef_search` for the vector search.
    pub hnsw_ef_search: i64,
    /// The sigmoid of the reranker logit below which a passage is dropped.
    pub rerank_min_score: f32,
    /// Seconds before the reranker is abandoned for the fused order.
    pub rerank_timeout_s: u64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            fts_top_k: 60,
            vector_top_k: 60,
            rrf_k: 60.0,
            fuse_top_k: 40,
            hnsw_ef_search: 100,
            rerank_min_score: 0.30,
            rerank_timeout_s: 10,
        }
    }
}

/// `[selection]`: how passages are chosen from the ranked candidates.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SelectionConfig {
    /// Distinct teachers the seed round aims for.
    pub target_teachers: usize,
    /// Passages per teacher.
    pub per_teacher_cap: usize,
    /// Passages per answer.
    pub max_passages: usize,
    /// Distinct talks per answer.
    pub max_sources: usize,
    /// MMR's weight on relevance against novelty.
    pub mmr_lambda: f32,
    /// Passages of one talk closer than this count as near-duplicates.
    pub same_talk_min_gap_s: i64,
    /// Fewer passages than this in named mode gives the `NamedFew` notice.
    pub named_min_passages: usize,
    /// Passages in named mode.
    pub named_max_passages: usize,
    /// Passages per talk in named mode.
    pub named_per_talk_cap: usize,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            target_teachers: 3,
            per_teacher_cap: 3,
            max_passages: 10,
            max_sources: 8,
            mmr_lambda: 0.7,
            same_talk_min_gap_s: 60,
            named_min_passages: 2,
            named_max_passages: 8,
            named_per_talk_cap: 2,
        }
    }
}

/// `[answer]`: assembly settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnswerConfig {
    /// How far before a passage its source link starts.
    pub link_lead_in_ms: i64,
}

impl Default for AnswerConfig {
    fn default() -> Self {
        Self {
            link_lead_in_ms: 2000,
        }
    }
}

/// Why the file could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists but could not be read.
    #[error("reading {path}: {source}")]
    Read {
        /// The file.
        path: String,
        /// The cause.
        source: std::io::Error,
    },
    /// The file is not valid TOML for these settings.
    #[error("parsing {path}: {source}")]
    Parse {
        /// The file.
        path: String,
        /// The cause.
        source: toml::de::Error,
    },
}

impl RetrievalConfig {
    /// Loads the file, or the defaults when it does not exist.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.display().to_string(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_shipped_file_matches_the_spec_defaults() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/retrieval.toml");
        let c = RetrievalConfig::load(&path).unwrap();
        assert_eq!(c.retrieval.fuse_top_k, 40);
        assert_eq!(c.selection.target_teachers, 3);
        assert_eq!(c.selection.max_sources, 8);
        assert_eq!(c.answer.link_lead_in_ms, 2000);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        assert!(toml::from_str::<RetrievalConfig>("[selection]\nmax_passagez = 3").is_err());
    }
}
