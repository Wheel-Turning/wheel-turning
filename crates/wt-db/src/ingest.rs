//! Discovery and classification storage (Spec 03): recording discovered videos as
//! `source_video` and `talk` rows with a `classify` job, and the reads and writes the
//! classification rules need. The rules themselves live in `wt-ingest`.
//!
//! Classification only ever touches a talk before the gate: in `discovered`, `needs_review` or
//! `awaiting_consent`. A talk that is cleared, in the pipeline, excluded or withdrawn is left
//! alone, and a withdrawn talk's stub row is never re-queued, so a takedown blocks re-ingest.

use sqlx::PgConnection;
use sqlx::types::chrono::{DateTime, NaiveDate, Utc};
use wt_core::ids::{JobId, TalkId};

use crate::DbError;
use crate::correction::log_change;
use crate::job::{self, JobError, JobKind};

/// States classification may change.
pub const PRE_GATE: [&str; 3] = ["discovered", "needs_review", "awaiting_consent"];

/// Errors from recording or classifying.
#[derive(Debug, thiserror::Error)]
pub enum IngestDbError {
    /// No talk with that id.
    #[error("talk {0} does not exist")]
    NotFound(TalkId),
    /// Enqueueing the classify job failed.
    #[error(transparent)]
    Job(#[from] JobError),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for IngestDbError {
    fn from(e: sqlx::Error) -> Self {
        IngestDbError::Db(DbError::Sqlx(e))
    }
}

/// A discovered video, ready to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewVideo<'a> {
    /// The configured source's name, kept in `raw.source` and the classify payload.
    pub source: &'a str,
    /// `source_platform` value.
    pub platform: &'a str,
    /// The platform's id.
    pub external_id: &'a str,
    /// Canonical page address.
    pub url: &'a str,
    /// Title as published.
    pub title: &'a str,
    /// Description.
    pub description: Option<&'a str>,
    /// Publish or upload time.
    pub published_at: Option<DateTime<Utc>>,
    /// Length in seconds.
    pub duration_s: Option<i32>,
    /// Account or channel.
    pub channel_ref: Option<&'a str>,
    /// Whether the source holds a captions or text track.
    pub has_captions: bool,
    /// The platform's record.
    pub item: &'a serde_json::Value,
    /// Hash of the metadata classification reads, so changed metadata queues a new run.
    pub input_hash: &'a str,
}

/// What [`record_video`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// A new video and talk, with a classify job.
    New {
        /// The talk.
        talk: TalkId,
    },
    /// A known video whose metadata changed, re-queued for classification.
    Changed {
        /// The talk.
        talk: TalkId,
    },
    /// A known video with the same metadata, or one past the gate: nothing to do.
    Unchanged {
        /// The talk.
        talk: TalkId,
    },
    /// A video whose talk was withdrawn. Its stub blocks re-ingest.
    Withdrawn {
        /// The talk.
        talk: TalkId,
    },
}

