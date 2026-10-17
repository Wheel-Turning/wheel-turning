//! `wt db catch-up` (Spec 11, PB1.1): the one-time copy of pipeline rows from the PC database to
//! RDS before the worker moves to RDS.
//!
//! RDS was loaded from the PC on October 13, 2026. Since then the worker kept writing to the PC,
//! while testers, logs, feedback, removal requests and rights actions reached RDS only. This copy
//! closes the first gap without touching the second:
//!
//! - It covers talks whose rows changed on the source after `since`, and text editions imported
//!   after it, and writes only pipeline-owned rows: the talk's pipeline columns, its source
//!   video, transcripts, segments, passages, citations and student questions, and an edition's
//!   text units. It never writes `tester`, `query_log`, `feedback`, `takedown_request`,
//!   `rights_event`, or any other table the web service or a rights command writes, and keeps
//!   the target's rights columns on every talk (consent, rights holder and basis, license,
//!   display level, source, quarantine reason).
//! - It skips any talk the target has withdrawn, quarantined or excluded, whose teacher opted out
//!   there, that a takedown on the target names, or whose URL an active exclusion there matches.
//! - Rows are matched by id: the October 13 load kept every id. A row only on the target is
//!   deleted, unless a table outside the pipeline (an answer, a feedback row, an evaluation
//!   rating, a removal request) refers to it or to a row its deletion would cascade to; then the
//!   whole talk is skipped, so nothing a tester saw loses its source.
//! - Each talk is copied in its own transaction. A dry run does the same work and rolls each one
//!   back, so its counts and failures are the ones `--apply` would see.
//!
//! [`run`] wraps the copy: the target's takedown replay first (with `--apply`), then the copy,
//! then `wt rights parity` between the two.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::types::Uuid;
use sqlx::{AssertSqlSafe, PgConnection, PgPool, Row};
use wt_core::objects::ObjectStore;

use crate::parity::{self, ParityReport};
use crate::rights::{self, RightsError};
use crate::takedown::{self, ReplayReport, TakedownError};
use crate::{DbError, identity};

/// Talk states that remove a talk from serving. A talk in one of them on the target is never
/// touched; one in them on the source only is left for `wt rights parity` to report.
const REMOVED: [&str; 3] = ["withdrawn", "quarantined", "excluded"];

/// States between `cleared` and `ready`, which only a queued job moves on.
const IN_PIPELINE: [&str; 8] = [
    "fetching",
    "transcribing",
    "diarizing",
    "segmenting",
    "correcting",
    "extracting",
    "resolving",
    "indexing",
];

/// Talk columns the catch-up never changes on a talk the target already holds: its keys, the
/// trigger-kept time, and every rights column a rights command writes.
const TALK_KEPT: [&str; 11] = [
    "id",
    "source_video_id",
    "created_at",
    "updated_at",
    "consent_status",
    "rights_holder",
    "rights_basis",
    "license_evidence_id",
    "display_level",
    "source_key",
    "quarantine_reason",
];

/// Talk columns set by speaker assignment. They are copied only to a talk the target has not
/// assigned yet; a talk assigned differently on each side is skipped.
const ASSIGNMENT: [&str; 4] = [
    "teacher_id",
    "sangha_id",
    "speaker_guess",
    "speaker_assigned_by",
];

/// A pipeline table under a talk or an edition, in the order rows are written, with the column
/// naming its owner and the parent whose deletion cascades to it.
struct Child {
    /// The table.
    table: &'static str,
    /// The column naming the talk or edition.
    owner: &'static str,
    /// The column and table of the parent row it is deleted with, if any.
    parent: Option<(&'static str, &'static str)>,
}

/// A talk's pipeline rows, parents first.
const TALK_CHILDREN: [Child; 5] = [
    Child {
        table: "transcript",
        owner: "talk_id",
        parent: None,
    },
    Child {
        table: "segment",
        owner: "talk_id",
        parent: Some(("transcript_id", "transcript")),
    },
    Child {
        table: "passage",
        owner: "talk_id",
        parent: Some(("segment_id", "segment")),
    },
    Child {
        table: "citation",
        owner: "talk_id",
        parent: Some(("passage_id", "passage")),
    },
    Child {
        table: "student_question",
        owner: "talk_id",
        parent: Some(("transcript_id", "transcript")),
    },
];

/// An edition's pipeline rows.
const EDITION_CHILDREN: [Child; 1] = [Child {
    table: "text_unit",
    owner: "edition_id",
    parent: None,
}];

/// Every table the catch-up writes. A foreign key from any other table protects the row it
/// names.
const WRITTEN: [&str; 8] = [
    "source_video",
    "talk",
    "transcript",
    "segment",
    "passage",
    "citation",
    "student_question",
    "text_unit",
];

