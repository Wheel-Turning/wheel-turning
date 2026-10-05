//! Spec 07, takedown: the database transaction, its record, and what it leaves behind.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, BO, consent, count, current_consent, make_ready, servable, state, talk};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::{PassageId, TalkId};
use wt_core::stores;
use wt_db::registry::RegistryDbError;
use wt_db::takedown::{Scope, TakedownError, dry_run, run};

/// Ada has two ready talks, Bo has one. Returns (Ada's talks, Ada's passages, Bo's talk, Bo's
/// passage).
async fn two_teachers(pool: &PgPool) -> (Vec<TalkId>, Vec<PassageId>, TalkId, PassageId) {
    let a1 = talk(pool, Some(ADA), Some(ADA)).await;
    let a2 = talk(pool, Some(ADA), Some(ADA)).await;
    let b = talk(pool, Some(BO), Some(BO)).await;
    consent(pool, ADA, ConsentStatus::Granted).await;
    consent(pool, BO, ConsentStatus::Granted).await;
    let p1 = make_ready(pool, a1).await;
    let p2 = make_ready(pool, a2).await;
    let pb = make_ready(pool, b).await;
    (vec![a1, a2], vec![p1, p2], b, pb)
}

/// Rows for `talk` in every registered table that takedown deletes from.
async fn rows_left(pool: &PgPool, talk: TalkId) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for (store, table, column) in stores::tables() {
        if store.on_takedown != stores::OnTakedown::Delete {
            continue;
        }
        let n = count(
            pool,
            &format!("select count(*) from {table} where {column} = $1"),
            talk.as_uuid(),
        )
        .await;
        out.push((table.to_owned(), n));
    }
    out
}

#[tokio::test]
async fn teacher_takedown_leaves_no_rows_and_keeps_the_stub() {
    let fx = common::start().await;
    let (ada_talks, _, bo_talk, _) = two_teachers(&fx.pool).await;
    assert_eq!(servable(&fx.pool).await, 3);

    let report = run(&fx.pool, &Scope::Teacher("ada".to_owned()), ACTOR)
        .await
        .unwrap();

    let mut talks = report.talks.clone();
    talks.sort();
    let mut expected = ada_talks.clone();
    expected.sort();
    assert_eq!(talks, expected);
    for table in ["passage", "citation", "segment", "transcript", "job"] {
        assert_eq!(
            report.rows.get(table),
            Some(&2),
            "{table} count in the report"
        );
    }
    assert!(!report.dry_run);

    for t in &ada_talks {
        for (table, n) in rows_left(&fx.pool, *t).await {
            assert_eq!(n, 0, "{table} still has rows for a withdrawn talk");
        }
        assert_eq!(state(&fx.pool, *t).await, "withdrawn");
        let (hash, reason, consent_status): (Option<String>, Option<String>, String) =
            sqlx::query_as(
                "select content_hash, state_reason, consent_status::text from talk where id = $1",
            )
            .bind(t.as_uuid())
            .fetch_one(&fx.pool)
            .await
            .unwrap();
        assert_eq!(hash, None);
        assert_eq!(reason.as_deref(), Some("takedown"));
        assert_eq!(consent_status, "withdrawn");
        assert_eq!(
            count(
                &fx.pool,
                "select count(*) from source_video sv join talk t on t.source_video_id = sv.id
                 where t.id = $1",
                t.as_uuid()
            )
            .await,
            1,
            "the stub talk and its source video stay so discovery cannot recreate the talk"
        );
    }

    assert_eq!(
        current_consent(&fx.pool, ADA).await.as_deref(),
        Some("withdrawn")
    );
    assert_eq!(state(&fx.pool, bo_talk).await, "ready", "Bo is untouched");
    for (table, n) in rows_left(&fx.pool, bo_talk).await {
        assert_eq!(n, 1, "Bo's {table} row must remain");
    }
    assert_eq!(servable(&fx.pool).await, 1);

    let (scope, logged_teacher, logged_talk, counts): (
        String,
        Option<Uuid>,
        Option<Uuid>,
        serde_json::Value,
    ) = sqlx::query_as("select scope, teacher_id, talk_id, counts from takedown_log")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(scope, "teacher");
    assert!(logged_teacher.is_some());
    assert_eq!(logged_talk, None);
    assert_eq!(counts["rows"]["passage"], 2);
}

