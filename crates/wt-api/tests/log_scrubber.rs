//! Spec 07, Observability: logs never carry query text, passage text, invite tokens, cookies or
//! secrets. Every request below runs under a JSON subscriber at the most verbose level for every
//! target, and no captured line may contain any of those strings. Synthetic data only.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::io::Write;
use std::sync::{Arc, Mutex};

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
/// A query no code path would print by accident.
const QUERY: &str = "How does zazen meet the quokkaflux of anger";
/// An acute question with a word no code path would print (Spec 11, PB3.4).
const ACUTE: &str = "I want to end my life because of the zembrolt anger";
/// A query string no code path would print: the request log names the route, never the URL.
const URL_QUERY: &str = "glimmerwhorl";
const PASSAGE: &str = "When anger arises, notice the quokkaflux warmth and breathe with it.";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

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
         from teacher te returning id",
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
        "test",
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
    .bind(PASSAGE)
    .fetch_one(pool)
    .await
    .unwrap();
    let v = wt_retrieve::search::vector_literal(&FakeEmbedder::vector(PASSAGE));
    sqlx::query(
        "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms, text,
                              word_count, content_hash, embedding, embedding_model)
         values ($1, $2, 'talk', 0, 65000, 125000, $3, 12, 'h', $4::text::vector, $5)",
    )
    .bind(talk)
    .bind(segment)
    .bind(PASSAGE)
    .bind(v)
    .bind(MODEL)
    .execute(pool)
    .await
    .unwrap();
}

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

fn with_cookie(r: axum::http::request::Builder, cookie: &str) -> axum::http::request::Builder {
    r.header(header::COOKIE, format!("wt_session={cookie}"))
}