/// Tables the catch-up must leave as they are, counted on the target before and after. The web
/// service may add rows while it runs, so only a drop counts as a change the catch-up made.
pub const UNTOUCHED: [&str; 12] = [
    "tester",
    "query_log",
    "answer",
    "answer_passage",
    "feedback",
    "takedown_request",
    "rights_event",
    "takedown_log",
    "speaker_consent",
    "source_exclusion",
    "dana_event",
    "accessibility_report",
];

/// What the catch-up is asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    /// Rows changed on the source after this time are candidates: the October 13 load.
    pub since: DateTime<Utc>,
    /// Write the copy. Without it every talk's transaction is rolled back.
    pub apply: bool,
    /// Who runs it, for the takedown replay's records.
    pub actor: String,
}

/// Why the catch-up stopped before copying anything.
#[derive(Debug, thiserror::Error)]
pub enum CatchUpError {
    /// A query failed outside a single talk's copy.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The two databases cannot be compared safely.
    #[error("{0}")]
    Refused(String),
    /// The target's takedown replay failed.
    #[error("the takedown replay on the target failed: {0}")]
    Replay(#[from] TakedownError),
    /// The target's exclusions could not be read.
    #[error(transparent)]
    Rights(#[from] RightsError),
}

impl From<sqlx::Error> for CatchUpError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(DbError::from(e))
    }
}

/// Rows written, or that a dry run would write, in one table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RowCounts {
    /// Rows only on the source.
    pub insert: usize,
    /// Rows on both whose content differs, and rows a deleted parent took with it.
    pub update: usize,
    /// Rows only on the target.
    pub delete: usize,
}

/// How the talks fared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TalkCounts {
    /// Talks the target did not hold.
    pub new: usize,
    /// Talks whose rows changed.
    pub updated: usize,
    /// Talks already the same on both.
    pub unchanged: usize,
    /// Talks left alone, with the reason in `skipped`.
    pub skipped: usize,
    /// Talks whose copy failed, with the error in `failed`.
    pub failed: usize,
}

/// One talk or edition left alone or failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    /// `platform:external_id` for a talk, or the edition id.
    pub key: String,
    /// Why.
    pub reason: String,
}

/// One table's row count on the target before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counted {
    /// Before the copy.
    pub before: i64,
    /// After it.
    pub after: i64,
}

/// What `wt db catch-up` did, or would do.
#[derive(Debug, Clone, Serialize)]
pub struct CatchUpReport {
    /// True unless `--apply` was given; then nothing was written.
    pub dry_run: bool,
    /// The source's name from `database_identity`.
    pub from: String,
    /// The target's name.
    pub to: String,
    /// The cut-off.
    pub since: DateTime<Utc>,
    /// The migration both databases are at.
    pub schema_version: Option<i64>,
    /// The target's takedown replay, run before copying with `--apply`.
    pub replay: Option<ReplayReport>,
    /// Talks changed on the source after `since`.
    pub candidates: usize,
    /// How they fared.
    pub talks: TalkCounts,
    /// Editions imported on the source after `since`.
    pub edition_candidates: usize,
    /// Editions whose text units were copied or were already the same.
    pub editions_copied: usize,
    /// Rows per table.
    pub rows: BTreeMap<String, RowCounts>,
    /// Talks copied in a pipeline state with no queued or running job on the target; `wt
    /// pipeline retry` or a requeue moves them on.
    pub in_pipeline_without_job: Vec<String>,
    /// Talks and editions left alone, and why.
    pub skipped: Vec<Item>,
    /// Talks and editions whose copy failed.
    pub failed: Vec<Item>,
    /// The tables the catch-up never writes, counted on the target.
    pub untouched: BTreeMap<String, Counted>,
    /// Untouched tables with fewer rows after than before.
    pub untouched_dropped: Vec<String>,
    /// `wt rights parity` between the target and the source, after the copy.
    pub parity: Option<ParityReport>,
}

impl CatchUpReport {
    /// True when the command must exit non-zero: a copy failed, an untouched table lost rows,
    /// or parity finds an unsafe difference.
    pub fn failed(&self) -> bool {
        !self.failed.is_empty()
            || !self.untouched_dropped.is_empty()
            || self.parity.as_ref().is_some_and(ParityReport::failed)
    }
}

