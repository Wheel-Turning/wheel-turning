//! Spec 07, Observability: one `http_response` event per request, with a request id, the tester,
//! the route as declared, the status and the time to the response's head. It never carries the
//! raw path, the query string, headers, cookies or a body, so question text, passage text, answer
//! ids and invite tokens cannot reach it. The 5xx-rate alarm counts these events
//! (`docs/adr-edge-cost.md`, Tunnel follow-on), and testers are told about them on the About page
//! (Spec 09, owner's decision of October 20, 2026).

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use wt_core::ids::TesterId;
use wt_db::page_view::PageView;

use crate::AppState;

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

/// The route a log line names: the declared template, such as `/join/{token}`, or `fallback` for
/// a path no route matched.
fn route_label(matched: Option<&MatchedPath>) -> &str {
    matched.map_or("fallback", MatchedPath::as_str)
}

/// A fresh request id: 8 random bytes from the operating system, as hex.
fn request_id() -> String {
    let mut bytes = [0u8; 8];
    match getrandom::fill(&mut bytes) {
        Ok(()) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        Err(_) => "unavailable".to_owned(),
    }
}

/// Logs `event = "http_response"` once the response's head is ready. A request that carried a
/// tester is also stored as a page view (Spec 07, Usage report), in a task of its own so the
/// insert never delays or fails the response.
pub(crate) async fn log(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let route = route_label(request.extensions().get::<MatchedPath>()).to_owned();
    let method = request.method().as_str().to_owned();
    let slot = TesterSlot::default();
    request.extensions_mut().insert(slot.clone());
    let started = Instant::now();
    let at = chrono::Utc::now();
    let response = next.run(request).await;
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let status = response.status().as_u16();
    let tester = slot.0.get().copied();
    tracing::info!(
        event = "http_response",
        request_id = %request_id(),
        tester = tester.map(|t| t.to_string()).as_deref(),
        method = %method,
        route = %route,
        status,
        ms,
        "responded"
    );
    if let Some(tester) = tester {
        let view = PageView {
            tester,
            method,
            route,
            status,
            ms,
            at,
        };
        tokio::spawn(async move {
            if let Err(e) = wt_db::page_view::record(&state.pool, &view).await {
                tracing::warn!(error = %e, "recording a page view");
            }
        });
    }
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
