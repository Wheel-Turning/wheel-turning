//! The pipeline stages a worker runs past classification (Spec 04): `audio`, `correct`,
//! `extract`, `resolve` and `index`. Everything a stage needs is built once at startup, and a worker
//! asked to run a stage whose settings are missing refuses to start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    ClaudeClient, ClaudeSettings, DataClass, LlmClient, LlmError, LlmLabeler, LogAlerter, PgLedger,
    PriceTable, UnavailableLlm,
};
use wt_index::ChunkSettings;
use wt_ingest::audio::{self, AudioDeps, AudioError};
use wt_ingest::config::{ClassifyConfig, Series};
use wt_ingest::media::MediaFetcher;
use wt_ingest::textjobs::{self, PgHandList, TextJobError};
use wt_ml::{Embedder, OnnxEmbedder, OnnxSettings};
use wt_registry::glossary::Glossary;
use wt_segment::{
    LabelError, RuleFallbackLabeler, SegmentSettings, SidecarDiarizer, WindowLabeler,
};

/// `[segment]`, `[asr]` and `[chunk]` from `config/pipeline.toml`.
#[derive(Debug, Default, Deserialize)]
struct Sections {
    #[serde(default)]
    segment: SegmentSettings,
    #[serde(default)]
    asr: AsrSettings,
    #[serde(default)]
    chunk: ChunkSettings,
}

/// Refuses a model the license record does not list with a revision (Spec 04).
pub fn require_licensed(config: &Config, model: &str) -> anyhow::Result<()> {
    wt_core::licenses::LicenseRecord::load(&config.model_licenses)?.require(model)?;
    Ok(())
}

/// Loads the embedding model named by `WT_MODEL_DIR` and `WT_EMBED_MODEL`.
pub fn embedder(config: &Config) -> anyhow::Result<OnnxEmbedder> {
    require_licensed(config, &config.embed_model)?;
    let mut settings = OnnxSettings::new(&config.model_dir, &config.embed_model);
    settings.runtime.clone_from(&config.ort_dylib);
    OnnxEmbedder::load(&settings).context("loading the embedding model")
}

/// `[asr]` from `config/pipeline.toml`.
pub fn asr_settings(config_dir: &Path) -> anyhow::Result<AsrSettings> {
    let text = std::fs::read_to_string(config_dir.join("pipeline.toml"))
        .context("reading pipeline.toml")?;
    let sections: Sections = toml::from_str(&text).context("reading [asr]")?;
    Ok(sections.asr)
}

/// `[chunk]` from `config/pipeline.toml`.
pub fn chunk_settings(config_dir: &Path) -> anyhow::Result<ChunkSettings> {
    let text = std::fs::read_to_string(config_dir.join("pipeline.toml"))
        .context("reading pipeline.toml")?;
    let sections: Sections = toml::from_str(&text).context("reading [chunk]")?;
    Ok(sections.chunk)
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
    chunk: ChunkSettings,
    series: Vec<Series>,
    whisper: Option<WhisperCli>,
    diarizer: Option<SidecarDiarizer>,
    fetcher: Option<MediaFetcher>,
    llm: Option<ClaudeClient>,
    /// `WT_LLM_REAL_DATA_OK`: whether real talks may go to the model.
    real_data_ok: bool,
    embedder: Option<OnnxEmbedder>,
    hand_list: PgHandList,
    actor: String,
    /// Talks indexed since the last `ANALYZE passage`.
    indexed: AtomicUsize,
    /// For `ocr_chunk` and `text_assemble` jobs.
    texts: Option<TextJobs>,
}

