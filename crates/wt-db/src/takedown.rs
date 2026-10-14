//! Takedown (Spec 07): the database transaction and its record, then the objects.
//!
//! A teacher can pull any talk or all talks at any time. Takedown deletes text and is the only
//! route to `withdrawn`. The stub `talk` and `source_video` rows remain so discovery never
//! recreates a removed talk. After the transaction commits, [`complete`] deletes the talks'
//! objects under every registered prefix and confirms nothing is left, and copies the
//! `takedown_log` row to the object store, where [`replay`] finds it after a database restore
//! that predates it. [`finish`] retries object deletion for every logged scope.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sqlx::types::Uuid;
use sqlx::{PgConnection, PgPool};
use wt_core::answer::AnswerPayload;
use wt_core::gate::ConsentStatus;
use wt_core::ids::{PassageId, TalkId, TeacherId};
use wt_core::objects::{ObjectError, ObjectStore};
use wt_core::stores::{self, OnTakedown, STORES, StoreKind};

use crate::DbError;
use crate::registry::{RegistryDbError, find_teacher};

/// What to take down.
#[derive(Debug, Clone)]
pub enum Scope {
    /// Every talk of a teacher, by display name or alias. Also records `withdrawn` consent.
    Teacher(String),
    /// One talk.
    Talk(TalkId),
}

/// Rows or objects removed per store, plus answer and rating cleanup.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct TakedownReport {
    /// Talks in scope.
    pub talks: Vec<TalkId>,
    /// Rows deleted per registered table (or that would be, in a dry run).
    pub rows: BTreeMap<String, i64>,
    /// `answer_passage` rows removed through the passage cascade.
    pub answer_passage_rows: i64,
    /// Stored answers whose themes cited a removed passage.
    pub answers_rewritten: i64,
    /// Themes stripped from those answers.
    pub themes_stripped: i64,
    /// Evaluation ratings whose notes were cleared.
    pub rating_notes_cleared: i64,
    /// Objects per registered object store: found, in a dry run with a store, or deleted.
    pub objects: BTreeMap<String, i64>,
    /// The `takedown_log` row, after a real run.
    pub log_id: Option<Uuid>,
    /// True when nothing was changed.
    pub dry_run: bool,
}

