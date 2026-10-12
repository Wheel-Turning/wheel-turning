//! Spec 05, Answer assembly, rule 8: answer excerpts on synthetic passages in a real Postgres.
//! One talk has a segment of three passages, the middle one starting and ending mid-sentence,
//! and a second segment after it. The model is a `FakeLlm`, the reranker a counting fake.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::json;
use sqlx::PgPool;
use sqlx::types::Uuid;
use tower::ServiceExt;
use wt_api::{AppState, Site, router, session};
use wt_core::excerpt::{Chooser, Excerpt};
use wt_core::gate::ConsentStatus;
use wt_core::ids::PassageId;
use wt_db::test_support::TestDb;
use wt_extract::{DataClass, FakeLlm};
use wt_ml::{FakeEmbedder, MlError, Reranker};
use wt_registry::RegistryFile;

const REGISTRY: &str = r#"
[[sangha]]
name = "North Fixture Zendo"
donation_url = "https://example.invalid/north"

[[teacher]]
display_name = "Ada Fixture"
sangha = "North Fixture Zendo"
"#;

const MODEL: &str = "fake-embed";
const ACTOR: &str = "test";

// Sentences, numbered as the model sees the middle passage's window:
// 1 "We begin ... settle." 2 "Before we start ... hall." 3 "When the mind wanders ... at all."
// (from P1 into P2) 4 "The lantern stays lit ... beginner." 5 "So keep the lantern in mind."
// 6 "And then later ... quietly." (from P2 into P3) 7 "Nothing more is needed tonight."
const P1: &str = "We begin by sitting down on the cushion and letting the body settle. Before \
                  we start there is one thing to say about the old lantern in the hall. When \
                  the mind wanders far from the breath we";
const P2: &str = "come back gently and without any blame at all. The lantern stays lit even \
                  when we forget it is there, and that is the whole of practice for a beginner. \
                  So keep the lantern in mind. And then later in the";
const P3: &str = "evening we chant together and go home quietly. Nothing more is needed tonight.";
const P4: &str = "A different turn begins here with a question about the lantern and the dark.";

struct Fx {
    _db: TestDb,
    pool: PgPool,
    /// The passages in talk order; the fourth is in the second segment.
    ids: Vec<PassageId>,
}

async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let file = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &file, ACTOR).await.unwrap();
    let ids = talk(&pool).await;
    Fx { _db: db, pool, ids }
}

/// A passage to insert: segment, text, start, end, sentence starts and their times.
type Row = (
    usize,
    &'static str,
    i32,
    i32,
    &'static [i32],
    &'static [i32],
);

async fn talk(pool: &PgPool) -> Vec<PassageId> {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('youtube', 'vid1', 'https://example.invalid/vid1', 'Fixture talk') returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, 'On the lantern', te.id, te.sangha_id, te.display_name, 'awaiting_consent'
         from teacher te where te.display_name = 'Ada Fixture' returning id",
    )
    .bind(video)
    .fetch_one(pool)
    .await
    .unwrap();
    wt_db::consent::set_consent(
        pool,
        "Ada Fixture",
        ConsentStatus::Granted,
        Some("f"),
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
         values ($1, 1, 'asr', 'k', 'h') returning id",
    )
    .bind(talk)
    .fetch_one(pool)
    .await
    .unwrap();
    let mut segments = Vec::new();
    for (ordinal, (from, to)) in [(0, 40_000), (60_000, 70_000)].into_iter().enumerate() {
        let seg: Uuid = sqlx::query_scalar(
            "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
             values ($1, $2, $3, 'talk', $4, $5, 'fixture') returning id",
        )
        .bind(talk)
        .bind(transcript)
        .bind(i32::try_from(ordinal).unwrap())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        .unwrap();
        segments.push(seg);
    }
    let rows: [Row; 4] = [
        (0, P1, 0, 14_000, &[0, 13, 29], &[0, 5_000, 11_000]),
        (
            0,
            P2,
            15_000,
            31_000,
            &[9, 30, 36],
            &[18_000, 26_000, 29_000],
        ),
        (0, P3, 32_000, 37_000, &[8], &[35_000]),
        (1, P4, 60_000, 66_000, &[0], &[60_000]),
    ];
    let mut ids = Vec::new();
    for (ordinal, (seg, text, start, end, starts, ms)) in rows.into_iter().enumerate() {
        let v = wt_retrieve::search::vector_literal(&FakeEmbedder::vector(text));
        let id: Uuid = sqlx::query_scalar(
            "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms,
                                  text, word_count, content_hash, embedding, embedding_model,
                                  sentence_starts, sentence_start_ms)
             values ($1, $2, 'talk', $3, $4, $5, $6, 40, 'h', $7::text::vector, $8, $9, $10)
             returning id",
        )
        .bind(talk)
        .bind(segments[seg])
        .bind(i32::try_from(ordinal).unwrap())
        .bind(start)
        .bind(end)
        .bind(text)
        .bind(v)
        .bind(MODEL)
        .bind(starts)
        .bind(ms)
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push(PassageId::from(id));
    }
    ids
}