fn form(path: &str, cookie: &str, body: String) -> Request<Body> {
    with_cookie(Request::post(path), cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
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

fn csrf(body: &str) -> String {
    let at = body.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
    body[at..].split('"').next().unwrap().to_owned()
}

#[tokio::test]
async fn no_log_line_carries_query_text_passage_text_tokens_or_cookies() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("trace"))
        .with_writer(move || writer.clone())
        .json()
        .flatten_event(true)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let file = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &file, "test").await.unwrap();
    passage(&pool).await;
    let app = router(AppState {
        pool: pool.clone(),
        site: Arc::new(Site::with_fakes(MODEL)),
    });

    let token = session::new_token().unwrap();
    wt_db::tester::invite(&pool, "owner", &session::token_hash(&token))
        .await
        .unwrap();
    let (status, headers, _) = send(
        &app,
        Request::get(format!("/join/{token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("wt_session=")
        .to_owned();

    let (_, _, home) = send(
        &app,
        with_cookie(Request::get("/"), &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let token_field = csrf(&home);
    let (status, headers, _) = send(
        &app,
        form(
            "/ask",
            &cookie,
            format!("q={}&csrf={}", encode(QUERY), encode(&token_field)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let permalink = headers[header::LOCATION].to_str().unwrap().to_owned();
    let (status, _, page) = send(
        &app,
        with_cookie(Request::get(&permalink), &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("quokkaflux warmth"), "the passage was served");
    // The deferred summary settles before the logs are read (no model here, so `no_key`).
    for _ in 0..100 {
        let (status, _, _) = send(
            &app,
            with_cookie(Request::get(format!("{permalink}/summary")), &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        if status != StatusCode::ACCEPTED {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    send(
        &app,
        form(
            &format!("{permalink}/feedback"),
            &cookie,
            format!("kind=flag_query&csrf={}", encode(&csrf(&page))),
        ),
    )
    .await;
    // Spec 07: a request with a distinctive query string, on a route and on no route.
    let (_, _, about_page) = send(
        &app,
        with_cookie(Request::get(format!("/about?q={URL_QUERY}")), &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    // Spec 09: testers are told about the request log.
    assert!(about_page.contains("technical log of each page you open"));
    send(
        &app,
        Request::get(format!("/nowhere/{URL_QUERY}?q={URL_QUERY}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    // A request that fails and a bad token both log what they log.
    send(
        &app,
        Request::get("/join/not-a-real-token-quokkaflux")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    // An acute question: its notice page, and neither its text in `query_log` nor in the logs.
    let (status, _, notice) = send(
        &app,
        form(
            "/ask",
            &cookie,
            format!("q={}&csrf={}", encode(ACUTE), encode(&token_field)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(notice.contains("Please reach out for help now"));
    let acute: Vec<Option<String>> =
        sqlx::query_scalar("select query_text from query_log where sensitive_category = 'acute'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(acute, [None], "one acute row, with no text");
    let anywhere: i64 =
        sqlx::query_scalar("select count(*) from query_log where query_text like $1")
            .bind("%zembrolt%")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(anywhere, 0);

    let logs = captured.text();
    assert!(
        logs.contains("answer stored") && logs.contains("retrieved"),
        "the ask path's own lines were not captured"
    );
    // Spec 11, PB4.3: the slow-answers alarm reads `ask_completed` and its milliseconds.
    let completed: Vec<serde_json::Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["event"] == "ask_completed")
        .collect();
    assert_eq!(completed.len(), 1, "one ask_completed per answer");
    assert!(completed[0]["ms"].is_u64(), "ask_completed carries ms");
    // Spec 11, PB2.4 and PB2.1: one summary_settled and one ranker_served per answer. They are
    // checked here, in a binary of its own, because a scoped subscriber in a binary whose other
    // tests log in parallel can miss events.
    let events = |name: &str| -> Vec<serde_json::Value> {
        logs.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["event"] == name)
            .collect()
    };
    let settled = events("summary_settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["summary_status"], "no_key");
    assert!(settled[0]["answer_id"].is_string());
    // The 5xx-rate alarm behind the Cloudflare Tunnel reads one `http_response` per request,
    // naming the declared route, never the path (which holds the invite token).
    let responses = events("http_response");
    assert!(
        responses
            .iter()
            .all(|v| v["route"].is_string() && v["status"].is_u64() && v["ms"].is_u64()),
        "{responses:?}"
    );
    assert!(
        responses
            .iter()
            .any(|v| v["route"] == "/join/{token}" && v["status"] == 303),
        "the invite link's response was logged by its route: {responses:?}"
    );
    assert!(
        responses
            .iter()
            .any(|v| v["route"] == "/ask" && v["status"] == 303),
        "{responses:?}"
    );
    let ranked = events("ranker_served");
    assert_eq!(ranked.len(), 1, "{ranked:?}");
    assert_eq!(ranked[0]["ranker"], "none (fallback)", "no reranker here");
    // Spec 07: the same events carry the request id, the method and the tester.
    let requests = &responses;
    let line = |method: &str, route: &str| -> Vec<&serde_json::Value> {
        requests
            .iter()
            .filter(|v| v["method"] == method && v["route"] == route)
            .collect()
    };
    let about = line("GET", "/about");
    assert_eq!(about.len(), 1, "{requests:?}");
    assert_eq!(about[0]["status"], 200);
    assert!(about[0]["ms"].is_u64());
    assert_eq!(about[0]["request_id"].as_str().map(str::len), Some(16));
    let tester: String = sqlx::query_scalar("select id::text from tester where label = 'owner'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(about[0]["tester"], tester.as_str());
    assert_eq!(line("POST", "/ask").len(), 2);
    assert_eq!(line("GET", "/a/{answer}").len(), 1);
    assert!(!line("GET", "/a/{answer}/summary").is_empty());
    let joins = line("GET", "/join/{token}");
    assert_eq!(joins.len(), 2, "{requests:?}");
    let named: Vec<_> = joins.iter().map(|v| v["tester"].clone()).collect();
    assert!(
        named.contains(&serde_json::Value::from(tester.as_str())),
        "the invite names the tester it signs in: {named:?}"
    );
    assert!(
        named.iter().any(serde_json::Value::is_null),
        "an unknown token names no one: {named:?}"
    );
    // Spec 07, Usage report: each request with a tester is a page view, by route template.
    let mut routes: Vec<(String, String)> = Vec::new();
    for _ in 0..50 {
        routes = sqlx::query_as(
            "select method, route from page_view p join tester t on t.id = p.tester_id
             where t.label = 'owner' order by p.created_at",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        if routes.len() == requests.iter().filter(|v| !v["tester"].is_null()).count() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let has = |m: &str, r: &str| routes.iter().any(|(rm, rr)| rm == m && rr == r);
    assert_eq!(
        routes.first().map(|(_, r)| r.as_str()),
        Some("/join/{token}")
    );
    assert!(has("GET", "/about") && has("POST", "/ask") && has("GET", "/a/{answer}"));
    assert!(
        !has("GET", "fallback"),
        "a request without a session is not stored"
    );
    assert_eq!(
        routes.len(),
        requests.iter().filter(|v| !v["tester"].is_null()).count(),
        "one page view per request with a tester: {routes:?}"
    );
    let nowhere = line("GET", "fallback");
    assert_eq!(nowhere.len(), 1);
    assert_eq!(nowhere[0]["status"], 404);
    assert!(nowhere[0]["tester"].is_null());
    for (what, secret) in [
        ("query text", "quokkaflux"),
        ("query text", QUERY),
        ("acute question text", "zembrolt"),
        ("passage text", PASSAGE),
        ("invite token", token.as_str()),
        ("session cookie", cookie.as_str()),
        ("CSRF token", token_field.as_str()),
        ("URL query string", URL_QUERY),
    ] {
        assert!(
            !logs.contains(secret),
            "a log line carries the {what}: {}",
            logs.lines()
                .find(|l| l.contains(secret))
                .unwrap_or_default()
        );
    }
}
