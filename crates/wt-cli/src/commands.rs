//! Handlers for the registry, consent, talk, takedown and audit commands. Each prints its result
//! to stdout as one JSON document, except `consent list`, which prints a table for people.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, bail};
use serde::Serialize;
use sqlx::PgPool;
use wt_core::Env;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_db::audit::AuditSettings;
use wt_db::consent::{Holder, RightsTarget};
use wt_db::takedown::{Scope, TakedownReport};
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

pub async fn consent_set(
    pool: &PgPool,
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
    print_json(&propagation)
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

pub async fn exclude(
    pool: &PgPool,
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
    print_json(&report)
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
    if dry_run {
        let report: TakedownReport = wt_db::takedown::dry_run(pool, &scope)
            .await
            .context("counting what the takedown would remove")?;
        return print_json(&report);
    }
    if !yes {
        return refuse_unconfirmed(pool, &scope, "the takedown").await;
    }
    let report = wt_db::takedown::run(pool, &scope, actor)
        .await
        .context("running the takedown")?;
    let deleted = crate::stages::delete_talk_objects(config, &report.talks)?;
    print_json(&crate::stages::with_objects(&report, deleted))
}

pub async fn audit(pool: &PgPool, env: Env) -> anyhow::Result<ExitCode> {
    let report = wt_db::audit::run(pool, AuditSettings::defaults(env))
        .await
        .context("running the audit")?;
    print_json(&report)?;
    Ok(if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
