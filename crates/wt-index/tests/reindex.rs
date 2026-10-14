//! Reindexing keeps the ids of unchanged passages (Spec 04, Chunking, embedding and indexing,
//! decision of October 12, 2026), against a real Postgres. All text is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::transcript::Word;
use wt_db::test_support::TestDb;
use wt_index::ChunkSettings;
use wt_index::index::{self, IndexCitation, IndexSegment, Plan, Reuse};

const MODEL: &str = "fake-embed";
const DIM: usize = 1024;

const A: &str = "The first stretch of invented words is about sitting still on a cushion at dawn.";
const B: &str = "The second stretch of invented words is about walking slowly around the hall.";
const C: &str = "The third stretch of invented words is about eating a quiet meal from bowls.";
const C2: &str = "The third stretch of invented words is about washing the bowls after the meal.";

struct Fx {
    _db: TestDb,
    pool: PgPool,
    talk: Uuid,
    /// One segment per stretch of text.
    segments: Vec<Uuid>,
}

async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "with v as (insert into source_video (platform, external_id, url, title, raw)
                    values ('youtube', 'fixture', 'https://example.invalid/v', 'Fixture', '{}')
                    returning id)
         insert into talk (source_video_id, title, state)
         select id, 'Fixture', 'awaiting_consent' from v
         returning id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let transcript: Uuid = sqlx::query_scalar(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 1, 'asr', 'transcripts/fixture.json', 'h') returning id",
    )
    .bind(talk)
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut segments = Vec::new();
    for (i, text) in [A, B, C].iter().enumerate() {
        let start = i32::try_from(i).unwrap() * 60_000;
        let id: Uuid = sqlx::query_scalar(
            "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
             values ($1, $2, $3, 'talk', $4, $5, $6) returning id",
        )
        .bind(talk)
        .bind(transcript)
        .bind(i32::try_from(i).unwrap())
        .bind(start)
        .bind(start + 50_000)
        .bind(*text)
        .fetch_one(&pool)
        .await
        .unwrap();
        segments.push(id);
    }
    Fx {
        _db: db,
        pool,
        talk,
        segments,
    }
}

/// Segment `i` holding `text`, its words numbered from `i * 100` in the stored transcript.
fn seg(fx: &Fx, i: usize, text: &str) -> IndexSegment {
    let base = i32::try_from(i).unwrap() * 60_000;
    IndexSegment {
        id: fx.segments[i],
        kind: "talk".into(),
        words: text
            .split_whitespace()
            .enumerate()
            .map(|(j, w)| {
                let ms = base + i32::try_from(j).unwrap() * 300;
                (
                    Word::new(w, ms, ms + 250, 0.9),
                    (i * 100 + j, i * 100 + j + 1),
                )
            })
            .collect(),
    }
}

/// A citation on the first words of segment `i`.
fn cite(i: usize) -> IndexCitation {
    IndexCitation {
        w0: i * 100,
        w1: i * 100 + 3,
        confidence: "unresolved".into(),
        method: "llm".into(),
        work_locator_id: None,
    }
}

/// Plans, matches, embeds with a fake and writes, as the index job does. Returns the match and
/// how many texts were embedded.
async fn index(
    fx: &Fx,
    segments: &[IndexSegment],
    citations: &[IndexCitation],
    model: &str,
) -> (Plan, Reuse, usize) {
    let plan = index::plan(segments, citations, &ChunkSettings::default(), model).unwrap();
    let mut conn = fx.pool.acquire().await.unwrap();
    let reuse = index::reuse(&mut conn, fx.talk, &plan, model, 2)
        .await
        .unwrap();
    let texts = plan.texts(&reuse);
    let vectors: Vec<Vec<f32>> = texts.iter().map(|_| vec![0.01; DIM]).collect();
    let mut tx = sqlx::Acquire::begin(&mut *conn).await.unwrap();
    index::write(
        &mut tx,
        fx.talk,
        &plan,
        &reuse,
        &index::Embedded {
            vectors: &vectors,
            model_id: model,
            vector_version: 2,
            dim: DIM,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (plan, reuse, texts.len())
}

/// The talk's passages in order: id, segment, text.
async fn passages(fx: &Fx) -> Vec<(Uuid, Uuid, String)> {
    sqlx::query_as("select id, segment_id, text from passage where talk_id = $1 order by ordinal")
        .bind(fx.talk)
        .fetch_all(&fx.pool)
        .await
        .unwrap()
}

/// An answer naming `passages`, with its `answer_passage` rows.
async fn answer(fx: &Fx, passages: &[Uuid]) -> Uuid {
    let tester: Uuid = sqlx::query_scalar(
        "insert into tester (label, token_hash) values ('fixture', decode(md5('t'), 'hex'))
         returning id",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let answer: Uuid = sqlx::query_scalar(
        "with l as (insert into query_log (tester_id, query_text, mode, teacher_count,
                                           sangha_count, passage_count, latency_ms)
                    values ($1, 'fixture question', 'standard', 1, 1, 3, 10) returning id)
         insert into answer (query_log_id, payload) select id, '{}' from l returning id",
    )
    .bind(tester)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    for (rank, p) in passages.iter().enumerate() {
        sqlx::query("insert into answer_passage (answer_id, passage_id, rank) values ($1, $2, $3)")
            .bind(answer)
            .bind(p)
            .bind(i32::try_from(rank).unwrap())
            .execute(&fx.pool)
            .await
            .unwrap();
    }
    answer
}

