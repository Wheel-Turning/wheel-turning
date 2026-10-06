//! The pipeline stages a worker runs past classification (Spec 04): `audio`, `correct`,
//! `extract` and `resolve`. Everything a stage needs is built once at startup, and a worker
//! asked to run a stage whose settings are missing refuses to start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::Deserialize;
use sqlx::PgPool;
use wt_asr::{AsrHints, AsrSettings, WhisperCli, glossary_prompt};
use wt_core::Config;
use wt_core::config::LlmStage;
use wt_core::objects::LocalObjects;
use wt_core::scratch::Ffmpeg;
use wt_db::job::{Job, JobKind};
use wt_extract::{
    ClaudeClient, ClaudeSettings, LlmError, LlmLabeler, LogAlerter, PgLedger, PriceTable,
};
use wt_ingest::audio::{self, AudioDeps, AudioError};
use wt_ingest::config::{ClassifyConfig, Series};
use wt_ingest::media::MediaFetcher;
use wt_ingest::textjobs::{self, PgHandList, TextJobError};
use wt_registry::glossary::Glossary;
use wt_segment::{LabelError, SegmentSettings, SidecarDiarizer};

/// `[segment]` and `[asr]` from `config/pipeline.toml`.
#[derive(Debug, Default, Deserialize)]
struct Sections {
    #[serde(default)]
    segment: SegmentSettings,
    #[serde(default)]
    asr: AsrSettings,
}

/// How a stage job ended, for the worker loop.
pub enum Ran {
    /// Done; a short description for the log.
    Done(String),
    /// The language-model cap refused it: the job waits for the next month.
    Capped(String),
}

/// The local object store: the transcripts bucket's stand-in while `WT_TRANSCRIPT_BUCKET` is
/// unset (Spec 01).
pub fn objects(config: &Config) -> anyhow::Result<LocalObjects> {
    if config.transcript_bucket.is_some() {
        bail!(
            "WT_TRANSCRIPT_BUCKET is set, but this build stores transcripts locally only; unset it"
        );
    }
    Ok(LocalObjects::new(PathBuf::from("data/objects")))
}

/// The stages a worker can run.
pub struct Stages {
    pool: PgPool,
    objects: LocalObjects,
    scratch_root: PathBuf,
    segment: SegmentSettings,
    asr: AsrSettings,
    series: Vec<Series>,
    whisper: Option<WhisperCli>,
    diarizer: Option<SidecarDiarizer>,
    fetcher: Option<MediaFetcher>,
    llm: Option<ClaudeClient>,
    hand_list: PgHandList,
    actor: String,
}

impl Stages {
    /// Builds what `kinds` need, refusing when a setting is missing.
    pub async fn build(
        pool: &PgPool,
        config: &Config,
        config_dir: &Path,
        cfg: &ClassifyConfig,
        kinds: &[JobKind],
        actor: &str,
    ) -> anyhow::Result<Self> {
        if kinds.contains(&JobKind::Index) {
            bail!("no handler for index jobs yet: it arrives with the embedder");
        }
        let text = std::fs::read_to_string(config_dir.join("pipeline.toml"))
            .context("reading pipeline.toml")?;
        let sections: Sections = toml::from_str(&text).context("reading [segment] and [asr]")?;
        let (whisper, diarizer, fetcher) = if kinds.contains(&JobKind::Audio) {
            let binary = config
                .whisper_cli
                .clone()
                .context("WT_WHISPER_CLI must name whisper-cli to run audio jobs")?;
            let model = config
                .whisper_model
                .clone()
                .context("WT_WHISPER_MODEL must name the model to run audio jobs")?;
            let mut conn = pool.acquire().await?;
            let exclusions = wt_ingest::http::Exclusions::load(&mut conn).await?;
            (
                Some(WhisperCli::new(binary, model, &sections.asr)),
                Some(SidecarDiarizer::new(&config.diarize_url)?),
                Some(MediaFetcher::new(
                    exclusions,
                    config.allow_third_party_fetch,
                )?),
            )
        } else {
            (None, None, None)
        };
        let needs_llm = kinds
            .iter()
            .any(|k| matches!(k, JobKind::Audio | JobKind::Extract));
        let llm = if needs_llm {
            Some(claude(pool, config, config_dir)?)
        } else {
            None
        };
        Ok(Self {
            pool: pool.clone(),
            objects: objects(config)?,
            scratch_root: config.scratch_dir.clone(),
            segment: sections.segment,
            asr: sections.asr,
            series: cfg.series.clone(),
            whisper,
            diarizer,
            fetcher,
            llm,
            hand_list: PgHandList::new(pool.clone()),
            actor: actor.to_owned(),
        })
    }