/// Takedown errors.
#[derive(Debug, thiserror::Error)]
pub enum TakedownError {
    /// The teacher or talk could not be found.
    #[error(transparent)]
    Registry(#[from] RegistryDbError),
    /// No talk with that id.
    #[error("talk {0} does not exist")]
    UnknownTalk(TalkId),
    /// A stored answer could not be read.
    #[error("answer {0} has a payload that is not an AnswerPayload: {1}")]
    BadPayload(Uuid, serde_json::Error),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
    /// Object storage refused, or objects remain after deletion. The `takedown_log` row stays
    /// as the record of what is left, and `wt takedown --finish` retries.
    #[error("object storage: {0}")]
    Objects(#[from] ObjectError),
    /// A stored takedown record could not be read.
    #[error("takedown record {0}: {1}")]
    Record(String, String),
}

impl From<sqlx::Error> for TakedownError {
    fn from(e: sqlx::Error) -> Self {
        TakedownError::Db(DbError::Sqlx(e))
    }
}

/// Counts what a takedown would remove and changes nothing.
pub async fn dry_run(pool: &PgPool, scope: &Scope) -> Result<TakedownReport, TakedownError> {
    let mut tx = pool.begin().await?;
    let (talks, _) = resolve_scope(&mut tx, scope).await?;
    let mut report = count(&mut tx, &talks).await?;
    report.dry_run = true;
    tx.rollback().await?;
    Ok(report)
}

/// Runs the takedown's database step in one transaction. Serving stops the moment it commits.
/// A teacher scope also records `withdrawn` consent.
pub async fn run(
    pool: &PgPool,
    scope: &Scope,
    actor: &str,
) -> Result<TakedownReport, TakedownError> {
    let mut tx = pool.begin().await?;
    let consent = if let Scope::Teacher(name) = scope {
        let (teacher_id, _) = find_teacher(&mut tx, name).await?;
        sqlx::query!(
            r#"insert into speaker_consent (teacher_id, status, decided_at, created_by)
               values ($1, 'withdrawn', now(), $2)"#,
            teacher_id.as_uuid(),
            actor
        )
        .execute(&mut *tx)
        .await?;
        Some(ConsentStatus::Withdrawn)
    } else {
        None
    };
    let report = run_in(&mut tx, scope, actor, consent, "takedown").await?;
    tx.commit().await?;
    Ok(report)
}

/// The takedown's database step inside the caller's transaction, so a consent change and its
/// takedown commit together. `consent` is written to the talks' `consent_status`; recording the
/// `speaker_consent` row is the caller's job. `reason` becomes the talks' `state_reason`.
pub(crate) async fn run_in(
    tx: &mut PgConnection,
    scope: &Scope,
    actor: &str,
    consent: Option<ConsentStatus>,
    reason: &str,
) -> Result<TakedownReport, TakedownError> {
    let (talks, teacher) = resolve_scope(tx, scope).await?;
    let mut report = count(tx, &talks).await?;
    let talk_ids: Vec<Uuid> = talks.iter().map(TalkId::as_uuid).collect();

    let removed: Vec<Uuid> =
        sqlx::query_scalar!("select id from passage where talk_id = any($1)", &talk_ids)
            .fetch_all(&mut *tx)
            .await?;
    let removed_ids: Vec<PassageId> = removed.iter().copied().map(PassageId::from).collect();
    let removed_text: Vec<String> = removed.iter().map(Uuid::to_string).collect();

    // Strip themes that cite removed passages from stored answers. This runs before the
    // passages go, while answer_passage still links them.
    let answers = sqlx::query!(
        r#"select id, payload from answer
           where exists (select 1 from jsonb_array_elements(coalesce(payload->'themes', '[]')) th,
                                       jsonb_array_elements_text(coalesce(th->'passage_ids', '[]')) p
                         where p = any($1))
              or exists (select 1 from jsonb_array_elements_text(coalesce(payload->'passage_ids', '[]')) p
                         where p = any($1))
           for update"#,
        &removed_text
    )
    .fetch_all(&mut *tx)
    .await?;
    for answer in answers {
        let mut payload: AnswerPayload = serde_json::from_value(answer.payload)
            .map_err(|e| TakedownError::BadPayload(answer.id, e))?;
        report.themes_stripped += i64::try_from(payload.strip_passages(&removed_ids)).unwrap_or(0);
        let value =
            serde_json::to_value(&payload).map_err(|e| TakedownError::BadPayload(answer.id, e))?;
        sqlx::query!(
            "update answer set payload = $2 where id = $1",
            answer.id,
            value
        )
        .execute(&mut *tx)
        .await?;
        report.answers_rewritten += 1;
    }

    report.rating_notes_cleared = i64::try_from(
        sqlx::query!(
            "update eval_rating set note = null where passage_id = any($1) and note is not null",
            &removed
        )
        .execute(&mut *tx)
        .await?
        .rows_affected(),
    )
    .unwrap_or(0);

    // Delete in dependency order. passage cascades to citation and answer_passage.
    sqlx::query!("delete from passage where talk_id = any($1)", &talk_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("delete from citation where talk_id = any($1)", &talk_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "delete from student_question where talk_id = any($1)",
        &talk_ids
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("delete from segment where talk_id = any($1)", &talk_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("delete from transcript where talk_id = any($1)", &talk_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("delete from job where talk_id = any($1)", &talk_ids)
        .execute(&mut *tx)
        .await?;

    sqlx::query!(
        r#"update talk set state = 'withdrawn', state_reason = $3, content_hash = null,
                  consent_status = coalesce($2::text::consent_status, consent_status)
           where id = any($1)"#,
        &talk_ids,
        consent.map(ConsentStatus::as_str),
        reason
    )
    .execute(&mut *tx)
    .await?;

    let counts = serde_json::to_value(&report).unwrap_or_default();
    let (scope_text, talk_col) = match scope {
        Scope::Teacher(_) => ("teacher", None),
        Scope::Talk(id) => ("talk", Some(id.as_uuid())),
    };
    report.log_id = Some(
        sqlx::query_scalar!(
            r#"insert into takedown_log (scope, teacher_id, talk_id, actor, counts)
               values ($1, $2, $3, $4, $5) returning id"#,
            scope_text,
            teacher.map(|t| t.as_uuid()),
            talk_col,
            actor,
            counts
        )
        .fetch_one(&mut *tx)
        .await?,
    );

    Ok(report)
}

/// The talks a scope covers, and the teacher when it names one.
async fn resolve_scope(
    conn: &mut PgConnection,
    scope: &Scope,
) -> Result<(Vec<TalkId>, Option<TeacherId>), TakedownError> {
    match scope {
        Scope::Teacher(name) => {
            let (teacher_id, _) = find_teacher(conn, name).await?;
            let talks = sqlx::query_scalar!(
                "select id from talk where teacher_id = $1 order by id for update",
                teacher_id.as_uuid()
            )
            .fetch_all(&mut *conn)
            .await?;
            Ok((
                talks.into_iter().map(TalkId::from).collect(),
                Some(teacher_id),
            ))
        }
        Scope::Talk(id) => {
            let found =
                sqlx::query_scalar!("select id from talk where id = $1 for update", id.as_uuid())
                    .fetch_optional(&mut *conn)
                    .await?;
            match found {
                Some(_) => Ok((vec![*id], None)),
                None => Err(TakedownError::UnknownTalk(*id)),
            }
        }
    }
}

/// What a takedown of these talks would remove, counted per store.
async fn count(conn: &mut PgConnection, talks: &[TalkId]) -> Result<TakedownReport, TakedownError> {
    let ids: Vec<Uuid> = talks.iter().map(TalkId::as_uuid).collect();
    let mut report = TakedownReport {
        talks: talks.to_vec(),
        ..TakedownReport::default()
    };
    for store in STORES
        .iter()
        .filter(|s| s.on_takedown == OnTakedown::Delete)
    {
        match store.kind {
            StoreKind::Table { table, talk_column } => {
                // Names come from the compile-time registry, never from input.
                let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "select count(*) from {table} where {talk_column} = any($1)"
                )))
                .bind(&ids)
                .fetch_one(&mut *conn)
                .await?;
                report.rows.insert(store.name.to_owned(), n);
            }
            // Objects are counted by `count_objects`, which needs the store.
            StoreKind::ObjectPrefix { .. } => {}
        }
    }
    report.answer_passage_rows = sqlx::query_scalar!(
        r#"select count(*) as "n!" from answer_passage ap join passage p on p.id = ap.passage_id
           where p.talk_id = any($1)"#,
        &ids
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(report)
}

// ---- objects, records, finish and replay ------------------------------------------------------

/// Objects under each registered object store for these talks.
pub fn count_objects(
    store: &dyn ObjectStore,
    talks: &[TalkId],
) -> Result<BTreeMap<String, i64>, ObjectError> {
    let mut out = BTreeMap::new();
    for talk in talks {
        for (name, prefix) in stores::talk_objects(&talk.as_uuid().to_string()) {
            let n = i64::try_from(store.list(&prefix)?.len()).unwrap_or(i64::MAX);
            *out.entry(name.to_owned()).or_insert(0) += n;
        }
    }
    Ok(out)
}

/// Deletes the talks' objects under every registered prefix, then lists each prefix to confirm
/// it is empty. Returns the objects deleted per store.
pub fn delete_objects(
    store: &dyn ObjectStore,
    talks: &[TalkId],
) -> Result<BTreeMap<String, i64>, ObjectError> {
    let mut out = BTreeMap::new();
    for talk in talks {
        for (name, prefix) in stores::talk_objects(&talk.as_uuid().to_string()) {
            let n = i64::try_from(store.delete_prefix(&prefix)?).unwrap_or(i64::MAX);
            *out.entry(name.to_owned()).or_insert(0) += n;
            if !store.list(&prefix)?.is_empty() {
                return Err(ObjectError::Remains(prefix));
            }
        }
    }
    Ok(out)
}

/// A dry run that also counts the objects takedown would delete.
pub async fn dry_run_with_objects(
    pool: &PgPool,
    scope: &Scope,
    store: &dyn ObjectStore,
) -> Result<TakedownReport, TakedownError> {
    let mut report = dry_run(pool, scope).await?;
    report.objects = count_objects(store, &report.talks)?;
    Ok(report)
}

/// The whole takedown: the database step, then the record copied to the object store, then the
/// talks' objects. If object deletion fails, the database step has already committed and
/// `takedown_log` records the scope, so [`finish`] can retry and the audit fails until it does.
pub async fn complete(
    pool: &PgPool,
    scope: &Scope,
    actor: &str,
    store: &dyn ObjectStore,
) -> Result<TakedownReport, TakedownError> {
    let mut report = run(pool, scope, actor).await?;
    write_records(pool, store).await?;
    report.objects = delete_objects(store, &report.talks)?;
    Ok(report)
}

/// A `takedown_log` row as kept outside the database. It holds ids and counts, never words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakedownRecord {
    /// The `takedown_log` row.
    pub log_id: Uuid,
    /// `teacher` or `talk`.
    pub scope: String,
    /// For a teacher scope.
    pub teacher_id: Option<Uuid>,
    /// For a talk scope.
    pub talk_id: Option<Uuid>,
    /// Every talk the takedown covered.
    pub talks: Vec<TalkId>,
    /// Each talk's source video as `platform:external_id`, so a talk a restore lost can be
    /// found again after discovery recreates it.
    #[serde(default)]
    pub sources: BTreeMap<TalkId, String>,
    /// Who ran it.
    pub actor: String,
    /// When it committed.
    pub at: chrono::DateTime<chrono::Utc>,
}

/// Where a record is kept in the object store.
pub fn record_key(log_id: Uuid) -> String {
    format!("takedowns/{log_id}.json")
}

/// Every `takedown_log` row as a record.
async fn logged(conn: &mut PgConnection) -> Result<Vec<TakedownRecord>, TakedownError> {
    let rows = sqlx::query!(
        "select id, scope, teacher_id, talk_id, actor, counts, created_at
         from takedown_log order by created_at, id"
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let talks = r
                .counts
                .get("talks")
                .cloned()
                .and_then(|t| serde_json::from_value(t).ok())
                .unwrap_or_else(|| r.talk_id.map(TalkId::from).into_iter().collect());
            TakedownRecord {
                log_id: r.id,
                scope: r.scope,
                teacher_id: r.teacher_id,
                talk_id: r.talk_id,
                talks,
                sources: BTreeMap::new(),
                actor: r.actor,
                at: r.created_at,
            }
        })
        .collect())
}

/// Copies every `takedown_log` row that has no record yet to the object store. Returns how many
/// it wrote.
pub async fn write_records(pool: &PgPool, store: &dyn ObjectStore) -> Result<usize, TakedownError> {
    let mut conn = pool.acquire().await?;
    let mut written = 0;
    for mut record in logged(&mut conn).await? {
        let key = record_key(record.log_id);
        if store.get(&key)?.is_none() {
            let ids: Vec<Uuid> = record.talks.iter().map(TalkId::as_uuid).collect();
            let rows = sqlx::query!(
                r#"select t.id, sv.platform::text || ':' || sv.external_id as "source!"
                   from talk t join source_video sv on sv.id = t.source_video_id
                   where t.id = any($1)"#,
                &ids
            )
            .fetch_all(&mut *conn)
            .await?;
            record.sources = rows
                .into_iter()
                .map(|r| (TalkId::from(r.id), r.source))
                .collect();
            let json = serde_json::to_vec(&record)
                .map_err(|e| TakedownError::Record(key.clone(), e.to_string()))?;
            store.put(&key, &json)?;
            written += 1;
        }
    }
    Ok(written)
}

/// Records from the database and the object store, merged by log id, oldest first.
async fn all_records(
    conn: &mut PgConnection,
    store: &dyn ObjectStore,
) -> Result<Vec<TakedownRecord>, TakedownError> {
    let mut by_id: BTreeMap<Uuid, TakedownRecord> = logged(conn)
        .await?
        .into_iter()
        .map(|r| (r.log_id, r))
        .collect();
    for key in store.list("takedowns/")? {
        let Some(bytes) = store.get(&key)? else {
            continue;
        };
        let record: TakedownRecord = serde_json::from_slice(&bytes)
            .map_err(|e| TakedownError::Record(key.clone(), e.to_string()))?;
        by_id.entry(record.log_id).or_insert(record);
    }
    let mut records: Vec<TakedownRecord> = by_id.into_values().collect();
    records.sort_by_key(|r| (r.at, r.log_id));
    Ok(records)
}

/// What `wt takedown --finish` did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FinishReport {
    /// Logged scopes looked at.
    pub scopes: usize,
    /// Talks whose objects were checked.
    pub talks: usize,
    /// Objects deleted per store.
    pub objects: BTreeMap<String, i64>,
    /// Records copied to the object store.
    pub records_written: usize,
}

