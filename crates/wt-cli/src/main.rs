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
mod rights;
mod source;

#[derive(Debug, Parser)]
#[command(name = "wt", version, about = "Wheel Turning operations")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Where statements, drafts and mandate decisions are stored, by SHA-256.
    #[arg(long, global = true, default_value = "data/records")]
    records_dir: PathBuf,
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
    /// Sources and their settings (Spec 03, Seattle Soto Zen sources; Spec 08, Mandated
    /// sources).
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// The rights ledger: permission requests and their answers (Spec 08, Rights lifecycle).
    Rights {
        #[command(subcommand)]
        command: RightsCommand,
    },
    /// Permanent exclusions (Spec 08, Rights lifecycle).
    Exclusion {
        #[command(subcommand)]
        command: ExclusionCommand,
    },
    /// A teacher's standing (Spec 03, Mandated talks).
    Teacher {
        #[command(subcommand)]
        command: TeacherCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TeacherCommand {
    /// Take a teacher out. Their passages leave the serving view at once and no job of theirs
    /// passes the gate. Unless --suspend-only, it then deletes their passages, embeddings,
    /// segments and transcripts and excludes the person and the transcripts' content hashes
    /// for good, which needs --yes.
    OptOut {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// Why, in their words or yours.
        #[arg(long)]
        reason: String,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Only suspend, while a request is verified. Deletes nothing.
        #[arg(long, conflicts_with = "yes")]
        suspend_only: bool,
        /// Confirm the takedown and exclusions.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct SourceOrTeacher {
    /// A source, by key.
    #[arg(long)]
    source: Option<String>,
    /// A teacher's notice, by display name or alias.
    #[arg(long)]
    teacher: Option<String>,
}

#[derive(Debug, Subcommand)]
enum RightsCommand {
    /// Record a drafted permission request. Nothing is sent: a person sends it by hand.
    Request {
        /// The source key.
        #[arg(long)]
        source: String,
        /// The address or contact page it goes to.
        #[arg(long)]
        to: String,
        /// email, form or other.
        #[arg(long, value_parser = ["email", "form", "other"])]
        channel: String,
        /// The drafted request, stored as a record.
        #[arg(long)]
        draft_file: PathBuf,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Record that the latest drafted request or teacher notice went out, and schedule its
    /// follow-up 14 days later.
    Sent {
        #[command(flatten)]
        to: SourceOrTeacher,
        /// The day it went out, YYYY-MM-DD.
        #[arg(long)]
        on: chrono::NaiveDate,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Record a grant: stores the reply as permission evidence for the source's prefixes and
    /// switches its basis to permission.
    Grant {
        /// The source key.
        #[arg(long)]
        source: String,
        /// The reply, for example an email pasted into a text file.
        #[arg(long)]
        statement_file: PathBuf,
        /// Who gave it.
        #[arg(long)]
        from: String,
        /// The day it was given, YYYY-MM-DD.
        #[arg(long)]
        given_on: chrono::NaiveDate,
        /// What it covers, in the grantor's words.
        #[arg(long)]
        scope: String,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Record a refusal: switches the source off and excludes its prefixes permanently. Needs
    /// --yes.
    Decline {
        /// The source key.
        #[arg(long)]
        source: String,
        /// The reply, stored as a record.
        #[arg(long)]
        statement_file: PathBuf,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Confirm.
        #[arg(long)]
        yes: bool,
    },
    /// Record that the latest sent request or notice went unanswered. Silence is never
    /// permission: a source stays on its mandate.
    NoResponse {
        #[command(flatten)]
        to: SourceOrTeacher,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Print the ledger in order: everything, one source's, or one teacher's.
    Ledger {
        /// Only this source.
        #[arg(long, conflicts_with = "teacher")]
        source: Option<String>,
        /// Only this teacher, by display name or alias.
        #[arg(long)]
        teacher: Option<String>,
    },
    /// Turn mandated talks on (mandate) or off (consent). Off deletes nothing: the serving
    /// view drops mandated talks and their jobs stop at the gate. On clears waiting talks that
    /// now pass.
    SetTalksBasis {
        /// mandate or consent.
        #[arg(value_parser = ["mandate", "consent"])]
        basis: String,
        /// Who decided.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Draft the notice to a teacher from templates/teacher_notice.md and record it as a
    /// drafted request. Nothing is sent: a person sends it by hand, then runs `rights sent
    /// --teacher`.
    NoticeDraft {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// Where it will go: an address or a contact page.
        #[arg(long)]
        to: String,
        /// email, form or other.
        #[arg(long, value_parser = ["email", "form", "other"], default_value = "email")]
        channel: String,
        /// Where to write the drafted notice.
        #[arg(long)]
        out: PathBuf,
        /// The sender's name, for the signature.
        #[arg(long)]
        from_name: Option<String>,
        /// How to reach the sender, for the signature.
        #[arg(long)]
        contact: Option<String>,
        /// The template.
        #[arg(long, default_value = "templates/teacher_notice.md")]
        template: PathBuf,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// List requests not yet sent and requests past their follow-up date.
    Due {
        /// The day to check against, YYYY-MM-DD. Defaults to today (UTC).
        #[arg(long)]
        today: Option<chrono::NaiveDate>,
    },
}

#[derive(Debug, Subcommand)]
enum ExclusionCommand {
    /// Every active exclusion.
    List,
    /// Lift an exclusion. Needs the rights holder's own written grant, which is stored and
    /// cited.
    Lift {
        /// The exclusion id.
        #[arg(long)]
        id: sqlx::types::Uuid,
        /// Who is lifting it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// The rights holder's written grant.
        #[arg(long)]
        evidence: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum SourceCommand {
    /// Add a source of texts or talks. By default it is used on the owner's mandate, recorded
    /// with a rationale and the attribution line shown on excerpts; --basis license or
    /// permission uses approved evidence instead. Refused if an exclusion covers a prefix.
    #[command(visible_alias = "mandate")]
    Add {
        /// Short key, such as bdkamerica.
        #[arg(long)]
        key: String,
        /// The publisher or rights holder.
        #[arg(long)]
        publisher: String,
        /// A URL prefix the source covers. Repeat for several.
        #[arg(long = "prefix", required = true)]
        prefixes: Vec<String>,
        /// mandate, license or permission.
        #[arg(long, default_value = "mandate", value_parser = parse_basis)]
        basis: wt_db::rights::Basis,
        /// texts or talks. A talks source is named by `rights_source` in sources.toml.
        #[arg(long, default_value = "texts", value_parser = ["texts", "talks"])]
        kind: String,
        /// The approved license or permission evidence, for those bases.
        #[arg(long)]
        evidence: Option<sqlx::types::Uuid>,
        /// Who decided.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Why the owner relies on the source. Required for a mandate.
        #[arg(long)]
        rationale: Option<String>,
        /// The attribution line on excerpts; may use {title}, {translator}, {publisher} and
        /// {url}. Required for a mandate.
        #[arg(long)]
        display_notice: Option<String>,
    },
    /// Switch a source off. Every serving view drops it at once.
    Disable {
        /// The source key.
        key: String,
        /// Why.
        #[arg(long)]
        reason: String,
        /// Who is doing it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Switch a source back on.
    Enable {
        /// The source key.
        key: String,
        /// Who is doing it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Look up a podcast's feed address from its Apple Podcasts id and print it. Saves it as
    /// feed_url on the source in sources.toml only with --yes, after you have checked it.
    ResolveFeed {
        /// The Apple Podcasts id, such as 1472964846 for Seattle Soto Zen.
        #[arg(long)]
        apple_id: u64,
        /// The feed source in sources.toml.
        #[arg(long, default_value = "ssz-podcast")]
        source: String,
        /// Save the address. Without it nothing is written.
        #[arg(long)]
        yes: bool,
        /// The directory holding sources.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
        /// The lookup API address. For tests.
        #[arg(long, hide = true, default_value = wt_ingest::podcast::APPLE_LOOKUP)]
        lookup_url: String,
    },
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
    /// and write the completeness page. Without WT_VIMEO_ACCESS_TOKEN the account list comes
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

fn parse_basis(value: &str) -> Result<wt_db::rights::Basis, String> {
    wt_db::rights::Basis::parse(value)
        .ok_or_else(|| format!("{value:?} is not one of mandate, license, permission"))
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
    let records = wt_core::records::LocalRecords::new(&cli.records_dir);
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
        Command::Ingest {
            command: IngestCommand::Report { out, config_dir },
        } => ingest::report(&pool().await?, &config_dir, &out).await?,
        Command::Ingest {
            command:
                IngestCommand::Completeness {
                    source,
                    vimeo_list,
                    out,
                    config_dir,
                },
        } => {
            ingest::completeness(
                &pool().await?,
                &config,
                &config_dir,
                &source,
                &vimeo_list,
                &out,
            )
            .await?;
        }
        Command::Ingest {
            command:
                IngestCommand::Reclassify {
                    talk,
                    dry_run,
                    config_dir,
                },
        } => ingest::reclassify(&pool().await?, &config_dir, talk, dry_run).await?,
        Command::Source {
            command:
                SourceCommand::ResolveFeed {
                    apple_id,
                    source,
                    yes,
                    config_dir,
                    lookup_url,
                },
        } => {
            source::resolve_feed(
                &pool().await?,
                &config_dir,
                &source,
                apple_id,
                &lookup_url,
                yes,
            )
            .await?
        }
        Command::Source {
            command:
                SourceCommand::Add {
                    key,
                    publisher,
                    prefixes,
                    basis,
                    kind,
                    evidence,
                    by,
                    rationale,
                    display_notice,
                },
        } => {
            rights::add(
                &pool().await?,
                &records,
                rights::AddArgs {
                    key: &key,
                    publisher: &publisher,
                    prefixes: &prefixes,
                    basis,
                    kind: &kind,
                    evidence,
                    by: &by,
                    rationale: rationale.as_deref(),
                    display_notice: display_notice.as_deref(),
                },
            )
            .await?;
        }
        Command::Source {
            command: SourceCommand::Disable { key, reason, by },
        } => rights::set_enabled(&pool().await?, &key, false, Some(&reason), &by).await?,
        Command::Source {
            command: SourceCommand::Enable { key, by },
        } => rights::set_enabled(&pool().await?, &key, true, None, &by).await?,
        Command::Rights { command } => match command {
            RightsCommand::Request {
                source,
                to,
                channel,
                draft_file,
                by,
            } => {
                rights::request(
                    &pool().await?,
                    &records,
                    &source,
                    &to,
                    &channel,
                    &draft_file,
                    &by,
                )
                .await?;
            }
            RightsCommand::Sent { to, on, by } => {
                rights::sent(
                    &pool().await?,
                    to.source.as_deref(),
                    to.teacher.as_deref(),
                    on,
                    &by,
                )
                .await?;
            }
            RightsCommand::Grant {
                source,
                statement_file,
                from,
                given_on,
                scope,
                by,
            } => {
                rights::grant(
                    &pool().await?,
                    &records,
                    &source,
                    &statement_file,
                    &from,
                    given_on,
                    &scope,
                    &by,
                )
                .await?;
            }
            RightsCommand::Decline {
                source,
                statement_file,
                by,
                yes,
            } => {
                rights::decline(&pool().await?, &records, &source, &statement_file, &by, yes)
                    .await?;
            }
            RightsCommand::NoResponse { to, by } => {
                rights::no_response(
                    &pool().await?,
                    to.source.as_deref(),
                    to.teacher.as_deref(),
                    &by,
                )
                .await?;
            }
            RightsCommand::SetTalksBasis { basis, by } => {
                rights::set_talks_basis(&pool().await?, &basis, &by).await?;
            }
            RightsCommand::NoticeDraft {
                teacher,
                to,
                channel,
                out,
                from_name,
                contact,
                template,
                by,
            } => {
                rights::notice_draft(
                    &pool().await?,
                    &records,
                    rights::NoticeArgs {
                        teacher: &teacher,
                        to: &to,
                        channel: &channel,
                        out: &out,
                        from_name: from_name.as_deref(),
                        contact: contact.as_deref(),
                        template: &template,
                        by: &by,
                    },
                )
                .await?;
            }
            RightsCommand::Ledger { source, teacher } => {
                rights::ledger(&pool().await?, source.as_deref(), teacher.as_deref()).await?;
            }
            RightsCommand::Due { today } => {
                let today = today.unwrap_or_else(|| chrono::Utc::now().date_naive());
                rights::due(&pool().await?, today).await?;
            }
        },
        Command::Teacher {
            command:
                TeacherCommand::OptOut {
                    teacher,
                    reason,
                    by,
                    suspend_only,
                    yes,
                },
        } => rights::opt_out(&pool().await?, &teacher, &reason, &by, suspend_only, yes).await?,
        Command::Exclusion { command } => match command {
            ExclusionCommand::List => rights::exclusions(&pool().await?).await?,
            ExclusionCommand::Lift { id, by, evidence } => {
                rights::lift(&pool().await?, &records, id, &by, &evidence).await?;
            }
        },
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
