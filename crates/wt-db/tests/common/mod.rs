//! Shared fixtures for the consent, takedown and audit tests. Everything here is synthetic:
//! invented teachers and sanghas, and passage text that only fills the length constraint.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::{PassageId, TalkId};
use wt_db::test_support::TestDb;
use wt_registry::RegistryFile;

/// Two teachers in one sangha. `Ada` and `Bo` are aliases.
pub const REGISTRY: &str = r#"
[[sangha]]
name = "Fixture Mountain Zendo"
donation_url = "https://example.invalid/give"

[[teacher]]
display_name = "Ada Fixture"
sangha = "Fixture Mountain Zendo"
aliases = ["Ada"]

[[teacher]]
display_name = "Bo Sample"
sangha = "Fixture Mountain Zendo"
aliases = ["Bo"]
"#;

pub const ADA: &str = "Ada Fixture";
pub const BO: &str = "Bo Sample";
pub const ACTOR: &str = "test";

/// A migrated throwaway database with the registry loaded. Keep it alive for the whole test.
pub struct Fx {
    _db: TestDb,
    pub pool: PgPool,
}

impl Fx {
    /// The superuser URL of the test database, for tests that log in as another role.
    pub fn url(&self) -> &wt_core::Secret {
        self._db.url()
    }
}

pub async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let file = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &file, ACTOR).await.unwrap();
    Fx { _db: db, pool }
}

/// A talk in `awaiting_consent`, credited to `teacher` and their sangha when given.
pub async fn talk(pool: &PgPool, teacher: Option<&str>, holder: Option<&str>) -> TalkId {
    talk_with(pool, teacher, holder, "A fixture talk on sitting", false).await
}

/// Like [`talk`], with a title and the source video's `raw.synthetic` flag.
pub async fn talk_with(
    pool: &PgPool,
    teacher: Option<&str>,
    holder: Option<&str>,
    title: &str,
    synthetic: bool,
) -> TalkId {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title, raw)
         values ('youtube', $1, 'https://example.invalid/v', $2, $3) returning id",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(title)
    .bind(serde_json::json!({ "synthetic": synthetic }))
    .fetch_one(pool)
    .await
    .unwrap();
    let id: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, $2, te.id, te.sangha_id, $4, 'awaiting_consent'
         from (select 1) one left join teacher te on te.display_name = $3
         returning id",
    )
    .bind(video)
    .bind(title)
    .bind(teacher)
    .bind(holder)
    .fetch_one(pool)
    .await
    .unwrap();
    TalkId::from(id)
}

/// Records consent through the library, which also propagates it to the teacher's talks.
pub async fn consent(pool: &PgPool, teacher: &str, status: ConsentStatus) {
    wt_db::consent::set_consent(pool, teacher, status, Some("fixture-reply"), None, ACTOR)
        .await
        .unwrap();
}

/// Moves a `cleared` talk to `ready` the way the pipeline would, writing a transcript, a
/// segment, one passage, a citation and a finished job. Returns the passage.
pub async fn make_ready(pool: &PgPool, talk: TalkId) -> PassageId {
    make_ready_with_model(pool, talk, "fixture-embed-v1").await
}

pub async fn make_ready_with_model(pool: &PgPool, talk: TalkId, model: &str) -> PassageId {
    let t = talk.as_uuid();
    assert_eq!(
        state(pool, talk).await,
        "cleared",
        "make_ready needs a cleared talk"
    );
    sqlx::query("update talk set state = 'ready', content_hash = 'fixture-hash' where id = $1")
        .bind(t)
        .execute(pool)
        .await
        .unwrap();
    let transcript: Uuid = sqlx::query_scalar(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 1, 'asr', 'transcripts/' || $1::text || '/v1.json', 'h') returning id",
    )
    .bind(t)
    .fetch_one(pool)
    .await
    .unwrap();
    let segment: Uuid = sqlx::query_scalar(
        "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
         values ($1, $2, 0, 'talk', 0, 60000, $3) returning id",
    )
    .bind(t)
    .bind(transcript)
    .bind(PASSAGE_TEXT)
    .fetch_one(pool)
    .await
    .unwrap();
    let passage: Uuid = sqlx::query_scalar(
        "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms, text,
                              word_count, content_hash, embedding, embedding_model)
         values ($1, $2, 'talk', 0, 0, 60000, $3, 20, 'h',
                 array_fill(0.01::real, array[1024])::vector, $4)
         returning id",
    )
    .bind(t)
    .bind(segment)
    .bind(PASSAGE_TEXT)
    .bind(model)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into citation (passage_id, talk_id, raw_text, confidence, method)
         values ($1, $2, 'the fixture sutra', 'unresolved', 'hand_list')",
    )
    .bind(passage)
    .bind(t)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into job (kind, talk_id, idempotency_key, state, finished_at)
         values ('index', $1, 'index:' || $1::text, 'succeeded', now())",
    )
    .bind(t)
    .execute(pool)
    .await
    .unwrap();
    PassageId::from(passage)
}