/// Runs the catch-up from `source` to `target`: names both, replays the target's takedowns
/// (with `apply`), copies, then compares the two with `wt rights parity`.
pub async fn run(
    source: &PgPool,
    target: &PgPool,
    store: &dyn ObjectStore,
    options: &Options,
) -> Result<CatchUpReport, CatchUpError> {
    let from = identity::read(source)
        .await
        .map_err(|e| CatchUpError::Refused(e.to_string()))?
        .ok_or_else(|| CatchUpError::Refused("the source database has no name".to_owned()))?;
    let to = identity::read(target)
        .await
        .map_err(|e| CatchUpError::Refused(e.to_string()))?
        .ok_or_else(|| CatchUpError::Refused("the target database has no name".to_owned()))?;
    if from == to {
        return Err(CatchUpError::Refused(format!(
            "both databases are named {from:?}; the source and target must differ"
        )));
    }
    let schema_version = same_schema(source, target).await?;

    let replay = if options.apply {
        Some(takedown::replay(target, store, &options.actor).await?)
    } else {
        None
    };

    let mut report = CatchUpReport {
        dry_run: !options.apply,
        from,
        to,
        since: options.since,
        schema_version,
        replay,
        candidates: 0,
        talks: TalkCounts::default(),
        edition_candidates: 0,
        editions_copied: 0,
        rows: BTreeMap::new(),
        in_pipeline_without_job: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
        untouched: BTreeMap::new(),
        untouched_dropped: Vec::new(),
        parity: None,
    };
    let before = untouched_counts(target).await?;
    copy(source, target, options, &mut report).await?;
    let after = untouched_counts(target).await?;
    for (table, before) in before {
        let after = after.get(&table).copied().unwrap_or(0);
        if after < before {
            report.untouched_dropped.push(table.clone());
        }
        report.untouched.insert(table, Counted { before, after });
    }
    let names = (report.to.clone(), report.from.clone());
    report.parity = Some(parity::run(target, source, (&names.0, &names.1)).await?);
    Ok(report)
}

/// Both databases' latest migration, refused unless they match, and their column lists for the
/// written tables, refused unless they match too.
async fn same_schema(source: &PgPool, target: &PgPool) -> Result<Option<i64>, CatchUpError> {
    let (a, b) = (version(source).await?, version(target).await?);
    if a != b {
        return Err(CatchUpError::Refused(format!(
            "the source is at migration {a:?} and the target at {b:?}; migrate both to the same \
             version first"
        )));
    }
    let (ca, cb) = (
        columns(&mut *source.acquire().await?).await?,
        columns(&mut *target.acquire().await?).await?,
    );
    if ca != cb {
        return Err(CatchUpError::Refused(
            "the written tables' columns differ between the databases".to_owned(),
        ));
    }
    Ok(a)
}

/// The latest migration applied to one database.
async fn version(pool: &PgPool) -> Result<Option<i64>, CatchUpError> {
    Ok(
        sqlx::query_scalar("select max(version) from _sqlx_migrations where success")
            .fetch_one(pool)
            .await?,
    )
}

/// The written tables' columns as `(table, column, generated)`, in column order.
type Columns = BTreeMap<String, Vec<(String, bool)>>;

/// Reads [`Columns`] from one database.
async fn columns(conn: &mut PgConnection) -> Result<Columns, CatchUpError> {
    let rows = sqlx::query(
        "select table_name::text, column_name::text, is_generated = 'ALWAYS' as generated
         from information_schema.columns
         where table_schema = 'public' and table_name = any($1)
         order by table_name, ordinal_position",
    )
    .bind(WRITTEN.map(str::to_owned).to_vec())
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Columns::new();
    for r in rows {
        out.entry(r.try_get(0)?)
            .or_default()
            .push((r.try_get(1)?, r.try_get(2)?));
    }
    Ok(out)
}

/// Row counts of [`UNTOUCHED`] on one database.
async fn untouched_counts(pool: &PgPool) -> Result<BTreeMap<String, i64>, CatchUpError> {
    let mut out = BTreeMap::new();
    for table in UNTOUCHED {
        let n: i64 = sqlx::query_scalar(AssertSqlSafe(format!("select count(*) from {table}")))
            .fetch_one(pool)
            .await?;
        out.insert(table.to_owned(), n);
    }
    // A deleted passage sets feedback.passage_id to null rather than deleting the row.
    let n: i64 = sqlx::query_scalar("select count(*) from feedback where passage_id is not null")
        .fetch_one(pool)
        .await?;
    out.insert("feedback.passage_id".to_owned(), n);
    Ok(out)
}

/// What the copy needs to know about the target's schema.
struct Schema {
    /// Each written table's columns that can be written, that is, not generated.
    writable: BTreeMap<String, Vec<String>>,
    /// Each written table's generated columns, left out of comparisons.
    generated: BTreeMap<String, Vec<String>>,
    /// Foreign keys into the written tables from tables outside them: `(table, column)` by the
    /// table they name.
    protecting: BTreeMap<String, Vec<(String, String)>>,
}

