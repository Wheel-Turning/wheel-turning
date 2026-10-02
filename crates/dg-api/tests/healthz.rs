//! Spec 01 M0: `dg-api` serves `/healthz` with database connectivity checked.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use dg_api::{AppState, router};
use dg_core::Secret;
use dg_db::test_support::TestDb;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get_healthz(state: AppState) -> (StatusCode, serde_json::Value) {
    let response = router(state)
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn healthz_is_ok_when_the_database_answers() {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = dg_db::lazy_pool(db.url()).unwrap();

    let (status, body) = get_healthz(AppState { pool }).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["database"], "ok");
}

#[tokio::test]
async fn healthz_is_unavailable_when_the_database_is_down() {
    // Nothing listens on port 1, so every connection attempt is refused.
    let pool = dg_db::lazy_pool(&Secret::new("postgres://dg:dg@127.0.0.1:1/dg")).unwrap();

    let (status, body) = get_healthz(AppState { pool }).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "unavailable");
    assert_eq!(body["database"], "unavailable");
}
