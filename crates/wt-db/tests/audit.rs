//! Spec 07, nightly audit: each database check has a failing fixture and a passing one, and
//! warn-level checks never change the outcome.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, consent, make_ready, talk};
use sqlx::PgPool;
use wt_core::Env;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_db::audit::{AuditReport, AuditSettings, Severity, run};

/// A ready, consented talk whose sangha's donation link was just verified, so every check
/// starts clean.
async fn clean(pool: &PgPool) -> TalkId {
    let t = talk(pool, Some(ADA), Some(ADA)).await;
    consent(pool, ADA, ConsentStatus::Granted).await;
    make_ready(pool, t).await;
    sqlx::query("update sangha set donation_verified = true, donation_verified_at = now()")
        .execute(pool)
        .await
        .unwrap();
    t
}

async fn audit(pool: &PgPool, env: Env) -> AuditReport {
    run(pool, AuditSettings::defaults(env)).await.unwrap()
}

fn findings(report: &AuditReport, name: &str) -> i64 {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check named {name}"))
        .findings
}

/// Asserts that exactly the named checks have findings.
fn only(report: &AuditReport, names: &[&str]) {
    for check in &report.checks {
        assert_eq!(
            check.findings > 0,
            names.contains(&check.name),
            "{}: {} findings {:?}",
            check.name,
            check.findings,
            check.examples
        );
    }
}

#[tokio::test]
async fn a_clean_database_passes_every_check_in_prod() {
    let fx = common::start().await;
    clean(&fx.pool).await;

    let report = audit(&fx.pool, Env::Prod).await;

    only(&report, &[]);
    assert!(!report.failed());
    let names: Vec<&str> = report.checks.iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        [
            "consent_drift",
            "serving_parity",
            "student_speech",
            "takedown_completeness",
            "fixture_leak",
            "mixed_embeddings",
            "job_health",
            "donation_links",
            "consent_staleness",
            "quarantine_age",
            "series_metadata",
        ]
    );
    let pending: Vec<&str> = report.not_yet_checked.iter().map(|(n, _)| *n).collect();
    assert!(pending.contains(&"audio_residue"), "{pending:?}");
}

#[tokio::test]
async fn consent_drift_fails_when_passages_outlive_consent() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;

    // Consent falling back to requested returns the talk to the gate and keeps its passages.
    consent(&fx.pool, ADA, ConsentStatus::Requested).await;
    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["consent_drift"]);
    assert!(report.failed());
    let drift = report
        .checks
        .iter()
        .find(|c| c.name == "consent_drift")
        .unwrap();
    assert_eq!(drift.severity, Severity::Fail);
    assert_eq!(drift.findings, 1);

    wt_db::takedown::run(&fx.pool, &wt_db::takedown::Scope::Talk(t), ACTOR)
        .await
        .unwrap();
    assert!(!audit(&fx.pool, Env::Prod).await.failed());
}

#[tokio::test]
async fn a_decline_leaves_nothing_for_the_audit_to_find() {
    let fx = common::start().await;
    clean(&fx.pool).await;
    consent(&fx.pool, ADA, ConsentStatus::Declined).await;
    only(&audit(&fx.pool, Env::Prod).await, &[]);
}

#[tokio::test]
async fn serving_parity_fails_when_the_view_drifts_from_the_rule() {
    let fx = common::start().await;
    clean(&fx.pool).await;

    // Simulate a later migration that narrows the view by mistake.
    let def: String = sqlx::query_scalar("select pg_get_viewdef('servable_passage')")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let narrowed = format!(
        "create or replace view servable_passage with (security_barrier = true) as {} and false",
        def.trim().trim_end_matches(';')
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(narrowed))
        .execute(&fx.pool)
        .await
        .unwrap();

    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["serving_parity"]);
    assert_eq!(findings(&report, "serving_parity"), 1);
    assert!(report.failed());
}

#[tokio::test]
async fn student_speech_fails_if_a_qa_student_segment_ever_gets_stored() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;

    // The schema forbids the row, so drop that guard to prove the audit is a second one.
    let constraint: String = sqlx::query_scalar(
        "select conname::text from pg_constraint
         where conrelid = 'segment'::regclass and pg_get_constraintdef(oid) like '%qa_student%'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "alter table segment drop constraint {constraint}"
    )))
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
         select talk_id, id, 1, 'qa_student', 60000, 61000, 'student words' from transcript
         where talk_id = $1",
    )
    .bind(t.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();

    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["student_speech"]);
    assert!(report.failed());
}

#[tokio::test]
async fn takedown_completeness_fails_when_rows_reappear_for_a_withdrawn_talk() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;
    wt_db::takedown::run(&fx.pool, &wt_db::takedown::Scope::Talk(t), ACTOR)
        .await
        .unwrap();
    assert!(!audit(&fx.pool, Env::Prod).await.failed());

    // For example, a restore from a backup taken before the takedown.
    sqlx::query(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 1, 'asr', 'restored', 'h')",
    )
    .bind(t.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();

    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["takedown_completeness"]);
    let check = report
        .checks
        .iter()
        .find(|c| c.name == "takedown_completeness")
        .unwrap();
    assert_eq!(check.examples, vec![format!("transcript: {t}")]);
    assert!(report.failed());
}

