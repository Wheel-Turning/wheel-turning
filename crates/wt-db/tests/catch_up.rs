//! `wt db catch-up` (Spec 11, PB1.1): two databases in one throwaway server stand in for the PC
//! and RDS. The target is created from the source as a template, which keeps every id the way
//! the October 13 load did; then each side changes on its own. Everything is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ADA, BO, PASSAGE_TEXT, consent, make_ready};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_core::objects::LocalObjects;
use wt_db::catch_up::{CatchUpReport, Options, RowCounts, run};
use wt_db::identity;
use wt_db::parity::Kind;

/// The web tables the acceptance item names, counted on the target.
const WEB_TABLES: [&str; 4] = ["tester", "query_log", "feedback", "takedown_request"];

/// A ready talk for `teacher` whose video has a fixed external id.
async fn ready_talk(pool: &PgPool, external_id: &str, teacher: &str) -> TalkId {
    let talk = keyed_talk(pool, external_id, teacher).await;
    sqlx::query(
        "update talk set state = 'cleared', rights_basis = 'speaker_consent' where id = $1",
    )
    .bind(talk.as_uuid())
    .execute(pool)
    .await
    .unwrap();
    make_ready(pool, talk).await;
    talk
}

/// A talk awaiting consent for `teacher`, with a fixed external id.
async fn keyed_talk(pool: &PgPool, external_id: &str, teacher: &str) -> TalkId {
    let id: Uuid = sqlx::query_scalar(
        "with v as (
           insert into source_video (platform, external_id, url, title)
           values ('youtube', $1, 'https://example.invalid/' || $1, 'A fixture talk')
           returning id)
         insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder,
                           consent_status, state)
         select v.id, 'A fixture talk', te.id, te.sangha_id, te.display_name, 'granted',
                'awaiting_consent'
         from v, teacher te where te.display_name = $2
         returning id",
    )
    .bind(external_id)
    .bind(teacher)
    .fetch_one(pool)
    .await
    .unwrap();
    TalkId::from(id)
}

/// Adds a second passage to a ready talk, as a reindex on the source would, and marks the
/// talk changed.
async fn add_passage(pool: &PgPool, talk: TalkId) {
    sqlx::query(
        "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms, text,
                              word_count, content_hash, embedding, embedding_model)
         select talk_id, id, 'talk', 1, 0, 60000, $2, 20, 'h2',
                array_fill(0.02::real, array[1024])::vector, 'fixture-embed-v1'
         from segment where talk_id = $1",
    )
    .bind(talk.as_uuid())
    .bind(PASSAGE_TEXT)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("update talk set state_reason = 'reindexed' where id = $1")
        .bind(talk.as_uuid())
        .execute(pool)
        .await
        .unwrap();
}

/// Rows of `sql` with one uuid parameter.
async fn count(pool: &PgPool, sql: &str, id: Uuid) -> i64 {
    common::count(pool, sql, id).await
}

/// Each web table's row count.
async fn web_counts(pool: &PgPool) -> Vec<i64> {
    let mut out = Vec::new();
    for table in WEB_TABLES {
        out.push(
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {table}")))
                .fetch_one(pool)
                .await
                .unwrap(),
        );
    }
    out
}

/// The skip reason recorded for a talk key.
fn skipped<'a>(report: &'a CatchUpReport, key: &str) -> Option<&'a str> {
    report
        .skipped
        .iter()
        .find(|i| i.key == key)
        .map(|i| i.reason.as_str())
}

struct Pair {
    _fx: common::Fx,
    source: PgPool,
    target: PgPool,
}

/// A source database with registry and consent, and a target made from it as a template, named
/// `pc` and `rds-v1`.
async fn pair(seed: impl AsyncFnOnce(&PgPool)) -> Pair {
    let fx = common::start().await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    consent(&fx.pool, BO, ConsentStatus::Granted).await;
    seed(&fx.pool).await;
    fx.pool.close().await;
    let url = fx.url().expose().to_owned();
    let base = url.rsplit_once('/').unwrap().0.to_owned();
    let admin = wt_db::connect(&wt_core::Secret::new(format!("{base}/postgres")))
        .await
        .unwrap();
    sqlx::query("create database rds template wt")
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let source = wt_db::connect(&wt_core::Secret::new(url.clone()))
        .await
        .unwrap();
    let target = wt_db::connect(&wt_core::Secret::new(format!("{base}/rds")))
        .await
        .unwrap();
    identity::set(&source, "pc").await.unwrap();
    identity::set(&target, "rds-v1").await.unwrap();
    Pair {
        _fx: fx,
        source,
        target,
    }
}

