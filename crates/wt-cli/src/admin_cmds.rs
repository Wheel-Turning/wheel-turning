//! `wt db`, `wt registry`, `wt sangha`, `wt report`, `wt consent`, `wt tester` and
//! `wt accessibility`: the owner's administrative commands (Spec 02, Spec 03, Spec 06).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Subcommand;
use wt_core::Config;

use wt_core::gate::ConsentStatus;

use crate::{Ctx, commands};

/// `wt db`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum DbCommand {
    /// Apply every pending migration. Safe to run repeatedly.
    Migrate,
    /// Create or update a login role that inherits one group role. Safe to run repeatedly.
    CreateLogin {
        /// Group role to inherit: wt_owner, wt_pipeline, wt_web, wt_analyst or wt_audit.
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
    /// Name this database, once: `rds-v1` on RDS, `pc` on the PC (Spec 11, PB1.1). The worker,
    /// the pipeline, the rights commands and takedown refuse a database whose name differs from
    /// WT_DATABASE_NAME. Naming it again with the same name changes nothing.
    SetIdentity {
        /// The name: lowercase letters, digits and hyphens.
        #[arg(long)]
        name: String,
    },
    /// Print this database's name, or `unnamed`.
    Identity,
    /// The one-time copy of pipeline rows from the PC to RDS (Spec 11, PB1.1): talks changed on
    /// the source after --since, with their transcripts, segments, passages, citations and
    /// student questions, and the text units of editions imported after it. Never writes
    /// testers, logs, feedback, removal requests, the rights ledger or any table the web service
    /// writes, and skips talks withdrawn, quarantined, excluded, taken down or opted out on the
    /// target. With --apply it replays the target's takedowns first. Always ends with `wt rights
    /// parity`, and exits 1 on any unsafe difference or failed copy. Prints a JSON report. A dry
    /// run unless --apply.
    CatchUp {
        /// The source database (the PC). Prefer the environment variable, which keeps the
        /// password out of shell history.
        #[arg(long, env = "WT_CATCH_UP_FROM", hide_env_values = true)]
        from: String,
        /// The target database (RDS), connecting as the owner login.
        #[arg(long, env = "WT_CATCH_UP_TO", hide_env_values = true)]
        to: String,
        /// Rows changed on the source after this time are copied: the October 13 load.
        #[arg(long, default_value = "2026-10-13T00:00:00Z")]
        since: chrono::DateTime<chrono::Utc>,
        /// Write the copy. Without it each talk's changes are rolled back.
        #[arg(long)]
        apply: bool,
        /// Who runs it, for the takedown replay's records.
        #[arg(long)]
        actor: String,
    },
}

/// `wt registry`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum RegistryCommand {
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

/// `wt sangha`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum SanghaCommand {
    /// Every sangha's donation link, kind and verification, as JSON.
    Links,
    /// Record that you opened a sangha's giving page and confirmed it takes gifts for the
    /// sangha. Only a page can be verified, and only the link you name, which must be the stored
    /// one. Records your name and the time.
    Verify {
        /// The sangha's name, as in the registry.
        #[arg(long)]
        sangha: String,
        /// The page you opened, exactly as stored (see `wt sangha links`).
        #[arg(long)]
        url: String,
        /// Who checked the page.
        #[arg(long)]
        by: String,
    },
    /// Clear a sangha's verification, with the reason kept in its donation note.
    Unverify {
        /// The sangha's name.
        #[arg(long)]
        sangha: String,
        /// Why, for example "the page no longer takes gifts".
        #[arg(long)]
        reason: String,
        /// Who is making the change.
        #[arg(long)]
        by: String,
    },
}