/// Stores one discovered video. A new video gets a `source_video` row, a `talk` in state
/// `discovered` and a `classify` job. A known one has its stored metadata refreshed, and its
/// talk is re-queued only when the metadata changed and the talk is still before the gate.
pub async fn record_video(
    conn: &mut PgConnection,
    v: &NewVideo<'_>,
) -> Result<Recorded, IngestDbError> {
    let raw = serde_json::json!({
        "source": v.source,
        "has_captions": v.has_captions,
        "item": v.item,
    });
    let video = sqlx::query!(
        r#"insert into source_video (platform, external_id, url, channel_ref, title, description,
                                     published_at, duration_s, raw)
           values ($1::text::source_platform, $2, $3, $4, $5, $6, $7, $8, $9)
           on conflict (platform, external_id) do update
             set url = excluded.url, channel_ref = excluded.channel_ref, title = excluded.title,
                 description = excluded.description, published_at = excluded.published_at,
                 duration_s = excluded.duration_s, raw = excluded.raw
           returning id, (xmax = 0) as "inserted!""#,
        v.platform,
        v.external_id,
        v.url,
        v.channel_ref,
        v.title,
        v.description,
        v.published_at,
        v.duration_s,
        raw
    )
    .fetch_one(&mut *conn)
    .await?;

    let existing = sqlx::query!(
        r#"select id, state::text as "state!" from talk where source_video_id = $1"#,
        video.id
    )
    .fetch_optional(&mut *conn)
    .await?;
    let (talk, state) = match existing {
        Some(row) => (TalkId::from(row.id), row.state),
        None => {
            let id = sqlx::query_scalar!(
                "insert into talk (source_video_id, title, talk_date, duration_s)
                 values ($1, $2, ($3::timestamptz at time zone 'UTC')::date, $4)
                 returning id",
                video.id,
                v.title,
                v.published_at,
                v.duration_s
            )
            .fetch_one(&mut *conn)
            .await?;
            (TalkId::from(id), "discovered".to_owned())
        }
    };
    if state == "withdrawn" {
        return Ok(Recorded::Withdrawn { talk });
    }
    if !PRE_GATE.contains(&state.as_str()) {
        return Ok(Recorded::Unchanged { talk });
    }
    if !video.inserted {
        sqlx::query!(
            // Not the date: classification derives it, and an unchanged video queues no job to
            // put back a recording date the upload date would overwrite.
            "update talk set title = $2, duration_s = $3 where id = $1",
            talk.as_uuid(),
            v.title,
            v.duration_s
        )
        .execute(&mut *conn)
        .await?;
    }
    let payload = serde_json::json!({ "source": v.source });
    let queued: Option<JobId> =
        job::enqueue(conn, JobKind::Classify, Some(talk), v.input_hash, &payload).await?;
    Ok(match (video.inserted, queued) {
        (true, _) => Recorded::New { talk },
        (false, Some(_)) => Recorded::Changed { talk },
        (false, None) => Recorded::Unchanged { talk },
    })
}

/// What classification reads about a talk.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifyFacts {
    /// The talk's title.
    pub title: String,
    /// The source video's description.
    pub description: Option<String>,
    /// Publish time.
    pub published_at: Option<DateTime<Utc>>,
    /// Length in seconds.
    pub duration_s: Option<i32>,
    /// Current state.
    pub state: String,
    /// Current reason, if any.
    pub state_reason: Option<String>,
    /// Whether the source holds captions.
    pub has_captions: bool,
}

/// Loads a talk's classification facts.
pub async fn facts(conn: &mut PgConnection, talk: TalkId) -> Result<ClassifyFacts, IngestDbError> {
    let row = sqlx::query!(
        r#"select t.title, sv.description, sv.published_at, t.duration_s,
                  t.state::text as "state!", t.state_reason,
                  coalesce((sv.raw->>'has_captions')::boolean, false) as "has_captions!"
           from talk t join source_video sv on sv.id = t.source_video_id
           where t.id = $1"#,
        talk.as_uuid()
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(IngestDbError::NotFound(talk))?;
    Ok(ClassifyFacts {
        title: row.title,
        description: row.description,
        published_at: row.published_at,
        duration_s: row.duration_s,
        state: row.state,
        state_reason: row.state_reason,
        has_captions: row.has_captions,
    })
}

/// A possible duplicate of the talk being classified.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The other talk.
    pub talk: TalkId,
    /// Its title as published, for the recording date it states.
    pub title: String,
    /// Its title, normalized the same way as `wt_ingest::classify::normalize_title`.
    pub normalized_title: String,
    /// `pg_trgm` similarity of the two normalized titles.
    pub similarity: f32,
    /// Publish time.
    pub published_at: Option<DateTime<Utc>>,
    /// Length in seconds.
    pub duration_s: Option<i32>,
    /// Whether its source holds captions.
    pub has_captions: bool,
    /// Its state.
    pub state: String,
}

