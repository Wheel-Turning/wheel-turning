//! `wt export transcripts` (Spec 07, Gate A notebook retrieval test): the passages of every
//! servable talk, read through `servable_passage` like any other reader outside `wt-index`.

use serde::Serialize;
use sqlx::PgConnection;
use sqlx::types::Uuid;

use crate::DbError;

/// One exported passage. No names, titles or audio: the ids are the database's UUIDs, which
/// carry nothing about the teacher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportPassage {
    /// `passage.id`.
    pub passage_id: Uuid,
    /// The talk's UUID.
    pub talk_id: Uuid,
    /// The teacher's UUID.
    pub teacher_id: Uuid,
    /// Start of the passage, milliseconds into the talk.
    pub start_ms: i32,
    /// The corrected passage text.
    pub text: String,
}

/// Every servable passage, by talk and place in the talk.
pub async fn servable_passages(conn: &mut PgConnection) -> Result<Vec<ExportPassage>, DbError> {
    let rows = sqlx::query!(
        r#"select passage_id as "passage_id!", talk_id as "talk_id!",
                  teacher_id as "teacher_id!", start_ms as "start_ms!", text as "text!"
           from servable_passage order by talk_id, ordinal"#
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ExportPassage {
            passage_id: r.passage_id,
            talk_id: r.talk_id,
            teacher_id: r.teacher_id,
            start_ms: r.start_ms,
            text: r.text,
        })
        .collect())
}
