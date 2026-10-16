//! Spec 06 acceptance for the pages the alpha uses: invite access, the session gate, CSRF,
//! asking, the owner-only permalink, escaping, feedback and the security headers; and Spec 05's
//! M6 items for the summary. Synthetic teachers and passages only.
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
use wt_extract::{DataClass, FakeLlm, LlmClient, LlmError, LlmRequest};
use wt_ml::FakeEmbedder;
use wt_registry::RegistryFile;

const REGISTRY: &str = r#"
[[sangha]]
name = "North Fixture Zendo"
donation_url = "https://example.invalid/north"

[[teacher]]
display_name = "Ada Fixture"
sangha = "North Fixture Zendo"

[[teacher]]
display_name = "Bo Fixture"
sangha = "North Fixture Zendo"
"#;

const MODEL: &str = "fake-embed";
const ACTOR: &str = "test";
const ANGER: &str = "When anger arises, turn toward the anger and breathe with it. \
                     <script>alert(1)</script> The bell rang twice before the service.";
const BO_ANGER: &str = "Anger is only energy. Sit with the anger, turn toward it, and breathe \
                        until it softens on its own.";
const HEADLINE: &str = "Two talks say to turn toward anger and breathe with it.";
const AGREE: &str = "Talks by two teachers describe turning toward anger and breathing with it.";

struct Fx {
    _db: TestDb,
    pool: PgPool,
    app: Router,
}

async fn start() -> Fx {
    start_with(Site::with_fakes(MODEL), false).await
}

/// A fixture with `site`; `two` adds Bo's passage on anger beside Ada's.
async fn start_with(site: Site, two: bool) -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let file = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &file, ACTOR).await.unwrap();
    passage(&pool, "Ada Fixture", "vid1", ANGER).await;
    if two {
        passage(&pool, "Bo Fixture", "vid2", BO_ANGER).await;
    }
    let app = router(AppState {
        pool: pool.clone(),
        site: Arc::new(site),
    });
    Fx { _db: db, pool, app }
}

/// A ready, consented talk by `teacher` with one passage.
async fn passage(pool: &PgPool, teacher: &str, video_id: &str, text: &str) {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('youtube', $1, 'https://example.invalid/' || $1, 'Fixture talk') returning id",
    )
    .bind(video_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, 'On anger', te.id, te.sangha_id, te.display_name, 'awaiting_consent'
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
    .bind(text)
    .fetch_one(pool)
    .await
    .unwrap();
    let v = wt_retrieve::search::vector_literal(&FakeEmbedder::vector(text));
    sqlx::query(
        "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms, text,
                              word_count, content_hash, embedding, embedding_model)
         values ($1, $2, 'talk', 0, 65000, 125000, $3, 20, 'h', $4::text::vector, $5)",
    )
    .bind(talk)
    .bind(segment)
    .bind(text)
    .bind(v)
    .bind(MODEL)
    .execute(pool)
    .await
    .unwrap();
}

async fn invite(pool: &PgPool, label: &str) -> String {
    let token = session::new_token().unwrap();
    wt_db::tester::invite(pool, label, &session::token_hash(&token))
        .await
        .unwrap();
    token
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

fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut r = Request::get(path);
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, format!("wt_session={c}"));
    }
    r.body(Body::empty()).unwrap()
}

