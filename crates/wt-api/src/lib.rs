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
pub mod excerpt;
pub mod limits;
pub mod privacy;
pub mod progress;
pub mod rights;
pub mod sensitive;
pub mod session;
pub mod site;
pub mod view;

use std::sync::Arc;
use std::time::Duration;

use askama::Template;
use axum::extract::{ConnectInfo, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
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
use wt_retrieve::sensitive::{Exemplars, SensitiveConfig, triage};
use wt_retrieve::{Level, RetrievalConfig, Sensitivity, Triage, Triaged, query};

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
    /// `WT_LLM_REAL_DATA_OK`: a real talk's text may go to the model. Off, excerpts are chosen
    /// without it (Spec 05, Answer assembly, rule 8).
    pub real_data_ok: bool,
    /// `config/retrieval.toml`.
    pub retrieval: RetrievalConfig,
    /// `config/common_words.txt`.
    pub common_words: String,
    /// Sensitive-question detection, run on every question before retrieval (Spec 05).
    pub sensitivity: Arc<Sensitivity>,
    /// The triage gate (Spec 05, layer 3); `None` without a language model, which logs
    /// `triage_degraded` on every question and leaves the rules and the embedding check.
    pub triage: Option<Arc<dyn Triage>>,
    /// Signs session cookies and CSRF tokens.
    pub keys: Keys,
    /// Marks the cookie `Secure`; everywhere but `dev`.
    pub secure_cookies: bool,
    /// The in-memory rate limits.
    pub limits: Limits,
    /// `WT_OWNER_NAME`: who runs the site, for About. Without it About says "an individual".
    pub owner_name: Option<String>,
    /// Question privacy settings.
    pub privacy: privacy::Settings,
    /// `WT_TRUST_FORWARDED_FOR`: behind the load balancer, the client address is the last
    /// entry of `X-Forwarded-For`. Off, the connection's peer address is used.
    pub trust_forwarded_for: bool,
    /// `WT_SITE_DANA_URL`: the site's own giving page, for the separate site line. Without it
    /// the panel offers no site line.
    pub site_dana_url: Option<String>,
    /// Passages-searched counts for the live progress display (Spec 06).
    pub scope_sizes: Arc<progress::ScopeSizes>,
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
            privacy: privacy::Settings::default(),
            trust_forwarded_for: false,
            embedder: Arc::new(FakeEmbedder::new(embedder_model)),
            reranker: None,
            llm: Arc::new(UnavailableLlm),
            real_data_ok: false,
            retrieval,
            common_words: String::new(),
            sensitivity: Arc::new(fake_sensitivity()),
            triage: None,
            keys: Keys::new(b"test-only-key"),
            secure_cookies: false,
            site_dana_url: None,
            scope_sizes: Arc::default(),
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

/// The shipped `config/sensitive.toml` and exemplars, embedded with the hashing fake, for test
/// sites.
fn fake_sensitivity() -> Sensitivity {
    let config =
        SensitiveConfig::parse(include_str!("../../../config/sensitive.toml")).unwrap_or_default();
    let exemplars = Exemplars::parse(include_str!("../../../config/sensitive_exemplars.toml"))
        .unwrap_or_default();
    let vectors = exemplars
        .sentences()
        .into_iter()
        .map(|(level, text)| (level, FakeEmbedder::vector(text)))
        .collect();
    Sensitivity::from_vectors(&config, vectors)
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
        .route("/static/dana.js", get(dana_script))
        .route("/static/references.js", get(references_script))
        .route("/static/progress.js", get(progress_script))
        .route("/join/{token}", get(join))
        .route("/rights", get(rights::rights_page))
        .route("/rights/request", post(rights::request))
        .route("/accessibility", get(accessibility::statement))
        .route("/accessibility/report", post(accessibility::report))
        .route("/", get(home))
        .route("/about", get(about))
        .route("/about/privacy", post(privacy::save))
        .route("/about/delete-my-data", post(privacy::delete))
        .route("/ask", post(ask))
        .route("/ask/curated", post(ask_curated))
        .route("/a/{answer}", get(answer_page))
        .route("/a/{answer}/feedback", post(feedback))
        .route("/a/{answer}/dana", post(dana))
        .route("/give/{answer}/{target}", get(give))
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

async fn dana_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/dana.js"),
    )
}

