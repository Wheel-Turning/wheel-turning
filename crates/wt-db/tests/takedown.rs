//! Spec 07, takedown: the database transaction, its record, and what it leaves behind.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, BO, consent, count, current_consent, make_ready, servable, state, talk};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::{PassageId, TalkId};
use wt_core::objects::{LocalObjects, ObjectStore, S3Objects};
use wt_core::s3::FakeBucket;
use wt_core::stores;
use wt_db::registry::RegistryDbError;
use wt_db::takedown::{
    Scope, TakedownError, TakedownRecord, complete, dry_run, finish, replay, run,
};

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

/// A fresh local object store under the system temp directory.
fn objects() -> (LocalObjects, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("wt-takedown-{}", Uuid::new_v4()));
    (LocalObjects::new(&root), root)
}

/// Writes what the pipeline stores for a talk: transcript versions, staged questions, glossary
/// suggestions and the citation export.
fn store_talk(store: &dyn ObjectStore, talk: TalkId) {
    let t = talk.as_uuid();
    for key in [
        format!("transcripts/{t}/v1.json"),
        format!("transcripts/{t}/v2.json"),
        format!("transcripts/{t}/questions.json"),
        format!("transcripts/{t}/glossary_suggestions.csv"),
        format!("citations/{t}.json"),
    ] {
        store.put(&key, b"{}").unwrap();
    }
}

fn talk_keys(store: &dyn ObjectStore, talk: TalkId) -> Vec<String> {
    let t = talk.as_uuid();
    let mut keys = store.list(&format!("transcripts/{t}/")).unwrap();
    keys.extend(store.list(&format!("citations/{t}.json")).unwrap());
    keys
}

#[tokio::test]
async fn a_takedown_leaves_no_keys_and_no_versions_under_its_prefixes_in_a_versioned_bucket() {
    // Spec 11, PB1.2: the S3 store over the in-process versioned bucket.
    let fx = common::start().await;
    let (ada_talks, _, bo_talk, _) = two_teachers(&fx.pool).await;
    let store = S3Objects::new(FakeBucket::new(2));
    for t in ada_talks.iter().chain([&bo_talk]) {
        store_talk(&store, *t);
        // A second version of each transcript, and a key deleted earlier that kept its version.
        let t = t.as_uuid();
        store
            .put(&format!("transcripts/{t}/v1.json"), b"{\"v\":2}")
            .unwrap();
        store
            .put(&format!("transcripts/{t}/old.json"), b"{}")
            .unwrap();
        store
            .bucket()
            .delete_current(&format!("transcripts/{t}/old.json"));
    }

    let report = complete(&fx.pool, &Scope::Teacher(ADA.into()), ACTOR, &store)
        .await
        .unwrap();
    assert_eq!(report.objects["transcript_objects"], 8);
    assert_eq!(report.objects["citation_objects"], 2);
    for t in &ada_talks {
        let t = t.as_uuid();
        for (_, prefix) in stores::talk_objects(&t.to_string()) {
            assert!(
                store.list(&prefix).unwrap().is_empty(),
                "keys left under {prefix}"
            );
            assert_eq!(
                store.versions(&prefix).unwrap(),
                0,
                "versions left under {prefix}"
            );
        }
        assert_eq!(
            store.bucket().version_count(&format!("transcripts/{t}/")),
            0
        );
    }
    assert_eq!(talk_keys(&store, bo_talk).len(), 5, "Bo's objects stay");
    assert!(
        store
            .get(&wt_db::takedown::record_key(report.log_id.unwrap()))
            .unwrap()
            .is_some(),
        "the takedown record is kept"
    );
}

