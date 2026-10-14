//! `wt audit` (Spec 07): integrity checks over the live database, object storage and the scratch
//! directory. Checks marked `Fail` make the command exit nonzero when they have findings; `Warn`
//! checks are listed only.
//!
//! [`run`] reads only the database. [`run_with_files`] adds the parts that read the scratch
//! directory and object storage: audio residue, student speech in transcript JSON, and objects
//! left for taken-down talks. The scratch bucket waits for the S3 client.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::Env;
use wt_core::objects::{ObjectError, ObjectStore};
use wt_core::scratch;
use wt_core::stores::{self, OnTakedown, STORES, StoreKind};

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
    /// `[deployment] exposure = "public"`: a mandated source's exposure grows once the tool is
    /// public, so the audit warns (Spec 08, Mandated sources).
    pub public: bool,
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
            public: false,
        }
    }
}

/// A talk whose rights still allow its text to be stored: a valid basis, a known holder, a
/// speaker and a sangha, and not excluded or withdrawn. It says nothing about the pipeline
/// state, so quarantined and partly indexed talks keep their passages without drifting.
pub(crate) const RIGHTS_VALID: &str = "t.state not in ('excluded', 'withdrawn')
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

/// Spec 07's student speech check: a `qa_student` segment, or a stored student question holding
/// a registry name, an email address, a phone number, a web address or six digits in a row
/// (Spec 04, Student questions). A one-word name under three letters is not matched, as in the
/// edit's own check.
const STUDENT_SPEECH: &str = r#"select id::text from segment where kind = 'qa_student'
union all
select 'question ' || q.id::text from student_question q
where q.text ~ '[0-9]{6,}'
   or q.text ~ '[[:alnum:]._+-]+@[[:alnum:]-]+\.[[:alnum:].-]+'
   or q.text ~* '(https?://|www\.)'
   or q.text ~* '\m[a-z0-9-]+\.(com|org|net|edu|gov|io|info|co|us|uk|ca|de)\M'
   or length(regexp_replace(coalesce(substring(q.text from '\+?\(?[0-9][0-9 ().-]{5,}[0-9]'), ''),
                            '[^0-9]', '', 'g')) >= 7
   or exists (select 1 from (select display_name as n from teacher
                             union select alias from teacher_alias
                             union select name from sangha) r
              where (length(r.n) >= 3 or r.n like '% %')
                and q.text ~* ('\m' || regexp_replace(r.n, '([^[:alnum:] ])', '\\\1', 'g') || '\M'))"#;

/// Talks whose words must be gone: withdrawn, or covered by a recorded takedown.
const TAKEN_DOWN: &str = "select t.id from talk t where t.state = 'withdrawn'
   union select l.talk_id from takedown_log l where l.talk_id is not null
   union select e::uuid from takedown_log l,
                jsonb_array_elements_text(coalesce(l.counts->'talks', '[]')) e";

/// Runs every database check, in the order the report lists them.
pub async fn run(pool: &PgPool, settings: AuditSettings) -> Result<AuditReport, DbError> {
    let mut checks = serving_checks(pool).await?;
    checks.extend(content_checks(pool, settings).await?);
    checks.extend(upkeep_checks(pool, settings).await?);
    checks.extend(mandate_checks(pool, settings).await?);
    checks.extend(text_checks(pool).await?);
    checks.extend(mandate_reach_checks(pool, settings).await?);

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

/// Rule 3's checks (Spec 07): no passage or stored student question outlives its talk's rights,
/// and each serving view holds exactly the rows the servable predicate allows, which is stated
/// again here rather than read from the view.
async fn serving_checks(pool: &PgPool) -> Result<Vec<CheckResult>, DbError> {
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
    checks.push(parity(
        "serving_parity",
        "servable_passage",
        in_view,
        expected,
    ));

    // Rule 3's checks over every stored student question (Spec 04, Student questions): none
    // outlives its talk's rights, and the serving view shows exactly the ones the rule allows.
    checks.push(
        check(
            pool,
            "question_consent_drift",
            Severity::Fail,
            &format!(
                "select q.id::text from student_question q join talk t on t.id = q.talk_id
                 where not ({RIGHTS_VALID})"
            ),
        )
        .await?,
    );
    let in_view: i64 =
        sqlx::query_scalar!(r#"select count(*) as "n!" from servable_student_question"#)
            .fetch_one(pool)
            .await?;
    let expected: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select count(*) from student_question q join talk t on t.id = q.talk_id where {}",
        consented_ready()
    )))
    .fetch_one(pool)
    .await?;
    checks.push(parity(
        "question_serving_parity",
        "servable_student_question",
        in_view,
        expected,
    ));
    Ok(checks)
}

/// A parity check's result: one finding per row of difference between the view's count and
/// the predicate's.
fn parity(name: &'static str, view: &str, in_view: i64, expected: i64) -> CheckResult {
    CheckResult {
        name,
        severity: Severity::Fail,
        findings: (in_view - expected).abs(),
        examples: if in_view == expected {
            vec![]
        } else {
            vec![format!("{view} has {in_view} rows, expected {expected}")]
        },
    }
}