async fn references_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/references.js"),
    )
}

async fn progress_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/progress.js"),
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
    pub(crate) fn csrf(&self, keys: &Keys) -> String {
        keys.csrf(&self.cookie)
    }

    pub(crate) fn csrf_ok(&self, keys: &Keys, token: &str) -> bool {
        keys.check_csrf(&self.cookie, token)
    }
}

pub(crate) fn csrf_refused() -> Response {
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
    switch: privacy::Switch,
    saved: bool,
    csrf: String,
    question: String,
    error: Option<String>,
    examples: [&'static str; 3],
}

/// `?saved=1` after the privacy switch, `?deleted=1` after deleting questions.
#[derive(Debug, Deserialize)]
struct Saved {
    saved: Option<u8>,
    deleted: Option<u8>,
}

async fn home(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Query(q): Query<Saved>,
) -> Response {
    let switch = match privacy::switch(&state.pool, &state.site.privacy, &s, &headers, "home").await
    {
        Ok(sw) => sw,
        Err(e) => return server_error("loading the privacy choice", &e),
    };
    page(
        StatusCode::OK,
        &HomePage {
            chrome: Chrome::new("/", Current::Ask, Some(s.csrf(&state.site.keys))),
            switch,
            saved: q.saved.is_some(),
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
    switch: privacy::Switch,
    csrf: String,
    saved: bool,
    deleted: bool,
}

async fn about(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Query(q): Query<Saved>,
) -> Response {
    let switch =
        match privacy::switch(&state.pool, &state.site.privacy, &s, &headers, "about").await {
            Ok(sw) => sw,
            Err(e) => return server_error("loading the privacy choice", &e),
        };
    let csrf = s.csrf(&state.site.keys);
    page(
        StatusCode::OK,
        &AboutPage {
            chrome: Chrome::new("/about", Current::About, Some(csrf.clone())),
            owner: state.site.owner_name.as_deref(),
            switch,
            csrf,
            saved: q.saved.is_some(),
            deleted: q.deleted.is_some(),
        },
    )
}

#[derive(Debug, Deserialize)]
struct AskForm {
    q: String,
    csrf: String,
    /// Set by the outside-these-talks page's "Search the talks anyway".
    #[serde(default)]
    anyway: Option<String>,
}

/// `POST /ask`: runs the query path, persists the answer and its log row together, and
/// redirects to the permalink; or, for the progress script, streams the same run (Spec 06,
/// Live progress while a question runs).
async fn ask(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Form(form): Form<AskForm>,
) -> Response {
    if !progress::wants_stream(&headers) {
        return ask_with(&state, &s, &headers, form, &progress::Sink::silent()).await;
    }
    progress::stream(move |sink| async move { ask_with(&state, &s, &headers, form, &sink).await })
}

async fn ask_with(
    state: &AppState,
    s: &Session,
    headers: &HeaderMap,
    form: AskForm,
    sink: &progress::Sink,
) -> Response {
    let site = &state.site;
    if !s.csrf_ok(&site.keys, &form.csrf) {
        return csrf_refused();
    }
    if let Err(wait) = site.limits.ask.check(&s.tester.to_string()) {
        // The limit never stands between an acute question and the notice.
        if site.sensitivity.rules_level(&form.q) == Level::Acute {
            return acute(state, s, std::time::Instant::now()).await;
        }
        return too_many(wait, "You have asked a lot of questions this hour.");
    }
    let Ok(text) = query::normalize(&form.q) else {
        let switch = match privacy::switch(&state.pool, &site.privacy, s, headers, "home").await {
            Ok(sw) => sw,
            Err(e) => return server_error("loading the privacy choice", &e),
        };
        return page(
            StatusCode::BAD_REQUEST,
            &HomePage {
                chrome: Chrome::new("/", Current::Ask, Some(s.csrf(&site.keys))),
                switch,
                saved: false,
                csrf: s.csrf(&site.keys),
                question: form.q,
                error: Some("Please write between 3 and 500 characters.".to_owned()),
                examples: EXAMPLES,
            },
        );
    };
    sink.stage(progress::Stage::Reading);
    // Spec 05: never part of ranking. The triage gate starts first and runs alongside the
    // embedding check and search; an acute rule match needs nothing else.
    let started = std::time::Instant::now();
    let gate = (site.sensitivity.rules_level(&text) != Level::Acute).then(|| {
        let (gate, settings, q) = (
            site.triage.clone(),
            site.sensitivity.triage_settings(),
            text.clone(),
        );
        tokio::spawn(async move { triage::run(gate.as_deref(), settings, &q).await })
    });
    let sensitive = site
        .sensitivity
        .classify(&text, site.embedder.as_ref())
        .await;
    tracing::info!(
        sensitive_category = sensitive.level.as_str(),
        sensitivity_degraded = sensitive.degraded,
        rules = sensitive.layers.rules.as_str(),
        embedding = sensitive.layers.embedding.map(Level::as_str),
        "sensitivity"
    );
    let tally = |triaged: Triaged, level: Level| {
        sensitive::tally(
            &state.pool,
            sensitive.layers,
            sensitive.degraded,
            triaged,
            level,
        )
    };
    if sensitive.level == Level::Acute {
        if let Some(gate) = gate {
            gate.abort();
        }
        tally(Triaged::not_run(), Level::Acute).await;
        return acute(state, s, started).await;
    }
    let framing = ask::Framing {
        level: sensitive.level,
        vector: sensitive.vector.clone(),
        curated: false,
        gpc: privacy::gpc(headers),
        anyway: form.anyway.is_some(),
    };
    match ask::answer_framed(&state.pool, site, s.tester, &text, framing, gate, sink).await {
        Ok(ask::Gated::Answered(asked)) => {
            tally(asked.triaged, asked.level).await;
            Redirect::to(&format!("/a/{}", asked.answer_id)).into_response()
        }
        Ok(ask::Gated::Stopped { stop, triaged }) => {
            let level = match stop {
                ask::Stop::CountsOnly(level) => level,
                ask::Stop::Unrelated => Level::None,
            };
            tally(triaged, level).await;
            match stop {
                ask::Stop::CountsOnly(Level::Acute) => acute_page(state, s),
                ask::Stop::CountsOnly(_) => out_of_scope_page(state, s),
                ask::Stop::Unrelated => unrelated_page(state, s, text),
            }
        }
        Err(e) => server_error("answering", &e),
    }
}

fn answered(result: Result<ask::Gated, ask::AskError>) -> Response {
    match result {
        Ok(ask::Gated::Answered(asked)) => {
            Redirect::to(&format!("/a/{}", asked.answer_id)).into_response()
        }
        Ok(ask::Gated::Stopped { stop, .. }) => {
            server_error("answering", &ask::AskError::Stopped(stop))
        }
        Err(e) => server_error("answering", &e),
    }
}

#[derive(Debug, Deserialize)]
struct CsrfForm {
    csrf: String,
}

/// `POST /ask/curated`: the acute notice's "Show teachings on facing difficult feelings". Runs
/// the fixed query with the support notice on top, no summary line and no giving panel.
async fn ask_curated(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> Response {
    if !progress::wants_stream(&headers) {
        return ask_curated_with(&state, &s, &headers, &form, &progress::Sink::silent()).await;
    }
    progress::stream(move |sink| async move {
        ask_curated_with(&state, &s, &headers, &form, &sink).await
    })
}

async fn ask_curated_with(
    state: &AppState,
    s: &Session,
    headers: &HeaderMap,
    form: &CsrfForm,
    sink: &progress::Sink,
) -> Response {
    let site = &state.site;
    if !s.csrf_ok(&site.keys, &form.csrf) {
        return csrf_refused();
    }
    if let Err(wait) = site.limits.ask.check(&s.tester.to_string()) {
        return too_many(wait, "You have asked a lot of questions this hour.");
    }
    let framing = ask::Framing {
        level: Level::Distress,
        vector: None,
        curated: true,
        gpc: privacy::gpc(headers),
        anyway: false,
    };
    let text = site.sensitivity.curated_query();
    sink.stage(progress::Stage::Reading);
    answered(ask::answer_framed(&state.pool, site, s.tester, text, framing, None, sink).await)
}

#[derive(Template)]
#[template(path = "sensitive_acute.html")]
struct AcutePage {
    chrome: Chrome,
    csrf: String,
}

/// Spec 06: a `200` with the notice and `Cache-Control: no-store`, since no answer exists. The
/// log row holds counts and the category only, never the text.
async fn acute(state: &AppState, s: &Session, started: std::time::Instant) -> Response {
    let latency = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
    if let Err(e) =
        wt_db::answer::log_counts_only(&state.pool, s.tester, "acute", latency, None, false).await
    {
        // The notice matters more than the count, so it is shown anyway.
        tracing::error!(error = %e, "logging an acute question's counts");
    }
    acute_page(state, s)
}

/// The acute notice page, for a question whose counts are already logged.
fn acute_page(state: &AppState, s: &Session) -> Response {
    no_store(page(
        StatusCode::OK,
        &AcutePage {
            chrome: Chrome::new("/ask", Current::Other, Some(s.csrf(&state.site.keys))),
            csrf: s.csrf(&state.site.keys),
        },
    ))
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Template)]
#[template(path = "sensitive_out_of_scope.html")]
struct OutOfScopePage {
    chrome: Chrome,
}

/// Spec 05's `out_of_scope` page, rendered as Spec 06 renders the acute page: a heading that
/// takes focus, no giving panel, no feedback, no permalink and `Cache-Control: no-store`.
fn out_of_scope_page(state: &AppState, s: &Session) -> Response {
    no_store(page(
        StatusCode::OK,
        &OutOfScopePage {
            chrome: Chrome::new("/ask", Current::Other, Some(s.csrf(&state.site.keys))),
        },
    ))
}

#[derive(Template)]
#[template(path = "sensitive_unrelated.html")]
struct UnrelatedPage {
    chrome: Chrome,
    csrf: String,
    question: String,
}

/// Spec 05's outside-these-talks page for an `unrelated` question: the question and "Search
/// the talks anyway", which asks it again as usual. No permalink until then.
fn unrelated_page(state: &AppState, s: &Session, question: String) -> Response {
    no_store(page(
        StatusCode::OK,
        &UnrelatedPage {
            chrome: Chrome::new("/ask", Current::Other, Some(s.csrf(&state.site.keys))),
            csrf: s.csrf(&state.site.keys),
            question,
        },
    ))
}

// ---- answer ---------------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "answer.html")]
struct AnswerPage {
    chrome: Chrome,
    query: String,
    /// Spec 05's notice for a sensitive question, above the question.
    banner: Option<sensitive::Banner>,
    /// Whether the summary line may show.
    summary: bool,
    /// Whether the giving panel may show.
    giving: bool,
    question_label: &'static str,
    view: view::AnswerView,
    answer_id: AnswerId,
    csrf: String,
    thanks: bool,
    /// The split, after "Show the split" without JavaScript.
    dana: Option<view::DanaView>,
    /// What the donor entered, kept in the fields.
    dana_total: String,
    /// See `dana_total`.
    dana_site: String,
    /// The next split is the panel's first use, so it also logs `panel_open`.
    dana_first: bool,
    /// The site has a giving page, so the panel offers the site line.
    site_giving: bool,
}

/// The dana split fragment, as the script swaps it into the live region.
#[derive(Template)]
#[template(path = "dana_split.html")]
struct DanaFragment {
    d: view::DanaView,
}

#[derive(Debug, Deserialize)]
struct AnswerQuery {
    thanks: Option<u8>,
}

fn parse<T: std::str::FromStr>(raw: &str) -> Option<T> {
    raw.parse().ok()
}

/// An answer re-rendered through the serving views, with its served passages.
struct Loaded {
    stored: wt_db::answer::Stored,
    served: Vec<Served>,
    view: view::AnswerView,
}

/// Loads the answer for its owner. `Ok(None)` is the generic 404.
async fn load_answer(
    state: &AppState,
    s: &Session,
    answer: AnswerId,
) -> Result<Option<Loaded>, Box<Response>> {
    let stored = match wt_db::answer::load(&state.pool, answer, s.tester).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return match wt_db::privacy::was_purged(&state.pool, answer, s.tester).await {
                Ok(true) => Err(Box::new(error_page(
                    StatusCode::GONE,
                    "Question not kept",
                    "This question was not kept.",
                ))),
                Ok(false) => Ok(None),
                Err(e) => Err(Box::new(server_error("checking a purged answer", &e))),
            };
        }
        Err(e) => return Err(Box::new(server_error("loading the answer", &e))),
    };
    let served: Vec<Served> =
        match wt_db::answer::passages(&state.pool, &stored.payload.passage_ids).await {
            Ok(p) => p,
            Err(e) => return Err(Box::new(server_error("loading passages", &e))),
        };
    let named = match stored.payload.named_teacher_id {
        Some(t) => wt_db::answer::teacher_name(&state.pool, t)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    // Each excerpt is rebuilt from the serving rows of the passages its span covers.
    let mut covered: Vec<wt_core::ids::PassageId> = stored.payload.passage_ids.clone();
    covered.extend(
        stored
            .payload
            .spans
            .iter()
            .flat_map(|s| s.covers.iter().copied()),
    );
    covered.sort_unstable();
    covered.dedup();
    let rows = match wt_db::answer::excerpt_rows(&state.pool, &covered).await {
        Ok(r) => r,
        Err(e) => return Err(Box::new(server_error("loading excerpts", &e))),
    };
    let excerpts = excerpt::rebuild(&stored.payload.spans, &served, &rows);
    let cited = match wt_db::answer::cited_works(&state.pool, &covered).await {
        Ok(c) => c,
        Err(e) => return Err(Box::new(server_error("loading citations", &e))),
    };
    let mut view = view::assemble(
        answer,
        &stored.payload,
        &served,
        &excerpts,
        &cited,
        named.as_deref(),
        state.site.retrieval.answer.link_lead_in_ms,
        stored.created_at.date_naive(),
    );
    match wt_db::questions::for_passages(&state.pool, &stored.payload.passage_ids).await {
        Ok(q) => view::attach_questions(&mut view, &q),
        Err(e) => return Err(Box::new(server_error("loading student questions", &e))),
    }
    Ok(Some(Loaded {
        stored,
        served,
        view,
    }))
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
    let loaded = match load_answer(&state, &s, answer).await {
        Ok(Some(l)) => l,
        Ok(None) => return not_found_page(),
        Err(r) => return *r,
    };
    let (summary, giving) = sensitive::shows(
        &loaded.stored.sensitive_category,
        loaded.stored.payload.curated,
    );
    page(
        StatusCode::OK,
        &AnswerPage {
            chrome: Chrome::new(
                format!("/a/{answer}"),
                Current::Other,
                Some(s.csrf(&state.site.keys)),
            ),
            banner: sensitive::Banner::for_answer(
                &loaded.stored.sensitive_category,
                loaded.stored.payload.curated,
            ),
            summary,
            giving,
            query: loaded.stored.query_text,
            question_label: view::QUESTION_LABEL,
            view: loaded.view,
            answer_id: answer,
            csrf: s.csrf(&state.site.keys),
            thanks: q.thanks.is_some(),
            dana: None,
            dana_total: String::new(),
            dana_site: String::new(),
            dana_first: true,
            site_giving: state.site.site_dana_url.is_some(),
        },
    )
}

#[derive(Debug, Deserialize)]
struct DanaForm {
    csrf: String,
    #[serde(default)]
    total: String,
    #[serde(default)]
    site: String,
    /// The script's beacon on the panel's first use: log `panel_open` and nothing else.
    open: Option<String>,
    /// Without the script, the first split also logs `panel_open`.
    first: Option<String>,
}

/// The message for amounts that are not dollars.
const DANA_AMOUNTS: &str = "Please enter amounts in US dollars, such as 20 or 12.50.";

/// Spec 05's split for the amounts entered, or the message to show instead, with the gift
/// total in cents when a split was computed.
fn compute_dana(
    answer: AnswerId,
    form: &DanaForm,
    sanghas: &[wt_db::answer::DanaSangha],
    site_giving: bool,
) -> (view::DanaView, Option<i64>) {
    let total = wt_dana::parse_dollars(&form.total);
    let site = if site_giving {
        wt_dana::parse_dollars(&form.site)
    } else {
        Some(0)
    };
    let (Some(total), Some(site)) = (total, site) else {
        return (view::DanaView::message(DANA_AMOUNTS), None);
    };
    if total == 0 && site == 0 {
        return (
            view::DanaView::message("Enter an amount to see the split."),
            None,
        );
    }
    let input = wt_dana::DanaInput {
        gift_total_cents: total,
        site_cents: site,
        recipients: view::recipients(sanghas),
        tiebreak_seed: view::dana_seed(answer),
    };
    match wt_dana::split(&input) {
        Ok(split) => (
            view::dana_view(answer, &split, sanghas, site_giving),
            Some(total),
        ),
        Err(wt_dana::DanaError::TooLarge { max_cents }) => (
            view::DanaView::message(&format!(
                "Please enter no more than ${} in all.",
                wt_dana::dollars(max_cents)
            )),
            None,
        ),
        Err(wt_dana::DanaError::Negative) => (view::DanaView::message(DANA_AMOUNTS), None),
    }
}

/// `POST /a/{answer}/dana`: the split for the entered amounts, as the live region's fragment
/// when the script asks (`HX-Request`) and as the whole page otherwise. Logs `split_computed`
/// with the gift total, and `panel_open` on the panel's first use.
async fn dana(
    State(state): State<AppState>,
    s: Session,
    Path(answer): Path<String>,
    headers: axum::http::HeaderMap,
    Form(form): Form<DanaForm>,
) -> Response {
    let site = &state.site;
    if !s.csrf_ok(&site.keys, &form.csrf) {
        return csrf_refused();
    }
    let Some(answer) = parse::<AnswerId>(&answer) else {
        return not_found_page();
    };
    let open = wt_db::answer::DanaEvent::PanelOpen;
    if form.open.is_some() {
        return match wt_db::answer::dana_event(&state.pool, answer, s.tester, open).await {
            Ok(true) => StatusCode::NO_CONTENT.into_response(),
            Ok(false) => not_found_page(),
            Err(e) => server_error("logging the dana panel", &e),
        };
    }
    let loaded = match load_answer(&state, &s, answer).await {
        Ok(Some(l)) => l,
        Ok(None) => return not_found_page(),
        Err(r) => return *r,
    };
    // Spec 05: a distress answer and the curated query have no giving panel.
    let (summary, giving) = sensitive::shows(
        &loaded.stored.sensitive_category,
        loaded.stored.payload.curated,
    );
    if !giving {
        return not_found_page();
    }
    // Texts never enter the split (Spec 05, Answer assembly, rule 6): only talk passages
    // name sanghas.
    let ids: Vec<SanghaId> = loaded
        .served
        .iter()
        .map(|p| p.talk.sangha_id)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let sanghas = match wt_db::answer::dana_sanghas(&state.pool, &ids).await {
        Ok(r) => r,
        Err(e) => return server_error("loading the cited sanghas", &e),
    };
    let site_giving = site.site_dana_url.is_some();
    let (view, total) = compute_dana(answer, &form, &sanghas, site_giving);
    if form.first.is_some()
        && let Err(e) = wt_db::answer::dana_event(&state.pool, answer, s.tester, open).await
    {
        return server_error("logging the dana panel", &e);
    }
    if let Some(total) = total {
        let event = wt_db::answer::DanaEvent::SplitComputed {
            total_cents: i32::try_from(total).unwrap_or(i32::MAX),
        };
        if let Err(e) = wt_db::answer::dana_event(&state.pool, answer, s.tester, event).await {
            return server_error("logging the dana split", &e);
        }
    }
    if headers.get("hx-request").is_some() {
        return page(StatusCode::OK, &DanaFragment { d: view });
    }
    page(
        StatusCode::OK,
        &AnswerPage {
            chrome: Chrome::new(
                format!("/a/{answer}"),
                Current::Other,
                Some(s.csrf(&state.site.keys)),
            ),
            banner: sensitive::Banner::for_answer(
                &loaded.stored.sensitive_category,
                loaded.stored.payload.curated,
            ),
            summary,
            giving,
            query: loaded.stored.query_text,
            question_label: view::QUESTION_LABEL,
            view: loaded.view,
            answer_id: answer,
            csrf: s.csrf(&site.keys),
            thanks: false,
            dana: Some(view),
            dana_total: form.total,
            dana_site: form.site,
            dana_first: false,
            site_giving,
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

#[derive(Debug, Deserialize)]
struct GiveQuery {
    amount_cents: Option<String>,
}

/// `GET /give/{answer}/{target}`: logs the click and redirects to the registry's link, or for
/// `site` to `WT_SITE_DANA_URL`. `amount_cents` only fills a verified page's amount template;
/// anything but a whole number of cents within the ceiling is refused.
async fn give(
    State(state): State<AppState>,
    s: Session,
    Path((answer, target)): Path<(String, String)>,
    Query(q): Query<GiveQuery>,
) -> Response {
    let Some(answer) = parse::<AnswerId>(&answer) else {
        return not_found_page();
    };
    let amount = match q.amount_cents.as_deref() {
        None => None,
        Some(raw) => match raw.parse::<i64>() {
            Ok(c) if (0..=wt_dana::MAX_TOTAL_CENTS).contains(&c) => Some(c),
            _ => return not_found_page(),
        },
    };
    if target == "site" {
        let Some(url) = state.site.site_dana_url.as_deref() else {
            return not_found_page();
        };
        return match wt_db::answer::give_site(&state.pool, answer, s.tester).await {
            Ok(true) => Redirect::to(url).into_response(),
            Ok(false) => not_found_page(),
            Err(e) => server_error("logging a give click", &e),
        };
    }
    let Some(sangha) = parse::<SanghaId>(&target) else {
        return not_found_page();
    };
    match wt_db::answer::give(&state.pool, answer, s.tester, sangha).await {
        Ok(Some(t)) => {
            let url = match amount {
                Some(c) => wt_dana::giving_link(&t.url, t.template.as_deref(), c),
                None => t.url,
            };
            Redirect::to(&url).into_response()
        }
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
