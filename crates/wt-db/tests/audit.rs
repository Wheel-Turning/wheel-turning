//! Spec 07, nightly audit: each database check has a failing fixture and a passing one, and
//! warn-level checks never change the outcome.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, BO, consent, make_ready, talk};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::Env;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_core::objects::{LocalObjects, ObjectStore, S3Objects};
use wt_core::s3::FakeBucket;
use wt_db::audit::{
    AuditReport, AuditSettings, Files, SCRATCH_TTL_H, Severity, run, run_with_files,
};

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
            "question_consent_drift",
            "question_serving_parity",
            "student_speech",
            "takedown_completeness",
            "fixture_leak",
            "mixed_embeddings",
            "job_health",
            "donation_links",
            "consent_staleness",
            "quarantine_age",
            "series_metadata",
            "mandate_unrequested",
            "request_follow_up",
            "opted_out_served",
            "mandate_served_while_off",
            "text_excluded_served",
            "text_source_served_while_off",
            "text_takedown_completeness",
            "takedown_request_overdue",
            "mandate_exposed",
            "teacher_unnotified",
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
    only(&report, &["consent_drift", "question_consent_drift"]);
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
        "create or replace view servable_passage as {} and false",
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
    // For example, a late duplicate finding that moved the talk without deleting anything.
    sqlx::query("update talk set state = 'excluded', state_reason = 'duplicate' where id = $1")
        .bind(t.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();

    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["consent_drift", "question_consent_drift"]);
    assert!(report.failed());
}

#[tokio::test]
async fn a_hand_exclusion_leaves_nothing_for_the_audit_to_find() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;
    wt_db::consent::exclude_talk(&fx.pool, t, "held out by the speaker", ACTOR)
        .await
        .unwrap();
    only(&audit(&fx.pool, Env::Prod).await, &[]);
}

#[tokio::test]
async fn a_mandate_without_a_request_and_an_overdue_follow_up_warn() {
    let fx = common::start().await;
    clean(&fx.pool).await;
    // The ledger is append-only, so the old events are written with their dates.
    sqlx::raw_sql(
        "insert into source_policy (key, publisher, url_prefixes, basis)
           values ('silent', 'Silent Press', '{https://silent.example/}', 'mandate'),
                  ('recent', 'Recent Press', '{https://recent.example/}', 'mandate'),
                  ('asked', 'Asked Press', '{https://asked.example/}', 'mandate');
         insert into rights_event (source_key, event, actor, created_at)
           values ('silent', 'mandated', 'test', now() - interval '15 days'),
                  ('recent', 'mandated', 'test', now() - interval '3 days'),
                  ('asked', 'mandated', 'test', now() - interval '40 days');
         insert into permission_request (source_key, recipient, channel, status, sent_on, follow_up_on)
           values ('asked', 'hello@asked.example', 'email', 'sent',
                   current_date - 20, current_date - 6);",
    )
    .execute(&fx.pool)
    .await
    .unwrap();

    let report = audit(&fx.pool, Env::Prod).await;

    only(&report, &["mandate_unrequested", "request_follow_up"]);
    let unrequested = report
        .checks
        .iter()
        .find(|c| c.name == "mandate_unrequested")
        .unwrap();
    assert_eq!(
        unrequested.examples,
        ["silent"],
        "a recent mandate has time to ask"
    );
    assert_eq!(findings(&report, "request_follow_up"), 1);
    assert!(!report.failed());
}

