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
    /// Days a mandated source may go without a permission request, default 14 (Spec 08).
    pub mandate_request_days: i32,
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
            mandate_request_days: 14,
        }
    }
}

/// A talk whose rights still allow its text to be stored: a valid basis, a known holder, a
/// speaker and a sangha, and not excluded or withdrawn. It says nothing about the pipeline
/// state, so quarantined and partly indexed talks keep their passages without drifting.
const RIGHTS_VALID: &str = "t.state not in ('excluded', 'withdrawn')
  and ((t.rights_basis = 'speaker_consent' and t.consent_status = 'granted')
    or (t.rights_basis in ('license', 'mandate') and exists (
          select 1 from license_evidence le where le.id = t.license_evidence_id and le.allowed)))
  and t.rights_holder is not null and lower(t.rights_holder) <> 'unknown'
  and t.teacher_id is not null and t.sangha_id is not null";

/// What serving adds to valid rights (Spec 03, Mandated talks): the teacher has not opted
/// out, and a mandated talk serves only while `talks_basis` is `mandate` and its source is on.
/// Switching either off keeps the talk's text, so these are not part of [`RIGHTS_VALID`].
const SERVING_ON: &str = "not exists (select 1 from teacher te
                 where te.id = t.teacher_id and te.opted_out_at is not null)
  and (t.rights_basis is distinct from 'mandate'
    or ((select value from rights_setting where key = 'talks_basis') = 'mandate'
        and (t.source_key is null or exists (
              select 1 from source_policy sp where sp.key = t.source_key and sp.enabled))))";

/// The servable predicate, stated independently of `servable_passage` so serving parity tests
/// the view against the rule: a ready talk with valid rights, being served.
fn consented_ready() -> String {
    format!("t.state = 'ready' and {RIGHTS_VALID} and {SERVING_ON}")
}

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
                 where not ({RIGHTS_VALID})"
            ),
        )
        .await?,
    );

    let in_view: i64 = sqlx::query_scalar!(r#"select count(*) as "n!" from servable_passage"#)
        .fetch_one(pool)
        .await?;
    let expected: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select count(*) from passage p join talk t on t.id = p.talk_id where {}",
        consented_ready()
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

    // Spec 08, Rights lifecycle: every mandate is followed by a request for permission, and
    // every request that went out is followed up.
    checks.push(
        check(
            pool,
            "mandate_unrequested",
            Severity::Warn,
            &format!(
                "select sp.key from source_policy sp
                 where sp.basis = 'mandate'
                   and not exists (select 1 from permission_request pr where pr.source_key = sp.key)
                   and (select min(e.created_at) from rights_event e
                        where e.source_key = sp.key and e.event = 'mandated')
                       < now() - interval '{} days'",
                settings.mandate_request_days
            ),
        )
        .await?,
    );
    checks.push(
        check(
            pool,
            "request_follow_up",
            Severity::Warn,
            "select coalesce(pr.source_key, te.display_name) || ' to ' || pr.recipient
                    || ', follow up due ' || pr.follow_up_on::text
             from permission_request pr left join teacher te on te.id = pr.teacher_id
             where pr.status = 'sent' and pr.follow_up_on < current_date",
        )
        .await?,
    );

    // Spec 03, Mandated talks: nothing of an opted-out or excluded teacher is served, no
    // mandated talk is served while the switch is off, and every teacher whose talks are used
    // on the mandate is sent a notice.
    checks.push(
        check(
            pool,
            "opted_out_served",
            Severity::Fail,
            "select sp.passage_id::text || ' ' || te.display_name from servable_passage sp
             join teacher te on te.id = sp.teacher_id
             where te.opted_out_at is not null
                or exists (select 1 from source_exclusion x
                           where x.scope = 'person' and x.lifted_at is null
                             and lower(x.pattern) = lower(te.display_name))",
        )
        .await?,
    );
    checks.push(
        check(
            pool,
            "mandate_served_while_off",
            Severity::Fail,
            "select sp.passage_id::text from servable_passage sp
             where sp.rights_basis = 'mandate'
               and (select value from rights_setting where key = 'talks_basis') <> 'mandate'",
        )
        .await?,
    );
    // Texts (Spec 08): nothing excluded or switched off serves, takedowns leave no units, and
    // removal requests are answered within a business day.
    checks.push(
        check(
            pool,
            "text_excluded_served",
            Severity::Fail,
            "select s.text_unit_id::text from servable_text_unit s
             join text_unit u on u.id = s.text_unit_id
             join text_edition e on e.id = s.edition_id
             where exists (
               select 1 from source_exclusion x where x.lifted_at is null and (
                    (x.scope = 'work' and lower(x.pattern) = lower(s.work_title))
                 or (x.scope = 'edition' and x.pattern = e.id::text)
                 or (x.scope = 'content' and x.pattern = u.content_hash)
                 or (x.scope = 'prefix' and starts_with(lower(e.source_url), lower(x.pattern)))))",
        )
        .await?,
    );
    checks.push(
        check(
            pool,
            "text_source_served_while_off",
            Severity::Fail,
            "select s.text_unit_id::text from servable_text_unit s
             join text_edition e on e.id = s.edition_id
             join source_policy sp on sp.key = e.source_key
             where not sp.enabled",
        )
        .await?,
    );
    checks.push(
        check(
            pool,
            "text_takedown_completeness",
            Severity::Fail,
            "select e.id::text from text_edition e
             where e.state = 'withdrawn'
               and (exists (select 1 from text_unit u where u.edition_id = e.id)
                    or e.legal_text is not null)",
        )
        .await?,
    );
    checks.push(
        check(
            pool,
            "takedown_request_overdue",
            Severity::Warn,
            // More than one business day: count the weekdays since it arrived.
            "select r.id::text from takedown_request r
             where r.status = 'open'
               and (select count(*) from generate_series(r.received_at::date + 1, now()::date,
                                                         interval '1 day') d
                    where extract(isodow from d) < 6) > 1",
        )
        .await?,
    );
    checks.push(
        check(
            pool,
            "teacher_unnotified",
            Severity::Warn,
            &format!(
                "select te.display_name from teacher te
                 where exists (select 1 from talk t
                               where t.teacher_id = te.id and t.rights_basis = 'mandate'
                                 and t.state <> 'withdrawn')
                   and not exists (select 1 from permission_request pr where pr.teacher_id = te.id)
                   and (select min(t.created_at) from talk t
                        where t.teacher_id = te.id and t.rights_basis = 'mandate')
                       < now() - interval '{} days'",
                settings.mandate_request_days
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
