//! Offline transcript review (Spec 04, Transcript review and owner exports; owner's decisions
//! of October 7, 2026). A reviewer, such as one of the teacher's priests, gets one HTML file per
//! talk: the corrected text paragraph by paragraph, each editable, with a link to that moment
//! of the recording. Saving downloads the changed paragraphs as JSON, and the import turns them
//! into a reviewed transcript version that no automated pass overwrites.
//!
//! Word times survive review: in an edited paragraph, words matched unchanged keep their times,
//! and new words share the time of the words they replace. Staged citations are moved to the
//! new word positions, so extraction does not run again.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wt_core::ids::TalkId;
use wt_core::objects::ObjectStore;
use wt_core::records::sha256_hex;
use wt_core::transcript::Word;
use wt_db::job::{self, JobKind};
use wt_db::transcript::{self, NewSegment, NewTranscript};
use wt_extract::extract::Staging;
use wt_index::{ChunkSettings, chunk};
use wt_registry::glossary::Glossary;

use crate::audio::transcript_key;
use crate::textjobs::{TextJobError, corrected, staging_key};

/// Version of the review and corrections formats.
pub const SCHEMA: u32 = 1;

/// One editable paragraph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewParagraph {
    /// The first stored word it covers.
    pub w0: usize,
    /// One past the last.
    pub w1: usize,
    /// Start of its first word.
    pub start_ms: i32,
    /// The text as shown.
    pub text: String,
}

/// What a review file holds, also embedded in its HTML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFile {
    /// Format version.
    pub schema: u32,
    /// The talk.
    pub talk_id: String,
    /// The transcript the file was made from; an import against another is refused.
    pub transcript_hash: String,
    /// The paragraphs.
    pub paragraphs: Vec<ReviewParagraph>,
}

/// One edit: the stored words `[w0, w1)` become `text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    /// The first stored word replaced.
    pub w0: usize,
    /// One past the last.
    pub w1: usize,
    /// The paragraph as the reviewer left it.
    pub text: String,
}

/// The JSON a review file saves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Corrections {
    /// Format version.
    pub schema: u32,
    /// The talk.
    pub talk_id: String,
    /// The transcript the edits were made against.
    pub transcript_hash: String,
    /// The changed paragraphs.
    pub edits: Vec<Edit>,
}

