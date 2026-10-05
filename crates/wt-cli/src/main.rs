//! The wt binary: commands and the worker.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use wt_core::Config;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_core::telemetry::{LogOutput, init_logging};

mod commands;
mod ingest;

#[derive(Debug, Parser)]
#[command(name = "wt", version, about = "Wheel Turning operations")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Database operations.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    /// The teacher, alias and sangha registry (Spec 03).
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },
    /// Speaker consent (Spec 03, the consent gate).
    Consent {
        #[command(subcommand)]
        command: ConsentCommand,
    },
    /// Rights holders and hand exclusions for talks (Spec 03, rule 5).
    Talk {
        #[command(subcommand)]
        command: TalkCommand,
    },
    /// Remove a teacher's or a talk's text from every store (Spec 07). Prints the counts as
    /// JSON. A real run needs --yes.
    Takedown(TakedownArgs),
    /// Run the integrity checks (Spec 07). Prints a JSON report and exits 1 when a check marked
    /// fail has findings.
    Audit,
    /// Discovery from the configured sources (Spec 03).
    Ingest {
        #[command(subcommand)]
        command: IngestCommand,
    },
    /// Run queued jobs (Spec 03, Job queue).
    Worker(WorkerArgs),
}

#[derive(Debug, Subcommand)]
enum IngestCommand {
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
}