/// `wt report`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum ReportCommand {
    /// Answers per day, mode and teacher count, from v_answer_stats.
    Answers {
        /// How far back: a number with d (days) or h (hours), for example 7d.
        #[arg(long, default_value = "7d", value_parser = parse_since)]
        since: chrono::Duration,
    },
    /// Dana events per day and kind, from v_dana_clicks.
    Dana,
    /// Talks per state, consent status and teacher, from v_consent_coverage.
    Coverage,
    /// Flagged queries, through flagged_queries(): the only questions anyone may read.
    Flagged,
    /// Feedback submissions and reasons per day, target and mode, from v_feedback_counts.
    Feedback {
        /// How far back: a number with d (days) or h (hours), for example 7d.
        #[arg(long, default_value = "7d", value_parser = parse_since)]
        since: chrono::Duration,
        /// Also print each note with its target and reasons, through feedback_notes(), never
        /// with its question.
        #[arg(long)]
        notes: bool,
    },
    /// The weekly beta report (Spec 11 PB4.6): testers, questions, feedback, dana, model cost
    /// and speed, as totals only, never question text.
    Beta {
        /// How far back: a number with d (days) or h (hours), for example 7d.
        #[arg(long, default_value = "7d", value_parser = parse_since)]
        since: chrono::Duration,
    },
    /// What each tester did (Spec 07, Usage report): page views, visits, days active, questions,
    /// feedback and dana clicks by label, plus page views per route and activity per day. Never
    /// question text or a tester's id.
    Usage {
        /// How far back: a number with d (days) or h (hours), for example 7d. Page views go back
        /// at most 30 days.
        #[arg(long, default_value = "7d", value_parser = parse_since)]
        since: chrono::Duration,
    },
}

/// A pool connected as `wt_analyst` through `DATABASE_URL_ANALYST`, refused when the role can
/// read `query_log`, so a report never runs as a role that could read unflagged questions.
pub(crate) async fn analyst_pool(config: &Config) -> anyhow::Result<sqlx::PgPool> {
    let url = config
        .database_url_analyst
        .as_ref()
        .context("wt report runs as the analyst role: set DATABASE_URL_ANALYST")?;
    let pool = wt_db::connect(url)
        .await
        .context("connecting as the analyst role")?;
    if wt_db::report::reads_query_log(&pool).await? {
        anyhow::bail!(
            "DATABASE_URL_ANALYST connects as a role that can read query_log;                      wt report runs only as wt_analyst, which cannot read unflagged questions"
        );
    }
    Ok(pool)
}

/// `7d` or `24h`.
pub(crate) fn parse_since(raw: &str) -> Result<chrono::Duration, String> {
    let raw = raw.trim();
    let bad = || format!("{raw:?} is not a duration such as 7d or 24h");
    let (n, unit) = raw.split_at(raw.len().saturating_sub(1));
    let n: i64 = n.parse().map_err(|_| bad())?;
    match unit {
        "d" => chrono::Duration::try_days(n).ok_or_else(bad),
        "h" => chrono::Duration::try_hours(n).ok_or_else(bad),
        _ => Err(bad()),
    }
}

/// `wt consent`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum ConsentCommand {
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

/// Parses a consent status for `--status`.
pub(crate) fn parse_status(value: &str) -> Result<ConsentStatus, String> {
    ConsentStatus::parse(value).ok_or_else(|| {
        format!("{value:?} is not one of granted, requested, declined, withdrawn, unknown")
    })
}

/// `wt tester`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum TesterCommand {
    /// Create a tester and print their invite link once. Only the token's SHA-256 is stored.
    Invite {
        /// How you know them; not a legal name.
        #[arg(long)]
        label: String,
        /// Where the site is served, for the printed link.
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        base_url: String,
    },
    /// Every tester, with when they were last seen.
    List,
    /// Revoke every active tester with this label.
    Revoke {
        /// The label.
        #[arg(long)]
        label: String,
    },
}

/// `wt accessibility`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum AccessibilityCommand {
    /// Read and handle reports.
    Reports {
        #[command(subcommand)]
        command: ReportsCommand,
    },
}

/// `wt accessibility reports`' subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum ReportsCommand {
    /// Open and in-progress reports, or all with --all, oldest first.
    List {
        /// Every report, handled ones too.
        #[arg(long)]
        all: bool,
    },
    /// One report.
    Show {
        /// The report.
        #[arg(long)]
        id: sqlx::types::Uuid,
    },
    /// Records how a report is handled. Replies are sent by hand until domain email exists.
    Set {
        /// The report.
        #[arg(long)]
        id: sqlx::types::Uuid,
        /// open, fixing, fixed or declined.
        #[arg(long)]
        status: String,
        /// Who is handling it.
        #[arg(long)]
        by: String,
        /// What was done, kept with the report.
        #[arg(long)]
        note: Option<String>,
    },
}

