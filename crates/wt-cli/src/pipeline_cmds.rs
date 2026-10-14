//! `wt ingest`, `wt worker`, `wt pipeline`, `wt reindex`, `wt transcript`, `wt export`,
//! `wt segment`, `wt glossary` and `wt llm`: the commands that move talks through the
//! pipeline and look at its output (Spec 03, Spec 04, Spec 07).

use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Subcommand};
use wt_core::ids::TalkId;

use wt_core::Config;

use crate::{Ctx, commands, index_cmds, ingest, stages};

#[derive(Debug, Subcommand)]
pub(crate) enum IngestCommand {
    /// List each source, or the named one, record new and changed videos and queue them for
    /// classification. Prints counts per source as JSON. Lists metadata only; fetches no media.
    Discover {
        /// Only this source from sources.toml.
        #[arg(long)]
        source: Option<String>,
        /// The directory holding sources.toml, pipeline.toml and series.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Queue classification again after pipeline.toml, series.toml or the registry's aliases
    /// change: for talks before the gate, and talks classification itself excluded (reopened
    /// when their job runs). Unchanged rules queue nothing. Prints counts as JSON; `wt worker`
    /// runs the jobs. Cleared, pipeline and withdrawn talks are never touched.
    Reclassify {
        /// Only this talk.
        #[arg(long)]
        talk: Option<TalkId>,
        /// Count what would be queued, and queue nothing.
        #[arg(long)]
        dry_run: bool,
        /// The directory holding pipeline.toml and series.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Compare the Vimeo account list with the talks page and with what discovery recorded,
    /// and write the completeness page. Without the source's Vimeo token (its token_env, such
    /// as WT_SSZ_VIMEO_ACCESS_TOKEN) the account list comes
    /// from account pages saved in a browser (--vimeo-list); the Vimeo site is never fetched.
    /// Prints counts and the ids on the talks page but not in the account list as JSON.
    Completeness {
        /// The Vimeo source in sources.toml.
        #[arg(long, default_value = "ssz-vimeo")]
        source: String,
        /// A saved Vimeo account videos page. Repeat for several pages.
        #[arg(long = "vimeo-list")]
        vimeo_list: Vec<PathBuf>,
        /// Where to write the HTML page.
        #[arg(long)]
        out: PathBuf,
        /// The directory holding sources.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Pair the files under a storage source's root (Spec 03, the storage adapter) with the
    /// Vimeo talks of its sangha by Vimeo id, normalized title, recording date and length, and
    /// write the review table, <out>/<source>.csv and .html: proposed matches, conflicts,
    /// unmatched files and unmatched talks. Changes nothing unless --apply records every
    /// proposed match, or --sheet records the rows marked yes in a reviewed copy of the CSV.
    /// Reads each file's header with ffprobe and no audio. Prints counts as JSON.
    MatchStorage {
        /// The storage source in sources.toml.
        #[arg(long, default_value = "ssz-storage")]
        source: String,
        /// Where to write the review table.
        #[arg(long, default_value = "data/storage-match")]
        out: PathBuf,
        /// Record every proposed match.
        #[arg(long, conflicts_with = "sheet")]
        apply: bool,
        /// A reviewed review table: record its rows marked yes.
        #[arg(long)]
        sheet: Option<PathBuf>,
        /// The directory holding sources.toml, pipeline.toml and series.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Write the review page: duplicate clusters, exclusions, review holds, series coverage,
    /// videos by month and donation-looking links. Reads only; it changes nothing.
    Report {
        /// Where to write the HTML page.
        #[arg(long)]
        out: PathBuf,
        /// The directory holding pipeline.toml and series.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
}

#[derive(Debug, Args)]
pub(crate) struct WorkerArgs {
    /// Job kinds to run, comma separated: classify, audio, correct, extract, resolve, index.
    #[arg(long, value_delimiter = ',', value_parser = parse_kind, default_value = "classify")]
    kinds: Vec<wt_db::job::JobKind>,
    /// Workers in this process.
    #[arg(long, default_value_t = 1)]
    concurrency: usize,
    /// Exit once no job is due instead of waiting for more.
    #[arg(long)]
    drain: bool,
    /// The directory holding sources.toml, pipeline.toml and series.toml.
    #[arg(long, default_value = "config")]
    config_dir: PathBuf,
}

pub(crate) fn parse_kind(value: &str) -> Result<wt_db::job::JobKind, String> {
    wt_db::job::JobKind::parse(value).ok_or_else(|| {
        let known: Vec<_> = wt_db::job::JobKind::ALL
            .iter()
            .map(|k| k.as_str())
            .collect();
        format!(
            "unknown job kind {value:?}; known kinds are {}",
            known.join(", ")
        )
    })
}

#[derive(Debug, Subcommand)]
pub(crate) enum PipelineCommand {
    /// Queues an audio job for every talk at cleared.
    Advance,
    /// Requeues a talk's dead job with fresh attempts.
    Retry {
        /// The talk.
        #[arg(long)]
        talk: TalkId,
    },
    /// Returns talks the audio job held for these reasons to cleared, each with a new audio
    /// job, such as `no_media` and `alignment_failed` for the text-only path (Spec 04).
    RetryHeld {
        /// A hold reason. Repeat for several.
        #[arg(long, required = true)]
        reason: Vec<String>,
    },
    /// Queues extraction again for talks whose citations were staged as skipped because no
    /// model could see them, once one may (Spec 04). Needs `ANTHROPIC_API_KEY`, the extraction
    /// model and `WT_LLM_REAL_DATA_OK=true`.
    RetryExtract {
        /// One talk instead of every skipped one.
        #[arg(long)]
        talk: Option<TalkId>,
        /// Count the skipped talks without queueing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Recovers the student questions of processed talks whose transcripts mark a student gap
    /// (Spec 04, Student questions): text-only talks get a `questions` job, which reruns their
    /// stored source text with no fetch; talks with recordings get an `audio` job, which fetches
    /// and transcribes again. Prints the count of each kind first.
    StudentQuestions {
        /// One talk.
        #[arg(long)]
        talk: Option<TalkId>,
        /// Only talks discovered from this source key.
        #[arg(long)]
        source: Option<String>,
        /// Count only; queue nothing.
        #[arg(long)]
        dry_run: bool,
        /// Queue only the text-only talks; talks with recordings wait for a GPU run.
        #[arg(long)]
        text_only: bool,
        /// Queue talks whose earlier recovery already ran, once more.
        #[arg(long)]
        again: bool,
    },
}

#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("which").required(true).args(["talk", "all", "citations"])))]
pub(crate) struct ReindexArgs {
    /// A talk to reindex. Repeat for several.
    #[arg(long)]
    talk: Vec<TalkId>,
    /// Every talk with corrected segments.
    #[arg(long)]
    all: bool,
    /// Relink citations to text units after a text or locator import, and queue resolution
    /// again for talks with staged citations when the hand list changed (Spec 08).
    #[arg(long)]
    citations: bool,
    /// The embedding model directory under WT_MODEL_DIR, instead of WT_EMBED_MODEL.
    #[arg(long)]
    model: Option<String>,
    /// The directory holding pipeline.toml.
    #[arg(long, default_value = "config")]
    config_dir: PathBuf,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TranscriptCommand {
    /// Writes one review file per talk: an HTML page with the corrected text paragraph by
    /// paragraph, each editable, and a "Listen at" link to that moment of the recording. Saving
    /// in the page downloads the corrections as JSON.
    ExportReview {
        /// One talk.
        #[arg(long, conflicts_with = "teacher", required_unless_present = "teacher")]
        talk: Option<TalkId>,
        /// Every transcribed talk of this teacher.
        #[arg(long)]
        teacher: Option<String>,
        /// The directory to write into. Keep it under data/: the files hold teachers' words.
        #[arg(long, default_value = "data/review")]
        out: PathBuf,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Applies a corrections file as a reviewed transcript version, which no automated pass
    /// replaces, and queues the talk to be indexed again.
    ImportCorrections {
        /// The JSON the review page saved.
        #[arg(long)]
        file: PathBuf,
        /// Who reviewed it.
        #[arg(long)]
        reviewer: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ExportCommand {
    /// Writes a teacher's talks for their own use, such as books (Spec 04, Owner exports): one
    /// Markdown file per talk with the teacher's words in paragraphs and the removed stretches
    /// marked, whatever the talk's display level. No student speech, no audio.
    Teacher {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// The directory to write into. Keep it under data/, which git ignores.
        #[arg(long, default_value = "data/owner-export")]
        out: PathBuf,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Writes passages.jsonl: one line per servable passage with its id, opaque talk and
    /// teacher ids, start time and text. No names, titles or audio.
    Transcripts {
        /// Output format.
        #[arg(long, value_parser = ["jsonl"], default_value = "jsonl")]
        format: String,
        /// The directory to write into. Keep it under data/, which git ignores.
        #[arg(long, default_value = "data/export")]
        out: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SegmentCommand {
    /// Runs fetch, recognition, diarization and segmentation on cleared talks without storing
    /// anything, and writes an HTML timeline per talk.
    Sample {
        /// How many cleared talks to pick at random.
        #[arg(long, default_value_t = 10)]
        count: i64,
        /// Specific talks instead of a random pick. Repeat for several.
        #[arg(long)]
        talk: Vec<TalkId>,
        /// The report file. Keep it under data/: it holds teachers' words.
        #[arg(long, default_value = "data/segment-sample.html")]
        out: PathBuf,
        /// The directory holding pipeline.toml and series.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum GlossaryCommand {
    /// Load terms from a glossary file, such as scripts/seed_glossary.toml. Variants merge into
    /// existing terms; a conflicting spelling or kind is reported and left as stored. Then
    /// queues correction for exactly the talks the change affects.
    Import {
        /// The glossary TOML.
        #[arg(long)]
        file: PathBuf,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Add one term, or variants to an existing term, then queue correction for the talks it
    /// affects.
    Add {
        /// The canonical spelling.
        #[arg(long)]
        canonical: String,
        /// term, name or text_title.
        #[arg(long, value_parser = ["term", "name", "text_title"], default_value = "term")]
        kind: String,
        /// A known variant. Repeat for several.
        #[arg(long = "variant")]
        variants: Vec<String>,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
}

/// Applies a glossary change and queues correction for exactly the talks it affects.
pub(crate) async fn glossary_change(
    pool: &sqlx::PgPool,
    config: &Config,
    terms: &[wt_registry::glossary::GlossaryTerm],
    by: &str,
) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    let report = wt_db::glossary::import(&mut conn, terms, by).await?;
    let glossary = wt_registry::glossary::Glossary::new(&wt_db::glossary::load(&mut conn).await?)?;
    let objects = stages::objects(config)?;
    let requeued =
        wt_ingest::textjobs::requeue_after_glossary_change(&mut conn, &objects, &glossary).await?;
    commands::print_json(&serde_json::json!({
        "added": report.added,
        "variants_added": report.variants_added,
        "conflicts": report.conflicts,
        "glossary_version": glossary.version(),
        "talks_requeued": requeued.len(),
    }))
}

#[derive(Debug, Subcommand)]
pub(crate) enum LlmCommand {
    /// Inspect or change the monthly cap.
    Cap {
        #[command(subcommand)]
        command: CapCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum CapCommand {
    /// Set the monthly cap in US dollars, and release ingestion jobs the cap had put off.
    Set {
        /// The cap.
        usd: f64,
        /// Who is changing it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Print the cap and the spend so far this month.
    Show,
}

/// `wt ingest`: discovery and the reports around it (Spec 03).
pub(crate) async fn ingest(ctx: &Ctx, command: IngestCommand) -> anyhow::Result<()> {
    match command {
        IngestCommand::Discover { source, config_dir } => {
            ingest::discover(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                source.as_deref(),
            )
            .await?
        }
        IngestCommand::Report { out, config_dir } => {
            ingest::report(&ctx.pool().await?, &config_dir, &out).await?
        }
        IngestCommand::MatchStorage {
            source,
            out,
            apply,
            sheet,
            config_dir,
        } => {
            ingest::match_storage(
                &ctx.pool().await?,
                &config_dir,
                &source,
                &out,
                apply,
                sheet.as_deref(),
            )
            .await?;
        }
        IngestCommand::Completeness {
            source,
            vimeo_list,
            out,
            config_dir,
        } => {
            ingest::completeness(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                &source,
                &vimeo_list,
                &out,
            )
            .await?;
        }
        IngestCommand::Reclassify {
            talk,
            dry_run,
            config_dir,
        } => ingest::reclassify(&ctx.pool().await?, &config_dir, talk, dry_run).await?,
    }
    Ok(())
}

/// `wt pipeline`: moves cleared talks in and requeues failed or skipped stages (Spec 03).
pub(crate) async fn pipeline(ctx: &Ctx, command: PipelineCommand) -> anyhow::Result<()> {
    let pool = ctx.pool().await?;
    let mut conn = pool.acquire().await?;
    match command {
        PipelineCommand::Advance => {
            let queued = wt_ingest::audio::advance_cleared(&mut conn).await?;
            commands::print_json(&serde_json::json!({ "audio_jobs_queued": queued.len() }))?;
        }
        PipelineCommand::Retry { talk } => match wt_db::job::retry_dead(&mut conn, talk).await? {
            Some(kind) => commands::print_json(
                &serde_json::json!({ "talk": talk, "requeued": kind.as_str() }),
            )?,
            None => anyhow::bail!("talk {talk} has no dead job to retry"),
        },
        PipelineCommand::StudentQuestions {
            talk,
            source,
            dry_run,
            text_only,
            again,
        } => {
            let report = wt_ingest::audio::queue_student_questions(
                &mut conn,
                talk,
                source.as_deref(),
                dry_run,
                text_only,
                again,
            )
            .await?;
            commands::print_json(&report)?;
        }
        PipelineCommand::RetryHeld { reason } => {
            let queued =
                wt_ingest::audio::retry_held(&mut conn, &reason, "wt pipeline retry-held").await?;
            commands::print_json(&serde_json::json!({ "audio_jobs_queued": queued.len() }))?;
        }
        PipelineCommand::RetryExtract { talk, dry_run } => {
            let model = ctx
                .config
                .llm_model_for(wt_core::config::LlmStage::Extract)?;
            if !dry_run && (ctx.config.anthropic_api_key.is_none() || !ctx.config.llm_real_data_ok)
            {
                anyhow::bail!(
                    "retry-extract needs ANTHROPIC_API_KEY and WT_LLM_REAL_DATA_OK=true,                              or the extraction would be skipped again"
                );
            }
            let objects = stages::objects(&ctx.config)?;
            let talks = wt_ingest::textjobs::retry_skipped_extracts(
                &mut conn, &objects, talk, model, dry_run,
            )
            .await?;
            commands::print_json(&serde_json::json!({
                "skipped_talks": talks.len(),
                "extract_jobs_queued": if dry_run { 0 } else { talks.len() },
                "dry_run": dry_run,
            }))?;
        }
    }
    Ok(())
}

/// `wt transcript`: the offline review export and its corrections (Spec 04).
pub(crate) async fn transcript(ctx: &Ctx, command: TranscriptCommand) -> anyhow::Result<()> {
    match command {
        TranscriptCommand::ExportReview {
            talk,
            teacher,
            out,
            config_dir,
        } => {
            let report = index_cmds::export_review(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                talk,
                teacher,
                &out,
            )
            .await?;
            commands::print_json(&report)?;
        }
        TranscriptCommand::ImportCorrections { file, reviewer } => {
            let report =
                index_cmds::import_corrections(&ctx.pool().await?, &ctx.config, &file, &reviewer)
                    .await?;
            commands::print_json(&report)?;
        }
    }
    Ok(())
}

/// `wt export`: the evaluation export and a teacher's own export (Spec 07).
pub(crate) async fn export(ctx: &Ctx, command: ExportCommand) -> anyhow::Result<()> {
    match command {
        ExportCommand::Transcripts { format: _, out } => {
            index_cmds::export_transcripts(&ctx.pool().await?, &out).await?;
        }
        ExportCommand::Teacher {
            teacher,
            out,
            config_dir,
        } => {
            let report = index_cmds::export_teacher(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                &teacher,
                &out,
            )
            .await?;
            commands::print_json(&report)?;
        }
    }
    Ok(())
}

/// `wt segment`: segmentation samples for review (Spec 04).
pub(crate) async fn segment(ctx: &Ctx, command: SegmentCommand) -> anyhow::Result<()> {
    match command {
        SegmentCommand::Sample {
            count,
            talk,
            out,
            config_dir,
        } => {
            let talks = if talk.is_empty() { None } else { Some(talk) };
            index_cmds::segment_sample(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                count,
                talks,
                &out,
            )
            .await?;
        }
    }
    Ok(())
}

/// `wt glossary`: glossary changes, each of which requeues correction (Spec 04).
pub(crate) async fn glossary(ctx: &Ctx, command: GlossaryCommand) -> anyhow::Result<()> {
    match command {
        GlossaryCommand::Import { file, by } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let parsed =
                wt_registry::glossary::GlossaryFile::parse(&text, &file.display().to_string())?;
            glossary_change(&ctx.pool().await?, &ctx.config, &parsed.terms, &by).await?;
        }
        GlossaryCommand::Add {
            canonical,
            kind,
            variants,
            by,
        } => {
            let kind = match kind.as_str() {
                "name" => wt_registry::glossary::TermKind::Name,
                "text_title" => wt_registry::glossary::TermKind::TextTitle,
                _ => wt_registry::glossary::TermKind::Term,
            };
            let term = wt_registry::glossary::GlossaryTerm {
                canonical,
                kind,
                variants,
            };
            glossary_change(&ctx.pool().await?, &ctx.config, &[term], &by).await?;
        }
    }
    Ok(())
}

/// `wt llm cap`: shows or sets the monthly language-model cap (Spec 04).
pub(crate) async fn llm(ctx: &Ctx, command: LlmCommand) -> anyhow::Result<()> {
    match command {
        LlmCommand::Cap { command } => {
            let pool = ctx.pool().await?;
            let mut conn = pool.acquire().await?;
            let now = chrono::Utc::now();
            if let CapCommand::Set { usd, by } = &command {
                if !(usd.is_finite() && *usd >= 0.0) {
                    anyhow::bail!("the cap must be a dollar amount of zero or more");
                }
                let released = wt_db::llm::set_cap(&mut conn, *usd, by).await?;
                tracing::info!(cap = usd, released, "language-model cap set");
            }
            let cap = wt_db::llm::cap(&mut conn, ctx.config.llm_monthly_cap_usd).await?;
            let spent = wt_db::llm::month_to_date(&mut conn, now).await?;
            commands::print_json(&serde_json::json!({
                "cap_usd": cap,
                "month_to_date_usd": (spent * 10_000.0).round() / 10_000.0,
                "month": wt_db::llm::month_of(now).to_string(),
            }))?;
        }
    }
    Ok(())
}

/// `wt reindex`: reruns the index step from stored segments (Spec 04), or relinks citations to
/// text units after a text or locator import (Spec 08).
pub(crate) async fn reindex(ctx: &Ctx, args: ReindexArgs) -> anyhow::Result<()> {
    if args.citations {
        let report =
            index_cmds::reindex_citations(&ctx.pool().await?, &ctx.config, &args.config_dir)
                .await?;
        return commands::print_json(&report);
    }
    let talks = if args.all { None } else { Some(args.talk) };
    index_cmds::reindex(
        &ctx.pool().await?,
        &ctx.config,
        &args.config_dir,
        talks,
        args.model,
    )
    .await
}

/// `wt worker`: runs queued jobs of the given kinds (Spec 03, Job queue).
pub(crate) async fn worker(ctx: &Ctx, args: WorkerArgs) -> anyhow::Result<()> {
    ingest::worker(
        &ctx.pool().await?,
        &ctx.config,
        &args.config_dir,
        &args.kinds,
        args.concurrency,
        args.drain,
    )
    .await
}
