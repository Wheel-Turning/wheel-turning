//! The `wt-api` binary: loads configuration, then serves the web app on `WT_BIND`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use wt_api::session::Keys;
use wt_api::{AppState, Site, router};
use wt_core::Config;
use wt_core::config::LlmStage;
use wt_core::telemetry::{LogOutput, init_logging};
use wt_extract::{
    ClaudeClient, ClaudeSettings, LlmClient, LogAlerter, PgLedger, PriceTable, UnavailableLlm,
};
use wt_ml::{MlError, OnnxEmbedder, OnnxReranker, OnnxSettings, Reranker};
use wt_retrieve::RetrievalConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env().context("loading configuration")?;
    init_logging(config.env, &config.log, LogOutput::Stdout)?;

    let pool = wt_db::lazy_pool(&config.database_url)?;
    let site = site(&config, &pool)?;
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
fn site(config: &Config, pool: &sqlx::PgPool) -> anyhow::Result<Site> {
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
    // Without the reranker export, answers run on the fused order and say so (Spec 05, step 4).
    let mut rerank_settings = OnnxSettings::new(&config.model_dir, &config.rerank_model);
    rerank_settings.runtime.clone_from(&config.ort_dylib);
    let reranker: Option<Arc<dyn Reranker>> = match OnnxReranker::load(&rerank_settings) {
        Ok(r) => Some(Arc::new(r)),
        Err(MlError::Files(missing)) => {
            tracing::warn!(model = %config.rerank_model, %missing, "no reranker loaded: relevance threshold off");
            None
        }
        Err(e) => return Err(e).context("loading the reranker"),
    };
    let llm: Arc<dyn LlmClient> = match claude(config, &dir, pool) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "no language model: answers have no synthesis line");
            Arc::new(UnavailableLlm)
        }
    };
    Ok(Site {
        embedder: Arc::new(embedder),
        reranker,
        llm,
        retrieval,
        common_words,
        keys,
        secure_cookies: !config.env.is_dev(),
    })
}

/// The Claude client for synthesis and verification: `WT_LLM_MODEL_SYNTH` and
/// `WT_LLM_MODEL_VERIFY`, each falling back to `WT_LLM_MODEL`. Real passage text is refused
/// unless `WT_LLM_REAL_DATA_OK=true`, and spend counts against the shared monthly cap.
fn claude(config: &Config, config_dir: &Path, pool: &sqlx::PgPool) -> anyhow::Result<ClaudeClient> {
    let api_key = config
        .anthropic_api_key
        .clone()
        .context("ANTHROPIC_API_KEY is not set")?;
    let prices = PriceTable::load(&config_dir.join("llm_prices.toml"))?;
    let licenses = wt_core::licenses::LicenseRecord::load(&config.model_licenses)?;
    let mut models = BTreeMap::new();
    for stage in [LlmStage::Synth, LlmStage::Verify] {
        let model = config.llm_model_for(stage)?;
        licenses.require(model)?;
        anyhow::ensure!(
            prices.get(model).is_some(),
            "{model} is not in llm_prices.toml, so its cost cannot be estimated"
        );
        models.insert(stage.as_str(), model.to_owned());
    }
    tracing::info!(
        ?models,
        real_data_ok = config.llm_real_data_ok,
        "synthesis model ready"
    );
    Ok(ClaudeClient::new(
        ClaudeSettings {
            base_url: "https://api.anthropic.com".into(),
            api_key,
            workspace_id: config.anthropic_workspace_id.clone(),
            models,
            real_data_ok: config.llm_real_data_ok,
            retry_base: Duration::from_secs(1),
        },
        prices,
        Arc::new(PgLedger::new(pool.clone(), config.llm_monthly_cap_usd)),
        Arc::new(LogAlerter),
    )?)
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
