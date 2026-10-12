//! Donation links and their verification (Spec 02, Donation links and verification).
//!
//! Only a person verifies a link: `verify` records who and when, and nothing else in the
//! codebase sets `donation_verified`.

use sqlx::PgPool;
use sqlx::types::chrono::{DateTime, Utc};
use wt_core::ids::SanghaId;

use crate::DbError;
use crate::correction::log_change;

/// One sangha's donation link as the registry holds it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Link {
    /// The sangha.
    pub sangha_id: SanghaId,
    /// Its name.
    pub name: String,
    /// `page`, `contact` or `none`.
    pub kind: String,
    /// The giving page.
    pub url: Option<String>,
    /// The page with an amount placeholder.
    pub url_template: Option<String>,
    /// A person checked it.
    pub verified: bool,
    /// Who checked it last.
    pub checked_by: Option<String>,
    /// When.
    pub checked_at: Option<DateTime<Utc>>,
    /// Why it is a contact page, missing or unverified.
    pub note: Option<String>,
    /// False when the sangha asked for attribution only.
    pub listed: bool,
}

/// Why a verification was refused.
#[derive(Debug, thiserror::Error)]
pub enum DonationError {
    /// No sangha has that name.
    #[error("no sangha is called {0:?}")]
    UnknownSangha(String),
    /// Only a giving page can be verified.
    #[error(
        "{name} has donation kind {kind:?} and no verifiable page; only a page with a URL can be verified"
    )]
    NotAPage {
        /// The sangha.
        name: String,
        /// Its kind.
        kind: String,
    },
    /// The page the person checked is not the one stored.
    #[error("{name}'s stored link is {stored:?}, not the {checked:?} you checked")]
    LinkChanged {
        /// The sangha.
        name: String,
        /// What the registry holds.
        stored: Option<String>,
        /// What the person said they opened.
        checked: String,
    },
    /// `--by` was blank.
    #[error("verification needs the name of the person who checked the page")]
    NoChecker,
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for DonationError {
    fn from(e: sqlx::Error) -> Self {
        DonationError::Db(DbError::Sqlx(e))
    }
}

/// Every sangha's link, by name.
pub async fn list(pool: &PgPool) -> Result<Vec<Link>, DbError> {
    let rows = sqlx::query!(
        r#"select id, name, donation_kind, donation_url, donation_url_template,
                  donation_verified, donation_checked_by, donation_checked_at, donation_note, listed
           from sangha order by name"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Link {
            sangha_id: SanghaId::from(r.id),
            name: r.name,
            kind: r.donation_kind,
            url: r.donation_url,
            url_template: r.donation_url_template,
            verified: r.donation_verified,
            checked_by: r.donation_checked_by,
            checked_at: r.donation_checked_at,
            note: r.donation_note,
            listed: r.listed,
        })
        .collect())
}

/// Marks a sangha's giving page verified by `by`, now. `url` is the page the person opened,
/// which must be the stored one, so a link changed since is never verified unseen.
pub async fn verify(pool: &PgPool, name: &str, url: &str, by: &str) -> Result<Link, DonationError> {
    let by = by.trim();
    if by.is_empty() {
        return Err(DonationError::NoChecker);
    }
    let mut tx = pool.begin().await?;
    let row = sqlx::query!(
        "select id, donation_kind, donation_url, donation_verified from sangha where name = $1 for update",
        name
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| DonationError::UnknownSangha(name.to_owned()))?;
    if row.donation_kind != "page" || row.donation_url.is_none() {
        return Err(DonationError::NotAPage {
            name: name.to_owned(),
            kind: row.donation_kind,
        });
    }
    if row.donation_url.as_deref() != Some(url) {
        return Err(DonationError::LinkChanged {
            name: name.to_owned(),
            stored: row.donation_url,
            checked: url.to_owned(),
        });
    }
    sqlx::query!(
        "update sangha set donation_verified = true, donation_verified_at = now(),
                donation_checked_at = now(), donation_checked_by = $2, donation_note = null
         where id = $1",
        row.id,
        by
    )
    .execute(&mut *tx)
    .await?;
    log_change(
        &mut tx,
        "sangha",
        row.id,
        "donation_verified",
        Some(serde_json::json!(row.donation_verified)),
        Some(serde_json::json!({ "verified": true, "url": url })),
        by,
        "a person opened the giving page and confirmed it takes gifts for the sangha",
    )
    .await?;
    tx.commit().await?;
    one(pool, name).await
}

/// Clears a sangha's verification, with the reason in `donation_note`.
pub async fn unverify(
    pool: &PgPool,
    name: &str,
    reason: &str,
    by: &str,
) -> Result<Link, DonationError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query!(
        "select id, donation_verified from sangha where name = $1 for update",
        name
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| DonationError::UnknownSangha(name.to_owned()))?;
    sqlx::query!(
        "update sangha set donation_verified = false, donation_verified_at = null,
                donation_note = $2
         where id = $1",
        row.id,
        reason
    )
    .execute(&mut *tx)
    .await?;
    log_change(
        &mut tx,
        "sangha",
        row.id,
        "donation_verified",
        Some(serde_json::json!(row.donation_verified)),
        Some(serde_json::json!(false)),
        by,
        reason,
    )
    .await?;
    tx.commit().await?;
    one(pool, name).await
}

async fn one(pool: &PgPool, name: &str) -> Result<Link, DonationError> {
    list(pool)
        .await?
        .into_iter()
        .find(|l| l.name == name)
        .ok_or_else(|| DonationError::UnknownSangha(name.to_owned()))
}
