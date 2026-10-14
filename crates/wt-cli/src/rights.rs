//! `wt source add|disable|enable`, `wt rights …` and `wt exclusion …` (Spec 08, Mandated
//! sources and Rights lifecycle). Statements, drafts and decisions are stored as records first,
//! and the ledger cites their keys.

use std::path::Path;

use anyhow::Context;
use chrono::NaiveDate;
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::records::LocalRecords;
use wt_db::rights::{self, Basis, NewBasis, NewSource};

use crate::commands::print_json;

/// How `wt source add` uses a source.
pub struct AddArgs<'a> {
    pub key: &'a str,
    pub publisher: &'a str,
    pub prefixes: &'a [String],
    pub basis: Basis,
    pub kind: &'a str,
    pub evidence: Option<Uuid>,
    pub by: &'a str,
    pub rationale: Option<&'a str>,
    pub display_notice: Option<&'a str>,
}

/// `wt source add`: on a mandate unless `--basis` names license or permission evidence.
pub async fn add(pool: &PgPool, records: &LocalRecords, args: AddArgs<'_>) -> anyhow::Result<()> {
    let decision;
    let basis = match args.basis {
        Basis::Mandate => {
            let rationale = args
                .rationale
                .context("a mandate needs --rationale: why the owner relies on this source")?;
            let display_notice = args
                .display_notice
                .context("a mandate needs --display-notice: the attribution line on excerpts")?;
            // The decision itself is the record a mandate's evidence points to.
            let text = serde_json::to_string_pretty(&serde_json::json!({
                "decision": "use this source on the owner's mandate",
                "key": args.key,
                "publisher": args.publisher,
                "prefixes": args.prefixes,
                "by": args.by,
                "rationale": rationale,
                "display_notice": display_notice,
            }))?;
            decision = records.store(text.as_bytes())?;
            NewBasis::Mandate {
                by: args.by,
                rationale,
                display_notice,
                decision: &decision,
            }
        }
        basis => NewBasis::Evidence {
            basis,
            evidence: args
                .evidence
                .context("a license or permission source needs --evidence <id>")?,
        },
    };
    let evidence = rights::add_source(
        pool,
        &NewSource {
            key: args.key,
            publisher: args.publisher,
            prefixes: args.prefixes,
            kind: args.kind,
            basis,
        },
    )
    .await
    .with_context(|| format!("adding source {}", args.key))?;
    print_json(&serde_json::json!({
        "source": args.key,
        "basis": args.basis,
        "kind": args.kind,
        "evidence": evidence,
    }))
}

/// `wt source disable` and `enable`.
pub async fn set_enabled(
    pool: &PgPool,
    key: &str,
    enabled: bool,
    reason: Option<&str>,
    by: &str,
) -> anyhow::Result<()> {
    let changed = rights::set_enabled(pool, key, enabled, reason, by).await?;
    print_json(&serde_json::json!({ "source": key, "enabled": enabled, "changed": changed }))
}

/// `wt rights request`.
pub async fn request(
    pool: &PgPool,
    records: &LocalRecords,
    key: &str,
    to: &str,
    channel: &str,
    draft: &Path,
    by: &str,
) -> anyhow::Result<()> {
    let stored = records.store_file(draft)?;
    let id = rights::request(pool, rights::Subject::Source(key), to, channel, &stored, by).await?;
    print_json(&serde_json::json!({ "source": key, "request": id, "draft": stored.key }))
}

/// A source or, failing that, a teacher; clap makes exactly one present.
fn subject<'a>(key: Option<&'a str>, teacher: Option<&'a str>) -> rights::Subject<'a> {
    match key {
        Some(key) => rights::Subject::Source(key),
        None => rights::Subject::Teacher(teacher.unwrap_or_default()),
    }
}

/// `wt rights sent`.
pub async fn sent(
    pool: &PgPool,
    key: Option<&str>,
    teacher: Option<&str>,
    on: NaiveDate,
    by: &str,
) -> anyhow::Result<()> {
    let follow_up = rights::sent(pool, subject(key, teacher), on, by).await?;
    print_json(
        &serde_json::json!({ "source": key, "teacher": teacher, "sent_on": on, "follow_up_on": follow_up }),
    )
}

/// A source's permission, as `wt rights grant` records it.
pub struct GrantArgs<'a> {
    /// The source.
    pub key: &'a str,
    /// The reply granting permission, stored as a record by its SHA-256.
    pub statement: &'a Path,
    /// Who gave it.
    pub from: &'a str,
    /// When.
    pub given_on: NaiveDate,
    /// What it covers, in the grantor's words.
    pub scope: &'a str,
    /// Who recorded it.
    pub by: &'a str,
}

/// `wt rights grant`.
pub async fn grant(
    pool: &PgPool,
    records: &LocalRecords,
    args: GrantArgs<'_>,
) -> anyhow::Result<()> {
    let GrantArgs {
        key,
        statement,
        from,
        given_on,
        scope,
        by,
    } = args;
    let stored = records.store_file(statement)?;
    let evidence = rights::grant(pool, key, &stored, from, given_on, scope, by).await?;
    print_json(&serde_json::json!({
        "source": key,
        "basis": "permission",
        "evidence": evidence,
        "statement": stored.key,
    }))
}

