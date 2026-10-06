//! The `audio` job (Spec 04): fetch, transcribe, diarize and segment one cleared talk in one
//! process, so audio never leaves the machine. One `ScratchDir` covers all four steps and is
//! deleted on every outcome. Only the teacher's words are persisted: the transcript JSON and
//! the `segment` rows are written from the segmenter's output, never from the raw recognition.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use wt_asr::{AsrError, AsrHints, Transcriber};
use wt_core::gate::{GateError, PipelineState};
use wt_core::ids::TalkId;
use wt_core::objects::{ObjectError, ObjectStore};
use wt_core::records::sha256_hex;
use wt_core::scratch::{Converter, ScratchDir, ScratchError};
use wt_db::feed;
use wt_db::ingest::IngestDbError;
use wt_db::job::{self, JobError, JobKind};
use wt_db::talk::{TalkError, TalkRepo};
use wt_db::transcript::{self, NewSegment, NewTranscript};
use wt_segment::{
    DiarizeError, Diarizer, LabelError, SegmentSettings, Segmentation, SpanKind, WindowLabeler,
};

use crate::media::{MediaFetcher, MediaSource};
use crate::source::IngestError;

/// Why an `audio` job failed. The job is retried; scratch is already gone.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// The talk could not be moved.
    #[error(transparent)]
    Talk(#[from] TalkError),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] wt_db::DbError),
    /// Reading the media route failed.
    #[error(transparent)]
    Route(#[from] IngestDbError),
    /// The queue refused.
    #[error(transparent)]
    Job(#[from] JobError),
    /// Fetching failed.
    #[error(transparent)]
    Fetch(#[from] IngestError),
    /// Scratch space failed.
    #[error(transparent)]
    Scratch(#[from] ScratchError),
    /// Recognition failed.
    #[error(transparent)]
    Asr(#[from] AsrError),
    /// Diarization failed.
    #[error(transparent)]
    Diarize(#[from] DiarizeError),
    /// Labeling ambiguous windows failed.
    #[error(transparent)]
    Label(#[from] LabelError),
    /// The labeler left a window unlabeled.
    #[error(transparent)]
    Missing(#[from] wt_segment::MissingLabel),
    /// The transcript could not be stored.
    #[error(transparent)]
    Objects(#[from] ObjectError),
    /// A database connection error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// Everything the job uses, so tests can swap in fakes.
pub struct AudioDeps<'a> {
    /// Speech recognition.
    pub transcriber: &'a dyn Transcriber,
    /// Speaker turns.
    pub diarizer: &'a dyn Diarizer,
    /// Labels for ambiguous windows.
    pub labeler: &'a dyn WindowLabeler,
    /// Media to WAV.
    pub converter: &'a dyn Converter,
    /// Media download.
    pub fetcher: &'a MediaFetcher,
    /// Where the transcript JSON goes.
    pub objects: &'a dyn ObjectStore,
    /// `WT_SCRATCH_DIR`.
    pub scratch_root: &'a Path,
    /// `[segment]`.
    pub segment: &'a SegmentSettings,
    /// Recognition hints, the glossary prompt among them.
    pub hints: &'a AsrHints,
    /// Who the correction log names.
    pub actor: &'a str,
}

/// How the job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioOutcome {
    /// The talk did not pass the gate, so nothing was fetched.
    NotCleared(GateError),
    /// No media route: the talk waits in `needs_review` with reason `no_media`, and nothing was
    /// fetched.
    NoMedia,
    /// The segmenter needs a person: the talk waits in `needs_review` with this reason.
    Held(String),
    /// Segmented and stored, with a `correct` job queued.
    Segmented {
        /// The transcript version written.
        version: i32,
        /// Segments stored.
        segments: usize,
        /// Teacher words kept.
        words: usize,
    },
}

/// The persisted transcript, Spec 04's schema 1.
#[derive(Debug, Serialize)]
struct TranscriptJson<'a> {
    schema: u32,
    talk_id: String,
    version: i32,
    origin: &'a str,
    model: &'a str,
    language: &'a str,
    audio_ms: i64,
    glossary_version: Option<&'a str>,
    words: Vec<JsonWord<'a>>,
    spans: Vec<JsonSpan>,
}

#[derive(Debug, Serialize)]
struct JsonWord<'a> {
    i: usize,
    w: &'a str,
    s: i32,
    e: i32,
    p: f32,
}

#[derive(Debug, Serialize)]
struct JsonSpan {
    kind: &'static str,
    s: i32,
    e: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    w0: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    w1: Option<usize>,
}

/// The transcript's object key.
pub fn transcript_key(talk: TalkId, version: i32) -> String {
    format!("transcripts/{}/v{version}.json", talk.as_uuid())
}

/// Runs the `audio` job for one talk.
pub async fn run(
    conn: &mut sqlx::PgConnection,
    talk_id: TalkId,
    deps: &AudioDeps<'_>,
) -> Result<AudioOutcome, AudioError> {
    // Every job re-authorizes, so an opt-out or withdrawal stops it here.
    let talk = match TalkRepo::authorize(conn, talk_id).await? {
        Ok(talk) => talk,
        Err(gate) => return Ok(AudioOutcome::NotCleared(gate)),
    };
    let route = feed::media_route(conn, talk_id).await?;
    let Some(source) = MediaSource::from_route(&route) else {
        transcript::hold_for_review(conn, talk_id, "no_media", deps.actor).await?;
        return Ok(AudioOutcome::NoMedia);
    };

    TalkRepo::advance(conn, &talk, PipelineState::Fetching).await?;
    let scratch = ScratchDir::create(deps.scratch_root, talk_id)?;
    let fetched = deps.fetcher.fetch(&talk, &source, &scratch).await?;
    let wav = deps.converter.to_wav(&scratch, &fetched)?;

    TalkRepo::advance(conn, &talk, PipelineState::Transcribing).await?;
    let raw = deps
        .transcriber
        .transcribe(&scratch, &wav, deps.hints)
        .await?;

    TalkRepo::advance(conn, &talk, PipelineState::Diarizing).await?;
    let turns = deps.diarizer.diarize(&wav).await?;

    TalkRepo::advance(conn, &talk, PipelineState::Segmenting).await?;
    let audio_ms = i32::try_from(raw.audio_ms).unwrap_or(i32::MAX);
    let plan = match wt_segment::plan(
        &raw.words,
        &turns,
        audio_ms,
        &deps.segment.chant_phrases,
        deps.segment,
    ) {
        Ok(plan) => plan,
        Err(review) => {
            transcript::hold_for_review(conn, talk_id, review.reason(), deps.actor).await?;
            scratch.finish()?;
            return Ok(AudioOutcome::Held(review.reason().to_owned()));
        }
    };
    let labels: BTreeMap<_, _> = if plan.ambiguous().is_empty() {
        BTreeMap::new()
    } else {
        deps.labeler.label(plan.ambiguous()).await?
    };
    let seg = plan.finish(&labels)?;
    // The raw recognition and the audio are no longer needed; delete them before storing.
    scratch.finish()?;

    let version = transcript::next_version(conn, talk_id).await?;
    let key = transcript_key(talk_id, version);
    let json = transcript_json(
        talk_id,
        version,
        &raw.model,
        &deps.hints.language,
        raw.audio_ms,
        &seg,
    );
    let hash = sha256_hex(json.as_bytes());
    deps.objects.put(&key, json.as_bytes())?;

    let segments: Vec<NewSegment<'_>> = seg
        .segments
        .iter()
        .map(|s| NewSegment {
            kind: s.kind.as_str(),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            speaker_label: &seg.teacher,
            text: &s.text,
        })
        .collect();
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    transcript::store(
        &mut tx,
        talk_id,
        &NewTranscript {
            version,
            origin: "asr",
            key: &key,
            content_hash: &hash,
            word_count: i32::try_from(seg.words.len()).unwrap_or(i32::MAX),
        },
        &segments,
    )
    .await?;
    job::enqueue(
        &mut tx,
        JobKind::Correct,
        Some(talk_id),
        &hash,
        &serde_json::json!({ "transcript_version": version }),
    )
    .await?;
    tx.commit().await?;
    Ok(AudioOutcome::Segmented {
        version,
        segments: segments.len(),
        words: seg.words.len(),
    })
}

fn transcript_json(
    talk: TalkId,
    version: i32,
    model: &str,
    language: &str,
    audio_ms: i64,
    seg: &Segmentation,
) -> String {
    let doc = TranscriptJson {
        schema: 1,
        talk_id: talk.as_uuid().to_string(),
        version,
        origin: "asr",
        model,
        language,
        audio_ms,
        glossary_version: None,
        words: seg
            .words
            .iter()
            .enumerate()
            .map(|(i, w)| JsonWord {
                i,
                w: &w.text,
                s: w.start_ms,
                e: w.end_ms,
                p: w.prob,
            })
            .collect(),
        spans: seg
            .spans
            .iter()
            .map(|s| JsonSpan {
                kind: span_kind(s.kind),
                s: s.start_ms,
                e: s.end_ms,
                w0: s.words.map(|w| w.0),
                w1: s.words.map(|w| w.1),
            })
            .collect(),
    };
    serde_json::to_string(&doc).unwrap_or_default()
}

fn span_kind(kind: SpanKind) -> &'static str {
    kind.as_str()
}