#[derive(Debug, Args)]
struct WorkerArgs {
    /// Job kinds to run, comma separated. This build runs classify.
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

fn parse_kind(value: &str) -> Result<wt_db::job::JobKind, String> {
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
enum DbCommand {
    /// Apply every pending migration. Safe to run repeatedly.
    Migrate,
    /// Create or update a login role that inherits one group role. Safe to run repeatedly.
    CreateLogin {
        /// Group role to inherit: wt_owner, wt_pipeline, wt_web or wt_analyst.
        #[arg(long)]
        role: String,
        /// Login role name. Defaults to <role>_login.
        #[arg(long)]
        login: Option<String>,
        /// Environment variable holding the password, for example one ECS injects from Secrets
        /// Manager. The password is never accepted as an argument.
        #[arg(long)]
        password_env: String,
    },
}

#[derive(Debug, Subcommand)]
enum RegistryCommand {
    /// Load sanghas, teachers and aliases from a registry file. Safe to run repeatedly.
    Import {
        /// The registry TOML, for example config/registry.toml.
        #[arg(long)]
        file: PathBuf,
        /// Who is making the change, for correction_log.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
}

#[derive(Debug, Subcommand)]
enum ConsentCommand {
    /// Record a teacher's consent and apply it to their talks. declined and withdrawn run the
    /// teacher takedown, which needs --yes.
    Set {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// granted, requested, declined, withdrawn or unknown.
        #[arg(long, value_parser = parse_status)]
        status: ConsentStatus,
        /// Pointer to the stored reply, never the reply text.
        #[arg(long)]
        evidence: Option<String>,
        /// Limits the speaker set, in their words.
        #[arg(long)]
        scope_note: Option<String>,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Confirm the takedown a decline or withdrawal runs.
        #[arg(long)]
        yes: bool,
    },
    /// Every teacher with their latest consent and talk counts by state.
    List,
}

#[derive(Debug, Subcommand)]
enum TalkCommand {
    /// Set the rights holder for a talk or every talk of a teacher, and clear any talk that
    /// now passes the gate.
    SetRights {
        #[command(flatten)]
        target: TalkOrTeacher,
        /// The holder's name, or `speaker` for the teacher's display name.
        #[arg(long)]
        holder: String,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Hold one talk out, which is how a speaker's limits apply. Runs a single-talk takedown,
    /// which needs --yes.
    Exclude {
        /// The talk id.
        #[arg(long)]
        talk: TalkId,
        /// Why, recorded as the talk's state reason.
        #[arg(long)]
        reason: String,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Confirm the takedown.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct TalkOrTeacher {
    /// One talk, by id.
    #[arg(long)]
    talk: Option<TalkId>,
    /// Every talk of a teacher, by display name or alias.
    #[arg(long)]
    teacher: Option<String>,
}

#[derive(Debug, Args)]
struct TakedownArgs {
    #[command(flatten)]
    target: TalkOrTeacher,
    /// Who asked for it or ran it, for takedown_log.
    #[arg(long, visible_alias = "by")]
    actor: String,
    /// Print the counts and change nothing.
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Confirm a real run.
    #[arg(long)]
    yes: bool,
}

fn parse_status(value: &str) -> Result<ConsentStatus, String> {
    ConsentStatus::parse(value).ok_or_else(|| {
        format!("{value:?} is not one of granted, requested, declined, withdrawn, unknown")
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    let config = Config::from_env().context("loading configuration")?;
    // Reports go to stdout, so logs must not.
    init_logging(config.env, &config.log, LogOutput::Stderr)?;
    let pool = || async {
        wt_db::connect(&config.database_url)
            .await
            .context("connecting to the database")
    };

    match cli.command {
        Command::Db {
            command: DbCommand::Migrate,
        } => {
            let report = wt_db::migrate(&pool().await?)
                .await
                .context("applying migrations")?;
            tracing::info!(
                applied = report.applied,
                known = report.known,
                "migrations up to date"
            );
        }
        Command::Db {
            command:
                DbCommand::CreateLogin {
                    role,
                    login,
                    password_env,
                },
        } => {
            let login = login.unwrap_or_else(|| format!("{role}_login"));
            let password = std::env::var(&password_env)
                .ok()
                .filter(|p| !p.is_empty())
                .map(wt_core::Secret::new)
                .with_context(|| format!("{password_env} is not set"))?;
            let outcome = wt_db::create_login(&pool().await?, &role, &login, &password)
                .await
                .with_context(|| format!("creating login {login}"))?;
            tracing::info!(login = %login, role = %role, outcome = ?outcome, "login ready");
        }
        Command::Registry {
            command: RegistryCommand::Import { file, by },
        } => commands::registry_import(&pool().await?, &file, &by).await?,
        Command::Consent {
            command:
                ConsentCommand::Set {
                    teacher,
                    status,
                    evidence,
                    scope_note,
                    by,
                    yes,
                },
        } => {
            commands::consent_set(
                &pool().await?,
                &teacher,
                status,
                evidence.as_deref(),
                scope_note.as_deref(),
                &by,
                yes,
            )
            .await?;
        }
        Command::Consent {
            command: ConsentCommand::List,
        } => commands::consent_list(&pool().await?).await?,
        Command::Talk {
            command: TalkCommand::SetRights { target, holder, by },
        } => commands::set_rights(&pool().await?, target.into(), &holder, &by).await?,
        Command::Talk {
            command:
                TalkCommand::Exclude {
                    talk,
                    reason,
                    by,
                    yes,
                },
        } => commands::exclude(&pool().await?, talk, &reason, &by, yes).await?,
        Command::Takedown(args) => {
            commands::takedown(
                &pool().await?,
                args.target.into(),
                &args.actor,
                args.dry_run,
                args.yes,
            )
            .await?;
        }
        Command::Audit => return commands::audit(&pool().await?, config.env).await,
        Command::Ingest {
            command: IngestCommand::Discover { source, config_dir },
        } => ingest::discover(&pool().await?, &config, &config_dir, source.as_deref()).await?,
        Command::Worker(args) => {
            ingest::worker(
                &pool().await?,
                &args.config_dir,
                &args.kinds,
                args.concurrency,
                args.drain,
            )
            .await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Exactly one of the two, which clap's argument group guarantees.
enum Target {
    Talk(TalkId),
    Teacher(String),
}

impl From<TalkOrTeacher> for Target {
    fn from(args: TalkOrTeacher) -> Self {
        match args.talk {
            Some(talk) => Target::Talk(talk),
            // The group makes --teacher present whenever --talk is absent.
            None => Target::Teacher(args.teacher.unwrap_or_default()),
        }
    }
}