impl Schema {
    /// Reads the target's schema.
    async fn read(conn: &mut PgConnection) -> Result<Self, CatchUpError> {
        let cols = columns(&mut *conn).await?;
        let mut writable = BTreeMap::new();
        let mut generated = BTreeMap::new();
        for (table, list) in cols {
            let (gen_cols, plain): (Vec<_>, Vec<_>) = list.into_iter().partition(|(_, g)| *g);
            writable.insert(table.clone(), plain.into_iter().map(|(c, _)| c).collect());
            generated.insert(table, gen_cols.into_iter().map(|(c, _)| c).collect());
        }
        let rows = sqlx::query(
            "select c.confrelid::regclass::text as named, c.conrelid::regclass::text as tbl,
                    a.attname::text as col
             from pg_constraint c
             join pg_attribute a on a.attrelid = c.conrelid and a.attnum = c.conkey[1]
             where c.contype = 'f' and cardinality(c.conkey) = 1
               and c.confrelid::regclass::text = any($1)
               and not (c.conrelid::regclass::text = any($1))",
        )
        .bind(WRITTEN.map(str::to_owned).to_vec())
        .fetch_all(&mut *conn)
        .await?;
        let mut protecting: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for r in rows {
            protecting
                .entry(r.try_get(0)?)
                .or_default()
                .push((r.try_get(1)?, r.try_get(2)?));
        }
        Ok(Self {
            writable,
            generated,
            protecting,
        })
    }

    /// The writable columns of `table`.
    fn writable(&self, table: &str) -> &[String] {
        self.writable.get(table).map_or(&[], Vec::as_slice)
    }

    /// The generated columns of `table`.
    fn generated(&self, table: &str) -> Vec<String> {
        self.generated.get(table).cloned().unwrap_or_default()
    }
}

/// One talk the source changed after the cut-off.
struct Candidate {
    /// Its id, the same on both databases for a talk the target holds.
    id: Uuid,
    /// `platform:external_id`.
    key: String,
    /// Its source video.
    video: Uuid,
    /// The video's URL, for the exclusion check.
    url: String,
    /// Its state on the source.
    state: String,
}

/// What a talk's copy did.
enum Outcome {
    /// The target did not hold it.
    New,
    /// Rows changed.
    Updated,
    /// Nothing differed.
    Unchanged,
    /// Left alone, and why.
    Skipped(String),
}

/// Copies every candidate talk and edition, recording the outcome in `report`.
async fn copy(
    source: &PgPool,
    target: &PgPool,
    options: &Options,
    report: &mut CatchUpReport,
) -> Result<(), CatchUpError> {
    let mut src = source.acquire().await?;
    sqlx::query("set time zone 'UTC'")
        .execute(&mut *src)
        .await?;
    let schema = Schema::read(&mut *target.acquire().await?).await?;
    let exclusions = rights::active_exclusions(&mut *target.acquire().await?).await?;

    let candidates: Vec<Candidate> = sqlx::query(
        "select t.id, sv.platform::text || ':' || sv.external_id as key, sv.id as video,
                sv.url, t.state::text as state
         from talk t join source_video sv on sv.id = t.source_video_id
         where t.updated_at > $1 or sv.discovered_at > $1
            or exists (select 1 from transcript x where x.talk_id = t.id and x.created_at > $1)
            or exists (select 1 from citation c where c.talk_id = t.id and c.created_at > $1)
            or exists (select 1 from student_question q
                       where q.talk_id = t.id and q.created_at > $1)
         order by key",
    )
    .bind(options.since)
    .fetch_all(&mut *src)
    .await?
    .into_iter()
    .map(|r| {
        Ok(Candidate {
            id: r.try_get("id")?,
            key: r.try_get("key")?,
            video: r.try_get("video")?,
            url: r.try_get("url")?,
            state: r.try_get("state")?,
        })
    })
    .collect::<Result<_, sqlx::Error>>()?;
    report.candidates = candidates.len();

    for cand in &candidates {
        if REMOVED.contains(&cand.state.as_str()) {
            skip(
                report,
                &cand.key,
                format!("{} on the source only", cand.state),
            );
            continue;
        }
        if let Some(ex) = rights::exclusion_for_url(&exclusions, &cand.url) {
            skip(
                report,
                &cand.key,
                format!("matches exclusion {} on the target", ex.id),
            );
            continue;
        }
        let mut tx = target.begin().await?;
        sqlx::query("set local time zone 'UTC'")
            .execute(&mut *tx)
            .await?;
        let mut rows = BTreeMap::new();
        let result = copy_talk(&mut src, &mut tx, &schema, cand, &mut rows).await;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => {
                tx.rollback().await?;
                // The gate refusing the source's state means the target's rights differ; the
                // rights action belongs on the target, not in this copy.
                let reason = e.to_string();
                if is_gate_refusal(&e) {
                    skip(
                        report,
                        &cand.key,
                        format!("the target's rights refuse the source's state: {reason}"),
                    );
                } else {
                    report.talks.failed += 1;
                    report.failed.push(Item {
                        key: cand.key.clone(),
                        reason,
                    });
                }
                continue;
            }
        };
        if let Outcome::Skipped(reason) = &outcome {
            tx.rollback().await?;
            skip(report, &cand.key, reason.clone());
            continue;
        }
        if IN_PIPELINE.contains(&cand.state.as_str()) && !has_job(&mut tx, cand.id).await? {
            report.in_pipeline_without_job.push(cand.key.clone());
        }
        if options.apply {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        match outcome {
            Outcome::New => report.talks.new += 1,
            Outcome::Updated => report.talks.updated += 1,
            _ => report.talks.unchanged += 1,
        }
        for (table, counts) in rows {
            let total: &mut RowCounts = report.rows.entry(table).or_default();
            total.insert += counts.insert;
            total.update += counts.update;
            total.delete += counts.delete;
        }
    }

    copy_editions(&mut src, target, &schema, options, report).await
}

