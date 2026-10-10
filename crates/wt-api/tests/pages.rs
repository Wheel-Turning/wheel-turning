//! Spec 06 acceptance for the pages the alpha uses: invite access, the session gate, CSRF,
//! asking, the owner-only permalink, escaping, feedback and the security headers. Synthetic
//! teachers and passages only.
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
const ANGER: &str = "When anger arises, turn toward the anger and breathe with it. \
                     <script>alert(1)</script> The bell rang twice before the service.";

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
    passage(&pool, ANGER).await;
    let app = router(AppState {
        pool: pool.clone(),
        site: Arc::new(Site::with_fakes(MODEL)),
    });
    Fx { _db: db, pool, app }
}

/// A ready, consented talk by Ada with one passage.
async fn passage(pool: &PgPool, text: &str) {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('youtube', 'vid1', 'https://example.invalid/v', 'Fixture talk') returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, 'On anger', te.id, te.sangha_id, 'Ada Fixture', 'awaiting_consent'
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