/// Retries object deletion for every logged scope and every withdrawn talk, and copies any
/// missing records.
pub async fn finish(pool: &PgPool, store: &dyn ObjectStore) -> Result<FinishReport, TakedownError> {
    let mut conn = pool.acquire().await?;
    let records = logged(&mut conn).await?;
    let withdrawn: Vec<Uuid> =
        sqlx::query_scalar!("select id from talk where state = 'withdrawn' order by id")
            .fetch_all(&mut *conn)
            .await?;
    drop(conn);
    let talks: BTreeSet<TalkId> = records
        .iter()
        .flat_map(|r| r.talks.iter().copied())
        .chain(withdrawn.into_iter().map(TalkId::from))
        .collect();
    let talks: Vec<TalkId> = talks.into_iter().collect();
    let records_written = write_records(pool, store).await?;
    Ok(FinishReport {
        scopes: records.len(),
        talks: talks.len(),
        objects: delete_objects(store, &talks)?,
        records_written,
    })
}

/// What `wt takedown --replay` did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReplayReport {
    /// Records read from the database and the object store.
    pub records: usize,
    /// Talks taken down again because rows had come back or they were no longer withdrawn.
    pub talks_reapplied: Vec<TalkId>,
    /// Teachers whose withdrawn consent was recorded again.
    pub consent_withdrawn_again: Vec<Uuid>,
    /// Recorded talks the database does not hold, such as talks discovered after the
    /// snapshot, with their source video where recorded. They have no text; once discovery
    /// finds the source again, exclude the new talk (runbook, procedure 6).
    pub missing_talks: Vec<(TalkId, Option<String>)>,
    /// Objects deleted per store.
    pub objects: BTreeMap<String, i64>,
}

