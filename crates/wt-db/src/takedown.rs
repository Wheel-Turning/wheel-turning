//! Takedown (Spec 07), skeleton for M2: the database transaction and its record.
//!
//! A teacher can pull any talk or all talks at any time. Takedown deletes text and is the only
//! route to `withdrawn`. The stub `talk` and `source_video` rows remain so discovery never
//! recreates a removed talk. Object-store deletion (step 3) arrives with the transcript
//! pipeline in M4, when transcripts first reach S3; until then the object stores are listed in
//! the report as not checked.

use std::collections::BTreeMap;

use sqlx::types::Uuid;
use sqlx::{PgConnection, PgPool};
use wt_core::answer::AnswerPayload;
use wt_core::gate::ConsentStatus;
use wt_core::ids::{PassageId, TalkId, TeacherId};
use wt_core::stores::{OnTakedown, STORES, StoreKind};

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
    /// Object stores not yet handled by this build, with the reason.
    pub objects_not_checked: BTreeMap<String, String>,
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
    sqlx::query!(
        r#"insert into takedown_log (scope, teacher_id, talk_id, actor, counts)
           values ($1, $2, $3, $4, $5)"#,
        scope_text,
        teacher.map(|t| t.as_uuid()),
        talk_col,
        actor,
        counts
    )
    .execute(&mut *tx)
    .await?;

    Ok(report)
}

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
            StoreKind::ObjectPrefix { .. } => {
                report.objects_not_checked.insert(
                    store.name.to_owned(),
                    "object-store deletion arrives with the transcript pipeline (M4)".to_owned(),
                );
            }
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
