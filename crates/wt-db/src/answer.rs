//! Answers and what the answer, talk and transcript pages read (Specs 05 and 06). Passages are
//! read through `servable_passage` only, so a passage that stops serving disappears from every
//! page at once. An answer stores passage ids, never passage text.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::answer::{StoredSummary, StoredTheme};
use wt_core::excerpt::{ExcerptSpan, Window, WindowPassage};
use wt_core::ids::{AnswerId, PassageId, SanghaId, TalkId, TeacherId, TesterId};

use crate::DbError;

/// What `answer.payload` holds: mode, notice, counts and ordered passage ids. Takedown reads
/// `passage_ids` and `themes` too, so their shape is shared with it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Payload {
    /// `standard` or `named_teacher`.
    pub mode: String,
    /// The named teacher in named mode.
    #[serde(default)]
    pub named_teacher_id: Option<TeacherId>,
    /// The notice key, such as `thin_coverage`, when one applies.
    #[serde(default)]
    pub notice: Option<String>,
    /// For `thin_coverage` and `named_few`: the count the notice states.
    #[serde(default)]
    pub notice_count: Option<u8>,
    /// Verified synthesis themes with the passage ids that support them, for answers stored
    /// before the summary.
    #[serde(default)]
    pub themes: Vec<StoredTheme>,
    /// The verified summary (Spec 05, Summary and verifier): sentences with the passages and
    /// quotation spans they cite, never quoted text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<StoredSummary>,
    /// Chosen passages, best first.
    pub passage_ids: Vec<PassageId>,
    /// The reranker was unavailable, so no relevance threshold applied.
    #[serde(default)]
    pub rerank_degraded: bool,
    /// The fixed query behind the acute notice's "Show teachings on facing difficult
    /// feelings", which the page renders with the full support notice on top (Spec 05).
    #[serde(default)]
    pub curated: bool,
    /// Each passage's excerpt (Spec 05, Answer assembly, rule 8): the passages it covers, its
    /// word offsets and which step chose it. An answer stored before excerpts has none, and its
    /// passages show whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<ExcerptSpan>,
}

impl Payload {
    /// The stored span of `passage`, if any.
    pub fn span(&self, passage: PassageId) -> Option<&ExcerptSpan> {
        self.spans.iter().find(|s| s.passage_id == passage)
    }
}

/// One ask to persist.
#[derive(Debug, Clone)]
pub struct NewAnswer<'a> {
    /// Who asked.
    pub tester: TesterId,
    /// The question as asked.
    pub query_text: &'a str,
    /// What the page renders.
    pub payload: Payload,
    /// Distinct teachers.
    pub teacher_count: i32,
    /// Distinct sanghas.
    pub sangha_count: i32,
    /// Fewer teachers than the target.
    pub thin_coverage: bool,
    /// End-to-end time.
    pub latency_ms: i32,
    /// Themes dropped by the filters or the verifier.
    pub themes_dropped: i32,
    /// `none`, `medical` or `distress`. Acute and out-of-scope questions store no answer: see
    /// [`log_counts_only`].
    pub sensitive_category: &'a str,
    /// The triage gate's `practice`, `adjacent` or `unrelated`; `None` when it did not answer.
    pub relevance: Option<&'a str>,
    /// The gate was off, failed, timed out or found the cap reached.
    pub triage_degraded: bool,
    /// `share` or `count_only` (Spec 06, Privacy modes).
    pub privacy_mode: &'a str,
    /// When the purge removes the text and the answer.
    pub text_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Spec 06, Logging, rule 1: the query log, the answer and its passages in one transaction, so
