//! The query path (Spec 05, steps 1 to 8) as one function, so the `/ask` route and `wt eval run`
//! answer a question the same way (Spec 07, Evaluation harness). It times each stage for the
//! evaluation run and the structured log.

use std::collections::BTreeSet;
use std::time::Instant;

use serde::Serialize;
use sqlx::PgPool;
use wt_core::ids::{AnswerId, TesterId};
use wt_db::answer::Payload;
use wt_extract::DataClass;
use wt_retrieve::{Level, Mode, NamedDetector, Notice, retrieve_embedded};
use wt_synth::{Synthesis, SynthesisInput};

use crate::Site;

/// Milliseconds per stage of one answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct StageTimings {
    /// Loading aliases and detecting a named teacher.
    pub parse_ms: u64,
    /// Embedding the query.
    pub embed_ms: u64,
    /// Full-text, vector and question searches with fusion.
    pub search_ms: u64,
    /// The reranker.
    pub rerank_ms: u64,
    /// Selection.
    pub select_ms: u64,
    /// Reading the chosen passages through `servable_passage`.
    pub load_ms: u64,
    /// Theme generation and verification; zero in named mode.
    pub synth_ms: u64,
    /// Writing the query log and answer.
    pub persist_ms: u64,
    /// The whole path.
    pub total_ms: u64,
}

/// What one question produced.
#[derive(Debug, Clone)]
pub struct Asked {
    /// The stored answer.
    pub answer_id: AnswerId,
    /// Standard or named.
    pub mode: Mode,
    /// Distinct teachers among the passages.
    pub teacher_count: usize,
    /// Passages chosen.
    pub passage_count: usize,
    /// Themes that survived the filters and verifier.
    pub theme_count: usize,
    /// Themes dropped, with reasons in the log.
    pub themes_dropped: usize,
    /// Set when synthesis failed or timed out.
    pub synthesis_failed: bool,
    /// The reranker was missing, failed or timed out.
    pub rerank_degraded: bool,
    /// The notice key stored in the payload, if any.
    pub notice: Option<&'static str>,
    /// Per-stage timings.
    pub timings: StageTimings,
}

/// Why a question could not be answered. The page shows a generic error for each.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    /// Teacher aliases could not be read.
    #[error("loading teacher aliases: {0}")]
    Aliases(wt_db::DbError),
    /// The alias matcher could not be built.
    #[error("building the alias matcher: {0}")]
    Detector(wt_retrieve::query::DetectorError),
    /// Retrieval failed.
    #[error("retrieving: {0}")]
    Retrieve(#[from] wt_retrieve::RetrieveError),
    /// The chosen passages could not be read.
    #[error("loading passages: {0}")]
    Passages(wt_db::DbError),
    /// The answer could not be stored.
    #[error("storing the answer: {0}")]
    Persist(wt_db::DbError),
}

/// How a question reached the query path after the sensitivity check (Spec 05, Sensitive
/// questions). The default is an ordinary question.
#[derive(Debug, Clone, Default)]
pub struct Framing {
    /// `none`, `medical` or `distress`; acute questions never reach the query path. Anything
    /// but `none` leaves out the summary line.
    pub level: Level,
    /// The question's vector from the check, reused for search when it fits.
    pub vector: Option<Vec<f32>>,
    /// The acute notice's fixed query, shown with the support notice on top.
    pub curated: bool,
    /// The request carried `Sec-GPC: 1`, which keeps the question as count only for a tester
    /// who never chose (Spec 06, Privacy modes).
    pub gpc: bool,
}

/// Answers a normalized, ordinary question as `tester` and stores the answer with its log row,
/// kept in the tester's privacy mode.
pub async fn answer(
    pool: &PgPool,
    site: &Site,
    tester: TesterId,
    text: &str,
) -> Result<Asked, AskError> {
    answer_framed(pool, site, tester, text, Framing::default()).await
}