#[tokio::test]
async fn a_teacher_used_on_a_mandate_without_a_notice_warns_after_fourteen_days() {
    let fx = common::start().await;
    clean(&fx.pool).await;
    let old = talk(&fx.pool, Some(BO), Some(BO)).await;
    // A mandated talk recorded 20 days ago, with no notice to its teacher.
    sqlx::raw_sql(
        "insert into license_evidence (publisher, license_code, evidence_snapshot_ref,
           snapshot_sha256, captured_at, reviewed_by, reviewed_at, allowed, kind)
         values ('Fixture Mountain Zendo', 'operator-mandate', 'records/x', 'x', now(),
                 'owner', now(), true, 'mandate');",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "update talk set rights_basis = 'mandate', created_at = now() - interval '20 days',
           license_evidence_id = (select id from license_evidence where kind = 'mandate')
         where id = $1",
    )
    .bind(old.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();

    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["teacher_unnotified"]);
    assert!(!report.failed());

    sqlx::query(
        "insert into permission_request (teacher_id, recipient, channel)
         select id, 'bo@example.invalid', 'email' from teacher where display_name = $1",
    )
    .bind(BO)
    .execute(&fx.pool)
    .await
    .unwrap();
    only(&audit(&fx.pool, Env::Prod).await, &[]);
}

/// A served text unit: an edition with approved evidence, ready and proofread.
async fn served_text(pool: &PgPool) -> sqlx::types::Uuid {
    let ev: sqlx::types::Uuid = sqlx::query_scalar(
        "insert into license_evidence (publisher, license_code, license_url, evidence_url,
                                       evidence_snapshot_ref, snapshot_sha256, captured_at,
                                       reviewed_by, reviewed_at, allowed)
         values ('Fixture Press', 'public-domain', 'https://example.invalid/l',
                 'https://example.invalid/e', 'records/x', 'x', now(), 'reviewer', now(), true)
         returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let edition: sqlx::types::Uuid = sqlx::query_scalar(
        "with w as (insert into work (title) values ('Audit Fixture Sutra') returning id)
         insert into text_edition (work_id, edition_label, rights_holder, license_evidence_id,
                                   excerptable, source_url)
         select id, 'Fixture 1901', 'public domain', $1, true, 'https://texts.example.invalid/a'
         from w returning id",
    )
    .bind(ev)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into text_unit (edition_id, ordinal, locator, label, text, word_count,
                                content_hash, embedding, embedding_model)
         values ($1, 0, 'audit:1', 'Audit Fixture Sutra, verse 1',
                 'All conditioned things are impermanent, the fixture says.', 8, 'unit-hash',
                 array_fill(0.03125::real, array[1024])::vector, 'fake')",
    )
    .bind(edition)
    .execute(pool)
    .await
    .unwrap();
    wt_db::texts::refresh_units_hash(&mut pool.acquire().await.unwrap(), edition)
        .await
        .unwrap();
    sqlx::query(
        "insert into proof_check (edition_id, sample_size, errors, reviewer, passed, units_hash)
         select id, 1, 0, 'reviewer', true, units_hash from text_edition where id = $1",
    )
    .bind(edition)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("update text_edition set state = 'ready' where id = $1")
        .bind(edition)
        .execute(pool)
        .await
        .unwrap();
    edition
}