#[tokio::test]
async fn fixture_leak_fails_outside_dev_only() {
    let fx = common::start().await;
    clean(&fx.pool).await;
    common::talk_with(&fx.pool, Some(ADA), None, "A synthetic talk", true).await;

    assert!(!audit(&fx.pool, Env::Dev).await.failed());
    for env in [Env::Staging, Env::Prod] {
        let report = audit(&fx.pool, env).await;
        only(&report, &["fixture_leak"]);
        assert!(report.failed(), "{env:?}");
    }
}

#[tokio::test]
async fn mixed_embeddings_fails_with_two_models() {
    let fx = common::start().await;
    clean(&fx.pool).await;
    let other = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    common::make_ready_with_model(&fx.pool, other, "fixture-embed-v2").await;

    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["mixed_embeddings"]);
    assert_eq!(
        findings(&report, "mixed_embeddings"),
        2,
        "one finding per model"
    );
    assert!(report.failed());
}

#[tokio::test]
async fn warn_checks_list_findings_without_failing_the_audit() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;

    sqlx::query(
        "insert into job (kind, talk_id, idempotency_key, state) values
           ('fetch', $1, 'dead-job', 'dead')",
    )
    .bind(t.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into job (kind, talk_id, idempotency_key, state, locked_at) values
           ('fetch', $1, 'stale-lock', 'running', now() - interval '3 hours')",
    )
    .bind(t.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query("update sangha set donation_verified = false")
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into speaker_consent (teacher_id, status, decided_at, created_by)
         select id, 'granted', now() - interval '100 days', 'test' from teacher
         where display_name = $1",
    )
    .bind(ADA)
    .execute(&fx.pool)
    .await
    .unwrap();
    let quarantined = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    sqlx::query(
        "update talk set state = 'quarantined', quarantine_reason = 'fixture' where id = $1",
    )
    .bind(quarantined.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();
    // The updated_at trigger stamps every update with now(), so age the row with it disabled.
    sqlx::raw_sql(
        "alter table talk disable trigger talk_set_updated_at;
         update talk set updated_at = now() - interval '8 days' where state = 'quarantined';
         alter table talk enable trigger talk_set_updated_at;",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query("update talk set title = 'Genjokoan, part 3' where id = $1")
        .bind(t.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();

    let report = audit(&fx.pool, Env::Prod).await;

    only(
        &report,
        &[
            "job_health",
            "donation_links",
            "consent_staleness",
            "quarantine_age",
            "series_metadata",
        ],
    );
    assert_eq!(findings(&report, "job_health"), 2);
    for check in report.checks.iter().filter(|c| c.findings > 0) {
        assert_eq!(check.severity, Severity::Warn, "{}", check.name);
    }
    assert!(!report.failed(), "warn checks do not fail the audit");
}

#[tokio::test]
async fn warn_thresholds_come_from_settings() {
    let fx = common::start().await;
    clean(&fx.pool).await;
    sqlx::query("update sangha set donation_verified_at = now() - interval '30 days'")
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into speaker_consent (teacher_id, status, decided_at, created_by)
         select id, 'granted', now() - interval '30 days', 'test' from teacher
         where display_name = $1",
    )
    .bind(ADA)
    .execute(&fx.pool)
    .await
    .unwrap();

    let defaults = audit(&fx.pool, Env::Prod).await;
    only(&defaults, &[]);

    let strict = run(
        &fx.pool,
        AuditSettings {
            link_review_days: 7,
            consent_review_days: 7,
            ..AuditSettings::defaults(Env::Prod)
        },
    )
    .await
    .unwrap();
    only(&strict, &["donation_links", "consent_staleness"]);
}

#[tokio::test]
async fn quarantined_and_partly_indexed_talks_keep_passages_without_drift() {
    let fx = common::start().await;
    let quarantined = clean(&fx.pool).await;
    let indexing = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    make_ready(&fx.pool, indexing).await;

    sqlx::query(
        "update talk set state = 'quarantined', quarantine_reason = 'fixture' where id = $1",
    )
    .bind(quarantined.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query("update talk set state = 'indexing' where id = $1")
        .bind(indexing.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();

    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &[]);
    assert_eq!(common::servable(&fx.pool).await, 0, "neither is served");
}

#[tokio::test]
async fn consent_drift_fails_for_an_excluded_talk_that_kept_its_passages() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;
    wt_db::consent::exclude_talk(&fx.pool, t, "held out by the speaker", ACTOR)
        .await
        .unwrap();

    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["consent_drift"]);
    assert!(report.failed());
}
