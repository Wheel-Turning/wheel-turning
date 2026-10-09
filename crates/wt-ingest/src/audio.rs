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
    /// The talk already has a current transcript and the job did not ask to retranscribe, so
    /// nothing was fetched or changed.
    Current,
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
        /// Ambiguous windows labeled by the rule fallback instead of the model.
        fallback_windows: usize,
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
    /// Ambiguous windows labeled by the rule fallback instead of the model.
    #[serde(skip_serializing_if = "is_zero")]
    label_fallback_windows: usize,
    /// For an aligned source transcript, the share of its words matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    alignment_share: Option<f32>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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

/// The audio steps' result, before anything is stored.
#[derive(Debug)]
pub enum Steps {
    /// The segmenter needs a person, for this reason.
    Held(String),
    /// Segmented; scratch is already deleted.
    Segmented {
        /// The segmenter's output.
        seg: Segmentation,
        /// The recognition model's name.
        model: String,
        /// Audio length.
        audio_ms: i64,
        /// Ambiguous windows labeled by the rule fallback instead of the model.
        fallback_windows: usize,
        /// For the source's own text, aligned: the share of its words matched (Spec 04,
        /// Aligned source transcripts).
        aligned: Option<f32>,
        /// `asr`, `aligned`, or `text` for a transcript built from the source's text alone.
        origin: &'static str,
    },
}

/// Fetch, convert, transcribe, diarize and segment, in one scratch directory deleted on every
/// outcome. With a connection, the talk moves through the pipeline states as each step starts;
/// without one, nothing in the database changes.
async fn steps(
    mut conn: Option<&mut sqlx::PgConnection>,
    talk: &wt_core::gate::Talk<wt_core::gate::Consented>,
    source: &MediaSource,
    source_text: Option<&str>,
    deps: &AudioDeps<'_>,
) -> Result<Steps, AudioError> {
    if let Some(c) = conn.as_deref_mut() {
        TalkRepo::advance(c, talk, PipelineState::Fetching).await?;
    }
    let scratch = ScratchDir::create(deps.scratch_root, talk.id)?;
    let fetched = deps.fetcher.fetch(talk, source, &scratch).await?;
    let wav = deps.converter.to_wav(&scratch, &fetched)?;

    if let Some(c) = conn.as_deref_mut() {
        TalkRepo::advance(c, talk, PipelineState::Transcribing).await?;
    }
    let raw = deps
        .transcriber
        .transcribe(&scratch, &wav, deps.hints)
        .await?;
    // A source's own transcript keeps its wording: the recognized words only lend it times, and
    // they never leave scratch (Spec 04, Aligned source transcripts).
    let (raw, aligned) = match source_text {
        Some(text) => {
            let a = wt_asr::align::align(text, &raw.words);
            tracing::info!(talk = %talk.id, share = a.share, "source transcript aligned");
            if a.share < wt_asr::align::MIN_ALIGNMENT_SHARE {
                scratch.finish()?;
                return Ok(Steps::Held("alignment_failed".to_owned()));
            }
            (
                wt_asr::RawTranscript {
                    model: format!("source-aligned/{}", raw.model),
                    audio_ms: raw.audio_ms,
                    words: a.words,
                },
                Some(a.share),
            )
        }
        None => (raw, None),
    };

    if let Some(c) = conn.as_deref_mut() {
        TalkRepo::advance(c, talk, PipelineState::Diarizing).await?;
    }
    // Diarization is the slow step, so only the questions are diarized (Spec 04, Diarization
    // and segmentation; owner's decision of October 8, 2026).
    let audio_len = i32::try_from(raw.audio_ms).unwrap_or(i32::MAX);
    let scope = if deps.segment.diarize_scope == "full" {
        wt_segment::scope::Scope::Full
    } else {
        wt_segment::scope::scope(
            &raw.words,
            audio_len,
            deps.segment.qa_preroll_s.saturating_mul(1000),
            deps.segment.monologue_max_replies_per_hour,
        )
    };
    tracing::info!(talk = %talk.id, ?scope, "diarization scope");
    let turns = match scope {
        wt_segment::scope::Scope::Monologue => wt_segment::scope::assemble(scope, audio_len, &[]),
        wt_segment::scope::Scope::From { start_ms, .. } => {
            match wt_core::scratch::wav_tail(&scratch, &wav, start_ms)? {
                Some(tail) => {
                    let turns = deps.diarizer.diarize(&tail).await;
                    let _ = std::fs::remove_file(&tail);
                    wt_segment::scope::assemble(scope, audio_len, &turns?)
                }
                None => deps.diarizer.diarize(&wav).await?,
            }
        }
        wt_segment::scope::Scope::Full => deps.diarizer.diarize(&wav).await?,
    };

    if let Some(c) = conn {
        TalkRepo::advance(c, talk, PipelineState::Segmenting).await?;
    }
    let audio_ms = i32::try_from(raw.audio_ms).unwrap_or(i32::MAX);
    let segmented = segment_words(&raw.words, &turns, audio_ms, deps).await;
    // The raw recognition and the audio are no longer needed; delete them before storing.
    scratch.finish()?;
    Ok(match segmented? {
        Err(reason) => Steps::Held(reason),
        Ok((seg, fallback_windows)) => Steps::Segmented {
            seg,
            model: raw.model,
            audio_ms: raw.audio_ms,
            fallback_windows,
            origin: if aligned.is_some() { "aligned" } else { "asr" },
            aligned,
        },
    })
}

