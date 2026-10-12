//! Spec 05, Sensitive questions, and Spec 06, Rendering sensitive questions: what `POST /ask`
//! does and stores for acute, distress and medical questions, and the curated query. Synthetic
//! questions, teachers and passages only.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use async_trait::async_trait;
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
use wt_extract::{FakeLlm, LlmClient};
use wt_ml::FakeEmbedder;
use wt_registry::RegistryFile;
use wt_retrieve::{Level, ModelCheck};

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
const ANGER: &str = "When anger arises, turn toward the anger and breathe with it. The bell \
                     rang twice before the service.";

const ACUTE: &str = "I want to end my life because of this anger";
const DISTRESS: &str = "I feel hopeless about my anger";
const MEDICAL: &str = "Should my medication change how I work with anger?";
const HEADING: &str = "Please reach out for help now";

struct Fx {
    _db: TestDb,
    pool: PgPool,
    app: Router,
    cookie: String,
}

async fn start(site: Site) -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let file = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &file, ACTOR).await.unwrap();
    // A verified giving link, so the give button would show if the page allowed it.
    sqlx::query("update sangha set donation_verified = true, listed = true")
        .execute(&pool)
        .await
        .unwrap();
    passage(&pool).await;
    let app = router(AppState {
        pool: pool.clone(),
        site: Arc::new(site),
    });
    let token = session::new_token().unwrap();
    wt_db::tester::invite(&pool, "tester", &session::token_hash(&token))
        .await
        .unwrap();
    let joined = send(&app, get(&format!("/join/{token}"), None)).await;
    let set = joined.headers[header::SET_COOKIE].to_str().unwrap();
    let cookie = set
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("wt_session=")
        .to_owned();
    Fx {
        _db: db,
        pool,
        app,
        cookie,
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

fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut r = Request::get(path);
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, format!("wt_session={c}"));
    }
    r.body(Body::empty()).unwrap()
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

impl Fx {
    async fn csrf(&self) -> String {
        let home = send(&self.app, get("/", Some(&self.cookie))).await;
        let at = home.body.find("name=\"csrf\" value=\"").unwrap() + 19;
        home.body[at..].split('"').next().unwrap().to_owned()
    }

