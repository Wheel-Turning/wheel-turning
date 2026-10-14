//! `wt source`, `wt rights` and `wt exclusion`: sources, the rights ledger and permanent
//! exclusions (Spec 03; Spec 08, Rights lifecycle). The work is in `rights` and `source`.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Subcommand};

use crate::{Ctx, commands, rights, source};

#[derive(Debug, Subcommand)]
// `[rights]` in the help text names the pipeline.toml table, not an intra-doc link.
#[allow(rustdoc::broken_intra_doc_links)]
pub(crate) enum SourceCommand {
    /// Lists text editions with a copyright notice the evidence does not cover, and those in
    /// quarantine, for one source or all.
    Report {
        /// One source.
        #[arg(long)]
        key: Option<String>,
    },
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
        /// mandate, license or permission. Defaults to [rights] default_basis_for_new_sources
        /// in pipeline.toml.
        #[arg(long, value_parser = parse_basis)]
        basis: Option<wt_db::rights::Basis>,
        /// The directory holding pipeline.toml, read when --basis is not given.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
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

pub(crate) fn parse_basis(value: &str) -> Result<wt_db::rights::Basis, String> {
    wt_db::rights::Basis::parse(value)
        .ok_or_else(|| format!("{value:?} is not one of mandate, license, permission"))
}

#[derive(Debug, Subcommand)]
pub(crate) enum RightsCommand {
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

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub(crate) struct SourceOrTeacher {
    /// A source, by key.
    #[arg(long)]
    source: Option<String>,
    /// A teacher's notice, by display name or alias.
    #[arg(long)]
    teacher: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ExclusionCommand {
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

/// `wt source`: adds, switches and reports on sources (Spec 03; Spec 08, Mandated sources).
pub(crate) async fn source(ctx: &Ctx, command: SourceCommand) -> anyhow::Result<()> {
    match command {
        SourceCommand::ResolveFeed {
            apple_id,
            source,
            yes,
            config_dir,
            lookup_url,
        } => {
            source::resolve_feed(
                &ctx.pool().await?,
                &config_dir,
                &source,
                apple_id,
                &lookup_url,
                yes,
            )
            .await?
        }
        SourceCommand::Add {
            key,
            publisher,
            prefixes,
            basis,
            config_dir,
            kind,
            evidence,
            by,
            rationale,
            display_notice,
        } => {
            let basis = match basis {
                Some(basis) => basis,
                None => {
                    let path = config_dir.join("pipeline.toml");
                    wt_ingest::config::RightsSettings::load(&path)
                        .with_context(|| {
                            format!(
                                "reading [rights] default_basis_for_new_sources from {}; \
                                 pass --basis or --config-dir",
                                path.display()
                            )
                        })?
                        .default_basis_for_new_sources
                }
            };
            rights::add(
                &ctx.pool().await?,
                &ctx.records,
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
        SourceCommand::Disable { key, reason, by } => {
            rights::set_enabled(&ctx.pool().await?, &key, false, Some(&reason), &by).await?
        }
        SourceCommand::Enable { key, by } => {
            rights::set_enabled(&ctx.pool().await?, &key, true, None, &by).await?
        }
        SourceCommand::Report { key } => {
            let pool = ctx.pool().await?;
            let mut conn = pool.acquire().await?;
            let rows = wt_db::text_takedown::noticed(&mut conn, key.as_deref()).await?;
            commands::print_json(&rows)?;
        }
    }
    Ok(())
}

/// `wt rights`: the ledger of requests and answers (Spec 08, Rights lifecycle).
pub(crate) async fn rights(ctx: &Ctx, command: RightsCommand) -> anyhow::Result<()> {
    match command {
        RightsCommand::Request {
            source,
            to,
            channel,
            draft_file,
            by,
        } => {
            rights::request(
                &ctx.pool().await?,
                &ctx.records,
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
                &ctx.pool().await?,
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
                &ctx.pool().await?,
                &ctx.records,
                rights::GrantArgs {
                    key: &source,
                    statement: &statement_file,
                    from: &from,
                    given_on,
                    scope: &scope,
                    by: &by,
                },
            )
            .await?;
        }
        RightsCommand::Decline {
            source,
            statement_file,
            by,
            yes,
        } => {
            rights::decline(
                &ctx.pool().await?,
                &ctx.config,
                &ctx.records,
                &source,
                &statement_file,
                &by,
                yes,
            )
            .await?;
        }
        RightsCommand::NoResponse { to, by } => {
            rights::no_response(
                &ctx.pool().await?,
                to.source.as_deref(),
                to.teacher.as_deref(),
                &by,
            )
            .await?;
        }
        RightsCommand::SetTalksBasis { basis, by } => {
            rights::set_talks_basis(&ctx.pool().await?, &basis, &by).await?;
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
                &ctx.pool().await?,
                &ctx.records,
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
            rights::ledger(&ctx.pool().await?, source.as_deref(), teacher.as_deref()).await?;
        }
        RightsCommand::Due { today } => {
            let today = today.unwrap_or_else(|| chrono::Utc::now().date_naive());
            rights::due(&ctx.pool().await?, today).await?;
        }
    }
    Ok(())
}

/// `wt exclusion`: lists and lifts permanent exclusions (Spec 08).
pub(crate) async fn exclusion(ctx: &Ctx, command: ExclusionCommand) -> anyhow::Result<()> {
    match command {
        ExclusionCommand::List => rights::exclusions(&ctx.pool().await?).await?,
        ExclusionCommand::Lift { id, by, evidence } => {
            rights::lift(&ctx.pool().await?, &ctx.records, id, &by, &evidence).await?;
        }
    }
    Ok(())
}