/// Records a skipped talk.
fn skip(report: &mut CatchUpReport, key: &str, reason: String) {
    report.talks.skipped += 1;
    report.skipped.push(Item {
        key: key.to_owned(),
        reason,
    });
}

/// True when the gate's rights check refused a talk's state.
fn is_gate_refusal(e: &CatchUpError) -> bool {
    match e {
        CatchUpError::Db(DbError::Sqlx(sqlx::Error::Database(d))) => {
            d.constraint() == Some("past_gate_requires_consent")
        }
        _ => false,
    }
}

/// Whether the target has a queued or running job for the talk.
async fn has_job(conn: &mut PgConnection, talk: Uuid) -> Result<bool, CatchUpError> {
    Ok(sqlx::query_scalar(
        "select exists (select 1 from job where talk_id = $1 and state in ('queued','running'))",
    )
    .bind(talk)
    .fetch_one(&mut *conn)
    .await?)
}

/// Copies one talk inside the target transaction `tx`.
async fn copy_talk(
    src: &mut PgConnection,
    tx: &mut PgConnection,
    schema: &Schema,
    cand: &Candidate,
    rows: &mut BTreeMap<String, RowCounts>,
) -> Result<Outcome, CatchUpError> {
    let (platform, external_id) = cand.key.split_once(':').unwrap_or((&cand.key, ""));
    let on_target = sqlx::query(
        "select t.id, t.state::text as state, t.teacher_id, sv.id as video,
                coalesce(te.opted_out_at is not null, false) as opted_out,
                exists (select 1 from takedown_log l
                        where l.talk_id = t.id
                           or (l.scope = 'teacher' and l.teacher_id = t.teacher_id)) as taken_down
         from source_video sv
         left join talk t on t.source_video_id = sv.id
         left join teacher te on te.id = t.teacher_id
         where sv.platform::text = $1 and sv.external_id = $2",
    )
    .bind(platform)
    .bind(external_id)
    .fetch_optional(&mut *tx)
    .await?;

    let mut is_new = false;
    let mut changed = false;
    match on_target {
        Some(row) => {
            let video: Uuid = row.try_get("video")?;
            let id: Option<Uuid> = row.try_get("id")?;
            if video != cand.video || id.is_some_and(|id| id != cand.id) {
                return Ok(Outcome::Skipped(
                    "the target holds this video under another id".to_owned(),
                ));
            }
            let Some(_) = id else {
                return Ok(Outcome::Skipped(
                    "the target holds this video without a talk".to_owned(),
                ));
            };
            let state: String = row.try_get("state")?;
            if REMOVED.contains(&state.as_str()) {
                return Ok(Outcome::Skipped(format!("{state} on the target")));
            }
            if row.try_get::<bool, _>("opted_out")? {
                return Ok(Outcome::Skipped(
                    "its teacher opted out on the target".to_owned(),
                ));
            }
            if row.try_get::<bool, _>("taken_down")? {
                return Ok(Outcome::Skipped(
                    "a takedown on the target names it".to_owned(),
                ));
            }
            let target_teacher: Option<Uuid> = row.try_get("teacher_id")?;
            changed |= sync_video(src, tx, schema, cand.video, rows).await?;
            match sync_talk(src, tx, schema, cand.id, target_teacher).await? {
                Some(true) => changed = true,
                Some(false) => {}
                None => {
                    return Ok(Outcome::Skipped(
                        "assigned to another teacher on the target".to_owned(),
                    ));
                }
            }
        }
        None => {
            if let Some(reason) = refused_new_talk(src, tx, cand.id).await? {
                return Ok(Outcome::Skipped(reason));
            }
            insert_rows(src, tx, schema, "source_video", &[cand.video]).await?;
            insert_rows(src, tx, schema, "talk", &[cand.id]).await?;
            rows.entry("source_video".to_owned()).or_default().insert += 1;
            rows.entry("talk".to_owned()).or_default().insert += 1;
            is_new = true;
        }
    }

    match sync_children(src, tx, schema, &TALK_CHILDREN, cand.id, rows).await? {
        Ok(c) => changed |= c,
        Err(reason) => return Ok(Outcome::Skipped(reason)),
    }
    if changed && !is_new {
        rows.entry("talk".to_owned()).or_default().update += 1;
    }
    Ok(if is_new {
        Outcome::New
    } else if changed {
        Outcome::Updated
    } else {
        Outcome::Unchanged
    })
}

