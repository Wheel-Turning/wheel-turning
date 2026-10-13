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
/// new transcript current, recording on the talk whether its text is the source's own. The
/// caller wraps this in the transaction that also finishes the job. `question_note` is always
/// null in v1.
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
    // Spec 05, Answer assembly, rule 2: a transcript of the source's own text makes the talk
    // link to the source's transcript and serve at `excerpts` at most.
    sqlx::query!(
        "update talk set transcript_from_source = $2 in ('aligned', 'text') where id = $1",
        talk.as_uuid(),
        transcript.origin
    )
    .execute(&mut *conn)
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

/// The talk's teacher's name and aliases, which name the teacher in a source's speaker labels
/// (Spec 04, Text-only source transcripts). Empty when the talk has no teacher.
pub async fn teacher_names(conn: &mut PgConnection, talk: TalkId) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select te.display_name as "name!" from talk t join teacher te on te.id = t.teacher_id
           where t.id = $1
           union
           select a.alias from talk t join teacher_alias a on a.teacher_id = t.teacher_id
           where t.id = $1"#,
        talk.as_uuid()
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// Talks whose current, unreviewed transcript marks a student gap (a teacher's answer), with
/// the transcript's origin and an optional talk or source filter (Spec 04, Student questions,
/// step 7).
pub async fn student_gap_talks(
    conn: &mut PgConnection,
    talk: Option<TalkId>,
    source: Option<&str>,
) -> Result<Vec<(TalkId, String)>, DbError> {
    let rows = sqlx::query!(
        r#"select t.id, tr.origin from talk t
           join transcript tr on tr.talk_id = t.id and tr.is_current and tr.origin <> 'reviewed'
           where exists (select 1 from segment s where s.talk_id = t.id and s.kind = 'qa_teacher')
             and ($1::uuid is null or t.id = $1)
             and ($2::text is null or t.source_key = $2)
           order by t.id"#,
        talk.map(|t| t.as_uuid()),
        source
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (TalkId::from(r.id), r.origin))
        .collect())
}

/// Every teacher name, alias and sangha name in the registry: an edited student question may
/// hold none of them (Spec 04, Student questions).
pub async fn registry_names(conn: &mut PgConnection) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select display_name as "name!" from teacher
           union select alias from teacher_alias
           union select name from sangha"#
    )
    .fetch_all(&mut *conn)
    .await?)
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

/// Returns talks the pipeline held in `needs_review` for one of `reasons` to `cleared`, logged,
/// with the time each was held, so the caller can queue each hold's retry once. Holds made
/// before the gate (classification's) are not touched: only these reasons are.
pub async fn release_held(
    conn: &mut PgConnection,
    reasons: &[String],
    actor: &str,
) -> Result<Vec<(TalkId, String)>, DbError> {
    let rows = sqlx::query!(
        r#"update talk t set state = 'cleared', state_reason = null
           from talk before
           where before.id = t.id and t.state = 'needs_review' and t.state_reason = any($1)
           returning t.id, before.state_reason as "reason!", before.updated_at"#,
        reasons
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        log_change(
            conn,
            "talk",
            r.id,
            "state",
            Some(serde_json::json!("needs_review")),
            Some(serde_json::json!("cleared")),
            actor,
            &format!("retry after {}", r.reason),
        )
        .await?;
        out.push((
            TalkId::from(r.id),
            format!("{}:{}", r.reason, r.updated_at.to_rfc3339()),
        ));
    }
    Ok(out)
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

/// Up to `limit` talks waiting at `cleared` that have audio to fetch (a matched podcast
/// enclosure, the route `feed::media_route` uses), in random order, with their titles: the
/// sample for `wt segment sample`.
pub async fn sample_cleared(
    conn: &mut PgConnection,
    limit: i64,
) -> Result<Vec<(TalkId, String)>, DbError> {
    let rows = sqlx::query!(
        "select t.id, t.title from talk t
         join source_video sv on sv.id = t.source_video_id
         where t.state = 'cleared'
           and exists (select 1 from source_video e
                       where e.platform = 'rss' and e.raw->'match'->>'vimeo_id' = sv.external_id
                         and sv.platform = 'vimeo'
                         and e.raw->'item'->'enclosure'->>'url' is not null)
         order by random() limit $1",
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

/// Every talk's current transcript object key and origin, for `wt asr requeue`.
pub async fn current_keys(
    conn: &mut PgConnection,
) -> Result<Vec<(TalkId, String, String)>, DbError> {
    let rows = sqlx::query!(
        "select talk_id, s3_key, origin from transcript where is_current order by talk_id"
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (TalkId::from(r.talk_id), r.s3_key, r.origin))
        .collect())
}

/// The current transcript's origin: `asr`, `captions`, `aligned`, `text` or `reviewed`.
pub async fn current_origin(
    conn: &mut PgConnection,
    talk: TalkId,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "select origin from transcript where talk_id = $1 and is_current",
        talk.as_uuid()
    )
    .fetch_optional(&mut *conn)
    .await?)
}

