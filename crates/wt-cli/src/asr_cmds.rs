//! `wt asr benchmark` and `wt asr requeue` (Spec 04, Transcription model and where it runs).

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::Subcommand;
use sqlx::PgPool;
use wt_asr::bench::{self, BenchConfig, WhisperEngine};
use wt_asr::{AsrHints, glossary_prompt, model_name};
use wt_core::Config;
use wt_core::ids::TalkId;
use wt_core::scratch::{Converter, Ffmpeg, ScratchDir};

use crate::Ctx;
use crate::commands;
use crate::stages;

/// What `wt asr benchmark` runs on.
pub struct BenchArgs {
    /// A clip to copy into scratch and convert.
    pub clip: Option<PathBuf>,
    /// Its known text.
    pub reference: Option<PathBuf>,
    /// Talks to fetch instead, each scored against `<references>/<talk id>.txt` if present.
    pub talks: Vec<TalkId>,
    /// The directory of hand-corrected references for talks.
    pub references: Option<PathBuf>,
    /// `config/asr_bench.toml`.
    pub setups_file: PathBuf,
    /// Where relative paths in it start.
    pub bench_root: PathBuf,
    /// Only these setups.
    pub only_setups: Vec<String>,
    /// Only these models.
    pub only_models: Vec<String>,
    /// Append the result to this Markdown file.
    pub record: Option<PathBuf>,
    /// A line for the record, such as the GPU and driver.
    pub note: Option<String>,
    /// Run without the glossary prompt, whatever `[asr]` says.
    pub no_prompt: bool,
    /// Run with the glossary prompt, whatever `[asr]` says.
    pub prompt: bool,
}

/// Glossary terms and teacher names from the database, for scoring.
async fn vocabulary(pool: &PgPool) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut conn = pool.acquire().await?;
    let terms: Vec<String> = wt_db::glossary::load(&mut conn)
        .await?
        .into_iter()
        .map(|t| t.canonical)
        .collect();
    let names = wt_db::registry::teacher_names(&mut conn).await?;
    Ok((terms, names))
}

/// Runs every available setup and model and prints the table and the decision.
pub async fn benchmark(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    args: BenchArgs,
) -> anyhow::Result<()> {
    let mut cfg = BenchConfig::load(&args.setups_file, &args.bench_root)?;
    if !args.only_setups.is_empty() {
        cfg.setups.retain(|s| args.only_setups.contains(&s.name));
    }
    if !args.only_models.is_empty() {
        cfg.models.retain(|m| args.only_models.contains(&m.name));
    }
    let (setups, skipped) = cfg.available();
    if setups.is_empty() {
        bail!("no setup in {} has its binary", args.setups_file.display());
    }
    let settings = stages::asr_settings(config_dir)?;
    let (terms, names) = vocabulary(pool).await?;
    let hints = AsrHints {
        glossary_prompt: if (settings.glossary_prompt || args.prompt) && !args.no_prompt {
            glossary_prompt(terms.iter().map(String::as_str))
        } else {
            String::new()
        },
        language: settings.language.clone(),
    };
    let engine = WhisperEngine {
        settings: settings.clone(),
    };
    let model_names: Vec<String> = cfg.models.iter().map(|m| m.name.clone()).collect();

    // Each input gets its own scratch directory, deleted after its runs.
    let mut inputs: Vec<(String, TalkId, Option<String>)> = Vec::new();
    match (&args.clip, args.talks.is_empty()) {
        (Some(clip), true) => {
            let reference = match &args.reference {
                Some(p) => Some(
                    std::fs::read_to_string(p)
                        .with_context(|| format!("reading {}", p.display()))?,
                ),
                None => None,
            };
            let name = clip.file_name().map_or_else(
                || clip.display().to_string(),
                |n| n.to_string_lossy().into(),
            );
            inputs.push((name, TalkId::from(sqlx::types::Uuid::new_v4()), reference));
        }
        (None, false) => {
            for talk in &args.talks {
                let reference = args
                    .references
                    .as_ref()
                    .map(|d| d.join(format!("{}.txt", talk.as_uuid())))
                    .filter(|p| p.is_file())
                    .map(std::fs::read_to_string)
                    .transpose()?;
                inputs.push((format!("talk {}", talk.as_uuid()), *talk, reference));
            }
        }
        _ => bail!("give either --clip or --talk"),
    }

    let fetcher = if args.talks.is_empty() {
        None
    } else {
        let mut conn = pool.acquire().await?;
        let exclusions = wt_ingest::http::Exclusions::load(&mut conn).await?;
        Some(
            wt_ingest::media::MediaFetcher::new(exclusions, config.allow_third_party_fetch)?
                .with_vimeo(crate::ingest::vimeo_media(config, config_dir)?)
                .with_storage(crate::ingest::storage_media(config_dir)?),
        )
    };
    let converter = Ffmpeg::default();
    let mut all = Vec::new();
    let mut described = Vec::new();
    for (name, id, reference) in &inputs {
        let scratch = ScratchDir::create(&config.scratch_dir, *id)?;
        let wav = match (&args.clip, &fetcher) {
            (Some(clip), _) => {
                let copy = scratch.file("clip-input")?;
                std::fs::copy(clip, &copy)
                    .with_context(|| format!("copying {}", clip.display()))?;
                converter.to_wav(&scratch, &copy)?
            }
            (None, Some(fetcher)) => {
                let mut conn = pool.acquire().await?;
                match wt_ingest::audio::fetch_wav(&mut conn, *id, fetcher, &converter, &scratch)
                    .await?
                {
                    Ok(Some(wav)) => wav,
                    Ok(None) => {
                        tracing::warn!(%name, "no media route; skipped");
                        continue;
                    }
                    Err(gate) => {
                        tracing::warn!(%name, %gate, "not cleared; skipped");
                        continue;
                    }
                }
            }
            (None, None) => bail!("nothing to benchmark"),
        };
        let audio_ms = wt_core::scratch::wav_duration_ms(&wav)?;
        let results = bench::run(
            &engine,
            &setups,
            &cfg.models,
            &wav,
            &scratch,
            &hints,
            reference.as_deref().map(|reference| bench::Scoring {
                reference,
                terms: &terms,
                names: &names,
            }),
        )
        .await?;
        scratch.finish()?;
        described.push(format!(
            "{name}, {:.1} minutes{}{}",
            audio_ms as f64 / 60_000.0,
            if hints.glossary_prompt.is_empty() {
                ", without the glossary prompt"
            } else {
                ", with the glossary prompt"
            },
            if reference.is_some() {
                ", scored against its known text"
            } else {
                ", no known text"
            }
        ));
        for mut r in results {
            if inputs.len() > 1 {
                r.setup = format!("{} ({name})", r.setup);
            }
            all.push(r);
        }
    }
    let decision = bench::decide(&all, &model_names, cfg.max_wer_percent);
    let table = bench::render(&all, &decision, &skipped);
    println!("{table}");
    if let Some(path) = &args.record {
        let section = bench::record_section(
            &chrono::Local::now().date_naive().to_string(),
            &bench::hardware(),
            &described.join("; "),
            args.note.as_deref(),
            &table,
        );
        let mut text = std::fs::read_to_string(path).unwrap_or_default();
        text.push_str(&section);
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        eprintln!("recorded in {}", path.display());
    }
    Ok(())
}

