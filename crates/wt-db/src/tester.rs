//! Testers and their invite tokens (Spec 06, Invite token access). Only the SHA-256 of a token
//! is stored; the token itself is printed once by `wt tester invite` and never logged.

use sqlx::PgPool;
use wt_core::ids::TesterId;

use crate::DbError;

/// One tester, for `wt tester list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Tester {
    /// The tester.
    pub id: TesterId,
    /// How the owner knows them.
    pub label: String,
    /// When they were invited.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When access was revoked, if it was.
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When they were last seen, to the hour.
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Records a tester with the SHA-256 of their token.
pub async fn invite(pool: &PgPool, label: &str, token_hash: &[u8]) -> Result<TesterId, DbError> {
    let id = sqlx::query_scalar!(
        "insert into tester (label, token_hash) values ($1, $2) returning id",
        label,
        token_hash
    )
    .fetch_one(pool)
    .await?;
    Ok(TesterId::from(id))
}

/// The active tester holding a token with this hash.
pub async fn by_token_hash(pool: &PgPool, token_hash: &[u8]) -> Result<Option<TesterId>, DbError> {
    Ok(sqlx::query_scalar!(
        "select id from tester where token_hash = $1 and revoked_at is null",
        token_hash
    )
    .fetch_optional(pool)
    .await?
    .map(TesterId::from))
}

/// Whether the tester exists and is not revoked. Also notes that they were seen, at most once
/// an hour, so the row is not rewritten on every request.
pub async fn active(pool: &PgPool, id: TesterId) -> Result<bool, DbError> {
    let found = sqlx::query_scalar!(
        r#"update tester set last_seen_at = case
             when last_seen_at is null or last_seen_at < now() - interval '1 hour' then now()
             else last_seen_at end
           where id = $1 and revoked_at is null
           returning id"#,
        id.as_uuid()
    )
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// Revokes every active tester with this label. Returns how many.
pub async fn revoke(pool: &PgPool, label: &str) -> Result<u64, DbError> {
    Ok(sqlx::query!(
        "update tester set revoked_at = now() where label = $1 and revoked_at is null",
        label
    )
    .execute(pool)
    .await?
    .rows_affected())
}

/// Every tester, newest first.
pub async fn list(pool: &PgPool) -> Result<Vec<Tester>, DbError> {
    let rows = sqlx::query!(
        "select id, label, created_at, revoked_at, last_seen_at from tester order by created_at desc"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Tester {
            id: TesterId::from(r.id),
            label: r.label,
            created_at: r.created_at,
            revoked_at: r.revoked_at,
            last_seen_at: r.last_seen_at,
        })
        .collect())
}
