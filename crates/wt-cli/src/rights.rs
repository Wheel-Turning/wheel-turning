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
            basis,
        },
    )
    .await
    .with_context(|| format!("adding source {}", args.key))?;
    print_json(&serde_json::json!({
        "source": args.key,
        "basis": args.basis,
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
    let id = rights::request(pool, key, to, channel, &stored, by).await?;
    print_json(&serde_json::json!({ "source": key, "request": id, "draft": stored.key }))
}

/// `wt rights sent`.
pub async fn sent(pool: &PgPool, key: &str, on: NaiveDate, by: &str) -> anyhow::Result<()> {
    let follow_up = rights::sent(pool, key, on, by).await?;
    print_json(&serde_json::json!({ "source": key, "sent_on": on, "follow_up_on": follow_up }))
}

/// `wt rights grant`.
#[allow(clippy::too_many_arguments)]
pub async fn grant(
    pool: &PgPool,
    records: &LocalRecords,
    key: &str,
    statement: &Path,
    from: &str,
    given_on: NaiveDate,
    scope: &str,
    by: &str,
) -> anyhow::Result<()> {
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
    print_json(&serde_json::json!({
        "source": key,
        "enabled": false,
        "exclusions": out.exclusions,
        "statement": stored.key,
    }))
}

/// `wt rights no-response`.
pub async fn no_response(pool: &PgPool, key: &str, by: &str) -> anyhow::Result<()> {
    rights::no_response(pool, key, by).await?;
    print_json(&serde_json::json!({ "source": key, "basis": "mandate", "noted": "no_response" }))
}

/// `wt rights ledger`.
pub async fn ledger(pool: &PgPool, key: Option<&str>) -> anyhow::Result<()> {
    print_json(&rights::ledger(pool, key).await?)
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
