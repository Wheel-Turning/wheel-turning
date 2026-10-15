//! Hybrid search, reranking and passage selection (Spec 05, steps 1 to 5).
//!
//! [`retrieve`] takes a parsed query to a ranked, diverse selection of passage ids. It reads
//! only `servable_passage` and `servable_student_question`, whose hits stand for the teacher
//! passages that answer them, and nothing it sees carries sangha, profile, volume or donation
//! fields, so those can never move a passage (CLAUDE.md rules 3 and 4). Answer assembly joins
//! sangha details afterwards.

// Private items carry docs too, as the public ones do through `missing_docs`.
#![warn(clippy::missing_docs_in_private_items)]

pub mod config;
pub mod excerpt;
pub mod fuse;
pub mod progress;
pub mod query;
pub mod rank;
pub mod search;
pub mod select;
pub mod sensitive;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use wt_core::ids::{PassageId, TeacherId};
use wt_ml::{Embedder, MlError, Reranker};

pub use config::{ExcerptConfig, Ranker, RankingConfig, RetrievalConfig};
pub use progress::{Progress, Silent, Step};
pub use query::{Mode, NamedDetector, ParsedQuery, QueryError};
pub use select::Chosen;
pub use sensitive::triage::{Relevance, Triage, Triaged};
pub use sensitive::{Classified, Level, Sensitivity};

use select::SelectInput;

/// What retrieval found worth saying above the passages (Spec 05, Answer assembly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// Fewer teachers than the target had passages above the threshold.
    ThinCoverage {
        /// How many did.
        teachers: u8,
    },
    /// Nothing passed the threshold.
    NoneFound,
    /// The named teacher has no passage on this.
    NamedNone,
    /// The named teacher has fewer than `named_min_passages`.
    NamedFew {
        /// How many were found.
        found: u8,
    },
    /// Several teachers were named; standard mode ran.
    MultiNamed,
}

/// Milliseconds per stage, for the structured log (Spec 06, Logging, rule 2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timings {
    /// Embedding the query.
    pub embed_ms: u64,
    /// Both searches.
    pub search_ms: u64,
    /// The reranker.
    pub rerank_ms: u64,
    /// Selection.
    pub select_ms: u64,
}

/// The result of one retrieval.
#[derive(Debug, Clone)]
pub struct Retrieved {
    /// Standard or named.
    pub mode: Mode,
    /// The notice to show, if any.
    pub notice: Option<Notice>,
    /// The chosen passages, best first.
    pub chosen: Vec<Chosen>,
    /// Distinct teachers among them.
    pub teacher_count: usize,
    /// The reranker was missing, failed or timed out, so the fused order stood and no
    /// relevance threshold applied.
    pub rerank_degraded: bool,
    /// Per-stage timings.
    pub timings: Timings,
}

