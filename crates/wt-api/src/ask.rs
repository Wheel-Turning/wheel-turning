//! The query path (Spec 05, steps 1 to 8) as one function, so the `/ask` route and `wt eval run`
//! answer a question the same way (Spec 07, Evaluation harness). It times each stage for the
//! evaluation run and the structured log.

use std::collections::BTreeSet;
use std::time::Instant;

use serde::Serialize;
use sqlx::PgPool;
use tokio::task::JoinHandle;
use wt_core::answer::SummaryStatus;
use wt_core::ids::{AnswerId, TesterId};
use wt_db::answer::Payload;
use wt_extract::DataClass;
use wt_retrieve::{Level, Mode, NamedDetector, Notice, Relevance, Triaged, retrieve_reporting};
use wt_synth::{SummaryExcerpt, SummaryInput, Synthesis};

use crate::Site;
use crate::progress::{Count, Sink, Stage};

/// Milliseconds per stage of one answer. A stage a run's build did not have reads as zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(default)]
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
    /// Reading each passage's window and cutting its excerpt (Spec 05, Answer assembly,
    /// rule 8).
    pub excerpt_ms: u64,
    /// Summary generation and verification; zero in named mode.
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
    /// Summary sentences that survived the filters and verifier.
    pub theme_count: usize,
    /// Summary sentences dropped, with reasons in the log.
    pub themes_dropped: usize,
    /// Set when the summary failed or timed out.
    pub synthesis_failed: bool,
    /// How the summary settled, or `pending` while a deferred one is written (Spec 11, PB2.4).
    pub summary_status: SummaryStatus,
    /// The reranker was missing, failed or timed out.
    pub rerank_degraded: bool,
    /// The notice key stored in the payload, if any.
    pub notice: Option<&'static str>,
    /// Per-stage timings.
    pub timings: StageTimings,
    /// The level stored, after the triage gate.
    pub level: Level,
    /// What the gate returned.
    pub triaged: Triaged,
}

/// Why the triage gate stopped a question (Spec 05, Sensitive questions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// `acute` or `out_of_scope`: counts only were stored.
    CountsOnly(Level),
    /// Relevance `unrelated` at level `none`: logged without an answer, for the
    /// outside-these-talks page.
    Unrelated,
}

/// What [`answer_framed`] did with a question.
#[derive(Debug, Clone)]
pub enum Gated {
    /// Answered and stored.
    Answered(Box<Asked>),
    /// Stopped by the gate. Any retrieval result was discarded.
    Stopped {
        /// Why.
        stop: Stop,
        /// What the gate returned.
        triaged: Triaged,
    },
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
    /// A question asked without the gate was stopped, which only a sensitive framing can do.
    #[error("the question was not answered: {0:?}")]
    Stopped(Stop),
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
    /// The tester pressed "Search the talks anyway": an `unrelated` verdict no longer stops it.
    pub anyway: bool,
    /// Store the answer before its summary is written, and write the summary in the background
    /// (owner's request of October 15, 2026: the page in under 5 seconds). The `/ask` route sets
    /// it; `wt eval run` does not, so an evaluation still scores the summary it waited for.
    pub defer_summary: bool,
}

/// Answers a normalized, ordinary question as `tester` and stores the answer with its log row,
/// kept in the tester's privacy mode.
pub async fn answer(
    pool: &PgPool,
    site: &Site,
    tester: TesterId,
    text: &str,
) -> Result<Asked, AskError> {
    match answer_framed(
        pool,
        site,
        tester,
        text,
        Framing::default(),
        None,
        &Sink::silent(),
    )
    .await?
    {
        Gated::Answered(asked) => Ok(*asked),
        Gated::Stopped { stop, .. } => Err(AskError::Stopped(stop)),
    }
}

/// Whether the gate's verdict stops the question: a level that runs no retrieval, or an
/// `unrelated` ordinary question the tester has not chosen to search anyway.
fn stop(framing: &Framing, triaged: &Triaged) -> Option<Stop> {
    let level = triaged.raise(framing.level);
    if !level.runs_retrieval() {
        return Some(Stop::CountsOnly(level));
    }
    let unrelated = triaged.relevance() == Some(Relevance::Unrelated);
    (level == Level::None && unrelated && !framing.anyway && !framing.curated)
        .then_some(Stop::Unrelated)
}

