//! The reports `wt report` prints (Spec 06, Logging, rule 6). Each reads only what the analyst
//! role may read: the aggregate views and `flagged_queries()`. Run them on a pool connected as
//! `wt_analyst`, which cannot read an unflagged query's text at all.

use sqlx::PgPool;
use sqlx::types::chrono::{DateTime, Utc};

use crate::DbError;

/// Whether this connection can read `query_log` directly. `wt report` refuses to run when it
/// can, so a report is never run as a role that could read unflagged questions.
pub async fn reads_query_log(pool: &PgPool) -> Result<bool, DbError> {
    Ok(
        sqlx::query_scalar!(r#"select has_table_privilege('query_log', 'select') as "reads!""#)
            .fetch_one(pool)
            .await?,
    )
}

/// One row of `v_answer_stats`: answers per day, mode and teacher count.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AnswerStats {
    /// The day.
    pub day: DateTime<Utc>,
    /// `standard` or `named_teacher`.
    pub mode: String,
    /// Teachers in the answer.
    pub teacher_count: i32,
    /// Answers.
    pub answers: i64,
    /// Mean latency.
    pub avg_latency_ms: Option<i32>,
    /// Answers with the thin-coverage notice.
    pub thin_answers: i64,
}

/// Answers per day since `since`.
pub async fn answers(pool: &PgPool, since: DateTime<Utc>) -> Result<Vec<AnswerStats>, DbError> {
    let rows = sqlx::query!(
        r#"select day as "day!", mode as "mode!", teacher_count as "teacher_count!",
                  answers as "answers!", avg_latency_ms, thin_answers as "thin_answers!"
           from v_answer_stats where day >= date_trunc('day', $1::timestamptz)
           order by day, mode, teacher_count"#,
        since
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| AnswerStats {
            day: r.day,
            mode: r.mode,
            teacher_count: r.teacher_count,
            answers: r.answers,
            avg_latency_ms: r.avg_latency_ms,
            thin_answers: r.thin_answers,
        })
        .collect())
}

/// One row of `v_dana_clicks`: dana events per day and kind.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DanaEvents {
    /// The day.
    pub day: DateTime<Utc>,
    /// `panel_open`, `split_computed` or `link_click`.
    pub kind: String,
    /// Events.
    pub events: i64,
}

/// Dana events per day and kind. With a small group, read link clicks as a floor test, not a
/// conversion rate (PRD, Dana mechanic).
pub async fn dana(pool: &PgPool) -> Result<Vec<DanaEvents>, DbError> {
    let rows = sqlx::query!(
        r#"select day as "day!", kind as "kind!", events as "events!"
           from v_dana_clicks order by day, kind"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DanaEvents {
            day: r.day,
            kind: r.kind,
            events: r.events,
        })
        .collect())
}

/// One row of `v_consent_coverage`: talks per state, consent status and teacher.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Coverage {
    /// The talk state.
    pub state: String,
    /// The consent status.
    pub consent_status: String,
    /// The teacher, when the talk has one.
    pub teacher: Option<String>,
    /// Talks.
    pub talks: i64,
}

/// Talks per state, consent status and teacher.
pub async fn coverage(pool: &PgPool) -> Result<Vec<Coverage>, DbError> {
    let rows = sqlx::query!(
        r#"select state::text as "state!", consent_status::text as "consent_status!", teacher,
                  talks as "talks!"
           from v_consent_coverage order by teacher nulls last, state, consent_status"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Coverage {
            state: r.state,
            consent_status: r.consent_status,
            teacher: r.teacher,
            talks: r.talks,
        })
        .collect())
}

/// A query its tester flagged, the only kind whose text may be read (Spec 06, Logging).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Flagged {
    /// When it was flagged.
    pub flagged_at: Option<DateTime<Utc>>,
    /// When it was asked.
    pub asked_at: DateTime<Utc>,
    /// `standard` or `named_teacher`.
    pub mode: String,
    /// Teachers in the answer.
    pub teacher_count: i32,
    /// The question; absent if a privacy purge removed it.
    pub query_text: Option<String>,
}

/// Every flagged query, through `flagged_queries()`.
pub async fn flagged(pool: &PgPool) -> Result<Vec<Flagged>, DbError> {
    let rows = sqlx::query!(
        r#"select flagged_at, created_at as "created_at!", mode as "mode!",
                  teacher_count as "teacher_count!", query_text
           from flagged_queries() order by flagged_at"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Flagged {
            flagged_at: r.flagged_at,
            asked_at: r.created_at,
            mode: r.mode,
            teacher_count: r.teacher_count,
            query_text: r.query_text,
        })
        .collect())
}

