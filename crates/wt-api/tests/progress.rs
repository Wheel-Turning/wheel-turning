//! Spec 06, Live progress while a question runs: a streamed ask reports its stages and counts and
//! closes with the permalink or the page a plain post would have shown, and stores what a plain
//! post stores. Also the answer page's "Return to the top". Synthetic teachers and passages only.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::PgPool;
use sqlx::types::Uuid;
use tower::ServiceExt;
use wt_api::{AppState, Site, router, session};
use wt_core::gate::ConsentStatus;
use wt_db::test_support::TestDb;
use wt_ml::FakeEmbedder;
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
const ANGER: &str = "When anger arises, turn toward the anger and breathe with it. The bell rang \
                     twice before the service, and the room was quiet.";
const QUESTION: &str = "How do I work with anger?";

struct Fx {
    _db: TestDb,
    pool: PgPool,
    app: Router,
}

async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let file = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &file, ACTOR).await.unwrap();
    passage(&pool).await;
    let app = router(AppState {
        pool: pool.clone(),
        site: Arc::new(Site::with_fakes(MODEL)),
    });
    Fx { _db: db, pool, app }
}

/// A ready, consented talk by Ada with one passage.
async fn passage(pool: &PgPool) {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('youtube', 'vid1', 'https://example.invalid/vid1', 'Fixture talk') returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, 'On anger', te.id, te.sangha_id, te.display_name, 'awaiting_consent'
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
    let segment: Uuid = sqlx::query_scalar(
        "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
         values ($1, $2, 0, 'talk', 65000, 125000, $3) returning id",
    )
    .bind(talk)
    .bind(transcript)
    .bind(ANGER)
    .fetch_one(pool)
    .await
    .unwrap();
    let v = wt_retrieve::search::vector_literal(&FakeEmbedder::vector(ANGER));
    sqlx::query(
        "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms, text,
                              word_count, content_hash, embedding, embedding_model)
         values ($1, $2, 'talk', 0, 65000, 125000, $3, 20, 'h', $4::text::vector, $5)",
    )
    .bind(talk)
    .bind(segment)
    .bind(ANGER)
    .bind(v)
    .bind(MODEL)
    .execute(pool)
    .await
    .unwrap();
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn send(app: &Router, req: Request<Body>) -> Reply {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn get(path: &str, cookie: &str) -> Request<Body> {
    Request::get(path)
        .header(header::COOKIE, format!("wt_session={cookie}"))
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, cookie: &str, form: &str, stream: bool) -> Request<Body> {
    let mut r = Request::post(path)
        .header(header::COOKIE, format!("wt_session={cookie}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if stream {
        r = r.header(header::ACCEPT, "text/event-stream");
    }
    r.body(Body::from(form.to_owned())).unwrap()
}

async fn join(fx: &Fx) -> String {
    let token = session::new_token().unwrap();
    wt_db::tester::invite(&fx.pool, "owner", &session::token_hash(&token))
        .await
        .unwrap();
    let r = send(
        &fx.app,
        Request::get(format!("/join/{token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let set = r.headers[header::SET_COOKIE].to_str().unwrap().to_owned();
    set.split(';')
        .next()
        .unwrap()
        .trim_start_matches("wt_session=")
        .to_owned()
}

fn csrf(body: &str) -> String {
    let at = body.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
    body[at..].split('"').next().unwrap().to_owned()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            b' ' => "+".to_owned(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The stream's events as (name, data) pairs, keep-alive comments left out.
fn events(body: &str) -> Vec<(String, serde_json::Value)> {
    body.split("\n\n")
        .filter_map(|block| {
            let mut name = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(n) = line.strip_prefix("event:") {
                    name = Some(n.trim().to_owned());
                } else if let Some(d) = line.strip_prefix("data:") {
                    data.push_str(d.strip_prefix(' ').unwrap_or(d));
                }
            }
            Some((name?, serde_json::from_str(&data).unwrap()))
        })
        .collect()
}

async fn ask(fx: &Fx, cookie: &str, question: &str, stream: bool) -> Reply {
    let token = csrf(&send(&fx.app, get("/", cookie)).await.body);
    send(
        &fx.app,
        post(
            "/ask",
            cookie,
            &format!("q={}&csrf={}", encode(question), encode(&token)),
            stream,
        ),
    )
    .await
}

#[tokio::test]
async fn a_streamed_ask_reports_each_stage_and_closes_with_the_permalink() {
    let fx = start().await;
    let cookie = join(&fx).await;
    let r = ask(&fx, &cookie, QUESTION, true).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(
        r.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert_eq!(r.headers[header::CACHE_CONTROL], "no-store");

    let events = events(&r.body);
    // The passages-searched count is read alongside the path and may land anywhere before the
    // close; it counts the one servable passage.
    for (_, data) in events.iter().filter(|(_, d)| d["name"] == "searched") {
        assert_eq!(data["value"], 1);
    }
    let ordered: Vec<String> = events
        .iter()
        .filter(|(_, d)| d["name"] != "searched")
        .map(|(name, d)| match name.as_str() {
            "stage" => format!("stage {}", d["stage"].as_str().unwrap()),
            "count" => format!("{} {}", d["name"].as_str().unwrap(), d["value"]),
            other => other.to_owned(),
        })
        .collect();
    assert_eq!(
        ordered,
        [
            "stage reading",
            "stage searching",
            "candidates 1",
            "stage weighing",
            "shortlisted 1",
            "stage choosing",
            "selected 1",
            // The summary is written after the answer is stored, so the stream closes first
            // (owner's request of October 15, 2026).
            "done",
        ],
        "{}",
        r.body
    );
    let url = events.last().unwrap().1["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("/a/"), "{url}");
    let page = send(&fx.app, get(&url, &cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<blockquote>"));
}

#[tokio::test]
async fn a_streamed_ask_stores_what_a_plain_post_stores() {
    let fx = start().await;
    let cookie = join(&fx).await;
    let plain = ask(&fx, &cookie, QUESTION, false).await;
    assert_eq!(plain.status, StatusCode::SEE_OTHER, "{}", plain.body);
    let streamed = ask(&fx, &cookie, QUESTION, true).await;
    assert!(streamed.body.contains("event: done"), "{}", streamed.body);

    // Each answer's summary is written in the background after it is stored, so wait for both
    // to settle before comparing them.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let pending: i64 = sqlx::query_scalar(
            "select count(*) from answer
             where coalesce((payload ->> 'summary_pending')::boolean, false)",
        )
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        if pending == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a deferred summary never settled"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let rows: Vec<(serde_json::Value, i32, i32, Option<String>)> = sqlx::query_as(
        "select a.payload, q.teacher_count, q.passage_count, q.query_text
         from answer a join query_log q on q.id = a.query_log_id order by q.created_at",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "one answer and one log row each");
    assert_eq!(rows[0], rows[1]);

    // Spec 11 PB4.6: both are timed to the page, and only the streamed one to its first event.
    let times: Vec<(Option<i32>, Option<i32>, Option<f64>)> = sqlx::query_as(
        "select first_progress_ms, page_ms, llm_cost_usd::float8 from query_log
         order by created_at",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert!(times[0].0.is_none(), "a plain post sends no progress event");
    assert!(
        times[1].0.is_some(),
        "the streamed ask is timed to its first event"
    );
    assert!(times.iter().all(|(_, page, _)| page.is_some()));
    assert!(times.iter().all(|(first, page, _)| first <= page));
    assert!(
        times.iter().all(|(_, _, cost)| cost.is_some()),
        "every answer records its model cost, zero without a priced model"
    );
}

#[tokio::test]
async fn a_streamed_ask_that_is_refused_closes_with_the_plain_page() {
    let fx = start().await;
    let cookie = join(&fx).await;

    let short = ask(&fx, &cookie, "hi", true).await;
    let events = events(&short.body);
    assert_eq!(events.len(), 1, "{}", short.body);
    assert_eq!(events[0].0, "page");
    let html = events[0].1["html"].as_str().unwrap();
    assert!(html.contains("Please write between 3 and 500 characters."));

    let forged = send(
        &fx.app,
        post("/ask", &cookie, "q=How+do+I+sit&csrf=forged", true),
    )
    .await;
    let html = events_html(&forged.body);
    assert!(html.contains("This form expired"), "{html}");
    let stored: i64 = sqlx::query_scalar("select count(*) from query_log")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(stored, 0, "nothing was asked");
}

fn events_html(body: &str) -> String {
    let events = events(body);
    let (name, data) = events.last().unwrap();
    assert_eq!(name, "page");
    data["html"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn the_progress_copy_and_script_are_served_and_every_ask_form_opts_in() {
    let fx = start().await;
    let cookie = join(&fx).await;
    let home = send(&fx.app, get("/", &cookie)).await;
    assert!(home.body.contains("<template id=\"progress-template\">"));
    assert!(
        home.body
            .contains("<script src=\"/static/progress.js\" defer></script>")
    );
    for stage in [
        "Reading the question",
        "Searching the talks",
        "Weighing excerpts",
        "Choosing excerpts",
        "Writing the summary",
    ] {
        assert!(home.body.contains(stage), "{stage}");
    }
    let ask_forms = home.body.matches("action=\"/ask\"").count();
    assert!(ask_forms >= 4, "the box, the menu panel and the examples");
    assert_eq!(
        home.body
            .matches("action=\"/ask\" class=\"ask\" data-progress")
            .count()
            + home
                .body
                .matches("class=\"panel-ask\" role=\"search\" data-progress")
                .count()
            + home.body.matches("action=\"/ask\" data-progress").count(),
        ask_forms
    );

    let script = send(&fx.app, get("/static/progress.js", &cookie)).await;
    assert_eq!(script.status, StatusCode::OK);
    assert!(script.body.contains("text/event-stream"));
}

#[tokio::test]
async fn the_answer_page_ends_with_return_to_the_top() {
    let fx = start().await;
    let cookie = join(&fx).await;
    let asked = ask(&fx, &cookie, QUESTION, false).await;
    let url = asked.headers[header::LOCATION].to_str().unwrap().to_owned();
    let page = send(&fx.app, get(&url, &cookie)).await;
    assert!(page.body.contains("<body id=\"top\">"));
    let link = page
        .body
        .find("<a href=\"#top\">Return to the top</a>")
        .expect("the link");
    assert!(
        link > page.body.find("Flag this query").unwrap(),
        "it closes the answer"
    );
    assert!(link < page.body.find("</main>").unwrap());
}
