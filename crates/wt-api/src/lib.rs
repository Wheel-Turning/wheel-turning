//! The Axum web app: routes, templates and invite auth (Spec 06).
//!
//! Every route but `/healthz`, `/robots.txt`, `/static/*` and `/join/{token}` needs a session,
//! and answers the same generic 404 without one. Passage text reaches the templates only from
//! `servable_passage`, and the templates escape it. Model text reaches only the "How approaches
//! differ" section, as verified themes whose passages the code, not the model, names.

pub mod session;
pub mod view;

use std::sync::Arc;
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use wt_core::ids::{AnswerId, SanghaId, TalkId, TesterId};
use wt_db::answer::{Payload, Served};
use wt_extract::{DataClass, LlmClient, UnavailableLlm};
use wt_ml::{Embedder, FakeEmbedder, Reranker};
use wt_retrieve::{Mode, NamedDetector, Notice, RetrievalConfig, query, retrieve};
use wt_synth::{Synthesis, SynthesisInput};

use session::Keys;

/// Everything the routes share besides the pool: models, settings and the signing key.
pub struct Site {
    /// Embeds questions.
    pub embedder: Arc<dyn Embedder>,
    /// Scores candidates; `None` until the reranker model is installed.
    pub reranker: Option<Arc<dyn Reranker>>,
    /// Drafts and verifies synthesis themes; [`UnavailableLlm`] when no model may be called,
    /// which leaves every answer without a synthesis line.
    pub llm: Arc<dyn LlmClient>,
    /// `config/retrieval.toml`.
    pub retrieval: RetrievalConfig,
    /// `config/common_words.txt`.
    pub common_words: String,
    /// Signs session cookies and CSRF tokens.
    pub keys: Keys,
    /// Marks the cookie `Secure`; everywhere but `dev`.
    pub secure_cookies: bool,
}

impl Site {
    /// A site with a fake embedder, no reranker, no language model and a fixed key, for tests.
    pub fn with_fakes(embedder_model: &str) -> Self {
        Self {
            embedder: Arc::new(FakeEmbedder::new(embedder_model)),
            reranker: None,
            llm: Arc::new(UnavailableLlm),
            retrieval: RetrievalConfig::default(),
            common_words: String::new(),
            keys: Keys::new(b"test-only-key"),
            secure_cookies: false,
        }
    }

    /// `[synthesis]` as the synthesizer takes it.
    fn synthesis(&self) -> wt_synth::Settings {
        let c = &self.retrieval.synthesis;
        wt_synth::Settings {
            max_themes: c.max_themes,
            min_grounding_ratio: c.min_grounding_ratio,
            timeout: Duration::from_secs(c.synthesis_timeout_s),
        }
    }
}

/// Shared state handed to every route.
#[derive(Clone)]
pub struct AppState {
    /// The application role's connection pool.
    pub pool: PgPool,
    /// Models and settings.
    pub site: Arc<Site>,
}

/// Builds the router with every route.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/robots.txt", get(robots))
        .route("/static/app.css", get(stylesheet))
        .route("/join/{token}", get(join))
        .route("/", get(home))
        .route("/about", get(about))
        .route("/ask", post(ask))
        .route("/a/{answer}", get(answer_page))
        .route("/a/{answer}/feedback", post(feedback))
        .route("/give/{answer}/{sangha}", get(give))
        .route("/talk/{talk}", get(talk_page))
        .route("/talk/{talk}/transcript", get(transcript_page))
        .fallback(not_found)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

/// The `/healthz` response body.
#[derive(Debug, Serialize)]
pub struct Health {
    /// `ok` when every dependency answered, otherwise `unavailable`.
    pub status: &'static str,
    /// `ok` when the database answered a query.
    pub database: &'static str,
}

/// Reports whether the service and its database are healthy. Returns 503 when the database does
/// not answer, so the load balancer stops sending traffic.
async fn healthz(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    match wt_db::ping(&state.pool).await {
        Ok(()) => (
            StatusCode::OK,
            Json(Health {
                status: "ok",
                database: "ok",
            }),
        ),
        Err(error) => {
            tracing::warn!(error = %error, "health check: database unavailable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Health {
                    status: "unavailable",
                    database: "unavailable",
                }),
            )
        }
    }
}

/// Spec 06, rule 7 of invite access: headers on every response.
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    response
}

async fn robots() -> &'static str {
    "User-agent: *\nDisallow: /\n"
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/app.css"),
    )
}

// ---- pages ----------------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    title: &'a str,
    message: &'a str,
}

fn page(status: StatusCode, t: &impl Template) -> Response {
    match t.render() {
        Ok(html) => (status, Html(html)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "template");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong on our side.",
            )
                .into_response()
        }
    }
}

