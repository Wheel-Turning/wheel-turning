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
use wt_retrieve::rank::LlmRanker;
use wt_retrieve::sensitive::triage::LlmTriage;
use wt_retrieve::sensitive::{Exemplars, SensitiveConfig};
use wt_retrieve::{Ranker, RetrievalConfig, Sensitivity, Triage};

use crate::Site;
use crate::mail::{Mailer, SesMailer, UnavailableMailer};
use crate::session::Keys;

/// Any error behind a [`SiteError`].
type Source = Box<dyn std::error::Error + Send + Sync>;

/// Why the site could not be built.
#[derive(Debug, thiserror::Error)]
#[error("{what}: {source}")]
pub struct SiteError {
    what: &'static str,
    source: Source,
}

/// A closure that wraps an error with what was being loaded.
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
    // The cross-encoder loads only when `[ranking]` asks for it: on the web service's CPUs it
    // took longer than the whole answer may (owner's decision of October 15, 2026). Without the
    // export, answers run on the fused order and say so (Spec 05, step 4).
    let onnx: Option<Arc<dyn Reranker>> = match retrieval.ranking.ranker {
        Ranker::Model => None,
        Ranker::Onnx => {
            let mut rerank_settings = OnnxSettings::new(&config.model_dir, &config.rerank_model);
            rerank_settings.runtime.clone_from(&config.ort_dylib);
            match OnnxReranker::load_pool(&rerank_settings, retrieval.retrieval.rerank_sessions) {
                Ok(r) => Some(Arc::new(r)),
                Err(MlError::Files(missing)) => {
                    tracing::warn!(model = %config.rerank_model, %missing, "no reranker loaded: relevance threshold off");
                    None
                }
                Err(e) => return Err(err("loading the reranker")(e)),
            }
        }
    };
    let (llm, triage): (Arc<dyn LlmClient>, Option<Arc<dyn Triage>>) = match claude(
        config, &dir, pool,
    ) {
        Ok(c) => {
            let c: Arc<dyn LlmClient> = Arc::new(c);
            let gate: Arc<dyn Triage> = Arc::new(LlmTriage::new(c.clone()));
            (c, Some(gate))
        }
        Err(e) => {
            tracing::warn!(error = %e, "no language model: answers have no synthesis line and every question logs triage_degraded");
            (Arc::new(UnavailableLlm), None)
        }
    };
    let reranker = match retrieval.ranking.ranker {
        Ranker::Model if config.llm_real_data_ok && triage.is_some() => {
            let ranker: Arc<dyn Reranker> = Arc::new(LlmRanker::new(
                llm.clone(),
                retrieval.ranking.batch,
                retrieval.ranking.max_chars,
            ));
            Some(ranker)
        }
        Ranker::Model => {
            tracing::warn!("no usable ranking model: answers run on the fused order");
            None
        }
        Ranker::Onnx => onnx.clone(),
    };
    let owner_name = std::env::var("WT_OWNER_NAME")
        .ok()
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    let trust_forwarded_for = std::env::var("WT_TRUST_FORWARDED_FOR").is_ok_and(|v| v == "true");
    let (mailer, public_url) = mailer().await;
    Ok(Site {
        limits: crate::Limits::new(retrieval.answer.ask_rate_per_hour),
        privacy: crate::privacy::Settings::from_env().map_err(err("reading the privacy keys"))?,
        owner_name,
        trust_forwarded_for,
        embedder: Arc::new(embedder),
        reranker,
        span_reranker: onnx,
        llm,
        real_data_ok: config.llm_real_data_ok,
        retrieval,
        common_words,
        sensitivity: Arc::new(sensitivity),
        triage,
        keys,
        secure_cookies: !config.env.is_dev(),
        site_dana_url: config.site_dana_url.clone(),
        scope_sizes: Arc::default(),
        mailer,
        public_url,
    })
}

/// `WT_MAIL_FROM` and `WT_PUBLIC_URL`, read from the environment: the sender address for SES and
/// the site's address for links. Without both, the rights form cannot confirm requests and says
/// so, pointing people to the rights mailbox instead.
async fn mailer() -> (Arc<dyn Mailer>, String) {
    let get = |key: &str| {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_owned())
            .filter(|v| !v.is_empty())
    };
    match (get("WT_MAIL_FROM"), get("WT_PUBLIC_URL")) {
        (Some(from), Some(url)) => (Arc::new(SesMailer::new(from).await), url),
        _ => {
            tracing::warn!(
                "no WT_MAIL_FROM or WT_PUBLIC_URL: removal requests cannot be confirmed"
            );
            (Arc::new(UnavailableMailer), String::new())
        }
    }
}

/// The Claude client for the web service's stages: synthesis, verification, the triage gate,
/// excerpts and ranking, set by `WT_LLM_MODEL_SYNTH`, `WT_LLM_MODEL_VERIFY`,
/// `WT_LLM_MODEL_TRIAGE`, `WT_LLM_MODEL_EXCERPT` and `WT_LLM_MODEL_RANK`, each falling back to
/// `WT_LLM_MODEL` (Spec 11, PB2.0). Real text is refused unless `WT_LLM_REAL_DATA_OK=true`, and
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
    // The triage gate's model is optional: without it, its calls fail, every question logs
    // triage_degraded, and the rules and the embedding check decide, which can never lower a
    // level.
    match config.llm_model_for(LlmStage::Sensitive) {
        Ok(model) if licenses.require(model).is_ok() && prices.get(model).is_some() => {
            models.insert(LlmStage::Sensitive.as_str(), model.to_owned());
        }
        _ => tracing::warn!("no usable triage model: the gate is degraded"),
    }
    // Excerpts fall back to the reranker and the trimmed passage without their model, and
    // ranking to the fused order.
    for (stage, missing) in [
        (
            LlmStage::Excerpt,
            "no usable excerpt model: excerpts are chosen without it",
        ),
        (
            LlmStage::Rank,
            "no usable ranking model: the model ranker's calls fail",
        ),
    ] {
        match config.llm_model_for(stage) {
            Ok(model) if licenses.require(model).is_ok() && prices.get(model).is_some() => {
                models.insert(stage.as_str(), model.to_owned());
            }
            _ => tracing::warn!("{missing}"),
        }
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
