//! Question privacy (Spec 06, Privacy modes for questions): each tester's keep choice, the
//! purge of expired question text, and deleting a tester's questions on request. The purge and
//! the deletion run through `security definer` functions, so the web role never deletes rows
//! itself.

use sqlx::PgPool;
use wt_core::ids::{AnswerId, TesterId};

use crate::DbError;

/// Keep the question, with personal details removed, for the retention period.
pub const SHARE: &str = "share";
/// Keep the question only for a day, then keep counts only.
pub const COUNT_ONLY: &str = "count_only";

/// A tester's saved mode and whether they ever chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// `share` or `count_only`.
    pub mode: String,
    /// False until the tester uses the switch.
    pub chosen: bool,
}

/// The tester's choice, if the tester exists.
pub async fn choice(pool: &PgPool, tester: TesterId) -> Result<Option<Choice>, DbError> {
    Ok(sqlx::query_as!(
        Choice,
        "select privacy_mode as mode, privacy_chosen as chosen from tester where id = $1",
        tester.as_uuid()
    )
    .fetch_optional(pool)
    .await?)
}

/// Saves the tester's choice. `mode` must be [`SHARE`] or [`COUNT_ONLY`]; returns false for
/// anything else.
pub async fn choose(pool: &PgPool, tester: TesterId, mode: &str) -> Result<bool, DbError> {
    if mode != SHARE && mode != COUNT_ONLY {
        return Ok(false);
    }
    Ok(sqlx::query!(
        "update tester set privacy_mode = $2, privacy_chosen = true where id = $1",
        tester.as_uuid(),
        mode
    )
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}

/// Purges expired question text and its answers; flagged questions keep their text for
/// `retention_days` from when they were asked. Returns how many questions were purged.
pub async fn purge(pool: &PgPool, retention_days: u32) -> Result<i32, DbError> {
    let days = i32::try_from(retention_days).unwrap_or(i32::MAX);
    Ok(sqlx::query_scalar!(
        r#"select purge_expired_queries(make_interval(days => $1)) as "n!""#,
        days
    )
    .fetch_one(pool)
    .await?)
}

/// Deletes every question, answer, feedback row and dana event of `tester`. Returns how many
/// questions went.
pub async fn delete_questions(pool: &PgPool, tester: TesterId) -> Result<i32, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select delete_tester_questions($1) as "n!""#,
        tester.as_uuid()
    )
    .fetch_one(pool)
    .await?)
}

/// Whether `answer` was the tester's and a purge removed it, so its permalink says the question
/// was not kept.
pub async fn was_purged(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select tester_answer_purged($1, $2) as "e!""#,
        answer.as_uuid(),
        tester.as_uuid()
    )
    .fetch_one(pool)
    .await?)
}