fn error_page(status: StatusCode, title: &str, message: &str) -> Response {
    page(status, &ErrorPage { title, message })
}

/// The generic 404, also what a request without a session gets.
fn not_found_page() -> Response {
    error_page(
        StatusCode::NOT_FOUND,
        "Page not found",
        "That page does not exist.",
    )
}

async fn not_found() -> Response {
    not_found_page()
}

/// Spec 09's server error. The cause goes to the log, never to the page.
fn server_error(what: &str, e: &dyn std::fmt::Display) -> Response {
    tracing::error!(error = %e, "{what}");
    error_page(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Something went wrong",
        "Something went wrong on our side. Your question was not saved. Please try again.",
    )
}

// ---- sessions -------------------------------------------------------------------------------

/// A verified session: the tester and the cookie value their CSRF tokens derive from.
pub struct Session {
    /// Who is signed in.
    pub tester: TesterId,
    cookie: String,
}

impl FromRequestParts<AppState> for Session {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let value = parts
            .headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| session::cookie(h, session::COOKIE))
            .ok_or_else(not_found_page)?;
        let tester = state
            .site
            .keys
            .verify_cookie(value, chrono::Utc::now().timestamp())
            .ok_or_else(not_found_page)?;
        match wt_db::tester::active(&state.pool, tester).await {
            Ok(true) => Ok(Session {
                tester,
                cookie: value.to_owned(),
            }),
            Ok(false) => Err(not_found_page()),
            Err(e) => Err(server_error("checking the session", &e)),
        }
    }
}

impl Session {
    fn csrf(&self, keys: &Keys) -> String {
        keys.csrf(&self.cookie)
    }

    fn csrf_ok(&self, keys: &Keys, token: &str) -> bool {
        keys.check_csrf(&self.cookie, token)
    }
}

fn csrf_refused() -> Response {
    error_page(
        StatusCode::FORBIDDEN,
        "Please try again",
        "This form expired. Go back, reload the page and try again.",
    )
}

/// `GET /join/{token}`: exchanges an invite token for the session cookie. Any failure is the
/// same 404, with no hint. The token is never logged.
async fn join(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let hash = session::token_hash(&token);
    let tester = match wt_db::tester::by_token_hash(&state.pool, &hash).await {
        Ok(Some(t)) => t,
        Ok(None) => return not_found_page(),
        Err(e) => return server_error("joining", &e),
    };
    let value = state
        .site
        .keys
        .cookie_value(tester, chrono::Utc::now().timestamp());
    let secure = if state.site.secure_cookies {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        session::COOKIE,
        session::LIFETIME_S
    );
    let mut response = Redirect::to("/").into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, v);
    }
    response
}

// ---- home and ask ---------------------------------------------------------------------------

const EXAMPLES: [&str; 3] = [
    "I keep judging myself during zazen.",
    "How do I stay with grief without being swallowed by it?",
    "What is the difference between practice and trying to improve myself?",
];