/// Why a review could not be written or imported.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    /// A text-job step failed.
    #[error(transparent)]
    Text(#[from] TextJobError),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] wt_db::DbError),
    /// A connection error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// The queue refused.
    #[error(transparent)]
    Job(#[from] wt_db::job::JobError),
    /// An object could not be read or written.
    #[error(transparent)]
    Objects(#[from] wt_core::objects::ObjectError),
    /// The transcript changed since the file was written.
    #[error("the transcript of talk {0} changed since this review file was made; export a new one")]
    Stale(String),
    /// The corrections file does not fit the transcript.
    #[error("corrections: {0}")]
    Invalid(String),
    /// A file could not be written.
    #[error("{0}")]
    Io(String),
}

/// A word as the transcript JSON stores it.
#[derive(Debug, Deserialize)]
struct StoredWord {
    w: String,
    s: i32,
    e: i32,
    p: f32,
}

/// The current transcript: its JSON, words, hash and version, and whether it was reviewed.
struct Current {
    doc: Value,
    words: Vec<Word>,
    hash: String,
    reviewed: bool,
}

/// The talk's current transcript from the object store, refusing a talk without one or whose object
/// is missing.
async fn current(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    talk: TalkId,
) -> Result<Current, ReviewError> {
    let (_, key, hash) = transcript::current(conn, talk)
        .await?
        .ok_or_else(|| ReviewError::Invalid(format!("talk {talk} has no transcript")))?;
    let bytes = objects
        .get(&key)?
        .ok_or_else(|| ReviewError::Invalid(format!("transcript object {key} is missing")))?;
    let doc: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ReviewError::Invalid(format!("transcript {key}: {e}")))?;
    let words: Vec<StoredWord> = serde_json::from_value(doc["words"].clone())
        .map_err(|e| ReviewError::Invalid(format!("transcript {key} words: {e}")))?;
    let reviewed = doc["origin"].as_str() == Some("reviewed");
    Ok(Current {
        words: words
            .into_iter()
            .map(|w| Word::new(w.w, w.s, w.e, w.p))
            .collect(),
        doc,
        hash,
        reviewed,
    })
}

/// The review file's paragraphs: each segment's corrected words packed by the chunker, each
/// with the stored-word range it covers.
async fn paragraphs(
    conn: &mut sqlx::PgConnection,
    cur: &Current,
    glossary: &Glossary,
    settings: &ChunkSettings,
    talk: TalkId,
) -> Result<Vec<ReviewParagraph>, ReviewError> {
    let segments = transcript::segments(conn, talk).await?;
    let fixed = corrected((!cur.reviewed).then_some(glossary), &cur.words, &segments);
    let mut out = Vec::new();
    for seg_words in &fixed {
        let plain: Vec<Word> = seg_words.iter().map(|(w, _)| w.clone()).collect();
        for p in chunk(&plain, settings) {
            let (a, b) = p.words;
            out.push(ReviewParagraph {
                w0: seg_words[a].1.0,
                w1: seg_words[b - 1].1.1,
                start_ms: p.start_ms,
                text: p.text,
            });
        }
    }
    Ok(out)
}

/// Escapes text for the review page's HTML, quotes included.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Milliseconds as H:MM:SS.
fn clock(ms: i32) -> String {
    let s = ms.max(0) / 1000;
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// Escapes JSON for a `<script type="application/json">` block.
fn script_json(value: &impl Serialize) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// The review page: self-contained, loading nothing remote and storing nothing. Each stored
/// student question stands, not editable, before the first paragraph of its answer.
fn render(
    file: &ReviewFile,
    title: &str,
    teacher: &str,
    source_url: &str,
    questions: &[wt_db::questions::StoredQuestion],
) -> String {
    use wt_segment::questions::{ATTRIBUTION, EDIT_LABEL};
    let mut body = String::new();
    let mut shown = 0;
    for (n, p) in file.paragraphs.iter().enumerate() {
        while let Some(q) = questions.get(shown)
            && q.answer_start_ms <= p.start_ms
        {
            body.push_str(&format!(
                "<section class=\"question\"><p><strong>{ATTRIBUTION}:</strong> {text}</p>\
                 <p class=\"note\">{EDIT_LABEL}</p></section>\n",
                text = esc(&q.text)
            ));
            shown += 1;
        }
        let secs = (p.start_ms.max(0) - 2000).max(0) / 1000;
        body.push_str(&format!(
            "<section><p class=\"at\"><a href=\"{url}#t={secs}s\" target=\"_blank\" rel=\"noopener noreferrer\">Listen at {clock}</a></p>\
             <textarea id=\"p{n}\" data-n=\"{n}\" rows=\"6\" aria-label=\"Paragraph at {clock}\">{text}</textarea></section>\n",
            url = esc(source_url),
            clock = clock(p.start_ms),
            text = esc(&p.text)
        ));
    }
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Review: {title}</title>
<style>
:root {{ --bg:#fbfaf7; --fg:#22211f; --muted:#6b675f; --line:#d9d4c8; --accent:#2f6f4f; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#1b1a18; --fg:#ecebe6; --muted:#a29d93; --line:#3a3833; --accent:#7fbf9f; }} }}
body {{ background:var(--bg); color:var(--fg); font:16px/1.55 system-ui, sans-serif; margin:0 auto; max-width:820px; padding:24px 16px 96px; }}
h1 {{ font-size:22px; margin:0 0 4px; }} .meta, .at, .note {{ color:var(--muted); font-size:14px; }}
textarea {{ width:100%; box-sizing:border-box; font:inherit; color:inherit; background:transparent; border:1px solid var(--line); border-radius:6px; padding:8px; }}
textarea.changed {{ border-color:var(--accent); border-width:2px; }}
section {{ margin:18px 0; }} a {{ color:var(--accent); }}
.bar {{ position:fixed; left:0; right:0; bottom:0; background:var(--bg); border-top:1px solid var(--line); padding:12px 16px; text-align:center; }}
button {{ font:inherit; padding:8px 18px; border-radius:6px; border:1px solid var(--accent); background:var(--accent); color:var(--bg); cursor:pointer; }}
</style></head><body>
<h1>{title}</h1>
<p class="meta">{teacher}</p>
<p class="note">Correct any word that was misheard, then press <strong>Save corrections</strong> and send the downloaded file back. Only the teacher's words are editable here. Students' questions appear, lightly edited by machine, only as context; their own words were never stored. Each "Listen at" link opens the recording at that moment. Nothing on this page is sent anywhere.</p>
{body}
<div class="bar"><button id="save" type="button">Save corrections</button> <span id="count" class="note"></span></div>
<script type="application/json" id="review">{data}</script>
<script>
const review = JSON.parse(document.getElementById('review').textContent);
const areas = [...document.querySelectorAll('textarea')];
const changed = () => areas.filter(a => a.value.trim().split(/\s+/).join(' ') !== review.paragraphs[a.dataset.n].text);
const update = () => {{ areas.forEach(a => a.classList.toggle('changed', changed().includes(a)));
  document.getElementById('count').textContent = changed().length + ' paragraph(s) changed'; }};
areas.forEach(a => a.addEventListener('input', update)); update();
document.getElementById('save').addEventListener('click', () => {{
  const edits = changed().map(a => {{ const p = review.paragraphs[a.dataset.n];
    return {{ w0: p.w0, w1: p.w1, text: a.value.trim().split(/\s+/).join(' ') }}; }});
  const out = {{ schema: review.schema, talk_id: review.talk_id, transcript_hash: review.transcript_hash, edits }};
  const blob = new Blob([JSON.stringify(out, null, 1)], {{ type: 'application/json' }});
  const link = document.createElement('a'); link.href = URL.createObjectURL(blob);
  link.download = 'corrections-' + review.talk_id + '.json'; link.click(); URL.revokeObjectURL(link.href);
}});
</script>
</body></html>
"#,
        title = esc(title),
        teacher = esc(teacher),
        body = body,
        data = script_json(file),
    )
}

/// Writes the review file for one talk into `out` and returns its path and contents.
pub async fn export_review(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    glossary: &Glossary,
    settings: &ChunkSettings,
    talk: TalkId,
    out: &Path,
) -> Result<(PathBuf, ReviewFile), ReviewError> {
    let cur = current(conn, objects, talk).await?;
    let file = ReviewFile {
        schema: SCHEMA,
        talk_id: talk.as_uuid().to_string(),
        transcript_hash: cur.hash.clone(),
        paragraphs: paragraphs(conn, &cur, glossary, settings, talk).await?,
    };
    let facts = transcript::review_facts(conn, talk).await?;
    std::fs::create_dir_all(out)
        .map_err(|e| ReviewError::Io(format!("creating {}: {e}", out.display())))?;
    let path = out.join(format!("review-{}.html", talk.as_uuid()));
    let questions = wt_db::questions::stored(conn, talk).await?;
    std::fs::write(
        &path,
        render(
            &file,
            &facts.title,
            &facts.teacher,
            &facts.source_url,
            &questions,
        ),
    )
    .map_err(|e| ReviewError::Io(format!("writing {}: {e}", path.display())))?;
    Ok((path, file))
}

/// Lowercase letters and digits, for matching a reviewer's words to the recognized ones.
fn key(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// For each new token, the index of the old word it keeps unchanged, by longest common
/// subsequence of the matching keys.
fn align(old: &[&str], new: &[&str]) -> Vec<Option<usize>> {
    let (n, m) = (old.len(), new.len());
    let mut l = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            l[i][j] = if key(old[i]) == key(new[j]) && !key(old[i]).is_empty() {
                l[i + 1][j + 1] + 1
            } else {
                l[i + 1][j].max(l[i][j + 1])
            };
        }
    }
    let mut out = vec![None; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if key(old[i]) == key(new[j]) && !key(old[i]).is_empty() {
            out[j] = Some(i);
            i += 1;
            j += 1;
        } else if l[i + 1][j] >= l[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Replaces a run of words with the reviewer's tokens: matched words keep their times, and each
/// stretch of new words shares, evenly, the time between its matched neighbors.
fn retime(old: &[Word], tokens: &[&str]) -> (Vec<Word>, Vec<Option<usize>>) {
    let old_text: Vec<&str> = old.iter().map(|w| w.text.as_str()).collect();
    let matched = align(&old_text, tokens);
    let run_start = old.first().map_or(0, |w| w.start_ms);
    let run_end = old.last().map_or(run_start, |w| w.end_ms);
    let mut out: Vec<Word> = Vec::with_capacity(tokens.len());
    let mut j = 0;
    while j < tokens.len() {
        if let Some(i) = matched[j] {
            let w = &old[i];
            out.push(Word::new(tokens[j], w.start_ms, w.end_ms, w.prob));
            j += 1;
            continue;
        }
        // A stretch of new words, between the previous kept word and the next.
        let k = (j..tokens.len())
            .find(|&k| matched[k].is_some())
            .unwrap_or(tokens.len());
        let from = out.last().map_or(run_start, |w| w.end_ms);
        let to = matched
            .get(k)
            .copied()
            .flatten()
            .map_or(run_end, |i| old[i].start_ms)
            .max(from);
        let count = i32::try_from(k - j).unwrap_or(i32::MAX).max(1);
        let step = (to - from) / count;
        for (n, t) in tokens[j..k].iter().enumerate() {
            let n = i32::try_from(n).unwrap_or(0);
            let s = from + step * n;
            let e = if n + 1 == count { to } else { s + step };
            out.push(Word::new(*t, s, e.max(s), 1.0));
        }
        j = k;
    }
    (out, matched)
}

/// What an import did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// The talk.
    pub talk: TalkId,
    /// The reviewed version written.
    pub version: i32,
    /// Edits applied.
    pub edits: usize,
    /// Words before and after.
    pub words: (usize, usize),
    /// Staged citations moved to the new positions.
    pub citations_moved: usize,
}

/// Applies a corrections file: a new transcript version with `origin = 'reviewed'`, the
/// segments rebuilt from it, each edit logged with the reviewer, the staged citations moved,
/// and `correct` queued so the talk is indexed again.
pub async fn import_corrections(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    glossary: &Glossary,
    corrections: &Corrections,
    reviewer: &str,
) -> Result<ImportReport, ReviewError> {
    if corrections.schema != SCHEMA {
        return Err(ReviewError::Invalid(format!(
            "schema {} is not {SCHEMA}",
            corrections.schema
        )));
    }
    let talk = TalkId::from(
        corrections
            .talk_id
            .parse::<sqlx::types::Uuid>()
            .map_err(|e| ReviewError::Invalid(format!("talk id: {e}")))?,
    );
    let cur = current(conn, objects, talk).await?;
    if cur.hash != corrections.transcript_hash {
        return Err(ReviewError::Stale(talk.as_uuid().to_string()));
    }
    let mut edits = corrections.edits.clone();
    edits.sort_by_key(|e| e.w0);
    for pair in edits.windows(2) {
        if pair[1].w0 < pair[0].w1 {
            return Err(ReviewError::Invalid("two edits overlap".into()));
        }
    }
    if edits.iter().any(|e| e.w0 >= e.w1 || e.w1 > cur.words.len()) {
        return Err(ReviewError::Invalid(
            "an edit is outside the transcript".into(),
        ));
    }

    // The text as the reviewer saw it: glossary-corrected unless already reviewed, each word
    // with the stored range it came from.
    let shown: Vec<(Word, (usize, usize))> = if cur.reviewed {
        cur.words
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, w)| (w, (i, i + 1)))
            .collect()
    } else {
        glossary.apply_mapped(&cur.words)
    };

    // New words, and for each old stored word the new index it now starts at.
    let mut words: Vec<Word> = Vec::with_capacity(shown.len());
    let mut map = vec![0usize; cur.words.len() + 1];
    let mut log: Vec<(String, String)> = Vec::new();
    let mut k = 0;
    for edit in &edits {
        while k < shown.len() && shown[k].1.0 < edit.w0 {
            let (w, (x, y)) = &shown[k];
            for slot in map.iter_mut().take(*y).skip(*x) {
                *slot = words.len();
            }
            words.push(w.clone());
            k += 1;
        }
        let start = k;
        while k < shown.len() && shown[k].1.1 <= edit.w1 {
            k += 1;
        }
        let run: Vec<Word> = shown[start..k].iter().map(|(w, _)| w.clone()).collect();
        let tokens: Vec<&str> = edit.text.split_whitespace().collect();
        let base = words.len();
        let (new_words, matched) = retime(&run, &tokens);
        // Old words kept map to their new place; dropped ones to the next kept word.
        let mut kept_at = vec![None; run.len()];
        for (j, m) in matched.iter().enumerate() {
            if let Some(i) = m {
                kept_at[*i] = Some(base + j);
            }
        }
        let mut next = base + new_words.len();
        for (i, (_, (x, y))) in shown[start..k].iter().enumerate().rev() {
            let at = kept_at[i].unwrap_or(next);
            next = at;
            for slot in map.iter_mut().take(*y).skip(*x) {
                *slot = at;
            }
        }
        log.push((
            run.iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            edit.text.clone(),
        ));
        words.extend(new_words);
    }
    for (w, (x, y)) in &shown[k..] {
        for slot in map.iter_mut().take(*y).skip(*x) {
            *slot = words.len();
        }
        words.push(w.clone());
    }
    map[cur.words.len()] = words.len();

    // The new JSON: the old one with the reviewed words, spans moved, and who reviewed it.
    let version = transcript::next_version(conn, talk).await?;
    let mut doc = cur.doc.clone();
    doc["version"] = serde_json::json!(version);
    doc["origin"] = serde_json::json!("reviewed");
    doc["reviewed_by"] = serde_json::json!(reviewer);
    doc["words"] = Value::Array(
        words
            .iter()
            .enumerate()
            .map(|(i, w)| serde_json::json!({ "i": i, "w": w.text, "s": w.start_ms, "e": w.end_ms, "p": w.prob }))
            .collect(),
    );
    if let Some(spans) = doc["spans"].as_array_mut() {
        for span in spans {
            for field in ["w0", "w1"] {
                if let Some(old) = span[field].as_u64().and_then(|v| usize::try_from(v).ok()) {
                    span[field] = serde_json::json!(map[old.min(cur.words.len())]);
                }
            }
        }
    }
    let json = serde_json::to_vec(&doc).map_err(|e| ReviewError::Invalid(e.to_string()))?;
    let hash = sha256_hex(&json);
    let key = transcript_key(talk, version);
    objects.put(&key, &json)?;

    // Segments keep their times; their text is the reviewed words inside them.
    let old_segments = transcript::segment_speakers(conn, talk).await?;
    let texts: Vec<String> = old_segments
        .iter()
        .map(|s| {
            words
                .iter()
                .filter(|w| w.start_ms >= s.start_ms && w.start_ms < s.end_ms)
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let segments: Vec<NewSegment<'_>> = old_segments
        .iter()
        .zip(&texts)
        .filter(|(_, t)| !t.is_empty())
        .map(|(s, t)| NewSegment {
            kind: &s.kind,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            speaker_label: &s.speaker_label,
            text: t,
        })
        .collect();

    // Staged citations move to the new positions.
    let mut citations_moved = 0;
    let staged = match objects.get(&staging_key(talk))? {
        Some(bytes) => {
            let mut staging: Staging = serde_json::from_slice(&bytes)
                .map_err(|e| ReviewError::Invalid(format!("staging: {e}")))?;
            for c in &mut staging.citations {
                let (a, b) = (
                    map[c.w0.min(cur.words.len())],
                    map[c.w1.min(cur.words.len())],
                );
                c.w0 = a;
                c.w1 = b.max(a + 1).min(words.len());
                citations_moved += 1;
            }
            // Extracted from other text: a later extract runs again rather than reuse it.
            staging.input_hash = None;
            Some(serde_json::to_vec(&staging).map_err(|e| ReviewError::Invalid(e.to_string()))?)
        }
        None => None,
    };

    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    let transcript_id = transcript::store(
        &mut tx,
        talk,
        &NewTranscript {
            version,
            origin: "reviewed",
            key: &key,
            content_hash: &hash,
            word_count: i32::try_from(words.len()).unwrap_or(i32::MAX),
        },
        &segments,
    )
    .await?;
    for (old, new) in &log {
        wt_db::correction::log_change(
            &mut tx,
            &wt_db::correction::Change {
                entity: "transcript",
                entity_id: transcript_id,
                field: "text",
                old_value: Some(serde_json::json!(old)),
                new_value: Some(serde_json::json!(new)),
                actor: reviewer,
                reason: "transcript review",
            },
        )
        .await?;
    }
    job::enqueue(
        &mut tx,
        JobKind::Correct,
        Some(talk),
        &hash,
        &serde_json::json!({ "reviewed_by": reviewer }),
    )
    .await?;
    tx.commit().await?;
    if let Some(bytes) = staged {
        objects.put(&staging_key(talk), &bytes)?;
    }
    Ok(ImportReport {
        talk,
        version,
        edits: edits.len(),
        words: (cur.words.len(), words.len()),
        citations_moved,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn w(text: &str, s: i32) -> Word {
        Word::new(text, s, s + 300, 0.8)
    }

    #[test]
    fn kept_words_keep_their_times_and_new_ones_share_the_gap() {
        let old = vec![
            w("we", 0),
            w("sit", 400),
            w("on", 800),
            w("the", 1200),
            w("cushion", 1600),
        ];
        let (new, matched) = retime(&old, &["We", "sit", "on", "a", "zafu."]);
        assert_eq!(matched, vec![Some(0), Some(1), Some(2), None, None]);
        assert_eq!(
            (new[0].start_ms, new[1].start_ms, new[2].end_ms),
            (0, 400, 1100)
        );
        // "a zafu." share the time from the end of "on" to the end of "cushion".
        assert_eq!((new[3].start_ms, new[4].end_ms), (1100, 1900));
        assert!(new[3].end_ms <= new[4].start_ms);
        assert_eq!(new[4].text, "zafu.");
        assert!((new[4].prob - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_review_page_escapes_text_and_embeds_its_data_safely() {
        let file = ReviewFile {
            schema: SCHEMA,
            talk_id: "t".into(),
            transcript_hash: "h".into(),
            paragraphs: vec![ReviewParagraph {
                w0: 0,
                w1: 3,
                start_ms: 65_000,
                text: "a </script> <b>bold</b>".into(),
            }],
        };
        let questions = [wt_db::questions::StoredQuestion {
            start_ms: 50_000,
            answer_start_ms: 60_000,
            text: "Why <sit>?".into(),
        }];
        let html = render(&file, "T & T", "Ada", "https://vimeo.com/1", &questions);
        let question = html
            .find("<strong>Student:</strong> Why &lt;sit&gt;?")
            .unwrap();
        assert!(
            question < html.find("<textarea").unwrap(),
            "before its answer"
        );
        assert!(html.contains("Question lightly edited by machine"));
        assert!(html.contains("T &amp; T"));
        assert!(html.contains("&lt;/script&gt; &lt;b&gt;"));
        assert!(
            !html.contains("a </script>"),
            "the embedded JSON cannot close the script"
        );
        assert!(html.contains("https://vimeo.com/1#t=63s"));
        assert!(html.contains("Listen at 0:01:05"));
        assert!(!html.contains("src=\"http"), "nothing remote is loaded");
    }
}
