//! What the pages without a session read and write (Spec 06, route table; Spec 08, Rights
//! lifecycle): the status of every source, removal requests and accessibility reports. The web
//! role may insert a request or a report and never reads one back; the CLI handles them.

use chrono::NaiveDate;
use sqlx::PgPool;
use sqlx::types::Uuid;

use crate::DbError;

/// One row of `v_source_rights`, as the rights page states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    /// The source's key.
    pub key: String,
    /// Who publishes it.
    pub publisher: String,
    /// `license`, `permission` or `mandate`.
    pub basis: String,
    /// `texts` or `talks`.
    pub kind: String,
    /// Whether it serves.
    pub enabled: bool,
    /// When the latest permission request went out.
    pub request_sent_on: Option<NaiveDate>,
    /// The license's name, for the license basis.
    pub license_name: Option<String>,
    /// When permission was recorded.
    pub permission_recorded_on: Option<NaiveDate>,
    /// When the license was recorded.
    pub license_recorded_on: Option<NaiveDate>,
    /// The rights holder declined or asked for removal.
    pub removal_requested: bool,
}

/// Every source, by publisher.
pub async fn source_statuses(pool: &PgPool) -> Result<Vec<SourceStatus>, DbError> {
    let rows = sqlx::query!(
        r#"select key as "key!", publisher as "publisher!", basis as "basis!", kind as "kind!",
                  enabled as "enabled!", latest_request_sent_on, license_name,
                  permission_recorded_on, license_recorded_on,
                  removal_requested as "removal_requested!"
           from v_source_rights order by publisher, key"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SourceStatus {
            key: r.key,
            publisher: r.publisher,
            basis: r.basis,
            kind: r.kind,
            enabled: r.enabled,
            request_sent_on: r.latest_request_sent_on,
            license_name: r.license_name,
            permission_recorded_on: r.permission_recorded_on,
            license_recorded_on: r.license_recorded_on,
            removal_requested: r.removal_requested,
        })
        .collect())
}

/// Whether `id` is a text unit that serves, so a report of it can name the unit.
pub async fn serving_text_unit(pool: &PgPool, id: Uuid) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select exists (select 1 from servable_text_unit where text_unit_id = $1) as "e!""#,
        id
    )
    .fetch_one(pool)
    .await?)
}

/// A removal or report request from `/rights`.
#[derive(Debug, Clone, Default)]
pub struct NewRequest<'a> {
    /// Their name.
    pub name: &'a str,
    /// Their email address.
    pub email: &'a str,
    /// What should be removed: a URL, a title or an excerpt code.
    pub target: &'a str,
    /// Their message.
    pub message: Option<&'a str>,
    /// The reported text unit, when the excerpt is one.
    pub text_unit: Option<Uuid>,
    /// The SHA-256 of the token emailed to confirm it.
    pub token_hash: &'a [u8],
}

/// Stores a request as `unverified`, its link valid for 24 hours. The web role may insert and
/// nothing more, so nothing is returned.
pub async fn request_removal(pool: &PgPool, r: &NewRequest<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "insert into takedown_request (requester_name, requester_email, target, message,
                                       text_unit_id, verification_token_hash)
         values ($1, $2, $3, $4, $5, $6)",
        r.name,
        r.email,
        r.target,
        r.message,
        r.text_unit,
        r.token_hash
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Confirms the unverified request whose token hashes to `token_hash`, if its link has not
/// expired or been used. Returns whether one was confirmed.
pub async fn confirm_removal(pool: &PgPool, token_hash: &[u8]) -> Result<bool, DbError> {
    Ok(
        sqlx::query_scalar!(r#"select confirm_takedown_request($1) as "c!""#, token_hash)
            .fetch_one(pool)
            .await?,
    )
}

/// Deletes requests nobody confirmed, a week after their link expired. Returns how many went.
pub async fn purge_unverified(pool: &PgPool) -> Result<i32, DbError> {
    Ok(
        sqlx::query_scalar!(r#"select purge_unverified_takedown_requests() as "n!""#)
            .fetch_one(pool)
            .await?,
    )
}

/// A report from `/accessibility`.
#[derive(Debug, Clone, Default)]
pub struct NewReport<'a> {
    /// The page the problem is on.
    pub page_path: Option<&'a str>,
    /// What happened, 5 to 4000 characters.
    pub description: &'a str,
    /// The assistive technology they use.
    pub assistive_tech: Option<&'a str>,
    /// How to reach them.
    pub contact: Option<&'a str>,
}

/// Stores an accessibility report.
pub async fn report_accessibility(pool: &PgPool, r: &NewReport<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "insert into accessibility_report (page_path, description, assistive_tech, contact)
         values ($1, $2, $3, $4)",
        r.page_path,
        r.description,
        r.assistive_tech,
        r.contact
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// An accessibility report, for `wt accessibility reports`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    /// Its id.
    pub id: Uuid,
    /// When it arrived.
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// The page.
    pub page_path: Option<String>,
    /// What happened.
    pub description: String,
    /// Their assistive technology.
    pub assistive_tech: Option<String>,
    /// How to reach them.
    pub contact: Option<String>,
    /// `open`, `fixing`, `fixed` or `declined`.
    pub status: String,
    /// Who handled it.
    pub handled_by: Option<String>,
    /// When.
    pub handled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// What was done.
    pub note: Option<String>,
}

/// Reports, open and in-progress ones unless `all`, oldest first.
pub async fn reports(pool: &PgPool, all: bool) -> Result<Vec<Report>, DbError> {
    Ok(sqlx::query_as!(
        Report,
        "select id, received_at, page_path, description, assistive_tech, contact, status,
                handled_by, handled_at, note
         from accessibility_report
         where $1 or status in ('open', 'fixing')
         order by received_at",
        all
    )
    .fetch_all(pool)
    .await?)
}

/// One report.
pub async fn report(pool: &PgPool, id: Uuid) -> Result<Option<Report>, DbError> {
    Ok(sqlx::query_as!(
        Report,
        "select id, received_at, page_path, description, assistive_tech, contact, status,
                handled_by, handled_at, note
         from accessibility_report where id = $1",
        id
    )
    .fetch_optional(pool)
    .await?)
}

/// The statuses a report can take.
pub const REPORT_STATUSES: [&str; 4] = ["open", "fixing", "fixed", "declined"];

/// Records how a report is being handled. Returns false when there is no such report.
pub async fn set_report(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    by: &str,
    note: Option<&str>,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "update accessibility_report
         set status = $2, handled_by = $3, handled_at = now(), note = coalesce($4, note)
         where id = $1",
        id,
        status,
        by,
        note
    )
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}
