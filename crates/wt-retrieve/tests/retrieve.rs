//! Spec 05, M5 acceptance: retrieval and selection over a synthetic corpus in a throwaway
//! database. Five invented teachers in four invented sanghas; every passage text is written
//! here and none comes from a real talk.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::collections::BTreeSet;

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::{PassageId, TeacherId};
use wt_db::test_support::TestDb;
use wt_ml::{Embedder, FakeEmbedder, FakeReranker, Reranker};
use wt_registry::RegistryFile;
use wt_retrieve::{
    Mode, NamedDetector, Notice, RetrievalConfig, Retrieved, query, retrieve, search,
};

const REGISTRY: &str = r#"
[[sangha]]
name = "North Fixture Zendo"
donation_url = "https://example.invalid/north"

[[sangha]]
name = "South Fixture Zendo"

[[sangha]]
name = "East Fixture Zendo"

[[sangha]]
name = "West Fixture Zendo"

[[teacher]]
display_name = "Ada Fixture"
sangha = "North Fixture Zendo"

[[teacher]]
display_name = "Bo Sample"
sangha = "South Fixture Zendo"

[[teacher]]
display_name = "Cy Example"
sangha = "East Fixture Zendo"

[[teacher]]
display_name = "Di Placeholder"
sangha = "West Fixture Zendo"

[[teacher]]
display_name = "Ed Stand-In"
sangha = "West Fixture Zendo"
"#;

const ACTOR: &str = "test";
const MODEL: &str = "fake-embed";

/// Filler that shares no word with the test questions.
const FILLER: &str = "The bell rang twice before the morning service and the kitchen crew \
                      washed the bowls slowly";

struct Fx {
    _db: TestDb,
    pool: PgPool,
}

async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let file = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &file, ACTOR).await.unwrap();
    Fx { _db: db, pool }
}

async fn teacher_id(pool: &PgPool, name: &str) -> TeacherId {
    let id: Uuid = sqlx::query_scalar("select id from teacher where display_name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    TeacherId::from(id)
}

/// A ready talk by `teacher` with one passage per text, 120 seconds apart. Consent is granted
/// first, so the talk serves.
async fn talk(pool: &PgPool, teacher: &str, texts: &[String]) -> Vec<PassageId> {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('youtube', $1, 'https://example.invalid/v', 'Fixture talk') returning id",
    )
    .bind(Uuid::new_v4().to_string())
    .fetch_one(pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, 'Fixture talk', te.id, te.sangha_id, $2, 'awaiting_consent'
         from teacher te where te.display_name = $2 returning id",
    )
    .bind(video)
    .bind(teacher)
    .fetch_one(pool)
    .await
    .unwrap();
    wt_db::consent::set_consent(
        pool,
        teacher,
        ConsentStatus::Granted,
        Some("fixture"),
        None,
        ACTOR,
    )
    .await
    .unwrap();
    sqlx::query("update talk set state = 'ready', content_hash = 'h' where id = $1")
        .bind(talk)
        .execute(pool)
        .await
        .unwrap();
    let transcript: Uuid = sqlx::query_scalar(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 1, 'asr', 'fixture', 'h') returning id",
    )
    .bind(talk)
    .fetch_one(pool)
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (i, text) in texts.iter().enumerate() {
        let ordinal = i32::try_from(i).unwrap();
        let start = ordinal * 120_000;
        let segment: Uuid = sqlx::query_scalar(
            "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
             values ($1, $2, $3, 'talk', $4, $4 + 60000, $5) returning id",
        )
        .bind(talk)
        .bind(transcript)
        .bind(ordinal)
        .bind(start)
        .bind(text)
        .fetch_one(pool)
        .await
        .unwrap();
        let vector = search::vector_literal(&FakeEmbedder::vector(text));
        let id: Uuid = sqlx::query_scalar(
            "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms,
                                  text, word_count, content_hash, embedding, embedding_model)
             values ($1, $2, 'talk', $3, $4, $4 + 60000, $5, 20, 'h', $6::text::vector, $7)
             returning id",
        )
        .bind(talk)
        .bind(segment)
        .bind(ordinal)
        .bind(start)
        .bind(text)
        .bind(vector)
        .bind(MODEL)
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push(PassageId::from(id));
    }
    ids
}

fn texts(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| format!("{l}. {FILLER}.")).collect()
}