/// What must never be stored at all: student speech, rows left for a taken-down talk, fixture
/// talks outside dev, and passages embedded by two models at once.
async fn content_checks(
    pool: &PgPool,
    settings: AuditSettings,
) -> Result<Vec<CheckResult>, DbError> {
    let mut checks = Vec::new();
    checks.push(check(pool, "student_speech", Severity::Fail, STUDENT_SPEECH).await?);

    // Takedown completeness: any row left in a deleting store for a withdrawn talk or a talk in
    // a scope recorded in takedown_log, which also catches a restore that brought one back.
    let mut leftovers = Vec::new();
    for store in STORES
        .iter()
        .filter(|s| s.on_takedown == OnTakedown::Delete)
    {
        if let StoreKind::Table { table, talk_column } = store.kind {
            leftovers.push(format!(
                "select '{name}: ' || x.{talk_column}::text from {table} x
                 where x.{talk_column} in ({TAKEN_DOWN})",
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
    Ok(checks)
}

/// Upkeep warnings: dead or stuck jobs, donation links and consent overdue for review, talks
/// quarantined too long, and series missing from talks whose titles name one.
async fn upkeep_checks(
    pool: &PgPool,
    settings: AuditSettings,
) -> Result<Vec<CheckResult>, DbError> {
    let mut checks = Vec::new();
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
    Ok(checks)
}

/// The mandate's obligations (Spec 08, Rights lifecycle; Spec 03, Mandated talks): requests go
/// out and are followed up, and nothing opted out, excluded or switched off is served.
async fn mandate_checks(
    pool: &PgPool,
    settings: AuditSettings,
) -> Result<Vec<CheckResult>, DbError> {
    let mut checks = Vec::new();
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
    Ok(checks)
}

/// Texts (Spec 08): nothing excluded or switched off serves, takedowns leave no units, and
/// removal requests are answered within a business day.
async fn text_checks(pool: &PgPool) -> Result<Vec<CheckResult>, DbError> {
    let mut checks = Vec::new();
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
    Ok(checks)
}

/// How far the mandate reaches (Spec 08, Mandated sources; Spec 03, Mandated talks): mandated
/// sources once the tool is public, and teachers used on the mandate who were never sent a
/// notice.
async fn mandate_reach_checks(
    pool: &PgPool,
    settings: AuditSettings,
) -> Result<Vec<CheckResult>, DbError> {
    let mut checks = Vec::new();
    checks.push(
        check(
            pool,
            "mandate_exposed",
            Severity::Warn,
            &format!(
                "select sp.key from source_policy sp
                 where {} and sp.enabled and sp.basis = 'mandate'",
                settings.public
            ),
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
    Ok(checks)
}

/// Runs one check's query, whose every row is a finding; the first 20 non-null rows are its
/// examples.
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

/// Passages a talk holds, served or not: the re-check before a quarantined talk is released,
/// which the serving view cannot answer while the talk is out of serving.
pub(crate) async fn held_passages(
    conn: &mut sqlx::PgConnection,
    talk: wt_core::ids::TalkId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"select count(*) as "n!" from passage where talk_id = $1"#,
        talk.as_uuid()
    )
    .fetch_one(&mut *conn)
    .await
}

/// The stores outside the database that the audit reads.
pub struct Files<'a> {
    /// Transcript and citation objects, and takedown records.
    pub objects: &'a dyn ObjectStore,
    /// The scratch root, `WT_SCRATCH_DIR`.
    pub scratch_root: &'a Path,
    /// A scratch directory older than this is audio residue (Spec 07, `scratch_resume_ttl_h`).
    pub scratch_ttl_h: u64,
}

/// `scratch_resume_ttl_h`'s default. Since a retry no longer resumes from scratch (Spec 04,
/// Media handling, rule 5), only a job running now has a scratch directory, and none runs a day.
pub const SCRATCH_TTL_H: u64 = 24;

/// Why the audit could not read a store.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
    /// Object storage refused.
    #[error("object storage: {0}")]
    Objects(#[from] ObjectError),
    /// The scratch directory could not be read.
    #[error("scratch directory {0}: {1}")]
    Scratch(PathBuf, std::io::Error),
}

impl From<sqlx::Error> for AuditError {
    fn from(e: sqlx::Error) -> Self {
        AuditError::Db(DbError::Sqlx(e))
    }
}

/// Every check, including the parts that read object storage and the scratch directory:
/// audio residue, student speech in transcript JSON, and objects left for taken-down talks.
/// The scratch bucket waits for the S3 client and stays listed as not yet checked.
pub async fn run_with_files(
    pool: &PgPool,
    settings: AuditSettings,
    files: &Files<'_>,
) -> Result<AuditReport, AuditError> {
    let mut report = run(pool, settings).await?;

    let mut residue = Vec::new();
    let ttl = std::time::Duration::from_secs(files.scratch_ttl_h.saturating_mul(3600));
    for dir in scratch::leftovers(files.scratch_root)
        .map_err(|e| AuditError::Scratch(files.scratch_root.to_owned(), e))?
    {
        let age = std::fs::metadata(&dir)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .unwrap_or_default();
        if age >= ttl {
            residue.push(format!("scratch {}", dir.display()));
        }
    }
    let keys = files.objects.list("")?;
    for key in &keys {
        let ext = key.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        if ext.is_some_and(|e| scratch::AUDIO_AND_VIDEO_EXTENSIONS.contains(&e.as_str())) {
            residue.push(format!("object {key}"));
        }
    }
    report
        .checks
        .push(result("audio_residue", Severity::Fail, residue));

    let mut speech = Vec::new();
    for key in keys.iter().filter(|k| is_transcript_key(k)) {
        if let Some(bytes) = files.objects.get(key)?
            && let Some(why) = student_speech_in(&bytes)
        {
            speech.push(format!("{key}: {why}"));
        }
    }
    extend(&mut report, "student_speech", speech);

    let taken_down: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select id from ({TAKEN_DOWN}) x(id) where id is not null"
    )))
    .fetch_all(pool)
    .await?;
    let mut left = Vec::new();
    for talk in taken_down {
        for (name, prefix) in stores::talk_objects(&talk.to_string()) {
            for key in files.objects.list(&prefix)? {
                left.push(format!("{name}: {key}"));
            }
        }
    }
    extend(&mut report, "takedown_completeness", left);

    report.not_yet_checked = vec![(
        "audio_residue (scratch bucket)",
        "needs the S3 client; the bucket's one-day lifecycle rule expires objects meanwhile",
    )];
    Ok(report)
}

fn result(name: &'static str, severity: Severity, found: Vec<String>) -> CheckResult {
    CheckResult {
        name,
        severity,
        findings: i64::try_from(found.len()).unwrap_or(i64::MAX),
        examples: found.into_iter().take(20).collect(),
    }
}

/// Adds findings from the files to a database check of the same name.
fn extend(report: &mut AuditReport, name: &str, found: Vec<String>) {
    if let Some(check) = report.checks.iter_mut().find(|c| c.name == name) {
        check.findings += i64::try_from(found.len()).unwrap_or(i64::MAX);
        let room = 20usize.saturating_sub(check.examples.len());
        check.examples.extend(found.into_iter().take(room));
    }
}

/// `transcripts/<talk>/v<n>.json`.
fn is_transcript_key(key: &str) -> bool {
    let parts: Vec<&str> = key.split('/').collect();
    matches!(parts.as_slice(), ["transcripts", _, file]
        if file.strip_prefix('v').and_then(|f| f.strip_suffix(".json"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}

/// Why a transcript object holds student speech, if it does: a `qa_student` span, or a span or
/// word attributed to any speaker but the teacher. A transcript that cannot be parsed is a
/// finding too, since it cannot be shown to be clean.
pub fn student_speech_in(bytes: &[u8]) -> Option<String> {
    let doc: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => return Some(format!("not readable JSON ({e})")),
    };
    let not_teacher = |v: &serde_json::Value| {
        v.get("speaker")
            .is_some_and(|s| s.as_str().is_none_or(|s| s != "teacher"))
    };
    let spans = doc.get("spans").and_then(|s| s.as_array());
    for span in spans.into_iter().flatten() {
        if span.get("kind").and_then(|k| k.as_str()) == Some("qa_student") {
            return Some("a qa_student span".to_owned());
        }
        if not_teacher(span) {
            return Some("a span attributed to another speaker".to_owned());
        }
    }
    let words = doc.get("words").and_then(|w| w.as_array());
    if words.into_iter().flatten().any(not_teacher) {
        return Some("a word attributed to another speaker".to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_keys_are_versions_only() {
        assert!(is_transcript_key("transcripts/t1/v1.json"));
        assert!(is_transcript_key("transcripts/t1/v12.json"));
        for other in [
            "transcripts/t1/questions.json",
            "transcripts/t1/glossary_suggestions.csv",
            "transcripts/t1/v.json",
            "citations/t1.json",
        ] {
            assert!(!is_transcript_key(other), "{other}");
        }
    }

    #[test]
    fn student_speech_is_found_in_spans_and_words() {
        let clean = br#"{"words":[{"i":0,"w":"sit","s":0,"e":1,"p":0.9}],
                         "spans":[{"kind":"talk","s":0,"e":1},{"kind":"qa_teacher","s":1,"e":2}]}"#;
        assert_eq!(student_speech_in(clean), None);
        let student = br#"{"words":[],"spans":[{"kind":"qa_student","s":0,"e":1}]}"#;
        assert!(student_speech_in(student).is_some());
        let speaker = br#"{"words":[{"w":"hi","speaker":"SPEAKER_01"}],"spans":[]}"#;
        assert!(student_speech_in(speaker).is_some());
        let teacher = br#"{"words":[{"w":"hi","speaker":"teacher"}],"spans":[]}"#;
        assert_eq!(student_speech_in(teacher), None);
        assert!(student_speech_in(b"not json").is_some());
    }
}
