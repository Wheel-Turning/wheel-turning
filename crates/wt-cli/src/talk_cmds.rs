//! `wt talk`, `wt takedown` and `wt teacher`: rights holders, removal and standing for
//! talks and teachers (Spec 03, Spec 07, Spec 08).

use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Subcommand};
use wt_core::ids::TalkId;

use crate::{Ctx, commands, index_cmds, rights, stages};

#[derive(Debug, Subcommand)]
pub(crate) enum TalkCommand {
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
    /// Take a ready talk out of serving after a blocking quality failure, deleting nothing
    /// (Spec 07, runbook procedure 4).
    Quarantine {
        /// The talk id.
        #[arg(long)]
        talk: TalkId,
        /// What failed, recorded as the quarantine reason and in correction_log.
        #[arg(long)]
        reason: String,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Return a talk quarantined with `wt talk quarantine` to serving, after re-checking that
    /// it has passages and a valid rights basis.
    Release {
        /// The talk id.
        #[arg(long)]
        talk: TalkId,
        /// The repair and the decision, recorded in correction_log.
        #[arg(long)]
        note: String,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Set the speaker of a talk the registry could not resolve, or correct a wrong match, or
    /// apply a review sheet's decisions (Spec 03, Assigning a speaker by hand).
    AssignSpeaker {
        /// The talk id.
        #[arg(long, requires = "teacher", required_unless_present = "sheet")]
        talk: Option<TalkId>,
        /// The speaking teacher, by display name or alias.
        #[arg(long, requires = "talk")]
        teacher: Option<String>,
        /// A review sheet with `talk_id`, `decision` and `note` columns: a teacher's name
        /// assigns, `exclude` excludes (needs --yes), blank or `hold` leaves the talk held.
        #[arg(long, conflicts_with_all = ["talk", "teacher", "sangha"])]
        sheet: Option<PathBuf>,
        /// The sangha where the talk was given, which receives its dana. Defaults to the
        /// source's default_sangha.
        #[arg(long)]
        sangha: Option<String>,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Print what each sheet row would do and change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Confirm the exclusions a sheet asks for.
        #[arg(long)]
        yes: bool,
        /// The directory holding sources.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub(crate) struct TalkOrTeacher {
    /// One talk, by id.
    #[arg(long)]
    talk: Option<TalkId>,
    /// Every talk of a teacher, by display name or alias.
    #[arg(long)]
    teacher: Option<String>,
}

#[derive(Debug, Args)]
#[group(id = "takedown_target", required = true, multiple = false)]
pub(crate) struct TakedownTarget {
    /// One talk, by id.
    #[arg(long)]
    talk: Option<TalkId>,
    /// Every talk of a teacher, by display name or alias.
    #[arg(long)]
    teacher: Option<String>,
    /// One text edition, by id (Spec 08).
    #[arg(long)]
    edition: Option<sqlx::types::Uuid>,
    /// Every text edition of a source, which is also switched off and its prefixes excluded.
    #[arg(long)]
    source: Option<String>,
    /// Every text edition whose source URL starts with this prefix, which is excluded.
    #[arg(long)]
    url: Option<String>,
    /// Retry object deletion for every scope in takedown_log, and copy missing takedown
    /// records to the object store.
    #[arg(long)]
    finish: bool,
    /// After a database restore, reapply every recorded takedown, from takedown_log and the
    /// copies in the object store. Needs --yes.
    #[arg(long)]
    replay: bool,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub(crate) struct TakedownArgs {
    #[command(subcommand)]
    requests: Option<TakedownSubcommand>,
    #[command(flatten)]
    target: TakedownTarget,
    /// Who asked for it or ran it, for takedown_log.
    #[arg(long, visible_alias = "by", required = true)]
    actor: Option<String>,
    /// For texts: why, kept with the exclusions.
    #[arg(long, default_value = "takedown")]
    reason: String,
    /// Print the counts and change nothing.
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Confirm a real run.
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TakedownSubcommand {
    /// Removal requests from the rights page (Spec 08, Mandated sources, Handling).
    Requests {
        #[command(subcommand)]
        command: RequestsCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RequestsCommand {
    /// Open and suspended requests, or all with --all.
    List {
        /// Every request, handled ones too.
        #[arg(long)]
        all: bool,
    },
    /// One request.
    Show {
        /// The request.
        #[arg(long)]
        id: sqlx::types::Uuid,
    },
    /// Suspends what a request names at once, before checking who sent it: the edition is
    /// quarantined, or the source switched off. Reversible.
    Suspend {
        /// The request.
        #[arg(long)]
        id: sqlx::types::Uuid,
        /// The edition; by default the one holding the reported excerpt.
        #[arg(long, conflicts_with = "source")]
        edition: Option<sqlx::types::Uuid>,
        /// A whole source instead.
        #[arg(long)]
        source: Option<String>,
        /// Who is handling it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Takes down what a request names, permanently.
    Remove {
        /// The request.
        #[arg(long)]
        id: sqlx::types::Uuid,
        /// The edition; by default the one holding the reported excerpt.
        #[arg(long, conflicts_with = "source")]
        edition: Option<sqlx::types::Uuid>,
        /// A whole source instead.
        #[arg(long)]
        source: Option<String>,
        /// Who is handling it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Confirm.
        #[arg(long)]
        yes: bool,
    },
    /// Declines a request, for example when the sender holds no rights; restores a suspension.
    Decline {
        /// The request.
        #[arg(long)]
        id: sqlx::types::Uuid,
        /// Who is handling it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Why.
        #[arg(long)]
        reason: String,
    },
}

/// Exactly one of the two, which clap's argument group guarantees.
pub(crate) enum Target {
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

#[derive(Debug, Subcommand)]
pub(crate) enum TeacherCommand {
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
    /// Set how much of a teacher's talks is shown (Spec 03, Display levels): a excluded,
    /// b excerpts only, c full (the default), d owner only, e export then delete, f full
    /// removal. Narrowing applies at once; widening needs --evidence pointing to the teacher's
    /// own request. e writes the owner export, then runs the opt-out takedown; f runs the
    /// opt-out takedown. Both need --yes.
    Display {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// a, b, c or d, or excluded, excerpts, full, owner_only.
        #[arg(long, value_parser = parse_display_level)]
        level: wt_db::display::DisplayLevel,
        /// Only this talk: a, b or d, no wider than the teacher's level.
        #[arg(long)]
        talk: Option<TalkId>,
        /// Why, in their words or yours.
        #[arg(long)]
        reason: String,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Pointer to the teacher's own request, needed to widen a level.
        #[arg(long)]
        evidence: Option<String>,
        /// For e: where the owner export goes before the takedown.
        #[arg(long, default_value = "data/owner-export")]
        out: PathBuf,
        /// For e and f: confirm the takedown and exclusions.
        #[arg(long)]
        yes: bool,
        /// The directory holding pipeline.toml, for e's export.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Set how the References list names and sorts a teacher (Spec 05, Answer assembly, rule
    /// 5), with a correction_log row. An empty value clears it, so the teacher is entered by
    /// display name. With --sheet, applies a reviewed CSV with the columns teacher,
    /// citation_name and citation_sort, all rows or none.
    Cite {
        /// Display name or alias.
        #[arg(long, required_unless_present = "sheet", conflicts_with = "sheet")]
        teacher: Option<String>,
        /// The name as References enter it, such as "Suzuki, Shunryu" or "Katagiri Roshi".
        #[arg(long, requires = "teacher")]
        citation_name: Option<String>,
        /// The key References sort it by, such as "Suzuki Shunryu".
        #[arg(long, requires = "teacher")]
        sort: Option<String>,
        /// A reviewed sheet instead of one teacher.
        #[arg(long)]
        sheet: Option<PathBuf>,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
}

pub(crate) fn parse_display_level(value: &str) -> Result<wt_db::display::DisplayLevel, String> {
    wt_db::display::DisplayLevel::parse(value)
        .ok_or_else(|| format!("unknown display level {value:?}; use a to f or its name"))
}

/// `wt talk`: rights holders, speakers, exclusions and quarantine for single talks.
pub(crate) async fn talk(ctx: &Ctx, command: TalkCommand) -> anyhow::Result<()> {
    match command {
        TalkCommand::SetRights { target, holder, by } => {
            commands::set_rights(&ctx.pool().await?, target.into(), &holder, &by).await?
        }
        TalkCommand::AssignSpeaker {
            talk,
            teacher,
            sheet,
            sangha,
            by,
            dry_run,
            yes,
            config_dir,
        } => {
            let pool = ctx.pool().await?;
            match (sheet, talk, teacher) {
                (Some(sheet), _, _) => {
                    commands::assign_speaker_sheet(&pool, &config_dir, &sheet, &by, dry_run, yes)
                        .await?
                }
                (None, Some(talk), Some(teacher)) => {
                    commands::assign_speaker(
                        &pool,
                        &config_dir,
                        talk,
                        &teacher,
                        sangha.as_deref(),
                        &by,
                    )
                    .await?
                }
                _ => anyhow::bail!("give --talk and --teacher, or --sheet"),
            }
        }
        TalkCommand::Exclude {
            talk,
            reason,
            by,
            yes,
        } => commands::exclude(&ctx.pool().await?, &ctx.config, talk, &reason, &by, yes).await?,
        TalkCommand::Quarantine { talk, reason, by } => {
            wt_db::quarantine::quarantine(&ctx.pool().await?, talk, &reason, &by)
                .await
                .with_context(|| format!("quarantining talk {talk}"))?;
            eprintln!("Talk {talk} is quarantined and no longer serves.");
        }
        TalkCommand::Release { talk, note, by } => {
            wt_db::quarantine::release(&ctx.pool().await?, talk, &note, &by)
                .await
                .with_context(|| format!("releasing talk {talk}"))?;
            eprintln!("Talk {talk} passed the re-check and serves again.");
        }
    }
    Ok(())
}

/// `wt teacher`: opt-out, display level and citation name (Spec 03, Mandated talks).
pub(crate) async fn teacher(ctx: &Ctx, command: TeacherCommand) -> anyhow::Result<()> {
    match command {
        TeacherCommand::OptOut {
            teacher,
            reason,
            by,
            suspend_only,
            yes,
        } => {
            rights::opt_out(
                &ctx.pool().await?,
                &ctx.config,
                &teacher,
                &reason,
                &by,
                suspend_only,
                yes,
            )
            .await?;
        }
        TeacherCommand::Display {
            teacher,
            level,
            talk,
            reason,
            by,
            evidence: _,
            out,
            yes,
            config_dir,
        } if matches!(
            level,
            wt_db::display::DisplayLevel::Removed | wt_db::display::DisplayLevel::ExportThenDelete
        ) =>
        {
            if talk.is_some() {
                anyhow::bail!(
                    "e and f apply to a teacher; to remove one talk use wt takedown --talk"
                );
            }
            if !yes {
                anyhow::bail!(
                    "{level} deletes {teacher}'s passages, segments and transcripts and excludes \
                     them for good; rerun with --yes"
                );
            }
            let pool = ctx.pool().await?;
            let export = if level == wt_db::display::DisplayLevel::ExportThenDelete {
                let report =
                    index_cmds::export_teacher(&pool, &ctx.config, &config_dir, &teacher, &out)
                        .await?;
                if !report.skipped.is_empty() {
                    anyhow::bail!(
                        "the export skipped {} talks whose transcripts could not be read, so \
                         nothing was deleted: {:?}",
                        report.skipped.len(),
                        report.skipped
                    );
                }
                Some(report)
            } else {
                None
            };
            let mut conn = pool.acquire().await?;
            let change =
                wt_db::display::mark_takedown_level(&mut conn, &teacher, level, &by, &reason)
                    .await?;
            drop(conn);
            let takedown = wt_db::rights::opt_out(&pool, &teacher, &reason, &by, false).await?;
            let talks = takedown
                .takedown
                .as_ref()
                .map(|t| t.talks.clone())
                .unwrap_or_default();
            let deleted = stages::delete_talk_objects(&pool, &ctx.config, &talks).await?;
            commands::print_json(&serde_json::json!({
                "level": change,
                "export": export,
                "takedown": takedown,
                "objects_deleted": deleted,
            }))?;
        }
        TeacherCommand::Display {
            teacher,
            level,
            talk,
            reason,
            by,
            evidence,
            ..
        } => {
            let pool = ctx.pool().await?;
            let mut conn = pool.acquire().await?;
            let change = wt_db::display::set_level(
                &mut conn,
                &teacher,
                talk,
                level,
                &by,
                &reason,
                evidence.as_deref(),
            )
            .await?;
            commands::print_json(&change)?;
        }
        TeacherCommand::Cite {
            teacher,
            citation_name,
            sort,
            sheet,
            by,
        } => {
            let pool = ctx.pool().await?;
            let changes = match (teacher, sheet) {
                (_, Some(sheet)) => commands::teacher_cite_sheet(&pool, &sheet, &by).await?,
                (Some(teacher), None) => {
                    let mut conn = pool.acquire().await?;
                    vec![
                        wt_db::registry::set_citation(
                            &mut conn,
                            &teacher,
                            citation_name.as_deref(),
                            sort.as_deref(),
                            &by,
                            "wt teacher cite",
                        )
                        .await?,
                    ]
                }
                (None, None) => anyhow::bail!("give --teacher or --sheet"),
            };
            commands::print_json(&changes)?;
        }
    }
    Ok(())
}

/// `wt takedown`: removes a teacher's, a talk's or a text's material from every store (Spec 07),
/// or works the removal requests from the rights page (Spec 08, Mandated sources, Handling).
pub(crate) async fn takedown(ctx: &Ctx, args: TakedownArgs) -> anyhow::Result<()> {
    if let Some(TakedownSubcommand::Requests { command }) = args.requests {
        return index_cmds::takedown_requests(&ctx.pool().await?, &ctx.config, command).await;
    }
    let actor = args.actor.unwrap_or_default();
    let t = args.target;
    let text_scope = match (t.edition, t.source, t.url) {
        (Some(e), _, _) => Some(wt_db::text_takedown::TextScope::Edition(e)),
        (_, Some(s), _) => Some(wt_db::text_takedown::TextScope::Source(s)),
        (_, _, Some(u)) => Some(wt_db::text_takedown::TextScope::UrlPrefix(u)),
        _ => None,
    };
    if t.finish {
        commands::takedown_finish(&ctx.pool().await?, &ctx.config).await?;
    } else if t.replay {
        commands::takedown_replay(&ctx.pool().await?, &ctx.config, &actor, args.yes).await?;
    } else if let Some(scope) = text_scope {
        index_cmds::takedown_texts(
            &ctx.pool().await?,
            &ctx.config,
            &scope,
            &args.reason,
            &actor,
            args.dry_run,
            args.yes,
        )
        .await?;
    } else {
        let target = match t.talk {
            Some(talk) => Target::Talk(talk),
            None => Target::Teacher(t.teacher.unwrap_or_default()),
        };
        commands::takedown(
            &ctx.pool().await?,
            &ctx.config,
            target,
            &actor,
            args.dry_run,
            args.yes,
        )
        .await?;
    }
    Ok(())
}
