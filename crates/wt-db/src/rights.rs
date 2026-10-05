//! The source policy and the rights ledger (Spec 08, Mandated sources and Rights lifecycle):
//! sources added on the owner's mandate, permission requests and their answers, the append-only
//! `rights_event` history, and permanent exclusions.
//!
//! Every write that changes a source's standing appends a `rights_event` in the same
//! transaction. Events take `clock_timestamp()`, not `now()`, so several written in one
//! transaction still read back in the order they happened.

use chrono::{DateTime, Days, NaiveDate, Utc};
use sqlx::types::Uuid;
use sqlx::{PgConnection, PgPool};
use wt_core::records::StoredRecord;

use crate::DbError;

/// Days from a request going out to its follow-up (Spec 08, the ledger).
pub const FOLLOW_UP_DAYS: u64 = 14;

/// The evidence code of a mandate.
pub const MANDATE_CODE: &str = "operator-mandate";

/// The evidence code of a recorded permission.
pub const PERMISSION_CODE: &str = "permission-excerpt";

/// Rights ledger errors.
#[derive(Debug, thiserror::Error)]
pub enum RightsError {
    /// No source has that key.
    #[error("no source with key {0:?}")]
    UnknownSource(String),
    /// A source with that key already exists.
    #[error("a source with key {0:?} already exists")]
    SourceExists(String),
    /// The source needs at least one URL prefix, each an http or https address.
    #[error("{0}")]
    BadPrefix(String),
    /// An active exclusion covers what was asked for. It is cited so a person can see why.
    #[error("refused by exclusion {}: {} {:?} ({})", .0.id, .0.scope, .0.pattern, .0.reason)]
    Excluded(Exclusion),
    /// The source has no request in the state the command needs.
    #[error("source {key:?} has no {wanted} permission request")]
    NoRequest {
        /// The source.
        key: String,
        /// The request state looked for.
        wanted: &'static str,
    },
    /// No exclusion has that id, or it was already lifted.
    #[error("no active exclusion with id {0}")]
    UnknownExclusion(Uuid),
    /// The license or permission evidence for a non-mandate source is missing or not approved.
    #[error("evidence {0} is not approved {1} evidence")]
    EvidenceNotUsable(Uuid, &'static str),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for RightsError {
    fn from(e: sqlx::Error) -> Self {
        RightsError::Db(DbError::Sqlx(e))
    }
}

/// The basis a source is used on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// A license the source states, reviewed and approved by a person.
    License,
    /// The rights holder's written yes.
    Permission,
    /// The owner's recorded decision to rely on the source. Never a license or a permission.
    Mandate,
}

impl Basis {
    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::License => "license",
            Basis::Permission => "permission",
            Basis::Mandate => "mandate",
        }
    }

    /// Parses a stored or typed value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "license" => Some(Basis::License),
            "permission" => Some(Basis::Permission),
            "mandate" => Some(Basis::Mandate),
            _ => None,
        }
    }
}

/// How a new source is used.
#[derive(Debug, Clone)]
pub enum NewBasis<'a> {
    /// On the owner's mandate: the decision is recorded as mandate evidence.
    Mandate {
        /// Who decided.
        by: &'a str,
        /// Why, in their words.
        rationale: &'a str,
        /// The attribution line shown with an excerpt.
        display_notice: &'a str,
        /// The stored record of the decision.
        decision: &'a StoredRecord,
    },
    /// On license or permission evidence already captured and approved.
    Evidence {
        /// `License` or `Permission`.
        basis: Basis,
        /// The approved evidence.
        evidence: Uuid,
    },
}

/// `wt source add`: what to add.
#[derive(Debug, Clone)]
pub struct NewSource<'a> {
    /// Short key, such as `bdkamerica`.
    pub key: &'a str,
    /// The publisher or rights holder's name.
    pub publisher: &'a str,
    /// The URL prefixes the source covers.
    pub prefixes: &'a [String],
    /// How it is used.
    pub basis: NewBasis<'a>,
}