    async fn post(&self, path: &str, form: &str) -> Reply {
        let token = self.csrf().await;
        let req = Request::post(path)
            .header(header::COOKIE, format!("wt_session={}", self.cookie))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!("{form}&csrf={}", encode(&token))))
            .unwrap();
        send(&self.app, req).await
    }

    async fn ask(&self, question: &str) -> Reply {
        self.post("/ask", &format!("q={}", encode(question))).await
    }

    /// Asks and follows the redirect to the answer page.
    async fn answer(&self, question: &str) -> Reply {
        let asked = self.ask(question).await;
        assert_eq!(asked.status, StatusCode::SEE_OTHER, "{}", asked.body);
        self.follow(&asked).await
    }

    async fn follow(&self, r: &Reply) -> Reply {
        let to = r.headers[header::LOCATION].to_str().unwrap();
        send(&self.app, get(to, Some(&self.cookie))).await
    }

    async fn counts(&self) -> (i64, i64) {
        sqlx::query_as("select (select count(*) from query_log), (select count(*) from answer)")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

fn themes() -> Arc<FakeLlm> {
    let theme = serde_json::json!({ "themes": [{
        "description": "Both passages turn toward anger.", "passage_refs": ["P1", "P2"] }] });
    Arc::new(FakeLlm::new(vec![
        Ok(theme),
        Ok(serde_json::json!({ "supported": true, "unsupported_part": null })),
    ]))
}

fn site(llm: Arc<dyn LlmClient>) -> Site {
    Site {
        llm,
        ..Site::with_fakes(MODEL)
    }
}

/// The text between `<main` and `</main>`.
fn main_of(body: &str) -> &str {
    &body[body.find("<main").unwrap()..body.find("</main>").unwrap()]
}

#[tokio::test]
async fn an_acute_question_gets_the_notice_page_and_stores_counts_only() {
    let llm = themes();
    let fx = start(site(llm.clone())).await;
    let r = fx.ask(ACUTE).await;

    assert_eq!(r.status, StatusCode::OK, "a page, not a redirect");
    assert!(r.headers.get(header::LOCATION).is_none());
    assert_eq!(r.headers[header::CACHE_CONTROL], "no-store");
    let main = main_of(&r.body);
    assert!(
        main.contains(&format!(
            "<h1 id=\"support-h\" tabindex=\"-1\" autofocus>{HEADING}</h1>"
        )),
        "the notice is the h1 and takes focus: {main}"
    );
    assert!(main.contains("aria-labelledby=\"support-h\""));
    for link in [
        "<a href=\"tel:988\">call 988</a>",
        "<a href=\"sms:988\">text 988</a>",
        "<a href=\"https://988lifeline.org/\">988lifeline.org</a>",
        "<a href=\"tel:911\">call 911</a>",
        "<a href=\"https://findahelpline.com/\">findahelpline.com</a>",
        "Show teachings on facing difficult feelings",
        "<a href=\"/\">Ask something else</a>",
    ] {
        assert!(main.contains(link), "{link} in {main}");
    }
    for absent in [
        "Flag this query",
        "feedback",
        "/give/",
        "/a/",
        "Give to",
        "blockquote",
        "detected",
        ACUTE,
    ] {
        assert!(!r.body.contains(absent), "{absent} must not appear");
    }
    assert!(llm.seen.lock().unwrap().is_empty(), "no model call at all");

    assert_eq!(fx.counts().await, (1, 0), "a log row and no answer row");
    let row: (Option<String>, String, i32, i32, i32, i32) = sqlx::query_as(
        "select query_text, sensitive_category, teacher_count, sangha_count, passage_count,
                theme_count from query_log",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(row, (None, "acute".to_owned(), 0, 0, 0, 0));
}

#[tokio::test]
async fn the_database_refuses_acute_text_and_analysis_never_sees_acute_or_distress() {
    let fx = start(site(themes())).await;
    fx.ask(ACUTE).await;
    fx.answer(DISTRESS).await;
    fx.answer(MEDICAL).await;
    fx.answer("How do I work with anger?").await;

    let stored: Vec<(String, bool)> = sqlx::query_as(
        "select sensitive_category, query_text is not null from query_log order by created_at",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    let categories: Vec<&str> = stored.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(categories, ["acute", "distress", "medical", "none"]);

    let analyzable: Vec<String> = sqlx::query_scalar(
        "select sensitive_category from analyzable_query_log order by sensitive_category",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(analyzable, ["medical", "none"]);

    let refused =
        sqlx::query("update query_log set query_text = 'kept' where sensitive_category = 'acute'")
            .execute(&fx.pool)
            .await;
    assert!(refused.is_err(), "an acute row can never hold text");
}

#[tokio::test]
async fn a_distress_answer_has_the_banner_and_no_summary_line_or_giving_panel() {
    let llm = themes();
    let fx = start(site(llm.clone())).await;
    let page = fx.answer(DISTRESS).await;
    assert_eq!(page.status, StatusCode::OK);
    let main = main_of(&page.body);
    let banner = main.find("you do not have to carry it alone").unwrap();
    assert!(
        banner < main.find("Your question").unwrap(),
        "above the question"
    );
    assert!(main.contains("aria-labelledby=\"support-h\""));
    assert!(main.contains("<a href=\"sms:988\">text 988</a>"));
    assert!(main.contains("href=\"tel:988\""));
    assert!(
        page.body.contains("<blockquote>"),
        "the answer itself is shown"
    );
    assert!(!page.body.contains("How approaches differ"));
    assert!(!page.body.contains("Give to"), "{}", page.body);
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "no synthesis call for a distress question"
    );
}

#[tokio::test]
async fn a_medical_answer_has_its_banner_and_keeps_the_giving_panel() {
    let llm = themes();
    let fx = start(site(llm.clone())).await;
    let page = fx.answer(MEDICAL).await;
    let main = main_of(&page.body);
    let banner = main.find("It is not medical advice.").unwrap();
    assert!(banner < main.find("Your question").unwrap());
    assert!(main.contains("<a href=\"tel:911\">call 911</a>"));
    assert!(!page.body.contains("How approaches differ"));
    assert!(page.body.contains("Give to North Fixture Zendo"));
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_ordinary_question_has_no_banner_and_keeps_both() {
    let fx = start(site(themes())).await;
    let page = fx.answer("How do I work with anger?").await;
    assert!(!page.body.contains("support-h"));
    assert!(page.body.contains("Give to North Fixture Zendo"));
}

#[tokio::test]
async fn the_curated_query_runs_with_the_notice_on_top_and_neither_summary_nor_giving() {
    let llm = themes();
    let fx = start(site(llm.clone())).await;
    let acute = fx.ask(ACUTE).await;
    assert!(acute.body.contains("action=\"/ask/curated\""));

    let r = fx.post("/ask/curated", "").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = fx.follow(&r).await;
    assert_eq!(page.status, StatusCode::OK);
    let main = main_of(&page.body);
    let notice = main
        .find(&format!("<h2 id=\"support-h\">{HEADING}</h2>"))
        .unwrap();
    assert!(notice < main.find("Your question").unwrap(), "on top");
    assert!(main.contains("<a href=\"tel:988\">call 988</a>"));
    assert!(!page.body.contains("How approaches differ"));
    assert!(!page.body.contains("Give to"));
    assert!(llm.seen.lock().unwrap().is_empty());

    let stored: (Option<String>, String, bool) = sqlx::query_as(
        "select q.query_text, q.sensitive_category, (a.payload->>'curated')::boolean
         from query_log q join answer a on a.query_log_id = q.id",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let curated = wt_retrieve::sensitive::SensitiveConfig::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/sensitive.toml"),
    )
    .unwrap()
    .curated_query;
    assert_eq!(stored, (Some(curated), "distress".to_owned(), true));

    let refused = send(
        &fx.app,
        Request::post("/ask/curated")
            .header(header::COOKIE, format!("wt_session={}", fx.cookie))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("csrf=wrong"))
            .unwrap(),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
}

/// A model layer that always answers `level`.
struct Says(Level);

#[async_trait]
impl ModelCheck for Says {
    async fn level(&self, _: &str) -> Result<Level, String> {
        Ok(self.0)
    }
}

/// A model layer that always fails, as when the monthly cap is reached.
struct CapReached;

#[async_trait]
impl ModelCheck for CapReached {
    async fn level(&self, _: &str) -> Result<Level, String> {
        Err("the monthly language-model cap is reached".into())
    }
}

#[tokio::test]
async fn the_model_layer_can_raise_a_question_to_acute_and_never_lowers_one() {
    // The rules say distress and the hashing embedder matches no exemplar: the layers disagree,
    // so the model is asked.
    let fx = start(Site {
        sensitivity_model: Some(Arc::new(Says(Level::Acute))),
        ..site(themes())
    })
    .await;
    let r = fx.ask(DISTRESS).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains(HEADING));

    for model in [
        Arc::new(Says(Level::None)) as Arc<dyn ModelCheck>,
        Arc::new(CapReached),
    ] {
        let fx = start(Site {
            sensitivity_model: Some(model),
            ..site(themes())
        })
        .await;
        let page = fx.answer(DISTRESS).await;
        assert!(page.body.contains("you do not have to carry it alone"));
    }
}

#[tokio::test]
async fn the_ask_limit_never_hides_the_notice_from_an_acute_question() {
    let fx = start(Site {
        limits: wt_api::Limits::new(1),
        ..site(themes())
    })
    .await;
    fx.answer("How do I work with anger?").await;
    let limited = fx.ask("How do I bow?").await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    let r = fx.ask(ACUTE).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains(HEADING));
    assert_eq!(r.headers[header::CACHE_CONTROL], "no-store");
}