/// Other live talks whose length is within `max_gap_s` of this one, with their title
/// similarity. The caller applies rule 3 exactly; this only narrows the search.
pub async fn duplicate_candidates(
    conn: &mut PgConnection,
    talk: TalkId,
    normalized_title: &str,
    duration_s: i32,
    max_gap_s: i32,
) -> Result<Vec<Candidate>, IngestDbError> {
    let rows = sqlx::query!(
        r#"with c as (
             select t.id, t.title, t.state::text as state, t.duration_s, sv.published_at,
                    coalesce((sv.raw->>'has_captions')::boolean, false) as has_captions,
                    trim(regexp_replace(lower(normalize(t.title, NFKC)), '[^[:alnum:]]+', ' ', 'g'))
                      as norm
             from talk t join source_video sv on sv.id = t.source_video_id
             where t.id <> $1 and t.duplicate_of is null
               and t.state not in ('excluded', 'withdrawn')
               and t.duration_s between $3::int - $4::int and $3::int + $4::int)
           select id as "id!", title as "title!", state as "state!", duration_s, published_at,
                  has_captions as "has_captions!", norm as "norm!",
                  similarity(norm, $2) as "similarity!"
           from c"#,
        talk.as_uuid(),
        normalized_title,
        duration_s,
        max_gap_s
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Candidate {
            talk: TalkId::from(r.id),
            title: r.title,
            normalized_title: r.norm,
            similarity: r.similarity,
            published_at: r.published_at,
            duration_s: r.duration_s,
            has_captions: r.has_captions,
            state: r.state,
        })
        .collect())
}

/// Moves a talk before the gate to `excluded` with `reason`, and with `talk_type` and
/// `duplicate_of` when given, logging the change. Returns false if the talk had already
/// moved on, in which case nothing changed.
pub async fn exclude(
    conn: &mut PgConnection,
    talk: TalkId,
    reason: &str,
    talk_type: Option<&str>,
    duplicate_of: Option<TalkId>,
    actor: &str,
) -> Result<bool, IngestDbError> {
    let old = sqlx::query!(
        r#"update talk t set state = 'excluded', state_reason = $2,
                  talk_type = coalesce($3::text::talk_type, t.talk_type), duplicate_of = $4
           from talk before
           where t.id = $1 and before.id = t.id
             and t.state in ('discovered', 'needs_review', 'awaiting_consent')
           returning before.state::text as "state!", before.state_reason"#,
        talk.as_uuid(),
        reason,
        talk_type,
        duplicate_of.map(|d| d.as_uuid())
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
        Some(serde_json::json!({ "state": old.state, "reason": old.state_reason })),
        Some(serde_json::json!({
            "state": "excluded", "reason": reason, "talk_type": talk_type,
            "duplicate_of": duplicate_of,
        })),
        actor,
        if duplicate_of.is_some() {
            "duplicate merged at classification"
        } else {
            "excluded at classification"
        },
    )
    .await?;
    Ok(true)
}

/// Rules 4 and 5: records type, series and part, and holds the talk in `needs_review` when
/// `review` names a reason. A talk already held for a reason other than its speaker keeps
/// that reason. Logs any change. Returns false if the talk had already moved past the gate.
pub async fn set_classification(
    conn: &mut PgConnection,
    talk: TalkId,
    talk_type: &str,
    series: Option<&str>,
    part: Option<i32>,
    review: Option<&str>,
    actor: &str,
) -> Result<bool, IngestDbError> {
    let row = sqlx::query!(
        r#"update talk t set talk_type = $2::text::talk_type, series = $3, part = $4,
                  state = case when $5::text is null then t.state
                               when t.state = 'needs_review' and t.state_reason is distinct from 'speaker_unresolved'
                                 then t.state
                               else 'needs_review' end,
                  state_reason = case when $5::text is null then t.state_reason
                               when t.state = 'needs_review' and t.state_reason is distinct from 'speaker_unresolved'
                                 then t.state_reason
                               else $5 end
           from talk before
           where t.id = $1 and before.id = t.id
             and t.state in ('discovered', 'needs_review', 'awaiting_consent')
           returning before.talk_type::text as "old_type!", before.series as old_series,
                     before.part as old_part, before.state::text as "old_state!",
                     before.state_reason as old_reason,
                     t.state::text as "new_state!", t.state_reason as new_reason"#,
        talk.as_uuid(),
        talk_type,
        series,
        part,
        review
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let before = serde_json::json!({
        "talk_type": row.old_type, "series": row.old_series, "part": row.old_part,
        "state": row.old_state, "reason": row.old_reason,
    });
    let after = serde_json::json!({
        "talk_type": talk_type, "series": series, "part": part,
        "state": row.new_state, "reason": row.new_reason,
    });
    if before != after {
        log_change(
            conn,
            "talk",
            talk.as_uuid(),
            "classification",
            Some(before),
            Some(after),
            actor,
            "classified",
        )
        .await?;
    }
    Ok(true)
}

/// One talk as the ingest report shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportRow {
    /// The talk.
    pub talk: TalkId,
    /// Platform of its source video.
    pub platform: String,
    /// The platform's id.
    pub external_id: String,
    /// Canonical page address.
    pub url: String,
    /// Title.
    pub title: String,
    /// The source video's description.
    pub description: Option<String>,
    /// Publish time.
    pub published_at: Option<DateTime<Utc>>,
    /// The talk's date: its recording date where known, else the upload date.
    pub talk_date: Option<NaiveDate>,
    /// Length in seconds.
    pub duration_s: Option<i32>,
    /// Whether its source holds captions.
    pub has_captions: bool,
    /// State.
    pub state: String,
    /// Reason for the state.
    pub state_reason: Option<String>,
    /// Talk type.
    pub talk_type: String,
    /// Series, if any.
    pub series: Option<String>,
    /// Part, if any.
    pub part: Option<i32>,
    /// The kept talk, when this one is a duplicate.
    pub duplicate_of: Option<TalkId>,
    /// The text the speaker was inferred from, or the candidates.
    pub speaker_guess: Option<String>,
    /// The resolved teacher's display name.
    pub teacher: Option<String>,
}

