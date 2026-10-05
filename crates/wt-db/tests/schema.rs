//! Spec 02 acceptance: the schema's own guards, the serving view, the role grants and the
//! vector index, each checked directly in the database rather than through the library.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ADA, BO, consent, make_ready, servable, talk};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::Secret;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;

const PASSWORD: &str = "test-password-0123456789abcdef";

/// The name of the constraint a failed statement violated, or `None` if it succeeded.
async fn violated(pool: &PgPool, sql: &str, talk: TalkId) -> Option<String> {
    match sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(talk.as_uuid())
        .execute(pool)
        .await
    {
        Ok(_) => None,
        Err(e) => Some(
            e.as_database_error()
                .and_then(|d| d.constraint())
                .unwrap_or("<no constraint>")
                .to_owned(),
        ),
    }
}

/// A talk by Ada in `awaiting_consent` whose consent is granted and holder known, so only
/// the column a case changes stands between it and `cleared`.
async fn eligible(pool: &PgPool) -> TalkId {
    let t = talk(pool, Some(ADA), Some(ADA)).await;
    sqlx::query(
        "update talk set rights_basis = 'speaker_consent', consent_status = 'granted'
         where id = $1",
    )
    .bind(t.as_uuid())
    .execute(pool)
    .await
    .unwrap();
    t
}

#[tokio::test]
async fn a_talk_cannot_reach_cleared_without_consent_a_holder_and_a_teacher() {
    let fx = common::start().await;
    let cases = [
        ("consent requested", "consent_status = 'requested'"),
        ("consent declined", "consent_status = 'declined'"),
        ("consent unknown", "consent_status = 'unknown'"),
        ("no rights basis", "rights_basis = null"),
        ("holder null", "rights_holder = null"),
        ("holder unknown", "rights_holder = 'unknown'"),
        ("holder Unknown", "rights_holder = 'Unknown'"),
        ("teacher null", "teacher_id = null"),
    ];
    for (case, change) in cases {
        let t = eligible(&fx.pool).await;
        let sql = format!("update talk set {change}, state = 'cleared' where id = $1");
        assert_eq!(
            violated(&fx.pool, &sql, t).await.as_deref(),
            Some("past_gate_requires_consent"),
            "{case} must not clear"
        );
    }

    let t = eligible(&fx.pool).await;
    assert_eq!(
        violated(
            &fx.pool,
            "update talk set state = 'cleared' where id = $1",
            t
        )
        .await,
        None,
        "with consent, a holder and a teacher the talk clears"
    );
    // The guard holds past cleared too, so a talk cannot lose its basis mid-pipeline.
    assert_eq!(
        violated(
            &fx.pool,
            "update talk set consent_status = 'withdrawn' where id = $1",
            t
        )
        .await
        .as_deref(),
        Some("past_gate_requires_consent")
    );
}

#[tokio::test]
async fn a_qa_student_segment_cannot_be_stored() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let transcript: Uuid = sqlx::query_scalar(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 1, 'asr', 'transcripts/fixture.json', 'h') returning id",
    )
    .bind(t.as_uuid())
    .fetch_one(&fx.pool)
    .await
    .unwrap();

    let insert = |ordinal: i32, kind: &'static str| {
        sqlx::query(
            "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
             values ($1, $2, $3, $4::segment_kind, 0, 1000, 'fixture words')",
        )
        .bind(t.as_uuid())
        .bind(transcript)
        .bind(ordinal)
        .bind(kind)
        .execute(&fx.pool)
    };

    let err = insert(0, "qa_student").await.unwrap_err();
    assert_eq!(
        err.as_database_error().and_then(|d| d.code()).as_deref(),
        Some("23514"),
        "a check violation, got {err}"
    );
    insert(1, "qa_teacher")
        .await
        .expect("a teacher's answer is stored");
}

