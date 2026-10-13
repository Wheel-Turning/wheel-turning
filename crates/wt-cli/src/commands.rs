//! Handlers for the registry, consent, talk, takedown and audit commands. Each prints its result
//! to stdout as one JSON document, except `consent list`, which prints a table for people.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, bail};
use serde::Serialize;
use sqlx::PgPool;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_db::audit::AuditSettings;
use wt_db::consent::{Holder, RightsTarget};
use wt_db::registry::{self, SpeakerOutcome};
use wt_db::takedown::{Scope, TakedownReport};
use wt_ingest::config::SourcesFile;
use wt_registry::RegistryFile;

use crate::Target;

pub(crate) fn print_json(value: &impl Serialize) -> anyhow::Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).context("rendering the report")?
    );
    Ok(())
}

/// Prints what a takedown would remove and stops, for runs that were not confirmed.
async fn refuse_unconfirmed(pool: &PgPool, scope: &Scope, what: &str) -> anyhow::Result<()> {
    let preview = wt_db::takedown::dry_run(pool, scope)
        .await
        .context("counting what the takedown would remove")?;
    print_json(&preview)?;
    bail!(
        "{what} deletes the text of {} talk(s), counted above; nothing was changed. Rerun with --yes",
        preview.talks.len()
    )
}

pub async fn registry_import(pool: &PgPool, file: &Path, by: &str) -> anyhow::Result<()> {
    let registry = RegistryFile::load(file)?;
    let report = wt_db::registry::import(pool, &registry, by)
        .await
        .with_context(|| format!("importing {}", file.display()))?;
    print_json(&report)
}

#[allow(clippy::too_many_arguments)]
pub async fn consent_set(
    pool: &PgPool,
    config: &wt_core::Config,
    teacher: &str,
    status: ConsentStatus,
    evidence: Option<&str>,
    scope_note: Option<&str>,
    by: &str,
    yes: bool,
) -> anyhow::Result<()> {
    let takes_down = matches!(status, ConsentStatus::Declined | ConsentStatus::Withdrawn);
    if takes_down && !yes {
        let what = format!("consent {status} for {teacher}");
        return refuse_unconfirmed(pool, &Scope::Teacher(teacher.to_owned()), &what).await;
    }
    let propagation = wt_db::consent::set_consent(pool, teacher, status, evidence, scope_note, by)
        .await
        .with_context(|| format!("recording consent {status} for {teacher}"))?;
    let talks = propagation
        .takedown
        .as_ref()
        .map(|t| t.talks.clone())
        .unwrap_or_default();
    let deleted = crate::stages::delete_talk_objects(pool, config, &talks).await?;
    print_json(&crate::stages::with_objects(&propagation, deleted))
}

