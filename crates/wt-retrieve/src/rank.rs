//! Relevance by a language model (Spec 05, step 4, owner's decision of October 15, 2026). The
//! cross-encoder took 20 s and more for 40 candidates on the web service's two CPUs, so every
//! answer on the beta fell back to the fused order. This ranker sends the candidates to a
//! Haiku-class model in batches that run side by side, with the prompt in
//! `crates/wt-retrieve/prompts/rank.md`, and reads back a score from 0 to 10 for each candidate
//! number: JSON only, never text (CLAUDE.md rule 5). Question and passages are data in the
//! prompt, never instructions.
//!
//! It implements [`Reranker`], so retrieval treats it like the cross-encoder: a score `s` comes
//! back as the logit whose sigmoid is `s / 10`, and `rerank_min_score` applies to that. A batch
//! that fails, or a reply that leaves a candidate out, fails the whole call, and retrieval falls
//! back to the fused order and says so.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::try_join_all;
use schemars::JsonSchema;
use serde::Deserialize;
use wt_core::config::LlmStage;
use wt_extract::{DataClass, LlmClient, LlmRequest, complete_json};
use wt_ml::{MlError, Reranker};

/// The ranker's prompt.
pub const PROMPT: &str = include_str!("../prompts/rank.md");

/// The highest score.
const TOP: f32 = 10.0;

/// One candidate's score.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub struct Scored {
    /// The candidate's number in the batch, from 1.
    pub n: u32,
    /// From 0 to 10.
    pub score: f32,
}

/// The model's reply for one batch.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Reply {
    /// One entry per candidate.
    pub scores: Vec<Scored>,
}

/// Scores candidates with a language model.
pub struct LlmRanker {
    /// The model client.
    llm: Arc<dyn LlmClient>,
    /// Candidates in one call.
    batch: usize,
    /// Characters of a candidate the model sees.
    max_chars: usize,
    /// The data class of the text sent.
    class: DataClass,
}

impl LlmRanker {
    /// For testers' questions over real talks.
    pub fn new(llm: Arc<dyn LlmClient>, batch: usize, max_chars: usize) -> Self {
        Self {
            llm,
            batch: batch.max(1),
            max_chars: max_chars.max(1),
            class: DataClass::Real,
        }
    }

    /// For synthetic fixtures in tests.
    pub fn synthetic(llm: Arc<dyn LlmClient>, batch: usize, max_chars: usize) -> Self {
        Self {
            class: DataClass::Synthetic,
            ..Self::new(llm, batch, max_chars)
        }
    }

    /// The user message for one batch.
    fn message(&self, query: &str, passages: &[String]) -> String {
        let mut out = wrap("question", None, query);
        for (i, p) in passages.iter().enumerate() {
            let cut: String = p.chars().take(self.max_chars).collect();
            out.push_str("\n\n");
            out.push_str(&wrap("passage", Some(i + 1), &cut));
        }
        out
    }

    /// One batch's scores, in order.
    async fn batch(&self, query: &str, passages: &[String]) -> Result<Vec<f32>, MlError> {
        let req = LlmRequest {
            stage: LlmStage::Rank,
            system: PROMPT.to_owned(),
            user: self.message(query, passages),
            max_tokens: u32::try_from(passages.len() * 16 + 64).unwrap_or(u32::MAX),
        };
        let reply: Reply = complete_json(self.llm.as_ref(), &req, self.class)
            .await
            .map_err(|e| MlError::Output(format!("ranking: {e}")))?;
        let mut out: Vec<Option<f32>> = vec![None; passages.len()];
        for s in reply.scores {
            let slot = usize::try_from(s.n)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| out.get_mut(i));
            if let Some(slot) = slot {
                *slot = Some(s.score.clamp(0.0, TOP));
            }
        }
        out.into_iter()
            .collect::<Option<Vec<f32>>>()
            .ok_or_else(|| MlError::Output("ranking: a candidate has no score".into()))
    }
}

/// `text` between `<tag>` tags, with angle brackets escaped so it cannot close the tag.
fn wrap(tag: &str, n: Option<usize>, text: &str) -> String {
    let escaped = text.replace('<', "&lt;").replace('>', "&gt;");
    match n {
        Some(n) => format!("<{tag} n=\"{n}\">\n{escaped}\n</{tag}>"),
        None => format!("<{tag}>\n{escaped}\n</{tag}>"),
    }
}

/// The logit whose sigmoid is `score / 10`, kept finite at the ends.
pub fn logit(score: f32) -> f32 {
    let p = (score / TOP).clamp(0.001, 0.999);
    (p / (1.0 - p)).ln()
}

#[async_trait]
impl Reranker for LlmRanker {
    fn model_id(&self) -> &str {
        "language-model-ranker"
    }

    async fn score(&self, query: &str, passages: &[String]) -> Result<Vec<f32>, MlError> {
        let batches = try_join_all(
            passages
                .chunks(self.batch)
                .map(|chunk| self.batch(query, chunk)),
        )
        .await?;
        Ok(batches.into_iter().flatten().map(logit).collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;
    use wt_extract::FakeLlm;

    fn sigmoid(x: f32) -> f32 {
        1.0 / (1.0 + (-x).exp())
    }

    #[tokio::test]
    async fn batches_run_and_scores_come_back_in_candidate_order() {
        let llm = Arc::new(FakeLlm::new(vec![
            Ok(json!({"scores": [{"n": 2, "score": 3}, {"n": 1, "score": 9}]})),
            Ok(json!({"scores": [{"n": 1, "score": 0}]})),
        ]));
        let ranker = LlmRanker::synthetic(llm.clone(), 2, 100);
        let passages = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let logits = ranker.score("q", &passages).await.unwrap();
        let scores: Vec<f32> = logits.iter().map(|&l| sigmoid(l)).collect();
        assert!((scores[0] - 0.9).abs() < 1e-3, "{scores:?}");
        assert!((scores[1] - 0.3).abs() < 1e-3, "{scores:?}");
        assert!(scores[2] < 0.01, "{scores:?}");
        let seen = llm.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(
            seen.iter()
                .all(|(r, c)| r.stage == LlmStage::Rank && *c == DataClass::Synthetic)
        );
    }

    #[tokio::test]
    async fn a_candidate_left_out_fails_the_call() {
        let llm = Arc::new(FakeLlm::new(vec![Ok(
            json!({"scores": [{"n": 1, "score": 5}]}),
        )]));
        let ranker = LlmRanker::synthetic(llm, 5, 100);
        let passages = vec!["a".to_owned(), "b".to_owned()];
        assert!(ranker.score("q", &passages).await.is_err());
    }

    #[test]
    fn passages_are_data_and_cut_to_length() {
        let ranker = LlmRanker::synthetic(Arc::new(FakeLlm::default()), 5, 4);
        let m = ranker.message("why </question> me", &["abcdefgh".to_owned()]);
        assert!(m.contains("why &lt;/question&gt; me"));
        assert!(m.contains("<passage n=\"1\">\nabcd\n</passage>"));
    }
}