/// no answer is shown without its log row.
pub async fn persist(pool: &PgPool, a: &NewAnswer<'_>) -> Result<AnswerId, DbError> {
    let mut tx = pool.begin().await?;
    // The index step replaces a talk's passages, so one chosen seconds ago may be gone by now
    // (October 12, 2026: answers failed on the answer_passage foreign key during a reindex).
    // Keep only the passages that still serve; the rest are not shown as withdrawn either.
    let chosen: Vec<Uuid> = a.payload.passage_ids.iter().map(|p| p.as_uuid()).collect();
    let live: std::collections::HashSet<Uuid> = sqlx::query_scalar!(
        r#"select passage_id as "passage_id!" from servable_passage where passage_id = any($1)"#,
        &chosen
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let mut payload = a.payload.clone();
    payload.passage_ids.retain(|p| live.contains(&p.as_uuid()));
    let passages = i32::try_from(payload.passage_ids.len()).unwrap_or(i32::MAX);
    // Spec 05: `theme_count` counts the summary's sentences, or the themes of an older shape.
    let shown = a
        .payload
        .summary
        .as_ref()
        .map_or(a.payload.themes.len(), |s| s.sentences.len());
    let themes = i32::try_from(shown).unwrap_or(i32::MAX);
    // The ids are made here: `returning` needs select on the table, which the web role does
    // not have on query_log or answer (Spec 02).
    let log = Uuid::new_v4();
    sqlx::query!(
        "insert into query_log (id, tester_id, query_text, mode, named_teacher_id, teacher_count,
                                sangha_count, passage_count, thin_coverage, latency_ms,
                                theme_count, themes_dropped, sensitive_category, privacy_mode,
                                text_expires_at, relevance, triage_degraded)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
        log,
        a.tester.as_uuid(),
        a.query_text,
        a.payload.mode,
        a.payload.named_teacher_id.map(|t| t.as_uuid()),
        a.teacher_count,
        a.sangha_count,
        passages,
        a.thin_coverage,
        a.latency_ms,
        themes,
        a.themes_dropped,
        a.sensitive_category,
        a.privacy_mode,
        a.text_expires_at,
        a.relevance,
        a.triage_degraded
    )
    .execute(&mut *tx)
    .await?;
    let ids: Vec<Uuid> = payload.passage_ids.iter().map(|p| p.as_uuid()).collect();
    let payload = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
    let answer = Uuid::new_v4();
    sqlx::query!(
        "insert into answer (id, query_log_id, payload) values ($1, $2, $3)",
        answer,
        log,
        payload
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "insert into answer_passage (answer_id, passage_id, rank)
         select $1, p, (r - 1)::int from unnest($2::uuid[]) with ordinality as u(p, r)",
        answer,
        &ids
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(AnswerId::from(answer))
}

/// Spec 05: an acute or out-of-scope question is logged as counts and its category only. No
/// question text, no answer row and no passages, whatever the tester's privacy mode, so the row
/// records `count_only`.
pub async fn log_counts_only(
    pool: &PgPool,
    tester: TesterId,
    category: &str,
    latency_ms: i32,
    relevance: Option<&str>,
    triage_degraded: bool,
) -> Result<(), DbError> {
    sqlx::query!(
        "insert into query_log (tester_id, query_text, mode, teacher_count, sangha_count,
                                passage_count, latency_ms, sensitive_category, privacy_mode,
                                relevance, triage_degraded)
         values ($1, null, 'standard', 0, 0, 0, $2, $3, 'count_only', $4, $5)",
        tester.as_uuid(),
        latency_ms,
        category,
        relevance,
        triage_degraded
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A question shown the outside-these-talks page (Spec 05, relevance `unrelated`): a log row
/// with no answer, its text kept as the tester's privacy mode says. "Search the talks anyway"
/// asks again and logs its own row.
pub struct Unanswered<'a> {
    /// Who asked.
    pub tester: TesterId,
    /// The question as kept for the privacy mode.
    pub query_text: &'a str,
    /// `share` or `count_only`.
    pub privacy_mode: &'a str,
    /// When the purge removes the text.
    pub text_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The time to the page.
    pub latency_ms: i32,
}

/// Logs an [`Unanswered`] question with category `none` and relevance `unrelated`.
pub async fn log_unrelated(pool: &PgPool, u: &Unanswered<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "insert into query_log (tester_id, query_text, mode, teacher_count, sangha_count,
                                passage_count, latency_ms, sensitive_category, privacy_mode,
                                text_expires_at, relevance, triage_degraded)
         values ($1, $2, 'standard', 0, 0, 0, $3, 'none', $4, $5, 'unrelated', false)",
        u.tester.as_uuid(),
        u.query_text,
        u.latency_ms,
        u.privacy_mode,
        u.text_expires_at
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A stored answer, as its owner sees it.
#[derive(Debug, Clone)]
pub struct Stored {
    /// The question as asked.
    pub query_text: String,
    /// The payload.
    pub payload: Payload,
    /// `none`, `medical` or `distress`: which notice the page shows.
    pub sensitive_category: String,
    /// When it was asked.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// The answer, if it exists and `tester` asked it. Anyone else gets `None`, which the page
/// turns into the same 404 as a missing answer.
pub async fn load(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
) -> Result<Option<Stored>, DbError> {
    let row = sqlx::query!(
        r#"select query_text, sensitive_category as "sensitive_category!",
                  payload as "payload!", created_at as "created_at!"
           from tester_answer($1, $2)"#,
        answer.as_uuid(),
        tester.as_uuid()
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| {
        Some(Stored {
            sensitive_category: r.sensitive_category,
            query_text: r.query_text.unwrap_or_default(),
            payload: serde_json::from_value(r.payload).ok()?,
            created_at: r.created_at,
        })
    }))
}

/// A serving passage with what the page shows around it.
#[derive(Debug, Clone)]
pub struct Served {
    /// The passage.
    pub id: PassageId,
    /// Verbatim text.
    pub text: String,
    /// Its position in the talk, for the transcript anchor.
    pub ordinal: i32,
    /// Start and end in the talk.
    pub start_ms: i32,
    /// End in the talk.
    pub end_ms: i32,
    /// The talk.
    pub talk: TalkCard,
}

/// What every page shows about a talk.
#[derive(Debug, Clone)]
pub struct TalkCard {
    /// The talk.
    pub id: TalkId,
    /// Its title.
    pub title: String,
    /// The recording date.
    pub date: Option<NaiveDate>,
    /// Series and part, when it belongs to one.
    pub series: Option<String>,
    /// See `series`.
    pub part: Option<i32>,
    /// `youtube`, `vimeo`, `engagewisdom` and so on.
    pub platform: String,
    /// The platform's id for the video.
    pub external_id: String,
    /// The source page.
    pub source_url: String,
    /// The teacher.
    pub teacher_id: TeacherId,
    /// Their name.
    pub teacher_name: String,
    /// Where the talk was given.
    pub sangha_id: SanghaId,
    /// Its name.
    pub sangha_name: String,
    /// The sangha shows give links only when listed and its link is verified.
    pub sangha_listed: bool,
    /// Its donation page.
    pub donation_url: Option<String>,
    /// Whether a person checked it.
    pub donation_verified: bool,
    /// `speaker_consent`, `license` or `mandate`.
    pub rights_basis: Option<String>,
    /// The license or mandate's attribution line.
    pub display_notice: Option<String>,
    /// `full` or `excerpts`: the transcript page exists only at `full`.
    pub display_level: String,
    /// The current transcript is the source's own text (`aligned` or `text`), so the page
    /// links to the source's transcript (Spec 05, Answer assembly, rule 2).
    pub transcript_from_source: bool,
    /// The source's transcript page, when it is not the talk page.
    pub transcript_url: Option<String>,
    /// The current transcript's origin: `asr`, `captions`, `reviewed`, `aligned` or `text`.
    pub transcript_origin: Option<String>,
    /// The teacher's name as References enter it, set by the owner (rule 5).
    pub citation_name: Option<String>,
    /// The teacher's References sort key.
    pub citation_sort: Option<String>,
}

macro_rules! card {
    ($r:expr) => {
        TalkCard {
            id: TalkId::from($r.talk_id),
            title: $r.talk_title,
            date: $r.talk_date,
            series: $r.series,
            part: $r.part,
            platform: $r.platform,
            external_id: $r.external_id,
            source_url: $r.source_url,
            teacher_id: TeacherId::from($r.teacher_id),
            teacher_name: $r.teacher_name,
            sangha_id: SanghaId::from($r.sangha_id),
            sangha_name: $r.sangha_name,
            sangha_listed: $r.listed,
            donation_url: $r.donation_url,
            donation_verified: $r.donation_verified,
            rights_basis: $r.rights_basis,
            display_notice: $r.display_notice,
            display_level: $r.display_level,
            transcript_from_source: $r.transcript_from_source,
            transcript_url: $r.transcript_url,
            transcript_origin: $r.transcript_origin,
            citation_name: $r.citation_name,
            citation_sort: $r.citation_sort,
        }
    };
}

/// The passages that still serve, in the order given. A passage that stopped serving is
/// absent, and the page shows a withdrawn marker in its place.
pub async fn passages(pool: &PgPool, ids: &[PassageId]) -> Result<Vec<Served>, DbError> {
    let uuids: Vec<Uuid> = ids.iter().map(|p| p.as_uuid()).collect();
    let rows = sqlx::query!(
        r#"select passage_id as "passage_id!", text as "text!", ordinal as "ordinal!",
                  start_ms as "start_ms!", end_ms as "end_ms!", talk_id as "talk_id!",
                  talk_title as "talk_title!", talk_date, series, part,
                  platform::text as "platform!", external_id as "external_id!",
                  source_url as "source_url!", teacher_id as "teacher_id!",
                  teacher_name as "teacher_name!", sangha_id as "sangha_id!",
                  sangha_name as "sangha_name!", listed as "listed!", donation_url,
                  donation_verified as "donation_verified!", rights_basis::text as rights_basis,
                  display_notice, display_level as "display_level!",
                  transcript_from_source as "transcript_from_source!", transcript_url,
                  transcript_origin, citation_name, citation_sort
           from servable_passage where passage_id = any($1)"#,
        &uuids
    )
    .fetch_all(pool)
    .await?;
    let mut out: Vec<Served> = rows
        .into_iter()
        .map(|r| Served {
            id: PassageId::from(r.passage_id),
            text: r.text,
            ordinal: r.ordinal,
            start_ms: r.start_ms,
            end_ms: r.end_ms,
            talk: card!(r),
        })
        .collect();
    out.sort_by_key(|s| ids.iter().position(|id| *id == s.id));
    Ok(out)
}

/// Each passage's excerpt window: the passage and the passages just before and after it in the
/// same talk and segment that still serve, in order (Spec 05, Answer assembly, rule 8). A
/// passage that no longer serves has no window. Returned in the order of `ids`.
pub async fn windows(pool: &PgPool, ids: &[PassageId]) -> Result<Vec<Window>, DbError> {
    let uuids: Vec<Uuid> = ids.iter().map(|p| p.as_uuid()).collect();
    let rows = sqlx::query!(
        r#"select p.passage_id as "anchor!", n.passage_id as "passage_id!", n.text as "text!",
                  n.ordinal as "ordinal!", n.start_ms as "start_ms!",
                  n.sentence_starts as "sentence_starts!", n.sentence_start_ms as "sentence_start_ms!"
           from servable_passage p
           join servable_passage n
             on n.talk_id = p.talk_id and n.segment_id = p.segment_id
            and n.ordinal between p.ordinal - 1 and p.ordinal + 1
           where p.passage_id = any($1)
           order by p.passage_id, n.ordinal"#,
        &uuids
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::new();
    for id in ids {
        let passages: Vec<WindowPassage> = rows
            .iter()
            .filter(|r| r.anchor == id.as_uuid())
            .map(|r| WindowPassage {
                id: PassageId::from(r.passage_id),
                text: r.text.clone(),
                ordinal: r.ordinal,
                start_ms: r.start_ms,
                sentence_starts: r.sentence_starts.clone(),
                sentence_start_ms: r.sentence_start_ms.clone(),
            })
            .collect();
        if let Some(selected) = passages.iter().position(|p| p.id == *id) {
            out.push(Window { passages, selected });
        }
    }
    Ok(out)
}

/// The serving rows of the given passages, for rebuilding stored excerpts. A passage that no
/// longer serves is absent.
pub async fn excerpt_rows(pool: &PgPool, ids: &[PassageId]) -> Result<Vec<WindowPassage>, DbError> {
    let uuids: Vec<Uuid> = ids.iter().map(|p| p.as_uuid()).collect();
    let rows = sqlx::query!(
        r#"select passage_id as "passage_id!", text as "text!", ordinal as "ordinal!",
                  start_ms as "start_ms!", sentence_starts as "sentence_starts!",
                  sentence_start_ms as "sentence_start_ms!"
           from servable_passage where passage_id = any($1)"#,
        &uuids
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| WindowPassage {
            id: PassageId::from(r.passage_id),
            text: r.text,
            ordinal: r.ordinal,
            start_ms: r.start_ms,
            sentence_starts: r.sentence_starts,
            sentence_start_ms: r.sentence_start_ms,
        })
        .collect())
}

/// A serving talk's card and its passages in order. `None` when the talk does not serve.
pub async fn talk(pool: &PgPool, talk: TalkId) -> Result<Option<(TalkCard, Vec<Served>)>, DbError> {
    let rows = sqlx::query!(
        r#"select passage_id as "passage_id!", text as "text!", ordinal as "ordinal!",
                  start_ms as "start_ms!", end_ms as "end_ms!", talk_id as "talk_id!",
                  talk_title as "talk_title!", talk_date, series, part,
                  platform::text as "platform!", external_id as "external_id!",
                  source_url as "source_url!", teacher_id as "teacher_id!",
                  teacher_name as "teacher_name!", sangha_id as "sangha_id!",
                  sangha_name as "sangha_name!", listed as "listed!", donation_url,
                  donation_verified as "donation_verified!", rights_basis::text as rights_basis,
                  display_notice, display_level as "display_level!",
                  transcript_from_source as "transcript_from_source!", transcript_url,
                  transcript_origin, citation_name, citation_sort
           from servable_passage where talk_id = $1 order by ordinal"#,
        talk.as_uuid()
    )
    .fetch_all(pool)
    .await?;
    let passages: Vec<Served> = rows
        .into_iter()
        .map(|r| Served {
            id: PassageId::from(r.passage_id),
            text: r.text,
            ordinal: r.ordinal,
            start_ms: r.start_ms,
            end_ms: r.end_ms,
            talk: card!(r),
        })
        .collect();
    Ok(passages
        .first()
        .map(|p| p.talk.clone())
        .map(|card| (card, passages)))
}

/// Reasons a tester can give about one excerpt (Spec 06, Logging, rule 4).
pub const EXCERPT_REASONS: [&str; 6] = [
    "good",
    "not_relevant",
    "out_of_context",
    "not_faithful",
    "wrong_details",
    "unclear",
];

/// Reasons a tester can give about the summary (Spec 06, Logging, rule 4).
pub const SUMMARY_REASONS: [&str; 5] =
    ["good", "unsupported", "misrepresents", "missing", "unclear"];

/// What a feedback submission is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackTarget {
    /// One excerpt of the answer.
    Excerpt(PassageId),
    /// The synthesis at the top of the answer.
    Summary,
}