/// `wt db`: migrations and the login roles (Spec 02).
pub(crate) async fn db(ctx: &Ctx, command: DbCommand) -> anyhow::Result<ExitCode> {
    match command {
        DbCommand::SetIdentity { name } => {
            wt_db::identity::set(&ctx.pool().await?, &name).await?;
            println!("{name}");
        }
        DbCommand::Identity => {
            let name = wt_db::identity::read(&ctx.pool().await?).await?;
            println!("{}", name.as_deref().unwrap_or("unnamed"));
        }
        DbCommand::CatchUp {
            from,
            to,
            since,
            apply,
            actor,
        } => {
            let source = wt_db::connect(&wt_core::Secret::new(from))
                .await
                .context("connecting to the source database")?;
            let target = wt_db::connect(&wt_core::Secret::new(to))
                .await
                .context("connecting to the target database")?;
            let store = crate::stages::objects(&ctx.config)?;
            let options = wt_db::catch_up::Options {
                since,
                apply,
                actor,
            };
            let report = wt_db::catch_up::run(&source, &target, &store, &options)
                .await
                .context("catching up")?;
            commands::print_json(&report)?;
            if report.failed() {
                return Ok(ExitCode::FAILURE);
            }
        }
        DbCommand::Migrate => {
            let report = wt_db::migrate(&ctx.pool().await?)
                .await
                .context("applying migrations")?;
            tracing::info!(
                applied = report.applied,
                known = report.known,
                "migrations up to date"
            );
        }
        DbCommand::CreateLogin {
            role,
            login,
            password_env,
        } => {
            let login = login.unwrap_or_else(|| format!("{role}_login"));
            let password = ctx
                .config
                .source_secret(&password_env)
                .with_context(|| format!("{password_env} is not set"))?;
            let outcome = wt_db::create_login(&ctx.pool().await?, &role, &login, &password)
                .await
                .with_context(|| format!("creating login {login}"))?;
            tracing::info!(login = %login, role = %role, outcome = ?outcome, "login ready");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `wt registry`: imports the teacher registry file (Spec 03).
pub(crate) async fn registry(ctx: &Ctx, command: RegistryCommand) -> anyhow::Result<()> {
    match command {
        RegistryCommand::Import { file, by } => {
            commands::registry_import(&ctx.pool().await?, &file, &by).await?
        }
    }
    Ok(())
}

/// `wt sangha`: the donation links and their verification (Spec 02).
pub(crate) async fn sangha(ctx: &Ctx, command: SanghaCommand) -> anyhow::Result<()> {
    let pool = ctx.pool().await?;
    match command {
        SanghaCommand::Links => commands::print_json(&wt_db::donation::list(&pool).await?)?,
        SanghaCommand::Verify { sangha, url, by } => {
            commands::print_json(&wt_db::donation::verify(&pool, &sangha, &url, &by).await?)?
        }
        SanghaCommand::Unverify { sangha, reason, by } => {
            commands::print_json(&wt_db::donation::unverify(&pool, &sangha, &reason, &by).await?)?
        }
    }
    Ok(())
}

/// `wt report`: the aggregate reports, read as the analyst role (Spec 06, rule 6).
pub(crate) async fn report(ctx: &Ctx, command: ReportCommand) -> anyhow::Result<()> {
    let pool = analyst_pool(&ctx.config).await?;
    match command {
        ReportCommand::Answers { since } => {
            let from = chrono::Utc::now() - since;
            commands::print_json(&wt_db::report::answers(&pool, from).await?)?;
        }
        ReportCommand::Dana => commands::print_json(&wt_db::report::dana(&pool).await?)?,
        ReportCommand::Coverage => {
            commands::print_json(&wt_db::report::coverage(&pool).await?)?;
        }
        ReportCommand::Flagged => {
            commands::print_json(&wt_db::report::flagged(&pool).await?)?;
        }
        ReportCommand::Feedback { since, notes } => {
            let from = chrono::Utc::now() - since;
            let counts = wt_db::report::feedback(&pool, from).await?;
            if notes {
                let notes = wt_db::report::feedback_notes(&pool, from).await?;
                commands::print_json(&serde_json::json!({ "counts": counts, "notes": notes }))?;
            } else {
                commands::print_json(&counts)?;
            }
        }
        ReportCommand::Beta { since } => {
            let from = chrono::Utc::now() - since;
            commands::print_json(&wt_db::report::beta(&pool, from).await?)?;
        }
        ReportCommand::Usage { since } => {
            let from = chrono::Utc::now() - since;
            commands::print_json(&wt_db::report::usage(&pool, from).await?)?;
        }
    }
    Ok(())
}

/// `wt consent`: records and lists speaker consent (Spec 03, the consent gate).
pub(crate) async fn consent(ctx: &Ctx, command: ConsentCommand) -> anyhow::Result<()> {
    match command {
        ConsentCommand::Set {
            teacher,
            status,
            evidence,
            scope_note,
            by,
            yes,
        } => {
            commands::consent_set(
                &ctx.pool().await?,
                &ctx.config,
                commands::ConsentChange {
                    teacher: &teacher,
                    status,
                    evidence: evidence.as_deref(),
                    scope_note: scope_note.as_deref(),
                    by: &by,
                    yes,
                },
            )
            .await?;
        }
        ConsentCommand::List => commands::consent_list(&ctx.pool().await?).await?,
    }
    Ok(())
}

/// `wt tester`: invite links, shown once and stored only as a hash (Spec 06).
pub(crate) async fn tester(ctx: &Ctx, command: TesterCommand) -> anyhow::Result<()> {
    let pool = ctx.pool().await?;
    match command {
        TesterCommand::Invite { label, base_url } => {
            let token = wt_api::session::new_token().context("generating a token")?;
            wt_db::tester::invite(&pool, &label, &wt_api::session::token_hash(&token))
                .await
                .context("recording the tester")?;
            let link = format!("{}/join/{token}", base_url.trim_end_matches('/'));
            println!("{link}");
            // The invitation carries the privacy line from its one source (Spec 11, PB3.1).
            println!("\n{}", wt_api::chrome::invitation(&link));
            eprintln!(
                "This link is shown once. Send it to {label} privately, with the invitation."
            );
        }
        TesterCommand::List => {
            commands::print_json(&wt_db::tester::list(&pool).await?)?;
        }
        TesterCommand::Revoke { label } => {
            let n = wt_db::tester::revoke(&pool, &label).await?;
            commands::print_json(&serde_json::json!({ "revoked": n }))?;
        }
    }
    Ok(())
}

/// `wt accessibility reports`: triage of the problem reports from `/accessibility`.
pub(crate) async fn accessibility(ctx: &Ctx, command: AccessibilityCommand) -> anyhow::Result<()> {
    match command {
        AccessibilityCommand::Reports { command } => {
            use wt_db::public_forms as forms;
            let pool = ctx.pool().await?;
            match command {
                ReportsCommand::List { all } => {
                    commands::print_json(&forms::reports(&pool, all).await?)?;
                }
                ReportsCommand::Show { id } => {
                    let report = forms::report(&pool, id)
                        .await?
                        .with_context(|| format!("no report {id}"))?;
                    commands::print_json(&report)?;
                }
                ReportsCommand::Set {
                    id,
                    status,
                    by,
                    note,
                } => {
                    anyhow::ensure!(
                        forms::REPORT_STATUSES.contains(&status.as_str()),
                        "status is one of {}",
                        forms::REPORT_STATUSES.join(", ")
                    );
                    anyhow::ensure!(
                        forms::set_report(&pool, id, &status, &by, note.as_deref()).await?,
                        "no report {id}"
                    );
                    commands::print_json(&serde_json::json!({ "report": id, "status": status }))?;
                }
            }
        }
    }
    Ok(())
}