/// Reapplies every recorded takedown, from the database's own log and the copies in the object
/// store, after a restore. A talk already withdrawn with nothing left is skipped, so replaying
/// twice changes nothing the second time.
pub async fn replay(
    pool: &PgPool,
    store: &dyn ObjectStore,
    actor: &str,
) -> Result<ReplayReport, TakedownError> {
    let mut conn = pool.acquire().await?;
    let records = all_records(&mut conn, store).await?;
    drop(conn);
    let mut report = ReplayReport {
        records: records.len(),
        ..ReplayReport::default()
    };
    let mut seen = BTreeSet::new();
    for record in &records {
        let mut tx = pool.begin().await?;
        // A withdrawal is recorded again only when the current consent row predates the
        // takedown: a later grant is the teacher's own decision and stands.
        let mut consent = None;
        if let (Some(teacher), "teacher") = (record.teacher_id, record.scope.as_str()) {
            let current = sqlx::query!(
                r#"select status::text as "status!", created_at as "created_at!"
                   from current_consent where teacher_id = $1"#,
                teacher
            )
            .fetch_optional(&mut *tx)
            .await?;
            let stale = current.is_none_or(|c| c.status != "withdrawn" && c.created_at < record.at);
            if stale {
                sqlx::query!(
                    r#"insert into speaker_consent (teacher_id, status, decided_at, created_by)
                       values ($1, 'withdrawn', now(), $2)"#,
                    teacher,
                    actor
                )
                .execute(&mut *tx)
                .await?;
                report.consent_withdrawn_again.push(teacher);
                consent = Some(ConsentStatus::Withdrawn);
            }
        }
        for talk in &record.talks {
            let state = sqlx::query_scalar!(
                r#"select state::text as "state!" from talk where id = $1"#,
                talk.as_uuid()
            )
            .fetch_optional(&mut *tx)
            .await?;
            let Some(state) = state else {
                if seen.insert(*talk) {
                    let source = record.sources.get(talk).cloned();
                    report.missing_talks.push((*talk, source));
                }
                continue;
            };
            let left = count(&mut tx, std::slice::from_ref(talk)).await?;
            let rows: i64 = left.rows.values().sum();
            if state != "withdrawn" || rows > 0 || consent.is_some() {
                run_in(
                    &mut tx,
                    &Scope::Talk(*talk),
                    actor,
                    consent,
                    "takedown_replay",
                )
                .await?;
                if seen.insert(*talk) {
                    report.talks_reapplied.push(*talk);
                }
            }
        }
        tx.commit().await?;
    }
    let talks: BTreeSet<TalkId> = records
        .iter()
        .flat_map(|r| r.talks.iter().copied())
        .collect();
    let talks: Vec<TalkId> = talks.into_iter().collect();
    report.objects = delete_objects(store, &talks)?;
    write_records(pool, store).await?;
    Ok(report)
}