impl FeedbackTarget {
    /// The `feedback.target` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Excerpt(_) => "excerpt",
            Self::Summary => "summary",
        }
    }

    /// The reasons this target accepts.
    pub fn reasons(self) -> &'static [&'static str] {
        match self {
            Self::Excerpt(_) => &EXCERPT_REASONS,
            Self::Summary => &SUMMARY_REASONS,
        }
    }
}

/// How a feedback submission went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackOutcome {
    /// Every row was written.
    Recorded,
    /// The answer is not the tester's, or the passage is not in it.
    NotFound,
    /// No reason and no note, or a reason the target does not take.
    Refused,
}

/// Writes one submission: a row per reason and, with a note, a `note` row, all under one
/// `submission_id`. The caller has already scrubbed the note and checked its length.
pub async fn submit_feedback(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
    target: FeedbackTarget,
    reasons: &[&str],
    note: Option<&str>,
) -> Result<FeedbackOutcome, DbError> {
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    if (reasons.is_empty() && note.is_none())
        || reasons.iter().any(|r| !target.reasons().contains(r))
    {
        return Ok(FeedbackOutcome::Refused);
    }
    let mut tx = pool.begin().await?;
    let theirs = sqlx::query_scalar!(
        r#"select tester_answer_query($1, $2) is not null as "theirs!""#,
        answer.as_uuid(),
        tester.as_uuid()
    )
    .fetch_one(&mut *tx)
    .await?;
    if !theirs {
        return Ok(FeedbackOutcome::NotFound);
    }
    let passage = match target {
        FeedbackTarget::Excerpt(p) => {
            let shown = sqlx::query_scalar!(
                r#"select answer_shows_passage($1, $2) as "shown!""#,
                answer.as_uuid(),
                p.as_uuid()
            )
            .fetch_one(&mut *tx)
            .await?;
            if !shown {
                return Ok(FeedbackOutcome::NotFound);
            }
            Some(p.as_uuid())
        }
        FeedbackTarget::Summary => None,
    };
    let mut kinds: Vec<String> = Vec::new();
    for r in reasons {
        if !kinds.iter().any(|k| k == r) {
            kinds.push((*r).to_owned());
        }
    }
    let mut notes: Vec<Option<String>> = vec![None; kinds.len()];
    if let Some(n) = note {
        kinds.push("note".to_owned());
        notes.push(Some(n.to_owned()));
    }
    sqlx::query!(
        "insert into feedback (answer_id, tester_id, target, passage_id, submission_id, kind, note)
         select $1, $2, $3, $4, s.id, k, n
         from (select gen_random_uuid() as id) s,
              unnest($5::text[], $6::text[]) as u(k, n)",
        answer.as_uuid(),
        tester.as_uuid(),
        target.as_str(),
        passage,
        &kinds,
        &notes as &[Option<String>]
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(FeedbackOutcome::Recorded)
}

/// "Flag this query" on an answer the tester asked: a `flag_query` row, and `flag_query()` sets
/// the flag on the query. Returns false when the answer is not theirs.
pub async fn flag_query(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let log = sqlx::query_scalar!(
        "select tester_answer_query($1, $2)",
        answer.as_uuid(),
        tester.as_uuid()
    )
    .fetch_one(&mut *tx)
    .await?;
    let Some(log) = log else {
        return Ok(false);
    };
    sqlx::query!(
        "insert into feedback (answer_id, tester_id, target, kind) values ($1, $2, 'query', 'flag_query')",
        answer.as_uuid(),
        tester.as_uuid()
    )
    .execute(&mut *tx)
    .await?;
    // The function's result is not needed, only its effect.
    sqlx::query("select flag_query($1)")
        .bind(log)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Every alias and display name with its teacher, for named-teacher detection.
pub async fn aliases(pool: &PgPool) -> Result<Vec<(TeacherId, String)>, DbError> {
    let rows = sqlx::query!(
        r#"select teacher_id as "teacher_id!", alias as "alias!" from teacher_alias
           union select id, display_name from teacher"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (TeacherId::from(r.teacher_id), r.alias))
        .collect())
}

/// Where a give click goes: the registry's page and its amount template, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiveTarget {
    /// The giving page.
    pub url: String,
    /// The page with an amount placeholder.
    pub template: Option<String>,
}

/// Spec 06, Logging, rule 3: the donation link for a give click, from the registry row and
/// never from the request, logged as `link_click`. `None` when the answer is not the tester's
/// or the sangha is unlisted, unverified or has no link.
pub async fn give(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
    sangha: SanghaId,
) -> Result<Option<GiveTarget>, DbError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query!(
        r#"select s.donation_url as "url!", s.donation_url_template from sangha s
           where s.id = $3 and s.listed and s.donation_verified and s.donation_url is not null
             and tester_answer_query($1, $2) is not null"#,
        answer.as_uuid(),
        tester.as_uuid(),
        sangha.as_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?;
    if row.is_some() {
        sqlx::query!(
            "insert into dana_event (answer_id, sangha_id, kind) values ($1, $2, 'link_click')",
            answer.as_uuid(),
            sangha.as_uuid()
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(row.map(|r| GiveTarget {
        url: r.url,
        template: r.donation_url_template,
    }))
}

/// A teacher's display name, for the named-teacher notices.
pub async fn teacher_name(pool: &PgPool, teacher: TeacherId) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "select display_name from teacher where id = $1",
        teacher.as_uuid()
    )
    .fetch_optional(pool)
    .await?)
}