    /// Runs one claimed job of a stage kind.
    pub async fn run(&self, conn: &mut sqlx::PgConnection, job: &Job) -> anyhow::Result<Ran> {
        let talk = job.talk_id.context("a stage job has no talk")?;
        let mut db = self.pool.acquire().await?;
        let glossary = Glossary::new(&wt_db::glossary::load(&mut db).await?)?;
        let outcome = match job.kind {
            JobKind::Audio => {
                let (Some(whisper), Some(diarizer), Some(fetcher), Some(llm)) =
                    (&self.whisper, &self.diarizer, &self.fetcher, &self.llm)
                else {
                    bail!("this worker was not started for audio jobs");
                };
                let class = textjobs::data_class(conn, talk).await?;
                let labeler = LlmLabeler::new(llm, class);
                let terms = wt_db::glossary::load(&mut db).await?;
                let hints = AsrHints {
                    glossary_prompt: glossary_prompt(terms.iter().map(|t| t.canonical.as_str())),
                    language: self.asr.language.clone(),
                };
                let deps = AudioDeps {
                    transcriber: whisper,
                    diarizer,
                    labeler: &labeler,
                    converter: &Ffmpeg::default(),
                    fetcher,
                    objects: &self.objects,
                    scratch_root: &self.scratch_root,
                    segment: &self.segment,
                    hints: &hints,
                    actor: &self.actor,
                };
                match audio::run(conn, talk, &deps).await {
                    Ok(o) => format!("{o:?}"),
                    Err(AudioError::Label(LabelError::CapReached(m))) => return Ok(Ran::Capped(m)),
                    Err(e) => return Err(e.into()),
                }
            }
            JobKind::Correct => {
                // No English word list is loaded yet, so every near miss is suggested; nothing
                // is applied either way.
                let outcome =
                    textjobs::run_correct(conn, talk, &self.objects, &glossary, &|_| false).await?;
                format!("{outcome:?}")
            }
            JobKind::Extract => {
                let llm = self
                    .llm
                    .as_ref()
                    .context("this worker has no language model")?;
                match textjobs::run_extract(conn, talk, &self.objects, &glossary, llm).await {
                    Ok(o) => format!("{o:?}"),
                    Err(TextJobError::Llm(e @ LlmError::CapReached { .. })) => {
                        return Ok(Ran::Capped(e.to_string()));
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            JobKind::Resolve => {
                let outcome =
                    textjobs::run_resolve(conn, talk, &self.objects, &self.series, &self.hand_list)
                        .await?;
                format!("{outcome:?}")
            }
            other => bail!("no stage handler for {other} jobs"),
        };
        Ok(Ran::Done(outcome))
    }
}

/// The Claude client for the ingestion stages, refusing to start without a model or key.
fn claude(pool: &PgPool, config: &Config, config_dir: &Path) -> anyhow::Result<ClaudeClient> {
    let api_key = config
        .anthropic_api_key
        .clone()
        .context("ANTHROPIC_API_KEY must be set for the language-model stages")?;
    let mut models = BTreeMap::new();
    for stage in [LlmStage::Extract, LlmStage::Classify] {
        models.insert(stage.as_str(), config.llm_model_for(stage)?.to_owned());
    }
    let prices = PriceTable::load(&config_dir.join("llm_prices.toml"))?;
    for model in models.values() {
        if prices.get(model).is_none() {
            bail!("{model} is not in llm_prices.toml, so its cost cannot be estimated");
        }
    }
    Ok(ClaudeClient::new(
        ClaudeSettings {
            base_url: "https://api.anthropic.com".into(),
            api_key,
            models,
            real_data_ok: config.llm_real_data_ok,
            retry_base: Duration::from_secs(2),
        },
        prices,
        Arc::new(PgLedger::new(pool.clone(), config.llm_monthly_cap_usd)),
        Arc::new(LogAlerter),
    )?)
}