#[tokio::test]
async fn texts_excluded_or_taken_down_fail_and_old_requests_warn() {
    let fx = common::start().await;
    clean(&fx.pool).await;
    let edition = served_text(&fx.pool).await;
    only(&audit(&fx.pool, Env::Prod).await, &[]);

    // An exclusion the view does not know about: the unit still serves, so the audit fails.
    for (scope, pattern) in [
        ("work", "audit fixture sutra"),
        ("content", "unit-hash"),
        ("prefix", "https://texts.example.invalid/"),
    ] {
        sqlx::query(
            "insert into source_exclusion (scope, pattern, reason, created_by)
             values ($1, $2, 'test', 'auditor')",
        )
        .bind(scope)
        .bind(pattern)
        .execute(&fx.pool)
        .await
        .unwrap();
        let report = audit(&fx.pool, Env::Prod).await;
        only(&report, &["text_excluded_served"]);
        assert!(report.failed());
        sqlx::query("update source_exclusion set lifted_at = now(), lifted_by = 'auditor'")
            .execute(&fx.pool)
            .await
            .unwrap();
    }

    // A withdrawn edition that kept its units.
    sqlx::query("update text_edition set state = 'withdrawn' where id = $1")
        .bind(edition)
        .execute(&fx.pool)
        .await
        .unwrap();
    only(
        &audit(&fx.pool, Env::Prod).await,
        &["text_takedown_completeness"],
    );
    sqlx::query("delete from text_unit")
        .execute(&fx.pool)
        .await
        .unwrap();
    only(&audit(&fx.pool, Env::Prod).await, &[]);

    // A removal request open for more than a business day since its sender confirmed it warns.
    // A fresh one does not, nor one never confirmed, nor an old one confirmed today.
    sqlx::raw_sql(
        "insert into takedown_request (target, status, verified_at) values ('fresh', 'open', now());
         insert into takedown_request (target, received_at)
           values ('never confirmed', now() - interval '6 days');
         insert into takedown_request (target, received_at, status, verified_at)
           values ('confirmed today', now() - interval '6 days', 'open', now())",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    only(&audit(&fx.pool, Env::Prod).await, &[]);
    // Requests from before confirmation existed have no verified_at and count from arrival.
    sqlx::query(
        "insert into takedown_request (target, received_at, status)
         values ('old', now() - interval '6 days', 'open')",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["takedown_request_overdue"]);
    assert!(!report.failed());
}

#[tokio::test]
async fn a_mandated_source_warns_once_the_tool_is_public() {
    let fx = common::start().await;
    clean(&fx.pool).await;
    sqlx::query(
        "insert into source_policy (key, publisher, url_prefixes, basis)
         values ('fixturepub', 'Fixture Publisher', '{https://pub.example.invalid/}', 'mandate')",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    // A fresh mandate also has no permission request yet, which warns after 14 days only.
    let private = audit(&fx.pool, Env::Prod).await;
    assert_eq!(findings(&private, "mandate_exposed"), 0);
    let public = run(
        &fx.pool,
        AuditSettings {
            public: true,
            ..AuditSettings::defaults(Env::Prod)
        },
    )
    .await
    .unwrap();
    assert_eq!(findings(&public, "mandate_exposed"), 1);
    assert!(!public.failed(), "a warning never fails the audit");
    sqlx::query("update source_policy set enabled = false")
        .execute(&fx.pool)
        .await
        .unwrap();
    let off = run(
        &fx.pool,
        AuditSettings {
            public: true,
            ..AuditSettings::defaults(Env::Prod)
        },
    )
    .await
    .unwrap();
    assert_eq!(findings(&off, "mandate_exposed"), 0);
}

/// Spec 04, Student questions: rule 3's checks cover every stored question, and a takedown
/// removes them.
#[tokio::test]
async fn stored_student_questions_get_rule_threes_checks() {
    let fx = common::start().await;
    // `make_ready` stores one question with the passage.
    let t = clean(&fx.pool).await;
    only(&audit(&fx.pool, Env::Prod).await, &[]);

    // A question holding a name or contact detail is student speech.
    for leak in [
        "Ada Fixture asked about this",
        "Write to sam@example.org about it",
        "Call 415 555 0100 about it",
        "See www.example.org about it",
        "Member 1234567 asks about it",
        "Is it like Fixture Mountain Zendo here?",
    ] {
        sqlx::query("update student_question set text = $1")
            .bind(leak)
            .execute(&fx.pool)
            .await
            .unwrap();
        only(&audit(&fx.pool, Env::Prod).await, &["student_speech"]);
    }
    sqlx::query("update student_question set text = 'What is the fixture asking?'")
        .execute(&fx.pool)
        .await
        .unwrap();

    // The view narrowed by mistake: the question parity check fails.
    let def: String = sqlx::query_scalar("select pg_get_viewdef('servable_student_question')")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "create or replace view servable_student_question as {} and false",
        def.trim().trim_end_matches(';')
    )))
    .execute(&fx.pool)
    .await
    .unwrap();
    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["question_serving_parity"]);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "create or replace view servable_student_question as {def}"
    )))
    .execute(&fx.pool)
    .await
    .unwrap();

    // Consent falling back keeps the question: drift, as for its passage.
    consent(&fx.pool, ADA, ConsentStatus::Requested).await;
    let report = audit(&fx.pool, Env::Prod).await;
    only(&report, &["consent_drift", "question_consent_drift"]);
    assert_eq!(findings(&report, "question_consent_drift"), 1);
    wt_db::takedown::run(&fx.pool, &wt_db::takedown::Scope::Talk(t), ACTOR)
        .await
        .unwrap();
    assert!(!audit(&fx.pool, Env::Prod).await.failed());
    let left: i64 = sqlx::query_scalar("select count(*) from student_question")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