/// A cited sangha's giving details for the dana panel, from the registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanaSangha {
    /// The sangha.
    pub id: SanghaId,
    /// Its name.
    pub name: String,
    /// Its giving page.
    pub donation_url: Option<String>,
    /// The page with an amount placeholder.
    pub donation_url_template: Option<String>,
    /// A person checked the page.
    pub donation_verified: bool,
    /// False when the sangha asked for attribution only.
    pub listed: bool,
    /// Why the link is a contact page, missing or unverified.
    pub donation_note: Option<String>,
}

/// The registry rows of the given sanghas, by name.
pub async fn dana_sanghas(pool: &PgPool, ids: &[SanghaId]) -> Result<Vec<DanaSangha>, DbError> {
    let uuids: Vec<Uuid> = ids.iter().map(|s| s.as_uuid()).collect();
    let rows = sqlx::query!(
        "select id, name, donation_url, donation_url_template, donation_verified, listed,
                donation_note
         from sangha where id = any($1) order by name",
        &uuids
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DanaSangha {
            id: SanghaId::from(r.id),
            name: r.name,
            donation_url: r.donation_url,
            donation_url_template: r.donation_url_template,
            donation_verified: r.donation_verified,
            listed: r.listed,
            donation_note: r.donation_note,
        })
        .collect())
}