/// Three teachers speak about anger, two only about the kitchen.
async fn corpus(pool: &PgPool) -> BTreeSet<PassageId> {
    let mut planted = BTreeSet::new();
    for (teacher, line) in [
        (
            "Ada Fixture",
            "When anger arises, turn toward the anger and breathe with it",
        ),
        (
            "Bo Sample",
            "Anger is energy; meet anger without feeding the story",
        ),
        (
            "Cy Example",
            "Notice anger in the body before naming the anger",
        ),
    ] {
        let ids = talk(
            pool,
            teacher,
            &texts(&[line, "The garden needs weeding", "Rain on the roof"]),
        )
        .await;
        planted.insert(ids[0]);
    }
    for teacher in ["Di Placeholder", "Ed Stand-In"] {
        talk(
            pool,
            teacher,
            &texts(&["The garden needs weeding", "Sweeping the zendo floor"]),
        )
        .await;
    }
    planted
}

async fn ask(
    pool: &PgPool,
    detector: &NamedDetector,
    text: &str,
    reranker: Option<&dyn Reranker>,
) -> Retrieved {
    let q = detector.parse(&query::normalize(text).unwrap());
    retrieve(
        pool,
        &FakeEmbedder::new(MODEL),
        reranker,
        &q,
        &RetrievalConfig::default(),
    )
    .await
    .unwrap()
}

async fn detector(pool: &PgPool) -> NamedDetector {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "select teacher_id, alias from teacher_alias
         union select id, display_name from teacher",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    NamedDetector::new(rows.into_iter().map(|(t, a)| (TeacherId::from(t), a)), "").unwrap()
}

fn ids(r: &Retrieved) -> BTreeSet<PassageId> {
    r.chosen.iter().map(|c| c.id).collect()
}

#[tokio::test]
async fn full_text_vector_and_fused_retrieval_find_the_planted_passages() {
    let fx = start().await;
    let planted = corpus(&fx.pool).await;
    let mut conn = fx.pool.acquire().await.unwrap();

    let q = search::or_query("How do I work with anger?").unwrap();
    let fts: BTreeSet<_> = search::full_text(&mut conn, &q, None, 60)
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(fts, planted, "only the anger passages hold the word");

    let v = FakeEmbedder::new(MODEL)
        .embed(&["How do I work with anger?".to_owned()])
        .await
        .unwrap()
        .remove(0);
    let near: Vec<_> = search::vector(&mut conn, &v, None, 3, 100).await.unwrap();
    assert_eq!(near.into_iter().collect::<BTreeSet<_>>(), planted);
    drop(conn);

    let r = ask(
        &fx.pool,
        &detector(&fx.pool).await,
        "How do I work with anger?",
        Some(&FakeReranker),
    )
    .await;
    assert_eq!(ids(&r), planted);
    assert_eq!(r.teacher_count, 3);
    assert_eq!(r.notice, None);
    assert!(!r.rerank_degraded);
}

#[tokio::test]
async fn the_threshold_never_pads_and_reports_thin_coverage() {
    let fx = start().await;
    corpus(&fx.pool).await;
    talk(
        &fx.pool,
        "Di Placeholder",
        &texts(&["Grief comes in waves; sit with grief"]),
    )
    .await;
    let d = detector(&fx.pool).await;

    let r = ask(&fx.pool, &d, "facing grief", Some(&FakeReranker)).await;
    assert_eq!(r.teacher_count, 1);
    assert_eq!(r.notice, Some(Notice::ThinCoverage { teachers: 1 }));

    let r = ask(
        &fx.pool,
        &d,
        "quantum chromodynamics lecture",
        Some(&FakeReranker),
    )
    .await;
    assert!(r.chosen.is_empty());
    assert_eq!(r.notice, Some(Notice::NoneFound));
}

#[tokio::test]
async fn without_a_reranker_the_fused_order_stands_and_is_marked_degraded() {
    let fx = start().await;
    let planted = corpus(&fx.pool).await;
    let r = ask(
        &fx.pool,
        &detector(&fx.pool).await,
        "How do I work with anger?",
        None,
    )
    .await;
    assert!(r.rerank_degraded);
    assert!(
        planted.is_subset(&ids(&r)),
        "the planted passages lead the fused order"
    );
}

#[tokio::test]
async fn named_mode_returns_only_that_teacher_and_says_when_there_is_little() {
    let fx = start().await;
    corpus(&fx.pool).await;
    let d = detector(&fx.pool).await;
    let bo = teacher_id(&fx.pool, "Bo Sample").await;

    let r = ask(
        &fx.pool,
        &d,
        "What does Bo Sample say about anger?",
        Some(&FakeReranker),
    )
    .await;
    assert_eq!(r.mode, Mode::NamedTeacher(bo));
    assert!(r.chosen.iter().all(|c| c.teacher_id == bo));
    assert_eq!(r.chosen.len(), 1);
    assert_eq!(r.notice, Some(Notice::NamedFew { found: 1 }));

    let r = ask(
        &fx.pool,
        &d,
        "What does Ed Stand-In say about anger?",
        Some(&FakeReranker),
    )
    .await;
    assert!(r.chosen.is_empty(), "never another teacher's passage");
    assert_eq!(r.notice, Some(Notice::NamedNone));
}

