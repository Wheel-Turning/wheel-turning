//! The Axum web app: routes, templates and invite auth (Spec 06).
//!
//! Every route but `/healthz`, `/robots.txt`, `/static/*`, `/favicon.ico`, `/join/{token}`,
//! `/rights`, `/rights/request`, `/accessibility` and `/accessibility/report` needs a session,
//! and answers the same generic 404 without one. Passage text reaches the templates only from
//! `servable_passage`, and the templates escape it. Model text reaches only the "How approaches
//! differ" section, as verified themes whose passages the code, not the model, names.

pub mod accessibility;
pub mod ask;
pub mod chrome;
pub mod limits;
pub mod rights;
pub mod session;
pub mod site;
pub mod view;

use std::sync::Arc;
use std::time::Duration;

use askama::Template;
use axum::extract::{ConnectInfo, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use wt_core::ids::{AnswerId, SanghaId, TalkId, TesterId};
use wt_db::answer::Served;
use wt_extract::{LlmClient, UnavailableLlm};
use wt_ml::{Embedder, FakeEmbedder, Reranker};
use wt_retrieve::{RetrievalConfig, query};

use chrome::{Chrome, Current};
use limits::Limiter;
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
    /// The in-memory rate limits.
    pub limits: Limits,
    /// `WT_OWNER_NAME`: who runs the site, for About. Without it About says "an individual".
    pub owner_name: Option<String>,
    /// `WT_TRUST_FORWARDED_FOR`: behind the load balancer, the client address is the last
    /// entry of `X-Forwarded-For`. Off, the connection's peer address is used.
    pub trust_forwarded_for: bool,
}

/// The rate limits (Spec 06): asks per tester, and the public forms per address.
#[derive(Debug)]
pub struct Limits {
    /// `ask_rate_per_hour` asks per tester.
    pub ask: Limiter,
    /// `/rights/request` and `/accessibility/report`: 5 an hour per address, together.
    pub public_forms: Limiter,
}

impl Limits {
    /// Limits with `ask_per_hour` asks per tester.
    pub fn new(ask_per_hour: u32) -> Self {
        Self {
            ask: Limiter::new(ask_per_hour),
            public_forms: Limiter::new(PUBLIC_FORMS_PER_HOUR),
        }
    }
}

/// Spec 08: the rights form takes 5 requests an hour from one address.
pub const PUBLIC_FORMS_PER_HOUR: u32 = 5;

