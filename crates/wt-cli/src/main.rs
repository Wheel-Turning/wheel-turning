//! The wt binary: commands and the worker.

// clap prints the commands' doc comments as `--help` text, where `<dir>` is a placeholder and not
// an HTML tag. Rustdoc would read it as one.
#![allow(rustdoc::invalid_html_tags)]

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use wt_core::Config;
use wt_core::telemetry::{LogOutput, init_logging};

mod admin_cmds;
mod alerts_cmds;
mod asr_cmds;
mod audit_outside;
mod commands;
mod eval_cmds;
mod index_cmds;
mod ingest;
mod objects_cmds;
mod pipeline_cmds;
mod rights;
mod rights_cmds;
mod sensitive_cmds;
mod source;
mod stages;
mod talk_cmds;
mod text_cmds;

/// The command line: one command, and where records are stored.
#[derive(Debug, Parser)]
#[command(name = "wt", version, about = "Wheel Turning operations")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Where statements, drafts and mandate decisions are stored, by SHA-256, while
    /// WT_RECORDS_BUCKET is unset.
    #[arg(long, global = true, default_value = "data/records")]
    records_dir: PathBuf,
}

/// Every `wt` command, one variant per group; each group's subcommands and handler live in its
/// module.
#[derive(Debug, Subcommand)]
enum Command {
    /// Alerts to the owner through WT_ALERT_TOPIC_ARN (Spec 11, PB4.2).
    Alerts {
        #[command(subcommand)]
        command: alerts_cmds::AlertsCommand,
    },
    /// Database operations.
    Db {
        #[command(subcommand)]
        command: admin_cmds::DbCommand,
    },
    /// The teacher, alias and sangha registry (Spec 03).
    Registry {
        #[command(subcommand)]
        command: admin_cmds::RegistryCommand,
    },
    /// Sanghas' donation links: list them, and record that a person checked one (Spec 02,
    /// Donation links and verification).
    Sangha {
        #[command(subcommand)]
        command: admin_cmds::SanghaCommand,
    },
    /// Aggregate reports, run as the analyst role through DATABASE_URL_ANALYST (Spec 06,
    /// Logging, rule 6). None of them can read an unflagged question.
    Report {
        #[command(subcommand)]
        command: admin_cmds::ReportCommand,
    },
    /// Sensitive questions (Spec 05, Finding problem areas): red-team the detector, apply the
    /// rows the owner confirms, and report counts as the analyst role.
    Sensitive {
        #[command(subcommand)]
        command: sensitive_cmds::SensitiveCommand,
    },
    /// Speaker consent (Spec 03, the consent gate).
    Consent {
        #[command(subcommand)]
        command: admin_cmds::ConsentCommand,
    },
    /// Rights holders and hand exclusions for talks (Spec 03, rule 5).
    Talk {
        #[command(subcommand)]
        command: talk_cmds::TalkCommand,
    },
    /// Testers and their invite links (Spec 06, Invite token access).
    Tester {
        #[command(subcommand)]
        command: admin_cmds::TesterCommand,
    },
    /// Accessibility problem reports from `/accessibility` (Spec 06, Accessibility standard).
    Accessibility {
        #[command(subcommand)]
        command: admin_cmds::AccessibilityCommand,
    },
    /// Remove a teacher's or a talk's text from every store (Spec 07). Prints the counts as
    /// JSON. A real run needs --yes.
    Takedown(talk_cmds::TakedownArgs),
    /// Object storage: the one-time copy into the transcripts bucket (Spec 11, PB1.2).
    Objects {
        #[command(subcommand)]
        command: objects_cmds::ObjectsCommand,
    },
    /// Rights-ledger records: the one-time copy into the records bucket (Spec 11, PB1.2).
    Records {
        #[command(subcommand)]
        command: objects_cmds::RecordsCommand,
    },
    /// Run the integrity checks (Spec 07). Prints a JSON report and exits 1 when a check marked
    /// fail has findings, or any check with --fail-on warning.
    Audit(commands::AuditArgs),
    /// Discovery from the configured sources (Spec 03).
    Ingest {
        #[command(subcommand)]
        command: pipeline_cmds::IngestCommand,
    },
    /// Run queued jobs (Spec 03, Job queue).
    Worker(pipeline_cmds::WorkerArgs),
    /// Sources and their settings (Spec 03, Seattle Soto Zen sources; Spec 08, Mandated
    /// sources).
    Source {
        #[command(subcommand)]
        command: rights_cmds::SourceCommand,
    },
    /// The rights ledger: permission requests and their answers (Spec 08, Rights lifecycle).
    Rights {
        #[command(subcommand)]
        command: rights_cmds::RightsCommand,
    },
    /// Permanent exclusions (Spec 08, Rights lifecycle).
    Exclusion {
        #[command(subcommand)]
        command: rights_cmds::ExclusionCommand,
    },
    /// Speech recognition: the benchmark and model changes (Spec 04).
    Asr {
        #[command(subcommand)]
        command: asr_cmds::AsrCommand,
    },
    /// Moves cleared talks into the pipeline and retries failed stages (Spec 03).
    Pipeline {
        #[command(subcommand)]
        command: pipeline_cmds::PipelineCommand,
    },
    /// Foundational texts (Spec 08).
    Text {
        #[command(subcommand)]
        command: text_cmds::TextCommand,
    },
    /// A walkthrough that builds manifests for folders in data-inbox/texts/: work, edition,
    /// license evidence approved by your typed name, unit scheme, dry run and apply (Spec 08,
    /// Guided loading).
    Intake {
        /// The inbox.
        #[arg(long, default_value = "data-inbox")]
        inbox: PathBuf,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// License evidence: capture a license page, record a permission, approve either (Spec 03,
    /// Licensed sources; Spec 08, Ingesting from URLs).
    License {
        #[command(subcommand)]
        command: text_cmds::LicenseCommand,
    },
    /// Reference assets: glossaries, alias lists, lineage and locator lists (Spec 08).
    Reference {
        #[command(subcommand)]
        command: text_cmds::ReferenceCommand,
    },
    /// Offline transcript review (Spec 04, Transcript review and owner exports).
    Transcript {
        #[command(subcommand)]
        command: pipeline_cmds::TranscriptCommand,
    },
    /// Reruns the index step from stored segments, with a new glossary or embedding model
    /// (Spec 04).
    Reindex(pipeline_cmds::ReindexArgs),
    /// The evaluation harness (Spec 07): load, run, export-review, import-ratings, report,
    /// compare.
    Eval {
        #[command(subcommand)]
        command: eval_cmds::EvalCommand,
    },
    /// Exports for the evaluation notebooks (Spec 07).
    Export {
        #[command(subcommand)]
        command: pipeline_cmds::ExportCommand,
    },
    /// Segmentation tools (Spec 04).
    Segment {
        #[command(subcommand)]
        command: pipeline_cmds::SegmentCommand,
    },
    /// The glossary (Spec 04, Glossary correction).
    Glossary {
        #[command(subcommand)]
        command: pipeline_cmds::GlossaryCommand,
    },
    /// The language-model spend cap (Spec 04).
    Llm {
        #[command(subcommand)]
        command: pipeline_cmds::LlmCommand,
    },
    /// A teacher's standing (Spec 03, Mandated talks).
    Teacher {
        #[command(subcommand)]
        command: talk_cmds::TeacherCommand,
    },
}

fn main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    // The dispatch future holds every command's state, more than the 1 MB main-thread stack a
    // Windows debug build gets, so it lives on the heap, and polling it runs on a thread with
    // room for the dispatch's frames.
    std::thread::Builder::new()
        .name("wt".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("starting the async runtime")?
                .block_on(Box::pin(run(cli)))
        })
        .context("starting the main thread")?
        .join()
        .map_err(|_| anyhow::anyhow!("the main thread panicked"))?
}

