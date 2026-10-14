//! A throwaway Postgres 16 with pgvector for integration tests. Each [`TestDb`] is its own
//! container, so tests never touch the dev database and never share state.

use std::time::{Duration, Instant};

use sqlx::Connection;
use sqlx::postgres::PgConnection;
use testcontainers::core::IntoContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use wt_core::Secret;

/// The image the compose file and RDS-compatible tests use.
pub const IMAGE: &str = "pgvector/pgvector";
/// The image tag.
pub const TAG: &str = "pg16";

/// How long [`TestDb::start`] waits for the server to accept a connection.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// The pause between connection attempts.
const READY_POLL: Duration = Duration::from_millis(250);

/// A running test database. The container stops when this value is dropped.
pub struct TestDb {
    /// The container, held so it lives as long as the database is used.
    _container: ContainerAsync<GenericImage>,
    /// The database's connection string.
    url: Secret,
}

/// Why a test database could not start.
#[derive(Debug, thiserror::Error)]
pub enum TestDbError {
    /// Docker could not start the container.
    #[error("could not start the test database container: {0}")]
    Container(#[from] testcontainers::TestcontainersError),
    /// The server did not accept a connection before the timeout.
    #[error("the test database did not accept connections within {READY_TIMEOUT:?}: {0}")]
    NotReady(#[source] sqlx::Error),
}

impl TestDb {
    /// Starts a fresh, empty database and waits until it accepts connections.
    pub async fn start() -> Result<Self, TestDbError> {
        // Readiness is checked by connecting, not by waiting on log lines: the container's log
        // stream can end early on a loaded Docker host, which failed the wait with `EndOfStream`
        // while Postgres was still initialising. The image's init server listens only on the
        // Unix socket, so a TCP connection succeeds only once the real server is up.
        let container = GenericImage::new(IMAGE, TAG)
            .with_exposed_port(5432.tcp())
            .with_env_var("POSTGRES_USER", "wt")
            .with_env_var("POSTGRES_PASSWORD", "wt")
            .with_env_var("POSTGRES_DB", "wt")
            .start()
            .await?;
        let host = container.get_host().await?;
        let port = container.get_host_port_ipv4(5432.tcp()).await?;
        let url = Secret::new(format!("postgres://wt:wt@{host}:{port}/wt"));
        wait_until_ready(&url).await?;
        Ok(Self {
            _container: container,
            url,
        })
    }

    /// The connection string for the superuser `wt`.
    pub fn url(&self) -> &Secret {
        &self.url
    }
}

/// Connects repeatedly until the server answers a ping or [`READY_TIMEOUT`] passes.
async fn wait_until_ready(url: &Secret) -> Result<(), TestDbError> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let attempt = match PgConnection::connect(url.expose()).await {
            Ok(mut conn) => {
                let pinged = conn.ping().await;
                // Closing is best effort: the server is up whether or not the goodbye lands.
                let _ = conn.close().await;
                pinged
            }
            Err(e) => Err(e),
        };
        match attempt {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => return Err(TestDbError::NotReady(e)),
            Err(_) => tokio::time::sleep(READY_POLL).await,
        }
    }
}