/// Segmentation of timed, attributed words: the plan, labels for its ambiguous windows and the
/// segments, or the reason the talk needs a person.
async fn segment_words(
    words: &[wt_core::transcript::Word],
    turns: &[wt_segment::Turn],
    audio_ms: i32,
    deps: &AudioDeps<'_>,
) -> Result<Result<(Segmentation, usize), String>, AudioError> {
    let plan = match wt_segment::plan(
        words,
        turns,
        audio_ms,
        &deps.segment.chant_phrases,
        deps.segment,
    ) {
        Ok(plan) => plan,
        Err(review) => return Ok(Err(review.reason().to_owned())),
    };
    let labels: BTreeMap<_, _> = if plan.ambiguous().is_empty() {
        BTreeMap::new()
    } else {
        deps.labeler.label(plan.ambiguous()).await?
    };
    let fallback_windows = if deps.labeler.is_fallback() {
        labels.len()
    } else {
        0
    };
    Ok(Ok((plan.finish(&labels)?, fallback_windows)))
}

/// The text-only path (Spec 04, Text-only source transcripts): the source's text, its speakers
/// from its labels and its times from its markers, segmented as recognized words are. Nothing
/// is fetched and no scratch is used. With `labeled_only`, after a recording failed to align,
/// an unlabeled text keeps the talk held, since only the audio could tell its speakers apart.
async fn text_steps(
    conn: &mut sqlx::PgConnection,
    talk: &wt_core::gate::Talk<wt_core::gate::Consented>,
    text: &str,
    labeled_only: bool,
    deps: &AudioDeps<'_>,
) -> Result<Steps, AudioError> {
    use crate::text_transcript::{TextPlan, plan, teacher_labels};
    let names = transcript::teacher_names(conn, talk.id).await?;
    let (words, turns, audio_ms) = match plan(
        text,
        &teacher_labels(&names),
        deps.segment.monologue_max_replies_per_hour,
    ) {
        TextPlan::Held(reason) => return Ok(Steps::Held(reason.into())),
        TextPlan::Ready { labeled: false, .. } if labeled_only => {
            return Ok(Steps::Held("alignment_failed".into()));
        }
        TextPlan::Ready {
            words,
            turns,
            audio_ms,
            ..
        } => (words, turns, audio_ms),
    };
    tracing::info!(talk = %talk.id, words = words.len(), turns = turns.len(), "text-only transcript");
    TalkRepo::advance(conn, talk, PipelineState::Segmenting).await?;
    Ok(match segment_words(&words, &turns, audio_ms, deps).await? {
        Err(reason) => Steps::Held(reason),
        Ok((seg, fallback_windows)) => Steps::Segmented {
            seg,
            model: "source-text".to_owned(),
            audio_ms: i64::from(audio_ms),
            fallback_windows,
            aligned: None,
            origin: "text",
        },
    })
}

/// How a segmentation sample went for one talk.
#[derive(Debug)]
pub enum SampleOutcome {
    /// The talk did not pass the gate.
    NotCleared(GateError),
    /// No media route.
    NoMedia,
    /// The steps ran.
    Ran(Steps),
}

/// Runs the audio steps for `wt segment sample` without persisting anything: no state change,
/// no transcript, no segments. Scratch is deleted as in the job.
pub async fn sample(
    conn: &mut sqlx::PgConnection,
    talk_id: TalkId,
    deps: &AudioDeps<'_>,
) -> Result<SampleOutcome, AudioError> {
    let talk = match TalkRepo::authorize(conn, talk_id).await? {
        Ok(talk) => talk,
        Err(gate) => return Ok(SampleOutcome::NotCleared(gate)),
    };
    let route = feed::media_route(conn, talk_id).await?;
    let Some(source) = MediaSource::from_route(&route) else {
        return Ok(SampleOutcome::NoMedia);
    };
    let text = feed::source_transcript(conn, talk_id).await?;
    Ok(SampleOutcome::Ran(
        steps(None, &talk, &source, text.as_deref(), deps).await?,
    ))
}