/// Why a talk the target does not hold must not be copied there, if it must not: its teacher
/// opted out or was taken down there, or a row it names is missing there.
async fn refused_new_talk(
    src: &mut PgConnection,
    tx: &mut PgConnection,
    talk: Uuid,
) -> Result<Option<String>, CatchUpError> {
    let row = sqlx::query(
        "select teacher_id, sangha_id, license_evidence_id, source_key, duplicate_of
         from talk where id = $1",
    )
    .bind(talk)
    .fetch_one(&mut *src)
    .await?;
    let refs: [(&str, &str, Option<String>); 5] = [
        (
            "teacher",
            "id",
            row.try_get::<Option<Uuid>, _>(0)?.map(|u| u.to_string()),
        ),
        (
            "sangha",
            "id",
            row.try_get::<Option<Uuid>, _>(1)?.map(|u| u.to_string()),
        ),
        (
            "license_evidence",
            "id",
            row.try_get::<Option<Uuid>, _>(2)?.map(|u| u.to_string()),
        ),
        ("source_policy", "key", row.try_get::<Option<String>, _>(3)?),
        (
            "talk",
            "id",
            row.try_get::<Option<Uuid>, _>(4)?.map(|u| u.to_string()),
        ),
    ];
    for (table, column, value) in refs {
        let Some(value) = value else { continue };
        let found: bool = sqlx::query_scalar(AssertSqlSafe(format!(
            "select exists (select 1 from {table} where {column}::text = $1)"
        )))
        .bind(&value)
        .fetch_one(&mut *tx)
        .await?;
        if !found {
            return Ok(Some(format!("its {table} {value} is not on the target")));
        }
    }
    if let Some(teacher) = row.try_get::<Option<Uuid>, _>(0)? {
        let (opted_out, taken_down): (bool, bool) = sqlx::query_as(
            "select coalesce((select opted_out_at is not null from teacher where id = $1), false),
                    exists (select 1 from takedown_log
                            where scope = 'teacher' and teacher_id = $1)",
        )
        .bind(teacher)
        .fetch_one(&mut *tx)
        .await?;
        if opted_out {
            return Ok(Some("its teacher opted out on the target".to_owned()));
        }
        if taken_down {
            return Ok(Some(
                "a takedown on the target names its teacher".to_owned(),
            ));
        }
    }
    Ok(None)
}

/// Brings the target's copy of a source video in line with the source. Returns whether it
/// changed.
async fn sync_video(
    src: &mut PgConnection,
    tx: &mut PgConnection,
    schema: &Schema,
    video: Uuid,
    rows: &mut BTreeMap<String, RowCounts>,
) -> Result<bool, CatchUpError> {
    let skip = schema.generated("source_video");
    let a = row_hash(src, "source_video", video, &skip).await?;
    let b = row_hash(tx, "source_video", video, &skip).await?;
    if a == b {
        return Ok(false);
    }
    upsert(src, tx, schema, "source_video", &[video]).await?;
    rows.entry("source_video".to_owned()).or_default().update += 1;
    Ok(true)
}

/// Brings the target's talk row in line with the source in its pipeline columns, and in its
/// assignment when the target has none. Returns whether it changed, or `None` when the two
/// sides assign the talk to different teachers.
async fn sync_talk(
    src: &mut PgConnection,
    tx: &mut PgConnection,
    schema: &Schema,
    talk: Uuid,
    target_teacher: Option<Uuid>,
) -> Result<Option<bool>, CatchUpError> {
    let source_teacher: Option<Uuid> =
        sqlx::query_scalar("select teacher_id from talk where id = $1")
            .bind(talk)
            .fetch_one(&mut *src)
            .await?;
    let adopt = match (target_teacher, source_teacher) {
        (Some(a), Some(b)) if a != b => return Ok(None),
        (Some(_), None) => return Ok(None),
        (None, Some(_)) => true,
        _ => false,
    };
    let mut kept: Vec<String> = TALK_KEPT.map(str::to_owned).to_vec();
    if !adopt {
        kept.extend(ASSIGNMENT.map(str::to_owned));
    }
    let mut excluded = kept.clone();
    excluded.extend(schema.generated("talk"));
    if row_hash(src, "talk", talk, &excluded).await?
        == row_hash(tx, "talk", talk, &excluded).await?
    {
        return Ok(Some(false));
    }
    let cols: Vec<String> = schema
        .writable("talk")
        .iter()
        .filter(|c| !kept.contains(c))
        .cloned()
        .collect();
    let row = fetch_rows(src, "talk", &[talk], &schema.generated("talk")).await?;
    let list = quoted(&cols);
    let sql = format!(
        "update talk set ({list}) = (select {list} from jsonb_populate_record(null::talk, $1))
         where id = $2"
    );
    sqlx::query(AssertSqlSafe(sql))
        .bind(row.into_iter().next().unwrap_or(Value::Null))
        .bind(talk)
        .execute(&mut *tx)
        .await?;
    Ok(Some(true))
}

