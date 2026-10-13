//! The server the `a11y` CI job checks (Spec 06, Accessibility checks in CI): it migrates the
//! empty database at `DATABASE_URL`, seeds synthetic teachers, talks, passages and sources,
//! starts the app with the fake embedder and a scripted language model, asks one question to
//! make an answer with a verified theme, writes the paths the browser visits as JSON to the file
//! named by its first argument, and serves on `127.0.0.1:$A11Y_PORT` (default 8099) until
//! stopped. Nothing here calls a network service. The embedder waits a little on each call so
//! the browser can see the progress section while a question runs.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::json;
use sqlx::PgPool;
use sqlx::types::Uuid;
use tower::ServiceExt;
use wt_api::{AppState, Site, router, session};
use wt_core::gate::ConsentStatus;
use wt_extract::FakeLlm;
use wt_ml::{Embedder, FakeEmbedder, MlError};
use wt_registry::RegistryFile;

const REGISTRY: &str = r#"
[[sangha]]
name = "North Fixture Zendo"
donation_url = "https://example.invalid/north"

[[sangha]]
name = "South Fixture Zendo"

[[teacher]]
display_name = "Ada Fixture"
sangha = "North Fixture Zendo"

[[teacher]]
display_name = "Bo Fixture"
sangha = "South Fixture Zendo"
"#;

const MODEL: &str = "fake-embed";
const ACTOR: &str = "a11y-fixture";
const QUESTION: &str = "How do I work with anger?";
const THEME: &str = "Both teachers suggest turning toward anger and breathing with it.";

/// Synthetic passages: teacher, video id, talk title and text.
const PASSAGES: [(&str, &str, &str, &str); 3] = [
    (
        "Ada Fixture",
        "a11y1",
        "On anger",
        "When anger arises, turn toward the anger and breathe with it. Let the breath be long \
         and the body be still, and see what the anger is made of before you act on it.",
    ),
    (
        "Ada Fixture",
        "a11y1",
        "On anger",
        "After the bell, we sat with the anger again. It was not an enemy. It was heat in the \
         chest and words in the mind, and both changed while we watched them.",
    ),
    (
        "Bo Fixture",
        "a11y2",
        "Anger and the breath",
        "Anger is only energy. Sit with the anger, turn toward it, and breathe until it softens \
         on its own. Nothing needs to be pushed away for practice to go on.",
    ),
];

/// The fake embedder, slowed so a question runs long enough to see its progress.
struct SlowEmbedder(FakeEmbedder);

#[async_trait]
impl Embedder for SlowEmbedder {
    fn model_id(&self) -> &str {
        self.0.model_id()
    }

    fn dim(&self) -> usize {
        self.0.dim()
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MlError> {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        self.0.embed(texts).await
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .context("usage: a11y_fixture <paths.json>")?;
    let url = std::env::var("DATABASE_URL").context("DATABASE_URL")?;
    let port: u16 = std::env::var("A11Y_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8099);

    let pool = PgPool::connect(&url).await.context("connecting")?;
    wt_db::migrate(&pool).await.context("migrating")?;
    seed(&pool).await?;

    let llm = Arc::new(FakeLlm::new(vec![
        Ok(json!({ "themes": [{ "description": THEME, "passage_refs": ["P1", "P2"] }] })),
        Ok(json!({ "supported": true, "unsupported_part": null })),
    ]));
    let mut site = Site::with_fakes(MODEL);
    site.embedder = Arc::new(SlowEmbedder(FakeEmbedder::new(MODEL)));
    site.llm = llm;
    site.site_dana_url = Some("https://example.invalid/site".to_owned());
    let app = router(AppState {
        pool: pool.clone(),
        site: Arc::new(site),
    });

    let token = session::new_token().context("making a token")?;
    wt_db::tester::invite(&pool, "a11y", &session::token_hash(&token)).await?;
    let join = format!("/join/{token}");
    let answer = ask(&app, &join).await?;
    let talk: Uuid = sqlx::query_scalar("select id from talk where title = 'On anger'")
        .fetch_one(&pool)
        .await?;
    let paths = json!({
        "join": join,
        "answer": answer,
        "talk": format!("/talk/{talk}"),
        "transcript": format!("/talk/{talk}/transcript"),
    });
    std::fs::write(&out, serde_json::to_string_pretty(&paths)?)
        .with_context(|| format!("writing {out}"))?;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("binding port {port}"))?;
    println!("a11y fixture serving on http://127.0.0.1:{port}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("serving")
}

/// Joins in-process, asks [`QUESTION`] and returns the permalink.
async fn ask(app: &axum::Router, join: &str) -> anyhow::Result<String> {
    let joined = app
        .clone()
        .oneshot(Request::get(join).body(Body::empty())?)
        .await?;
    let cookie = joined
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("no session cookie")?
        .to_owned();
    let home = app
        .clone()
        .oneshot(
            Request::get("/")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())?,
        )
        .await?;
    let body = String::from_utf8(home.into_body().collect().await?.to_bytes().to_vec())?;
    let marker = "name=\"csrf\" value=\"";
    let at = body.find(marker).context("no CSRF token")? + marker.len();
    let csrf = body[at..].split('"').next().unwrap_or_default();
    let asked = app
        .clone()
        .oneshot(
            Request::post("/ask")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "q={}&csrf={csrf}",
                    QUESTION.replace(' ', "+").replace('?', "%3F")
                )))?,
        )
        .await?;
    if asked.status() != StatusCode::SEE_OTHER {
        bail!("asking returned {}", asked.status());
    }
    Ok(asked
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .context("no permalink")?
        .to_owned())
}