/// Runs the `audio` job for one talk.
pub async fn run(
    conn: &mut sqlx::PgConnection,
    talk_id: TalkId,
    deps: &AudioDeps<'_>,
    retranscribe: bool,
) -> Result<AudioOutcome, AudioError> {
    // Every job re-authorizes, so an opt-out or withdrawal stops it here.
    let talk = match TalkRepo::authorize(conn, talk_id).await? {
        Ok(talk) => talk,
        Err(gate) => return Ok(AudioOutcome::NotCleared(gate)),
    };
    // Rerunning on unchanged input is a no-op; a new model asks for `retranscribe`. A reviewed
    // transcript is never replaced by recognition, even when asked.
    match transcript::current_origin(conn, talk_id).await?.as_deref() {
        Some("reviewed") => return Ok(AudioOutcome::Current),
        Some(_) if !retranscribe => return Ok(AudioOutcome::Current),
        _ => {}
    }
    let route = feed::media_route(conn, talk_id).await?;
    let text = feed::source_transcript(conn, talk_id).await?;
    // Without audio, the source's own text is the transcript, and a labeled text whose
    // recording does not align is used alone too (Spec 04, Text-only source transcripts).
    let done = match (MediaSource::from_route(&route), text.as_deref()) {
        (None, None) => {
            transcript::hold_for_review(conn, talk_id, "no_media", deps.actor).await?;
            return Ok(AudioOutcome::NoMedia);
        }
        (None, Some(text)) => text_steps(conn, &talk, text, false, deps).await?,
        (Some(source), text) => match steps(Some(&mut *conn), &talk, &source, text, deps).await? {
            Steps::Held(reason) if reason == "alignment_failed" => match text {
                Some(text) => text_steps(conn, &talk, text, true, deps).await?,
                None => Steps::Held(reason),
            },
            other => other,
        },
    };
    let (seg, model, audio_ms, fallback_windows, aligned, origin) = match done {
        Steps::Held(reason) => {
            transcript::hold_for_review(conn, talk_id, &reason, deps.actor).await?;
            return Ok(AudioOutcome::Held(reason));
        }
        Steps::Segmented {
            seg,
            model,
            audio_ms,
            fallback_windows,
            aligned,
            origin,
        } => (seg, model, audio_ms, fallback_windows, aligned, origin),
    };

    let version = transcript::next_version(conn, talk_id).await?;
    let key = transcript_key(talk_id, version);
    let json = transcript_json(
        talk_id,
        version,
        &model,
        &deps.hints.language,
        audio_ms,
        &seg,
        fallback_windows,
        aligned,
        origin,
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
            origin,
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
        fallback_windows,
    })
}

#[allow(clippy::too_many_arguments)]
fn transcript_json(
    talk: TalkId,
    version: i32,
    model: &str,
    language: &str,
    audio_ms: i64,
    seg: &Segmentation,
    fallback_windows: usize,
    aligned: Option<f32>,
    origin: &str,
) -> String {
    let doc = TranscriptJson {
        schema: 1,
        talk_id: talk.as_uuid().to_string(),
        version,
        origin,
        alignment_share: aligned,
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
        label_fallback_windows: fallback_windows,
    };
    serde_json::to_string(&doc).unwrap_or_default()
}

fn span_kind(kind: SpanKind) -> &'static str {
    kind.as_str()
}

/// `wt pipeline advance`: queues an `audio` job for every talk at `cleared`, keyed by its media
/// route, so running it twice queues nothing new and a talk whose route changes gets a new job.
/// Returns the talks queued.
pub async fn advance_cleared(conn: &mut sqlx::PgConnection) -> Result<Vec<TalkId>, AudioError> {
    let mut queued = Vec::new();
    for talk in job::cleared_talks(conn).await? {
        let route = match feed::media_route(conn, talk).await? {
            feed::MediaRoute::FeedEnclosure { url, .. } => format!("enclosure:{url}"),
            // The source's text is input too: a changed transcript queues the talk again.
            feed::MediaRoute::Direct { url } => format!(
                "direct:{url}:{}",
                sha256_hex(
                    feed::source_transcript(conn, talk)
                        .await?
                        .unwrap_or_default()
                        .as_bytes()
                )
            ),
            feed::MediaRoute::None => "none".to_owned(),
        };
        let key = sha256_hex(route.as_bytes());
        if job::enqueue(
            conn,
            JobKind::Audio,
            Some(talk),
            &key,
            &serde_json::json!({}),
        )
        .await?
        .is_some()
        {
            queued.push(talk);
        }
    }
    Ok(queued)
}