/// Scores a text by how often it says "lantern", or fails.
struct Counting {
    fail: bool,
}

#[async_trait]
impl Reranker for Counting {
    fn model_id(&self) -> &str {
        "counting"
    }
    async fn score(&self, _q: &str, passages: &[String]) -> Result<Vec<f32>, MlError> {
        if self.fail {
            return Err(MlError::Output("down".into()));
        }
        Ok(passages
            .iter()
            .map(|p| p.matches("lantern").count() as f32 * 3.0 - 2.0)
            .collect())
    }
}

fn site(replies: Vec<serde_json::Value>, reranker: Option<Counting>, real: bool) -> Site {
    Site {
        llm: Arc::new(FakeLlm::new(replies.into_iter().map(Ok).collect())),
        reranker: reranker.map(|r| Arc::new(r) as Arc<dyn Reranker>),
        real_data_ok: real,
        ..Site::with_fakes(MODEL)
    }
}

/// The middle passage's excerpt, chosen as an answer would.
async fn excerpt_of_p2(fx: &Fx, site: &Site) -> Excerpt {
    let ids = [fx.ids[1]];
    let served = wt_db::answer::passages(&fx.pool, &ids).await.unwrap();
    let windows = wt_db::answer::windows(&fx.pool, &ids).await.unwrap();
    let mut out = wt_api::excerpt::choose(site, "What is the lantern?", &served, &windows).await;
    out.remove(&fx.ids[1]).unwrap()
}

#[tokio::test]
async fn windows_hold_same_segment_neighbors_only() {
    let fx = start().await;
    let windows = wt_db::answer::windows(&fx.pool, &[fx.ids[1], fx.ids[2], fx.ids[3]])
        .await
        .unwrap();
    let ids = |i: usize| -> Vec<PassageId> { windows[i].passages.iter().map(|p| p.id).collect() };
    assert_eq!(ids(0), fx.ids[0..3].to_vec());
    assert_eq!(
        ids(1),
        fx.ids[1..3].to_vec(),
        "the next segment is left out"
    );
    assert_eq!(ids(2), vec![fx.ids[3]]);
}

#[tokio::test]
async fn the_model_can_reach_into_the_passage_before_on_sentence_boundaries() {
    let fx = start().await;
    let llm_site = site(vec![json!({"start": 3, "end": 4})], None, true);
    let e = excerpt_of_p2(&fx, &llm_site).await;
    let span = e.span.clone().unwrap();
    assert_eq!(span.chooser, Chooser::Model);
    assert_eq!(span.covers, vec![fx.ids[0], fx.ids[1]]);
    assert!(
        e.text
            .starts_with("When the mind wanders far from the breath we come back")
    );
    assert!(e.text.ends_with("practice for a beginner."));
    assert!(e.words <= wt_core::excerpt::MAX_EXCERPT_WORDS);
    assert_eq!(
        e.start_ms, 11_000,
        "the excerpt's first word, in the passage before"
    );
}

#[tokio::test]
async fn a_reply_outside_the_window_falls_back_to_the_reranker() {
    let fx = start().await;
    let s = site(
        vec![json!({"start": 1, "end": 9})],
        Some(Counting { fail: false }),
        true,
    );
    let e = excerpt_of_p2(&fx, &s).await;
    assert_eq!(e.span.as_ref().unwrap().chooser, Chooser::Reranker);
    assert_eq!(
        e.text,
        "The lantern stays lit even when we forget it is there, and that is the whole of \
         practice for a beginner. So keep the lantern in mind."
    );
}

