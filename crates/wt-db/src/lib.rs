//! The sqlx connection pool, the migration runner, repositories and views.

use std::str::FromStr;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use wt_core::Secret;

pub mod answer;
pub mod audit;
pub mod consent;
pub mod correction;
pub mod display;
pub mod donation;
pub mod export;
pub mod feed;
pub mod glossary;
pub mod ingest;
pub mod job;
pub mod license;
pub mod llm;
pub mod locator;
pub mod privacy;
pub mod public_forms;
pub mod quarantine;
pub mod questions;
pub mod reference;
pub mod registry;
pub mod report;
pub mod rights;
pub mod storage;
pub mod takedown;
pub mod talk;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod tester;
pub mod text_takedown;
pub mod texts;
pub mod transcript;
pub mod triage;

/// Every migration in `migrations/`, embedded at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Database errors.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The connection string could not be parsed. The string itself is never included.
    #[error("the database connection string is not valid")]
    InvalidUrl,
    /// A query or connection failed.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// A migration failed to apply.
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// The request breaks a rule the database layer keeps, such as a reserved name.
    #[error("{0}")]
    Refused(String),
}

/// Connection options from a secret connection string.
pub fn connect_options(url: &Secret) -> Result<PgConnectOptions, DbError> {
    PgConnectOptions::from_str(url.expose()).map_err(|_| DbError::InvalidUrl)
}

/// A pool that connects on first use, so a service can start and report an unhealthy database
/// instead of exiting.
pub fn lazy_pool(url: &Secret) -> Result<PgPool, DbError> {
    Ok(PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy_with(connect_options(url)?))
}

/// A pool with one connection already open, for commands that need the database right away.
pub async fn connect(url: &Secret) -> Result<PgPool, DbError> {
    Ok(PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(connect_options(url)?)
        .await?)
}

/// The outcome of [`migrate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrateReport {
    /// Migrations applied by this run.
    pub applied: usize,
    /// Migrations known to this build.
    pub known: usize,
}

/// Applies every pending migration. Running it again on an up-to-date database applies nothing.
pub async fn migrate(pool: &PgPool) -> Result<MigrateReport, DbError> {
    let before = applied_count(pool).await?;
    MIGRATOR.run(pool).await?;
    let after = applied_count(pool).await?;
    Ok(MigrateReport {
        applied: after.saturating_sub(before),
        known: MIGRATOR.iter().count(),
    })
}

async fn applied_count(pool: &PgPool) -> Result<usize, DbError> {
    // Postgres resolves table names when it parses a statement, so the existence check has to be
    // its own query before the count can name the table.
    let exists: bool =
        sqlx::query_scalar("select to_regclass('public._sqlx_migrations') is not null")
            .fetch_one(pool)
            .await?;
    if !exists {
        return Ok(0);
    }
    let count: i64 =
        sqlx::query_scalar("select count(*) from public._sqlx_migrations where success")
            .fetch_one(pool)
            .await?;
    Ok(usize::try_from(count).unwrap_or(0))
}

/// The NOLOGIN group roles from Spec 02. A login role inherits from exactly one of them.
pub const GROUP_ROLES: [&str; 4] = ["wt_owner", "wt_pipeline", "wt_web", "wt_analyst"];

/// Why a login role could not be created.
#[derive(Debug, thiserror::Error)]
pub enum CreateLoginError {
    /// The group role is not one of [`GROUP_ROLES`].
    #[error("{0} is not a Wheel Turning group role")]
    UnknownGroup(String),
    /// The login name is not a plain lowercase identifier.
    #[error("login name {0:?} must be lowercase letters, digits and underscores")]
    InvalidLogin(String),
    /// The password is too short to be a generated secret.
    #[error("the password must be at least 24 characters")]
    WeakPassword,
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Whether a login was created or an existing one updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginOutcome {
    /// The login role did not exist and was created.
    Created,
    /// The login role existed; its password and membership were reset.
    Updated,
}

/// Creates or updates a login role that inherits `group` and starts every session as `group`,
/// so objects it creates belong to the group role. Idempotent: rerunning it resets the password
/// and membership. The connecting user needs CREATEROLE and admin rights on the group role.
pub async fn create_login(
    pool: &PgPool,
    group: &str,
    login: &str,
    password: &Secret,
) -> Result<LoginOutcome, CreateLoginError> {
    if !GROUP_ROLES.contains(&group) {
        return Err(CreateLoginError::UnknownGroup(group.to_owned()));
    }
    let valid_login = !login.is_empty()
        && login.len() <= 63
        && login.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && login
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid_login {
        return Err(CreateLoginError::InvalidLogin(login.to_owned()));
    }
    if password.expose().len() < 24 {
        return Err(CreateLoginError::WeakPassword);
    }

    let mut tx = pool.begin().await.map_err(DbError::from)?;
    let exists: bool =
        sqlx::query_scalar("select exists (select 1 from pg_roles where rolname = $1)")
            .bind(login)
            .fetch_one(&mut *tx)
            .await
            .map_err(DbError::from)?;

    // Role names and passwords cannot be bind parameters in DDL, so the server quotes them with
    // format(%I, %L) and the statement it returns is executed. Nothing is interpolated here.
    let statements = [
        if exists {
            "select format('alter role %I with login inherit password %L', $1::text, $2::text)"
        } else {
            "select format('create role %I with login inherit password %L', $1::text, $2::text)"
        },
        "select format('grant %I to %I', $3::text, $1::text)",
        "select format('alter role %I set role %I', $1::text, $3::text)",
    ];
    for builder in statements {
        let ddl: String = sqlx::query_scalar(builder)
            .bind(login)
            .bind(password.expose())
            .bind(group)
            .fetch_one(&mut *tx)
            .await
            .map_err(DbError::from)?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(ddl))
            .execute(&mut *tx)
            .await
            .map_err(DbError::from)?;
    }
    tx.commit().await.map_err(DbError::from)?;

    Ok(if exists {
        LoginOutcome::Updated
    } else {
        LoginOutcome::Created
    })
}

/// Checks that the database answers a query.
pub async fn ping(pool: &PgPool) -> Result<(), DbError> {
    let one = sqlx::query_scalar!(r#"select 1 as "one!""#)
        .fetch_one(pool)
        .await?;
    debug_assert_eq!(one, 1);
    Ok(())
}
