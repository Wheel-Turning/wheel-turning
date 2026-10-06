//! The job queue (Spec 03): idempotent enqueue, claiming with `SKIP LOCKED`, finishing inside
//! the handler's transaction, retry with exponential backoff, `dead` after `max_attempts`, and
//! the stale-lock reaper.
//!
//! A claimed [`Job`] is a lease. Every later write checks the job is still `running` under the
//! same worker and attempt number, so a worker whose lock was reaped and whose job was claimed
//! again cannot finish or fail the newer attempt.

use std::fmt;
use std::time::Duration;

use sqlx::types::chrono::{DateTime, Utc};
use sqlx::{Acquire, PgConnection};
use wt_core::ids::{JobId, TalkId};

use crate::DbError;

/// First retry delay. Each later retry doubles it.
pub const BASE_BACKOFF: Duration = Duration::from_secs(30);
/// The longest retry delay.
pub const MAX_BACKOFF: Duration = Duration::from_secs(3600);
/// `job_lock_timeout_s` default: a `running` job unrefreshed for this long is reaped.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(7200);

/// The pipeline job kinds. `audio` runs fetch, transcribe, diarize and segment on one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// Talk classification after discovery.
    Classify,
    /// Fetch, transcribe, diarize and segment, in one job so audio never leaves the machine.
    Audio,
    /// Glossary correction.
    Correct,
    /// Citation extraction.
    Extract,
    /// Citation resolution.
    Resolve,
    /// Chunking, embedding and indexing.
    Index,
}

impl JobKind {
    /// Every kind, in pipeline order.
    pub const ALL: [JobKind; 6] = [
        JobKind::Classify,
        JobKind::Audio,
        JobKind::Correct,
        JobKind::Extract,
        JobKind::Resolve,
        JobKind::Index,
    ];

    /// The value stored in `job.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Classify => "classify",
            JobKind::Audio => "audio",
            JobKind::Correct => "correct",
            JobKind::Extract => "extract",
            JobKind::Resolve => "resolve",
            JobKind::Index => "index",
        }
    }

    /// Parses a stored or command-line kind.
    pub fn parse(s: &str) -> Option<JobKind> {
        JobKind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

impl fmt::Display for JobKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Job queue errors.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// A row holds a kind this build does not know.
    #[error("job {id} has unknown kind {kind:?}")]
    UnknownKind {
        /// The job.
        id: JobId,
        /// The stored kind.
        kind: String,
    },
    /// The job is no longer this worker's attempt: it was reaped, and maybe claimed again.
    #[error("job {0} is no longer held by this worker")]
    LostLock(JobId),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for JobError {
    fn from(e: sqlx::Error) -> Self {
        JobError::Db(DbError::Sqlx(e))
    }
}

/// A claimed job: the lease a worker holds while it runs the handler.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The job row.
    pub id: JobId,
    /// What to run.
    pub kind: JobKind,
    /// The talk it works on, if any.
    pub talk_id: Option<TalkId>,
    /// Stage-specific input.
    pub payload: serde_json::Value,
    /// This attempt's number, from 1. Also the lease's fencing token.
    pub attempt: i32,
    /// Attempts allowed before the job goes `dead`.
    pub max_attempts: i32,
    /// The worker holding the lease.
    pub worker: String,
}

/// What [`fail`] did with a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Back in the queue, to run no earlier than `run_at`.
    Retry {
        /// When the next attempt may start.
        run_at: DateTime<Utc>,
    },
    /// Out of attempts. The talk keeps its stage state with the error as its reason.
    Dead,
}

/// `{kind}:{talk_id}:{input_hash}`. The input hash covers everything the stage reads, so
/// unchanged work maps to the same key and changed inputs to a new one.
pub fn idempotency_key(kind: JobKind, talk: Option<TalkId>, input_hash: &str) -> String {
    match talk {
        Some(talk) => format!("{kind}:{talk}:{input_hash}"),
        None => format!("{kind}:-:{input_hash}"),
    }
}

