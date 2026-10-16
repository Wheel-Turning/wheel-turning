//! A database's own name (Spec 11, PB1.1). The one-row `database_identity` table is written once
//! at setup; the commands that write pipeline or rights data read it at start and refuse to run
//! against a database other than the one `WT_DATABASE_NAME` names.

use sqlx::PgPool;
use wt_core::config::Env;

use crate::DbError;

/// Why a command may not run against this database.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdentityError {
    /// The database is named, and not as `WT_DATABASE_NAME` expects.
    #[error("this database is {found:?}, but WT_DATABASE_NAME is {expected:?}; refusing to run")]
    Mismatch {
        /// The name the database holds.
        found: String,
        /// `WT_DATABASE_NAME`.
        expected: String,
    },
    /// `WT_DATABASE_NAME` is set but the database has no name yet.
    #[error(
        "this database has no name, but WT_DATABASE_NAME is {expected:?}; \
         name it once with `wt db set-identity --name <name>`"
    )]
    Unnamed {
        /// `WT_DATABASE_NAME`.
        expected: String,
    },
    /// `WT_ENV=prod` and `WT_DATABASE_NAME` is unset.
    #[error("WT_DATABASE_NAME is required when WT_ENV=prod")]
    KeyRequired,
    /// `set` was asked for a name other than the one already written.
    #[error("this database is already named {0:?}; its name is written once")]
    AlreadyNamed(String),
    /// The name is not lowercase letters, digits and hyphens.
    #[error("database name {0:?} must be lowercase letters, digits and hyphens, at most 63")]
    InvalidName(String),
    /// The database could not be read or written.
    #[error("reading the database's name: {0}")]
    Db(String),
}

impl From<DbError> for IdentityError {
    fn from(e: DbError) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<sqlx::Error> for IdentityError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e.to_string())
    }
}

/// The database's name, or `None` before it is set.
pub async fn read(pool: &PgPool) -> Result<Option<String>, IdentityError> {
    Ok(
        sqlx::query_scalar!("select name from database_identity where only_row")
            .fetch_optional(pool)
            .await?,
    )
}

/// Names the database. Naming it again with the same name changes nothing; another name is
/// refused, since the name is written once.
pub async fn set(pool: &PgPool, name: &str) -> Result<(), IdentityError> {
    let valid = !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid {
        return Err(IdentityError::InvalidName(name.to_owned()));
    }
    sqlx::query!(
        "insert into database_identity (name) values ($1) on conflict (only_row) do nothing",
        name
    )
    .execute(pool)
    .await?;
    match read(pool).await? {
        Some(found) if found == name => Ok(()),
        Some(found) => Err(IdentityError::AlreadyNamed(found)),
        None => Err(IdentityError::Db("the name was not stored".to_owned())),
    }
}

/// Whether a command may run against a database named `found`, given `WT_DATABASE_NAME`
/// (`expected`) and `WT_ENV`. Without the key, outside prod, any database is accepted.
pub fn check(found: Option<&str>, expected: Option<&str>, env: Env) -> Result<(), IdentityError> {
    match (found, expected) {
        (_, None) if env == Env::Prod => Err(IdentityError::KeyRequired),
        (_, None) => Ok(()),
        (Some(found), Some(expected)) if found == expected => Ok(()),
        (Some(found), Some(expected)) => Err(IdentityError::Mismatch {
            found: found.to_owned(),
            expected: expected.to_owned(),
        }),
        (None, Some(expected)) => Err(IdentityError::Unnamed {
            expected: expected.to_owned(),
        }),
    }
}

/// Reads the database's name and checks it against `WT_DATABASE_NAME` and `WT_ENV`, returning
/// the name to print (`unnamed` before it is set).
pub async fn verify(
    pool: &PgPool,
    expected: Option<&str>,
    env: Env,
) -> Result<String, IdentityError> {
    let found = read(pool).await?;
    check(found.as_deref(), expected, env)?;
    Ok(found.unwrap_or_else(|| "unnamed".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_differs_from_the_key_is_refused_both_ways() {
        assert!(matches!(
            check(Some("pc"), Some("rds-v1"), Env::Dev),
            Err(IdentityError::Mismatch { .. })
        ));
        assert!(matches!(
            check(Some("rds-v1"), Some("pc"), Env::Prod),
            Err(IdentityError::Mismatch { .. })
        ));
        assert_eq!(check(Some("rds-v1"), Some("rds-v1"), Env::Prod), Ok(()));
    }

    #[test]
    fn prod_needs_the_key_and_an_unnamed_database_fails_a_set_key() {
        assert_eq!(
            check(Some("rds-v1"), None, Env::Prod),
            Err(IdentityError::KeyRequired)
        );
        assert_eq!(check(None, None, Env::Dev), Ok(()));
        assert!(matches!(
            check(None, Some("pc"), Env::Dev),
            Err(IdentityError::Unnamed { .. })
        ));
    }
}