/// [`answer`] for a question the sensitivity check has classified, or a request that may carry
/// `Sec-GPC: 1`.
pub async fn answer_framed(
    pool: &PgPool,
    site: &Site,
    tester: TesterId,
    text: &str,
    framing: Framing,
) -> Result<Asked, AskError> {
    let started = Instant::now();
    let mut timings = StageTimings::default();

    let stage = Instant::now();
    let aliases = wt_db::answer::aliases(pool)
        .await
        .map_err(AskError::Aliases)?;
    let alias_names: Vec<String> = aliases.iter().map(|(_, a)| a.clone()).collect();
    let detector = NamedDetector::new(aliases, &site.common_words).map_err(AskError::Detector)?;
    let parsed = detector.parse(text);
    timings.parse_ms = millis(stage);

    // A named teacher is left out of the search text, so the check's vector no longer fits.
    let vector = framing.vector.filter(|_| parsed.search_text == text);
    let found = retrieve_embedded(
        pool,
        site.embedder.as_ref(),
        vector,
        site.reranker.as_deref(),
        &parsed,
        &site.retrieval,
    )
    .await?;
    timings.embed_ms = found.timings.embed_ms;
    timings.search_ms = found.timings.search_ms;
    timings.rerank_ms = found.timings.rerank_ms;
    timings.select_ms = found.timings.select_ms;

    let stage = Instant::now();
    let ids: Vec<_> = found.chosen.iter().map(|c| c.id).collect();
    let served = wt_db::answer::passages(pool, &ids)
        .await
        .map_err(AskError::Passages)?;
    timings.load_ms = millis(stage);

    // Spec 05: synthesis runs in standard mode only, over the passages as served, and not for a
    // sensitive question, whose page omits the summary line. Any failure leaves the answer
    // without the line and nothing else.
    let stage = Instant::now();
    let synthesis = match found.mode {
        Mode::Standard if framing.level.shows_summary() && !framing.curated => {
            let input = SynthesisInput {
                query: text.to_owned(),
                passages: served.iter().map(|p| (p.id, p.text.clone())).collect(),
            };
            wt_synth::synthesize(
                site.llm.as_ref(),
                &input,
                &alias_names,
                &site.synthesis(),
                DataClass::Real,
            )
            .await
        }
        _ => Synthesis::default(),
    };
    timings.synth_ms = millis(stage);

    let sanghas = served
        .iter()
        .map(|p| p.talk.sangha_id)
        .collect::<BTreeSet<_>>()
        .len();
    let (notice, notice_count) = match found.notice {
        Some(Notice::ThinCoverage { teachers }) => (Some("thin_coverage"), Some(teachers)),
        Some(Notice::NoneFound) => (Some("none_found"), None),
        Some(Notice::NamedNone) => (Some("named_none"), None),
        Some(Notice::NamedFew { found }) => (Some("named_few"), Some(found)),
        Some(Notice::MultiNamed) => (Some("multi_named"), None),
        None => (None, None),
    };
    let (mode, named) = match found.mode {
        Mode::Standard => ("standard", None),
        Mode::NamedTeacher(t) => ("named_teacher", Some(t)),
    };
    let theme_count = synthesis.themes.len();
    let passage_count = ids.len();
    let payload = Payload {
        mode: mode.to_owned(),
        named_teacher_id: named,
        notice: notice.map(str::to_owned),
        notice_count,
        themes: synthesis.themes,
        passage_ids: ids,
        rerank_degraded: found.rerank_degraded,
        curated: framing.curated,
    };
    let small = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    let stage = Instant::now();
    let choice = wt_db::privacy::choice(pool, tester)
        .await
        .map_err(AskError::Persist)?;
    let privacy_mode = site.privacy.effective(choice.as_ref(), framing.gpc);
    // Keep mode stores the text with obvious personal details removed; count-only keeps it as
    // asked, for a day.
    let kept = if privacy_mode == wt_db::privacy::SHARE {
        crate::privacy::scrub(text)
    } else {
        text.to_owned()
    };
    let new = wt_db::answer::NewAnswer {
        tester,
        query_text: &kept,
        privacy_mode,
        text_expires_at: Some(site.privacy.expires_at(privacy_mode)),
        payload,
        teacher_count: small(found.teacher_count),
        sangha_count: small(sanghas),
        thin_coverage: matches!(found.notice, Some(Notice::ThinCoverage { .. })),
        latency_ms: i32::try_from(millis(started)).unwrap_or(i32::MAX),
        themes_dropped: small(synthesis.dropped.len()),
        sensitive_category: framing.level.as_str(),
    };
    let answer_id = wt_db::answer::persist(pool, &new)
        .await
        .map_err(AskError::Persist)?;
    timings.persist_ms = millis(stage);
    timings.total_ms = millis(started);

    tracing::info!(
        latency_ms = timings.total_ms,
        synth_ms = timings.synth_ms,
        "answer stored"
    );
    Ok(Asked {
        answer_id,
        mode: found.mode,
        teacher_count: found.teacher_count,
        passage_count,
        theme_count,
        themes_dropped: synthesis.dropped.len(),
        synthesis_failed: synthesis.failure.is_some(),
        rerank_degraded: found.rerank_degraded,
        notice,
        timings,
    })
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
