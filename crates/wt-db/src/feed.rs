//! Podcast feed episodes (Spec 03, Seattle Soto Zen sources item 2a). An `audio_only` feed
//! never creates a talk: each episode is a `source_video` of platform `rss` with no talk, and
//! a match to a Vimeo talk is recorded on the episode, in `raw.match`, so rediscovering the
//! Vimeo video cannot overwrite it. A matched talk's media route is the episode's enclosure.

use sqlx::PgConnection;
use sqlx::types::chrono::{DateTime, NaiveDate, Utc};
use wt_core::ids::{SourceVideoId, TalkId};

use crate::correction::log_change;
use crate::ingest::IngestDbError;

/// A feed episode, ready to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEpisode<'a> {
    /// The feed source's name.
    pub source: &'a str,
    /// The episode's guid, or its enclosure address.
    pub external_id: &'a str,
    /// The episode's page, or its enclosure.
    pub url: &'a str,
    /// Title.
    pub title: &'a str,
    /// Description.
    pub description: Option<&'a str>,
    /// Publish time.
    pub published_at: Option<DateTime<Utc>>,
    /// Length in seconds.
    pub duration_s: Option<i32>,
    /// The feed's address.
    pub channel_ref: Option<&'a str>,
    /// The item as parsed, with its enclosure.
    pub item: &'a serde_json::Value,
}

/// Stores an episode, or refreshes a known one, keeping any match already recorded. Returns
/// the row and whether it was new.
pub async fn record_episode(
    conn: &mut PgConnection,
    e: &NewEpisode<'_>,
) -> Result<(SourceVideoId, bool), IngestDbError> {
    let raw = serde_json::json!({ "source": e.source, "item": e.item });
    let row = sqlx::query!(
        r#"insert into source_video (platform, external_id, url, channel_ref, title, description,
                                     published_at, duration_s, raw)
           values ('rss', $1, $2, $3, $4, $5, $6, $7, $8)
           on conflict (platform, external_id) do update
             set url = excluded.url, channel_ref = excluded.channel_ref, title = excluded.title,
                 description = excluded.description, published_at = excluded.published_at,
                 duration_s = excluded.duration_s,
                 raw = excluded.raw || jsonb_strip_nulls(
                         jsonb_build_object('match', source_video.raw->'match'))
           returning id, (xmax = 0) as "inserted!""#,
        e.external_id,
        e.url,
        e.channel_ref,
        e.title,
        e.description,
        e.published_at,
        e.duration_s,
        raw
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok((SourceVideoId::from(row.id), row.inserted))
}

/// An episode not yet matched to a talk.
#[derive(Debug, Clone, PartialEq)]
pub struct Unmatched {
    /// The episode's row.
    pub id: SourceVideoId,
    /// Title.
    pub title: String,
    /// Title normalized the same way as rule 1.
    pub normalized_title: String,
    /// Publish time.
    pub published_at: Option<DateTime<Utc>>,
    /// Length in seconds.
    pub duration_s: Option<i32>,
}

/// The source's episodes that have no match yet.
pub async fn unmatched_episodes(
    conn: &mut PgConnection,
    source: &str,
) -> Result<Vec<Unmatched>, IngestDbError> {
    let rows = sqlx::query!(
        r#"select id, title, published_at, duration_s,
                  trim(regexp_replace(lower(normalize(title, NFKC)), '[^[:alnum:]]+', ' ', 'g'))
                    as "norm!"
           from source_video
           where platform = 'rss' and raw->>'source' = $1 and raw->'match' is null
           order by published_at nulls last, id"#,
        source
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Unmatched {
            id: SourceVideoId::from(r.id),
            title: r.title,
            normalized_title: r.norm,
            published_at: r.published_at,
            duration_s: r.duration_s,
        })
        .collect())
}

/// A Vimeo talk an episode might belong to.
#[derive(Debug, Clone, PartialEq)]
pub struct TalkCandidate {
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
    /// `pg_trgm` similarity of the two normalized titles.
    pub similarity: f32,
}

