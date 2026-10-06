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

/// The talk's current transcript: version, object key and content hash.
pub async fn current(
    conn: &mut PgConnection,
    talk: TalkId,
) -> Result<Option<(i32, String, String)>, DbError> {
    let row = sqlx::query!(
        "select version, s3_key, content_hash from transcript
         where talk_id = $1 and is_current order by version desc limit 1",
        talk.as_uuid()
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| (r.version, r.s3_key, r.content_hash)))
}

/// One stored segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRow {
    /// Its id.
    pub id: Uuid,
    /// Its place in the talk.
    pub ordinal: i32,
    /// `talk` or `qa_teacher`.
    pub kind: String,
    /// Start, milliseconds.
    pub start_ms: i32,
    /// End, milliseconds.
    pub end_ms: i32,
    /// Its text: uncorrected until the correct step, corrected after.
    pub text: String,
}

/// The talk's segments in order.
pub async fn segments(conn: &mut PgConnection, talk: TalkId) -> Result<Vec<SegmentRow>, DbError> {
    let rows = sqlx::query!(
        r#"select id, ordinal, kind::text as "kind!", start_ms, end_ms, text from segment
           where talk_id = $1 order by ordinal"#,
        talk.as_uuid()
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SegmentRow {
            id: r.id,
            ordinal: r.ordinal,
            kind: r.kind,
            start_ms: r.start_ms,
            end_ms: r.end_ms,
            text: r.text,
        })
        .collect())
}

/// Replaces a segment's text with its corrected form.
pub async fn set_segment_text(
    conn: &mut PgConnection,
    id: Uuid,
    text: &str,
) -> Result<(), DbError> {
    sqlx::query!("update segment set text = $2 where id = $1", id, text)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Facts about a talk the text jobs need: whether it is a fixture (data class), and its series
/// and part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TalkFacts {
    /// `source_video.raw.synthetic` is true.
    pub synthetic: bool,
    /// Its series, if any.
    pub series: Option<String>,
    /// Its part, if any.
    pub part: Option<i32>,
}

/// The talk's [`TalkFacts`].
pub async fn talk_facts(conn: &mut PgConnection, talk: TalkId) -> Result<TalkFacts, DbError> {
    let row = sqlx::query!(
        r#"select coalesce((sv.raw->>'synthetic')::boolean, false) as "synthetic!", t.series, t.part
           from talk t join source_video sv on sv.id = t.source_video_id where t.id = $1"#,
        talk.as_uuid()
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(TalkFacts {
        synthetic: row.synthetic,
        series: row.series,
        part: row.part,
    })
}

/// Talks with a current transcript that are past segmenting, for re-queuing `correct` after a
/// glossary change.
pub async fn corrected_talks(conn: &mut PgConnection) -> Result<Vec<TalkId>, DbError> {
    let rows = sqlx::query_scalar!(
        "select distinct t.id from talk t join transcript tr on tr.talk_id = t.id and tr.is_current
         where t.state in ('correcting','extracting','resolving','indexing','ready')
         order by t.id"
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(TalkId::from).collect())
}

/// Up to `limit` talks waiting at `cleared`, in random order, with their titles: the sample for
/// `wt segment sample`.
pub async fn sample_cleared(
    conn: &mut PgConnection,
    limit: i64,
) -> Result<Vec<(TalkId, String)>, DbError> {
    let rows = sqlx::query!(
        "select id, title from talk where state = 'cleared' order by random() limit $1",
        limit
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (TalkId::from(r.id), r.title))
        .collect())
}

/// A talk's title.
pub async fn title(conn: &mut PgConnection, talk: TalkId) -> Result<Option<String>, DbError> {
    Ok(
        sqlx::query_scalar!("select title from talk where id = $1", talk.as_uuid())
            .fetch_optional(&mut *conn)
            .await?,
    )
}

/// Every talk's current transcript object key, for `wt asr requeue`.
pub async fn current_keys(conn: &mut PgConnection) -> Result<Vec<(TalkId, String)>, DbError> {
    let rows =
        sqlx::query!("select talk_id, s3_key from transcript where is_current order by talk_id")
            .fetch_all(&mut *conn)
            .await?;
    Ok(rows
        .into_iter()
        .map(|r| (TalkId::from(r.talk_id), r.s3_key))
        .collect())
}