/// What a review file's header shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFacts {
    /// The talk's title.
    pub title: String,
    /// The teacher's display name, or "Teacher unknown".
    pub teacher: String,
    /// The source page.
    pub source_url: String,
}

/// The talk's [`ReviewFacts`].
pub async fn review_facts(conn: &mut PgConnection, talk: TalkId) -> Result<ReviewFacts, DbError> {
    let row = sqlx::query!(
        r#"select t.title, te.display_name as "teacher?", sv.url
           from talk t join source_video sv on sv.id = t.source_video_id
           left join teacher te on te.id = t.teacher_id where t.id = $1"#,
        talk.as_uuid()
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(ReviewFacts {
        title: row.title,
        teacher: row.teacher.unwrap_or_else(|| "Teacher unknown".to_owned()),
        source_url: row.url,
    })
}

/// A segment's kind, times and speaker, for rebuilding segments from a reviewed transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentSpeaker {
    /// `talk` or `qa_teacher`.
    pub kind: String,
    /// Start, milliseconds.
    pub start_ms: i32,
    /// End, milliseconds.
    pub end_ms: i32,
    /// The teacher's diarizer label.
    pub speaker_label: String,
}

/// The talk's segments with their speakers, in order.
pub async fn segment_speakers(
    conn: &mut PgConnection,
    talk: TalkId,
) -> Result<Vec<SegmentSpeaker>, DbError> {
    let rows = sqlx::query!(
        r#"select kind::text as "kind!", start_ms, end_ms, speaker_label from segment
           where talk_id = $1 order by ordinal"#,
        talk.as_uuid()
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SegmentSpeaker {
            kind: r.kind,
            start_ms: r.start_ms,
            end_ms: r.end_ms,
            speaker_label: r.speaker_label.unwrap_or_else(|| "S1".to_owned()),
        })
        .collect())
}

/// A talk with a current transcript, for the owner export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportTalk {
    /// The talk.
    pub id: TalkId,
    /// Its title.
    pub title: String,
    /// When it was given, if known.
    pub talk_date: Option<chrono::NaiveDate>,
    /// Its sangha's name.
    pub sangha: Option<String>,
    /// The source page.
    pub source_url: String,
    /// `asr`, `captions` or `reviewed`.
    pub origin: String,
}

/// Every talk of `teacher` with a current transcript, oldest first, whatever its display level:
/// the owner export is for the teacher's own use, including talks nobody else sees.
pub async fn teacher_transcribed_talks(
    conn: &mut PgConnection,
    teacher: wt_core::ids::TeacherId,
) -> Result<Vec<ExportTalk>, DbError> {
    let rows = sqlx::query!(
        r#"select t.id, t.title, t.talk_date, s.name as "sangha?", sv.url, tr.origin
           from talk t
           join transcript tr on tr.talk_id = t.id and tr.is_current
           join source_video sv on sv.id = t.source_video_id
           left join sangha s on s.id = t.sangha_id
           where t.teacher_id = $1
           order by t.talk_date nulls last, t.title, t.id"#,
        teacher.as_uuid()
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ExportTalk {
            id: TalkId::from(r.id),
            title: r.title,
            talk_date: r.talk_date,
            sangha: r.sangha,
            source_url: r.url,
            origin: r.origin,
        })
        .collect())
}
