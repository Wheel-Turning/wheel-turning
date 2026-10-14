//! The owner export (Spec 04, Transcript review and owner exports; display levels d and e): a
//! teacher's own talks, written for their use, such as books. One Markdown file per talk with
//! its title, date, sangha and source link, then the teacher's words in paragraphs, with the
//! removed stretches marked. It never writes student speech or audio, and it reads the stored
//! transcript and segments rather than the serving view, since a teacher at level d or e has
//! nothing servable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wt_core::ids::TalkId;
use wt_core::objects::ObjectStore;
use wt_core::transcript::Word;
use wt_db::transcript::{self, ExportTalk};
use wt_index::{ChunkSettings, chunk};
use wt_registry::glossary::Glossary;

use wt_segment::questions::{ATTRIBUTION, EDIT_LABEL, GAP_LABEL};

use crate::textjobs::{TextJobError, corrected};

/// One block of a talk's text, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Block {
    /// A paragraph of the teacher's words.
    Paragraph {
        /// Start of its first word.
        start_ms: i32,
        /// The words.
        text: String,
    },
    /// A stretch left out: a student's question, chant, announcements, sitting or other.
    Gap {
        /// What it was, in words.
        label: &'static str,
        /// Where it started.
        start_ms: i32,
    },
    /// A student's question, lightly edited by machine, attributed only to "Student" (Spec 04,
    /// Student questions).
    Question {
        /// The edited text.
        text: String,
        /// Where the student turn started.
        start_ms: i32,
    },
}

/// Puts each stored question before the first paragraph at or after its answer, replacing the
/// "A student asked a question" mark there.
fn place_questions(blocks: &mut Vec<Block>, questions: &[wt_db::questions::StoredQuestion]) {
    for q in questions {
        let Some(at) = blocks.iter().position(
            |b| matches!(b, Block::Paragraph { start_ms, .. } if *start_ms >= q.answer_start_ms),
        ) else {
            continue;
        };
        let question = Block::Question {
            text: q.text.clone(),
            start_ms: q.start_ms,
        };
        match at.checked_sub(1).map(|i| &blocks[i]) {
            Some(Block::Gap { label, .. }) if *label == GAP_LABEL => blocks[at - 1] = question,
            _ => blocks.insert(at, question),
        }
    }
}

/// A word as the transcript JSON stores it: text, start, end and probability.
#[derive(Debug, Deserialize)]
struct StoredWord {
    w: String,
    s: i32,
    e: i32,
    p: f32,
}

/// A span as the transcript JSON stores it: its kind and start.
#[derive(Debug, Deserialize)]
struct StoredSpan {
    kind: String,
    s: i32,
}

/// The parts of a stored transcript the export reads.
#[derive(Debug, Deserialize)]
struct StoredDoc {
    words: Vec<StoredWord>,
    #[serde(default)]
    spans: Vec<StoredSpan>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    origin: Option<String>,
}

/// How the export marks a stretch left out of the teacher's words, or none for the spans it keeps.
fn gap_label(kind: &str) -> Option<&'static str> {
    match kind {
        "chant" => Some("Chanting"),
        "announcement" => Some("Announcements"),
        "sitting" => Some("Sitting"),
        "other" => Some("Not transcribed"),
        _ => None,
    }
}