/// The database's clock, so the cut-off orders correctly against row times.
async fn now(pool: &PgPool) -> chrono::DateTime<chrono::Utc> {
    sqlx::query_scalar("select now()")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn store() -> LocalObjects {
    LocalObjects::new(std::env::temp_dir().join(format!("wt-catch-up-{}", Uuid::new_v4())))
}

#[tokio::test]
async fn catch_up_copies_a_changed_talk_skips_one_withdrawn_on_the_target_and_leaves_web_rows() {
    let p = pair(async |pool| {
        ready_talk(pool, "changed", ADA).await;
        ready_talk(pool, "withdrawn-there", BO).await;
    })
    .await;
    let id = |key: &'static str| {
        let pool = p.source.clone();
        async move {
            let id: Uuid = sqlx::query_scalar(
                "select t.id from talk t join source_video sv on sv.id = t.source_video_id
                 where sv.external_id = $1",
            )
            .bind(key)
            .fetch_one(&pool)
            .await
            .unwrap();
            TalkId::from(id)
        }
    };
    let changed = id("changed").await;
    let withdrawn = id("withdrawn-there").await;
    let since = now(&p.source).await;

    // The target: beta rows from the web service, and a talk withdrawn there.
    let changed_passage: Uuid = sqlx::query_scalar("select id from passage where talk_id = $1")
        .bind(changed.as_uuid())
        .fetch_one(&p.target)
        .await
        .unwrap();
    let answer = common::answer(&p.target, &[wt_core::ids::PassageId::from(changed_passage)]).await;
    sqlx::query(
        "insert into feedback (answer_id, tester_id, target, kind, passage_id)
         select a.id, l.tester_id, 'excerpt', 'good', $2
         from answer a join query_log l on l.id = a.query_log_id where a.id = $1",
    )
    .bind(answer)
    .bind(changed_passage)
    .execute(&p.target)
    .await
    .unwrap();
    sqlx::query("insert into takedown_request (target, status) values ('a fixture url', 'open')")
        .execute(&p.target)
        .await
        .unwrap();
    sqlx::query("update talk set state = 'withdrawn' where id = $1")
        .bind(withdrawn.as_uuid())
        .execute(&p.target)
        .await
        .unwrap();
    let web_before = web_counts(&p.target).await;

    // The source: the pipeline keeps working on both talks and finds a new one.
    add_passage(&p.source, changed).await;
    add_passage(&p.source, withdrawn).await;
    let new = ready_talk(&p.source, "new-on-pc", ADA).await;

    let options = |apply| Options {
        since,
        apply,
        actor: "test".to_owned(),
    };
    let passages = "select count(*) from passage where talk_id = $1";

    // A dry run reports the work and writes nothing.
    let dry = run(&p.source, &p.target, &store(), &options(false))
        .await
        .unwrap();
    assert!(dry.dry_run);
    assert_eq!((dry.from.as_str(), dry.to.as_str()), ("pc", "rds-v1"));
    assert_eq!(dry.candidates, 3, "{dry:#?}");
    assert_eq!(
        (dry.talks.new, dry.talks.updated, dry.talks.skipped),
        (1, 1, 1)
    );
    assert!(dry.replay.is_none());
    assert_eq!(count(&p.target, passages, changed.as_uuid()).await, 1);
    assert_eq!(
        count(
            &p.target,
            "select count(*) from talk where id = $1",
            new.as_uuid()
        )
        .await,
        0
    );

    let report = run(&p.source, &p.target, &store(), &options(true))
        .await
        .unwrap();
    assert!(!report.dry_run);
    assert!(report.replay.is_some(), "the replay runs before the copy");
    assert!(report.failed.is_empty(), "{:#?}", report.failed);
    assert_eq!((report.talks.new, report.talks.updated), (1, 1));
    assert_eq!(
        report.rows.get("passage"),
        Some(&RowCounts {
            insert: 2,
            update: 0,
            delete: 0
        }),
        "the new passage of the changed talk and the new talk's passage"
    );

    // The changed talk caught up; its passage the answer cites is untouched.
    assert_eq!(count(&p.target, passages, changed.as_uuid()).await, 2);
    assert_eq!(
        count(
            &p.target,
            "select count(*) from passage where id = $1",
            changed_passage
        )
        .await,
        1
    );
    // The new talk arrived with every pipeline row.
    for table in ["talk", "source_video"] {
        let sql = format!(
            "select count(*) from {table} where id = (select {} from talk where id = $1)",
            if table == "talk" {
                "id"
            } else {
                "source_video_id"
            }
        );
        assert_eq!(count(&p.source, &sql, new.as_uuid()).await, 1);
        assert_eq!(count(&p.target, &sql, new.as_uuid()).await, 1, "{table}");
    }
    for table in [
        "transcript",
        "segment",
        "passage",
        "citation",
        "student_question",
    ] {
        let sql = format!("select count(*) from {table} where talk_id = $1");
        assert_eq!(count(&p.target, &sql, new.as_uuid()).await, 1, "{table}");
    }
    // The talk withdrawn on the target stays as it was there.
    assert_eq!(
        skipped(&report, "youtube:withdrawn-there"),
        Some("withdrawn on the target")
    );
    assert_eq!(common::state(&p.target, withdrawn).await, "withdrawn");
    assert_eq!(count(&p.target, passages, withdrawn.as_uuid()).await, 1);

    // The web rows are exactly as they were.
    assert_eq!(web_counts(&p.target).await, web_before);
    assert!(report.untouched_dropped.is_empty());

    // Parity: the withdrawal exists on the target only, so the PC still serves that talk. That
    // is the one unsafe difference, and it makes the command exit non-zero until it is replayed.
    let parity = report.parity.as_ref().unwrap();
    let unsafe_keys: Vec<(Kind, &str)> = parity
        .differences
        .iter()
        .filter(|d| d.is_unsafe)
        .map(|d| (d.kind, d.key.as_str()))
        .collect();
    assert_eq!(unsafe_keys, vec![(Kind::Talk, "youtube:withdrawn-there")]);
    assert!(report.failed());

    // Running it again changes nothing.
    let again = run(&p.source, &p.target, &store(), &options(true))
        .await
        .unwrap();
    assert_eq!(again.talks.new + again.talks.updated, 0, "{again:#?}");
    assert_eq!(again.talks.unchanged, 2);
}