/// An exclusion row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Exclusion {
    /// Its id, cited when it refuses something.
    pub id: Uuid,
    /// `host`, `prefix`, `work`, `edition`, `person` or `content`.
    pub scope: String,
    /// What it matches.
    pub pattern: String,
    /// Why it exists.
    pub reason: String,
    /// The source it came from, if any.
    pub source_key: Option<String>,
}

/// One ledger entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LedgerEntry {
    /// When it happened.
    pub at: DateTime<Utc>,
    /// The source.
    pub source_key: String,
    /// What happened.
    pub event: String,
    /// Who did it.
    pub actor: String,
    /// Detail in words.
    pub detail: Option<String>,
    /// The stored request, reply or statement.
    pub evidence_ref: Option<String>,
}

/// A permission request that needs action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DueRequest {
    /// The source.
    pub source_key: String,
    /// Who it goes to.
    pub recipient: String,
    /// `draft` or `sent`.
    pub status: String,
    /// When it went out.
    pub sent_on: Option<NaiveDate>,
    /// When to follow up.
    pub follow_up_on: Option<NaiveDate>,
    /// `unsent` or `follow_up_due`.
    pub why: &'static str,
}

/// What `wt rights decline` did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Declined {
    /// Exclusions added, one per URL prefix.
    pub exclusions: Vec<Uuid>,
}

/// The URL with its scheme and host lowercased, which is how prefixes compare.
pub fn normalize_url(url: &str) -> String {
    let url = url.trim();
    match url.find("://") {
        Some(at) => {
            let rest = &url[at + 3..];
            let host_end = rest.find('/').unwrap_or(rest.len());
            format!(
                "{}://{}{}",
                url[..at].to_ascii_lowercase(),
                rest[..host_end].to_ascii_lowercase(),
                &rest[host_end..]
            )
        }
        None => url.to_owned(),
    }
}

/// The host of an http or https URL, lowercased and without a port.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn host_matches(host: &str, pattern: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    host == pattern || host.ends_with(&format!(".{pattern}"))
}

/// The first active exclusion that covers `url`: its host, or a prefix it falls under.
pub fn exclusion_for_url<'a>(exclusions: &'a [Exclusion], url: &str) -> Option<&'a Exclusion> {
    let url = normalize_url(url);
    let host = host_of(&url);
    exclusions.iter().find(|x| match x.scope.as_str() {
        "host" => host.as_deref().is_some_and(|h| host_matches(h, &x.pattern)),
        "prefix" => url.starts_with(&normalize_url(&x.pattern)),
        _ => false,
    })
}

/// The first active exclusion a new source prefix would run into: one covering it, or one
/// inside the area it would cover.
pub fn exclusion_for_prefix<'a>(
    exclusions: &'a [Exclusion],
    prefix: &str,
) -> Option<&'a Exclusion> {
    let prefix = normalize_url(prefix);
    exclusion_for_url(exclusions, &prefix).or_else(|| {
        exclusions
            .iter()
            .find(|x| x.scope == "prefix" && normalize_url(&x.pattern).starts_with(&prefix))
    })
}

/// Every active exclusion.
pub async fn active_exclusions(conn: &mut PgConnection) -> Result<Vec<Exclusion>, RightsError> {
    let rows = sqlx::query!(
        "select id, scope, pattern, reason, source_key from source_exclusion
         where lifted_at is null order by created_at, id"
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Exclusion {
            id: r.id,
            scope: r.scope,
            pattern: r.pattern,
            reason: r.reason,
            source_key: r.source_key,
        })
        .collect())
}