/// Loads the configuration and starts logging, which every command shares, then hands the
/// command to its group's handler.
async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let config = Config::from_env().context("loading configuration")?;
    // Reports go to stdout, so logs must not.
    init_logging(config.env, &config.log, LogOutput::Stderr)?;
    let ctx = Ctx {
        records: wt_core::records::open(config.records_bucket.as_deref(), &cli.records_dir)
            .context("opening the records store")?,
        guarded: cli.command.writes_pipeline_or_rights(),
        named: std::sync::OnceLock::new(),
        config,
    };
    match cli.command {
        Command::Alerts { command } => alerts_cmds::alerts(&ctx.config, command).await?,
        Command::Db { command } => return admin_cmds::db(&ctx, command).await,
        Command::Registry { command } => admin_cmds::registry(&ctx, command).await?,
        Command::Sangha { command } => admin_cmds::sangha(&ctx, command).await?,
        Command::Report { command } => admin_cmds::report(&ctx, command).await?,
        Command::Consent { command } => admin_cmds::consent(&ctx, command).await?,
        Command::Tester { command } => admin_cmds::tester(&ctx, command).await?,
        Command::Accessibility { command } => admin_cmds::accessibility(&ctx, command).await?,
        Command::Talk { command } => talk_cmds::talk(&ctx, command).await?,
        Command::Takedown(args) => talk_cmds::takedown(&ctx, args).await?,
        Command::Teacher { command } => talk_cmds::teacher(&ctx, command).await?,
        Command::Audit(args) => {
            return commands::audit(&ctx.pool().await?, &ctx.config, &args).await;
        }
        Command::Objects { command } => return objects_cmds::objects(&ctx, command).await,
        Command::Records { command } => return objects_cmds::records(&ctx, command).await,
        Command::Source { command } => rights_cmds::source(&ctx, command).await?,
        Command::Rights { command } => return rights_cmds::rights(&ctx, command).await,
        Command::Exclusion { command } => rights_cmds::exclusion(&ctx, command).await?,
        Command::Text { command } => text_cmds::text(&ctx, command).await?,
        Command::Intake { inbox, config_dir } => {
            let pool = ctx.pool().await?;
            index_cmds::intake(&pool, &ctx.config, &ctx.records, &config_dir, &inbox).await?;
        }
        Command::License { command } => {
            index_cmds::license(&ctx.pool().await?, &ctx.records, command).await?;
        }
        Command::Reference { command } => text_cmds::reference(&ctx, command).await?,
        Command::Ingest { command } => pipeline_cmds::ingest(&ctx, command).await?,
        Command::Worker(args) => pipeline_cmds::worker(&ctx, args).await?,
        Command::Pipeline { command } => pipeline_cmds::pipeline(&ctx, command).await?,
        Command::Reindex(args) => pipeline_cmds::reindex(&ctx, args).await?,
        Command::Transcript { command } => pipeline_cmds::transcript(&ctx, command).await?,
        Command::Export { command } => pipeline_cmds::export(&ctx, command).await?,
        Command::Segment { command } => pipeline_cmds::segment(&ctx, command).await?,
        Command::Glossary { command } => pipeline_cmds::glossary(&ctx, command).await?,
        Command::Llm { command } => pipeline_cmds::llm(&ctx, command).await?,
        Command::Asr { command } => asr_cmds::run(&ctx, command).await?,
        Command::Sensitive { command } => sensitive_cmds::run(&ctx.config, command).await?,
        Command::Eval { command } => {
            eval_cmds::eval(&ctx.pool().await?, &ctx.config, command).await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

impl Command {
    /// The commands that write pipeline or rights data (Spec 11, PB1.1). Their database must
    /// hold the name `WT_DATABASE_NAME` gives, and in prod that key is required.
    fn writes_pipeline_or_rights(&self) -> bool {
        matches!(
            self,
            Self::Worker(_)
                | Self::Pipeline { .. }
                | Self::Ingest { .. }
                | Self::Reindex(_)
                | Self::Takedown(_)
                | Self::Rights { .. }
                | Self::Exclusion { .. }
                | Self::Source { .. }
                | Self::Teacher { .. }
                | Self::Talk { .. }
                | Self::Consent { .. }
                | Self::Text { .. }
                | Self::Intake { .. }
                | Self::License { .. }
        )
    }
}

/// What every command handler shares: the configuration and the records store. The database
/// pool opens on demand, so the commands that need no database run without one.
pub(crate) struct Ctx {
    config: Config,
    records: Box<dyn wt_core::records::Records>,
    /// Whether this command checks the database's name before using it.
    guarded: bool,
    /// The database's name, once checked and printed.
    named: std::sync::OnceLock<String>,
}

impl Ctx {
    /// A new pool on `DATABASE_URL`. Each call connects afresh, as the commands always have.
    /// For the commands that write pipeline or rights data, the database's name is read,
    /// printed to stderr once, and checked against `WT_DATABASE_NAME`.
    async fn pool(&self) -> anyhow::Result<sqlx::PgPool> {
        let pool = wt_db::connect(&self.config.database_url)
            .await
            .context("connecting to the database")?;
        if self.guarded {
            let name = wt_db::identity::verify(
                &pool,
                self.config.database_name.as_deref(),
                self.config.env,
            )
            .await?;
            if self.named.set(name.clone()).is_ok() {
                eprintln!("database: {name}");
            }
        }
        Ok(pool)
    }
}
