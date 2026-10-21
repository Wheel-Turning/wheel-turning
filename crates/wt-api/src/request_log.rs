//! Spec 07, Observability: one structured line per request, with a request id, the tester, the
//! matched route template, the status and the latency. It never carries the raw path, the query
//! string, headers, cookies or a body, so question text, passage text and invite tokens cannot
//! reach it. Testers are told about it on the About page (Spec 09, owner's decision of October 20,
//! 2026).

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use wt_core::ids::TesterId;

/// Where the session extractor records the signed-in tester for the request's log line. The
/// logger puts an empty one in each request's extensions; a verified [`crate::Session`] fills it.
#[derive(Clone, Default)]
pub(crate) struct TesterSlot(Arc<OnceLock<TesterId>>);

impl TesterSlot {
    /// Records the tester, once.
    pub(crate) fn set(&self, tester: TesterId) {
        let _ = self.0.set(tester);
    }
}

/// The route a log line names: the matched template, with the invite route's token placeholder
/// shown as Spec 06 writes it, or `fallback` for a path no route matched.
fn route_label(matched: Option<&MatchedPath>) -> &str {
    match matched.map(MatchedPath::as_str) {
        Some("/join/{token}") => "/join/[redacted]",
        Some(route) => route,
        None => "fallback",
    }
}

/// A fresh request id: 8 random bytes from the operating system, as hex.
fn request_id() -> String {
    let mut bytes = [0u8; 8];
    match getrandom::fill(&mut bytes) {
        Ok(()) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        Err(_) => "unavailable".to_owned(),
    }
}

/// Logs `event = "request"` once the response is ready. The load balancer's `/healthz` probe is
/// left out, since it would outnumber every tester's requests and says nothing about them.
pub(crate) async fn log(mut request: Request, next: Next) -> Response {
    let route = route_label(request.extensions().get::<MatchedPath>()).to_owned();
    if route == "/healthz" {
        return next.run(request).await;
    }
    let method = request.method().as_str().to_owned();
    let slot = TesterSlot::default();
    request.extensions_mut().insert(slot.clone());
    let started = Instant::now();
    let response = next.run(request).await;
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let tester = slot.0.get().map(ToString::to_string);
    tracing::info!(
        event = "request",
        request_id = %request_id(),
        tester = tester.as_deref(),
        method = %method,
        route = %route,
        status = response.status().as_u16(),
        ms,
        "request"
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_hex_and_differ() {
        let (a, b) = (request_id(), request_id());
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn unmatched_paths_are_named_fallback() {
        assert_eq!(route_label(None), "fallback");
    }
}