/// Every talk, for the ingest report. Read-only.
pub async fn report_rows(conn: &mut PgConnection) -> Result<Vec<ReportRow>, IngestDbError> {
    let rows = sqlx::query!(
        r#"select t.id, sv.platform::text as "platform!", sv.external_id, sv.url, t.title,
                  sv.description, sv.published_at, t.talk_date, t.duration_s,
                  coalesce((sv.raw->>'has_captions')::boolean, false) as "has_captions!",
                  t.state::text as "state!", t.state_reason, t.talk_type::text as "talk_type!",
                  t.series, t.part, t.duplicate_of, t.speaker_guess, te.display_name as "teacher?"
           from talk t
           join source_video sv on sv.id = t.source_video_id
           left join teacher te on te.id = t.teacher_id
           order by sv.published_at nulls last, sv.external_id"#
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ReportRow {
            talk: TalkId::from(r.id),
            platform: r.platform,
            external_id: r.external_id,
            url: r.url,
            title: r.title,
            description: r.description,
            published_at: r.published_at,
            talk_date: r.talk_date,
            duration_s: r.duration_s,
            has_captions: r.has_captions,
            state: r.state,
            state_reason: r.state_reason,
            talk_type: r.talk_type,
            series: r.series,
            part: r.part,
            duplicate_of: r.duplicate_of.map(TalkId::from),
            speaker_guess: r.speaker_guess,
            teacher: r.teacher,
        })
        .collect())
}

/// Exclusion reasons that classification itself set, and so may undo when its rules change.
/// Hand exclusions and refusals end in `withdrawn` through a takedown and are never reopened.
pub const CLASSIFIER_EXCLUSIONS: [&str; 4] =
    ["non_talk", "too_short", "excluded_session", "duplicate"];

/// A talk `wt ingest reclassify` may run classification on again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclassifyTarget {
    /// The talk.
    pub talk: TalkId,
    /// The configured source it came from, from `source_video.raw.source`.
    pub source: Option<String>,
    /// Its state.
    pub state: String,
}

/// Talks before the gate, and talks excluded by classification, optionally only `only`.
pub async fn reclassify_targets(
    conn: &mut PgConnection,
    only: Option<TalkId>,
) -> Result<Vec<ReclassifyTarget>, IngestDbError> {
    let rows = sqlx::query!(
        r#"select t.id, sv.raw->>'source' as source, t.state::text as "state!"
           from talk t join source_video sv on sv.id = t.source_video_id
           where (t.state in ('discovered', 'needs_review', 'awaiting_consent')
                  or (t.state = 'excluded' and t.state_reason = any($1)))
             and ($2::uuid is null or t.id = $2)
           order by sv.published_at nulls last, t.id"#,
        &CLASSIFIER_EXCLUSIONS.map(str::to_owned)[..],
        only.map(|t| t.as_uuid())
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ReclassifyTarget {
            talk: TalkId::from(r.id),
            source: r.source,
            state: r.state,
        })
        .collect())
}