/// `wt pipeline retry-held`: talks the `audio` job held for one of `reasons`, such as
/// `no_media` or `alignment_failed` once the text-only path can take them (Spec 04, Text-only
/// source transcripts), go back to `cleared` with an `audio` job each, keyed by the hold so
/// asking twice queues nothing new. Returns those talks.
pub async fn retry_held(
    conn: &mut sqlx::PgConnection,
    reasons: &[String],
    actor: &str,
) -> Result<Vec<TalkId>, AudioError> {
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    let released = transcript::release_held(&mut tx, reasons, actor).await?;
    let mut queued = Vec::with_capacity(released.len());
    for (talk, hold) in released {
        job::enqueue(
            &mut tx,
            JobKind::Audio,
            Some(talk),
            &sha256_hex(format!("retry-held:{hold}").as_bytes()),
            &serde_json::json!({}),
        )
        .await?;
        queued.push(talk);
    }
    tx.commit().await?;
    Ok(queued)
}

/// `wt asr requeue`: queues `audio` with `retranscribe` for every talk whose current transcript
/// records a model other than `model`, keyed by the model so asking twice queues nothing new.
/// Returns those talks; with `dry_run`, queues nothing.
pub async fn requeue_for_model(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    model: &str,
    dry_run: bool,
) -> Result<Vec<TalkId>, AudioError> {
    #[derive(serde::Deserialize)]
    struct Recorded {
        model: String,
    }
    let mut affected = Vec::new();
    for (talk, key, origin) in transcript::current_keys(conn).await? {
        // A reviewed transcript is a person's work: no automated pass replaces it.
        if origin == "reviewed" {
            continue;
        }
        let Some(bytes) = objects.get(&key)? else {
            tracing::warn!(%talk, %key, "transcript object missing; not requeued");
            continue;
        };
        let recorded = serde_json::from_slice::<Recorded>(&bytes)
            .map(|r| r.model)
            .unwrap_or_default();
        if recorded == model {
            continue;
        }
        if !dry_run {
            job::enqueue(
                conn,
                JobKind::Audio,
                Some(talk),
                &sha256_hex(format!("retranscribe:{model}").as_bytes()),
                &serde_json::json!({ "retranscribe": true, "model": model, "was": recorded }),
            )
            .await?;
        }
        affected.push(talk);
    }
    Ok(affected)
}

/// Fetches a cleared talk's media into `scratch` and converts it to WAV, for the benchmark's
/// talk mode. Nothing in the database changes. `None` when the talk has no media route.
pub async fn fetch_wav(
    conn: &mut sqlx::PgConnection,
    talk_id: TalkId,
    fetcher: &MediaFetcher,
    converter: &dyn Converter,
    scratch: &ScratchDir,
) -> Result<Result<Option<std::path::PathBuf>, GateError>, AudioError> {
    let talk = match TalkRepo::authorize(conn, talk_id).await? {
        Ok(talk) => talk,
        Err(gate) => return Ok(Err(gate)),
    };
    let route = feed::media_route(conn, talk_id).await?;
    let Some(source) = MediaSource::from_route(&route) else {
        return Ok(Ok(None));
    };
    let fetched = fetcher.fetch(&talk, &source, scratch).await?;
    Ok(Ok(Some(converter.to_wav(scratch, &fetched)?)))
}

/// Spec 07, takedown step 3: after the database commit, deletes each talk's stored objects,
/// `transcripts/<talk>/` (every version and the suggestions file) and `citations/<talk>.json`,
/// then checks nothing is left. Returns how many objects were deleted.
pub fn delete_talk_objects(
    store: &dyn ObjectStore,
    talks: &[TalkId],
) -> Result<usize, ObjectError> {
    let mut deleted = 0;
    for talk in talks {
        let id = talk.as_uuid();
        for prefix in [format!("transcripts/{id}/"), format!("citations/{id}.json")] {
            deleted += store.delete_prefix(&prefix)?;
            if store.delete_prefix(&prefix)? != 0 {
                return Err(ObjectError::Remains(prefix));
            }
        }
    }
    Ok(deleted)
}