#[derive(Template)]
#[template(path = "home.html")]
struct HomePage {
    csrf: String,
    question: String,
    error: Option<String>,
    examples: [&'static str; 3],
}

async fn home(State(state): State<AppState>, s: Session) -> Response {
    page(
        StatusCode::OK,
        &HomePage {
            csrf: s.csrf(&state.site.keys),
            question: String::new(),
            error: None,
            examples: EXAMPLES,
        },
    )
}

#[derive(Template)]
#[template(path = "about.html")]
struct AboutPage;

async fn about(_s: Session) -> Response {
    page(StatusCode::OK, &AboutPage)
}

#[derive(Debug, Deserialize)]
struct AskForm {
    q: String,
    csrf: String,
}

/// `POST /ask`: runs the query path, persists the answer and its log row together, and
/// redirects to the permalink.
async fn ask(State(state): State<AppState>, s: Session, Form(form): Form<AskForm>) -> Response {
    let site = &state.site;
    if !s.csrf_ok(&site.keys, &form.csrf) {
        return csrf_refused();
    }
    let started = Instant::now();
    let Ok(text) = query::normalize(&form.q) else {
        return page(
            StatusCode::BAD_REQUEST,
            &HomePage {
                csrf: s.csrf(&site.keys),
                question: form.q,
                error: Some("Please write between 3 and 500 characters.".to_owned()),
                examples: EXAMPLES,
            },
        );
    };
    let aliases = match wt_db::answer::aliases(&state.pool).await {
        Ok(a) => a,
        Err(e) => return server_error("loading teacher aliases", &e),
    };
    let alias_names: Vec<String> = aliases.iter().map(|(_, a)| a.clone()).collect();
    let detector = match NamedDetector::new(aliases, &site.common_words) {
        Ok(d) => d,
        Err(e) => return server_error("building the alias matcher", &e),
    };
    let parsed = detector.parse(&text);
    let found = match retrieve(
        &state.pool,
        site.embedder.as_ref(),
        site.reranker.as_deref(),
        &parsed,
        &site.retrieval,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return server_error("retrieving", &e),
    };
    let ids: Vec<_> = found.chosen.iter().map(|c| c.id).collect();
    let served = match wt_db::answer::passages(&state.pool, &ids).await {
        Ok(s) => s,
        Err(e) => return server_error("loading passages", &e),
    };
    // Spec 05: synthesis runs in standard mode only, over the passages as served. Any failure
    // leaves the answer without the line and nothing else.
    let synthesis = match found.mode {
        Mode::Standard => {
            let input = SynthesisInput {
                query: text.clone(),
                passages: served.iter().map(|p| (p.id, p.text.clone())).collect(),
            };
            wt_synth::synthesize(
                site.llm.as_ref(),
                &input,
                &alias_names,
                &site.synthesis(),
                DataClass::Real,
            )
            .await
        }
        Mode::NamedTeacher(_) => Synthesis::default(),
    };
    let sanghas = served
        .iter()
        .map(|p| p.talk.sangha_id)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let (notice, notice_count) = match found.notice {
        Some(Notice::ThinCoverage { teachers }) => (Some("thin_coverage"), Some(teachers)),
        Some(Notice::NoneFound) => (Some("none_found"), None),
        Some(Notice::NamedNone) => (Some("named_none"), None),
        Some(Notice::NamedFew { found }) => (Some("named_few"), Some(found)),
        Some(Notice::MultiNamed) => (Some("multi_named"), None),
        None => (None, None),
    };
    let (mode, named) = match found.mode {
        Mode::Standard => ("standard", None),
        Mode::NamedTeacher(t) => ("named_teacher", Some(t)),
    };
    let payload = Payload {
        mode: mode.to_owned(),
        named_teacher_id: named,
        notice: notice.map(str::to_owned),
        notice_count,
        themes: synthesis.themes,
        passage_ids: ids,
        rerank_degraded: found.rerank_degraded,
    };
    let small = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    let latency = small(usize::try_from(started.elapsed().as_millis()).unwrap_or(usize::MAX));
    let new = wt_db::answer::NewAnswer {
        tester: s.tester,
        query_text: &text,
        payload,
        teacher_count: small(found.teacher_count),
        sangha_count: small(sanghas),
        thin_coverage: matches!(found.notice, Some(Notice::ThinCoverage { .. })),
        latency_ms: latency,
        themes_dropped: small(synthesis.dropped.len()),
    };
    match wt_db::answer::persist(&state.pool, &new).await {
        Ok(id) => {
            tracing::info!(latency_ms = latency, "answer stored");
            Redirect::to(&format!("/a/{id}")).into_response()
        }
        Err(e) => server_error("storing the answer", &e),
    }
}

// ---- answer ---------------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "answer.html")]
struct AnswerPage {
    query: String,
    view: view::AnswerView,
    answer_id: AnswerId,
    csrf: String,
    thanks: bool,
}

#[derive(Debug, Deserialize)]
struct AnswerQuery {
    thanks: Option<u8>,
}

fn parse<T: std::str::FromStr>(raw: &str) -> Option<T> {
    raw.parse().ok()
}

/// `GET /a/{answer}`: re-renders the answer through the serving view, for its owner only.
async fn answer_page(
    State(state): State<AppState>,
    s: Session,
    Path(answer): Path<String>,
    Query(q): Query<AnswerQuery>,
) -> Response {
    let Some(answer) = parse::<AnswerId>(&answer) else {
        return not_found_page();
    };
    let stored = match wt_db::answer::load(&state.pool, answer, s.tester).await {
        Ok(Some(a)) => a,
        Ok(None) => return not_found_page(),
        Err(e) => return server_error("loading the answer", &e),
    };
    let served: Vec<Served> =
        match wt_db::answer::passages(&state.pool, &stored.payload.passage_ids).await {
            Ok(p) => p,
            Err(e) => return server_error("loading passages", &e),
        };
    let named = match stored.payload.named_teacher_id {
        Some(t) => wt_db::answer::teacher_name(&state.pool, t)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let view = view::assemble(
        answer,
        &stored.payload,
        &served,
        named.as_deref(),
        state.site.retrieval.answer.link_lead_in_ms,
    );
    page(
        StatusCode::OK,
        &AnswerPage {
            query: stored.query_text,
            view,
            answer_id: answer,
            csrf: s.csrf(&state.site.keys),
            thanks: q.thanks.is_some(),
        },
    )
}

#[derive(Debug, Deserialize)]
struct FeedbackForm {
    kind: String,
    csrf: String,
    passage: Option<String>,
}

/// `POST /a/{answer}/feedback`: Good, Not faithful, Not relevant, or Flag this query.
async fn feedback(
    State(state): State<AppState>,
    s: Session,
    Path(answer): Path<String>,
    Form(form): Form<FeedbackForm>,
) -> Response {
    if !s.csrf_ok(&state.site.keys, &form.csrf) {
        return csrf_refused();
    }
    let Some(answer) = parse::<AnswerId>(&answer) else {
        return not_found_page();
    };
    let note = form
        .passage
        .as_deref()
        .filter(|p| parse::<wt_core::ids::PassageId>(p).is_some());
    match wt_db::answer::feedback(&state.pool, answer, s.tester, &form.kind, note).await {
        Ok(true) => Redirect::to(&format!("/a/{answer}?thanks=1")).into_response(),
        Ok(false) => not_found_page(),
        Err(e) => server_error("recording feedback", &e),
    }
}

/// `GET /give/{answer}/{sangha}`: logs the click and redirects to the registry's link.
async fn give(
    State(state): State<AppState>,
    s: Session,
    Path((answer, sangha)): Path<(String, String)>,
) -> Response {
    let (Some(answer), Some(sangha)) = (parse::<AnswerId>(&answer), parse::<SanghaId>(&sangha))
    else {
        return not_found_page();
    };
    match wt_db::answer::give(&state.pool, answer, s.tester, sangha).await {
        Ok(Some(url)) => Redirect::to(&url).into_response(),
        Ok(None) => not_found_page(),
        Err(e) => server_error("logging a give click", &e),
    }
}

// ---- talks ----------------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "talk.html")]
struct TalkPage {
    title: String,
    teacher: String,
    sangha: String,
    meta: String,
    source_href: String,
    credit: String,
    transcript_href: Option<String>,
}

async fn talk_page(
    State(state): State<AppState>,
    _s: Session,
    Path(talk): Path<String>,
) -> Response {
    let Some(talk) = parse::<TalkId>(&talk) else {
        return not_found_page();
    };
    let (card, _) = match wt_db::answer::talk(&state.pool, talk).await {
        Ok(Some(t)) => t,
        Ok(None) => return not_found_page(),
        Err(e) => return server_error("loading the talk", &e),
    };
    page(
        StatusCode::OK,
        &TalkPage {
            title: card.title.clone(),
            teacher: card.teacher_name.clone(),
            sangha: card.sangha_name.clone(),
            meta: view::meta(&card),
            source_href: card.source_url.clone(),
            credit: view::credit_line(&card),
            transcript_href: (card.display_level == "full")
                .then(|| format!("/talk/{}/transcript", card.id)),
        },
    )
}

struct Row {
    ordinal: i32,
    at: String,
    text: String,
    gap_before: bool,
}

#[derive(Template)]
#[template(path = "transcript.html")]
struct TranscriptPage {
    title: String,
    teacher: String,
    sangha: String,
    meta: String,
    credit: String,
    source_href: String,
    rows: Vec<Row>,
}

/// Passages further apart than this get a marked gap on the transcript page.
const GAP_MS: i32 = 5_000;

/// `GET /talk/{talk}/transcript`: the teacher's words passage by passage, at display level
/// `full` only.
async fn transcript_page(
    State(state): State<AppState>,
    _s: Session,
    Path(talk): Path<String>,
) -> Response {
    let Some(talk) = parse::<TalkId>(&talk) else {
        return not_found_page();
    };
    let (card, passages) = match wt_db::answer::talk(&state.pool, talk).await {
        Ok(Some(t)) => t,
        Ok(None) => return not_found_page(),
        Err(e) => return server_error("loading the transcript", &e),
    };
    if card.display_level != "full" {
        return not_found_page();
    }
    let mut rows = Vec::with_capacity(passages.len());
    let mut last_end: Option<i32> = None;
    for p in &passages {
        rows.push(Row {
            ordinal: p.ordinal,
            at: view::clock(i64::from(p.start_ms)),
            text: p.text.clone(),
            gap_before: last_end.is_some_and(|e| p.start_ms - e > GAP_MS),
        });
        last_end = Some(p.end_ms);
    }
    page(
        StatusCode::OK,
        &TranscriptPage {
            title: card.title.clone(),
            teacher: card.teacher_name.clone(),
            sangha: card.sangha_name.clone(),
            meta: view::meta(&card),
            credit: view::credit_line(&card),
            source_href: card.source_url.clone(),
            rows,
        },
    )
}