/// Returns a talk excluded by classification to `discovered`, clearing `duplicate_of`, so the
/// rules run on it afresh. Logs the change. Returns false for any other talk.
pub async fn reopen(
    conn: &mut PgConnection,
    talk: TalkId,
    actor: &str,
) -> Result<bool, IngestDbError> {
    let old = sqlx::query!(
        r#"update talk t set state = 'discovered', state_reason = null, duplicate_of = null
           from talk before
           where t.id = $1 and before.id = t.id
             and t.state = 'excluded' and t.state_reason = any($2)
           returning before.state_reason, before.duplicate_of"#,
        talk.as_uuid(),
        &CLASSIFIER_EXCLUSIONS.map(str::to_owned)[..]
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
        Some(serde_json::json!({
            "state": "excluded", "reason": old.state_reason, "duplicate_of": old.duplicate_of,
        })),
        Some(serde_json::json!({ "state": "discovered" })),
        actor,
        "reopened for reclassification",
    )
    .await?;
    Ok(true)
}

/// Every alias with its teacher, in a stable order, for the rules version.
pub async fn alias_list(conn: &mut PgConnection) -> Result<Vec<(String, String)>, IngestDbError> {
    let rows = sqlx::query!(
        "select teacher_id::text as \"teacher!\", alias from teacher_alias order by 1, 2"
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.teacher, r.alias)).collect())
}

/// Records a talk's date, from the recording date its title states or else its upload date.
/// Only talks before the gate change. Derived from the title, so not logged.
pub async fn set_talk_date(
    conn: &mut PgConnection,
    talk: TalkId,
    date: Option<sqlx::types::chrono::NaiveDate>,
) -> Result<(), IngestDbError> {
    sqlx::query!(
        "update talk set talk_date = $2
         where id = $1 and talk_date is distinct from $2
           and state in ('discovered', 'needs_review', 'awaiting_consent')",
        talk.as_uuid(),
        date
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Another upload merged into a talk on a feed episode's evidence (reason `duplicate_upload`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedUpload {
    /// Its title.
    pub title: String,
    /// When its source video was published.
    pub published_at: Option<DateTime<Utc>>,
}

/// The uploads merged into `keeper` as other copies of its recording.
pub async fn merged_uploads(
    conn: &mut PgConnection,
    keeper: TalkId,
) -> Result<Vec<MergedUpload>, IngestDbError> {
    let rows = sqlx::query!(
        "select t.title, sv.published_at
         from talk t join source_video sv on sv.id = t.source_video_id
         where t.duplicate_of = $1 and t.state = 'excluded' and t.state_reason = 'duplicate_upload'
         order by sv.published_at nulls last, sv.external_id",
        keeper.as_uuid()
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| MergedUpload {
            title: r.title,
            published_at: r.published_at,
        })
        .collect())
}

/// Gives a talk before the gate the recording date another upload of it states, logged since
/// it comes from that upload rather than the talk's own title. Returns whether it changed.
pub async fn carry_talk_date(
    conn: &mut PgConnection,
    talk: TalkId,
    date: NaiveDate,
    actor: &str,
) -> Result<bool, IngestDbError> {
    let old = sqlx::query!(
        "update talk t set talk_date = $2
         from talk before
         where t.id = $1 and before.id = t.id and t.talk_date is distinct from $2
           and t.state in ('discovered', 'needs_review', 'awaiting_consent')
         returning before.talk_date",
        talk.as_uuid(),
        date
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
        "talk_date",
        Some(serde_json::json!(old.talk_date.map(|d| d.to_string()))),
        Some(serde_json::json!(date.to_string())),
        actor,
        "recording date stated by another upload of the same recording",
    )
    .await?;
    Ok(true)
}