// ---- checks that read object storage and the scratch directory --------------------------------

struct Dirs {
    objects: LocalObjects,
    scratch: std::path::PathBuf,
    root: std::path::PathBuf,
}

impl Dirs {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("wt-audit-{}", Uuid::new_v4()));
        let scratch = root.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        Dirs {
            objects: LocalObjects::new(root.join("objects")),
            scratch,
            root,
        }
    }

    async fn audit(&self, pool: &PgPool, ttl_h: u64) -> AuditReport {
        let files = Files {
            objects: &self.objects,
            scratch_root: &self.scratch,
            scratch_ttl_h: ttl_h,
        };
        run_with_files(pool, AuditSettings::defaults(Env::Prod), &files)
            .await
            .unwrap()
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const CLEAN_TRANSCRIPT: &[u8] = br#"{"schema":1,"words":[{"i":0,"w":"sit","s":0,"e":400,"p":0.9}],
    "spans":[{"kind":"talk","s":0,"e":400,"w0":0,"w1":1}]}"#;

#[tokio::test]
async fn the_file_checks_pass_on_a_clean_store_and_scratch() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;
    let dirs = Dirs::new();
    dirs.objects
        .put(&format!("transcripts/{t}/v1.json"), CLEAN_TRANSCRIPT)
        .unwrap();
    // A job running now has a fresh scratch directory.
    std::fs::create_dir_all(dirs.scratch.join(t.to_string())).unwrap();

    let report = dirs.audit(&fx.pool, SCRATCH_TTL_H).await;
    only(&report, &[]);
    assert!(!report.failed());
    assert!(report.checks.iter().any(|c| c.name == "audio_residue"));
    let pending: Vec<&str> = report.not_yet_checked.iter().map(|(n, _)| *n).collect();
    assert_eq!(pending, ["audio_residue (scratch bucket)"]);
}

#[tokio::test]
async fn audio_residue_fails_for_an_old_scratch_directory_or_an_audio_object() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;
    let dirs = Dirs::new();
    std::fs::create_dir_all(dirs.scratch.join(t.to_string())).unwrap();

    // With a zero TTL the directory is already too old.
    let report = dirs.audit(&fx.pool, 0).await;
    only(&report, &["audio_residue"]);
    assert!(report.failed());
    std::fs::remove_dir_all(dirs.scratch.join(t.to_string())).unwrap();
    only(&dirs.audit(&fx.pool, 0).await, &[]);

    dirs.objects
        .put(&format!("transcripts/{t}/talk.M4A"), b"not really audio")
        .unwrap();
    let report = dirs.audit(&fx.pool, SCRATCH_TTL_H).await;
    only(&report, &["audio_residue"]);
    assert_eq!(findings(&report, "audio_residue"), 1);
}

#[tokio::test]
async fn student_speech_fails_for_a_transcript_object_with_student_words() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;
    let dirs = Dirs::new();
    let key = format!("transcripts/{t}/v1.json");
    for bad in [
        br#"{"words":[],"spans":[{"kind":"qa_student","s":0,"e":10}]}"#.as_slice(),
        br#"{"words":[{"w":"hello","speaker":"SPEAKER_02"}],"spans":[]}"#.as_slice(),
    ] {
        dirs.objects.put(&key, bad).unwrap();
        let report = dirs.audit(&fx.pool, SCRATCH_TTL_H).await;
        only(&report, &["student_speech"]);
        assert!(report.failed());
    }
    dirs.objects.put(&key, CLEAN_TRANSCRIPT).unwrap();
    only(&dirs.audit(&fx.pool, SCRATCH_TTL_H).await, &[]);
}

