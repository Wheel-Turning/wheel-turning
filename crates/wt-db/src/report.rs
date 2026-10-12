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
