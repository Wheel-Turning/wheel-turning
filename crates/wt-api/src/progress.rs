//! Live progress while a question runs (Spec 06): with `Accept: text/event-stream`, `POST /ask`
//! and `POST /ask/curated` run the same path as a plain post and stream its stages as
//! server-sent events, closing with the permalink or the page a plain post would have shown.
//! Nothing here changes what the query path computes or stores.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::to_bytes;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use wt_core::ids::TeacherId;
use wt_retrieve::{Progress, Step};

/// How long a passages-searched count stands before it is read again.
const SCOPE_TTL: Duration = Duration::from_secs(300);

/// The largest closing page the stream carries; every page the site renders is far smaller.
const PAGE_LIMIT: usize = 4 * 1024 * 1024;

/// A stage of the query path as the display names it, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Reading the question: the sensitivity check, alias detection and the embedding.
    Reading,
    /// Searching the talks.
    Searching,
    /// Weighing excerpts: the reranker.
    Weighing,
    /// Choosing excerpts: selection, reading the passages and cutting their excerpts.
    Choosing,
    /// Writing the summary; only when synthesis runs.
    Writing,
}

/// A figure the display shows beside its stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Count {
    /// Servable passages in the search's scope.
    Searched,
    /// Candidates read back from the fused list.
    Candidates,
    /// Candidates at or above the relevance threshold.
    Shortlisted,
    /// Passages chosen.
    Selected,
}

/// One server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A stage started.
    Stage(Stage),
    /// A count is known.
    Count(Count, u64),
    /// The answer is stored at this permalink. Closes the stream.
    Done(String),
    /// The whole page a plain post would have shown. Closes the stream.
    Page(String),
}

impl Event {
    /// Whether the event ends the stream.
    fn closes(&self) -> bool {
        matches!(self, Self::Done(_) | Self::Page(_))
    }

    /// The event as server-sent event data for the progress script.
    fn sse(&self) -> SseEvent {
        #[derive(Serialize)]
        struct StageData {
            stage: Stage,
        }
        #[derive(Serialize)]
        struct CountData {
            name: Count,
            value: u64,
        }
        #[derive(Serialize)]
        struct DoneData<'a> {
            url: &'a str,
        }
        #[derive(Serialize)]
        struct PageData<'a> {
            html: &'a str,
        }
        let (name, data) = match self {
            Self::Stage(stage) => ("stage", serde_json::to_string(&StageData { stage: *stage })),
            Self::Count(name, value) => (
                "count",
                serde_json::to_string(&CountData {
                    name: *name,
                    value: *value,
                }),
            ),
            Self::Done(url) => ("done", serde_json::to_string(&DoneData { url })),
            Self::Page(html) => ("page", serde_json::to_string(&PageData { html })),
        };
        // These types always serialize; an empty object is a safe stand-in regardless.
        SseEvent::default()
            .event(name)
            .data(data.unwrap_or_else(|_| "{}".to_owned()))
    }
}

/// Where the query path reports its stages: nowhere for a plain post, or a stream.
#[derive(Debug, Clone, Default)]
pub struct Sink(Option<UnboundedSender<Event>>);

impl Sink {
    /// Reports nowhere.
    pub fn silent() -> Self {
        Self(None)
    }

    /// Whether anyone is listening.
    pub fn streaming(&self) -> bool {
        self.0.is_some()
    }

    /// Sends an event to the stream, if there is one.
    fn send(&self, event: Event) {
        if let Some(tx) = &self.0 {
            // A reader who left closes the channel; the question still runs to the end.
            let _ = tx.send(event);
        }
    }

    /// `stage` started.
    pub fn stage(&self, stage: Stage) {
        self.send(Event::Stage(stage));
    }

    /// `name` is `value`.
    pub fn count(&self, name: Count, value: usize) {
        self.send(Event::Count(name, u64::try_from(value).unwrap_or(u64::MAX)));
    }

    /// Sends the passages-searched count for `teacher`'s scope, or the whole collection's,
    /// from the cache or read alongside the query path. Only a streamed ask reads it.
    pub fn searched(&self, pool: &PgPool, sizes: &Arc<ScopeSizes>, teacher: Option<TeacherId>) {
        if !self.streaming() {
            return;
        }
        if let Some(n) = sizes.fresh(teacher) {
            self.send(Event::Count(Count::Searched, n));
            return;
        }
        let (sink, pool, sizes) = (self.clone(), pool.clone(), Arc::clone(sizes));
        tokio::spawn(async move {
            match wt_retrieve::search::scope_size(&pool, teacher).await {
                Ok(n) => {
                    let n = u64::try_from(n).unwrap_or(0);
                    sizes.store(teacher, n);
                    sink.send(Event::Count(Count::Searched, n));
                }
                Err(e) => tracing::warn!(error = %e, "counting the passages searched"),
            }
        });
    }
}

impl Progress for Sink {
    fn step(&self, step: Step) {
        match step {
            Step::Searching => self.stage(Stage::Searching),
            Step::Weighing { candidates } => {
                self.count(Count::Candidates, candidates);
                self.stage(Stage::Weighing);
            }
            Step::Choosing { shortlisted } => {
                self.count(Count::Shortlisted, shortlisted);
                self.stage(Stage::Choosing);
            }
        }
    }
}

/// Passages-searched counts per scope, each kept for five minutes.
#[derive(Debug, Default)]
pub struct ScopeSizes(Mutex<HashMap<Option<TeacherId>, (Instant, u64)>>);

impl ScopeSizes {
    /// The scope's passage count, when it was counted less than five minutes ago.
    fn fresh(&self, teacher: Option<TeacherId>) -> Option<u64> {
        let map = self.0.lock().ok()?;
        map.get(&teacher)
            .filter(|(at, _)| at.elapsed() < SCOPE_TTL)
            .map(|(_, n)| *n)
    }

    /// Keeps a scope's passage count.
    fn store(&self, teacher: Option<TeacherId>, n: u64) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(teacher, (Instant::now(), n));
        }
    }
}

/// Whether the request asks for the stream: `Accept: text/event-stream`, which only the
/// progress script sends.
pub fn wants_stream(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.contains("text/event-stream"))
}

/// Runs `ask` with a streaming sink in its own task, so the question runs to the end even if
/// the reader leaves, and answers with the event stream. `ask`'s response becomes the closing
/// event: a redirect is `done` with its target, anything else is `page` with its body.
pub fn stream<F, Fut>(ask: F) -> Response
where
    F: FnOnce(Sink) -> Fut + Send + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    let (tx, rx) = unbounded_channel();
    let sink = Sink(Some(tx));
    tokio::spawn(async move {
        let closing = closing(ask(sink.clone()).await).await;
        sink.send(closing);
    });
    let events = futures_util::stream::unfold((rx, false), |(mut rx, closed)| async move {
        if closed {
            return None;
        }
        let event = rx.recv().await?;
        let closed = event.closes();
        Some((Ok::<_, Infallible>(event.sse()), (rx, closed)))
    });
    let mut response = Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The closing event for the response a plain post would have sent.
async fn closing(response: Response) -> Event {
    if response.status().is_redirection()
        && let Some(url) = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
    {
        return Event::Done(url.to_owned());
    }
    match to_bytes(response.into_body(), PAGE_LIMIT).await {
        Ok(body) => Event::Page(String::from_utf8_lossy(&body).into_owned()),
        Err(e) => {
            tracing::error!(error = %e, "reading the page for the progress stream");
            Event::Page(String::new())
        }
    }
}