/// Queues retranscription for the talks transcribed with a model other than `WT_WHISPER_MODEL`.
pub async fn requeue(pool: &PgPool, config: &Config, dry_run: bool) -> anyhow::Result<()> {
    let model = config
        .whisper_model
        .as_deref()
        .map(model_name)
        .context("WT_WHISPER_MODEL must name the model talks should be transcribed with")?;
    let objects = stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    let talks = wt_ingest::audio::requeue_for_model(&mut conn, &objects, &model, dry_run).await?;
    commands::print_json(&serde_json::json!({
        "model": model,
        "talks": talks.len(),
        "queued": !dry_run,
    }))
}

#[derive(Debug, Subcommand)]
pub(crate) enum AsrCommand {
    /// Runs every available setup and model on a clip, or on talks, and prints the real-time
    /// factor, peak memory, word error rate and error rates on glossary terms and teacher names,
    /// then the setup Spec 04's decision rule picks.
    Benchmark(Box<BenchmarkArgs>),
    /// Queues retranscription for every talk transcribed with a model other than
    /// WT_WHISPER_MODEL's.
    Requeue {
        /// Count them without queueing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, clap::Args)]
// `[asr]` in the help text names the config.toml table, not an intra-doc link.
#[allow(rustdoc::broken_intra_doc_links)]
pub(crate) struct BenchmarkArgs {
    /// A clip with known text: a public-domain or CC0 recording, never a teacher's talk.
    #[arg(long, conflicts_with = "talk")]
    clip: Option<PathBuf>,
    /// The clip's known text.
    #[arg(long, requires = "clip")]
    reference: Option<PathBuf>,
    /// Cleared talks to fetch instead. Repeat for several.
    #[arg(long)]
    talk: Vec<TalkId>,
    /// Hand-corrected text for talks, as <dir>/<talk id>.txt. Keep it under data/.
    #[arg(long, requires = "talk")]
    references: Option<PathBuf>,
    /// The setups and models.
    #[arg(long, default_value = "config/asr_bench.toml")]
    setups: PathBuf,
    /// Where the file's relative paths start.
    #[arg(long, default_value = ".")]
    bench_root: PathBuf,
    /// Only this setup. Repeat for several.
    #[arg(long = "setup")]
    only_setups: Vec<String>,
    /// Only this model. Repeat for several.
    #[arg(long = "model")]
    only_models: Vec<String>,
    /// Append the result, the date and the hardware to this file.
    #[arg(long, num_args = 0..=1, default_missing_value = "docs/asr-benchmark.md")]
    record: Option<PathBuf>,
    /// A line for the record, such as the GPU and its driver.
    #[arg(long)]
    note: Option<String>,
    /// Run without the glossary prompt, whatever [asr] glossary_prompt says.
    #[arg(long, conflicts_with = "prompt")]
    no_prompt: bool,
    /// Run with the glossary prompt, whatever [asr] glossary_prompt says.
    #[arg(long)]
    prompt: bool,
    /// The directory holding pipeline.toml.
    #[arg(long, default_value = "config")]
    config_dir: PathBuf,
}

/// `wt asr`: the benchmark and the requeue after a model change (Spec 04).
pub(crate) async fn run(ctx: &Ctx, command: AsrCommand) -> anyhow::Result<()> {
    match command {
        AsrCommand::Benchmark(args) => {
            let BenchmarkArgs {
                clip,
                reference,
                talk,
                references,
                setups,
                bench_root,
                only_setups,
                only_models,
                record,
                note,
                no_prompt,
                prompt,
                config_dir,
            } = *args;
            benchmark(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                BenchArgs {
                    clip,
                    reference,
                    talks: talk,
                    references,
                    setups_file: setups,
                    bench_root,
                    only_setups,
                    only_models,
                    record,
                    note,
                    no_prompt,
                    prompt,
                },
            )
            .await?;
        }
        AsrCommand::Requeue { dry_run } => {
            requeue(&ctx.pool().await?, &ctx.config, dry_run).await?;
        }
    }
    Ok(())
}
