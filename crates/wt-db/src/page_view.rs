//! Page views (Spec 07, Usage report): one row per request that carried a tester, written by the
//! web role after the response and purged after the retention the About page states. The route
//! is the matched template, so a row never holds an answer id, an invite token or a query string.

use sqlx::PgPool;
use sqlx::types::chrono::{DateTime, Utc};
use wt_core::ids::TesterId;

use crate::DbError;

/// One request that carried a tester.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageView {
    /// The signed-in tester.
    pub tester: TesterId,
    /// The HTTP method.
    pub method: String,
    /// The matched route template, as the request log names it.
    pub route: String,
    /// The response status.
    pub status: u16,
    /// Milliseconds to the response.
    pub ms: u64,
    /// When the request arrived. The insert runs later, in a task of its own.
    pub at: DateTime<Utc>,
}

/// Records one page view.
pub async fn record(pool: &PgPool, view: &PageView) -> Result<(), DbError> {
    let status = i16::try_from(view.status).unwrap_or(i16::MAX);
    let ms = i32::try_from(view.ms).unwrap_or(i32::MAX);
    sqlx::query!(
        "insert into page_view (tester_id, method, route, status, ms, created_at)
         values ($1, $2, $3, $4, $5, $6)",
        view.tester.as_uuid(),
        view.method,
        view.route,
        status,
        ms,
        view.at
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Deletes page views older than `retention_days`. Returns how many went.
pub async fn purge(pool: &PgPool, retention_days: u32) -> Result<i32, DbError> {
    let days = i32::try_from(retention_days).unwrap_or(i32::MAX);
    Ok(sqlx::query_scalar!(
        r#"select purge_page_views(make_interval(days => $1)) as "n!""#,
        days
    )
    .fetch_one(pool)
    .await?)
}