/// Live Vimeo talks from `vimeo_sources` within `max_gap_s` of `duration_s` that no other
/// episode has matched yet, with their title similarity to `normalized_title`.
pub async fn talk_candidates(
    conn: &mut PgConnection,
    vimeo_sources: &[String],
    normalized_title: &str,
    duration_s: i32,
    max_gap_s: i32,
) -> Result<Vec<TalkCandidate>, IngestDbError> {
    let rows = sqlx::query!(
        r#"select t.id, sv.external_id, t.title, t.talk_date, sv.published_at, t.duration_s,
                  similarity(
                    trim(regexp_replace(lower(normalize(t.title, NFKC)), '[^[:alnum:]]+', ' ', 'g')),
                    $2) as "similarity!"
           from talk t join source_video sv on sv.id = t.source_video_id
           where sv.platform = 'vimeo' and sv.raw->>'source' = any($1)
             and t.state not in ('excluded', 'withdrawn')
             and t.duration_s between $3::int - $4::int and $3::int + $4::int
             and not exists (select 1 from source_video e
                             where e.platform = 'rss' and e.raw->'match'->>'vimeo_id' = sv.external_id)"#,
        vimeo_sources,
        normalized_title,
        duration_s,
        max_gap_s
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| TalkCandidate {
            talk: TalkId::from(r.id),
            vimeo_id: r.external_id,
            title: r.title,
            talk_date: r.talk_date,
            published_at: r.published_at,
            duration_s: r.duration_s,
            similarity: r.similarity,
        })
        .collect())
}

/// Records that `episode` is the audio of the talk with Vimeo id `vimeo_id`, and logs it.
pub async fn set_match(
    conn: &mut PgConnection,
    episode: SourceVideoId,
    vimeo_id: &str,
    how: &str,
    actor: &str,
) -> Result<(), IngestDbError> {
    let matched = serde_json::json!({ "vimeo_id": vimeo_id, "how": how });
    sqlx::query!(
        "update source_video set raw = raw || jsonb_build_object('match', $2::jsonb) where id = $1",
        episode.as_uuid(),
        matched
    )
    .execute(&mut *conn)
    .await?;
    log_change(
        conn,
        "source_video",
        episode.as_uuid(),
        "match",
        None,
        Some(matched),
        actor,
        "feed episode matched to a Vimeo talk",
    )
    .await?;
    Ok(())
}

/// Where a talk's audio can come from, in the spec's order of preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaRoute {
    /// A matched podcast episode's enclosure. Nothing is downloaded from Vimeo.
    FeedEnclosure {
        /// The enclosure address.
        url: String,
        /// Its MIME type, as the feed states it.
        mime_type: Option<String>,
        /// The episode's row.
        episode: SourceVideoId,
    },
    /// No route is known yet. Vimeo downloads and the storage adapter arrive with M4.
    None,
}

/// The talk's media route.
pub async fn media_route(
    conn: &mut PgConnection,
    talk: TalkId,
) -> Result<MediaRoute, IngestDbError> {
    let row = sqlx::query!(
        r#"select e.id, e.raw->'item'->'enclosure'->>'url' as url,
                  e.raw->'item'->'enclosure'->>'type' as mime_type
           from talk t
           join source_video sv on sv.id = t.source_video_id
           join source_video e on e.platform = 'rss' and e.raw->'match'->>'vimeo_id' = sv.external_id
           where t.id = $1 and sv.platform = 'vimeo'
           limit 1"#,
        talk.as_uuid()
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match row {
        Some(r) => match r.url {
            Some(url) => MediaRoute::FeedEnclosure {
                url,
                mime_type: r.mime_type,
                episode: SourceVideoId::from(r.id),
            },
            None => MediaRoute::None,
        },
        None => MediaRoute::None,
    })
}

/// One episode, for the completeness check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeRow {
    /// The episode's guid.
    pub external_id: String,
    /// Title.
    pub title: String,
    /// Length in seconds.
    pub duration_s: Option<i32>,
    /// The Vimeo id it matched, if any.
    pub matched_vimeo_id: Option<String>,
}

/// Every episode of `source`.
pub async fn episodes(
    conn: &mut PgConnection,
    source: &str,
) -> Result<Vec<EpisodeRow>, IngestDbError> {
    let rows = sqlx::query!(
        r#"select external_id, title, duration_s, raw->'match'->>'vimeo_id' as matched
           from source_video where platform = 'rss' and raw->>'source' = $1
           order by published_at desc nulls last, external_id"#,
        source
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| EpisodeRow {
            external_id: r.external_id,
            title: r.title,
            duration_s: r.duration_s,
            matched_vimeo_id: r.matched,
        })
        .collect())
}