/// `wt rights decline`. It disables the source and excludes it permanently, so it needs --yes.
pub async fn decline(
    pool: &PgPool,
    config: &wt_core::Config,
    records: &LocalRecords,
    key: &str,
    statement: &Path,
    by: &str,
    yes: bool,
) -> anyhow::Result<()> {
    if !yes {
        anyhow::bail!(
            "a decline switches {key} off and excludes its prefixes permanently; rerun with --yes"
        );
    }
    let stored = records.store_file(statement)?;
    let out = rights::decline(pool, key, &stored, by).await?;
    // A texts source loses its editions too (Spec 08, Rights lifecycle).
    let objects = crate::stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    let texts = wt_ingest::texts::takedown::take_down(
        &mut conn,
        &objects,
        &wt_db::text_takedown::TextScope::Source(key.to_owned()),
        "permission declined",
        by,
        false,
    )
    .await?;
    print_json(&serde_json::json!({
        "source": key,
        "enabled": false,
        "exclusions": out.exclusions,
        "statement": stored.key,
        "texts": texts,
    }))
}

/// `wt rights no-response`.
pub async fn no_response(
    pool: &PgPool,
    key: Option<&str>,
    teacher: Option<&str>,
    by: &str,
) -> anyhow::Result<()> {
    rights::no_response(pool, subject(key, teacher), by).await?;
    print_json(&serde_json::json!({ "source": key, "teacher": teacher, "noted": "no_response" }))
}

/// `wt rights ledger`.
pub async fn ledger(
    pool: &PgPool,
    source: Option<&str>,
    teacher: Option<&str>,
) -> anyhow::Result<()> {
    let subject = match (source, teacher) {
        (Some(key), _) => Some(rights::Subject::Source(key)),
        (None, Some(name)) => Some(rights::Subject::Teacher(name)),
        (None, None) => None,
    };
    print_json(&rights::ledger(pool, subject).await?)
}

/// `wt rights due`.
pub async fn due(pool: &PgPool, today: NaiveDate) -> anyhow::Result<()> {
    print_json(&rights::due(pool, today).await?)
}

/// `wt exclusion list`.
pub async fn exclusions(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    print_json(&rights::active_exclusions(&mut conn).await?)
}

/// `wt exclusion lift`: needs the rights holder's written grant, which is stored and cited.
pub async fn lift(
    pool: &PgPool,
    records: &LocalRecords,
    id: Uuid,
    by: &str,
    evidence: &Path,
) -> anyhow::Result<()> {
    let stored = records.store_file(evidence)?;
    let lifted = rights::lift_exclusion(pool, id, by, &stored).await?;
    print_json(&serde_json::json!({ "lifted": lifted, "evidence": stored.key }))
}

/// `wt rights set-talks-basis`.
pub async fn set_talks_basis(pool: &PgPool, basis: &str, by: &str) -> anyhow::Result<()> {
    print_json(&rights::set_talks_basis(pool, basis, by).await?)
}

/// `wt teacher opt-out`. The takedown and exclusions need --yes; --suspend-only does not.
pub async fn opt_out(
    pool: &PgPool,
    config: &wt_core::Config,
    teacher: &str,
    reason: &str,
    by: &str,
    suspend_only: bool,
    yes: bool,
) -> anyhow::Result<()> {
    if !suspend_only && !yes {
        anyhow::bail!(
            "an opt-out deletes {teacher}'s passages, segments and transcripts and excludes \
             them for good; rerun with --yes, or with --suspend-only to stop serving only"
        );
    }
    let out = rights::opt_out(pool, teacher, reason, by, suspend_only).await?;
    let talks = out
        .takedown
        .as_ref()
        .map(|t| t.talks.clone())
        .unwrap_or_default();
    let deleted = crate::stages::delete_talk_objects(pool, config, &talks).await?;
    print_json(&crate::stages::with_objects(&out, deleted))
}

/// What `wt rights notice-draft` needs.
pub struct NoticeArgs<'a> {
    pub teacher: &'a str,
    pub to: &'a str,
    pub channel: &'a str,
    pub out: &'a Path,
    pub from_name: Option<&'a str>,
    pub contact: Option<&'a str>,
    pub template: &'a Path,
    pub by: &'a str,
}

/// `wt rights notice-draft`: fills the template with the teacher's facts, writes it to --out,
/// and records it as a drafted request. It sends nothing.
pub async fn notice_draft(
    pool: &PgPool,
    records: &LocalRecords,
    args: NoticeArgs<'_>,
) -> anyhow::Result<()> {
    let template = std::fs::read_to_string(args.template)
        .with_context(|| format!("reading {}", args.template.display()))?;
    let facts = rights::notice_facts(pool, args.teacher).await?;
    let text = fill_notice(&template, &facts, args.from_name, args.contact);
    std::fs::write(args.out, &text).with_context(|| format!("writing {}", args.out.display()))?;
    let stored = records.store(text.as_bytes())?;
    let id = rights::request(
        pool,
        rights::Subject::Teacher(&facts.teacher),
        args.to,
        args.channel,
        &stored,
        args.by,
    )
    .await?;
    print_json(&serde_json::json!({
        "teacher": facts.teacher,
        "talks": facts.talks,
        "request": id,
        "draft": stored.key,
        "out": args.out,
        "sent": false,
    }))
}

/// Fills the template's placeholders. A placeholder with nothing to fill stays as it is, for
/// the sender to complete by hand.
pub fn fill_notice(
    template: &str,
    facts: &rights::NoticeFacts,
    from_name: Option<&str>,
    contact: Option<&str>,
) -> String {
    let mut text = template
        .replace("[Teacher name]", &facts.teacher)
        .replace("[N]", &facts.talks.to_string());
    if !facts.sanghas.is_empty() {
        text = text.replace("[Sangha or source]", &facts.sanghas.join(" and "));
    }
    if let Some(name) = from_name {
        text = text.replace("[Your name]", name);
    }
    if let Some(contact) = contact {
        text = text.replace("[How to reach you]", contact);
    }
    text
}
