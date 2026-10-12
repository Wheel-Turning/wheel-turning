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
use wt_extract::resolve::{self, HandList, WorkNames};
use wt_extract::{DataClass, LlmClient, LlmError};
use wt_index::ChunkSettings;
use wt_index::index::{self, IndexCitation, IndexQuestion, IndexSegment};
use wt_ml::Embedder;
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
    /// The index step refused.
    #[error(transparent)]
    Index(#[from] wt_index::IndexError),
    /// The embedder failed.
    #[error(transparent)]
    Ml(#[from] wt_ml::MlError),
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
    #[serde(default)]
    origin: Option<String>,
}

/// A talk's stored teacher words.
pub(crate) struct Stored {
    /// The words.
    pub words: Vec<Word>,
    /// The current transcript's hash.
    pub hash: String,
    /// Whether a person reviewed them, so the glossary leaves them alone.
    pub reviewed: bool,
}

impl Stored {
    /// The glossary to apply: none to a reviewed transcript.
    pub fn glossary<'g>(&self, glossary: &'g Glossary) -> Option<&'g Glossary> {
        (!self.reviewed).then_some(glossary)
    }
}

/// The talk's stored teacher words, the current transcript's hash, and whether it was reviewed.
pub(crate) async fn stored_words(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    talk: TalkId,
) -> Result<Stored, TextJobError> {
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
    Ok(Stored {
        words,
        hash,
        reviewed: doc.origin.as_deref() == Some("reviewed"),
    })
}

/// The stored words a segment covers: the first and one past the last index.
fn segment_range(words: &[Word], seg: &SegmentRow) -> (usize, usize) {
    let first = words.partition_point(|w| w.start_ms < seg.start_ms);
    let end = words.partition_point(|w| w.start_ms < seg.end_ms);
    (first, end.max(first))
}