#[tokio::test]
async fn a_reply_under_the_limit_and_a_failing_reranker_leave_the_trimmed_passage() {
    let fx = start().await;
    // Sentence 5 alone is six words, under excerpt_min_words.
    let s = site(
        vec![json!({"start": 5, "end": 5})],
        Some(Counting { fail: true }),
        true,
    );
    let e = excerpt_of_p2(&fx, &s).await;
    let span = e.span.clone().unwrap();
    assert_eq!(span.chooser, Chooser::Passage);
    assert_eq!(span.covers, vec![fx.ids[1]]);
    assert_eq!((span.start, span.end), (9, 36));
    assert!(e.text.starts_with("The lantern stays lit"));
    assert!(e.text.ends_with("keep the lantern in mind."));
}

#[tokio::test]
async fn without_real_data_ok_no_text_goes_to_the_model() {
    let fx = start().await;
    let llm = Arc::new(FakeLlm::new(vec![Ok(json!({"start": 3, "end": 4}))]));
    let s = Site {
        llm: llm.clone(),
        real_data_ok: false,
        ..Site::with_fakes(MODEL)
    };
    let e = excerpt_of_p2(&fx, &s).await;
    assert_eq!(e.span.unwrap().chooser, Chooser::Passage);
    assert!(llm.seen.lock().unwrap().is_empty());
    // With it on, the call is marked as real text.
    let llm = Arc::new(FakeLlm::new(vec![Ok(json!({"start": 3, "end": 4}))]));
    let s = Site {
        llm: llm.clone(),
        real_data_ok: true,
        ..Site::with_fakes(MODEL)
    };
    excerpt_of_p2(&fx, &s).await;
    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1, DataClass::Real);
    assert!(
        seen[0]
            .0
            .user
            .contains("[3] (18 words) When the mind wanders")
    );
    assert!(
        !seen[0].0.user.contains("A different turn"),
        "no other segment"
    );
}

// ---- the page --------------------------------------------------------------------------------

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn get(path: &str, cookie: &str) -> Request<Body> {
    Request::get(path)
        .header(header::COOKIE, format!("wt_session={cookie}"))
        .body(Body::empty())
        .unwrap()
}

async fn ask(fx: &Fx, app: &Router, question: &str) -> (String, String) {
    let token = session::new_token().unwrap();
    wt_db::tester::invite(&fx.pool, "owner", &session::token_hash(&token))
        .await
        .unwrap();
    let (_, headers, _) = send(
        app,
        Request::get(format!("/join/{token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("wt_session=")
        .to_owned();
    let (_, _, home) = send(app, get("/", &cookie)).await;
    let at = home.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
    let csrf = home[at..].split('"').next().unwrap().to_owned();
    let (status, headers, body) = send(
        app,
        Request::post("/ask")
            .header(header::COOKIE, format!("wt_session={cookie}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "q={}&csrf={csrf}",
                question.replace(' ', "+")
            )))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let permalink = headers[header::LOCATION].to_str().unwrap().to_owned();
    (cookie, permalink)
}

#[tokio::test]
async fn the_page_shows_the_excerpt_rebuilt_from_serving_rows_and_listens_at_its_first_word() {
    let fx = start().await;
    let app = router(AppState {
        pool: fx.pool.clone(),
        site: Arc::new(site(vec![json!({"start": 3, "end": 4})], None, true)),
    });
    // Only the middle passage says "beginner", and the others fall within a minute of it.
    let (cookie, permalink) = ask(&fx, &app, "practice for a beginner").await;
    let (status, _, page) = send(&app, get(&permalink, &cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("When the mind wanders far from the breath we come back gently"));
    assert!(page.contains("practice for a beginner.</blockquote>"));
    assert!(
        !page.contains("We begin by sitting"),
        "the sentence before is not shown"
    );
    assert!(!page.contains("And then later"), "nor the fragment after");
    // 11 s into the talk, less the 2 s lead-in.
    assert!(page.contains("watch?v=vid1&#38;t=9s"), "{page}");
    assert!(page.contains("Listen at 0:11"));
    assert!(page.contains("/transcript#p0"));

    // The payload stores the span, never the text.
    let payload: serde_json::Value = sqlx::query_scalar("select payload from answer")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let span = &payload["spans"][0];
    assert_eq!(span["chooser"], "model");
    assert_eq!(span["start"], 29);
    assert_eq!(span["end"], 30);
    assert!(!payload.to_string().contains("lantern stays lit"));

    // A covered neighbor that stops serving: the excerpt falls back to its own passage.
    sqlx::query("delete from passage where id = $1")
        .bind(fx.ids[0].as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    let (_, _, page) = send(&app, get(&permalink, &cookie)).await;
    assert!(page.contains("come back gently and without any blame at all."));
    assert!(page.contains("And then later in the</blockquote>"));
    assert!(page.contains("Listen at 0:15"));
}