/// One table's rows under an owner on one side: id to content hash and parent.
type Rows = BTreeMap<Uuid, (String, Option<Uuid>)>;

/// Brings an owner's child rows on the target in line with the source: deletes rows only on
/// the target (refused when a table outside the pipeline names one, or one its deletion would
/// cascade to), then writes rows that are new, changed or cascaded away. Returns whether
/// anything changed, or the reason the owner must be skipped.
async fn sync_children(
    src: &mut PgConnection,
    tx: &mut PgConnection,
    schema: &Schema,
    children: &[Child],
    owner: Uuid,
    rows: &mut BTreeMap<String, RowCounts>,
) -> Result<Result<bool, String>, CatchUpError> {
    let mut sides: Vec<(Rows, Rows)> = Vec::new();
    for child in children {
        let skip = schema.generated(child.table);
        sides.push((
            child_hashes(src, child, owner, &skip).await?,
            child_hashes(tx, child, owner, &skip).await?,
        ));
    }

    // What goes from the target: rows it alone has, and rows whose parent goes.
    let mut gone: BTreeMap<&str, BTreeSet<Uuid>> = BTreeMap::new();
    for (child, (a, b)) in children.iter().zip(&sides) {
        let parent_gone = child
            .parent
            .and_then(|(_, table)| gone.get(table))
            .cloned()
            .unwrap_or_default();
        let set: BTreeSet<Uuid> = b
            .iter()
            .filter(|(id, (_, parent))| {
                !a.contains_key(id) || parent.is_some_and(|p| parent_gone.contains(&p))
            })
            .map(|(id, _)| *id)
            .collect();
        gone.insert(child.table, set);
    }
    for child in children {
        let ids: Vec<Uuid> = gone[child.table].iter().copied().collect();
        if let Some(refs) = schema.protecting.get(child.table) {
            for (table, column) in refs {
                let n: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
                    "select count(*) from {table} where {column} = any($1)"
                )))
                .bind(&ids)
                .fetch_one(&mut *tx)
                .await?;
                if n > 0 {
                    return Ok(Err(format!(
                        "{n} {table} rows on the target name {} rows the copy would delete",
                        child.table
                    )));
                }
            }
        }
    }

    let mut changed = false;
    for child in children.iter().rev() {
        let ids: Vec<Uuid> = gone[child.table].iter().copied().collect();
        if ids.is_empty() {
            continue;
        }
        sqlx::query(AssertSqlSafe(format!(
            "delete from {} where id = any($1)",
            child.table
        )))
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    }
    for (child, (a, b)) in children.iter().zip(&sides) {
        let gone = &gone[child.table];
        let counts = rows.entry(child.table.to_owned()).or_default();
        counts.delete += b.keys().filter(|id| !a.contains_key(id)).count();
        let mut write = Vec::new();
        for (id, (hash, _)) in a {
            match b.get(id) {
                None => counts.insert += 1,
                Some((h, _)) if h != hash || gone.contains(id) => counts.update += 1,
                Some(_) => continue,
            }
            write.push(*id);
        }
        changed |= !write.is_empty() || !gone.is_empty();
        // Rows go over in batches, so a talk with thousands of passages stays within one
        // statement's parameter size.
        for batch in write.chunks(200) {
            upsert(src, tx, schema, child.table, batch).await?;
        }
    }
    Ok(Ok(changed))
}

