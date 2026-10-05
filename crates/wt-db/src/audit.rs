//! `dg audit` (Spec 07): integrity checks over the live database. Checks marked `Fail` make the
//! command exit nonzero when they have findings; `Warn` checks are listed only.
//!
//! M2 runs every check that reads only the database. The parts that read the scratch directory
//! or object storage (audio residue, transcript JSON speakers, takedown objects) join when the
//! transcript pipeline writes those stores in M4, and are listed as not yet checked.

use serde::Serialize;
use sqlx::PgPool;
use wt_core::Env;
use wt_core::stores::{OnTakedown, STORES, StoreKind};

use crate::DbError;

/// Whether a check's findings fail the audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Findings make `dg audit` exit nonzero.
    Fail,
    /// Findings are listed and do not change the exit code.
    Warn,
}

/// One check's result.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// Check name, as in Spec 07.
    pub name: &'static str,
    /// Its severity.
    pub severity: Severity,
    /// Number of findings.
    pub findings: i64,
    /// Up to 20 example findings.
    pub examples: Vec<String>,
}

/// The whole report.
#[derive(Debug, Clone, Serialize)]
pub struct AuditReport {
    /// Every check that ran.
    pub checks: Vec<CheckResult>,
    /// Checks this build cannot run yet, with the reason.
    pub not_yet_checked: Vec<(&'static str, &'static str)>,
}

impl AuditReport {
    /// True when any `Fail` check has findings.
    pub fn failed(&self) -> bool {
        self.checks
            .iter()
            .any(|c| c.severity == Severity::Fail && c.findings > 0)
    }
}

/// Audit thresholds, with Spec 07's defaults.
#[derive(Debug, Clone, Copy)]
pub struct AuditSettings {
    /// The deployment environment; fixture talks fail the audit outside dev.
    pub env: Env,
    /// `link_review_days`, default 90.
    pub link_review_days: i32,
    /// `consent_review_days`, default 90.
    pub consent_review_days: i32,
    /// `job_lock_timeout_s`, default 7200.
    pub job_lock_timeout_s: i32,
    /// Days a talk may stay quarantined, default 7.
    pub quarantine_days: i32,
}

impl AuditSettings {
    /// Spec 07 defaults for an environment.
    pub fn defaults(env: Env) -> Self {
        Self {
            env,
            link_review_days: 90,
            consent_review_days: 90,
            job_lock_timeout_s: 7200,
            quarantine_days: 7,
        }
    }
}

/// The servable predicate, the same one `servable_passage` uses. Kept here once so consent
/// drift and serving parity test the view against an independent statement of the rule.
const CONSENTED_READY: &str = "t.state = 'ready'
  and ((t.rights_basis = 'speaker_consent' and t.consent_status = 'granted')
    or (t.rights_basis = 'license' and exists (
          select 1 from license_evidence le where le.id = t.license_evidence_id and le.allowed)))
  and t.rights_holder is not null and lower(t.rights_holder) <> 'unknown'
  and t.teacher_id is not null and t.sangha_id is not null";

/// The five series named in the PRD; a ready talk whose title names one but has no series set
/// is a metadata gap. M3 replaces this with `config/series.toml`.
const SERIES_PATTERN: &str = "(genjokoan|genjo koan|uji|bendowa|blue cliff|grass hut)";

