//! Transcripts and segments (Spec 04, Diarization and segmentation, steps 6 and 7): the
//! `transcript` row pointing at the teacher-only JSON, and the `segment` rows of a talk, written
//! in one transaction. Only `talk` and `qa_teacher` segments can be stored, and the database
//! refuses `qa_student` as well.

use sqlx::PgConnection;
use sqlx::types::Uuid;
use wt_core::ids::TalkId;

use crate::DbError;
use crate::correction::log_change;

/// One segment to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSegment<'a> {
    /// `talk` or `qa_teacher`.
    pub kind: &'a str,
    /// Start, milliseconds.
    pub start_ms: i32,
    /// End, milliseconds.
    pub end_ms: i32,
    /// The teacher's diarizer label.
    pub speaker_label: &'a str,
    /// The teacher's words, uncorrected.
    pub text: &'a str,
}

/// The transcript to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTranscript<'a> {
    /// Its version for the talk.
    pub version: i32,
    /// `asr` or `captions`.
    pub origin: &'a str,
    /// The object key of the teacher-only JSON.
    pub key: &'a str,
    /// SHA-256 of that JSON.
    pub content_hash: &'a str,
    /// Teacher words in it.
    pub word_count: i32,
}

/// The next transcript version for a talk: one more than the highest stored.
pub async fn next_version(conn: &mut PgConnection, talk: TalkId) -> Result<i32, DbError> {
    let max = sqlx::query_scalar!(
        "select max(version) from transcript where talk_id = $1",
        talk.as_uuid()
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(max.unwrap_or(0) + 1)
}

/// Stores a transcript and its segments, replacing the talk's earlier segments, and makes the
/// new transcript current. The caller wraps this in the transaction that also finishes the
/// job. `question_note` is always null in v1.
pub async fn store(
    conn: &mut PgConnection,
    talk: TalkId,
    transcript: &NewTranscript<'_>,
    segments: &[NewSegment<'_>],
) -> Result<Uuid, DbError> {
    sqlx::query!(
        "update transcript set is_current = false where talk_id = $1 and is_current",
        talk.as_uuid()
    )
    .execute(&mut *conn)
    .await?;
    let id = sqlx::query_scalar!(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash, word_count)
         values ($1, $2, $3, $4, $5, $6) returning id",
        talk.as_uuid(),
        transcript.version,
        transcript.origin,
        transcript.key,
        transcript.content_hash,
        transcript.word_count
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query!("delete from segment where talk_id = $1", talk.as_uuid())
        .execute(&mut *conn)
        .await?;
    for (ordinal, s) in segments.iter().enumerate() {
        sqlx::query!(
            "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms,
                                  speaker_label, text, question_note)
             values ($1, $2, $3, $4::text::segment_kind, $5, $6, $7, $8, null)",
            talk.as_uuid(),
            id,
            i32::try_from(ordinal).unwrap_or(i32::MAX),
            s.kind,
            s.start_ms,
            s.end_ms,
            s.speaker_label,
            s.text
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(id)
}

/// Holds a talk in the pipeline for a person: `needs_review` with `reason`, such as
/// `multi_speaker` or `no_media`, logged in `correction_log`. Returns whether it moved.
pub async fn hold_for_review(
    conn: &mut PgConnection,
    talk: TalkId,
    reason: &str,
    actor: &str,
) -> Result<bool, DbError> {
    let old = sqlx::query_scalar!(
        r#"update talk t set state = 'needs_review', state_reason = $2
           from talk before
           where t.id = $1 and before.id = t.id
             and t.state in ('cleared','fetching','transcribing','diarizing','segmenting')
           returning before.state::text as "state!""#,
        talk.as_uuid(),
        reason
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(old) = old else {
        return Ok(false);
    };
    log_change(
        conn,
        "talk",
        talk.as_uuid(),
        "state",
        Some(serde_json::json!(old)),
        Some(serde_json::json!("needs_review")),
        actor,
        reason,
    )
    .await?;
    Ok(true)
}