#[tokio::test]
async fn catch_up_skips_a_talk_whose_cited_passage_the_source_replaced() {
    let p = pair(async |pool| {
        ready_talk(pool, "cited", ADA).await;
    })
    .await;
    let since = now(&p.source).await;
    let (talk, passage): (Uuid, Uuid) = sqlx::query_as("select talk_id, id from passage")
        .fetch_one(&p.target)
        .await
        .unwrap();
    common::answer(&p.target, &[wt_core::ids::PassageId::from(passage)]).await;
    let answers = web_counts(&p.target).await;

    // A reindex on the source gives the passage a new id.
    sqlx::query("delete from passage where id = $1")
        .bind(passage)
        .execute(&p.source)
        .await
        .unwrap();
    add_passage(&p.source, TalkId::from(talk)).await;

    let report = run(
        &p.source,
        &p.target,
        &store(),
        &Options {
            since,
            apply: true,
            actor: "test".to_owned(),
        },
    )
    .await
    .unwrap();
    let reason = skipped(&report, "youtube:cited").unwrap();
    assert!(reason.contains("answer_passage"), "{reason}");
    assert_eq!(
        count(
            &p.target,
            "select count(*) from passage where id = $1",
            passage
        )
        .await,
        1
    );
    assert_eq!(web_counts(&p.target).await, answers);
    assert_eq!(
        count(
            &p.target,
            "select count(*) from answer_passage where passage_id = $1",
            passage
        )
        .await,
        1
    );
}

#[tokio::test]
async fn catch_up_refuses_two_databases_with_one_name_or_different_migrations() {
    let p = pair(async |_| {}).await;
    let options = Options {
        since: now(&p.source).await,
        apply: false,
        actor: "test".to_owned(),
    };
    let same = run(&p.source, &p.source, &store(), &options).await;
    assert!(
        same.unwrap_err()
            .to_string()
            .contains("both databases are named"),
    );
    sqlx::query(
        "delete from _sqlx_migrations where version = (select max(version) from _sqlx_migrations)",
    )
    .execute(&p.target)
    .await
    .unwrap();
    let behind = run(&p.source, &p.target, &store(), &options).await;
    assert!(behind.unwrap_err().to_string().contains("migration"));
}

#[tokio::test]
async fn a_database_is_named_once() {
    let fx = common::start().await;
    assert_eq!(identity::read(&fx.pool).await.unwrap(), None);
    identity::set(&fx.pool, "rds-v1").await.unwrap();
    identity::set(&fx.pool, "rds-v1").await.unwrap();
    assert!(matches!(
        identity::set(&fx.pool, "pc").await,
        Err(identity::IdentityError::AlreadyNamed(_))
    ));
    assert!(matches!(
        identity::set(&fx.pool, "Not A Name").await,
        Err(identity::IdentityError::InvalidName(_))
    ));
    let renamed = sqlx::query("update database_identity set name = 'pc'")
        .execute(&fx.pool)
        .await;
    assert!(renamed.is_err(), "the name is written once");
    assert_eq!(
        identity::verify(&fx.pool, Some("rds-v1"), wt_core::Env::Prod)
            .await
            .unwrap(),
        "rds-v1"
    );
}