/// What the dana panel logs (Spec 06, Logging, rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DanaEvent {
    /// The donor started using the panel.
    PanelOpen,
    /// A split was shown, with the gift total entered.
    SplitComputed {
        /// The gift total in cents.
        total_cents: i32,
    },
}

/// Logs a panel event on an answer the tester asked. Returns false, logging nothing, when the
/// answer is not theirs.
pub async fn dana_event(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
    event: DanaEvent,
) -> Result<bool, DbError> {
    let (kind, total) = match event {
        DanaEvent::PanelOpen => ("panel_open", None),
        DanaEvent::SplitComputed { total_cents } => ("split_computed", Some(total_cents)),
    };
    let done = sqlx::query!(
        "insert into dana_event (answer_id, kind, total_cents)
         select $1, $3, $4 where tester_answer_query($1, $2) is not null",
        answer.as_uuid(),
        tester.as_uuid(),
        kind,
        total
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The site line's give click: logged as `link_click` with no sangha when the answer is the
/// tester's. Returns whether it was logged.
pub async fn give_site(pool: &PgPool, answer: AnswerId, tester: TesterId) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "insert into dana_event (answer_id, sangha_id, kind)
         select $1, null, 'link_click' where tester_answer_query($1, $2) is not null",
        answer.as_uuid(),
        tester.as_uuid()
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A resolved or probable citation in a serving passage, with what its References entry needs
/// (Spec 05, Answer assembly, rules 4 and 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedWork {
    /// The citing passage.
    pub passage_id: PassageId,
    /// Where the citing words start in the passage text, in characters.
    pub span_start: Option<i32>,
    /// Where they end.
    pub span_end: Option<i32>,
    /// `resolved` or `probable`.
    pub confidence: String,
    /// The work.
    pub work_id: Uuid,
    /// Its title.
    pub work_title: String,
    /// Its author as stored.
    pub author: Option<String>,
    /// Its translator as stored.
    pub translator: Option<String>,
    /// The canonical locator; `*` for the work as a whole.
    pub locator: String,
    /// The locator as displayed, such as "Genjokoan, paragraph 5".
    pub label: String,
    /// The locator's link-out page, else the work's.
    pub link_out_url: Option<String>,
}

/// The resolved and probable citations in the given passages, read through
/// `servable_citation`, in passage order and then by where they start.
pub async fn cited_works(pool: &PgPool, ids: &[PassageId]) -> Result<Vec<CitedWork>, DbError> {
    let uuids: Vec<Uuid> = ids.iter().map(|p| p.as_uuid()).collect();
    let rows = sqlx::query!(
        r#"select c.passage_id as "passage_id!", c.span_start, c.span_end,
                  c.confidence::text as "confidence!", w.id as "work_id!",
                  w.title as "work_title!", w.author, w.translator, c.locator as "locator!",
                  c.label as "label!", coalesce(c.locator_url, w.link_out_url) as link_out_url
           from servable_citation c
           join work w on w.id = c.work_id
           where c.passage_id = any($1) and c.confidence::text in ('resolved', 'probable')
           order by c.passage_id, c.span_start nulls last, c.citation_id"#,
        &uuids
    )
    .fetch_all(pool)
    .await?;
    let mut out: Vec<CitedWork> = rows
        .into_iter()
        .map(|r| CitedWork {
            passage_id: PassageId::from(r.passage_id),
            span_start: r.span_start,
            span_end: r.span_end,
            confidence: r.confidence,
            work_id: r.work_id,
            work_title: r.work_title,
            author: r.author,
            translator: r.translator,
            locator: r.locator,
            label: r.label,
            link_out_url: r.link_out_url,
        })
        .collect();
    out.sort_by_key(|c| ids.iter().position(|id| *id == c.passage_id));
    Ok(out)
}