/// What text jobs need (Spec 08, Large documents).
struct TextJobs {
    tools: wt_ingest::texts::pdf::tools::Poppler,
    word_list: PathBuf,
    allowed_codes: Vec<String>,
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
            let file = model
                .file_name()
                .and_then(|n| n.to_str())
                .context("WT_WHISPER_MODEL must name a model file")?;
            require_licensed(config, file)?;
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
        let texts = if kinds.contains(&JobKind::OcrChunk) || kinds.contains(&JobKind::TextAssemble)
        {
            let pipeline = config_dir.join("pipeline.toml");
            Some(TextJobs {
                tools: wt_ingest::texts::pdf::tools::Poppler::from_env(),
                word_list: wt_ingest::config::TextSettings::load(&pipeline)?.word_list,
                allowed_codes: wt_ingest::config::LicenseSettings::load(&pipeline)?
                    .allowed_license_codes,
            })
        } else {
            None
        };
        let embedder = if kinds.contains(&JobKind::Index) || kinds.contains(&JobKind::TextAssemble)
        {
            Some(embedder(config)?)
        } else {
            None
        };
        let needs_llm = kinds
            .iter()
            .any(|k| matches!(k, JobKind::Audio | JobKind::Extract));
        // Without an API key the worker still runs (owner's decision of October 7, 2026):
        // ambiguous windows get the rule fallback and extraction is staged as skipped.
        let llm = if needs_llm && config.anthropic_api_key.is_some() {
            Some(claude(pool, config, config_dir)?)
        } else {
            if needs_llm {
                tracing::warn!(
                    "no ANTHROPIC_API_KEY: ambiguous windows get the rule fallback and                      extraction is skipped"
                );
            }
            None
        };
        Ok(Self {
            pool: pool.clone(),
            objects: objects(config)?,
            scratch_root: config.scratch_dir.clone(),
            segment: sections.segment,
            asr: sections.asr,
            chunk: sections.chunk,
            series: cfg.series.clone(),
            whisper,
            diarizer,
            fetcher,
            llm,
            real_data_ok: config.llm_real_data_ok,
            embedder,
            hand_list: PgHandList::new(pool.clone()).with_stoplist(
                wt_ingest::config::CitationSettings::load(&config_dir.join("pipeline.toml"))?
                    .work_alias_stoplist,
            ),
            actor: actor.to_owned(),
            indexed: AtomicUsize::new(0),
            texts,
        })
    }

    /// Runs one claimed job of a stage kind.
    pub async fn run(&self, conn: &mut sqlx::PgConnection, job: &Job) -> anyhow::Result<Ran> {
        if matches!(job.kind, JobKind::OcrChunk | JobKind::TextAssemble) {
            return self.run_text(conn, job).await;
        }
        let talk = job.talk_id.context("a stage job has no talk")?;
        let mut db = self.pool.acquire().await?;
        let glossary = Glossary::new(&wt_db::glossary::load(&mut db).await?)?;
        let outcome = match job.kind {
            JobKind::Audio => {
                let (labeler, hints) = self.audio_parts(conn, talk).await?;
                let ffmpeg = Ffmpeg::default();
                let deps = self.audio_deps(labeler.as_ref(), &hints, &ffmpeg)?;
                let retranscribe = job
                    .payload
                    .get("retranscribe")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                match audio::run(conn, talk, &deps, retranscribe).await {
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
                let unavailable = UnavailableLlm;
                let llm: &dyn LlmClient = match &self.llm {
                    Some(client) => client,
                    None => &unavailable,
                };
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
            JobKind::Index => {
                let embedder = self
                    .embedder
                    .as_ref()
                    .context("this worker was not started for index jobs")?;
                let outcome = textjobs::run_index(
                    conn,
                    talk,
                    &self.objects,
                    &glossary,
                    &self.chunk,
                    embedder as &dyn Embedder,
                )
                .await?;
                if matches!(&outcome, Ok(i) if i.written) {
                    self.indexed.fetch_add(1, Ordering::Relaxed);
                }
                format!("{outcome:?}")
            }
            other => bail!("no stage handler for {other} jobs"),
        };
        Ok(Ran::Done(outcome))
    }

    /// Runs an `ocr_chunk` or `text_assemble` job. OCR is CPU work in subprocesses, one page at
    /// a time; run `wt worker --kinds ocr_chunk --concurrency <cores>`.
    async fn run_text(&self, conn: &mut sqlx::PgConnection, job: &Job) -> anyhow::Result<Ran> {
        use wt_ingest::texts::large;
        let texts = self
            .texts
            .as_ref()
            .context("this worker was not started for text jobs")?;
        let scratch = self.scratch_root.join("texts");
        let outcome = match job.kind {
            JobKind::OcrChunk => {
                let input: large::ChunkJob = serde_json::from_value(job.payload.clone())
                    .context("reading the ocr_chunk payload")?;
                let o =
                    large::run_chunk(conn, &self.objects, &texts.tools, &scratch, &input).await?;
                format!("{o:?}")
            }
            JobKind::TextAssemble => {
                let input: large::AssembleJob = serde_json::from_value(job.payload.clone())
                    .context("reading the text_assemble payload")?;
                let embedder = self
                    .embedder
                    .as_ref()
                    .context("this worker was not started for text_assemble jobs")?;
                let env = wt_ingest::texts::PdfEnv {
                    tools: &texts.tools,
                    scratch,
                    word_list: texts.word_list.clone(),
                };
                let report = large::run_assemble(
                    conn,
                    &self.objects,
                    embedder as &dyn Embedder,
                    &env,
                    &texts.allowed_codes,
                    &input,
                )
                .await?;
                format!(
                    "applied {}, {} units, {} parts",
                    report.applied,
                    report.units,
                    report.parts.len()
                )
            }
            other => bail!("{other} is not a text job"),
        };
        Ok(Ran::Done(outcome))
    }

    /// The labeler and recognition hints an audio run needs for `talk`.
    async fn audio_parts(
        &self,
        conn: &mut sqlx::PgConnection,
        talk: wt_core::ids::TalkId,
    ) -> anyhow::Result<(Box<dyn WindowLabeler + '_>, AsrHints)> {
        let class = textjobs::data_class(conn, talk).await?;
        let terms = wt_db::glossary::load(conn).await?;
        let hints = AsrHints {
            glossary_prompt: if self.asr.glossary_prompt {
                glossary_prompt(terms.iter().map(|t| t.canonical.as_str()))
            } else {
                String::new()
            },
            language: self.asr.language.clone(),
        };
        // The model labels ambiguous windows only when it may see the talk: a fixture, or a
        // real talk once the owner sets `WT_LLM_REAL_DATA_OK`. Otherwise the rule fallback does.
        let labeler: Box<dyn WindowLabeler + '_> = match &self.llm {
            Some(llm) if class == DataClass::Synthetic || self.real_data_ok => {
                Box::new(LlmLabeler::new(llm, class))
            }
            _ => Box::new(RuleFallbackLabeler),
        };
        Ok((labeler, hints))
    }

    fn audio_deps<'a>(
        &'a self,
        labeler: &'a dyn WindowLabeler,
        hints: &'a AsrHints,
        ffmpeg: &'a Ffmpeg,
    ) -> anyhow::Result<AudioDeps<'a>> {
        let (Some(whisper), Some(diarizer), Some(fetcher)) =
            (&self.whisper, &self.diarizer, &self.fetcher)
        else {
            bail!("this worker was not started for audio jobs");
        };
        Ok(AudioDeps {
            transcriber: whisper,
            diarizer,
            labeler,
            converter: ffmpeg,
            fetcher,
            objects: &self.objects,
            scratch_root: &self.scratch_root,
            segment: &self.segment,
            hints,
            actor: &self.actor,
        })
    }

    /// Runs the audio steps for one talk without persisting anything, for the segmentation
    /// sample report.
    pub async fn sample(
        &self,
        conn: &mut sqlx::PgConnection,
        talk: wt_core::ids::TalkId,
    ) -> anyhow::Result<audio::SampleOutcome> {
        let (labeler, hints) = self.audio_parts(conn, talk).await?;
        let ffmpeg = Ffmpeg::default();
        let deps = self.audio_deps(labeler.as_ref(), &hints, &ffmpeg)?;
        Ok(audio::sample(conn, talk, &deps).await?)
    }

    /// When the queue is empty: `ANALYZE passage` if talks were indexed since the last one.
    pub async fn idle(&self, conn: &mut sqlx::PgConnection) -> anyhow::Result<()> {
        let n = self.indexed.swap(0, Ordering::Relaxed);
        if n > 0 {
            wt_index::index::analyze(conn).await?;
            tracing::info!(talks = n, "analyzed passage after indexing");
        }
        Ok(())
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
        let model = config.llm_model_for(stage)?;
        require_licensed(config, model)?;
        models.insert(stage.as_str(), model.to_owned());
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

/// Spec 07, takedown step 3: after the database commit, deletes each talk's stored objects,
/// `transcripts/<talk>/` and `citations/<talk>.json`, then checks nothing is left. Returns how
/// many objects were deleted. Retrying a failure and the audit's check come with M8.
pub fn delete_talk_objects(
    config: &Config,
    talks: &[wt_core::ids::TalkId],
) -> anyhow::Result<usize> {
    let store = objects(config)?;
    audio::delete_talk_objects(&store, talks).context("deleting the talks' stored objects")
}

/// A takedown's printed report with the deleted objects added.
pub fn with_objects(report: &impl serde::Serialize, deleted: usize) -> serde_json::Value {
    let mut value = serde_json::to_value(report).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.insert("objects_deleted".into(), serde_json::json!(deleted));
    }
    value
}