/// The corrected words of every segment, in order, each with its source range in the stored
/// words. With no glossary, as for a reviewed transcript, the words are taken as they are.
pub(crate) fn corrected(
    glossary: Option<&Glossary>,
    words: &[Word],
    segments: &[SegmentRow],
) -> Vec<Vec<(Word, (usize, usize))>> {
    segments
        .iter()
        .map(|seg| {
            let (a, b) = segment_range(words, seg);
            match glossary {
                Some(g) => g
                    .apply_mapped(&words[a..b])
                    .into_iter()
                    .map(|(w, (x, y))| (w, (a + x, a + y)))
                    .collect(),
                None => words[a..b]
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(i, w)| (w, (a + i, a + i + 1)))
                    .collect(),
            }
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

/// Moves the talk to `stage` unless it is there or further along already, so a rerun never
/// moves a talk back.
async fn advance_forward(
    conn: &mut sqlx::PgConnection,
    talk: &wt_core::gate::Talk<wt_core::gate::Consented>,
    stage: PipelineState,
) -> Result<(), TextJobError> {
    if !talk.state.is_at_or_past(stage) {
        TalkRepo::advance(conn, talk, stage).await?;
    }
    Ok(())
}

/// Hash of the corrected text of every segment.
fn corrected_hash(fixed: &[Vec<(Word, (usize, usize))>]) -> String {
    let texts: Vec<String> = fixed.iter().map(|ws| join(ws)).collect();
    sha256_hex(texts.join("\u{1e}").as_bytes())
}

/// The `index` job's input hash: the corrected segment text and the staged citations. `correct`
/// and `resolve` both queue `index` with it, so a rerun of either queues nothing new.
fn index_input(texts_hash: &str, staging: &[u8]) -> String {
    sha256_hex(format!("{texts_hash}{}", sha256_hex(staging)).as_bytes())
}

/// The `resolve` job's input hash: the text extracted from and the staged citations as extracted,
/// without the resolutions `resolve` writes into them, so a new extraction of the same text, such
/// as one that was skipped and then ran, is resolved and indexed again, and a rerun is not.
fn resolve_input(texts_hash: &str, staging: &Staging) -> String {
    let mut extracted = staging.clone();
    for c in &mut extracted.citations {
        c.resolution = None;
    }
    let json = serde_json::to_vec(&extracted).unwrap_or_default();
    sha256_hex(format!("{texts_hash}{}", sha256_hex(&json)).as_bytes())
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
    advance_forward(conn, &talk, PipelineState::Correcting).await?;
    let stored = stored_words(conn, objects, talk_id).await?;
    let (words, transcript_hash) = (&stored.words, stored.hash.clone());
    let segments = transcript::segments(conn, talk_id).await?;
    let fixed = corrected(stored.glossary(glossary), words, &segments);

    let mut rows = String::from("talk_id,word_index,heard,canonical,distance,context\n");
    let mut suggested = 0;
    for seg in &segments {
        let (a, b) = segment_range(words, seg);
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
    if suggested > 0 && objects.get(&suggestions_key(talk_id))?.as_deref() != Some(rows.as_bytes())
    {
        objects.put(&suggestions_key(talk_id), rows.as_bytes())?;
    }

    let texts: Vec<String> = fixed.iter().map(|ws| join(ws)).collect();
    let corrected_hash = corrected_hash(&fixed);
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
            job::enqueue(
                &mut tx,
                JobKind::Index,
                Some(talk_id),
                &index_input(&corrected_hash, bytes),
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
    advance_forward(conn, &talk, PipelineState::Extracting).await?;
    let class = data_class(conn, talk_id).await?;
    let stored = stored_words(conn, objects, talk_id).await?;
    let words = &stored.words;
    let segments = transcript::segments(conn, talk_id).await?;
    let fixed = corrected(stored.glossary(glossary), words, &segments);
    let input = corrected_hash(&fixed);
    let staged = objects
        .get(&staging_key(talk_id))?
        .and_then(|b| serde_json::from_slice::<Staging>(&b).ok());
    let staging = match staged {
        // When the same text was extracted before, the staged citations stand and the model is
        // not called again.
        Some(s) if s.input_hash.as_deref() == Some(input.as_str()) => s,
        _ => {
            let all: Vec<(Word, (usize, usize))> = fixed.into_iter().flatten().collect();
            let mut staging =
                match extract::extract(llm, &talk_id.as_uuid().to_string(), &all, class).await {
                    Ok(staging) => staging,
                    // No model may see this talk yet (owner's decision of October 7, 2026): stage
                    // an empty, skipped extraction so the talk is still indexed; no input hash, so
                    // `wt pipeline retry-extract` runs it once a model may.
                    Err(e @ (LlmError::RealDataRefused | LlmError::NoModel(_))) => {
                        tracing::warn!(talk = %talk_id, reason = %e, "extraction skipped");
                        Staging {
                            schema: 1,
                            talk_id: talk_id.as_uuid().to_string(),
                            practice_period: None,
                            citations: Vec::new(),
                            discarded: 0,
                            input_hash: None,
                            skipped: Some(e.to_string()),
                        }
                    }
                    Err(e) => return Err(e.into()),
                };
            if staging.skipped.is_none() {
                staging.input_hash = Some(input.clone());
            }
            let json = serde_json::to_vec(&staging).unwrap_or_default();
            objects.put(&staging_key(talk_id), &json)?;
            staging
        }
    };
    job::enqueue(
        conn,
        JobKind::Resolve,
        Some(talk_id),
        &resolve_input(&input, &staging),
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
    advance_forward(conn, &talk, PipelineState::Resolving).await?;
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
    let stored = stored_words(conn, objects, talk_id).await?;
    let words = &stored.words;
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
    if json != bytes {
        objects.put(&key, &json)?;
    }
    let segments = transcript::segments(conn, talk_id).await?;
    let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
    job::enqueue(
        conn,
        JobKind::Index,
        Some(talk_id),
        &index_input(&sha256_hex(texts.join("\u{1e}").as_bytes()), &json),
        &serde_json::json!({}),
    )
    .await?;
    tracing::info!(talk = %talk_id, ?counts, "citations resolved");
    Ok(TextOutcome::Done(Some(JobKind::Index)))
}

/// `wt pipeline retry-extract`: queues `extract` again for every talk whose staged extraction
/// was skipped because no model could see it, or for `talk` alone. Each job is keyed by the
/// skipped staging object and the extraction model, so asking twice queues nothing new and a
/// talk skipped again for another reason, or with another model, can be retried. The new
/// extraction then flows through `resolve` and `index`, whose keys cover the staged citations.
/// Returns the skipped talks; with `dry_run`, queues nothing.
pub async fn retry_skipped_extracts(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    talk: Option<TalkId>,
    model: &str,
    dry_run: bool,
) -> Result<Vec<TalkId>, TextJobError> {
    let talks = match talk {
        Some(t) => vec![t],
        None => transcript::corrected_talks(conn).await?,
    };
    let mut skipped = Vec::new();
    for talk in talks {
        let Some(bytes) = objects.get(&staging_key(talk))? else {
            continue;
        };
        let is_skipped = serde_json::from_slice::<Staging>(&bytes)
            .is_ok_and(|s| s.skipped.is_some() && s.input_hash.is_none());
        if !is_skipped {
            continue;
        }
        if !dry_run {
            job::enqueue(
                conn,
                JobKind::Extract,
                Some(talk),
                &sha256_hex(format!("retry-extract:{model}:{}", sha256_hex(&bytes)).as_bytes()),
                &serde_json::json!({ "reason": "retry-extract" }),
            )
            .await?;
        }
        skipped.push(talk);
    }
    Ok(skipped)
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
        let stored = stored_words(conn, objects, talk).await?;
        let (words, transcript_hash) = (&stored.words, stored.hash.clone());
        let segments = transcript::segments(conn, talk).await?;
        let changed = corrected(stored.glossary(glossary), words, &segments)
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
    stoplist: Vec<String>,
}

impl PgHandList {
    /// A hand list read through `pool`, with no work-name stoplist.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            stoplist: Vec::new(),
        }
    }

    /// The same, leaving out work names on `[citations] work_alias_stoplist`.
    #[must_use]
    pub fn with_stoplist(mut self, stoplist: Vec<String>) -> Self {
        self.stoplist = stoplist;
        self
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

    async fn works(&self) -> Result<Vec<WorkNames>, LlmError> {
        let mut conn = self.pool.acquire().await.map_err(hand_list_error)?;
        Ok(wt_db::locator::work_names(&mut conn)
            .await
            .map_err(hand_list_error)?
            .into_iter()
            .map(|w| WorkNames {
                id: w.id.to_string(),
                names: w
                    .names
                    .into_iter()
                    .filter(|n| resolve::usable_work_name(n, &self.stoplist))
                    .collect(),
            })
            .collect())
    }
}

/// How an index job went, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexed {
    /// Passages written; none when the talk was already current.
    pub passages: usize,
    /// Citations written.
    pub citations: usize,
    /// Short passages left out.
    pub skipped_short: usize,
    /// Citations in no kept passage.
    pub dropped_citations: usize,
    /// Student questions written.
    pub questions: usize,
    /// Whether anything was written.
    pub written: bool,
}

/// The `index` job: chunks the corrected words of each segment, embeds the passages, maps the
/// staged citations onto them and, in one transaction, replaces the talk's passages and moves it
/// to `ready`. A talk already `ready` with the same content hash is left as it is.
pub async fn run_index(
    conn: &mut sqlx::PgConnection,
    talk_id: TalkId,
    objects: &dyn ObjectStore,
    glossary: &Glossary,
    chunk: &ChunkSettings,
    embedder: &dyn Embedder,
) -> Result<Result<Indexed, GateError>, TextJobError> {
    let talk = match TalkRepo::authorize(conn, talk_id).await? {
        Ok(t) => t,
        Err(gate) => return Ok(Err(gate)),
    };
    let stored = stored_words(conn, objects, talk_id).await?;
    let words = &stored.words;
    let segments = transcript::segments(conn, talk_id).await?;
    let index_segments: Vec<IndexSegment> = corrected(stored.glossary(glossary), words, &segments)
        .into_iter()
        .zip(&segments)
        .map(|(words, seg)| IndexSegment {
            id: seg.id,
            kind: seg.kind.clone(),
            words,
        })
        .collect();
    let staging: Option<Staging> = match objects.get(&staging_key(talk_id))? {
        Some(bytes) => Some(
            serde_json::from_slice(&bytes)
                .map_err(|e| TextJobError::Missing(format!("staging for {talk_id}: {e}")))?,
        ),
        None => None,
    };
    let citations: Vec<IndexCitation> = staging
        .iter()
        .flat_map(|s| &s.citations)
        .map(|c| {
            let r = c.resolution.as_ref();
            let locator = r
                .and_then(|r| r.work_locator_id.as_deref())
                .and_then(|id| id.parse().ok());
            IndexCitation {
                w0: c.w0,
                w1: c.w1,
                confidence: match (r, locator) {
                    (Some(r), Some(_)) => r.confidence.clone(),
                    _ => "unresolved".to_owned(),
                },
                // Found by the model and matched to nothing: the database names the finder.
                method: r
                    .and_then(|r| r.method.clone())
                    .filter(|_| locator.is_some())
                    .unwrap_or_else(|| "llm".to_owned()),
                work_locator_id: locator,
            }
        })
        .collect();
    // The talk's edited student questions, staged by the `audio` job (Spec 04, Student
    // questions), are embedded and stored beside its passages.
    let questions: Vec<IndexQuestion> = match objects.get(&crate::audio::questions_key(talk_id))? {
        Some(bytes) => serde_json::from_slice::<crate::audio::StagedQuestions>(&bytes)
            .map_err(|e| TextJobError::Missing(format!("staged questions for {talk_id}: {e}")))?
            .questions
            .into_iter()
            .map(|q| IndexQuestion {
                start_ms: q.start_ms,
                end_ms: q.end_ms,
                answer_start_ms: q.answer_start_ms,
                text: q.text,
                word_count: q.word_count,
                content_hash: q.content_hash,
                edit_model: q.edit_model,
            })
            .collect(),
        None => Vec::new(),
    };
    let plan = index::plan(&index_segments, &citations, chunk, &embedder.vector_id())?
        .with_questions(questions);
    let mut indexed = Indexed {
        passages: plan.passages.len(),
        citations: plan.citations.len(),
        skipped_short: plan.skipped_short,
        dropped_citations: plan.dropped_citations,
        questions: plan.questions.len(),
        written: false,
    };
    if index::is_current(conn, talk_id.as_uuid(), &plan.content_hash).await? {
        return Ok(Ok(indexed));
    }
    // A ready talk keeps serving its old passages until the new ones commit.
    advance_forward(conn, &talk, PipelineState::Indexing).await?;
    // Unchanged passages keep their ids, and their vectors when current.
    let reuse = index::reuse(
        conn,
        talk_id.as_uuid(),
        &plan,
        embedder.model_id(),
        wt_ml::VECTOR_VERSION,
    )
    .await?;
    let vectors = embedder.embed(&plan.texts(&reuse)).await?;
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    index::write(
        &mut tx,
        talk_id.as_uuid(),
        &plan,
        &reuse,
        &vectors,
        embedder.model_id(),
        wt_ml::VECTOR_VERSION,
        embedder.dim(),
    )
    .await?;
    advance_forward(&mut tx, &talk, PipelineState::Ready).await?;
    tx.commit().await?;
    indexed.written = true;
    tracing::info!(talk = %talk_id, passages = indexed.passages, citations = indexed.citations,
                   model = embedder.model_id(), "talk indexed");
    Ok(Ok(indexed))
}