#[tokio::test]
async fn servable_passage_hides_talks_that_are_not_ready_consented_and_held() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    make_ready(&fx.pool, t).await;
    assert_eq!(
        servable(&fx.pool).await,
        1,
        "a ready, consented, held talk serves"
    );

    // Not ready: every other state the passages can sit in.
    for state in ["indexing", "quarantined"] {
        sqlx::query("update talk set state = $2::talk_state where id = $1")
            .bind(t.as_uuid())
            .bind(state)
            .execute(&fx.pool)
            .await
            .unwrap();
        assert_eq!(
            servable(&fx.pool).await,
            0,
            "a talk in {state} must not serve"
        );
    }
    sqlx::query("update talk set state = 'ready' where id = $1")
        .bind(t.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(servable(&fx.pool).await, 1);

    // The talk constraint already refuses the next rows, so drop it to prove the view is a
    // second, independent guard.
    sqlx::raw_sql("alter table talk drop constraint past_gate_requires_consent")
        .execute(&fx.pool)
        .await
        .unwrap();
    for (case, change) in [
        ("consent requested", "consent_status = 'requested'"),
        ("consent declined", "consent_status = 'declined'"),
        ("consent withdrawn", "consent_status = 'withdrawn'"),
        ("no rights basis", "rights_basis = null"),
        ("holder null", "rights_holder = null"),
        ("holder unknown", "rights_holder = 'unknown'"),
        ("holder UNKNOWN", "rights_holder = 'UNKNOWN'"),
    ] {
        let sql = format!("update talk set {change} where id = $1");
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(t.as_uuid())
            .execute(&fx.pool)
            .await
            .unwrap();
        assert_eq!(servable(&fx.pool).await, 0, "{case} must not serve");
        sqlx::query(
            "update talk set consent_status = 'granted', rights_basis = 'speaker_consent',
                             rights_holder = $2
             where id = $1",
        )
        .bind(t.as_uuid())
        .bind(ADA)
        .execute(&fx.pool)
        .await
        .unwrap();
        assert_eq!(servable(&fx.pool).await, 1, "restored after {case}");
    }
}

/// A pool logged in through a fresh login role that inherits `group`.
async fn login_as(fx: &common::Fx, group: &str) -> PgPool {
    let login = format!("{group}_login");
    wt_db::create_login(&fx.pool, group, &login, &Secret::new(PASSWORD))
        .await
        .unwrap();
    let rest = fx.url().expose().split_once('@').unwrap().1;
    let url = Secret::new(format!("postgres://{login}:{PASSWORD}@{rest}"));
    wt_db::connect(&url).await.unwrap()
}

async fn allowed(pool: &PgPool, sql: &str) -> bool {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(pool)
        .await
        .is_ok()
}

/// A ready, served talk by `teacher` plus a stored answer, so the web and analyst roles have
/// rows to see or be refused.
async fn served_talk_with_answer(pool: &PgPool, teacher: &str) {
    let t = talk(pool, Some(teacher), Some(teacher)).await;
    consent(pool, teacher, ConsentStatus::Granted).await;
    let p = make_ready(pool, t).await;
    common::answer(pool, &[p]).await;
}

#[tokio::test]
async fn the_web_role_reads_only_the_serving_views_and_flags_through_flag_query() {
    let fx = common::start().await;
    served_talk_with_answer(&fx.pool, ADA).await;
    served_talk_with_answer(&fx.pool, BO).await;
    let web = login_as(&fx, "wt_web").await;

    for table in ["passage", "talk", "segment", "query_log"] {
        assert!(
            !allowed(&web, &format!("select count(*) from {table}")).await,
            "wt_web must not select from {table}"
        );
    }
    for view in ["servable_passage", "servable_citation"] {
        assert!(
            allowed(&web, &format!("select count(*) from {view}")).await,
            "wt_web reads {view}"
        );
    }
    for column in [
        "flagged = true",
        "flagged_at = now()",
        "query_text = 'changed'",
        "teacher_count = 0",
    ] {
        assert!(
            !allowed(&web, &format!("update query_log set {column}")).await,
            "wt_web must not update query_log directly: {column}"
        );
    }

    let ids: Vec<Uuid> = sqlx::query_scalar("select id from query_log order by created_at")
        .fetch_all(&fx.pool)
        .await
        .unwrap();
    let flag = |id: Uuid| {
        sqlx::query_scalar::<_, bool>("select flag_query($1)")
            .bind(id)
            .fetch_one(&web)
    };
    assert!(flag(ids[0]).await.unwrap(), "an existing query is flagged");
    assert!(flag(ids[0]).await.unwrap(), "flagging twice is harmless");
    assert!(
        !flag(Uuid::new_v4()).await.unwrap(),
        "an unknown id flags nothing"
    );

    let rows: Vec<(Uuid, bool, bool)> = sqlx::query_as(
        "select id, flagged, flagged_at is not null from query_log order by created_at",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [(ids[0], true, true), (ids[1], false, false)],
        "only the chosen row is flagged, with its time"
    );
}

