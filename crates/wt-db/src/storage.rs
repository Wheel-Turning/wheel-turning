//! Storage matches (Spec 03, the storage adapter; Seattle Soto Zen sources item 2c). A file the
//! owner received from a sangha, listed by a `storage` source, is matched to the talk record it
//! holds, and the match is kept on that record's `source_video` in `raw.storage`: the source,
//! the file's key under the source's root, its size and its fingerprint. Rediscovering the video
//! keeps it, as a feed episode's match is kept. No audio is stored: only where the owner's file
//! is.

use sqlx::PgConnection;
use sqlx::types::chrono::{DateTime, NaiveDate, Utc};
use wt_core::ids::TalkId;

use crate::correction::{Change, log_change};
use crate::ingest::IngestDbError;

/// A storage file a talk's media can be read from (route c).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageRef {
    /// The storage source's name, such as `ssz-storage`, whose `root` the key is under.
    pub source: String,
    /// The file's path under the root, with `/` between parts.
    pub key: String,
    /// The file's size in bytes when it was matched.
    pub bytes: u64,
    /// `sha256(key, size, modified time)` when it was matched: what the match is keyed by.
    pub fingerprint: String,
}

impl StorageRef {
    /// The stored match, if `raw.storage` holds one.
    pub fn from_raw(value: Option<serde_json::Value>) -> Option<Self> {
        serde_json::from_value(value?).ok()
    }
}

/// A live talk from the listed Vimeo sources, as the storage matcher sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct StorageTalk {
    /// The talk.
    pub talk: TalkId,
    /// Its Vimeo id.
    pub vimeo_id: String,
    /// Its title as published.
    pub title: String,
    /// Its date: the recording date, or else the upload date.
    pub talk_date: Option<NaiveDate>,
    /// Its upload time.
    pub published_at: Option<DateTime<Utc>>,
    /// Its length in seconds.
    pub duration_s: Option<i32>,
    /// Its state.
    pub state: String,
    /// Why it is held, if it is.
    pub state_reason: Option<String>,
    /// The storage file already matched to it.
    pub storage: Option<StorageRef>,
    /// `pg_trgm` similarity of its normalized title to the one asked about; 0 when none was.
    pub similarity: f32,
}

/// Live talks of `vimeo_sources`, each with its title's similarity to `normalized_title`
/// (normalized as in rule 1), ordered by date.
pub async fn storage_talks(
    conn: &mut PgConnection,
    vimeo_sources: &[String],
    normalized_title: &str,
) -> Result<Vec<StorageTalk>, IngestDbError> {
    let rows = sqlx::query!(
        r#"select t.id, sv.external_id, t.title, t.talk_date, sv.published_at, t.duration_s,
                  t.state::text as "state!", t.state_reason, sv.raw->'storage' as storage,
                  similarity(
                    trim(regexp_replace(lower(normalize(t.title, NFKC)), '[^[:alnum:]]+', ' ', 'g')),
                    $2) as "similarity!"
           from talk t join source_video sv on sv.id = t.source_video_id
           where sv.platform = 'vimeo' and sv.raw->>'source' = any($1)
             and t.state not in ('excluded', 'withdrawn')
           order by t.talk_date nulls last, sv.external_id"#,
        vimeo_sources,
        normalized_title
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| StorageTalk {
            talk: TalkId::from(r.id),
            vimeo_id: r.external_id,
            title: r.title,
            talk_date: r.talk_date,
            published_at: r.published_at,
            duration_s: r.duration_s,
            state: r.state,
            state_reason: r.state_reason,
            storage: StorageRef::from_raw(r.storage),
            similarity: r.similarity,
        })
        .collect())
}

/// Records that the Vimeo video `vimeo_id` of one of `vimeo_sources` is held by `file`, and
/// logs it. Any other record holding the same file loses it, so one file serves one talk.
/// Returns false, changing and logging nothing, when the record already holds this file
/// unchanged; a video not among `vimeo_sources` is `UnknownVideo`.
pub async fn set_storage(
    conn: &mut PgConnection,
    vimeo_sources: &[String],
    vimeo_id: &str,
    file: &StorageRef,
    how: &str,
    actor: &str,
) -> Result<bool, IngestDbError> {
    let Some(row) = sqlx::query!(
        r#"select id, raw->'storage' as storage from source_video
           where platform = 'vimeo' and external_id = $1 and raw->>'source' = any($2)"#,
        vimeo_id,
        vimeo_sources
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Err(IngestDbError::UnknownVideo(vimeo_id.to_owned()));
    };
    let old = StorageRef::from_raw(row.storage.clone());
    if old.as_ref() == Some(file) {
        return Ok(false);
    }
    let others = sqlx::query!(
        r#"update source_video set raw = raw - 'storage'
           where id <> $1 and raw->'storage'->>'source' = $2 and raw->'storage'->>'key' = $3
           returning id, raw->'storage' as storage"#,
        row.id,
        file.source,
        file.key
    )
    .fetch_all(&mut *conn)
    .await?;
    for other in others {
        log_change(
            conn,
            &Change {
                entity: "source_video",
                entity_id: other.id,
                field: "storage",
                old_value: other.storage,
                new_value: None,
                actor,
                reason: "storage file matched to another talk",
            },
        )
        .await?;
    }
    let mut value = serde_json::to_value(file).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.insert("how".to_owned(), serde_json::json!(how));
    }
    sqlx::query!(
        "update source_video set raw = raw || jsonb_build_object('storage', $2::jsonb) where id = $1",
        row.id,
        value
    )
    .execute(&mut *conn)
    .await?;
    log_change(
        conn,
        &Change {
            entity: "source_video",
            entity_id: row.id,
            field: "storage",
            old_value: row.storage,
            new_value: Some(value),
            actor,
            reason: "storage file matched to a Vimeo talk",
        },
    )
    .await?;
    Ok(true)
}