pub async fn consent_list(pool: &PgPool) -> anyhow::Result<()> {
    let rows = wt_db::consent::list_consent(pool)
        .await
        .context("listing consent")?;
    let width = rows
        .iter()
        .map(|r| r.teacher.len())
        .max()
        .unwrap_or(7)
        .max(7);
    println!(
        "{:width$}  {:9}  {:10}  {:20}  talks",
        "teacher", "status", "recorded", "evidence"
    );
    for row in rows {
        let recorded = row
            .recorded_at
            .map(|at| at.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        println!(
            "{:width$}  {:9}  {:10}  {:20}  {}",
            row.teacher,
            row.status,
            recorded,
            row.evidence_ref.unwrap_or_default(),
            row.talks
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct Cleared {
    cleared: Vec<TalkId>,
}

pub async fn set_rights(
    pool: &PgPool,
    target: Target,
    holder: &str,
    by: &str,
) -> anyhow::Result<()> {
    let target = match target {
        Target::Talk(id) => RightsTarget::Talk(id),
        Target::Teacher(name) => RightsTarget::Teacher(name),
    };
    let holder = if holder.trim().eq_ignore_ascii_case("speaker") {
        Holder::Speaker
    } else {
        Holder::Named(holder.to_owned())
    };
    let cleared = wt_db::consent::set_rights(pool, target, holder, by)
        .await
        .context("setting the rights holder")?;
    print_json(&Cleared { cleared })
}

/// One talk's speaker, as set by hand.
#[derive(Debug, Serialize)]
struct SpeakerAssigned {
    talk: TalkId,
    teacher: String,
    sangha: String,
    /// `cleared` when the rights basis was already in place, otherwise `awaiting_consent`.
    state: &'static str,
}

/// `wt talk assign-speaker --talk --teacher` (Spec 03, Assigning a speaker by hand).
pub async fn assign_speaker(
    pool: &PgPool,
    config_dir: &Path,
    talk: TalkId,
    teacher: &str,
    sangha: Option<&str>,
    by: &str,
) -> anyhow::Result<()> {
    let sources = SourcesFile::load(&config_dir.join("sources.toml"))?;
    let done = assign_one(pool, &sources, talk, teacher, sangha, by).await?;
    print_json(&done)
}

async fn assign_one(
    pool: &PgPool,
    sources: &SourcesFile,
    talk: TalkId,
    teacher: &str,
    sangha: Option<&str>,
    by: &str,
) -> anyhow::Result<SpeakerAssigned> {
    let mut tx = pool.begin().await?;
    let (teacher_id, display_name) = registry::find_teacher(&mut tx, teacher).await?;
    let sangha_name = match sangha {
        Some(name) => name.to_owned(),
        None => {
            let key = registry::talk_source_key(&mut tx, talk)
                .await?
                .with_context(|| format!("talk {talk} has no source; give its sangha"))?;
            sources.get(&key)?.default_sangha.clone()
        }
    };
    let sangha_id = registry::find_sangha(&mut tx, &sangha_name).await?;
    let outcome = registry::assign_speaker_by_hand(&mut tx, talk, teacher_id, sangha_id, by)
        .await
        .with_context(|| format!("assigning the speaker of talk {talk}"))?;
    tx.commit().await?;
    Ok(SpeakerAssigned {
        talk,
        teacher: display_name,
        sangha: sangha_name,
        state: match outcome {
            SpeakerOutcome::Cleared => "cleared",
            SpeakerOutcome::AwaitingConsent => "awaiting_consent",
            SpeakerOutcome::NeedsReview => "needs_review",
        },
    })
}

/// A row of the speaker review sheet. Other columns are ignored.
#[derive(Debug, serde::Deserialize)]
struct SheetRow {
    talk_id: String,
    #[serde(default)]
    decision: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    sangha: String,
    #[serde(default)]
    title: String,
}

/// What one sheet row asks for.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Hold,
    Exclude,
    Assign(String),
}

impl Decision {
    fn of(decision: &str) -> Self {
        let d = decision.trim();
        if d.is_empty() || d.eq_ignore_ascii_case("hold") {
            Decision::Hold
        } else if d.eq_ignore_ascii_case("exclude") {
            Decision::Exclude
        } else {
            Decision::Assign(d.to_owned())
        }
    }
}

/// What happened to one sheet row.
#[derive(Debug, Serialize)]
struct SheetOutcome {
    talk_id: String,
    title: String,
    decision: String,
    /// `assigned`, `excluded`, `held`, `would assign`, `would exclude` or `failed`.
    result: &'static str,
    /// The teacher and state for an assignment, or the error for a failure.
    detail: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct SheetReport {
    assigned: usize,
    excluded: usize,
    held: usize,
    failed: usize,
    rows: Vec<SheetOutcome>,
}

/// `wt talk assign-speaker --sheet` (Spec 03, Assigning a speaker by hand). Each row applies on
/// its own, so a failed row is reported and the rest still apply.
pub async fn assign_speaker_sheet(
    pool: &PgPool,
    config_dir: &Path,
    sheet: &Path,
    by: &str,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let sources = SourcesFile::load(&config_dir.join("sources.toml"))?;
    let text =
        std::fs::read_to_string(sheet).with_context(|| format!("reading {}", sheet.display()))?;
    let rows = csv::Reader::from_reader(text.trim_start_matches('\u{feff}').as_bytes())
        .deserialize()
        .collect::<Result<Vec<SheetRow>, _>>()
        .with_context(|| format!("parsing {}", sheet.display()))?;
    let exclusions = rows
        .iter()
        .filter(|r| Decision::of(&r.decision) == Decision::Exclude)
        .count();
    if exclusions > 0 && !dry_run && !yes {
        bail!(
            "{exclusions} rows ask to exclude their talk, which runs a takedown: rerun with --yes"
        );
    }

    let mut report = SheetReport::default();
    for row in rows {
        let decision = Decision::of(&row.decision);
        let (result, detail) = match (&decision, dry_run) {
            (Decision::Hold, _) => {
                report.held += 1;
                ("held", None)
            }
            (Decision::Exclude, true) => ("would exclude", None),
            (Decision::Assign(teacher), true) => ("would assign", Some(teacher.clone())),
            (Decision::Exclude, false) => {
                let reason = match row.note.trim() {
                    "" => "excluded at speaker review",
                    note => note,
                };
                match exclude_row(pool, &row.talk_id, reason, by).await {
                    Ok(()) => {
                        report.excluded += 1;
                        ("excluded", None)
                    }
                    Err(e) => {
                        report.failed += 1;
                        ("failed", Some(format!("{e:#}")))
                    }
                }
            }
            (Decision::Assign(teacher), false) => {
                let sangha = Some(row.sangha.trim()).filter(|s| !s.is_empty());
                let done = match row.talk_id.trim().parse::<TalkId>() {
                    Ok(talk) => assign_one(pool, &sources, talk, teacher, sangha, by).await,
                    Err(e) => Err(anyhow::anyhow!("bad talk id {:?}: {e}", row.talk_id)),
                };
                match done {
                    Ok(done) => {
                        report.assigned += 1;
                        (
                            "assigned",
                            Some(format!("{}, {}", done.teacher, done.state)),
                        )
                    }
                    Err(e) => {
                        report.failed += 1;
                        ("failed", Some(format!("{e:#}")))
                    }
                }
            }
        };
        report.rows.push(SheetOutcome {
            talk_id: row.talk_id,
            title: row.title,
            decision: row.decision,
            result,
            detail,
        });
    }
    print_json(&report)
}

async fn exclude_row(pool: &PgPool, talk: &str, reason: &str, by: &str) -> anyhow::Result<()> {
    let talk: TalkId = talk
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("bad talk id {talk:?}: {e}"))?;
    wt_db::consent::exclude_talk(pool, talk, reason, by)
        .await
        .with_context(|| format!("excluding talk {talk}"))?;
    Ok(())
}

pub async fn exclude(
    pool: &PgPool,
    config: &wt_core::Config,
    talk: TalkId,
    reason: &str,
    by: &str,
    yes: bool,
) -> anyhow::Result<()> {
    if !yes {
        return refuse_unconfirmed(pool, &Scope::Talk(talk), &format!("excluding talk {talk}"))
            .await;
    }
    let report = wt_db::consent::exclude_talk(pool, talk, reason, by)
        .await
        .with_context(|| format!("excluding talk {talk}"))?;
    let deleted = crate::stages::delete_talk_objects(pool, config, &[talk]).await?;
    print_json(&crate::stages::with_objects(&report, deleted))
}

pub async fn takedown(
    pool: &PgPool,
    config: &wt_core::Config,
    target: Target,
    actor: &str,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let scope = match target {
        Target::Talk(id) => Scope::Talk(id),
        Target::Teacher(name) => Scope::Teacher(name),
    };
    let store = crate::stages::objects(config)?;
    if dry_run {
        let report: TakedownReport = wt_db::takedown::dry_run_with_objects(pool, &scope, &store)
            .await
            .context("counting what the takedown would remove")?;
        return print_json(&report);
    }
    if !yes {
        return refuse_unconfirmed(pool, &scope, "the takedown").await;
    }
    let report = wt_db::takedown::complete(pool, &scope, actor, &store)
        .await
        .context("running the takedown; if only objects failed, rerun with --finish")?;
    print_json(&report)
}

/// `wt takedown --finish`: retries object deletion for every logged scope.
pub async fn takedown_finish(pool: &PgPool, config: &wt_core::Config) -> anyhow::Result<()> {
    let store = crate::stages::objects(config)?;
    let report = wt_db::takedown::finish(pool, &store)
        .await
        .context("finishing logged takedowns")?;
    print_json(&report)
}

/// `wt takedown --replay`: after a database restore, reapplies every recorded takedown.
pub async fn takedown_replay(
    pool: &PgPool,
    config: &wt_core::Config,
    actor: &str,
    yes: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        yes,
        "--replay reapplies every recorded takedown to this database; rerun with --yes"
    );
    let store = crate::stages::objects(config)?;
    let report = wt_db::takedown::replay(pool, &store, actor)
        .await
        .context("replaying recorded takedowns")?;
    print_json(&report)
}

pub async fn audit(
    pool: &PgPool,
    config: &wt_core::Config,
    config_dir: &Path,
) -> anyhow::Result<ExitCode> {
    // Without a pipeline.toml, as in a bare checkout, the tool is private.
    let pipeline = config_dir.join("pipeline.toml");
    let deployment = if pipeline.exists() {
        wt_ingest::config::Deployment::load(&pipeline)?
    } else {
        wt_ingest::config::Deployment::default()
    };
    let settings = AuditSettings {
        public: deployment.public(),
        ..AuditSettings::defaults(config.env)
    };
    let store = crate::stages::objects(config)?;
    let files = wt_db::audit::Files {
        objects: &store,
        scratch_root: &config.scratch_dir,
        scratch_ttl_h: wt_db::audit::SCRATCH_TTL_H,
    };
    let report = wt_db::audit::run_with_files(pool, settings, &files)
        .await
        .context("running the audit")?;
    print_json(&report)?;
    Ok(if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// One row of a reviewed citation-name sheet.
#[derive(Debug, serde::Deserialize)]
struct CiteRow {
    teacher: String,
    #[serde(default)]
    citation_name: String,
    #[serde(default)]
    citation_sort: String,
}

/// `wt teacher cite --sheet` (Spec 05, Answer assembly, rule 5): every row in one transaction,
/// so an unknown teacher leaves the sheet unapplied.
pub async fn teacher_cite_sheet(
    pool: &PgPool,
    sheet: &Path,
    by: &str,
) -> anyhow::Result<Vec<registry::CitationChange>> {
    let text =
        std::fs::read_to_string(sheet).with_context(|| format!("reading {}", sheet.display()))?;
    let rows = csv::Reader::from_reader(text.trim_start_matches('\u{feff}').as_bytes())
        .deserialize()
        .collect::<Result<Vec<CiteRow>, _>>()
        .with_context(|| format!("parsing {}", sheet.display()))?;
    let reason = format!("wt teacher cite --sheet {}", sheet.display());
    let mut tx = pool.begin().await?;
    let mut changes = Vec::with_capacity(rows.len());
    for row in rows {
        changes.push(
            registry::set_citation(
                &mut tx,
                &row.teacher,
                Some(&row.citation_name),
                Some(&row.citation_sort),
                by,
                &reason,
            )
            .await
            .with_context(|| format!("row for {:?}", row.teacher))?,
        );
    }
    tx.commit().await?;
    Ok(changes)
}
