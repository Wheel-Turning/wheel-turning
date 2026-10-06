//! Spec 03, the job queue: idempotent enqueue, concurrent claiming, retry backoff, `dead` after
//! `max_attempts`, the stale-lock reaper, and the lease that stops a reaped worker writing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use common::{ADA, state, state_reason, talk};
use sqlx::types::chrono::Utc;
use sqlx::{Acquire, PgPool};
use tokio::sync::Barrier;
use wt_core::ids::JobId;
use wt_db::job::{self, Failure, Job, JobError, JobKind, Reaped};

const NO_PAYLOAD: serde_json::Value = serde_json::Value::Null;

async fn enqueue(pool: &PgPool, kind: JobKind, input_hash: &str) -> Option<JobId> {
    let mut conn = pool.acquire().await.unwrap();
    job::enqueue(&mut conn, kind, None, input_hash, &NO_PAYLOAD)
        .await
        .unwrap()
}

async fn claim(pool: &PgPool, kinds: &[JobKind], worker: &str) -> Option<Job> {
    let mut conn = pool.acquire().await.unwrap();
    job::claim(&mut conn, kinds, worker).await.unwrap()
}

async fn job_state(pool: &PgPool, id: JobId) -> String {
    sqlx::query_scalar("select state::text from job where id = $1")
        .bind(id.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Makes a queued job due now, as if its backoff had passed.
async fn make_due(pool: &PgPool, id: JobId) {
    sqlx::query("update job set run_at = now() where id = $1")
        .bind(id.as_uuid())
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn enqueue_is_a_no_op_for_unchanged_input_and_new_for_changed_input() {
    let fx = common::start().await;
    let first = enqueue(&fx.pool, JobKind::Correct, "glossary-v1").await;
    assert!(first.is_some());
    assert_eq!(
        enqueue(&fx.pool, JobKind::Correct, "glossary-v1").await,
        None
    );
    assert_eq!(
        enqueue(&fx.pool, JobKind::Correct, "glossary-v1").await,
        None,
        "still a no-op after the job ran"
    );
    let changed = enqueue(&fx.pool, JobKind::Correct, "glossary-v2").await;
    assert!(changed.is_some() && changed != first);
    assert!(
        enqueue(&fx.pool, JobKind::Index, "glossary-v1")
            .await
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn eight_concurrent_workers_never_claim_the_same_job() {
    const JOBS: usize = 400;
    let fx = common::start().await;
    {
        let mut conn = fx.pool.acquire().await.unwrap();
        for i in 0..JOBS {
            job::enqueue(
                &mut conn,
                JobKind::Classify,
                None,
                &format!("h{i}"),
                &NO_PAYLOAD,
            )
            .await
            .unwrap();
        }
    }

    // Each worker gets its own connection, as separate processes would. All connect first and
    // start together, so the claims overlap instead of the first worker draining the queue.
    let start = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for w in 0..8 {
        let mut conn = wt_db::connect(fx.url())
            .await
            .unwrap()
            .acquire()
            .await
            .unwrap()
            .detach();
        let start = Arc::clone(&start);
        workers.push(tokio::spawn(async move {
            let mut claimed = Vec::new();
            let name = format!("worker-{w}");
            start.wait().await;
            while let Some(j) = job::claim(&mut conn, &[JobKind::Classify], &name)
                .await
                .unwrap()
            {
                job::succeed(&mut conn, &j).await.unwrap();
                claimed.push(j.id);
            }
            claimed
        }));
    }
    let mut all = Vec::new();
    let mut busy = 0;
    for w in workers {
        let claimed = w.await.unwrap();
        busy += usize::from(!claimed.is_empty());
        all.extend(claimed);
    }

    let unique: HashSet<_> = all.iter().collect();
    assert_eq!(all.len(), JOBS, "every job was claimed");
    assert_eq!(unique.len(), JOBS, "no job was claimed twice");
    assert!(busy > 1, "the work was shared, not taken by one worker");
    let attempts: Vec<i32> = sqlx::query_scalar("select distinct attempts from job")
        .fetch_all(&fx.pool)
        .await
        .unwrap();
    assert_eq!(attempts, [1], "each job ran exactly once");
}

#[tokio::test]
async fn claim_takes_only_due_jobs_of_the_named_kinds_oldest_first() {
    let fx = common::start().await;
    let audio = enqueue(&fx.pool, JobKind::Audio, "a").await.unwrap();
    let later = enqueue(&fx.pool, JobKind::Correct, "later").await.unwrap();
    let sooner = enqueue(&fx.pool, JobKind::Correct, "sooner").await.unwrap();
    sqlx::query("update job set run_at = now() + interval '1 hour' where id = $1")
        .bind(later.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query("update job set run_at = now() - interval '1 minute' where id = $1")
        .bind(sooner.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();

    let got = claim(&fx.pool, &[JobKind::Correct, JobKind::Index], "w")
        .await
        .unwrap();
    assert_eq!(got.id, sooner);
    assert_eq!(
        (got.kind, got.attempt, got.worker.as_str()),
        (JobKind::Correct, 1, "w")
    );
    assert!(
        claim(&fx.pool, &[JobKind::Correct, JobKind::Index], "w")
            .await
            .is_none(),
        "the other correct job is not due and audio is not a named kind"
    );
    assert_eq!(
        claim(&fx.pool, &[JobKind::Audio], "pc").await.unwrap().id,
        audio
    );
}

#[tokio::test]
async fn a_failed_attempt_retries_after_a_doubling_backoff() {
    let fx = common::start().await;
    let id = enqueue(&fx.pool, JobKind::Extract, "h").await.unwrap();
    let mut conn = fx.pool.acquire().await.unwrap();

    for (attempt, delay) in [(1, 30), (2, 60), (3, 120)] {
        let j = job::claim(&mut conn, &[JobKind::Extract], "w")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(j.attempt, attempt);
        let before = Utc::now();
        let Failure::Retry { run_at } = job::fail(&mut conn, &j, "model timed out").await.unwrap()
        else {
            panic!("attempt {attempt} has attempts left");
        };
        let waited = (run_at - before).num_seconds();
        assert!(
            (delay - 2..=delay + 2).contains(&waited),
            "attempt {attempt}: expected about {delay}s, got {waited}s"
        );
        assert_eq!(job_state(&fx.pool, id).await, "queued");
        assert!(
            job::claim(&mut conn, &[JobKind::Extract], "w")
                .await
                .unwrap()
                .is_none(),
            "not claimable until the backoff passes"
        );
        make_due(&fx.pool, id).await;
    }
    let (error, locked): (Option<String>, Option<String>) =
        sqlx::query_as("select last_error, locked_by from job where id = $1")
            .bind(id.as_uuid())
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(error.as_deref(), Some("model timed out"));
    assert_eq!(locked, None);
}

#[tokio::test]
async fn a_job_goes_dead_after_max_attempts_and_its_talk_keeps_its_state() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let id = {
        let mut conn = fx.pool.acquire().await.unwrap();
        job::enqueue(&mut conn, JobKind::Classify, Some(t), "h", &NO_PAYLOAD)
            .await
            .unwrap()
            .unwrap()
    };
    sqlx::query("update job set max_attempts = 3 where id = $1")
        .bind(id.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();

    let mut conn = fx.pool.acquire().await.unwrap();
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        let j = job::claim(&mut conn, &[JobKind::Classify], "w")
            .await
            .unwrap()
            .unwrap();
        outcomes.push(
            job::fail(&mut conn, &j, "parse error\ndetails")
                .await
                .unwrap(),
        );
        make_due(&fx.pool, id).await;
    }

    assert!(matches!(
        outcomes[..],
        [Failure::Retry { .. }, Failure::Retry { .. }, Failure::Dead]
    ));
    assert_eq!(job_state(&fx.pool, id).await, "dead");
    assert!(
        job::claim(&mut conn, &[JobKind::Classify], "w")
            .await
            .unwrap()
            .is_none(),
        "a dead job is never claimed"
    );
    assert_eq!(state(&fx.pool, t).await, "awaiting_consent");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("classify job dead: parse error")
    );
}

#[tokio::test]
async fn the_reaper_requeues_stale_locks_and_leaves_fresh_ones() {
    let fx = common::start().await;
    let stale = enqueue(&fx.pool, JobKind::Audio, "stale").await.unwrap();
    let fresh = enqueue(&fx.pool, JobKind::Audio, "fresh").await.unwrap();
    let mut conn = fx.pool.acquire().await.unwrap();
    let a = job::claim(&mut conn, &[JobKind::Audio], "dead-pc")
        .await
        .unwrap()
        .unwrap();
    let b = job::claim(&mut conn, &[JobKind::Audio], "live-pc")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(HashSet::from([a.id, b.id]), HashSet::from([stale, fresh]));
    let (stale_job, fresh_job) = if a.id == stale { (a, b) } else { (b, a) };

    sqlx::query("update job set locked_at = now() - interval '2 hours 1 minute' where id = $1")
        .bind(stale.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    // The live worker's heartbeat keeps its lock fresh even though it started long ago.
    sqlx::query("update job set locked_at = now() - interval '3 hours' where id = $1")
        .bind(fresh.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    job::heartbeat(&mut conn, &fresh_job).await.unwrap();

    assert_eq!(
        job::reap(&mut conn, job::DEFAULT_LOCK_TIMEOUT)
            .await
            .unwrap(),
        Reaped {
            requeued: 1,
            dead: 0
        }
    );
    assert_eq!(job_state(&fx.pool, stale).await, "queued");
    assert_eq!(job_state(&fx.pool, fresh).await, "running");
    assert_eq!(
        job::reap(&mut conn, job::DEFAULT_LOCK_TIMEOUT)
            .await
            .unwrap(),
        Reaped::default()
    );

    // Another worker takes the reaped job. The original worker, if it wakes, holds nothing.
    let again = job::claim(&mut conn, &[JobKind::Audio], "other-pc")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((again.id, again.attempt), (stale, 2));
    for result in [
        job::heartbeat(&mut conn, &stale_job).await,
        job::succeed(&mut conn, &stale_job).await,
        job::fail(&mut conn, &stale_job, "late").await.map(|_| ()),
    ] {
        assert!(matches!(result, Err(JobError::LostLock(id)) if id == stale));
    }
    job::succeed(&mut conn, &again).await.unwrap();
    assert_eq!(job_state(&fx.pool, stale).await, "succeeded");
}

#[tokio::test]
async fn the_reaper_retires_a_stale_job_on_its_last_attempt() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let id = job::enqueue(&mut conn, JobKind::Audio, Some(t), "h", &NO_PAYLOAD)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("update job set max_attempts = 2 where id = $1")
        .bind(id.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    let expire = || async {
        sqlx::query("update job set locked_at = now() - interval '3 hours' where id = $1")
            .bind(id.as_uuid())
            .execute(&fx.pool)
            .await
            .unwrap();
    };

    // The worker dies on each attempt, as one killed by running out of memory would.
    let first = job::claim(&mut conn, &[JobKind::Audio], "pc")
        .await
        .unwrap()
        .unwrap();
    expire().await;
    let reaped = job::reap(&mut conn, job::DEFAULT_LOCK_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(
        reaped,
        Reaped {
            requeued: 1,
            dead: 0
        },
        "attempt 1 of 2 retries"
    );

    let second = job::claim(&mut conn, &[JobKind::Audio], "pc")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.attempt, second.attempt), (1, 2));
    expire().await;
    let reaped = job::reap(&mut conn, job::DEFAULT_LOCK_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(
        reaped,
        Reaped {
            requeued: 0,
            dead: 1
        },
        "attempt 2 of 2 is the last"
    );

    assert_eq!(job_state(&fx.pool, id).await, "dead");
    assert!(
        job::claim(&mut conn, &[JobKind::Audio], "pc")
            .await
            .unwrap()
            .is_none()
    );
    let error: Option<String> = sqlx::query_scalar("select last_error from job where id = $1")
        .bind(id.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(error.as_deref(), Some(job::LOCK_EXPIRED));
    assert_eq!(state(&fx.pool, t).await, "awaiting_consent");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("audio job dead: worker lock expired on the last attempt")
    );
}

#[tokio::test]
async fn finishing_a_stage_and_enqueueing_the_next_commit_together() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    {
        let mut conn = fx.pool.acquire().await.unwrap();
        job::enqueue(&mut conn, JobKind::Correct, Some(t), "v1", &NO_PAYLOAD)
            .await
            .unwrap();
    }
    let mut conn = fx.pool.acquire().await.unwrap();
    let j = job::claim(&mut conn, &[JobKind::Correct], "w")
        .await
        .unwrap()
        .unwrap();

    let next_jobs = || async {
        sqlx::query_scalar::<_, i64>("select count(*) from job where kind = 'extract'")
            .fetch_one(&fx.pool)
            .await
            .unwrap()
    };

    // A handler that fails after writing rolls everything back and still holds the lease.
    {
        let mut tx = conn.begin().await.unwrap();
        job::succeed(&mut tx, &j).await.unwrap();
        job::enqueue(&mut tx, JobKind::Extract, Some(t), "v1", &NO_PAYLOAD)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
    }
    assert_eq!(job_state(&fx.pool, j.id).await, "running");
    assert_eq!(next_jobs().await, 0);

    let mut tx = conn.begin().await.unwrap();
    job::succeed(&mut tx, &j).await.unwrap();
    job::enqueue(&mut tx, JobKind::Extract, Some(t), "v1", &NO_PAYLOAD)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(job_state(&fx.pool, j.id).await, "succeeded");
    assert_eq!(next_jobs().await, 1);
    let finished: bool = sqlx::query_scalar(
        "select finished_at is not null and locked_by is null from job where id = $1",
    )
    .bind(j.id.as_uuid())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert!(finished);
}

#[tokio::test]
async fn a_capped_job_waits_for_next_month_without_using_an_attempt() {
    let fx = common::start().await;
    let id = enqueue(&fx.pool, JobKind::Extract, "capped-input")
        .await
        .unwrap();
    let mut conn = fx.pool.acquire().await.unwrap();
    let claimed = job::claim(&mut conn, &[JobKind::Extract], "w")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, id);
    let until = wt_db::llm::next_month(Utc::now());
    job::defer(&mut conn, &claimed, until, "cap_reached")
        .await
        .unwrap();
    let (state, attempts, run_at, error): (
        String,
        i32,
        sqlx::types::chrono::DateTime<Utc>,
        String,
    ) = sqlx::query_as("select state::text, attempts, run_at, last_error from job where id = $1")
        .bind(id.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        (state.as_str(), attempts, error.as_str()),
        ("queued", 0, "cap_reached")
    );
    assert_eq!(run_at, until, "the first of next month");
    assert!(
        job::claim(&mut conn, &[JobKind::Extract], "w")
            .await
            .unwrap()
            .is_none(),
        "not due"
    );
    // Raising the cap releases it at once.
    assert_eq!(
        wt_db::llm::set_cap(&mut conn, 60.0, "owner").await.unwrap(),
        1
    );
    assert!(
        job::claim(&mut conn, &[JobKind::Extract], "w")
            .await
            .unwrap()
            .is_some()
    );
}
