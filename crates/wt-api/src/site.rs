//! Builds the [`Site`] from configuration: the models, the retrieval settings, sensitive-question
//! detection and the session key. The web server and `wt eval run` both load it here, so an evaluation answers with the
//! same models and settings as the page (Spec 07, Evaluation harness).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use wt_core::Config;
use wt_core::config::LlmStage;
use wt_extract::{
    ClaudeClient, ClaudeSettings, LlmClient, LogAlerter, PgLedger, PriceTable, UnavailableLlm,
};
use wt_ml::{MlError, OnnxEmbedder, OnnxReranker, OnnxSettings, Reranker};
use wt_retrieve::sensitive::{Exemplars, SensitiveConfig};
use wt_retrieve::{ModelCheck, RetrievalConfig, Sensitivity};

use crate::Site;
use crate::sensitive::LlmSensitivity;
use crate::session::Keys;

type Source = Box<dyn std::error::Error + Send + Sync>;

/// Why the site could not be built.
#[derive(Debug, thiserror::Error)]
#[error("{what}: {source}")]
pub struct SiteError {
    what: &'static str,
    source: Source,
}

fn err<E: Into<Source>>(what: &'static str) -> impl FnOnce(E) -> SiteError {
    move |e| SiteError {
        what,
        source: e.into(),
    }
}

/// The configuration directory: `WT_CONFIG_DIR`, default `config`. It holds `retrieval.toml`,
/// `common_words.txt`, `llm_prices.toml`, `sensitive.toml` and `sensitive_exemplars.toml`.
pub fn config_dir() -> PathBuf {
    PathBuf::from(std::env::var("WT_CONFIG_DIR").unwrap_or_else(|_| "config".into()))
}

/// Loads the embedder, the reranker if installed, the language model if it may be called, the
/// retrieval settings, sensitive-question detection and the session key.
pub async fn load(config: &Config, pool: &sqlx::PgPool) -> Result<Site, SiteError> {
    let mut settings = OnnxSettings::new(&config.model_dir, &config.embed_model);
    settings.runtime.clone_from(&config.ort_dylib);
    let embedder = OnnxEmbedder::load(&settings).map_err(err("loading the embedding model"))?;
    let dir = config_dir();
    let retrieval = RetrievalConfig::load(&dir.join("retrieval.toml"))
        .map_err(err("loading retrieval.toml"))?;
    let common_words = std::fs::read_to_string(dir.join("common_words.txt")).unwrap_or_default();
    // Spec 05: without its rules and exemplars the server does not start.
    let sensitivity = Sensitivity::prepare(
        &SensitiveConfig::load(&dir.join("sensitive.toml"))
            .map_err(err("loading sensitive.toml"))?,
        &Exemplars::load(&dir.join("sensitive_exemplars.toml"))
            .map_err(err("loading sensitive_exemplars.toml"))?,
        &embedder,
    )
    .await
    .map_err(err("preparing sensitive-question detection"))?;
    let keys = match &config.session_secret {
        Some(secret) => Keys::new(secret.expose().as_bytes()),
        None => {
            tracing::warn!("no WT_SESSION_SECRET: sessions end when the server stops (dev only)");
            Keys::random().map_err(err("generating a session key"))?
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
        Err(e) => return Err(err("loading the reranker")(e)),
    };
    let (llm, sensitivity_model): (Arc<dyn LlmClient>, Option<Arc<dyn ModelCheck>>) = match claude(
        config, &dir, pool,
    ) {
        Ok(c) => {
            let c: Arc<dyn LlmClient> = Arc::new(c);
            let check: Arc<dyn ModelCheck> = Arc::new(LlmSensitivity::new(c.clone()));
            (c, Some(check))
        }
        Err(e) => {
            tracing::warn!(error = %e, "no language model: answers have no synthesis line and sensitive-question detection has no model layer");
            (Arc::new(UnavailableLlm), None)
        }
    };
    let owner_name = std::env::var("WT_OWNER_NAME")
        .ok()
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    let trust_forwarded_for = std::env::var("WT_TRUST_FORWARDED_FOR").is_ok_and(|v| v == "true");
    Ok(Site {
        limits: crate::Limits::new(retrieval.answer.ask_rate_per_hour),
        privacy: crate::privacy::Settings::from_env().map_err(err("reading the privacy keys"))?,
        owner_name,
        trust_forwarded_for,
        embedder: Arc::new(embedder),
        reranker,
        llm,
        retrieval,
        common_words,
        sensitivity: Arc::new(sensitivity),
        sensitivity_model,
        keys,
        secure_cookies: !config.env.is_dev(),
    })
}

/// The Claude client for synthesis, verification and the sensitivity model layer:
/// `WT_LLM_MODEL_SYNTH`, `WT_LLM_MODEL_VERIFY` and `WT_LLM_MODEL_EXTRACT` (Haiku-class), each
/// falling back to `WT_LLM_MODEL`. Real text is refused unless `WT_LLM_REAL_DATA_OK=true`, and
/// spend counts against the shared monthly cap.
fn claude(
    config: &Config,
    config_dir: &Path,
    pool: &sqlx::PgPool,
) -> Result<ClaudeClient, SiteError> {
    let api_key = config
        .anthropic_api_key
        .clone()
        .ok_or_else(|| err("ANTHROPIC_API_KEY")("is not set"))?;
    let prices = PriceTable::load(&config_dir.join("llm_prices.toml"))
        .map_err(err("loading llm_prices.toml"))?;
    let licenses = wt_core::licenses::LicenseRecord::load(&config.model_licenses)
        .map_err(err("loading the model licenses"))?;
    let mut models = BTreeMap::new();
    for stage in [LlmStage::Synth, LlmStage::Verify] {
        let model = config
            .llm_model_for(stage)
            .map_err(err("choosing the model"))?;
        licenses
            .require(model)
            .map_err(err("checking the license"))?;
        if prices.get(model).is_none() {
            return Err(err("pricing")(format!(
                "{model} is not in llm_prices.toml, so its cost cannot be estimated"
            )));
        }
        models.insert(stage.as_str(), model.to_owned());
    }
    // The sensitivity model layer is optional: without its model, its calls fail and the rules
    // and the embedding check decide, which can never lower a level.
    match config.llm_model_for(LlmStage::Sensitive) {
        Ok(model) if licenses.require(model).is_ok() && prices.get(model).is_some() => {
            models.insert(LlmStage::Sensitive.as_str(), model.to_owned());
        }
        _ => tracing::warn!("no usable sensitivity model: that layer is off"),
    }
    tracing::info!(
        ?models,
        real_data_ok = config.llm_real_data_ok,
        "synthesis model ready"
    );
    ClaudeClient::new(
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
    )
    .map_err(err("building the Claude client"))
}