/// Copies the text units of editions imported on the source after the cut-off, for editions
/// the target holds and has not withdrawn.
async fn copy_editions(
    src: &mut PgConnection,
    target: &PgPool,
    schema: &Schema,
    options: &Options,
    report: &mut CatchUpReport,
) -> Result<(), CatchUpError> {
    let editions: Vec<Uuid> =
        sqlx::query_scalar("select id from text_edition where imported_at > $1 order by id")
            .bind(options.since)
            .fetch_all(&mut *src)
            .await?;
    report.edition_candidates = editions.len();
    for edition in editions {
        let key = edition.to_string();
        let mut tx = target.begin().await?;
        sqlx::query("set local time zone 'UTC'")
            .execute(&mut *tx)
            .await?;
        let state: Option<String> =
            sqlx::query_scalar("select state::text from text_edition where id = $1")
                .bind(edition)
                .fetch_optional(&mut *tx)
                .await?;
        let reason = match state.as_deref() {
            None => Some("edition only on the source; import it on the target".to_owned()),
            Some(s @ ("withdrawn" | "quarantined")) => Some(format!("{s} on the target")),
            Some(_) => None,
        };
        if let Some(reason) = reason {
            tx.rollback().await?;
            report.skipped.push(Item { key, reason });
            continue;
        }
        let mut rows = BTreeMap::new();
        match sync_children(src, &mut tx, schema, &EDITION_CHILDREN, edition, &mut rows).await {
            Ok(Ok(_)) => {
                if options.apply {
                    tx.commit().await?;
                } else {
                    tx.rollback().await?;
                }
                report.editions_copied += 1;
                for (table, counts) in rows {
                    let total = report.rows.entry(table).or_default();
                    total.insert += counts.insert;
                    total.update += counts.update;
                    total.delete += counts.delete;
                }
            }
            Ok(Err(reason)) => {
                tx.rollback().await?;
                report.skipped.push(Item { key, reason });
            }
            Err(e) => {
                tx.rollback().await?;
                report.failed.push(Item {
                    key,
                    reason: e.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// `"a", "b"` for a column list.
fn quoted(cols: &[String]) -> String {
    cols.iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The md5 of one row's columns other than `skip`, or `None` when the row is absent.
async fn row_hash(
    conn: &mut PgConnection,
    table: &str,
    id: Uuid,
    skip: &[String],
) -> Result<Option<String>, CatchUpError> {
    Ok(sqlx::query_scalar(AssertSqlSafe(format!(
        "select md5((to_jsonb(x) - $2::text[])::text) from {table} x where id = $1"
    )))
    .bind(id)
    .bind(skip)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Every row of `child` under `owner`: its hash over the columns other than `skip`, and its
/// parent.
async fn child_hashes(
    conn: &mut PgConnection,
    child: &Child,
    owner: Uuid,
    skip: &[String],
) -> Result<Rows, CatchUpError> {
    let parent = child.parent.map_or("null::uuid", |(column, _)| column);
    let sql = format!(
        "select id, md5((to_jsonb(x) - $2::text[])::text), {parent}
         from {} x where {} = $1",
        child.table, child.owner
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(owner)
        .bind(skip)
        .fetch_all(&mut *conn)
        .await?;
    let mut out = Rows::new();
    for r in rows {
        out.insert(r.try_get(0)?, (r.try_get(1)?, r.try_get(2)?));
    }
    Ok(out)
}

/// The source rows of `table` with these ids, as JSON objects without generated columns.
async fn fetch_rows(
    src: &mut PgConnection,
    table: &str,
    ids: &[Uuid],
    skip: &[String],
) -> Result<Vec<Value>, CatchUpError> {
    Ok(sqlx::query_scalar(AssertSqlSafe(format!(
        "select to_jsonb(x) - $2::text[] from {table} x where id = any($1)"
    )))
    .bind(ids)
    .bind(skip)
    .fetch_all(&mut *src)
    .await?)
}

/// Inserts the source rows of `table` with these ids into the target, which must not hold them.
async fn insert_rows(
    src: &mut PgConnection,
    tx: &mut PgConnection,
    schema: &Schema,
    table: &str,
    ids: &[Uuid],
) -> Result<(), CatchUpError> {
    let rows = fetch_rows(src, table, ids, &schema.generated(table)).await?;
    let list = quoted(schema.writable(table));
    sqlx::query(AssertSqlSafe(format!(
        "insert into {table} ({list})
         select {list} from jsonb_populate_recordset(null::{table}, $1)"
    )))
    .bind(Value::Array(rows))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Writes the source rows of `table` with these ids to the target, inserting or replacing each
/// by id.
async fn upsert(
    src: &mut PgConnection,
    tx: &mut PgConnection,
    schema: &Schema,
    table: &str,
    ids: &[Uuid],
) -> Result<(), CatchUpError> {
    let mut rows = fetch_rows(src, table, ids, &schema.generated(table)).await?;
    // A talk has one current transcript at a time, checked row by row: rows that stop being
    // current are written before the one that becomes current.
    rows.sort_by_key(|r| {
        r.get("is_current")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    let cols = schema.writable(table);
    let list = quoted(cols);
    let set = cols
        .iter()
        .filter(|c| *c != "id")
        .map(|c| format!("\"{c}\" = excluded.\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query(AssertSqlSafe(format!(
        "insert into {table} ({list})
         select {list} from jsonb_populate_recordset(null::{table}, $1)
         on conflict (id) do update set {set}"
    )))
    .bind(Value::Array(rows))
    .execute(&mut *tx)
    .await?;
    Ok(())
}