fn post(path: &str, cookie: &str, form: &str) -> Request<Body> {
    Request::post(path)
        .header(header::COOKIE, format!("wt_session={cookie}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .unwrap()
}

/// Joins with a token and returns the session cookie value.
async fn join(fx: &Fx, label: &str) -> String {
    let token = invite(&fx.pool, label).await;
    let r = send(&fx.app, get(&format!("/join/{token}"), None)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let set = r.headers[header::SET_COOKIE].to_str().unwrap().to_owned();
    assert!(
        set.contains("HttpOnly") && set.contains("SameSite=Lax"),
        "{set}"
    );
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

#[tokio::test]
async fn an_invite_sets_the_session_and_a_bad_token_gets_the_generic_404() {
    let fx = start().await;
    let bad = send(&fx.app, get("/join/not-a-token", None)).await;
    assert_eq!(bad.status, StatusCode::NOT_FOUND);
    assert!(bad.body.contains("That page does not exist."));

    let cookie = join(&fx, "owner").await;
    let home = send(&fx.app, get("/", Some(&cookie))).await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("What have Zen teachers said"));

    wt_db::tester::revoke(&fx.pool, "owner").await.unwrap();
    let revoked = send(&fx.app, get("/", Some(&cookie))).await;
    assert_eq!(revoked.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pages_need_a_session_and_every_response_carries_the_headers() {
    let fx = start().await;
    for path in ["/", "/about", "/talk/00000000-0000-0000-0000-000000000000"] {
        let r = send(&fx.app, get(path, None)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(r.headers["content-security-policy"], "default-src 'self'");
        assert_eq!(r.headers["x-content-type-options"], "nosniff");
        assert_eq!(r.headers["referrer-policy"], "no-referrer");
        assert_eq!(r.headers["x-robots-tag"], "noindex");
    }
    let robots = send(&fx.app, get("/robots.txt", None)).await;
    assert!(robots.body.contains("Disallow: /"));
}

#[tokio::test]
async fn asking_stores_the_answer_and_only_its_owner_can_read_it() {
    let fx = start().await;
    let cookie = join(&fx, "owner").await;

    let refused = send(&fx.app, post("/ask", &cookie, "q=anger&csrf=wrong")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);

    let home = send(&fx.app, get("/", Some(&cookie))).await;
    let token = csrf(&home.body);
    let asked = send(
        &fx.app,
        post(
            "/ask",
            &cookie,
            &format!(
                "q={}&csrf={}",
                encode("How do I work with anger?"),
                encode(&token)
            ),
        ),
    )
    .await;
    assert_eq!(asked.status, StatusCode::SEE_OTHER);
    let permalink = asked.headers[header::LOCATION].to_str().unwrap().to_owned();
    assert!(permalink.starts_with("/a/"));

    let page = send(&fx.app, get(&permalink, Some(&cookie))).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Ada Fixture"));
    assert!(page.body.contains("alert(1)"), "the passage is shown");
    assert!(!page.body.contains("<script>"), "and escaped");
    assert!(page.body.contains("&#60;script&#62;"));
    assert!(
        page.body
            .contains("https://www.youtube.com/watch?v=vid1&#38;t=63s")
    );
    assert!(page.body.contains("Used with permission of Ada Fixture."));
    assert!(
        page.body.contains("/rights?excerpt="),
        "every passage has Report this excerpt"
    );
    assert!(
        page.body
            .contains("aria-label=\"Report this excerpt\">Report</a>")
    );

    let logged: (i64, i64) = sqlx::query_as(
        "select (select count(*) from query_log), (select count(*) from answer_passage)",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(logged, (1, 1));

    let other = join(&fx, "someone else").await;
    let theirs = send(&fx.app, get(&permalink, Some(&other))).await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn feedback_and_flagging_are_recorded() {
    let fx = start().await;
    let cookie = join(&fx, "owner").await;
    let token = csrf(&send(&fx.app, get("/", Some(&cookie))).await.body);
    let asked = send(
        &fx.app,
        post("/ask", &cookie, &format!("q=anger&csrf={}", encode(&token))),
    )
    .await;
    let permalink = asked.headers[header::LOCATION].to_str().unwrap().to_owned();
    let fb = format!("{permalink}/feedback");

    let r = send(
        &fx.app,
        post(
            &fb,
            &cookie,
            &format!("kind=flag_query&csrf={}", encode(&token)),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let flagged: bool = sqlx::query_scalar("select flagged from query_log")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert!(flagged);

    let r = send(
        &fx.app,
        post(
            &fb,
            &cookie,
            &format!("kind=nonsense&csrf={}", encode(&token)),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_short_question_is_refused_with_the_message() {
    let fx = start().await;
    let cookie = join(&fx, "owner").await;
    let token = csrf(&send(&fx.app, get("/", Some(&cookie))).await.body);
    let r = send(
        &fx.app,
        post("/ask", &cookie, &format!("q=a&csrf={}", encode(&token))),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.body
            .contains("Please write between 3 and 500 characters.")
    );
}

// ---- the summary (Spec 05, M6) --------------------------------------------------------------

fn summary_reply() -> serde_json::Value {
    let cite = |e: &str| serde_json::json!({ "excerpt": e, "start": 1, "end": 1 });
    serde_json::json!({
        "headline": { "text": HEADLINE, "cites": [cite("E1"), cite("E2")] },
        "framing": [],
        "agree": [{ "text": AGREE, "cites": [cite("E1"), cite("E2")] }],
        "differ": [],
        "keep_in_mind": [],
        "sum_up": null
    })
}

fn verdict(supported: bool) -> serde_json::Value {
    let part = (!supported).then_some("breathe");
    serde_json::json!({ "supported": supported, "unsupported_part": part })
}

fn site_with(llm: Arc<dyn LlmClient>) -> Site {
    Site {
        llm,
        ..Site::with_fakes(MODEL)
    }
}

/// Asks `question` and returns the answer page.
async fn ask(fx: &Fx, question: &str) -> Reply {
    let cookie = join(fx, "owner").await;
    let token = csrf(&send(&fx.app, get("/", Some(&cookie))).await.body);
    let asked = send(
        &fx.app,
        post(
            "/ask",
            &cookie,
            &format!("q={}&csrf={}", encode(question), encode(&token)),
        ),
    )
    .await;
    assert_eq!(asked.status, StatusCode::SEE_OTHER, "{}", asked.body);
    let permalink = asked.headers[header::LOCATION].to_str().unwrap().to_owned();
    settled(fx, &permalink, &cookie).await;
    send(&fx.app, get(&permalink, Some(&cookie))).await
}

/// Waits, as the page's script does, until the answer's summary is no longer being written.
async fn settled(fx: &Fx, permalink: &str, cookie: &str) {
    for _ in 0..100 {
        let r = send(&fx.app, get(&format!("{permalink}/summary"), Some(cookie))).await;
        if r.status != StatusCode::ACCEPTED {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the summary was still pending after 10 s");
}

async fn theme_counts(pool: &PgPool) -> (i32, i32) {
    sqlx::query_as("select theme_count, themes_dropped from query_log")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The answer's `summary_status` (Spec 11, PB2.4).
async fn summary_status(pool: &PgPool) -> String {
    sqlx::query_scalar("select summary_status from answer")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The summary section of a page.
fn summary_of(body: &str) -> &str {
    let start = body.find("<section id=\"summary\"").unwrap();
    let section = &body[start..];
    &section[..section.find("</section>").unwrap()]
}

#[tokio::test]
async fn a_verified_summary_renders_with_quoted_lines_and_is_stored_as_spans() {
    let llm = Arc::new(FakeLlm::new(vec![
        Ok(summary_reply()),
        Ok(verdict(true)),
        Ok(verdict(true)),
    ]));
    let fx = start_with(site_with(llm.clone()), true).await;
    let page = ask(&fx, "How do I work with anger?").await;
    assert_eq!(page.status, StatusCode::OK);
    let section = summary_of(&page.body);
    assert_eq!(
        page.body.matches(HEADLINE).count(),
        1,
        "model text appears once"
    );
    assert!(
        section.contains(HEADLINE) && section.contains(AGREE),
        "{section}"
    );
    assert!(
        section.contains("<h3>Where the talks agree</h3>"),
        "{section}"
    );
    assert!(
        !section.contains("Where they differ"),
        "an empty section is left out"
    );
    assert!(
        !section.contains("Feedback on"),
        "the summary has no control of its own; one control closes the page"
    );
    // Each marker points to a quoted line the code took verbatim from the excerpt.
    assert!(section.contains("href=\"#sq-1\"") && section.contains("href=\"#sq-2\""));
    assert!(
        section.contains("<q>When anger arises, turn toward the anger and breathe with it.</q>"),
        "{section}"
    );
    assert!(
        section.contains("<q>Anger is only energy.</q>"),
        "{section}"
    );
    assert!(section.contains("href=\"#p1\"") && section.contains("href=\"#p2\""));
    assert!(
        section.contains("Drawn from 2 talks by 2 teachers."),
        "{section}"
    );
    assert!(
        section.contains("Only 2 talks in this collection matched this question closely."),
        "{section}"
    );
    assert!(
        !section.contains("excerpts come from teachers at"),
        "the one-sangha note needs three excerpts"
    );
    assert!(
        page.body.find("id=\"summary\"").unwrap() < page.body.find("<blockquote>").unwrap(),
        "above the excerpts"
    );
    assert_eq!(
        page.body.matches("<blockquote>").count(),
        2,
        "the excerpts stay"
    );

    assert_eq!(theme_counts(&fx.pool).await, (2, 0));
    assert_eq!(summary_status(&fx.pool).await, "shown");
    let stored: serde_json::Value = sqlx::query_scalar("select payload from answer")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let sentences = &stored["summary"]["sentences"];
    assert_eq!(sentences[0]["part"], "headline");
    assert_eq!(sentences[0]["text"], HEADLINE);
    let cited: Vec<&serde_json::Value> = sentences[0]["cites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["passage_id"])
        .collect();
    let ids: Vec<&serde_json::Value> = stored["passage_ids"].as_array().unwrap().iter().collect();
    assert_eq!(cited, ids, "the code maps E1 and E2 to the offered ids");
    let raw = stored.to_string();
    assert!(
        !raw.contains("anger arises") && !raw.contains("only energy"),
        "spans, never passage text: {raw}"
    );

    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "one generation and two verifier calls");
    assert!(seen.iter().all(|(_, class)| *class == DataClass::Real));
}

#[tokio::test]
async fn a_rejected_headline_leaves_no_summary_and_counts_the_drop() {
    // The headline is rejected in the draft and again in its one revision.
    let llm = Arc::new(FakeLlm::new(vec![
        Ok(summary_reply()),
        Ok(verdict(false)),
        Ok(verdict(true)),
        Ok(summary_reply()),
        Ok(verdict(false)),
        Ok(verdict(true)),
    ]));
    let fx = start_with(site_with(llm), true).await;
    let page = ask(&fx, "How do I work with anger?").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("id=\"summary\""));
    assert!(!page.body.contains(HEADLINE) && !page.body.contains(AGREE));
    assert_eq!(page.body.matches("<blockquote>").count(), 2);
    assert_eq!(theme_counts(&fx.pool).await, (0, 2));
    assert_eq!(summary_status(&fx.pool).await, "all_sentences_rejected");
}

#[tokio::test]
async fn a_failed_verifier_leaves_a_fully_rendered_answer_without_the_summary() {
    // The generation call succeeds and the verifier has no reply left, so it fails.
    let llm = Arc::new(FakeLlm::new(vec![Ok(summary_reply())]));
    let fx = start_with(site_with(llm), true).await;
    let page = ask(&fx, "How do I work with anger?").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("id=\"summary\""));
    assert!(page.body.contains("Ada Fixture") && page.body.contains("Bo Fixture"));
    assert_eq!(page.body.matches("<blockquote>").count(), 2);
    assert_eq!(summary_status(&fx.pool).await, "model_error");
}

/// A model that never answers in time.
struct Stalled;

#[async_trait::async_trait]
impl LlmClient for Stalled {
    async fn complete_value(
        &self,
        _: &LlmRequest,
        _: &serde_json::Value,
        _: DataClass,
    ) -> Result<serde_json::Value, LlmError> {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        Ok(summary_reply())
    }
}

#[tokio::test]
async fn a_generation_timeout_leaves_a_fully_rendered_answer_without_the_summary() {
    let mut site = site_with(Arc::new(Stalled));
    site.retrieval.synthesis.synthesis_timeout_s = 1;
    let fx = start_with(site, true).await;
    let page = ask(&fx, "How do I work with anger?").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("id=\"summary\""));
    assert_eq!(page.body.matches("<blockquote>").count(), 2);
    assert_eq!(summary_status(&fx.pool).await, "timeout");
}

/// A model that refuses every call at the month's cap.
struct Capped;

#[async_trait::async_trait]
impl LlmClient for Capped {
    async fn complete_value(
        &self,
        _: &LlmRequest,
        _: &serde_json::Value,
        _: DataClass,
    ) -> Result<serde_json::Value, LlmError> {
        Err(LlmError::CapReached {
            month_to_date: 60.0,
            cap: 60.0,
        })
    }
}

#[tokio::test]
async fn a_reached_cap_is_stored_as_cap_reached() {
    let fx = start_with(site_with(Arc::new(Capped)), true).await;
    let page = ask(&fx, "How do I work with anger?").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("id=\"summary\""));
    assert_eq!(summary_status(&fx.pool).await, "cap_reached");
}

/// Spec 11, PB2.4 and PB4.3: no model is stored as `no_key`, and the answer logs one
/// `summary_settled` event with its status and no question text.
#[tokio::test]
async fn no_model_is_stored_as_no_key_and_logs_summary_settled() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_writer(move || writer.clone())
        .json()
        .flatten_event(true)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let fx = start_with(site_with(Arc::new(wt_extract::UnavailableLlm)), true).await;
    let page = ask(&fx, "How do I work with anger?").await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(summary_status(&fx.pool).await, "no_key");
    let answer: Uuid = sqlx::query_scalar("select id from answer")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let events: Vec<serde_json::Value> = captured
        .text()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["event"] == "summary_settled")
        .collect();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["summary_status"], "no_key");
    assert_eq!(events[0]["answer_id"], answer.to_string());
    assert!(
        !captured.text().contains("work with anger"),
        "no question text"
    );
}

#[derive(Clone, Default)]
struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
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

/// The page comes back before the summary is written: its place shows at the top, and the
/// summary route answers 202 until the summary is stored, then 200 with the section
/// (owner's request of October 15, 2026).
#[tokio::test]
async fn the_page_comes_back_before_its_summary_and_the_summary_fills_in() {
    let mut site = site_with(Arc::new(Slow));
    site.retrieval.synthesis.synthesis_timeout_s = 10;
    let fx = start_with(site, true).await;
    let cookie = join(&fx, "owner").await;
    let token = csrf(&send(&fx.app, get("/", Some(&cookie))).await.body);
    let asked = send(
        &fx.app,
        post("/ask", &cookie, &format!("q=anger&csrf={}", encode(&token))),
    )
    .await;
    let permalink = asked.headers[header::LOCATION].to_str().unwrap().to_owned();
    let first = send(&fx.app, get(&permalink, Some(&cookie))).await;
    assert!(
        first.body.contains("data-summary=\"") && first.body.contains("Writing a summary"),
        "{}",
        first.body
    );
    assert_eq!(
        first.body.matches("<blockquote>").count(),
        2,
        "the excerpts are ready"
    );
    let pending = send(&fx.app, get(&format!("{permalink}/summary"), Some(&cookie))).await;
    assert_eq!(pending.status, StatusCode::ACCEPTED);
    settled(&fx, &permalink, &cookie).await;
    let ready = send(&fx.app, get(&format!("{permalink}/summary"), Some(&cookie))).await;
    assert_eq!(ready.status, StatusCode::OK);
    assert!(
        ready.body.contains("<section id=\"summary\""),
        "{}",
        ready.body
    );
    let after = send(&fx.app, get(&permalink, Some(&cookie))).await;
    assert!(!after.body.contains("data-summary=\""));
    assert!(after.body.contains("<section id=\"summary\""));
}

/// A model that answers after a second, as a real one does.
struct Slow;

#[async_trait::async_trait]
impl LlmClient for Slow {
    async fn complete_value(
        &self,
        req: &LlmRequest,
        _: &serde_json::Value,
        _: DataClass,
    ) -> Result<serde_json::Value, LlmError> {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        Ok(match req.stage {
            wt_core::config::LlmStage::Verify => verdict(true),
            _ => summary_reply(),
        })
    }
}

#[tokio::test]
async fn named_mode_makes_no_summary_call() {
    let llm = Arc::new(FakeLlm::new(vec![Ok(summary_reply()), Ok(verdict(true))]));
    let fx = start_with(site_with(llm.clone()), true).await;
    let page = ask(&fx, "What does Ada Fixture say about anger?").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("id=\"summary\""));
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "no model call in named mode"
    );
    assert_eq!(summary_status(&fx.pool).await, "not_attempted");
}

/// Spec 04, Student questions, step 5: the transcript and answer pages show the student's
/// question, lightly edited, with "Student" and the edit label, above the answer and outside
/// any blockquote.
#[tokio::test]
async fn the_pages_show_a_students_question_as_context_not_quotation() {
    let fx = start().await;
    let question = "How do I stay with <b>anger</b> when it is strong?";
    let talk: Uuid = sqlx::query_scalar(
        "insert into student_question (talk_id, transcript_id, start_ms, end_ms, answer_start_ms,
                                       text, word_count, content_hash, embedding,
                                       embedding_model, edit_model)
         select t.id, tr.id, 50000, 60000, 64000, $1, 10, 'h', p.embedding, $2, 'fake-editor'
         from talk t join transcript tr on tr.talk_id = t.id join passage p on p.talk_id = t.id
         returning talk_id",
    )
    .bind(question)
    .bind(MODEL)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let shown = "How do I stay with &#60;b&#62;anger&#60;/b&#62; when it is strong?";
    let label = "Question lightly edited by machine; names and identifying details removed.";
    let cookie = join(&fx, "owner").await;

    let transcript = send(
        &fx.app,
        get(&format!("/talk/{talk}/transcript"), Some(&cookie)),
    )
    .await;
    assert_eq!(transcript.status, StatusCode::OK);
    let at = transcript
        .body
        .find(&format!("<strong>Student:</strong> {shown}"))
        .unwrap_or_else(|| panic!("{}", transcript.body));
    assert!(
        at < transcript.body.find("When anger arises").unwrap(),
        "above its answer"
    );
    assert!(transcript.body.contains(label));
    assert!(!transcript.body.contains("A student asked a question"));

    let home = send(&fx.app, get("/", Some(&cookie))).await;
    let asked = send(
        &fx.app,
        post(
            "/ask",
            &cookie,
            &format!(
                "q={}&csrf={}",
                encode("How do I work with anger?"),
                encode(&csrf(&home.body))
            ),
        ),
    )
    .await;
    let permalink = asked.headers[header::LOCATION].to_str().unwrap().to_owned();
    let page = send(&fx.app, get(&permalink, Some(&cookie))).await;
    let at = page
        .body
        .find(&format!("<strong>Student:</strong> {shown}"))
        .unwrap_or_else(|| panic!("{}", page.body));
    let quote = page.body.find("<blockquote>").unwrap();
    assert!(at < quote, "above the passage");
    let blockquote = &page.body[quote..page.body.find("</blockquote>").unwrap()];
    assert!(
        !blockquote.contains("stay with"),
        "never inside the quotation"
    );
    assert!(page.body.contains(label));
}

// ---- dana panel and give links (Spec 05, Dana calculator; Spec 06, item 8 and Logging) -------

const NORTH_URL: &str = "https://example.invalid/north";
const SOUTH_URL: &str = "https://give.south.example/donate";
const SITE_URL: &str = "https://site.example/give";

/// Two answers' worth of fixture: Ada at North and Bo moved to South, both pages verified.
async fn dana_fixture(site_url: Option<&str>) -> Fx {
    let mut site = Site::with_fakes(MODEL);
    site.site_dana_url = site_url.map(str::to_owned);
    let fx = start_with(site, true).await;
    sqlx::query(
        "insert into sangha (name, donation_url, donation_url_template, donation_kind)
         values ('South Fixture Zendo', $1, $1 || '?amount={amount}', 'page')",
    )
    .bind(SOUTH_URL)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "update talk set sangha_id = (select id from sangha where name = 'South Fixture Zendo')
         where teacher_id = (select id from teacher where display_name = 'Bo Fixture')",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    for (name, url) in [
        ("North Fixture Zendo", NORTH_URL),
        ("South Fixture Zendo", SOUTH_URL),
    ] {
        wt_db::donation::verify(&fx.pool, name, url, "test")
            .await
            .unwrap();
    }
    fx
}

/// Asks as `cookie` and returns the permalink, the answer id and the page's CSRF token.
async fn ask_as(fx: &Fx, cookie: &str) -> (String, Uuid, String) {
    let token = csrf(&send(&fx.app, get("/", Some(cookie))).await.body);
    let asked = send(
        &fx.app,
        post(
            "/ask",
            cookie,
            &format!(
                "q={}&csrf={}",
                encode("How do I work with anger?"),
                encode(&token)
            ),
        ),
    )
    .await;
    assert_eq!(asked.status, StatusCode::SEE_OTHER, "{}", asked.body);
    let permalink = asked.headers[header::LOCATION].to_str().unwrap().to_owned();
    let id = permalink.trim_start_matches("/a/").parse().unwrap();
    (permalink, id, token)
}

fn fragment_post(path: &str, cookie: &str, form: &str) -> Request<Body> {
    Request::post(path)
        .header(header::COOKIE, format!("wt_session={cookie}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("HX-Request", "true")
        .body(Body::from(form.to_owned()))
        .unwrap()
}

async fn events(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("select count(*) from dana_event where kind = $1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// id, name, donation URL, template, verified, listed.
type SanghaRow = (Uuid, String, Option<String>, Option<String>, bool, bool);

/// The calculator's own lines for this answer, as the page should print them.
async fn expected_lines(pool: &PgPool, answer: Uuid, total: i64, site: i64) -> Vec<String> {
    let rows: Vec<SanghaRow> = sqlx::query_as(
        "select id, name, donation_url, donation_url_template, donation_verified, listed
         from sangha where name like '%Fixture Zendo'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let recipients = rows
        .into_iter()
        .map(
            |(id, name, url, url_template, verified, listed)| wt_dana::Recipient {
                sangha_id: id.into(),
                name,
                url,
                url_template,
                verified,
                listed,
            },
        )
        .collect();
    let split = wt_dana::split(&wt_dana::DanaInput {
        gift_total_cents: total,
        site_cents: site,
        recipients,
        tiebreak_seed: wt_dana::seed_from(answer.as_bytes()),
    })
    .unwrap();
    split
        .lines
        .iter()
        .map(|l| format!("{}: ${}.", l.name, wt_dana::dollars(l.cents)))
        .collect()
}

#[tokio::test]
async fn the_dana_fragment_and_the_no_script_page_show_the_calculators_split() {
    let fx = dana_fixture(None).await;
    let cookie = join(&fx, "owner").await;
    let (permalink, id, token) = ask_as(&fx, &cookie).await;
    let page = send(&fx.app, get(&permalink, Some(&cookie))).await;
    assert!(page.body.contains("<h2 id=\"dana-h\">Dana</h2>"));
    assert!(page.body.contains("This site never handles your money."));
    assert!(page.body.contains("Show the split"));
    assert!(page.body.contains("role=\"status\" aria-live=\"polite\""));
    assert!(page.body.contains("name=\"first\""));
    assert!(
        !page.body.contains("Optional gift to this site"),
        "no site page, no site field"
    );
    let dana = format!("{permalink}/dana");

    // An uneven total, so the extra cent goes where the seed says.
    let expected = expected_lines(&fx.pool, id, 1001, 0).await;
    assert_eq!(expected.len(), 2);
    let fragment = send(
        &fx.app,
        fragment_post(
            &dana,
            &cookie,
            &format!("total=10.01&csrf={}", encode(&token)),
        ),
    )
    .await;
    assert_eq!(fragment.status, StatusCode::OK);
    assert!(!fragment.body.contains("<html"), "a fragment, not a page");
    assert!(
        fragment
            .body
            .contains("id=\"dana-result-h\" tabindex=\"-1\"")
    );
    for line in &expected {
        assert!(
            fragment.body.contains(&encode_html(line)),
            "{line} in {}",
            fragment.body
        );
    }
    assert!(fragment.body.contains("Total you would give: $10.01"));
    assert!(fragment.body.contains("Shares are equal."));
    assert_eq!(events(&fx.pool, "split_computed").await, 1);
    assert_eq!(
        events(&fx.pool, "panel_open").await,
        0,
        "the script logs the open itself"
    );

    // Without the script: the whole page, the same split, the amount kept, the open logged.
    let full = send(
        &fx.app,
        post(
            &dana,
            &cookie,
            &format!("total=10.01&first=1&csrf={}", encode(&token)),
        ),
    )
    .await;
    assert_eq!(full.status, StatusCode::OK);
    assert!(full.body.contains("<html"));
    for line in &expected {
        assert!(full.body.contains(&encode_html(line)), "{line}");
    }
    assert!(full.body.contains("value=\"10.01\""));
    assert!(
        !full.body.contains("name=\"first\""),
        "the open is logged once"
    );
    assert_eq!(events(&fx.pool, "panel_open").await, 1);
    assert_eq!(events(&fx.pool, "split_computed").await, 2);
    let total: i32 = sqlx::query_scalar(
        "select total_cents from dana_event where kind = 'split_computed' limit 1",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(total, 1001);

    // The script's beacon logs the open and returns nothing.
    let beacon = send(
        &fx.app,
        post(&dana, &cookie, &format!("open=1&csrf={}", encode(&token))),
    )
    .await;
    assert_eq!(beacon.status, StatusCode::NO_CONTENT);
    assert_eq!(events(&fx.pool, "panel_open").await, 2);

    // Bad amounts show a message and log no split.
    for bad in ["total=ten", "total=-5", "total=100001", "total="] {
        let r = send(
            &fx.app,
            fragment_post(&dana, &cookie, &format!("{bad}&csrf={}", encode(&token))),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        assert!(r.body.contains("class=\"error\""), "{bad}: {}", r.body);
    }
    assert_eq!(events(&fx.pool, "split_computed").await, 2);

    // CSRF, and someone else's answer.
    let refused = send(&fx.app, fragment_post(&dana, &cookie, "total=5&csrf=wrong")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let other = join(&fx, "someone else").await;
    let other_token = csrf(&send(&fx.app, get("/", Some(&other))).await.body);
    let theirs = send(
        &fx.app,
        fragment_post(
            &dana,
            &other,
            &format!("total=5&csrf={}", encode(&other_token)),
        ),
    )
    .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
}

fn encode_html(s: &str) -> String {
    s.replace('&', "&#38;").replace('\'', "&#39;")
}

#[tokio::test]
async fn the_site_line_is_separate_and_a_contact_page_drops_out_with_its_note() {
    let fx = dana_fixture(Some(SITE_URL)).await;
    // South turns out to take gifts only through a contact page (Spec 02, Donation links, 4).
    sqlx::query(
        "update sangha set donation_verified = false, donation_kind = 'contact',
                donation_note = 'Gifts by contact page only.'
         where name = 'South Fixture Zendo'",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    let cookie = join(&fx, "owner").await;
    let (permalink, id, token) = ask_as(&fx, &cookie).await;
    let page = send(&fx.app, get(&permalink, Some(&cookie))).await;
    assert!(
        page.body
            .contains("Optional gift to this site (US dollars)")
    );
    let r = send(
        &fx.app,
        fragment_post(
            &format!("{permalink}/dana"),
            &cookie,
            &format!("total=20&site=5&csrf={}", encode(&token)),
        ),
    )
    .await;
    assert!(
        r.body.contains("North Fixture Zendo: $20.00."),
        "{}",
        r.body
    );
    assert!(
        !r.body.contains("South Fixture Zendo: $"),
        "a contact page drops out"
    );
    assert!(
        r.body
            .contains("South Fixture Zendo: Gifts by contact page only."),
        "with its note"
    );
    assert!(r.body.contains("This site, optional: $5.00."));
    assert!(r.body.contains(&format!("href=\"/give/{id}/site\"")));
    assert!(r.body.contains("Total you would give: $25.00"));

    // With no eligible sangha the panel says so plainly.
    wt_db::donation::unverify(&fx.pool, "North Fixture Zendo", "checking again", "test")
        .await
        .unwrap();
    let r = send(
        &fx.app,
        fragment_post(
            &format!("{permalink}/dana"),
            &cookie,
            &format!("total=20&csrf={}", encode(&token)),
        ),
    )
    .await;
    assert!(
        r.body
            .contains("None of the sanghas in this answer has a verified giving page yet")
    );
}

#[tokio::test]
async fn give_redirects_only_to_the_registry_link_and_logs_each_click() {
    let fx = dana_fixture(Some(SITE_URL)).await;
    let cookie = join(&fx, "owner").await;
    let (_, id, _) = ask_as(&fx, &cookie).await;
    let sangha = |name: &str| {
        let name = name.to_owned();
        let pool = fx.pool.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>("select id from sangha where name = $1")
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let north = sangha("North Fixture Zendo").await;
    let south = sangha("South Fixture Zendo").await;

    let r = send(
        &fx.app,
        get(
            &format!("/give/{id}/{north}?amount_cents=500"),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.headers[header::LOCATION],
        NORTH_URL,
        "no template, the page itself"
    );
    let r = send(
        &fx.app,
        get(
            &format!("/give/{id}/{south}?amount_cents=1234"),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(
        r.headers[header::LOCATION].to_str().unwrap(),
        format!("{SOUTH_URL}?amount=12.34"),
        "the verified template takes the amount"
    );
    let r = send(&fx.app, get(&format!("/give/{id}/site"), Some(&cookie))).await;
    assert_eq!(r.headers[header::LOCATION], SITE_URL);
    let clicks: Vec<Option<Uuid>> = sqlx::query_scalar(
        "select sangha_id from dana_event where kind = 'link_click' order by created_at",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(clicks, [Some(north), Some(south), None]);

    // A tampered amount is refused, and logs nothing.
    for bad in ["abc", "-1", "10000001", "1.5"] {
        let r = send(
            &fx.app,
            get(
                &format!("/give/{id}/{south}?amount_cents={bad}"),
                Some(&cookie),
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{bad}");
    }
    // Unverified and unlisted sanghas are refused.
    wt_db::donation::unverify(&fx.pool, "South Fixture Zendo", "checking", "test")
        .await
        .unwrap();
    let r = send(&fx.app, get(&format!("/give/{id}/{south}"), Some(&cookie))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "unverified");
    sqlx::query("update sangha set listed = false where id = $1")
        .bind(north)
        .execute(&fx.pool)
        .await
        .unwrap();
    let r = send(&fx.app, get(&format!("/give/{id}/{north}"), Some(&cookie))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "unlisted");
    assert_eq!(events(&fx.pool, "link_click").await, 3);

    // Someone else's answer, and the site line without a site page.
    let other = join(&fx, "someone else").await;
    let r = send(&fx.app, get(&format!("/give/{id}/site"), Some(&other))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let fx2 = dana_fixture(None).await;
    let cookie2 = join(&fx2, "owner").await;
    let (_, id2, _) = ask_as(&fx2, &cookie2).await;
    let r = send(&fx2.app, get(&format!("/give/{id2}/site"), Some(&cookie2))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "no site page configured");
}

#[tokio::test]
async fn the_dana_script_is_served_from_the_site() {
    let fx = start().await;
    let r = send(&fx.app, get("/static/dana.js", None)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    assert!(r.body.contains("dana-result"));
    assert!(!r.body.contains("http"), "no external host");
}

/// The answer page's excerpt footer (Spec 06, Answer page structure, rules 2 and 5, owner's
/// decisions of October 12 and 15, 2026), with ids, the token and the access date replaced so the
/// snapshot is stable.
#[tokio::test]
async fn the_excerpt_footer_reads_listen_read_cite_give_report() {
    let fx = start().await;
    let page = ask(&fx, "How do I work with anger?").await;
    assert_eq!(page.status, StatusCode::OK);
    let body = &page.body;
    assert!(
        !body.contains(">Not faithful</button>"),
        "the old buttons are gone"
    );
    assert!(
        body.contains("<div class=\"sticky-q\" aria-hidden=\"true\" hidden data-sticky-q>"),
        "the sticky copy is hidden from assistive technology and starts hidden"
    );
    let start = body.find("<ul class=\"excerpt-links\">").unwrap();
    let end = body[start..].find("</article>").unwrap() + start;
    let mut fragment = body[start..end].to_owned();
    let uuid =
        regex::Regex::new("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap();
    fragment = uuid.replace_all(&fragment, "[id]").into_owned();
    fragment = fragment.replace(&csrf(body), "[csrf]");
    let accessed = wt_api::view::mla_date(chrono::Utc::now().date_naive());
    fragment = fragment.replace(&accessed, "[accessed]");
    insta::assert_snapshot!(fragment);
}

/// Asks about anger and returns the cookie, the token, the permalink and the passage shown.
async fn asked(fx: &Fx) -> (String, String, String, Uuid) {
    let cookie = join(fx, "owner").await;
    let token = csrf(&send(&fx.app, get("/", Some(&cookie))).await.body);
    let r = send(
        &fx.app,
        post("/ask", &cookie, &format!("q=anger&csrf={}", encode(&token))),
    )
    .await;
    let permalink = r.headers[header::LOCATION].to_str().unwrap().to_owned();
    let passage: Uuid = sqlx::query_scalar("select passage_id from answer_passage")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    (cookie, token, permalink, passage)
}

/// A form post as the page's script sends it.
fn hx(path: &str, cookie: &str, form: &str) -> Request<Body> {
    let mut r = post(path, cookie, form);
    r.headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    r
}

async fn feedback_rows(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from feedback")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The page shows one control now; a post from an excerpt control on a page served before still
/// records.
#[tokio::test]
async fn excerpt_feedback_takes_several_reasons_and_a_note_in_one_submission() {
    let fx = start().await;
    let (cookie, token, permalink, passage) = asked(&fx).await;
    let fb = format!("{permalink}/feedback");
    let base = format!(
        "csrf={}&target=excerpt&n=1&passage={passage}",
        encode(&token)
    );

    let r = send(
        &fx.app,
        post(
            &fb,
            &cookie,
            &format!(
                "{base}&reason=good&reason=unclear&note={}",
                encode("Reach me at someone@example.com")
            ),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let to = r.headers[header::LOCATION].to_str().unwrap().to_owned();
    assert_eq!(to, format!("{permalink}?fb=p1&fbs=ok#fb-p1"));
    let rows: Vec<(String, String, Option<Uuid>, Option<String>)> =
        sqlx::query_as("select kind, target, passage_id, note from feedback order by kind")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["good", "note", "unclear"]
    );
    assert!(
        rows.iter()
            .all(|r| r.1 == "excerpt" && r.2 == Some(passage))
    );
    let note = rows[1].3.as_deref().unwrap();
    assert!(!note.contains("someone@example.com"), "{note}");
    let submissions: i64 = sqlx::query_scalar("select count(distinct submission_id) from feedback")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(submissions, 1);

    // With the script the status comes back as text for the live region.
    let r = send(
        &fx.app,
        hx(&fb, &cookie, &format!("{base}&reason=not_relevant")),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.body,
        "Thank you. Your feedback on excerpt 1 was recorded."
    );

    // Nothing chosen, a summary reason on an excerpt and a note that is too long are refused.
    let before = feedback_rows(&fx.pool).await;
    let r = send(&fx.app, hx(&fb, &cookie, &format!("{base}&note=+++"))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body, "Choose at least one reason or write a note.");
    let r = send(
        &fx.app,
        post(&fb, &cookie, &format!("{base}&reason=unsupported")),
    )
    .await;
    assert_eq!(
        r.headers[header::LOCATION].to_str().unwrap(),
        format!("{permalink}?fb=p1&fbs=empty#fb-p1")
    );
    let long = "a".repeat(1001);
    let r = send(&fx.app, hx(&fb, &cookie, &format!("{base}&note={long}"))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body, "Please keep the note under 1,000 characters.");
    assert_eq!(feedback_rows(&fx.pool).await, before);

    // A passage that is not in the answer, and another tester, get the generic 404.
    let stranger = Uuid::new_v4();
    let r = send(
        &fx.app,
        post(
            &fb,
            &cookie,
            &format!(
                "csrf={}&target=excerpt&n=1&passage={stranger}&reason=good",
                encode(&token)
            ),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let other = join(&fx, "someone else").await;
    let other_token = csrf(&send(&fx.app, get("/", Some(&other))).await.body);
    let r = send(
        &fx.app,
        post(
            &fb,
            &other,
            &format!(
                "csrf={}&target=excerpt&n=1&passage={passage}&reason=good",
                encode(&other_token)
            ),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(feedback_rows(&fx.pool).await, before);
}

#[tokio::test]
async fn one_feedback_control_on_these_answers_closes_the_page() {
    let fx = start().await;
    let (cookie, token, permalink, _) = asked(&fx).await;
    let page = send(&fx.app, get(&permalink, Some(&cookie))).await;
    let body = &page.body;
    assert_eq!(body.matches("<details class=\"fb\"").count(), 1, "{body}");
    assert!(!body.contains("Feedback on excerpt"), "{body}");
    let control = body
        .find("<summary>Feedback on these answers</summary>")
        .unwrap();
    let last_excerpt = body.rfind("<article class=\"passage\"").unwrap();
    assert!(control > last_excerpt, "the control follows the excerpts");
    assert!(body.contains("name=\"target\" value=\"answer\""));

    let fb = format!("{permalink}/feedback");
    let base = format!("csrf={}&target=answer", encode(&token));
    let r = send(
        &fx.app,
        post(
            &fb,
            &cookie,
            &format!("{base}&reason=not_relevant&reason=unsupported&note=thin"),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let to = r.headers[header::LOCATION].to_str().unwrap().to_owned();
    assert_eq!(to, format!("{permalink}?fb=answer&fbs=ok#fb-answer"));
    let rows: Vec<(String, String, Option<Uuid>)> =
        sqlx::query_as("select kind, target, passage_id from feedback order by kind")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            ("note".to_owned(), "answer".to_owned(), None),
            ("not_relevant".to_owned(), "answer".to_owned(), None),
            ("unsupported".to_owned(), "answer".to_owned(), None),
        ]
    );

    // Without the script the page comes back with the control open and the status in it.
    let back = to.split('#').next().unwrap();
    let page = send(&fx.app, get(back, Some(&cookie))).await;
    assert!(
        page.body.contains("id=\"fb-answer\" open>"),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains("Thank you. Your feedback on these answers was recorded.</p>")
    );
    // A summary-only reason is not an answer reason.
    let r = send(
        &fx.app,
        hx(&fb, &cookie, &format!("{base}&reason=misrepresents")),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn summary_feedback_records_its_own_reasons() {
    let fx = start().await;
    let (cookie, token, permalink, _) = asked(&fx).await;
    let fb = format!("{permalink}/feedback");
    let r = send(
        &fx.app,
        hx(
            &fb,
            &cookie,
            &format!(
                "csrf={}&target=summary&reason=unsupported&reason=misrepresents&reason=missing",
                encode(&token)
            ),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.body,
        "Thank you. Your feedback on the summary was recorded."
    );
    let rows: Vec<(String, String, Option<Uuid>)> =
        sqlx::query_as("select kind, target, passage_id from feedback order by kind")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            ("misrepresents".to_owned(), "summary".to_owned(), None),
            ("missing".to_owned(), "summary".to_owned(), None),
            ("unsupported".to_owned(), "summary".to_owned(), None),
        ]
    );
    // An excerpt reason is not a summary reason.
    let r = send(
        &fx.app,
        hx(
            &fb,
            &cookie,
            &format!("csrf={}&target=summary&reason=not_faithful", encode(&token)),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}