/// Questions by category, relevance and whether the gate was degraded, from `v_query_triage`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TriageRow {
    /// `none`, `medical`, `distress`, `out_of_scope` or `acute`.
    pub category: String,
    /// `practice`, `adjacent`, `unrelated`, or absent when the gate did not answer.
    pub relevance: Option<String>,
    /// The gate was off, failed, timed out or found the cap reached.
    pub triage_degraded: bool,
    /// Questions.
    pub questions: i64,
    /// Of those, answered: an unrelated question answered was searched anyway.
    pub answered: i64,
    /// Answers that found nothing.
    pub none_found: i64,
    /// Answers with a Not relevant vote.
    pub not_relevant: i64,
    /// Flagged questions.
    pub flagged: i64,
}

/// What each layer said, summed from `triage_tally`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LayerRow {
    /// The rules' level.
    pub rules: String,
    /// The exemplars' level, `down` or `skipped`.
    pub embedding: String,
    /// The gate's risk, or `unanswered`.
    pub risk: String,
    /// The level that risk maps to, or `unanswered`.
    pub gate_level: String,
    /// The gate's relevance, or `unanswered`.
    pub relevance: String,
    /// The gate's confidence, or `unanswered`.
    pub confidence: String,
    /// The level the question got.
    pub level: String,
    /// Questions.
    pub n: i64,
}

/// Totals and rates over the period.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct TriageTotals {
    /// Questions.
    pub questions: i64,
    /// With `triage_degraded`.
    pub triage_degraded: i64,
    /// Given `out_of_scope`.
    pub out_of_scope: i64,
    /// Given relevance `unrelated` at level none.
    pub unrelated: i64,
    /// Of those, searched anyway.
    pub unrelated_searched_anyway: i64,
    /// `triage_degraded` over questions.
    pub triage_degraded_rate: f64,
    /// `out_of_scope` over questions.
    pub out_of_scope_rate: f64,
    /// Questions shown the outside-these-talks page, over questions.
    pub unrelated_rate: f64,
    /// Answers.
    pub answered: i64,
    /// Answers that found nothing, over answers.
    pub none_found_rate: f64,
    /// Answers with a Not relevant vote, over answers.
    pub not_relevant_rate: f64,
    /// Flagged questions, over questions.
    pub flagged_rate: f64,
    /// Gate answers whose level differs from the rules'.
    pub gate_vs_rules: i64,
    /// Gate answers whose level differs from the exemplars', where the exemplars ran.
    pub gate_vs_exemplars: i64,
    /// Questions the gate raised above both other layers.
    pub gate_raised: i64,
}

/// `wt sensitive report`: counts only, never question text.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TriageReport {
    /// The start of the period.
    pub since: DateTime<Utc>,
    /// Totals and rates.
    pub totals: TriageTotals,
    /// Questions by category, relevance and degradation.
    pub by_category: Vec<TriageRow>,
    /// Layer combinations where the gate disagreed with the rules or the exemplars.
    pub disagreements: Vec<LayerRow>,
}

/// A sensitivity level's order from none to acute, or none for an unknown name.
fn rank(level: &str) -> Option<u8> {
    ["none", "medical", "distress", "out_of_scope", "acute"]
        .iter()
        .position(|l| *l == level)
        .and_then(|i| u8::try_from(i).ok())
}

/// `n` as a share of `of`, zero when `of` is.
fn rate(n: i64, of: i64) -> f64 {
    if of == 0 { 0.0 } else { n as f64 / of as f64 }
}