#[tokio::test]
async fn a_complete_takedown_leaves_no_rows_and_no_objects_in_any_registered_store() {
    let fx = common::start().await;
    let (ada_talks, _, bo_talk, _) = two_teachers(&fx.pool).await;
    let (store, root) = objects();
    for t in ada_talks.iter().chain([&bo_talk]) {
        store_talk(&store, *t);
    }

    let dry = wt_db::takedown::dry_run_with_objects(&fx.pool, &Scope::Teacher(ADA.into()), &store)
        .await
        .unwrap();
    assert_eq!(dry.objects["transcript_objects"], 8);
    assert_eq!(dry.objects["citation_objects"], 2);
    assert_eq!(
        talk_keys(&store, ada_talks[0]).len(),
        5,
        "a dry run deletes nothing"
    );

    let report = complete(&fx.pool, &Scope::Teacher(ADA.into()), ACTOR, &store)
        .await
        .unwrap();
    assert_eq!(report.objects["transcript_objects"], 8);
    assert_eq!(report.objects["citation_objects"], 2);
    for t in &ada_talks {
        assert!(talk_keys(&store, *t).is_empty(), "objects left for {t}");
        for (table, n) in rows_left(&fx.pool, *t).await {
            assert_eq!(n, 0, "{table} still has rows for a withdrawn talk");
        }
    }
    assert_eq!(talk_keys(&store, bo_talk).len(), 5, "Bo's objects stay");

    // The takedown_log row is copied outside the database, holding ids and no words.
    let log_id = report.log_id.unwrap();
    let record: TakedownRecord = serde_json::from_slice(
        &store
            .get(&wt_db::takedown::record_key(log_id))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(record.scope, "teacher");
    let mut talks = record.talks.clone();
    talks.sort();
    let mut expected = ada_talks.clone();
    expected.sort();
    assert_eq!(talks, expected);
    assert_eq!(
        record.sources.len(),
        2,
        "each talk's source video is recorded"
    );
    assert!(record.sources.values().all(|s| s.starts_with("youtube:")));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn replay_lists_a_recorded_talk_the_restore_does_not_hold_with_its_source() {
    let fx = common::start().await;
    let (ada_talks, _, _, _) = two_teachers(&fx.pool).await;
    let (store, root) = objects();
    complete(&fx.pool, &Scope::Talk(ada_talks[0]), ACTOR, &store)
        .await
        .unwrap();
    let source: String = sqlx::query_scalar(
        "select 'youtube:' || sv.external_id from talk t
         join source_video sv on sv.id = t.source_video_id where t.id = $1",
    )
    .bind(ada_talks[0].as_uuid())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    // A snapshot from before the talk was discovered holds neither it nor the log row.
    // Triggers are off for the session, as in a restore.
    let mut conn = fx.pool.acquire().await.unwrap();
    for sql in [
        "set session_replication_role = replica",
        "delete from takedown_log",
        "set session_replication_role = origin",
    ] {
        sqlx::query(sql).execute(&mut *conn).await.unwrap();
    }
    sqlx::query("delete from talk where id = $1")
        .bind(ada_talks[0].as_uuid())
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);

    let report = replay(&fx.pool, &store, "restore").await.unwrap();
    assert_eq!(report.missing_talks, [(ada_talks[0], Some(source))]);
    assert!(report.talks_reapplied.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn finish_deletes_objects_a_takedown_left_behind() {
    let fx = common::start().await;
    let (ada_talks, _, bo_talk, _) = two_teachers(&fx.pool).await;
    let (store, root) = objects();
    for t in ada_talks.iter().chain([&bo_talk]) {
        store_talk(&store, *t);
    }
    // The database step committed, then object deletion never ran.
    run(&fx.pool, &Scope::Talk(ada_talks[0]), ACTOR)
        .await
        .unwrap();
    assert_eq!(talk_keys(&store, ada_talks[0]).len(), 5);

    let done = finish(&fx.pool, &store).await.unwrap();
    assert_eq!(done.scopes, 1);
    assert_eq!(done.records_written, 1);
    assert_eq!(done.objects["transcript_objects"], 4);
    assert!(talk_keys(&store, ada_talks[0]).is_empty());
    assert_eq!(talk_keys(&store, ada_talks[1]).len(), 5, "not in scope");
    assert_eq!(talk_keys(&store, bo_talk).len(), 5, "not in scope");

    // Nothing left: a second finish deletes nothing and writes no record.
    let again = finish(&fx.pool, &store).await.unwrap();
    assert_eq!(again.objects.values().sum::<i64>(), 0);
    assert_eq!(again.records_written, 0);
    let _ = std::fs::remove_dir_all(root);
}

/// Puts the database back as a snapshot taken before Ada's takedown would hold it: her talks
/// ready with their rows, her consent granted, and no takedown_log rows. Triggers are off for
/// the session, as in a restore.
async fn restore_snapshot(pool: &PgPool, talks: &[TalkId]) {
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("set session_replication_role = replica")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("delete from takedown_log")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("delete from speaker_consent where status = 'withdrawn'")
        .execute(&mut *conn)
        .await
        .unwrap();
    let ids: Vec<Uuid> = talks.iter().map(TalkId::as_uuid).collect();
    sqlx::query(
        "update talk set state = 'cleared', state_reason = null, consent_status = 'granted'
         where id = any($1)",
    )
    .bind(&ids)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("set session_replication_role = origin")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    for t in talks {
        make_ready(pool, *t).await;
    }
}

#[tokio::test]
async fn replay_reapplies_every_recorded_takedown_after_a_restore() {
    let fx = common::start().await;
    let (ada_talks, _, bo_talk, _) = two_teachers(&fx.pool).await;
    let (store, root) = objects();
    complete(&fx.pool, &Scope::Teacher(ADA.into()), ACTOR, &store)
        .await
        .unwrap();
    assert_eq!(servable(&fx.pool).await, 1);

    restore_snapshot(&fx.pool, &ada_talks).await;
    assert_eq!(
        servable(&fx.pool).await,
        3,
        "the restore brought Ada's talks back"
    );
    assert_eq!(
        current_consent(&fx.pool, ADA).await.as_deref(),
        Some("granted")
    );
    // Their objects came back too, as from an older copy of the bucket.
    for t in &ada_talks {
        store_talk(&store, *t);
    }

    let report = replay(&fx.pool, &store, "restore").await.unwrap();
    assert_eq!(report.records, 1);
    let mut reapplied = report.talks_reapplied.clone();
    reapplied.sort();
    let mut expected = ada_talks.clone();
    expected.sort();
    assert_eq!(reapplied, expected);
    assert_eq!(report.consent_withdrawn_again.len(), 1);
    assert!(report.missing_talks.is_empty());
    assert_eq!(report.objects["transcript_objects"], 8);

    assert_eq!(servable(&fx.pool).await, 1);
    for t in &ada_talks {
        assert_eq!(state(&fx.pool, *t).await, "withdrawn");
        for (table, n) in rows_left(&fx.pool, *t).await {
            assert_eq!(n, 0, "{table} has rows after the replay");
        }
        assert!(talk_keys(&store, *t).is_empty());
    }
    assert_eq!(
        current_consent(&fx.pool, ADA).await.as_deref(),
        Some("withdrawn")
    );
    assert_eq!(state(&fx.pool, bo_talk).await, "ready");

    // Replaying again changes nothing.
    let again = replay(&fx.pool, &store, "restore").await.unwrap();
    assert!(again.talks_reapplied.is_empty(), "{again:?}");
    assert!(again.consent_withdrawn_again.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn replay_leaves_a_later_grant_of_consent_alone() {
    let fx = common::start().await;
    let (ada_talks, _, _, _) = two_teachers(&fx.pool).await;
    let (store, root) = objects();
    complete(&fx.pool, &Scope::Teacher(ADA.into()), ACTOR, &store)
        .await
        .unwrap();
    // Ada grants consent again after the takedown. Her taken-down talks stay withdrawn.
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;

    let report = replay(&fx.pool, &store, "restore").await.unwrap();
    assert!(report.consent_withdrawn_again.is_empty());
    assert!(report.talks_reapplied.is_empty());
    assert_eq!(
        current_consent(&fx.pool, ADA).await.as_deref(),
        Some("granted")
    );
    for t in &ada_talks {
        assert_eq!(state(&fx.pool, *t).await, "withdrawn");
    }
    let _ = std::fs::remove_dir_all(root);
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