#[tokio::test]
async fn the_analyst_role_reads_aggregates_and_only_flagged_query_text() {
    let fx = common::start().await;
    served_talk_with_answer(&fx.pool, ADA).await;
    served_talk_with_answer(&fx.pool, BO).await;
    sqlx::query(
        "update query_log set flagged = true, flagged_at = now(), query_text = 'flagged question'
         where id = (select id from query_log order by created_at limit 1)",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    let analyst = login_as(&fx, "wt_analyst").await;
    assert!(
        !allowed(&analyst, "select flag_query(gen_random_uuid())").await,
        "only the web role may flag"
    );

    for table in ["query_log", "passage", "talk", "answer", "dana_event"] {
        assert!(
            !allowed(&analyst, &format!("select count(*) from {table}")).await,
            "wt_analyst must not select from {table}"
        );
    }
    for view in ["v_answer_stats", "v_consent_coverage", "v_dana_clicks"] {
        assert!(
            allowed(&analyst, &format!("select * from {view}")).await,
            "wt_analyst reads {view}"
        );
    }
    let answers: i64 = sqlx::query_scalar("select sum(answers)::bigint from v_answer_stats")
        .fetch_one(&analyst)
        .await
        .unwrap();
    assert_eq!(answers, 2, "the aggregate counts every query");

    let texts: Vec<(String, bool)> =
        sqlx::query_as("select query_text, flagged from flagged_queries()")
            .fetch_all(&analyst)
            .await
            .unwrap();
    assert_eq!(texts, [("flagged question".to_owned(), true)]);
}

#[tokio::test]
async fn a_vector_query_on_servable_passage_uses_the_hnsw_index() {
    let fx = common::start().await;
    let ada = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let bo = talk(&fx.pool, Some(BO), Some(BO)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    consent(&fx.pool, BO, ConsentStatus::Granted).await;
    make_ready(&fx.pool, ada).await;
    make_ready(&fx.pool, bo).await;

    // 9,998 more synthetic passages split across the two talks, 10,000 in all. The outer
    // reference in the subquery makes Postgres draw a fresh random vector for each row.
    sqlx::query(
        "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms, text,
                              word_count, content_hash, embedding, embedding_model)
         select s.talk_id, s.id, 'talk', g.i, g.i * 1000, g.i * 1000 + 900, $1, 20, 'h' || g.i,
                (select array_agg(random()::real) from generate_series(1, 1024) where g.i > 0)::vector,
                'fixture-embed-v1'
         from generate_series(1, 4999) g(i)
         cross join segment s",
    )
    .bind(common::PASSAGE_TEXT)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        "analyze passage; analyze talk; analyze source_video; analyze teacher; analyze sangha",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    let rows: i64 = sqlx::query_scalar("select count(*) from servable_passage")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(rows, 10_000);

    let plan: Vec<String> = sqlx::query_scalar(
        "explain select passage_id from servable_passage
         order by embedding <=> (select embedding from passage limit 1) limit 10",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    let plan = plan.join("\n");
    assert!(
        plan.contains("passage_embedding_hnsw"),
        "expected an index scan on passage_embedding_hnsw, got:\n{plan}"
    );
}