/// Runs every database check.
pub async fn run(pool: &PgPool, settings: AuditSettings) -> Result<AuditReport, DbError> {
    let mut checks = Vec::new();

    checks.push(
        check(
            pool,
            "consent_drift",
            Severity::Fail,
            &format!(
                "select p.id::text from passage p join talk t on t.id = p.talk_id
                 where not ({CONSENTED_READY})"
            ),
        )
        .await?,
    );

    let in_view: i64 = sqlx::query_scalar!(r#"select count(*) as "n!" from servable_passage"#)
        .fetch_one(pool)
        .await?;
    let expected: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select count(*) from passage p join talk t on t.id = p.talk_id where {CONSENTED_READY}"
    )))
    .fetch_one(pool)
    .await?;
    checks.push(CheckResult {
        name: "serving_parity",
        severity: Severity::Fail,
        findings: (in_view - expected).abs(),
        examples: if in_view == expected {
            vec![]
        } else {
            vec![format!(
                "servable_passage has {in_view} rows, expected {expected}"
            )]
        },
    });

    checks.push(
        check(
            pool,
            "student_speech",
            Severity::Fail,
            "select id::text from segment where kind = 'qa_student'",
        )
        .await?,
    );

    // Takedown completeness: any row left in a deleting store for a talk in a logged scope.
    let mut leftovers = Vec::new();
    for store in STORES
        .iter()
        .filter(|s| s.on_takedown == OnTakedown::Delete)
    {
        if let StoreKind::Table { table, talk_column } = store.kind {
            leftovers.push(format!(
                "select '{name}: ' || x.{talk_column}::text from {table} x
                 where x.{talk_column} in (select t.id from talk t where t.state = 'withdrawn')",
                name = store.name
            ));
        }
    }
    checks.push(
        check(
            pool,
            "takedown_completeness",
            Severity::Fail,
            &leftovers.join(" union all "),
        )
        .await?,
    );

    let fixture_sql = if settings.env.is_dev() {
        "select null::text where false".to_owned()
    } else {
        "select t.id::text from talk t join source_video sv on sv.id = t.source_video_id
         where sv.raw->>'synthetic' = 'true'"
            .to_owned()
    };
    checks.push(check(pool, "fixture_leak", Severity::Fail, &fixture_sql).await?);

    checks.push(
        check(
            pool,
            "mixed_embeddings",
            Severity::Fail,
            "select embedding_model || ' (' || count(*) || ' passages)' from passage
             group by embedding_model having (select count(distinct embedding_model) from passage) > 1",
        )
        .await?,
    );

    checks.push(
        check(
            pool,
            "job_health",
            Severity::Warn,
            &format!(
                "select kind || ' ' || id::text || ' ' || state::text from job
                 where state = 'dead'
                    or (state = 'running' and locked_at < now() - interval '{} seconds')",
                settings.job_lock_timeout_s
            ),
        )
        .await?,
    );

    checks.push(
        check(
            pool,
            "donation_links",
            Severity::Warn,
            &format!(
                "select s.name from sangha s
                 where s.listed and exists (select 1 from talk t where t.sangha_id = s.id and t.state = 'ready')
                   and (not s.donation_verified
                        or s.donation_verified_at < now() - interval '{} days')",
                settings.link_review_days
            ),
        )
        .await?,
    );

    checks.push(
        check(
            pool,
            "consent_staleness",
            Severity::Warn,
            &format!(
                "select te.display_name || ' (' || cc.status::text || ')' from current_consent cc
                 join teacher te on te.id = cc.teacher_id
                 where cc.status = 'granted'
                   and coalesce(cc.last_verified_at, cc.decided_at, cc.created_at)
                       < now() - interval '{} days'",
                settings.consent_review_days
            ),
        )
        .await?,
    );

    checks.push(
        check(
            pool,
            "quarantine_age",
            Severity::Warn,
            &format!(
                "select id::text || ' ' || coalesce(quarantine_reason, '') from talk
                 where state = 'quarantined' and updated_at < now() - interval '{} days'",
                settings.quarantine_days
            ),
        )
        .await?,
    );

    checks.push(
        check(
            pool,
            "series_metadata",
            Severity::Warn,
            &format!(
                "select id::text || ' ' || title from talk
                 where state = 'ready' and series is null and title ~* '{SERIES_PATTERN}'"
            ),
        )
        .await?,
    );

    Ok(AuditReport {
        checks,
        not_yet_checked: vec![
            (
                "audio_residue",
                "reads the scratch directory and buckets; joins with the transcript pipeline (M4)",
            ),
            (
                "student_speech (transcript JSON)",
                "reads transcript objects in S3; joins with the transcript pipeline (M4)",
            ),
            (
                "takedown_completeness (objects)",
                "reads transcript and citation objects in S3; joins with the transcript pipeline (M4)",
            ),
        ],
    })
}

async fn check(
    pool: &PgPool,
    name: &'static str,
    severity: Severity,
    sql: &str,
) -> Result<CheckResult, DbError> {
    // Every statement is a constant or built from compile-time values in this module.
    let rows: Vec<Option<String>> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_all(pool)
        .await?;
    Ok(CheckResult {
        name,
        severity,
        findings: i64::try_from(rows.len()).unwrap_or(i64::MAX),
        examples: rows.into_iter().flatten().take(20).collect(),
    })
}