async fn answer_rows(fx: &Fx, answer: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar("select passage_id from answer_passage where answer_id = $1 order by rank")
        .bind(answer)
        .fetch_all(&fx.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_unchanged_reindex_keeps_every_passage_and_answer_row() {
    let fx = start().await;
    let segments = [seg(&fx, 0, A), seg(&fx, 1, B), seg(&fx, 2, C)];
    let (_, _, embedded) = index(&fx, &segments, &[cite(1)], MODEL).await;
    assert_eq!(embedded, 3);
    let before = passages(&fx).await;
    assert_eq!(before.len(), 3);
    let ids: Vec<Uuid> = before.iter().map(|p| p.0).collect();
    let answer = answer(&fx, &ids).await;

    // Citations change (resolution ran again), the passages do not.
    let (_, reuse, embedded) = index(&fx, &segments, &[cite(1), cite(2)], MODEL).await;
    assert_eq!(embedded, 0, "current vectors are kept");
    assert!(reuse.keep_vector.iter().all(|k| *k));
    assert_eq!(passages(&fx).await, before);
    assert_eq!(answer_rows(&fx, answer).await, ids);
    let cited: Vec<Uuid> = sqlx::query_scalar(
        "select passage_id from citation where talk_id = $1 order by span_start, passage_id",
    )
    .bind(fx.talk)
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    let mut want = vec![ids[1], ids[2]];
    want.sort();
    let mut got = cited.clone();
    got.sort();
    assert_eq!(got, want, "citations are re-linked to the kept passages");

    // A new embedding model keeps the ids and replaces every vector.
    let (_, _, embedded) = index(&fx, &segments, &[], "fake-embed-2").await;
    assert_eq!(embedded, 3);
    assert_eq!(passages(&fx).await, before);
    let models: Vec<String> =
        sqlx::query_scalar("select distinct embedding_model from passage where talk_id = $1")
            .bind(fx.talk)
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(models, ["fake-embed-2"]);
    assert_eq!(answer_rows(&fx, answer).await, ids);
}

#[tokio::test]
async fn a_changed_chunk_is_new_and_a_removed_one_is_deleted() {
    let fx = start().await;
    index(
        &fx,
        &[seg(&fx, 0, A), seg(&fx, 1, B), seg(&fx, 2, C)],
        &[],
        MODEL,
    )
    .await;
    let before = passages(&fx).await;
    let ids: Vec<Uuid> = before.iter().map(|p| p.0).collect();
    let answer = answer(&fx, &ids).await;

    // The first stretch is gone and the third is corrected; the second moves to ordinal 0.
    let (_, _, embedded) = index(&fx, &[seg(&fx, 1, B), seg(&fx, 2, C2)], &[], MODEL).await;
    assert_eq!(embedded, 1, "only the changed chunk is embedded");
    let after = passages(&fx).await;
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], before[1], "the unchanged passage keeps its id");
    assert_eq!(after[1].1, fx.segments[2]);
    assert_eq!(after[1].2, C2);
    assert!(!ids.contains(&after[1].0), "the changed chunk is a new row");
    let gone: i64 = sqlx::query_scalar("select count(*) from passage where id = any($1)")
        .bind(vec![ids[0], ids[2]])
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(gone, 0, "the removed and the replaced passages are deleted");
    assert_eq!(answer_rows(&fx, answer).await, [ids[1]]);
}

#[tokio::test]
async fn a_passage_gone_since_matching_fails_the_write() {
    let fx = start().await;
    let segments = [seg(&fx, 0, A), seg(&fx, 1, B)];
    index(&fx, &segments, &[], MODEL).await;
    let plan = index::plan(&segments, &[], &ChunkSettings::default(), MODEL).unwrap();
    let mut conn = fx.pool.acquire().await.unwrap();
    let reuse = index::reuse(&mut conn, fx.talk, &plan, MODEL, 2)
        .await
        .unwrap();
    // A takedown or a new transcript removes the passages while the talk is being embedded.
    sqlx::query("delete from passage where talk_id = $1")
        .bind(fx.talk)
        .execute(&fx.pool)
        .await
        .unwrap();
    let mut tx = sqlx::Acquire::begin(&mut *conn).await.unwrap();
    let err = index::write(
        &mut tx,
        fx.talk,
        &plan,
        &reuse,
        &index::Embedded {
            vectors: &[],
            model_id: MODEL,
            vector_version: 2,
            dim: DIM,
        },
    )
    .await;
    assert!(matches!(err, Err(index::IndexError::Changed(_))), "{err:?}");
}

#[tokio::test]
async fn a_reindex_fills_sentence_boundaries_and_keeps_ids() {
    let fx = start().await;
    let segments = [seg(&fx, 0, A), seg(&fx, 1, B), seg(&fx, 2, C)];
    index(&fx, &segments, &[], MODEL).await;
    let before = passages(&fx).await;
    // As a talk indexed before boundaries were stored.
    sqlx::query(
        "update passage set sentence_starts = '{}', sentence_start_ms = '{}' where talk_id = $1",
    )
    .bind(fx.talk)
    .execute(&fx.pool)
    .await
    .unwrap();
    let (plan, _, embedded) = index(&fx, &segments, &[], MODEL).await;
    assert_eq!(embedded, 0);
    assert_eq!(passages(&fx).await, before, "same ids, same words");
    let stored: Vec<(Vec<i32>, Vec<i32>)> = sqlx::query_as(
        "select sentence_starts, sentence_start_ms from passage where talk_id = $1
         order by ordinal",
    )
    .bind(fx.talk)
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    let planned: Vec<(Vec<i32>, Vec<i32>)> = plan
        .passages
        .iter()
        .map(|p| {
            (
                p.passage.sentence_starts.clone(),
                p.passage.sentence_start_ms.clone(),
            )
        })
        .collect();
    assert_eq!(stored, planned);
    assert!(stored.iter().all(|(s, _)| s.first() == Some(&0)));
}
