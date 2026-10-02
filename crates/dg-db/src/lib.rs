//! The sqlx connection pool, the migration runner, repositories and views.

use std::str::FromStr;
use std::time::Duration;

use dg_core::Secret;
use sqlx::PgPool;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[cfg(feature = "test-support")]
pub mod test_support;

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

/// Checks that the database answers a query.
pub async fn ping(pool: &PgPool) -> Result<(), DbError> {
    let one = sqlx::query_scalar!(r#"select 1 as "one!""#)
        .fetch_one(pool)
        .await?;
    debug_assert_eq!(one, 1);
    Ok(())
}
