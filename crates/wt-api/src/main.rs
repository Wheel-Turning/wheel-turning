//! The `wt-api` binary: loads configuration, then serves the web app on `WT_BIND`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use wt_api::session::Keys;
use wt_api::{AppState, Site, router};
use wt_core::Config;
use wt_core::telemetry::{LogOutput, init_logging};
use wt_ml::{OnnxEmbedder, OnnxSettings};
use wt_retrieve::RetrievalConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env().context("loading configuration")?;
    init_logging(config.env, &config.log, LogOutput::Stdout)?;

    let pool = wt_db::lazy_pool(&config.database_url)?;
    let site = site(&config)?;
    let app = router(AppState {
        pool,
        site: Arc::new(site),
    });

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

/// Loads the embedder, the retrieval settings and the session key. `WT_CONFIG_DIR` (default
/// `config`) holds `retrieval.toml` and `common_words.txt`.
fn site(config: &Config) -> anyhow::Result<Site> {
    let mut settings = OnnxSettings::new(&config.model_dir, &config.embed_model);
    settings.runtime.clone_from(&config.ort_dylib);
    let embedder = OnnxEmbedder::load(&settings).context("loading the embedding model")?;
    let dir = PathBuf::from(std::env::var("WT_CONFIG_DIR").unwrap_or_else(|_| "config".into()));
    let retrieval = RetrievalConfig::load(&dir.join("retrieval.toml"))?;
    let common_words = std::fs::read_to_string(dir.join("common_words.txt")).unwrap_or_default();
    let keys = match &config.session_secret {
        Some(secret) => Keys::new(secret.expose().as_bytes()),
        None => {
            tracing::warn!("no WT_SESSION_SECRET: sessions end when the server stops (dev only)");
            Keys::random().context("generating a session key")?
        }
    };
    // The reranker model is not exported yet (Spec 05, step 4): answers run on the fused order
    // and say so.
    tracing::warn!(model = %config.rerank_model, "no reranker loaded: relevance threshold off");
    Ok(Site {
        embedder: Arc::new(embedder),
        reranker: None,
        retrieval,
        common_words,
        keys,
        secure_cookies: !config.env.is_dev(),
    })
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