/// Counts since `since` from `v_query_triage` and `triage_tally`, both readable by
/// `wt_analyst`.
pub async fn triage(pool: &PgPool, since: DateTime<Utc>) -> Result<TriageReport, DbError> {
    let rows = sqlx::query!(
        r#"select sensitive_category as "category!", relevance,
                  triage_degraded as "triage_degraded!",
                  sum(questions)::bigint as "questions!", sum(answered)::bigint as "answered!",
                  sum(none_found)::bigint as "none_found!",
                  sum(not_relevant)::bigint as "not_relevant!", sum(flagged)::bigint as "flagged!"
           from v_query_triage where day >= date_trunc('day', $1::timestamptz)
           group by 1, 2, 3 order by 1, 2 nulls last, 3"#,
        since
    )
    .fetch_all(pool)
    .await?;
    let by_category: Vec<TriageRow> = rows
        .into_iter()
        .map(|r| TriageRow {
            category: r.category,
            relevance: r.relevance,
            triage_degraded: r.triage_degraded,
            questions: r.questions,
            answered: r.answered,
            none_found: r.none_found,
            not_relevant: r.not_relevant,
            flagged: r.flagged,
        })
        .collect();
    let layers = sqlx::query!(
        r#"select rules as "rules!", embedding as "embedding!", risk as "risk!",
                  gate_level as "gate_level!", relevance as "relevance!",
                  confidence as "confidence!", level as "level!", sum(n)::bigint as "n!"
           from triage_tally where day >= ($1::timestamptz)::date
           group by 1, 2, 3, 4, 5, 6, 7 order by 8 desc, 1, 2, 3, 4, 5, 6, 7"#,
        since
    )
    .fetch_all(pool)
    .await?;

    let mut t = TriageTotals::default();
    let mut not_relevant = 0;
    let mut none_found = 0;
    let mut flagged = 0;
    for r in &by_category {
        t.questions += r.questions;
        t.answered += r.answered;
        none_found += r.none_found;
        not_relevant += r.not_relevant;
        flagged += r.flagged;
        if r.triage_degraded {
            t.triage_degraded += r.questions;
        }
        if r.category == "out_of_scope" {
            t.out_of_scope += r.questions;
        }
        if r.category == "none" && r.relevance.as_deref() == Some("unrelated") {
            t.unrelated += r.questions - r.answered;
            t.unrelated_searched_anyway += r.answered;
        }
    }
    let mut disagreements = Vec::new();
    for r in layers {
        let row = LayerRow {
            rules: r.rules,
            embedding: r.embedding,
            risk: r.risk,
            gate_level: r.gate_level,
            relevance: r.relevance,
            confidence: r.confidence,
            level: r.level,
            n: r.n,
        };
        let Some(gate) = rank(&row.gate_level) else {
            continue;
        };
        let rules = rank(&row.rules);
        let embedding = rank(&row.embedding);
        let vs_rules = rules != Some(gate);
        let vs_exemplars = embedding.is_some_and(|e| e != gate);
        if vs_rules {
            t.gate_vs_rules += row.n;
        }
        if vs_exemplars {
            t.gate_vs_exemplars += row.n;
        }
        if rules.into_iter().chain(embedding).all(|l| l < gate) {
            t.gate_raised += row.n;
        }
        if vs_rules || vs_exemplars {
            disagreements.push(row);
        }
    }
    t.triage_degraded_rate = rate(t.triage_degraded, t.questions);
    t.out_of_scope_rate = rate(t.out_of_scope, t.questions);
    t.unrelated_rate = rate(t.unrelated, t.questions);
    t.none_found_rate = rate(none_found, t.answered);
    t.not_relevant_rate = rate(not_relevant, t.answered);
    t.flagged_rate = rate(flagged, t.questions);
    Ok(TriageReport {
        since,
        totals: t,
        by_category,
        disagreements,
    })
}

/// One row of `v_feedback_counts`: feedback per day, target, reason and privacy mode.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FeedbackCounts {
    /// The day.
    pub day: DateTime<Utc>,
    /// `excerpt`, `summary` or `query`.
    pub target: String,
    /// The reason, `note` or `flag_query`.
    pub kind: String,
    /// `share` or `count_only`.
    pub privacy_mode: String,
    /// Rows.
    pub rows: i64,
    /// Submissions those rows came in.
    pub submissions: i64,
}

/// Feedback counts since `since` (Spec 06, Privacy modes, `wt report feedback`).
pub async fn feedback(pool: &PgPool, since: DateTime<Utc>) -> Result<Vec<FeedbackCounts>, DbError> {
    let rows = sqlx::query!(
        r#"select day as "day!", target as "target!", kind as "kind!",
                  privacy_mode as "privacy_mode!", rows as "rows!", submissions as "submissions!"
           from v_feedback_counts where day >= date_trunc('day', $1::timestamptz)
           order by day, target, kind, privacy_mode"#,
        since
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| FeedbackCounts {
            day: r.day,
            target: r.target,
            kind: r.kind,
            privacy_mode: r.privacy_mode,
            rows: r.rows,
            submissions: r.submissions,
        })
        .collect())
}

/// A tester's note with what it was about, never with the question.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FeedbackNote {
    /// When it was sent.
    pub created_at: DateTime<Utc>,
    /// `excerpt` or `summary`.
    pub target: String,
    /// The reasons sent with it.
    pub reasons: Vec<String>,
    /// The note, with personal details removed.
    pub note: String,
}

/// Notes since `since`, through `feedback_notes()`.
pub async fn feedback_notes(
    pool: &PgPool,
    since: DateTime<Utc>,
) -> Result<Vec<FeedbackNote>, DbError> {
    let rows = sqlx::query!(
        r#"select created_at as "created_at!", target as "target!", reasons as "reasons!",
                  note as "note!"
           from feedback_notes($1)"#,
        since
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| FeedbackNote {
            created_at: r.created_at,
            target: r.target,
            reasons: r.reasons,
            note: r.note,
        })
        .collect())
}
