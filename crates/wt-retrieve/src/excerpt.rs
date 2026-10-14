//! The reranker step of excerpt selection (Spec 05, Answer assembly, rule 8, step 2): when the
//! model gives no usable run, the reranker scores the sentences near the selected passage, then
//! spans grown from the best of them, and the shortest span scoring close to the best is kept.

use wt_core::excerpt::{Limits, Window};
use wt_ml::{MlError, Reranker};

/// Why the reranker step gave no span.
#[derive(Debug, thiserror::Error)]
pub enum RerankMiss {
    /// No run of whole sentences touches the passage within the limits.
    #[error("no candidate span")]
    NoCandidates,
    /// The reranker failed or returned the wrong number of scores.
    #[error("reranker: {0}")]
    Reranker(#[from] MlError),
    /// The scores did not match the texts.
    #[error("the reranker returned {got} scores for {sent} texts")]
    Count {
        /// Returned.
        got: usize,
        /// Sent.
        sent: usize,
    },
}

/// The logistic function, which turns a reranker logit into a score between 0 and 1.
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Reranker scores for the texts, refusing an answer with the wrong number of scores.
async fn scores(
    reranker: &dyn Reranker,
    query: &str,
    texts: Vec<String>,
) -> Result<Vec<f32>, RerankMiss> {
    let logits = reranker.score(query, &texts).await?;
    if logits.len() != texts.len() {
        return Err(RerankMiss::Count {
            got: logits.len(),
            sent: texts.len(),
        });
    }
    Ok(logits.into_iter().map(sigmoid).collect())
}

/// Chooses a span by the reranker. Returns window-wide word offsets.
pub async fn choose(
    reranker: &dyn Reranker,
    query: &str,
    window: &Window,
    limits: Limits,
    max_spans: usize,
    keep_ratio: f32,
) -> Result<(usize, usize), RerankMiss> {
    let sentences = window.sentences();
    let near = window.sentences_near_selected();
    if near.is_empty() {
        return Err(RerankMiss::NoCandidates);
    }
    let texts = near
        .iter()
        .map(|&i| window.text(sentences[i].0, sentences[i].1))
        .collect();
    let scored: Vec<(usize, f32)> = near
        .iter()
        .copied()
        .zip(scores(reranker, query, texts).await?)
        .collect();
    let runs = window.candidate_runs(&scored, limits, max_spans);
    let spans: Vec<(usize, usize)> = runs
        .iter()
        .filter_map(|&(f, l)| window.check_run(f, l, limits))
        .collect();
    if spans.is_empty() {
        return Err(RerankMiss::NoCandidates);
    }
    let texts = spans.iter().map(|&(s, e)| window.text(s, e)).collect();
    let span_scores = scores(reranker, query, texts).await?;
    let best = span_scores.iter().copied().fold(f32::MIN, f32::max);
    spans
        .iter()
        .zip(&span_scores)
        .filter(|(_, s)| **s >= keep_ratio * best)
        .min_by(|(a, sa), (b, sb)| (a.1 - a.0).cmp(&(b.1 - b.0)).then(sb.total_cmp(sa)))
        .map(|(span, _)| *span)
        .ok_or(RerankMiss::NoCandidates)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use async_trait::async_trait;
    use uuid::Uuid;
    use wt_core::excerpt::WindowPassage;
    use wt_core::ids::PassageId;

    /// Scores a text by how many times it says "breath", and fails when told to.
    struct Counting {
        fail: bool,
    }

    #[async_trait]
    impl Reranker for Counting {
        fn model_id(&self) -> &str {
            "counting"
        }
        async fn score(&self, _q: &str, passages: &[String]) -> Result<Vec<f32>, MlError> {
            if self.fail {
                return Err(MlError::Output("down".into()));
            }
            Ok(passages
                .iter()
                .map(|p| p.matches("breath").count() as f32 * 4.0 - 2.0)
                .collect())
        }
    }

    fn window() -> Window {
        let p = |n: u128, text: &str, starts: &[i32]| WindowPassage {
            id: PassageId::from_uuid(Uuid::from_u128(n)),
            text: text.into(),
            ordinal: 0,
            start_ms: 0,
            sentence_starts: starts.to_vec(),
            sentence_start_ms: vec![0; starts.len()],
        };
        Window {
            passages: vec![
                p(1, "We sit on the cushion. Then the breath", &[0, 5]),
                p(
                    2,
                    "comes and goes by itself. We do nothing. Rain falls.",
                    &[5, 8],
                ),
            ],
            selected: 1,
        }
    }

    #[tokio::test]
    async fn the_shortest_span_close_to_the_best_wins() {
        let w = window();
        let l = Limits {
            min_words: 3,
            max_words: 250,
        };
        let (s, e) = choose(&Counting { fail: false }, "breath", &w, l, 8, 0.9)
            .await
            .unwrap();
        assert_eq!(w.text(s, e), "Then the breath comes and goes by itself.");
    }

    #[tokio::test]
    async fn a_failing_reranker_gives_no_span() {
        let got = choose(
            &Counting { fail: true },
            "breath",
            &window(),
            Limits::default(),
            8,
            0.9,
        )
        .await;
        assert!(matches!(got, Err(RerankMiss::Reranker(_))));
    }
}