/// Why retrieval failed. The page shows a friendly error for any of these.
#[derive(Debug, thiserror::Error)]
pub enum RetrieveError {
    /// The database refused.
    #[error("search: {0}")]
    Db(#[from] sqlx::Error),
    /// The query could not be embedded.
    #[error("embedding the query: {0}")]
    Embed(MlError),
}

/// Runs steps 2 to 5 for a parsed query. With no reranker, or when it fails or times out, the
/// fused order stands, no threshold applies and `rerank_degraded` is set.
pub async fn retrieve(
    pool: &PgPool,
    embedder: &dyn Embedder,
    reranker: Option<&dyn Reranker>,
    query: &ParsedQuery,
    cfg: &RetrievalConfig,
) -> Result<Retrieved, RetrieveError> {
    retrieve_embedded(pool, embedder, None, reranker, query, cfg).await
}

/// [`retrieve`] with the vector of `query.search_text` when the caller already has it, as the
/// sensitivity check does for a question that names no teacher.
pub async fn retrieve_embedded(
    pool: &PgPool,
    embedder: &dyn Embedder,
    vector: Option<Vec<f32>>,
    reranker: Option<&dyn Reranker>,
    query: &ParsedQuery,
    cfg: &RetrievalConfig,
) -> Result<Retrieved, RetrieveError> {
    retrieve_reporting(pool, embedder, vector, reranker, query, cfg, &Silent).await
}

/// [`retrieve_embedded`] that tells `progress` as each stage starts, for the live progress
/// display (Spec 06). What it computes is the same.
pub async fn retrieve_reporting(
    pool: &PgPool,
    embedder: &dyn Embedder,
    vector: Option<Vec<f32>>,
    reranker: Option<&dyn Reranker>,
    query: &ParsedQuery,
    cfg: &RetrievalConfig,
    progress: &dyn Progress,
) -> Result<Retrieved, RetrieveError> {
    let named = match query.mode {
        Mode::NamedTeacher(t) => Some(t),
        Mode::Standard => None,
    };
    let mut timings = Timings::default();

    let started = Instant::now();
    let vector = match vector {
        Some(v) => v,
        None => embedder
            .embed(std::slice::from_ref(&query.search_text))
            .await
            .map_err(RetrieveError::Embed)?
            .into_iter()
            .next()
            .ok_or_else(|| RetrieveError::Embed(MlError::Output("no vector".into())))?,
    };
    timings.embed_ms = millis(started.elapsed());

    progress.step(Step::Searching);
    // The three lists are independent, so each runs on its own connection at the same time.
    // Candidates load afterwards; a passage that stopped serving in between is simply absent.
    let started = Instant::now();
    let tsquery = search::or_query(&query.search_text);
    let (fts, vec_hits, question_hits) = tokio::try_join!(
        async {
            match &tsquery {
                Some(q) => {
                    let mut conn = pool.acquire().await?;
                    search::full_text(
                        &mut conn,
                        q,
                        named,
                        cfg.retrieval.fts_top_k,
                        cfg.retrieval.fts_max_lexeme_share,
                    )
                    .await
                }
                None => Ok(Vec::new()),
            }
        },
        async {
            let mut conn = pool.acquire().await?;
            search::vector(
                &mut conn,
                &vector,
                named,
                cfg.retrieval.vector_top_k,
                cfg.retrieval.hnsw_ef_search,
            )
            .await
        },
        async {
            let mut conn = pool.acquire().await?;
            search::question_vector(&mut conn, &vector, named, cfg.retrieval.question_top_k).await
        },
    )?;
    let fused = fuse::rrf(
        &[&fts, &vec_hits, &question_hits],
        cfg.retrieval.rrf_k,
        cfg.retrieval.fuse_top_k,
    );
    let ids: Vec<PassageId> = fused.iter().map(|(id, _)| *id).collect();
    let found = search::candidates(&mut *pool.acquire().await?, &ids).await?;
    timings.search_ms = millis(started.elapsed());

    progress.step(Step::Weighing {
        candidates: found.len(),
    });
    let started = Instant::now();
    let fused_score: HashMap<PassageId, f32> = fused.iter().copied().collect();
    let (scored, rerank_degraded) =
        score(reranker, &query.search_text, &found, &fused_score, cfg).await;
    timings.rerank_ms = millis(started.elapsed());

    let started = Instant::now();
    let inputs: Vec<SelectInput> = found
        .iter()
        .zip(&scored)
        .filter_map(|(c, s)| {
            s.map(|score| SelectInput {
                id: c.id,
                teacher_id: c.teacher_id,
                talk_id: c.talk_id,
                start_ms: c.start_ms,
                score,
                embedding: c.embedding.clone(),
            })
        })
        .collect();
    progress.step(Step::Choosing {
        shortlisted: inputs.len(),
    });
    let mut chosen = match named {
        Some(t) => select::named(&only(inputs, t), &cfg.selection),
        None => select::standard(&inputs, &cfg.selection),
    };
    // Selection decides which passages; the page shows them most relevant first (owner's
    // decision of October 15, 2026). The sort is stable, so equal scores keep selection order.
    chosen.sort_by(|a, b| b.score.total_cmp(&a.score));
    timings.select_ms = millis(started.elapsed());

    let teacher_count = select::teacher_count(&chosen);
    let notice = notice(query, named, &chosen, teacher_count, cfg);
    tracing::info!(
        embed_ms = timings.embed_ms,
        search_ms = timings.search_ms,
        rerank_ms = timings.rerank_ms,
        select_ms = timings.select_ms,
        fts = fts.len(),
        vector = vec_hits.len(),
        questions = question_hits.len(),
        passages = chosen.len(),
        teachers = teacher_count,
        rerank_degraded,
        "retrieved"
    );
    Ok(Retrieved {
        mode: query.mode,
        notice,
        chosen,
        teacher_count,
        rerank_degraded,
        timings,
    })
}

/// Step 4. Returns one score per candidate, `None` for a candidate below the threshold, and
/// whether the reranker was unavailable. Without it, fused scores scaled to `[0, 1]` stand in,
/// so MMR weighs relevance against similarity on the same scale.
async fn score(
    reranker: Option<&dyn Reranker>,
    query: &str,
    found: &[search::Candidate],
    fused: &HashMap<PassageId, f32>,
    cfg: &RetrievalConfig,
) -> (Vec<Option<f32>>, bool) {
    if let Some(r) = reranker {
        let texts: Vec<String> = found.iter().map(|c| c.text.clone()).collect();
        let limit = Duration::from_secs(cfg.retrieval.rerank_timeout_s);
        match tokio::time::timeout(limit, r.score(query, &texts)).await {
            Ok(Ok(logits)) if logits.len() == found.len() => {
                let scores = logits
                    .into_iter()
                    .map(|l| {
                        let s = sigmoid(l);
                        (s >= cfg.retrieval.rerank_min_score).then_some(s)
                    })
                    .collect();
                return (scores, false);
            }
            Ok(Ok(_)) => tracing::warn!("reranker returned the wrong number of scores"),
            Ok(Err(e)) => tracing::warn!(error = %e, "reranker failed"),
            Err(_) => tracing::warn!("reranker timed out"),
        }
    }
    let top = found
        .iter()
        .filter_map(|c| fused.get(&c.id))
        .fold(0.0f32, |a, &b| a.max(b));
    let scores = found
        .iter()
        .map(|c| {
            let s = fused.get(&c.id).copied().unwrap_or(0.0);
            Some(if top > 0.0 { s / top } else { 0.0 })
        })
        .collect();
    (scores, true)
}

/// The candidates from one teacher, for a question that names them.
fn only(inputs: Vec<SelectInput>, teacher: TeacherId) -> Vec<SelectInput> {
    inputs
        .into_iter()
        .filter(|c| c.teacher_id == teacher)
        .collect()
}

/// The notice above an answer, when one applies (Spec 05): a named teacher with no or too few
/// passages, nothing found, several teachers named, or fewer teachers than the selection aims for.
fn notice(
    query: &ParsedQuery,
    named: Option<TeacherId>,
    chosen: &[Chosen],
    teachers: usize,
    cfg: &RetrievalConfig,
) -> Option<Notice> {
    let small = |n: usize| u8::try_from(n).unwrap_or(u8::MAX);
    match named {
        Some(_) if chosen.is_empty() => Some(Notice::NamedNone),
        Some(_) if chosen.len() < cfg.selection.named_min_passages => Some(Notice::NamedFew {
            found: small(chosen.len()),
        }),
        Some(_) => None,
        None if chosen.is_empty() => Some(Notice::NoneFound),
        None if query.multi_named => Some(Notice::MultiNamed),
        None if teachers < cfg.selection.target_teachers => Some(Notice::ThinCoverage {
            teachers: small(teachers),
        }),
        None => None,
    }
}

/// The logistic function, which turns a reranker logit into a score between 0 and 1.
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// A duration in whole milliseconds, for the timings log.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
