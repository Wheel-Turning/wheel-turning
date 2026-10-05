//! The append-only `correction_log`: every merge, exclusion, reclassification and registry change.

use sqlx::PgConnection;

use crate::DbError;

/// Appends one change record.
#[allow(clippy::too_many_arguments)]
pub async fn log_change(
    conn: &mut PgConnection,
    entity: &str,
    entity_id: impl Into<sqlx::types::Uuid>,
    field: &str,
    old_value: Option<serde_json::Value>,
    new_value: Option<serde_json::Value>,
    actor: &str,
    reason: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        r#"insert into correction_log (entity, entity_id, field, old_value, new_value, actor, reason)
           values ($1, $2, $3, $4, $5, $6, $7)"#,
        entity,
        entity_id.into(),
        field,
        old_value,
        new_value,
        actor,
        reason
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}