#[tokio::test]
async fn takedown_completeness_fails_while_objects_remain_and_passes_after_finish() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;
    let dirs = Dirs::new();
    dirs.objects
        .put(&format!("transcripts/{t}/v1.json"), CLEAN_TRANSCRIPT)
        .unwrap();
    dirs.objects
        .put(&format!("citations/{t}.json"), b"[]")
        .unwrap();
    // The database step committed and object deletion failed.
    wt_db::takedown::run(&fx.pool, &wt_db::takedown::Scope::Talk(t), ACTOR)
        .await
        .unwrap();

    let report = dirs.audit(&fx.pool, SCRATCH_TTL_H).await;
    only(&report, &["takedown_completeness"]);
    assert_eq!(findings(&report, "takedown_completeness"), 2);
    assert!(report.failed());

    wt_db::takedown::finish(&fx.pool, &dirs.objects)
        .await
        .unwrap();
    only(&dirs.audit(&fx.pool, SCRATCH_TTL_H).await, &[]);
}

#[tokio::test]
async fn takedown_completeness_follows_the_log_when_a_restore_undoes_the_state() {
    let fx = common::start().await;
    let t = clean(&fx.pool).await;
    wt_db::takedown::run(&fx.pool, &wt_db::takedown::Scope::Talk(t), ACTOR)
        .await
        .unwrap();
    // A partial restore brings the talk back to ready with a transcript row, while the log keeps
    // the takedown. Triggers are off for the session, as in a restore.
    let mut conn = fx.pool.acquire().await.unwrap();
    sqlx::query("set session_replication_role = replica")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("update talk set state = 'ready' where id = $1")
        .bind(t.as_uuid())
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 1, 'asr', 'restored', 'h')",
    )
    .bind(t.as_uuid())
    .execute(&mut *conn)
    .await
    .unwrap();
    drop(conn);

    let report = audit(&fx.pool, Env::Prod).await;
    assert!(findings(&report, "takedown_completeness") >= 1);
    assert!(report.failed());
}

#[tokio::test]
async fn takedown_completeness_reports_an_object_planted_in_the_bucket_under_a_withdrawn_talk() {
    // Spec 11, PB1.2: the S3 store over the in-process versioned bucket.
    let fx = common::start().await;
    let t = clean(&fx.pool).await;
    let bucket = S3Objects::new(FakeBucket::new(2));
    wt_db::takedown::complete(&fx.pool, &wt_db::takedown::Scope::Talk(t), ACTOR, &bucket)
        .await
        .unwrap();
    let scratch = std::env::temp_dir().join(format!("wt-audit-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&scratch).unwrap();
    let files = Files {
        objects: &bucket,
        scratch_root: &scratch,
        scratch_ttl_h: SCRATCH_TTL_H,
    };
    let audit = || run_with_files(&fx.pool, AuditSettings::defaults(Env::Prod), &files);
    only(&audit().await.unwrap(), &[]);

    // A planted object, as a stray sync or a restored backup could leave.
    let planted = format!("transcripts/{t}/v1.json");
    bucket.put(&planted, CLEAN_TRANSCRIPT).unwrap();
    let report = audit().await.unwrap();
    only(&report, &["takedown_completeness"]);
    assert_eq!(findings(&report, "takedown_completeness"), 1);
    assert!(report.failed());

    // Deleted without its versions, the key still holds words: the audit counts the versions.
    bucket.bucket().delete_current(&planted);
    assert!(bucket.list(&planted).unwrap().is_empty());
    let report = audit().await.unwrap();
    only(&report, &["takedown_completeness"]);
    assert_eq!(findings(&report, "takedown_completeness"), 1);

    wt_db::takedown::finish(&fx.pool, &bucket).await.unwrap();
    only(&audit().await.unwrap(), &[]);
    let _ = std::fs::remove_dir_all(&scratch);
}
