//! License evidence for texts (Spec 03, Licensed sources; Spec 08, Ingesting from URLs):
//! `wt license capture` saves a license page and records it unapproved, `record-permission`
//! records an organization's permission for URL prefixes, unapproved, and `approve` is the
//! person's step after reading the snapshot, refused for a code outside the allowed list.

use sqlx::PgConnection;
use sqlx::types::Uuid;
use wt_core::records::StoredRecord;

use crate::DbError;

/// The code a recorded permission carries.
pub const PERMISSION_CODE: &str = "permission-excerpt";

/// A captured license page.
#[derive(Debug, Clone)]
pub struct Captured<'a> {
    /// Who publishes what the license covers.
    pub publisher: &'a str,
    /// Such as `cc-by-4.0` or `public-domain`.
    pub code: &'a str,
    /// The page that states it.
    pub url: &'a str,
    /// What it covers, in words.
    pub scope: &'a str,
    /// The saved page.
    pub snapshot: &'a StoredRecord,
    /// URL prefixes it covers, for URL ingestion.
    pub prefixes: &'a [String],
}

/// Records captured evidence, unapproved.
pub async fn capture(conn: &mut PgConnection, c: &Captured<'_>) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar!(
        "insert into license_evidence
           (publisher, license_code, license_url, evidence_url, evidence_snapshot_ref,
            snapshot_sha256, scope_note, captured_at, allowed, kind, url_prefixes)
         values ($1, $2, $3, $3, $4, $5, $6, now(), false, 'license', $7)
         returning id",
        c.publisher,
        c.code,
        c.url,
        c.snapshot.key,
        c.snapshot.sha256,
        c.scope,
        c.prefixes
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// A recorded permission.
#[derive(Debug, Clone)]
pub struct Permission<'a> {
    /// The organization that gave it, also taken as the publisher.
    pub grantor: &'a str,
    /// The URL prefixes it covers.
    pub prefixes: &'a [String],
    /// What it allows, such as "excerpting with attribution".
    pub covers: &'a str,
    /// The saved statement.
    pub statement: &'a StoredRecord,
    /// When it was given, `YYYY-MM-DD`.
    pub given_on: &'a str,
    /// Who recorded it.
    pub recorded_by: &'a str,
}

/// Records a permission, unapproved. The database refuses one without a grantor or prefixes.
pub async fn record_permission(
    conn: &mut PgConnection,
    p: &Permission<'_>,
) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar!(
        "insert into license_evidence
           (publisher, license_code, evidence_snapshot_ref, snapshot_sha256, scope_note,
            captured_at, allowed, kind, grantor, url_prefixes, reviewed_by)
         values ($1, $2, $3, $4, $5, $6::text::date::timestamptz, false, 'permission', $1, $7,
                 null)
         returning id",
        p.grantor,
        PERMISSION_CODE,
        p.statement.key,
        p.statement.sha256,
        format!("{} (recorded by {})", p.covers, p.recorded_by),
        p.given_on,
        p.prefixes
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// What an approval found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Approval {
    /// Approved now.
    Approved,
    /// It was approved before.
    AlreadyApproved,
    /// Its code is not in `allowed_license_codes`, so it stays unapproved.
    CodeNotAllowed(String),
    /// No such evidence.
    Missing,
}

/// A person's approval after reading the snapshot.
pub async fn approve(
    conn: &mut PgConnection,
    id: Uuid,
    reviewer: &str,
    allowed_codes: &[String],
) -> Result<Approval, DbError> {
    let Some(row) = sqlx::query!(
        "select license_code, allowed from license_evidence where id = $1 for update",
        id
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(Approval::Missing);
    };
    if row.allowed {
        return Ok(Approval::AlreadyApproved);
    }
    if !allowed_codes.iter().any(|c| c == &row.license_code) {
        return Ok(Approval::CodeNotAllowed(row.license_code));
    }
    sqlx::query!(
        "update license_evidence set allowed = true, reviewed_by = $2, reviewed_at = now()
         where id = $1",
        id,
        reviewer
    )
    .execute(&mut *conn)
    .await?;
    Ok(Approval::Approved)
}
