//! Spec 01 M0: `wt db migrate` applies every migration to an empty database and is idempotent.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use wt_db::test_support::TestDb;

#[tokio::test]
async fn migrations_apply_to_an_empty_database_and_are_idempotent() {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();

    let first = wt_db::migrate(&pool).await.unwrap();
    assert_eq!(first.applied, first.known);
    assert!(
        first.known >= 8,
        "Spec 02 defines eight migrations, later milestones add more"
    );

    let second = wt_db::migrate(&pool).await.unwrap();
    assert_eq!(second.applied, 0, "a second run must change nothing");

    wt_db::ping(&pool).await.unwrap();
}

#[tokio::test]
async fn migrations_create_the_spec_objects() {
    let db = TestDb::start().await.unwrap();
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();

    let relations: Vec<String> = sqlx::query_scalar(
        "select c.relname::text from pg_class c join pg_namespace n on n.oid = c.relnamespace
         where n.nspname = 'public' and c.relkind in ('r', 'v') order by 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for expected in [
        "sangha",
        "teacher",
        "teacher_alias",
        "speaker_consent",
        "current_consent",
        "license_evidence",
        "glossary_term",
        "source_video",
        "talk",
        "transcript",
        "segment",
        "passage",
        "work",
        "work_locator",
        "citation",
        "job",
        "tester",
        "query_log",
        "answer",
        "answer_passage",
        "feedback",
        "dana_event",
        "correction_log",
        "takedown_log",
        "source_policy",
        "rights_event",
        "permission_request",
        "source_exclusion",
        "v_source_rights",
        "eval_set",
        "eval_question",
        "eval_run",
        "eval_result",
        "eval_rating",
        "claim_audit",
        "v_answer_stats",
        "v_consent_coverage",
        "v_dana_clicks",
        "servable_passage",
        "servable_citation",
    ] {
        assert!(
            relations.iter().any(|r| r == expected),
            "missing relation {expected}"
        );
    }

    let roles: Vec<String> = sqlx::query_scalar(
        "select rolname::text from pg_roles where rolname like 'wt\\_%' and not rolcanlogin order by 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(roles, ["wt_analyst", "wt_owner", "wt_pipeline", "wt_web"]);

    let foreign_owned: Vec<String> = sqlx::query_scalar(
        "select c.relname::text from pg_class c join pg_namespace n on n.oid = c.relnamespace
         where n.nspname = 'public' and c.relkind in ('r', 'v')
           and c.relname <> '_sqlx_migrations' and pg_get_userbyid(c.relowner) <> 'wt_owner'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        foreign_owned.is_empty(),
        "not owned by wt_owner: {foreign_owned:?}"
    );
}
