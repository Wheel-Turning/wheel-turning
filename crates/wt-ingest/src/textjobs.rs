//! The `correct`, `extract` and `resolve` jobs (Spec 04). They read the stored teacher-only
//! transcript and the segment rows, never audio, and can run on any worker. Each re-authorizes
//! the talk first, so an opt-out or withdrawal stops it.
//!
//! Positions in the stored, uncorrected teacher words are what citations record, so a glossary
//! change re-runs `correct` and then goes straight to `index`, reusing the staged citations.

use std::collections::BTreeMap;

use serde::Deserialize;
use wt_core::gate::{GateError, PipelineState};
use wt_core::ids::TalkId;
use wt_core::objects::ObjectStore;
use wt_core::records::sha256_hex;
use wt_core::transcript::Word;
use wt_db::job::{self, JobKind};
use wt_db::talk::TalkRepo;
use wt_db::transcript::{self, SegmentRow};
use wt_extract::extract::{self, Staging};
use wt_extract::resolve::{self, HandList};
use wt_extract::{DataClass, LlmClient, LlmError};
use wt_registry::glossary::Glossary;

use crate::config::Series;

/// Why a text job failed.
#[derive(Debug, thiserror::Error)]
pub enum TextJobError {
    /// The talk could not be moved.
    #[error(transparent)]
    Talk(#[from] wt_db::talk::TalkError),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] wt_db::DbError),
    /// The queue refused.
    #[error(transparent)]
    Job(#[from] wt_db::job::JobError),
    /// An object could not be read or written.
    #[error(transparent)]
    Objects(#[from] wt_core::objects::ObjectError),
    /// The language model failed or refused.
    #[error(transparent)]
    Llm(#[from] LlmError),
    /// A stored object is missing or unreadable.
    #[error("{0}")]
    Missing(String),
    /// A connection error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// How a text job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextOutcome {
    /// The talk did not pass the gate.
    NotCleared(GateError),
    /// Done; the next job, if one was queued.
    Done(Option<JobKind>),
}

/// The staging object's key.
pub fn staging_key(talk: TalkId) -> String {
    format!("citations/{}.json", talk.as_uuid())
}

/// The near-miss suggestions' key, under the talk's transcript prefix so a takedown removes it.
pub fn suggestions_key(talk: TalkId) -> String {
    format!("transcripts/{}/glossary_suggestions.csv", talk.as_uuid())
}

#[derive(Debug, Deserialize)]
struct StoredWord {
    w: String,
    s: i32,
    e: i32,
    p: f32,
}

#[derive(Debug, Deserialize)]
struct StoredTranscript {
    words: Vec<StoredWord>,
}

/// The talk's stored teacher words and the current transcript's hash.
async fn stored_words(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    talk: TalkId,
) -> Result<(Vec<Word>, String), TextJobError> {
    let (_, key, hash) = transcript::current(conn, talk)
        .await?
        .ok_or_else(|| TextJobError::Missing(format!("talk {talk} has no transcript")))?;
    let bytes = objects
        .get(&key)?
        .ok_or_else(|| TextJobError::Missing(format!("transcript object {key} is missing")))?;
    let doc: StoredTranscript = serde_json::from_slice(&bytes)
        .map_err(|e| TextJobError::Missing(format!("transcript {key}: {e}")))?;
    let words = doc
        .words
        .into_iter()
        .map(|w| Word::new(w.w, w.s, w.e, w.p))
        .collect();
    Ok((words, hash))
}

/// The stored words a segment covers: the first and one past the last index.
fn segment_range(words: &[Word], seg: &SegmentRow) -> (usize, usize) {
    let first = words.partition_point(|w| w.start_ms < seg.start_ms);
    let end = words.partition_point(|w| w.start_ms < seg.end_ms);
    (first, end.max(first))
}

/// The corrected words of every segment, in order, each with its source range in the stored
/// words.
fn corrected(
    glossary: &Glossary,
    words: &[Word],
    segments: &[SegmentRow],
) -> Vec<Vec<(Word, (usize, usize))>> {
    segments
        .iter()
        .map(|seg| {
            let (a, b) = segment_range(words, seg);
            glossary
                .apply_mapped(&words[a..b])
                .into_iter()
                .map(|(w, (x, y))| (w, (a + x, a + y)))
                .collect()
        })
        .collect()
}

fn join(words: &[(Word, (usize, usize))]) -> String {
    words
        .iter()
        .map(|(w, _)| w.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The data class of a talk: `Synthetic` for fixtures, `Real` otherwise.
pub async fn data_class(
    conn: &mut sqlx::PgConnection,
    talk: TalkId,
) -> Result<DataClass, TextJobError> {
    Ok(if transcript::talk_facts(conn, talk).await?.synthetic {
        DataClass::Synthetic
    } else {
        DataClass::Real
    })
}

/// The `correct` job: writes corrected text into the segments, the near-miss suggestions into
/// the talk's suggestions file, and queues `extract`, or `index` when citations are already
/// staged.
pub async fn run_correct(
    conn: &mut sqlx::PgConnection,
    talk_id: TalkId,
    objects: &dyn ObjectStore,
    glossary: &Glossary,
    is_english: &(dyn Fn(&str) -> bool + Sync),
) -> Result<TextOutcome, TextJobError> {
    let talk = match TalkRepo::authorize(conn, talk_id).await? {
        Ok(t) => t,
        Err(gate) => return Ok(TextOutcome::NotCleared(gate)),
    };
    TalkRepo::advance(conn, &talk, PipelineState::Correcting).await?;
    let (words, transcript_hash) = stored_words(conn, objects, talk_id).await?;
    let segments = transcript::segments(conn, talk_id).await?;
    let fixed = corrected(glossary, &words, &segments);

    let mut rows = String::from("talk_id,word_index,heard,canonical,distance,context\n");
    let mut suggested = 0;
    for seg in &segments {
        let (a, b) = segment_range(&words, seg);
        for s in glossary.near_misses(&words[a..b], is_english) {
            rows.push_str(&format!(
                "{},{},{},{},{},\"{}\"\n",
                talk_id.as_uuid(),
                a + s.word_index,
                s.heard.replace(',', " "),
                s.canonical,
                s.distance,
                s.context.replace('"', "'")
            ));
            suggested += 1;
        }
    }
    if suggested > 0 {
        objects.put(&suggestions_key(talk_id), rows.as_bytes())?;
    }

    let texts: Vec<String> = fixed.iter().map(|ws| join(ws)).collect();
    let corrected_hash = sha256_hex(texts.join("\u{1e}").as_bytes());
    let staged = objects.get(&staging_key(talk_id))?;
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    for (seg, text) in segments.iter().zip(&texts) {
        if *text != seg.text {
            transcript::set_segment_text(&mut tx, seg.id, text).await?;
        }
    }
    let next = match &staged {
        // Citations record stored-word positions, so a re-correction needs only a new index.
        Some(bytes) => {
            let key = sha256_hex(format!("{corrected_hash}{}", sha256_hex(bytes)).as_bytes());
            job::enqueue(
                &mut tx,
                JobKind::Index,
                Some(talk_id),
                &key,
                &serde_json::json!({}),
            )
            .await?;
            JobKind::Index
        }
        None => {
            job::enqueue(
                &mut tx,
                JobKind::Extract,
                Some(talk_id),
                &corrected_hash,
                &serde_json::json!({ "transcript": transcript_hash }),
            )
            .await?;
            JobKind::Extract
        }
    };
    tx.commit().await?;
    tracing::info!(talk = %talk_id, glossary = glossary.version(), suggestions = suggested,
                   "segments corrected");
    Ok(TextOutcome::Done(Some(next)))
}

/// The `extract` job: finds verified citations in the corrected words, stages them, and queues
/// `resolve`.
pub async fn run_extract(
    conn: &mut sqlx::PgConnection,
    talk_id: TalkId,
    objects: &dyn ObjectStore,
    glossary: &Glossary,
    llm: &dyn LlmClient,
) -> Result<TextOutcome, TextJobError> {
    let talk = match TalkRepo::authorize(conn, talk_id).await? {
        Ok(t) => t,
        Err(gate) => return Ok(TextOutcome::NotCleared(gate)),
    };
    TalkRepo::advance(conn, &talk, PipelineState::Extracting).await?;
    let class = data_class(conn, talk_id).await?;
    let (words, _) = stored_words(conn, objects, talk_id).await?;
    let segments = transcript::segments(conn, talk_id).await?;
    let all: Vec<(Word, (usize, usize))> = corrected(glossary, &words, &segments)
        .into_iter()
        .flatten()
        .collect();
    let staging = extract::extract(llm, &talk_id.as_uuid().to_string(), &all, class).await?;
    let json = serde_json::to_vec(&staging).unwrap_or_default();
    objects.put(&staging_key(talk_id), &json)?;
    job::enqueue(
        conn,
        JobKind::Resolve,
        Some(talk_id),
        &sha256_hex(&json),
        &serde_json::json!({}),
    )
    .await?;
    Ok(TextOutcome::Done(Some(JobKind::Resolve)))
}

/// The `resolve` job: resolves each staged citation against the hand list and the series map,
/// stores the result in the staging object, and queues `index`.
pub async fn run_resolve(
    conn: &mut sqlx::PgConnection,
    talk_id: TalkId,
    objects: &dyn ObjectStore,
    series: &[Series],
    hand_list: &dyn HandList,
) -> Result<TextOutcome, TextJobError> {
    let talk = match TalkRepo::authorize(conn, talk_id).await? {
        Ok(t) => t,
        Err(gate) => return Ok(TextOutcome::NotCleared(gate)),
    };
    TalkRepo::advance(conn, &talk, PipelineState::Resolving).await?;
    let key = staging_key(talk_id);
    let bytes = objects
        .get(&key)?
        .ok_or_else(|| TextJobError::Missing(format!("staging object {key} is missing")))?;
    let mut staging: Staging = serde_json::from_slice(&bytes)
        .map_err(|e| TextJobError::Missing(format!("staging {key}: {e}")))?;
    let facts = transcript::talk_facts(conn, talk_id).await?;
    let series_locator = match (&facts.series, facts.part) {
        (Some(name), Some(part)) => series
            .iter()
            .find(|s| &s.name == name)
            .and_then(|s| s.locator_for(part)),
        _ => None,
    };
    let (words, _) = stored_words(conn, objects, talk_id).await?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for c in &mut staging.citations {
        let from = c.w0.saturating_sub(20);
        let to = (c.w1 + 20).min(words.len());
        let context = words
            .get(from..to)
            .unwrap_or_default()
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let r = resolve::resolve(c, &context, series_locator.as_deref(), hand_list).await?;
        *counts.entry(r.confidence.clone()).or_default() += 1;
        c.resolution = Some(r);
    }
    let json = serde_json::to_vec(&staging).unwrap_or_default();
    objects.put(&key, &json)?;
    job::enqueue(
        conn,
        JobKind::Index,
        Some(talk_id),
        &sha256_hex(&json),
        &serde_json::json!({}),
    )
    .await?;
    tracing::info!(talk = %talk_id, ?counts, "citations resolved");
    Ok(TextOutcome::Done(Some(JobKind::Index)))
}

/// After a glossary change: queues `correct` for exactly the talks whose corrected text it
/// changes, keyed by the transcript and the new glossary version. Returns those talks.
pub async fn requeue_after_glossary_change(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    glossary: &Glossary,
) -> Result<Vec<TalkId>, TextJobError> {
    let mut affected = Vec::new();
    for talk in transcript::corrected_talks(conn).await? {
        let (words, transcript_hash) = stored_words(conn, objects, talk).await?;
        let segments = transcript::segments(conn, talk).await?;
        let changed = corrected(glossary, &words, &segments)
            .iter()
            .zip(&segments)
            .any(|(ws, seg)| join(ws) != seg.text);
        if changed {
            job::enqueue(
                conn,
                JobKind::Correct,
                Some(talk),
                &sha256_hex(format!("{transcript_hash}{}", glossary.version()).as_bytes()),
                &serde_json::json!({ "glossary": glossary.version() }),
            )
            .await?;
            affected.push(talk);
        }
    }
    Ok(affected)
}

/// The hand list in Postgres.
#[derive(Debug, Clone)]
pub struct PgHandList {
    pool: sqlx::PgPool,
}

impl PgHandList {
    /// A hand list read through `pool`.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

fn hand_list_error(e: impl std::fmt::Display) -> LlmError {
    LlmError::Ledger(format!("reading the hand list: {e}"))
}

#[async_trait::async_trait]
impl HandList for PgHandList {
    async fn exact(&self, text: &str) -> Result<Option<String>, LlmError> {
        let mut conn = self.pool.acquire().await.map_err(hand_list_error)?;
        Ok(wt_db::locator::exact(&mut conn, text)
            .await
            .map_err(hand_list_error)?
            .map(|id| id.to_string()))
    }

    async fn most_similar(&self, text: &str) -> Result<Option<(String, f32)>, LlmError> {
        let mut conn = self.pool.acquire().await.map_err(hand_list_error)?;
        Ok(wt_db::locator::most_similar(&mut conn, text)
            .await
            .map_err(hand_list_error)?
            .map(|(id, sim)| (id.to_string(), sim)))
    }

    async fn by_key(&self, key: &str) -> Result<Option<String>, LlmError> {
        let mut conn = self.pool.acquire().await.map_err(hand_list_error)?;
        Ok(wt_db::locator::by_key(&mut conn, key)
            .await
            .map_err(hand_list_error)?
            .map(|id| id.to_string()))
    }
}