/// A talk's text as blocks: each segment's corrected words packed into paragraphs by the
/// chunker, the transcript's non-talk spans between segments as gaps, and before each teacher
/// answer its student's question, lightly edited, or where none is stored "A student asked a
/// question". Also returns the recognition model's name.
pub async fn talk_blocks(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    glossary: &Glossary,
    paragraphs: &ChunkSettings,
    talk: TalkId,
) -> Result<(Vec<Block>, Option<String>), TextJobError> {
    let (_, key, _) = transcript::current(conn, talk)
        .await?
        .ok_or_else(|| TextJobError::Missing(format!("talk {talk} has no transcript")))?;
    let bytes = objects
        .get(&key)?
        .ok_or_else(|| TextJobError::Missing(format!("transcript object {key} is missing")))?;
    let doc: StoredDoc = serde_json::from_slice(&bytes)
        .map_err(|e| TextJobError::Missing(format!("transcript {key}: {e}")))?;
    let words: Vec<Word> = doc
        .words
        .into_iter()
        .map(|w| Word::new(w.w, w.s, w.e, w.p))
        .collect();
    let segments = transcript::segments(conn, talk).await?;
    let reviewed = doc.origin.as_deref() == Some("reviewed");
    let fixed = corrected((!reviewed).then_some(glossary), &words, &segments);

    let mut blocks = Vec::new();
    let mut previous_end = i32::MIN;
    for (seg, seg_words) in segments.iter().zip(&fixed) {
        for span in doc
            .spans
            .iter()
            .filter(|s| s.s >= previous_end && s.s < seg.start_ms)
        {
            if let Some(label) = gap_label(&span.kind) {
                blocks.push(Block::Gap {
                    label,
                    start_ms: span.s,
                });
            }
        }
        if seg.kind == "qa_teacher" {
            blocks.push(Block::Gap {
                label: GAP_LABEL,
                start_ms: seg.start_ms,
            });
        }
        let plain: Vec<Word> = seg_words.iter().map(|(w, _)| w.clone()).collect();
        for p in chunk(&plain, paragraphs) {
            blocks.push(Block::Paragraph {
                start_ms: p.start_ms,
                text: p.text,
            });
        }
        previous_end = seg.end_ms;
    }
    for span in doc.spans.iter().filter(|s| s.s >= previous_end) {
        if let Some(label) = gap_label(&span.kind) {
            blocks.push(Block::Gap {
                label,
                start_ms: span.s,
            });
        }
    }
    place_questions(&mut blocks, &wt_db::questions::stored(conn, talk).await?);
    Ok((blocks, doc.model))
}

/// Milliseconds as H:MM:SS.
fn clock(ms: i32) -> String {
    let s = ms.max(0) / 1000;
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// A file name: the date, then the title in lowercase letters, digits and dashes.
fn file_name(talk: &ExportTalk) -> String {
    let date = talk
        .talk_date
        .map_or_else(|| "undated".to_owned(), |d| d.to_string());
    let mut slug = String::new();
    for c in talk.title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(60).collect();
    let short_id: String = talk
        .id
        .as_uuid()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect();
    format!("{date}-{slug}-{short_id}.md")
}

/// One talk as Markdown.
pub fn render(talk: &ExportTalk, teacher: &str, blocks: &[Block], model: Option<&str>) -> String {
    let mut out = format!("# {}\n\n", talk.title.trim());
    let date = talk
        .talk_date
        .map_or_else(|| "Date unknown".to_owned(), |d| d.to_string());
    out.push_str(&format!(
        "{teacher} · {} · {date}\n\nSource: {}\n\n",
        talk.sangha.as_deref().unwrap_or("Sangha unknown"),
        talk.source_url
    ));
    let provenance = if talk.origin == "reviewed" {
        "Transcript reviewed by a person.".to_owned()
    } else {
        format!(
            "Transcribed by machine{}, with known spellings corrected; not yet reviewed, so it may contain errors.",
            model.map(|m| format!(" ({m})")).unwrap_or_default()
        )
    };
    out.push_str(&format!(
        "_{provenance} Only the teacher's words are kept, with each student's question lightly edited by machine and attributed to \"{ATTRIBUTION}\"; students' own words and voices are never stored._\n\n---\n\n"
    ));
    for b in blocks {
        match b {
            Block::Paragraph { start_ms, text } => {
                out.push_str(&format!("{text} [{}]\n\n", clock(*start_ms)));
            }
            Block::Gap { label, start_ms } => {
                out.push_str(&format!("_[{label}, {}]_\n\n", clock(*start_ms)));
            }
            Block::Question { text, start_ms } => {
                out.push_str(&format!(
                    "**{ATTRIBUTION}:** {text} [{}]\n\n_{EDIT_LABEL}_\n\n",
                    clock(*start_ms)
                ));
            }
        }
    }
    out
}

/// What the export wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportReport {
    /// The teacher's display name.
    pub teacher: String,
    /// The directory.
    pub dir: PathBuf,
    /// One file per talk.
    pub files: Vec<String>,
    /// Talks whose transcript object could not be read, with why.
    pub skipped: Vec<(TalkId, String)>,
}