/// Where the `/ask` route sends an ordinary question that the rules and the embedding check put
/// at `level`, given the gate's verdict: `None` answers it at `triaged.raise(level)`. `wt eval
/// gate` scores routes with this, so the harness and the page cannot drift apart.
pub fn gated(level: Level, triaged: &Triaged) -> Option<Stop> {
    stop(
        &Framing {
            level,
            ..Framing::default()
        },
        triaged,
    )
}

/// [`answer`] for a question the sensitivity check has classified, or a request that may carry
/// `Sec-GPC: 1`. `gate` is the triage gate's call, started alongside embedding and search
/// (Spec 05, Sensitive questions, layer 3): its verdict raises the level before synthesis, and a
/// verdict that stops the question discards whatever retrieval found and stores counts only,
/// or for `unrelated` a log row without an answer. Each stage is reported to `progress` as it
/// starts (Spec 06, Live progress while a question runs).
pub async fn answer_framed(
    pool: &PgPool,
    site: &Site,
    tester: TesterId,
    text: &str,
    framing: Framing,
    gate: Option<JoinHandle<Triaged>>,
    progress: &Sink,
) -> Result<Gated, AskError> {
    let started = Instant::now();
    let question = Question {
        tester,
        text,
        framing: &framing,
        started,
    };
    let mut timings = StageTimings::default();

    let stage = Instant::now();
    let aliases = wt_db::answer::aliases(pool)
        .await
        .map_err(AskError::Aliases)?;
    let alias_pairs = aliases.clone();
    let detector = NamedDetector::new(aliases, &site.common_words).map_err(AskError::Detector)?;
    let parsed = detector.parse(text);
    timings.parse_ms = millis(stage);
    let scope = match parsed.mode {
        Mode::NamedTeacher(t) => Some(t),
        Mode::Standard => None,
    };
    progress.searched(pool, &site.scope_sizes, scope);

    // A named teacher is left out of the search text, so the check's vector no longer fits.
    let vector = framing
        .vector
        .clone()
        .filter(|_| parsed.search_text == text);
    let retrieval = async {
        let found = retrieve_reporting(
            pool,
            site.embedder.as_ref(),
            vector,
            site.reranker.as_deref(),
            &parsed,
            &site.retrieval,
            progress,
        )
        .await?;
        let stage = Instant::now();
        let ids: Vec<_> = found.chosen.iter().map(|c| c.id).collect();
        let served = wt_db::answer::passages(pool, &ids)
            .await
            .map_err(AskError::Passages)?;
        Ok::<_, AskError>((found, ids, served, millis(stage)))
    };
    // The gate runs alongside: whichever finishes first, its verdict is in before synthesis.
    let joined = |t: Result<Triaged, _>| t.unwrap_or_else(|_| Triaged::degraded());
    let ((found, ids, served, load_ms), triaged) = match gate {
        None => (retrieval.await?, Triaged::not_run()),
        Some(mut gate) => {
            tokio::pin!(retrieval);
            tokio::select! {
                t = &mut gate => {
                    let t = joined(t);
                    if let Some(stop) = stop(&framing, &t) {
                        return stopped(pool, site, &question, stop, t).await;
                    }
                    (retrieval.await?, t)
                }
                r = &mut retrieval => (r?, joined(gate.await)),
            }
        }
    };
    if let Some(stop) = stop(&framing, &triaged) {
        return stopped(pool, site, &question, stop, triaged).await;
    }
    let level = triaged.raise(framing.level);
    timings.embed_ms = found.timings.embed_ms;
    timings.search_ms = found.timings.search_ms;
    timings.rerank_ms = found.timings.rerank_ms;
    timings.select_ms = found.timings.select_ms;
    timings.load_ms = load_ms;

    // Spec 05, rule 8: the page shows an excerpt of each passage cut for the question.
    let stage = Instant::now();
    let windows = wt_db::answer::windows(pool, &ids)
        .await
        .map_err(AskError::Passages)?;
    let excerpts = crate::excerpt::choose(site, text, &served, &windows).await;
    timings.excerpt_ms = millis(stage);
    progress.count(Count::Selected, ids.len());

    // Spec 05: the summary runs in standard mode only, over the excerpts as the page shows
    // them, and not for a sensitive question, whose page omits it. Any failure leaves the
    // answer without the summary and nothing else.
    let stage = Instant::now();
    let wants_summary =
        matches!(found.mode, Mode::Standard) && level.shows_summary() && !framing.curated;
    let deferred = wants_summary && framing.defer_summary;
    let input = wants_summary.then(|| SummaryInput {
        query: text.to_owned(),
        excerpts: summary_excerpts(&served, &windows, &excerpts),
    });
    let synthesis = match &input {
        Some(input) if !deferred => {
            progress.stage(Stage::Writing);
            wt_synth::summarize(
                site.llm.as_ref(),
                input,
                &alias_pairs,
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
    let summary_status = match &input {
        None => SummaryStatus::NotAttempted,
        Some(_) if deferred => SummaryStatus::Pending,
        Some(_) => synthesis.status(),
    };
    let theme_count = synthesis.shown();
    let passage_count = ids.len();
    let payload = Payload {
        mode: mode.to_owned(),
        named_teacher_id: named,
        notice: notice.map(str::to_owned),
        notice_count,
        themes: Vec::new(),
        summary: synthesis.summary,
        summary_pending: deferred,
        passage_ids: ids,
        rerank_degraded: found.rerank_degraded,
        curated: framing.curated,
        spans: served
            .iter()
            .filter_map(|p| excerpts.get(&p.id).and_then(|e| e.span.clone()))
            .collect(),
    };
    let small = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    let stage = Instant::now();
    let (kept, privacy_mode) = kept(pool, site, tester, text, &framing).await?;
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
        sensitive_category: level.as_str(),
        relevance: triaged.relevance().map(Relevance::as_str),
        triage_degraded: triaged.degraded,
        summary_status,
    };
    let answer_id = wt_db::answer::persist(pool, &new)
        .await
        .map_err(AskError::Persist)?;
    timings.persist_ms = millis(stage);
    timings.total_ms = millis(started);
    if summary_status != SummaryStatus::Pending {
        settled(answer_id, summary_status);
    }
    if deferred && let Some(input) = input {
        tokio::spawn(write_summary(
            pool.clone(),
            site.llm.clone(),
            site.synthesis(),
            alias_pairs,
            input,
            answer_id,
        ));
    }

    tracing::info!(
        latency_ms = timings.total_ms,
        synth_ms = timings.synth_ms,
        "answer stored"
    );
    Ok(Gated::Answered(Box::new(Asked {
        answer_id,
        mode: found.mode,
        teacher_count: found.teacher_count,
        passage_count,
        theme_count,
        themes_dropped: synthesis.dropped.len(),
        synthesis_failed: synthesis.failure.is_some(),
        summary_status,
        rerank_degraded: found.rerank_degraded,
        notice,
        timings,
        level,
        triaged,
    })))
}

/// Logs `summary_settled` (Spec 11, PB2.4 and PB4.3): how one answer's summary settled. It
/// carries the answer id and the status, never the question.
pub fn settled(answer: AnswerId, status: SummaryStatus) {
    tracing::info!(
        event = "summary_settled",
        summary_status = status.as_str(),
        answer_id = %answer,
        "summary settled"
    );
}

/// Writes a deferred summary and stores it, or that there is none, on the answer, with how it
/// settled. A failure leaves the answer without a summary, as an inline failure does; the page
/// stops waiting.
async fn write_summary(
    pool: PgPool,
    llm: std::sync::Arc<dyn wt_extract::LlmClient>,
    settings: wt_synth::Settings,
    aliases: Vec<(wt_core::ids::TeacherId, String)>,
    input: SummaryInput,
    answer: AnswerId,
) {
    let started = Instant::now();
    let synthesis =
        wt_synth::summarize(llm.as_ref(), &input, &aliases, &settings, DataClass::Real).await;
    let status = synthesis.status();
    let stored = wt_db::answer::set_summary(
        &pool,
        answer,
        synthesis.summary.as_ref(),
        synthesis.shown(),
        synthesis.dropped.len(),
        status,
    )
    .await;
    match stored {
        Ok(true) => {
            tracing::info!(
                synth_ms = millis(started),
                shown = synthesis.shown(),
                failed = synthesis.failure.is_some(),
                "summary stored"
            );
            settled(answer, status);
        }
        // The purge already marked it lost, and counted it so.
        Ok(false) => tracing::warn!(
            synth_ms = millis(started),
            summary_status = status.as_str(),
            "a deferred summary settled after it was marked lost"
        ),
        Err(e) => tracing::error!(error = %e, "storing a deferred summary"),
    }
}

/// Each served passage's excerpt as the summary sees it: the window it was cut from and its
/// words there, with the teacher and the talk's year. A passage without a window, or whose
/// span no longer fits it, is offered whole.
fn summary_excerpts(
    served: &[wt_db::answer::Served],
    windows: &[wt_core::excerpt::Window],
    excerpts: &std::collections::BTreeMap<wt_core::ids::PassageId, wt_core::excerpt::Excerpt>,
) -> Vec<SummaryExcerpt> {
    use chrono::Datelike;
    use wt_core::excerpt::{Window, WindowPassage};
    served
        .iter()
        .map(|p| {
            let found = windows
                .iter()
                .find(|w| w.passages.get(w.selected).is_some_and(|s| s.id == p.id));
            let span = excerpts.get(&p.id).and_then(|e| e.span.as_ref());
            let (window, range) = match (found, span) {
                (Some(w), Some(span)) => match w.locate(span) {
                    Some(range) => (w.clone(), range),
                    None => (w.clone(), w.selected_range()),
                },
                (Some(w), None) => (w.clone(), w.selected_range()),
                (None, _) => {
                    let w = Window::alone(WindowPassage {
                        id: p.id,
                        text: p.text.clone(),
                        ordinal: p.ordinal,
                        start_ms: p.start_ms,
                        sentence_starts: Vec::new(),
                        sentence_start_ms: Vec::new(),
                    });
                    let range = (0, w.len());
                    (w, range)
                }
            };
            SummaryExcerpt {
                passage_id: p.id,
                teacher_id: p.talk.teacher_id,
                teacher: p.talk.teacher_name.clone(),
                year: p.talk.date.map(|d| d.year()),
                window,
                range,
            }
        })
        .collect()
}

/// The question's text as the tester's privacy mode keeps it, and that mode.
async fn kept(
    pool: &PgPool,
    site: &Site,
    tester: TesterId,
    text: &str,
    framing: &Framing,
) -> Result<(String, &'static str), AskError> {
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
    Ok((kept, privacy_mode))
}

/// One question in [`answer_framed`]: who asked it, its words, how it was framed and when it
/// started, for the log row of a question the gate stops.
#[derive(Debug, Clone, Copy)]
struct Question<'a> {
    /// The tester who asked.
    tester: TesterId,
    /// The question as typed.
    text: &'a str,
    /// How it reached the query path.
    framing: &'a Framing,
    /// When the question started, for its latency.
    started: Instant,
}

/// Logs a question the gate stopped: counts only for `acute` and `out_of_scope`, and for
/// `unrelated` a row without an answer, kept as the privacy mode says. A logging failure is
/// reported and the page shows anyway, since the notice matters more than the count.
async fn stopped(
    pool: &PgPool,
    site: &Site,
    question: &Question<'_>,
    stop: Stop,
    triaged: Triaged,
) -> Result<Gated, AskError> {
    let Question {
        tester,
        text,
        framing,
        started,
    } = *question;
    let latency_ms = i32::try_from(millis(started)).unwrap_or(i32::MAX);
    let logged = match stop {
        Stop::CountsOnly(level) => {
            wt_db::answer::log_counts_only(
                pool,
                tester,
                level.as_str(),
                latency_ms,
                triaged.relevance().map(Relevance::as_str),
                triaged.degraded,
            )
            .await
        }
        Stop::Unrelated => {
            let (kept, privacy_mode) = kept(pool, site, tester, text, framing).await?;
            wt_db::answer::log_unrelated(
                pool,
                &wt_db::answer::Unanswered {
                    tester,
                    query_text: &kept,
                    privacy_mode,
                    text_expires_at: Some(site.privacy.expires_at(privacy_mode)),
                    latency_ms,
                },
            )
            .await
        }
    };
    if let Err(e) = logged {
        tracing::error!(error = %e, ?stop, "logging a question the triage gate stopped");
    }
    Ok(Gated::Stopped { stop, triaged })
}

/// Milliseconds since `since`, for the timings log.
fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