async fn event(
    conn: &mut PgConnection,
    key: &str,
    event: &str,
    actor: &str,
    detail: Option<&str>,
    evidence_ref: Option<&str>,
) -> Result<(), RightsError> {
    sqlx::query!(
        "insert into rights_event (source_key, event, actor, detail, evidence_ref, created_at)
         values ($1, $2, $3, $4, $5, clock_timestamp())",
        key,
        event,
        actor,
        detail,
        evidence_ref
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn source_prefixes(conn: &mut PgConnection, key: &str) -> Result<Vec<String>, RightsError> {
    sqlx::query_scalar!(
        "select url_prefixes from source_policy where key = $1 for update",
        key
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| RightsError::UnknownSource(key.to_owned()))
}

/// `wt source add` (and `wt source mandate`): records the source, its evidence and, for a
/// mandate, a `mandated` event. Refused if an active exclusion covers any prefix.
pub async fn add_source(pool: &PgPool, new: &NewSource<'_>) -> Result<Uuid, RightsError> {
    if new.prefixes.is_empty() {
        return Err(RightsError::BadPrefix(
            "a source needs at least one --prefix".into(),
        ));
    }
    let prefixes: Vec<String> = new.prefixes.iter().map(|p| normalize_url(p)).collect();
    if let Some(bad) = prefixes
        .iter()
        .find(|p| !(p.starts_with("https://") || p.starts_with("http://")) || host_of(p).is_none())
    {
        return Err(RightsError::BadPrefix(format!(
            "{bad:?} is not an http or https address"
        )));
    }

    let mut tx = pool.begin().await?;
    let exclusions = active_exclusions(&mut tx).await?;
    for prefix in &prefixes {
        if let Some(x) = exclusion_for_prefix(&exclusions, prefix) {
            return Err(RightsError::Excluded(x.clone()));
        }
    }
    let exists = sqlx::query_scalar!(
        r#"select exists (select 1 from source_policy where key = $1) as "e!""#,
        new.key
    )
    .fetch_one(&mut *tx)
    .await?;
    if exists {
        return Err(RightsError::SourceExists(new.key.to_owned()));
    }

    let (basis, evidence, actor) = match &new.basis {
        NewBasis::Mandate {
            by,
            rationale,
            display_notice,
            decision,
        } => {
            let id = sqlx::query_scalar!(
                "insert into license_evidence
                   (publisher, license_code, evidence_snapshot_ref, snapshot_sha256, scope_note,
                    captured_at, reviewed_by, reviewed_at, allowed, kind, url_prefixes,
                    mandated_by, rationale, display_notice)
                 values ($1, $2, $3, $4, 'the owner''s decision to use this source',
                         now(), $5, now(), true, 'mandate', $6, $5, $7, $8)
                 returning id",
                new.publisher,
                MANDATE_CODE,
                decision.key,
                decision.sha256,
                by,
                &prefixes,
                rationale,
                display_notice
            )
            .fetch_one(&mut *tx)
            .await?;
            (Basis::Mandate, id, *by)
        }
        NewBasis::Evidence { basis, evidence } => {
            let usable = sqlx::query_scalar!(
                r#"select exists (select 1 from license_evidence
                                  where id = $1 and kind = $2 and allowed) as "u!""#,
                evidence,
                basis.as_str()
            )
            .fetch_one(&mut *tx)
            .await?;
            if !usable || *basis == Basis::Mandate {
                return Err(RightsError::EvidenceNotUsable(*evidence, basis.as_str()));
            }
            (*basis, *evidence, "")
        }
    };
    sqlx::query!(
        "insert into source_policy (key, publisher, url_prefixes, basis, license_evidence_id)
         values ($1, $2, $3, $4, $5)",
        new.key,
        new.publisher,
        &prefixes,
        basis.as_str(),
        evidence
    )
    .execute(&mut *tx)
    .await?;
    match &new.basis {
        NewBasis::Mandate { decision, .. } => {
            event(
                &mut tx,
                new.key,
                "mandated",
                actor,
                Some(&format!(
                    "used on the owner's decision: {}",
                    prefixes.join(" ")
                )),
                Some(&decision.key),
            )
            .await?;
        }
        NewBasis::Evidence { basis, evidence } => {
            event(
                &mut tx,
                new.key,
                "enabled",
                "wt source add",
                Some(&format!("added on {} evidence {evidence}", basis.as_str())),
                None,
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(evidence)
}

/// `wt source disable` and `enable`. The serving views read the flag, so the effect is
/// immediate. Returns whether anything changed.
pub async fn set_enabled(
    pool: &PgPool,
    key: &str,
    enabled: bool,
    reason: Option<&str>,
    actor: &str,
) -> Result<bool, RightsError> {
    let mut tx = pool.begin().await?;
    let changed = set_enabled_in(&mut tx, key, enabled, reason, actor).await?;
    tx.commit().await?;
    Ok(changed)
}

async fn set_enabled_in(
    conn: &mut PgConnection,
    key: &str,
    enabled: bool,
    reason: Option<&str>,
    actor: &str,
) -> Result<bool, RightsError> {
    source_prefixes(conn, key).await?;
    let changed = sqlx::query!(
        "update source_policy
         set enabled = $2,
             disabled_reason = case when $2 then null else $3 end,
             disabled_at = case when $2 then null else now() end
         where key = $1 and enabled <> $2",
        key,
        enabled,
        reason
    )
    .execute(&mut *conn)
    .await?
    .rows_affected()
        > 0;
    if changed {
        let name = if enabled { "enabled" } else { "disabled" };
        event(conn, key, name, actor, reason, None).await?;
    }
    Ok(changed)
}

/// `wt rights request`: records a drafted permission request.
pub async fn request(
    pool: &PgPool,
    key: &str,
    recipient: &str,
    channel: &str,
    draft: &StoredRecord,
    actor: &str,
) -> Result<Uuid, RightsError> {
    let mut tx = pool.begin().await?;
    source_prefixes(&mut tx, key).await?;
    let id = sqlx::query_scalar!(
        "insert into permission_request (source_key, recipient, channel, draft_ref)
         values ($1, $2, $3, $4) returning id",
        key,
        recipient,
        channel,
        draft.key
    )
    .fetch_one(&mut *tx)
    .await?;
    event(
        &mut tx,
        key,
        "request_drafted",
        actor,
        Some(&format!("to {recipient} by {channel}")),
        Some(&draft.key),
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// `wt rights sent`: the latest drafted request went out on `on`. Returns the follow-up date,
/// `FOLLOW_UP_DAYS` later.
pub async fn sent(
    pool: &PgPool,
    key: &str,
    on: NaiveDate,
    actor: &str,
) -> Result<NaiveDate, RightsError> {
    let follow_up = on
        .checked_add_days(Days::new(FOLLOW_UP_DAYS))
        .unwrap_or(NaiveDate::MAX);
    let mut tx = pool.begin().await?;
    source_prefixes(&mut tx, key).await?;
    let row = sqlx::query!(
        "update permission_request set status = 'sent', sent_on = $2, follow_up_on = $3
         where id = (select id from permission_request
                     where source_key = $1 and status = 'draft'
                     order by created_at desc, id desc limit 1)
         returning recipient",
        key,
        on,
        follow_up
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| RightsError::NoRequest {
        key: key.to_owned(),
        wanted: "drafted",
    })?;
    event(
        &mut tx,
        key,
        "request_sent",
        actor,
        Some(&format!(
            "sent to {} on {on}; follow up on {follow_up}",
            row.recipient
        )),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(follow_up)
}

/// Marks the latest sent request with the answer and returns whether one existed. An answer
/// that arrives without a recorded request is still recorded in the ledger.
async fn answer_request(
    conn: &mut PgConnection,
    key: &str,
    status: &str,
    reply: &StoredRecord,
) -> Result<bool, RightsError> {
    Ok(sqlx::query!(
        "update permission_request set status = $2, reply_ref = $3
         where id = (select id from permission_request
                     where source_key = $1 and status in ('draft','sent','replied','no_response')
                     order by created_at desc, id desc limit 1)",
        key,
        status,
        reply.key
    )
    .execute(&mut *conn)
    .await?
    .rows_affected()
        > 0)
}

/// `wt rights grant`: stores the reply as approved permission evidence for the source's
/// prefixes and switches the source's basis to `permission`. Returns the new evidence id.
#[allow(clippy::too_many_arguments)]
pub async fn grant(
    pool: &PgPool,
    key: &str,
    statement: &StoredRecord,
    from: &str,
    given_on: NaiveDate,
    scope: &str,
    actor: &str,
) -> Result<Uuid, RightsError> {
    let mut tx = pool.begin().await?;
    let prefixes = source_prefixes(&mut tx, key).await?;
    let publisher = sqlx::query_scalar!("select publisher from source_policy where key = $1", key)
        .fetch_one(&mut *tx)
        .await?;
    let evidence = sqlx::query_scalar!(
        "insert into license_evidence
           (publisher, license_code, evidence_snapshot_ref, snapshot_sha256, scope_note,
            captured_at, reviewed_by, reviewed_at, allowed, kind, grantor, url_prefixes)
         values ($1, $2, $3, $4, $5, $6::date::timestamptz, $7, now(), true, 'permission', $8, $9)
         returning id",
        publisher,
        PERMISSION_CODE,
        statement.key,
        statement.sha256,
        scope,
        given_on,
        actor,
        from,
        &prefixes
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "update source_policy set basis = 'permission', license_evidence_id = $2 where key = $1",
        key,
        evidence
    )
    .execute(&mut *tx)
    .await?;
    answer_request(&mut tx, key, "granted", statement).await?;
    event(
        &mut tx,
        key,
        "permission_granted",
        actor,
        Some(&format!("from {from}, given on {given_on}: {scope}")),
        Some(&statement.key),
    )
    .await?;
    tx.commit().await?;
    Ok(evidence)
}

/// `wt rights decline`: logs the refusal, switches the source off and excludes each of its
/// prefixes permanently. The takedown of the source's stored material runs with it once
/// talks and texts carry their source (Spec 03, Mandated talks; Spec 08, texts).
pub async fn decline(
    pool: &PgPool,
    key: &str,
    statement: &StoredRecord,
    actor: &str,
) -> Result<Declined, RightsError> {
    let mut tx = pool.begin().await?;
    let prefixes = source_prefixes(&mut tx, key).await?;
    answer_request(&mut tx, key, "declined", statement).await?;
    event(
        &mut tx,
        key,
        "permission_declined",
        actor,
        None,
        Some(&statement.key),
    )
    .await?;
    set_enabled_in(&mut tx, key, false, Some("permission declined"), actor).await?;
    let mut out = Declined::default();
    for prefix in prefixes {
        let id = add_exclusion_in(
            &mut tx,
            "prefix",
            &prefix,
            "the rights holder declined",
            Some(key),
            actor,
        )
        .await?;
        out.exclusions.push(id);
    }
    tx.commit().await?;
    Ok(out)
}

/// `wt rights no-response`: logs that a request went unanswered. Silence is never permission,
/// so the source stays on its mandate and is labeled as one.
pub async fn no_response(pool: &PgPool, key: &str, actor: &str) -> Result<(), RightsError> {
    let mut tx = pool.begin().await?;
    source_prefixes(&mut tx, key).await?;
    let marked = sqlx::query!(
        "update permission_request set status = 'no_response'
         where id = (select id from permission_request
                     where source_key = $1 and status = 'sent'
                     order by created_at desc, id desc limit 1)",
        key
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if marked == 0 {
        return Err(RightsError::NoRequest {
            key: key.to_owned(),
            wanted: "sent",
        });
    }
    event(&mut tx, key, "no_response_noted", actor, None, None).await?;
    tx.commit().await?;
    Ok(())
}

async fn add_exclusion_in(
    conn: &mut PgConnection,
    scope: &str,
    pattern: &str,
    reason: &str,
    source_key: Option<&str>,
    actor: &str,
) -> Result<Uuid, RightsError> {
    let id = sqlx::query_scalar!(
        "insert into source_exclusion (scope, pattern, reason, source_key, created_by)
         values ($1, $2, $3, $4, $5) returning id",
        scope,
        pattern,
        reason,
        source_key,
        actor
    )
    .fetch_one(&mut *conn)
    .await?;
    if let Some(key) = source_key {
        event(
            conn,
            key,
            "exclusion_added",
            actor,
            Some(&format!("{scope} {pattern}: {reason} (exclusion {id})")),
            None,
        )
        .await?;
    }
    Ok(id)
}

/// Adds a permanent exclusion. Takedowns, declines and opt-outs call this.
pub async fn add_exclusion(
    conn: &mut PgConnection,
    scope: &str,
    pattern: &str,
    reason: &str,
    source_key: Option<&str>,
    actor: &str,
) -> Result<Uuid, RightsError> {
    add_exclusion_in(conn, scope, pattern, reason, source_key, actor).await
}

/// `wt exclusion lift`: lifts an exclusion on the rights holder's written grant, which is
/// stored and cited. There is no way to lift one without it.
pub async fn lift_exclusion(
    pool: &PgPool,
    id: Uuid,
    by: &str,
    evidence: &StoredRecord,
) -> Result<Exclusion, RightsError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query!(
        "update source_exclusion set lifted_at = now(), lifted_by = $2, lift_evidence_ref = $3
         where id = $1 and lifted_at is null
         returning scope, pattern, reason, source_key",
        id,
        by,
        evidence.key
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(RightsError::UnknownExclusion(id))?;
    if let Some(key) = &row.source_key {
        event(
            &mut tx,
            key,
            "exclusion_lifted",
            by,
            Some(&format!("{} {} (exclusion {id})", row.scope, row.pattern)),
            Some(&evidence.key),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Exclusion {
        id,
        scope: row.scope,
        pattern: row.pattern,
        reason: row.reason,
        source_key: row.source_key,
    })
}

/// `wt rights ledger`: every event, or one source's, in the order they happened.
pub async fn ledger(pool: &PgPool, key: Option<&str>) -> Result<Vec<LedgerEntry>, RightsError> {
    let rows = sqlx::query!(
        "select created_at, source_key, event, actor, detail, evidence_ref from rights_event
         where $1::text is null or source_key = $1
         order by created_at, id",
        key
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| LedgerEntry {
            at: r.created_at,
            source_key: r.source_key,
            event: r.event,
            actor: r.actor,
            detail: r.detail,
            evidence_ref: r.evidence_ref,
        })
        .collect())
}

/// `wt rights due`: requests not yet sent, and sent requests at or past their follow-up date.
pub async fn due(pool: &PgPool, today: NaiveDate) -> Result<Vec<DueRequest>, RightsError> {
    let rows = sqlx::query!(
        "select source_key, recipient, status, sent_on, follow_up_on from permission_request
         where status = 'draft' or (status = 'sent' and follow_up_on <= $1)
         order by coalesce(follow_up_on, created_at::date), source_key",
        today
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DueRequest {
            why: if r.status == "draft" {
                "unsent"
            } else {
                "follow_up_due"
            },
            source_key: r.source_key,
            recipient: r.recipient,
            status: r.status,
            sent_on: r.sent_on,
            follow_up_on: r.follow_up_on,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(scope: &str, pattern: &str) -> Exclusion {
        Exclusion {
            id: Uuid::nil(),
            scope: scope.into(),
            pattern: pattern.into(),
            reason: "test".into(),
            source_key: None,
        }
    }

    #[test]
    fn prefixes_and_hosts_cover_what_they_should() {
        let rules = [
            rule("prefix", "https://Example.org/zen/"),
            rule("host", "blocked.example"),
        ];
        assert!(exclusion_for_url(&rules, "https://example.org/zen/a.pdf").is_some());
        assert!(exclusion_for_url(&rules, "HTTPS://EXAMPLE.ORG/zen/b").is_some());
        assert!(exclusion_for_url(&rules, "https://example.org/other").is_none());
        assert!(exclusion_for_url(&rules, "http://www.blocked.example:8080/x").is_some());
        assert!(exclusion_for_url(&rules, "https://notblocked.example/").is_none());
        // A new source covering the excluded area is refused too.
        assert!(exclusion_for_prefix(&rules, "https://example.org/").is_some());
        assert!(exclusion_for_prefix(&rules, "https://example.org/texts/").is_none());
        assert_eq!(
            host_of("https://user@Host.Example:443/p?q").as_deref(),
            Some("host.example")
        );
        assert_eq!(host_of("not a url"), None);
    }
}