/// Writes every transcribed talk of `teacher` under `out/<teacher>/`, plus an index.
pub async fn export_teacher(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    glossary: &Glossary,
    paragraphs: &ChunkSettings,
    teacher: &str,
    out: &Path,
) -> Result<ExportReport, TextJobError> {
    let (id, name) = wt_db::registry::find_teacher(conn, teacher)
        .await
        .map_err(|e| TextJobError::Missing(e.to_string()))?;
    let talks = transcript::teacher_transcribed_talks(conn, id).await?;
    let folder: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let dir = out.join(folder);
    std::fs::create_dir_all(&dir)
        .map_err(|e| TextJobError::Missing(format!("creating {}: {e}", dir.display())))?;
    let mut report = ExportReport {
        teacher: name.clone(),
        dir: dir.clone(),
        files: Vec::new(),
        skipped: Vec::new(),
    };
    let mut index = format!("# Talks of {name}\n\n");
    for talk in &talks {
        let (blocks, model) = match talk_blocks(conn, objects, glossary, paragraphs, talk.id).await
        {
            Ok(b) => b,
            Err(TextJobError::Missing(why)) => {
                report.skipped.push((talk.id, why));
                continue;
            }
            Err(e) => return Err(e),
        };
        let file = file_name(talk);
        std::fs::write(
            dir.join(&file),
            render(talk, &name, &blocks, model.as_deref()),
        )
        .map_err(|e| TextJobError::Missing(format!("writing {file}: {e}")))?;
        index.push_str(&format!(
            "- {} — [{}]({file})\n",
            talk.talk_date
                .map_or_else(|| "undated".to_owned(), |d| d.to_string()),
            talk.title.trim()
        ));
        report.files.push(file);
    }
    std::fs::write(dir.join("README.md"), index)
        .map_err(|e| TextJobError::Missing(format!("writing the index: {e}")))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_rendered_talk_marks_gaps_and_names_its_provenance() {
        let talk = ExportTalk {
            id: TalkId::from(sqlx::types::Uuid::nil()),
            title: "Beginning Again: A Fixture".into(),
            talk_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 7),
            sangha: Some("Fixture Mountain Zendo".into()),
            source_url: "https://vimeo.com/1".into(),
            origin: "asr".into(),
        };
        let blocks = vec![
            Block::Gap {
                label: "Chanting",
                start_ms: 0,
            },
            Block::Paragraph {
                start_ms: 65_000,
                text: "We sit together.".into(),
            },
            Block::Gap {
                label: "A student asked a question",
                start_ms: 600_000,
            },
            Block::Question {
                text: "How do I begin again?".into(),
                start_ms: 660_000,
            },
        ];
        let md = render(&talk, "Ada Fixture", &blocks, Some("whisper-large-v3"));
        assert!(md.starts_with("# Beginning Again: A Fixture\n"));
        assert!(md.contains("Ada Fixture · Fixture Mountain Zendo · 2026-10-07"));
        assert!(md.contains("(whisper-large-v3)"));
        assert!(md.contains("_[Chanting, 0:00:00]_"));
        assert!(md.contains("We sit together. [0:01:05]"));
        assert!(md.contains("_[A student asked a question, 0:10:00]_"));
        assert!(md.contains(
            "**Student:** How do I begin again? [0:11:00]\n\n_Question lightly edited by machine; \
             names and identifying details removed._"
        ));
        assert!(
            !md.contains("> How do I"),
            "a question is never a quotation"
        );
        assert_eq!(
            file_name(&talk),
            "2026-10-07-beginning-again-a-fixture-00000000.md"
        );
    }
}