#[tokio::test]
async fn naming_two_teachers_runs_standard_mode_with_the_notice() {
    let fx = start().await;
    corpus(&fx.pool).await;
    let r = ask(
        &fx.pool,
        &detector(&fx.pool).await,
        "Ada Fixture and Bo Sample on anger",
        Some(&FakeReranker),
    )
    .await;
    assert_eq!(r.mode, Mode::Standard);
    assert_eq!(r.notice, Some(Notice::MultiNamed));
}

#[tokio::test]
async fn sangha_and_donation_fields_never_move_a_passage() {
    let fx = start().await;
    corpus(&fx.pool).await;
    let d = detector(&fx.pool).await;
    let before = ask(&fx.pool, &d, "anger in the body", None).await;

    sqlx::query(
        "update sangha set listed = not listed, donation_verified = not donation_verified,
                donation_url = 'https://example.invalid/changed', donation_kind = 'page'",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    let after = ask(&fx.pool, &d, "anger in the body", None).await;

    let order = |r: &Retrieved| r.chosen.iter().map(|c| c.id).collect::<Vec<_>>();
    assert_eq!(order(&before), order(&after));
}

#[tokio::test]
async fn a_talk_that_stops_serving_is_never_retrieved() {
    let fx = start().await;
    let planted = corpus(&fx.pool).await;
    sqlx::query("update teacher set display_level = 'excluded' where display_name = 'Ada Fixture'")
        .execute(&fx.pool)
        .await
        .unwrap();
    let ada = teacher_id(&fx.pool, "Ada Fixture").await;
    let r = ask(
        &fx.pool,
        &detector(&fx.pool).await,
        "How do I work with anger?",
        None,
    )
    .await;
    assert!(r.chosen.iter().all(|c| c.teacher_id != ada));
    assert_eq!(ids(&r).intersection(&planted).count(), 2);
}

/// Spec 04, Student questions, step 6: a query that matches only a student question finds the
/// teacher passage that answers it, the first one starting at or after the answer, and never
/// the question itself. Named mode searches only that teacher's questions.
#[tokio::test]
async fn a_query_matching_only_a_student_question_returns_the_teachers_answer() {
    let fx = start().await;
    corpus(&fx.pool).await;
    let di = talk(
        &fx.pool,
        "Di Placeholder",
        &texts(&["The garden needs weeding", "Sweeping the zendo floor"]),
    )
    .await;
    let question = "What about loneliness on long retreats?";
    let question_id: Uuid = sqlx::query_scalar(
        "insert into student_question (talk_id, transcript_id, start_ms, end_ms, answer_start_ms,
                                       text, word_count, content_hash, embedding,
                                       embedding_model, edit_model)
         select p.talk_id, tr.id, 100000, 110000, 115000, $2, 6, 'h', $3::text::vector, $4,
                'fake-editor'
         from passage p join transcript tr on tr.talk_id = p.talk_id
         where p.id = $1 returning id",
    )
    .bind(di[0].as_uuid())
    .bind(question)
    .bind(search::vector_literal(&FakeEmbedder::vector(question)))
    .bind(MODEL)
    .fetch_one(&fx.pool)
    .await
    .unwrap();

    let v = FakeEmbedder::new(MODEL)
        .embed(&["loneliness on long retreats".to_owned()])
        .await
        .unwrap()
        .remove(0);
    let mut conn = fx.pool.acquire().await.unwrap();
    assert!(
        search::full_text(&mut conn, "loneliness | long | retreats", None, 60)
            .await
            .unwrap()
            .is_empty(),
        "no passage holds the words"
    );
    let hits = search::question_vector(&mut conn, &v, None, 10)
        .await
        .unwrap();
    assert_eq!(
        hits,
        vec![di[1]],
        "the first passage at or after the answer"
    );
    let bo = teacher_id(&fx.pool, "Bo Sample").await;
    assert!(
        search::question_vector(&mut conn, &v, Some(bo), 10)
            .await
            .unwrap()
            .is_empty()
    );
    drop(conn);

    let r = ask(
        &fx.pool,
        &detector(&fx.pool).await,
        "loneliness on long retreats",
        None,
    )
    .await;
    assert_eq!(r.chosen.first().map(|c| c.id), Some(di[1]));
    assert!(
        r.chosen.iter().all(|c| c.id.as_uuid() != question_id),
        "a question is never returned"
    );
}