/// The delay before attempt `failed_attempt + 1`: 30 seconds doubled per failed attempt,
/// capped at one hour.
pub fn backoff(failed_attempt: i32) -> Duration {
    let doublings = u32::try_from(failed_attempt.saturating_sub(1)).unwrap_or(0);
    BASE_BACKOFF
        .checked_mul(2u32.saturating_pow(doublings))
        .map_or(MAX_BACKOFF, |d| d.min(MAX_BACKOFF))
}

/// Queues a job unless one with the same idempotency key exists. Returns the new job's id, or
/// `None` when the work is already queued, running or done. Run it in the handler's
/// transaction to enqueue the next stage atomically with the current one's output.
pub async fn enqueue(
    conn: &mut PgConnection,
    kind: JobKind,
    talk: Option<TalkId>,
    input_hash: &str,
    payload: &serde_json::Value,
) -> Result<Option<JobId>, JobError> {
    let id = sqlx::query_scalar!(
        "insert into job (kind, talk_id, payload, idempotency_key) values ($1, $2, $3, $4)
         on conflict (idempotency_key) do nothing
         returning id",
        kind.as_str(),
        talk.map(|t| t.as_uuid()),
        payload,
        idempotency_key(kind, talk, input_hash)
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(id.map(JobId::from))
}

/// Claims the next due job of one of `kinds` for `worker`, or `None` when nothing is due.
/// `SKIP LOCKED` lets any number of workers claim at once without taking the same job.
pub async fn claim(
    conn: &mut PgConnection,
    kinds: &[JobKind],
    worker: &str,
) -> Result<Option<Job>, JobError> {
    let kinds: Vec<String> = kinds.iter().map(|k| k.as_str().to_owned()).collect();
    let Some(row) = sqlx::query!(
        r#"with next as (
             select id from job
             where state = 'queued' and run_at <= now() and kind = any($1)
             order by run_at
             for update skip locked
             limit 1)
           update job set state = 'running', locked_by = $2, locked_at = now(),
                          attempts = attempts + 1
           from next where job.id = next.id
           returning job.id, job.kind, job.talk_id, job.payload, job.attempts, job.max_attempts"#,
        &kinds,
        worker
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let id = JobId::from(row.id);
    let kind = JobKind::parse(&row.kind).ok_or_else(|| JobError::UnknownKind {
        id,
        kind: row.kind.clone(),
    })?;
    Ok(Some(Job {
        id,
        kind,
        talk_id: row.talk_id.map(TalkId::from),
        payload: row.payload,
        attempt: row.attempts,
        max_attempts: row.max_attempts,
        worker: worker.to_owned(),
    }))
}

/// Refreshes the lease so the reaper leaves a long job alone. Long jobs such as ASR call it
/// every minute.
pub async fn heartbeat(conn: &mut PgConnection, job: &Job) -> Result<(), JobError> {
    let updated = sqlx::query!(
        "update job set locked_at = now()
         where id = $1 and state = 'running' and locked_by = $2 and attempts = $3",
        job.id.as_uuid(),
        job.worker,
        job.attempt
    )
    .execute(&mut *conn)
    .await?;
    held(updated.rows_affected(), job)
}

/// Marks the job done. Call it in the handler's transaction, with the stage's output, the
/// talk's state change and the next stage's [`enqueue`], so all of it commits or none does.
pub async fn succeed(conn: &mut PgConnection, job: &Job) -> Result<(), JobError> {
    let updated = sqlx::query!(
        "update job set state = 'succeeded', finished_at = now(), last_error = null,
                        locked_by = null, locked_at = null
         where id = $1 and state = 'running' and locked_by = $2 and attempts = $3",
        job.id.as_uuid(),
        job.worker,
        job.attempt
    )
    .execute(&mut *conn)
    .await?;
    held(updated.rows_affected(), job)
}

/// Puts a job back in the queue until `until` without counting the attempt, such as an
/// ingestion job the language-model cap refused (Spec 04): it runs on the first of the next
/// month, or when `wt llm cap set` releases it. `reason` becomes `last_error`.
pub async fn defer(
    conn: &mut PgConnection,
    job: &Job,
    until: chrono::DateTime<chrono::Utc>,
    reason: &str,
) -> Result<(), JobError> {
    sqlx::query!(
        "update job set state = 'queued', run_at = $4, last_error = $5, attempts = attempts - 1,
                        locked_by = null, locked_at = null
         where id = $1 and state = 'running' and locked_by = $2 and attempts = $3",
        job.id.as_uuid(),
        job.worker,
        job.attempt,
        until,
        reason
    )
    .execute(&mut *conn)
    .await
    .map_err(DbError::from)?;
    Ok(())
}

/// Records a failed attempt. With attempts left the job is queued again after [`backoff`];
/// otherwise it is `dead` and its talk keeps its stage state with `state_reason` set to the
/// error summary.
pub async fn fail(conn: &mut PgConnection, job: &Job, error: &str) -> Result<Failure, JobError> {
    let mut tx = conn.begin().await?;
    let outcome = if job.attempt < job.max_attempts {
        let delay = i32::try_from(backoff(job.attempt).as_secs()).unwrap_or(i32::MAX);
        let run_at = sqlx::query_scalar!(
            r#"update job set state = 'queued', run_at = now() + make_interval(secs => $4),
                              last_error = $5, locked_by = null, locked_at = null
               where id = $1 and state = 'running' and locked_by = $2 and attempts = $3
               returning run_at"#,
            job.id.as_uuid(),
            job.worker,
            job.attempt,
            f64::from(delay),
            error
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(JobError::LostLock(job.id))?;
        Failure::Retry { run_at }
    } else {
        let updated = sqlx::query!(
            "update job set state = 'dead', finished_at = now(), last_error = $4,
                            locked_by = null, locked_at = null
             where id = $1 and state = 'running' and locked_by = $2 and attempts = $3",
            job.id.as_uuid(),
            job.worker,
            job.attempt,
            error
        )
        .execute(&mut *tx)
        .await?;
        held(updated.rows_affected(), job)?;
        if let Some(talk) = job.talk_id {
            sqlx::query!(
                "update talk set state_reason = $2 where id = $1",
                talk.as_uuid(),
                dead_reason(job.kind, error)
            )
            .execute(&mut *tx)
            .await?;
        }
        Failure::Dead
    };
    tx.commit().await?;
    Ok(outcome)
}

/// Talks waiting at `cleared`, for `wt pipeline advance`.
pub async fn cleared_talks(conn: &mut PgConnection) -> Result<Vec<TalkId>, JobError> {
    let rows =
        sqlx::query_scalar!("select id from talk where state = 'cleared' order by created_at")
            .fetch_all(&mut *conn)
            .await?;
    Ok(rows.into_iter().map(TalkId::from).collect())
}

/// Requeues the talk's most recent dead job with fresh attempts and clears the talk's
/// `state_reason`, for `wt pipeline retry`. Returns the job's kind, or `None` when the talk
/// has no dead job.
pub async fn retry_dead(
    conn: &mut PgConnection,
    talk: TalkId,
) -> Result<Option<JobKind>, JobError> {
    let mut tx = conn.begin().await?;
    let kind = sqlx::query_scalar!(
        "update job set state = 'queued', attempts = 0, run_at = now(), last_error = null,
                        finished_at = null, locked_by = null, locked_at = null
         where id = (select id from job where talk_id = $1 and state = 'dead'
                     order by updated_at desc, created_at desc limit 1)
         returning kind",
        talk.as_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?;
    if kind.is_some() {
        sqlx::query!(
            "update talk set state_reason = null where id = $1",
            talk.as_uuid()
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(kind.as_deref().and_then(JobKind::parse))
}

/// Returns `running` jobs whose lock is older than `timeout` to `queued`, for a worker that
/// died or lost its connection. A stale job that has used all its attempts goes `dead`
/// instead, like a failed one, so a job that kills its worker every time cannot cycle forever
/// (decision of October 5, 2026).
pub async fn reap(conn: &mut PgConnection, timeout: Duration) -> Result<Reaped, JobError> {
    let secs = f64::from(i32::try_from(timeout.as_secs()).unwrap_or(i32::MAX));
    let mut tx = conn.begin().await?;
    let dead = sqlx::query!(
        "update job set state = 'dead', finished_at = now(), last_error = $2,
                        locked_by = null, locked_at = null
         where state = 'running' and locked_at < now() - make_interval(secs => $1)
           and attempts >= max_attempts
         returning kind, talk_id",
        secs,
        LOCK_EXPIRED
    )
    .fetch_all(&mut *tx)
    .await?;
    for row in &dead {
        if let (Some(kind), Some(talk)) = (JobKind::parse(&row.kind), row.talk_id) {
            sqlx::query!(
                "update talk set state_reason = $2 where id = $1",
                talk,
                dead_reason(kind, LOCK_EXPIRED)
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    let requeued = sqlx::query!(
        "update job set state = 'queued', locked_by = null, locked_at = null
         where state = 'running' and locked_at < now() - make_interval(secs => $1)",
        secs
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Reaped {
        requeued: requeued.rows_affected(),
        dead: u64::try_from(dead.len()).unwrap_or(u64::MAX),
    })
}

/// The `last_error` of a job the reaper retires.
pub const LOCK_EXPIRED: &str = "worker lock expired on the last attempt";

/// What [`reap`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reaped {
    /// Stale jobs with attempts left, back in the queue.
    pub requeued: u64,
    /// Stale jobs out of attempts, now `dead`.
    pub dead: u64,
}

/// The talk's `state_reason` for a dead job: the kind and the first line of the error, short
/// enough to read in a report.
fn dead_reason(kind: JobKind, error: &str) -> String {
    const MAX_CHARS: usize = 200;
    let first = error.lines().next().unwrap_or_default().trim();
    let mut summary: String = first.chars().take(MAX_CHARS).collect();
    if first.chars().count() > MAX_CHARS {
        summary.push('…');
    }
    format!("{kind} job dead: {summary}")
}

fn held(rows: u64, job: &Job) -> Result<(), JobError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(JobError::LostLock(job.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_thirty_seconds_and_caps_at_an_hour() {
        let secs: Vec<u64> = (1..=9).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(secs, [30, 60, 120, 240, 480, 960, 1920, 3600, 3600]);
        assert_eq!(backoff(i32::MAX), MAX_BACKOFF);
        assert_eq!(backoff(0), BASE_BACKOFF);
    }

    #[test]
    fn kinds_round_trip_and_unknown_kinds_are_refused() {
        for kind in JobKind::ALL {
            assert_eq!(JobKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(JobKind::parse("discover"), None);
    }

    #[test]
    fn the_key_names_kind_talk_and_input_hash() {
        let talk = TalkId::from(sqlx::types::Uuid::nil());
        assert_eq!(
            idempotency_key(JobKind::Correct, Some(talk), "abc"),
            format!("correct:{talk}:abc")
        );
        assert_eq!(
            idempotency_key(JobKind::Classify, None, "abc"),
            "classify:-:abc"
        );
    }

    #[test]
    fn a_dead_reason_keeps_the_first_line_and_stays_short() {
        assert_eq!(
            dead_reason(JobKind::Audio, "disk full\nstack trace"),
            "audio job dead: disk full"
        );
        let long = "x".repeat(500);
        assert_eq!(dead_reason(JobKind::Index, &long).chars().count(), 217);
    }
}
