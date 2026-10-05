//! The `wt-api` binary: loads configuration, then serves the web app on `WT_BIND`.

use anyhow::Context;
use wt_api::{AppState, router};
use wt_core::Config;
use wt_core::telemetry::init_logging;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env().context("loading configuration")?;
    init_logging(config.env, &config.log)?;

    let pool = wt_db::lazy_pool(&config.database_url)?;
    let app = router(AppState { pool });

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!(bind = %config.bind, env = ?config.env, "wt-api listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving")?;
    tracing::info!("wt-api stopped");
    Ok(())
}

/// Resolves on Ctrl-C, or on SIGTERM, which is how ECS stops a task.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %error, "could not listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(error = %error, "could not listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
