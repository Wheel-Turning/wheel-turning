//! Quarantine for a talk that fails a quality check (Spec 03, Talk states; Spec 07, runbook
//! procedure 4). Quarantine takes a ready talk out of serving at once and deletes nothing.
//! Release re-checks it and returns it to `ready`. Both are recorded in `correction_log`.

use sqlx::PgPool;
use wt_core::ids::TalkId;

use crate::DbError;
use crate::audit::RIGHTS_VALID;
use crate::correction::{Change, log_change};

/// Why a quarantine or release was refused.
#[derive(Debug, thiserror::Error)]
pub enum QuarantineError {
    /// The talk is not in a state the command applies to, or fails the re-check.
    #[error("{0}")]
    Refused(String),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for QuarantineError {
    fn from(e: sqlx::Error) -> Self {
        QuarantineError::Db(DbError::Sqlx(e))
    }
}

/// Takes a ready talk out of serving with a reason.
pub async fn quarantine(
    pool: &PgPool,
    talk: TalkId,
    reason: &str,
    by: &str,
) -> Result<(), QuarantineError> {
    let mut tx = pool.begin().await?;
    let updated = sqlx::query_scalar!(
        "update talk set state = 'quarantined', quarantine_reason = $2
         where id = $1 and state = 'ready' returning id",
        talk.as_uuid(),
        reason
    )
    .fetch_optional(&mut *tx)
    .await?;
    if updated.is_none() {
        return Err(QuarantineError::Refused(format!(
            "talk {talk} is not ready, so there is nothing serving to quarantine"
        )));
    }
    log_change(
        &mut tx,
        &Change {
            entity: "talk",
            entity_id: talk.as_uuid(),
            field: "state",
            old_value: Some(serde_json::json!("ready")),
            new_value: Some(serde_json::json!("quarantined")),
            actor: by,
            reason,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Returns a talk this command quarantined to `ready`, after re-checking that it still has
/// passages and a valid rights basis. A talk the pipeline quarantined goes back through
/// `wt pipeline retry` instead, since it never finished.
pub async fn release(
    pool: &PgPool,
    talk: TalkId,
    note: &str,
    by: &str,
) -> Result<(), QuarantineError> {
    let mut tx = pool.begin().await?;
    let state = sqlx::query_scalar!(
        r#"select state::text as "state!" from talk where id = $1 for update"#,
        talk.as_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| QuarantineError::Refused(format!("no talk {talk}")))?;
    if state != "quarantined" {
        return Err(QuarantineError::Refused(format!(
            "talk {talk} is {state}, not quarantined"
        )));
    }
    let from_ready = sqlx::query_scalar!(
        r#"select new_value = '"quarantined"'::jsonb and old_value = '"ready"'::jsonb as "ok!"
           from correction_log where entity = 'talk' and entity_id = $1 and field = 'state'
           order by created_at desc limit 1"#,
        talk.as_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(false);
    if !from_ready {
        return Err(QuarantineError::Refused(format!(
            "talk {talk} was quarantined by the pipeline before it was ready; repair it and \
             requeue it with wt pipeline retry"
        )));
    }
    let passages = crate::audit::held_passages(&mut tx, talk).await?;
    // Names come from a constant in this crate, never from input.
    let rights_ok: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select ({RIGHTS_VALID}) from talk t where t.id = $1"
    )))
    .bind(talk.as_uuid())
    .fetch_one(&mut *tx)
    .await?;
    if passages == 0 || !rights_ok {
        return Err(QuarantineError::Refused(format!(
            "talk {talk} fails the re-check: {passages} passages, rights basis {}",
            if rights_ok { "valid" } else { "not valid" }
        )));
    }
    sqlx::query!(
        "update talk set state = 'ready', quarantine_reason = null where id = $1",
        talk.as_uuid()
    )
    .execute(&mut *tx)
    .await?;
    log_change(
        &mut tx,
        &Change {
            entity: "talk",
            entity_id: talk.as_uuid(),
            field: "state",
            old_value: Some(serde_json::json!("quarantined")),
            new_value: Some(serde_json::json!("ready")),
            actor: by,
            reason: note,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