async fn seed(pool: &PgPool) -> anyhow::Result<()> {
    let file = RegistryFile::parse(REGISTRY, "a11y fixture")?;
    wt_db::registry::import(pool, &file, ACTOR).await?;
    wt_db::donation::verify(
        pool,
        "North Fixture Zendo",
        "https://example.invalid/north",
        ACTOR,
    )
    .await?;
    // Talks wait for consent, as the gate requires, and are ready once it is recorded.
    let mut talks = Vec::new();
    for (teacher, video_id, title, _) in PASSAGES {
        talks.push(talk(pool, teacher, video_id, title).await?);
    }
    for teacher in ["Ada Fixture", "Bo Fixture"] {
        wt_db::consent::set_consent(
            pool,
            teacher,
            ConsentStatus::Granted,
            Some("a11y"),
            None,
            ACTOR,
        )
        .await?;
    }
    sqlx::query("update talk set state = 'ready', content_hash = 'h'")
        .execute(pool)
        .await?;
    let mut transcripts = std::collections::HashMap::new();
    for (ordinal, (talk, (_, _, _, text))) in talks.iter().zip(PASSAGES).enumerate() {
        let transcript = match transcripts.get(talk) {
            Some(t) => *t,
            None => {
                let t: Uuid = sqlx::query_scalar(
                    "insert into transcript (talk_id, version, origin, s3_key, content_hash)
                     values ($1, 1, 'asr', 'k', 'h') returning id",
                )
                .bind(talk)
                .fetch_one(pool)
                .await?;
                transcripts.insert(*talk, t);
                t
            }
        };
        let ordinal = i32::try_from(ordinal)?;
        let start_ms = 65_000 + ordinal * 120_000;
        let segment: Uuid = sqlx::query_scalar(
            "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
             values ($1, $2, $3, 'talk', $4, $4 + 60000, $5) returning id",
        )
        .bind(talk)
        .bind(transcript)
        .bind(ordinal)
        .bind(start_ms)
        .bind(text)
        .fetch_one(pool)
        .await?;
        let v = wt_retrieve::search::vector_literal(&FakeEmbedder::vector(text));
        sqlx::query(
            "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms,
                                  text, word_count, content_hash, embedding, embedding_model)
             values ($1, $2, 'talk', $3, $4, $4 + 60000, $5, 40, 'h', $6::text::vector, $7)",
        )
        .bind(talk)
        .bind(segment)
        .bind(ordinal)
        .bind(start_ms)
        .bind(text)
        .bind(v)
        .bind(MODEL)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        "insert into source_policy (key, publisher, url_prefixes, basis, kind) values
           ('mand', 'Mandate Fixture Press', '{https://m.invalid/}', 'mandate', 'texts'),
           ('perm', 'Permission Fixture Zendo', '{https://p.invalid/}', 'permission', 'talks')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The talk `title` by `teacher`, made on first use.
async fn talk(pool: &PgPool, teacher: &str, video_id: &str, title: &str) -> anyhow::Result<Uuid> {
    if let Some(id) = sqlx::query_scalar("select id from talk where title = $1")
        .bind(title)
        .fetch_optional(pool)
        .await?
    {
        return Ok(id);
    }
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('youtube', $1, 'https://example.invalid/' || $1, $2) returning id",
    )
    .bind(video_id)
    .bind(title)
    .fetch_one(pool)
    .await?;
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, $2, te.id, te.sangha_id, te.display_name, 'awaiting_consent'
         from teacher te where te.display_name = $3 returning id",
    )
    .bind(video)
    .bind(title)
    .bind(teacher)
    .fetch_one(pool)
    .await?;
    Ok(talk)
}
