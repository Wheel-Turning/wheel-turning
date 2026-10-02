//! The Axum web app: routes, templates and invite auth.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::PgPool;

/// Shared state handed to every route.
#[derive(Clone)]
pub struct AppState {
    /// The application role's connection pool.
    pub pool: PgPool,
}

/// Builds the router with every route.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
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
    match dg_db::ping(&state.pool).await {
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