#[tokio::test]
async fn takedown_strips_themes_from_stored_answers_and_clears_rating_notes() {
    let fx = common::start().await;
    let (_, ada_passages, _, bo_passage) = two_teachers(&fx.pool).await;
    let answer = common::answer(&fx.pool, &[ada_passages[0], bo_passage]).await;
    common::ratings(&fx.pool, &[ada_passages[0], bo_passage]).await;

    let report = run(&fx.pool, &Scope::Teacher(ADA.to_owned()), ACTOR)
        .await
        .unwrap();

    assert_eq!(report.answers_rewritten, 1);
    assert_eq!(report.themes_stripped, 1);
    assert_eq!(report.rating_notes_cleared, 1);
    assert_eq!(report.answer_passage_rows, 1);

    let payload: serde_json::Value = sqlx::query_scalar("select payload from answer where id = $1")
        .bind(answer)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let bo = serde_json::json!(bo_passage);
    assert_eq!(payload["themes"].as_array().unwrap().len(), 1);
    assert_eq!(payload["themes"][0]["passage_ids"], serde_json::json!([bo]));
    assert_eq!(payload["passage_ids"], serde_json::json!([bo]));
    assert_eq!(
        payload["mode"], "standard",
        "fields takedown does not know survive"
    );

    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from answer_passage where answer_id = $1",
            answer
        )
        .await,
        1
    );
    let notes: Vec<(Uuid, Option<String>)> =
        sqlx::query_as("select passage_id, note from eval_rating order by note nulls first")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        notes,
        vec![
            (ada_passages[0].as_uuid(), None),
            (bo_passage.as_uuid(), Some("a reviewer note".to_owned())),
        ]
    );
}

#[tokio::test]
async fn dry_run_reports_the_same_counts_and_changes_nothing() {
    let fx = common::start().await;
    let (ada_talks, ada_passages, _, _) = two_teachers(&fx.pool).await;
    common::answer(&fx.pool, &ada_passages).await;
    let scope = Scope::Teacher(ADA.to_owned());

    let preview = dry_run(&fx.pool, &scope).await.unwrap();

    assert!(preview.dry_run);
    assert_eq!(preview.rows.get("passage"), Some(&2));
    assert_eq!(preview.answer_passage_rows, 2);
    assert_eq!(servable(&fx.pool).await, 3);
    for t in &ada_talks {
        assert_eq!(state(&fx.pool, *t).await, "ready");
    }
    assert_eq!(
        current_consent(&fx.pool, ADA).await.as_deref(),
        Some("granted")
    );
    let logged: i64 = sqlx::query_scalar("select count(*) from takedown_log")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(logged, 0);

    let real = run(&fx.pool, &scope, ACTOR).await.unwrap();
    assert_eq!(real.rows, preview.rows);
    assert_eq!(real.answer_passage_rows, preview.answer_passage_rows);
}

#[tokio::test]
async fn talk_takedown_removes_one_talk_and_leaves_consent_alone() {
    let fx = common::start().await;
    let (ada_talks, _, _, _) = two_teachers(&fx.pool).await;
    let (gone, kept) = (ada_talks[0], ada_talks[1]);

    let report = run(&fx.pool, &Scope::Talk(gone), ACTOR).await.unwrap();

    assert_eq!(report.talks, vec![gone]);
    assert_eq!(state(&fx.pool, gone).await, "withdrawn");
    assert_eq!(state(&fx.pool, kept).await, "ready");
    let status: String = sqlx::query_scalar("select consent_status::text from talk where id = $1")
        .bind(gone.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        status, "granted",
        "a talk scope does not withdraw the teacher's consent"
    );
    assert_eq!(
        current_consent(&fx.pool, ADA).await.as_deref(),
        Some("granted")
    );

    let (scope, teacher, logged_talk): (String, Option<Uuid>, Option<Uuid>) =
        sqlx::query_as("select scope, teacher_id, talk_id from takedown_log")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(scope, "talk");
    assert_eq!(teacher, None);
    assert_eq!(logged_talk, Some(gone.as_uuid()));
}

#[tokio::test]
async fn a_withdrawn_talk_stays_withdrawn_when_consent_is_granted_again() {
    let fx = common::start().await;
    let (ada_talks, _, _, _) = two_teachers(&fx.pool).await;
    run(&fx.pool, &Scope::Teacher(ADA.to_owned()), ACTOR)
        .await
        .unwrap();

    let out = wt_db::consent::set_consent(&fx.pool, ADA, ConsentStatus::Granted, None, None, ACTOR)
        .await
        .unwrap();

    assert!(out.cleared.is_empty());
    for t in &ada_talks {
        assert_eq!(state(&fx.pool, *t).await, "withdrawn");
    }
}

#[tokio::test]
async fn object_stores_are_reported_as_not_yet_checked() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let report = run(&fx.pool, &Scope::Talk(t), ACTOR).await.unwrap();
    let names: Vec<&str> = report
        .objects_not_checked
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(names, vec!["citation_objects", "transcript_objects"]);
}

#[tokio::test]
async fn unknown_scopes_are_refused() {
    let fx = common::start().await;
    let missing = TalkId::from(Uuid::new_v4());
    let err = run(&fx.pool, &Scope::Talk(missing), ACTOR)
        .await
        .unwrap_err();
    assert!(
        matches!(err, TakedownError::UnknownTalk(id) if id == missing),
        "{err:?}"
    );

    let err = dry_run(&fx.pool, &Scope::Teacher("Nobody".to_owned()))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            TakedownError::Registry(RegistryDbError::UnknownTeacher(_))
        ),
        "{err:?}"
    );
}
