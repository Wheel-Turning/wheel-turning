//! A throwaway Postgres 16 with pgvector for integration tests. Each [`TestDb`] is its own
//! container, so tests never touch the dev database and never share state.

use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use wt_core::Secret;

/// The image the compose file and RDS-compatible tests use.
pub const IMAGE: &str = "pgvector/pgvector";
/// The image tag.
pub const TAG: &str = "pg16";

/// A running test database. The container stops when this value is dropped.
pub struct TestDb {
    _container: ContainerAsync<GenericImage>,
    url: Secret,
}

/// Why a test database could not start.
#[derive(Debug, thiserror::Error)]
pub enum TestDbError {
    /// Docker could not start the container.
    #[error("could not start the test database container: {0}")]
    Container(#[from] testcontainers::TestcontainersError),
}

impl TestDb {
    /// Starts a fresh, empty database and waits until it accepts connections.
    pub async fn start() -> Result<Self, TestDbError> {
        // Postgres logs "ready to accept connections" once for the init server and once for the
        // real one, so wait for the line that only the real server prints after init.
        let container = GenericImage::new(IMAGE, TAG)
            .with_exposed_port(5432.tcp())
            .with_wait_for(WaitFor::message_on_stdout(
                "PostgreSQL init process complete; ready for start up.",
            ))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_USER", "wt")
            .with_env_var("POSTGRES_PASSWORD", "wt")
            .with_env_var("POSTGRES_DB", "wt")
            .start()
            .await?;
        let host = container.get_host().await?;
        let port = container.get_host_port_ipv4(5432.tcp()).await?;
        let url = Secret::new(format!("postgres://wt:wt@{host}:{port}/wt"));
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
