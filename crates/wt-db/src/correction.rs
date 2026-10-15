//! The append-only `correction_log`: every merge, exclusion, reclassification and registry change.

use sqlx::PgConnection;

use crate::DbError;

/// One change record: a row of `correction_log`.
#[derive(Debug, Clone)]
pub struct Change<'a> {
    /// The kind of record changed, such as `talk` or `teacher`.
    pub entity: &'a str,
    /// The changed record's id.
    pub entity_id: sqlx::types::Uuid,
    /// The field changed.
    pub field: &'a str,
    /// Its value before, if it had one.
    pub old_value: Option<serde_json::Value>,
    /// Its value after, if it has one.
    pub new_value: Option<serde_json::Value>,
    /// Who made the change.
    pub actor: &'a str,
    /// Why.
    pub reason: &'a str,
}

/// Appends one change record.
pub async fn log_change(conn: &mut PgConnection, change: &Change<'_>) -> Result<(), DbError> {
    sqlx::query!(
        r#"insert into correction_log (entity, entity_id, field, old_value, new_value, actor, reason)
           values ($1, $2, $3, $4, $5, $6, $7)"#,
        change.entity,
        change.entity_id,
        change.field,
        change.old_value,
        change.new_value,
        change.actor,
        change.reason
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}