impl Site {
    /// A site with a fake embedder, no reranker, no language model and a fixed key, for tests.
    pub fn with_fakes(embedder_model: &str) -> Self {
        let retrieval = RetrievalConfig::default();
        Self {
            limits: Limits::new(retrieval.answer.ask_rate_per_hour),
            owner_name: None,
            trust_forwarded_for: false,
            embedder: Arc::new(FakeEmbedder::new(embedder_model)),
            reranker: None,
            llm: Arc::new(UnavailableLlm),
            retrieval,
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
        .route("/static/menu.js", get(chrome::menu_js))
        .route("/static/brand/favicon.svg", get(chrome::favicon_svg))
        .route("/favicon.ico", get(chrome::favicon_ico))
        .route("/join/{token}", get(join))
        .route("/rights", get(rights::rights_page))
        .route("/rights/request", post(rights::request))
        .route("/accessibility", get(accessibility::statement))
        .route("/accessibility/report", post(accessibility::report))
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
    chrome: Chrome,
    title: &'a str,
    message: &'a str,
}

pub(crate) fn page(status: StatusCode, t: &impl Template) -> Response {
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
    page(
        status,
        &ErrorPage {
            chrome: Chrome::bare(),
            title,
            message,
        },
    )
}

/// The friendly 429 (Spec 09, Messages): what happened and when to try again, with
/// `Retry-After`.
pub(crate) fn too_many(wait: Duration, what: &str) -> Response {
    let minutes = limits::minutes(wait);
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    let mut response = error_page(
        StatusCode::TOO_MANY_REQUESTS,
        "Please wait a little",
        &format!("{what} Please try again in {minutes} {unit}."),
    );
    if let Ok(v) = HeaderValue::from_str(&(minutes * 60).to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, v);
    }
    response
}

/// The address a public form's rate limit counts against: the last `X-Forwarded-For` entry
/// when the site sits behind the load balancer, which appends the client it saw, otherwise the
/// connection's peer.
pub struct ClientAddress(pub String);

impl FromRequestParts<AppState> for ClientAddress {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let forwarded = if state.site.trust_forwarded_for {
            parts
                .headers
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(','))
                .map(str::trim)
                .rfind(|v| !v.is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        let peer = parts
            .extensions
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip().to_string());
        Ok(Self(
            forwarded.or(peer).unwrap_or_else(|| "unknown".to_owned()),
        ))
    }
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
pub(crate) fn server_error(what: &str, e: &dyn std::fmt::Display) -> Response {
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
    chrome: Chrome,
    csrf: String,
    question: String,
    error: Option<String>,
    examples: [&'static str; 3],
}

async fn home(State(state): State<AppState>, s: Session) -> Response {
    page(
        StatusCode::OK,
        &HomePage {
            chrome: Chrome::new("/", Current::Ask, Some(s.csrf(&state.site.keys))),
            csrf: s.csrf(&state.site.keys),
            question: String::new(),
            error: None,
            examples: EXAMPLES,
        },
    )
}

#[derive(Template)]
#[template(path = "about.html")]
struct AboutPage<'a> {
    chrome: Chrome,
    owner: Option<&'a str>,
}

async fn about(State(state): State<AppState>, s: Session) -> Response {
    page(
        StatusCode::OK,
        &AboutPage {
            chrome: Chrome::new("/about", Current::About, Some(s.csrf(&state.site.keys))),
            owner: state.site.owner_name.as_deref(),
        },
    )
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
    if let Err(wait) = site.limits.ask.check(&s.tester.to_string()) {
        return too_many(wait, "You have asked a lot of questions this hour.");
    }
    let Ok(text) = query::normalize(&form.q) else {
        return page(
            StatusCode::BAD_REQUEST,
            &HomePage {
                chrome: Chrome::new("/", Current::Ask, Some(s.csrf(&site.keys))),
                csrf: s.csrf(&site.keys),
                question: form.q,
                error: Some("Please write between 3 and 500 characters.".to_owned()),
                examples: EXAMPLES,
            },
        );
    };
    match ask::answer(&state.pool, site, s.tester, &text).await {
        Ok(asked) => Redirect::to(&format!("/a/{}", asked.answer_id)).into_response(),
        Err(e) => server_error("answering", &e),
    }
}

// ---- answer ---------------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "answer.html")]
struct AnswerPage {
    chrome: Chrome,
    query: String,
    question_label: &'static str,
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
    let mut view = view::assemble(
        answer,
        &stored.payload,
        &served,
        named.as_deref(),
        state.site.retrieval.answer.link_lead_in_ms,
    );
    match wt_db::questions::for_passages(&state.pool, &stored.payload.passage_ids).await {
        Ok(q) => view::attach_questions(&mut view, &q),
        Err(e) => return server_error("loading student questions", &e),
    }
    page(
        StatusCode::OK,
        &AnswerPage {
            chrome: Chrome::new(
                format!("/a/{answer}"),
                Current::Other,
                Some(s.csrf(&state.site.keys)),
            ),
            query: stored.query_text,
            question_label: view::QUESTION_LABEL,
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
    chrome: Chrome,
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
    s: Session,
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
            chrome: Chrome::new(
                format!("/talk/{talk}"),
                Current::Other,
                Some(s.csrf(&state.site.keys)),
            ),
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
    /// The student's question this passage answers, lightly edited.
    question: Option<String>,
    /// An answer whose question has no stored edit: "A student asked a question".
    asked: bool,
}

#[derive(Template)]
#[template(path = "transcript.html")]
struct TranscriptPage {
    chrome: Chrome,
    talk_id: TalkId,
    title: String,
    teacher: String,
    sangha: String,
    meta: String,
    credit: String,
    source_href: String,
    rows: Vec<Row>,
    question_label: &'static str,
}

/// Passages further apart than this get a marked gap on the transcript page.
const GAP_MS: i32 = 5_000;

/// `GET /talk/{talk}/transcript`: the teacher's words passage by passage, at display level
/// `full` only.
async fn transcript_page(
    State(state): State<AppState>,
    s: Session,
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
    let questions = match wt_db::questions::for_talk(&state.pool, talk, GAP_MS).await {
        Ok(q) => q,
        Err(e) => return server_error("loading student questions", &e),
    };
    let mut rows = Vec::with_capacity(passages.len());
    let mut last_end: Option<i32> = None;
    for p in &passages {
        let question = questions
            .shown
            .iter()
            .find(|q| q.passage_id == p.id)
            .map(|q| q.text.clone());
        rows.push(Row {
            ordinal: p.ordinal,
            at: view::clock(i64::from(p.start_ms)),
            text: p.text.clone(),
            gap_before: last_end.is_some_and(|e| p.start_ms - e > GAP_MS),
            asked: question.is_none() && questions.asked.contains(&p.id),
            question,
        });
        last_end = Some(p.end_ms);
    }
    page(
        StatusCode::OK,
        &TranscriptPage {
            chrome: Chrome::new(
                format!("/talk/{talk}/transcript"),
                Current::Other,
                Some(s.csrf(&state.site.keys)),
            ),
            talk_id: talk,
            title: card.title.clone(),
            teacher: card.teacher_name.clone(),
            sangha: card.sangha_name.clone(),
            meta: view::meta(&card),
            credit: view::credit_line(&card),
            source_href: card.source_url.clone(),
            rows,
            question_label: view::QUESTION_LABEL,
        },
    )
}