/// Synthetic passage text, long enough for the passage length check.
pub const PASSAGE_TEXT: &str =
    "Fixture words about sitting still and breathing, written only to fill the passage length.";

/// Stores an answer whose payload has one theme per passage and lists every passage, with
/// matching `answer_passage` rows. Returns the answer id.
pub async fn answer(pool: &PgPool, passages: &[PassageId]) -> Uuid {
    let tester: Uuid = sqlx::query_scalar(
        "insert into tester (label, token_hash) values ('fixture', decode(md5(random()::text), 'hex'))
         returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let log: Uuid = sqlx::query_scalar(
        "insert into query_log (tester_id, query_text, mode, teacher_count, sangha_count,
                                passage_count, latency_ms)
         values ($1, 'fixture question', 'standard', 2, 1, $2, 10) returning id",
    )
    .bind(tester)
    .bind(i32::try_from(passages.len()).unwrap())
    .fetch_one(pool)
    .await
    .unwrap();
    let themes: Vec<_> = passages
        .iter()
        .enumerate()
        .map(
            |(i, p)| serde_json::json!({ "description": format!("theme {i}"), "passage_ids": [p] }),
        )
        .collect();
    let payload =
        serde_json::json!({ "mode": "standard", "themes": themes, "passage_ids": passages });
    let answer: Uuid = sqlx::query_scalar(
        "insert into answer (query_log_id, payload) values ($1, $2) returning id",
    )
    .bind(log)
    .bind(payload)
    .fetch_one(pool)
    .await
    .unwrap();
    for (rank, p) in passages.iter().enumerate() {
        sqlx::query("insert into answer_passage (answer_id, passage_id, rank) values ($1, $2, $3)")
            .bind(answer)
            .bind(p.as_uuid())
            .bind(i32::try_from(rank).unwrap())
            .execute(pool)
            .await
            .unwrap();
    }
    answer
}

/// Adds an evaluation rating with a note for each passage.
pub async fn ratings(pool: &PgPool, passages: &[PassageId]) {
    let set: Uuid = sqlx::query_scalar(
        "insert into eval_set (name, version) values ('fixture', 1) returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let question: Uuid = sqlx::query_scalar(
        "insert into eval_question (set_id, ordinal, text) values ($1, 1, 'fixture question')
         returning id",
    )
    .bind(set)
    .fetch_one(pool)
    .await
    .unwrap();
    let run: Uuid = sqlx::query_scalar(
        "insert into eval_run (set_id, build, config) values ($1, 'test', '{}') returning id",
    )
    .bind(set)
    .fetch_one(pool)
    .await
    .unwrap();
    for p in passages {
        sqlx::query(
            "insert into eval_rating (run_id, question_id, passage_id, reviewer, faithful,
                                      relevant, note)
             values ($1, $2, $3, 'reviewer', true, true, 'a reviewer note')",
        )
        .bind(run)
        .bind(question)
        .bind(p.as_uuid())
        .execute(pool)
        .await
        .unwrap();
    }
}

pub async fn state(pool: &PgPool, talk: TalkId) -> String {
    sqlx::query_scalar("select state::text from talk where id = $1")
        .bind(talk.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

pub async fn state_reason(pool: &PgPool, talk: TalkId) -> Option<String> {
    sqlx::query_scalar("select state_reason from talk where id = $1")
        .bind(talk.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Runs a `select count(*) ...` with one uuid parameter.
pub async fn count(pool: &PgPool, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Rows in `servable_passage`.
pub async fn servable(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from servable_passage")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The latest consent status recorded for a teacher.
pub async fn current_consent(pool: &PgPool, teacher: &str) -> Option<String> {
    sqlx::query_scalar(
        "select cc.status::text from current_consent cc join teacher te on te.id = cc.teacher_id
         where te.display_name = $1",
    )
    .bind(teacher)
    .fetch_optional(pool)
    .await
    .unwrap()
}
