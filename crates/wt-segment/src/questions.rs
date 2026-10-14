//! Student questions (Spec 04, Student questions; owner's decision of October 10, 2026). A
//! student turn that a teacher's answer follows goes, with the start of the answer for context,
//! to a [`QuestionEditor`], which returns the student's own wording lightly edited: identifying
//! details replaced by generic words, filler removed, nothing summarized, answered or added.
//! [`check`] then decides in code whether the edit may be stored. Only an edit that passes is
//! kept; the student's original words are dropped with the [`AnsweredTurn`] that held them.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use wt_core::records::sha256_hex;

use crate::segmenter::{AnsweredTurn, SegmentSettings};

/// The system prompt, `prompts/student_question.md`.
pub const PROMPT: &str = include_str!("../prompts/student_question.md");

pub use wt_core::transcript::{
    STUDENT_ATTRIBUTION as ATTRIBUTION, STUDENT_GAP_LABEL as GAP_LABEL,
    STUDENT_QUESTION_LABEL as EDIT_LABEL,
};

/// The editor's reply.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QuestionEdit {
    /// The edited question; empty when the student asked nothing.
    pub question: String,
    /// Whether the editor replaced an identifying detail.
    pub removed_identifying_details: bool,
}

/// Why an edit could not be made. The gap keeps "A student asked a question" in every case.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The month's language-model cap is reached.
    #[error("{0}")]
    CapReached(String),
    /// The call failed or was refused.
    #[error("{0}")]
    Failed(String),
}

/// Edits one student turn. The language-model editor lives in `wt-extract`.
#[async_trait]
pub trait QuestionEditor: Send + Sync {
    /// The model that makes the edit, recorded as `edit_model`.
    fn model(&self) -> &str;
    /// Edits the student's words in `turn`.
    async fn edit(&self, turn: &AnsweredTurn) -> Result<QuestionEdit, EditError>;
}

/// The user message for one turn.
pub fn user_message(turn: &AnsweredTurn) -> String {
    format!(
        "Student's words:\n{}\n\nStart of the teacher's answer, for context only:\n{}",
        turn.student, turn.answer
    )
}

/// An edit that passed the checks, ready to stage and, at the index step, to embed and store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditedQuestion {
    /// Start of the student turn.
    pub start_ms: i32,
    /// End of the student turn.
    pub end_ms: i32,
    /// Start of the teacher's answer.
    pub answer_start_ms: i32,
    /// The edited text.
    pub text: String,
    /// Its words.
    pub word_count: i32,
    /// SHA-256 of the text.
    pub content_hash: String,
    /// The model that edited it.
    pub edit_model: String,
}

/// Why an edit is not stored.
#[derive(Debug, Clone, PartialEq)]
pub enum Rejection {
    /// Fewer than three words, or more than the maximum.
    WordCount(usize),
    /// A teacher, alias or sangha name from the registry.
    RegistryName,
    /// An email address.
    Email,
    /// A phone number.
    Phone,
    /// A web address.
    WebAddress,
    /// Six or more digits in a row.
    DigitRun,
    /// Too few of its content words are the student's own: a rewrite or an answer.
    Overlap(f64),
}

impl Rejection {
    /// The reason in the job log. It never quotes the text.
    pub fn reason(&self) -> String {
        match self {
            Rejection::WordCount(n) => format!("word_count {n}"),
            Rejection::RegistryName => "registry_name".into(),
            Rejection::Email => "email".into(),
            Rejection::Phone => "phone".into(),
            Rejection::WebAddress => "web_address".into(),
            Rejection::DigitRun => "digit_run".into(),
            Rejection::Overlap(share) => format!("overlap {share:.2}"),
        }
    }
}

/// Compiles one of the constant patterns below; a fixed pattern that compiles once always does.
#[allow(clippy::expect_used)]
fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a constant pattern compiles")
}

/// An email address in an edit, which the edit must not keep (Spec 04, Student questions).
static EMAIL: LazyLock<Regex> = LazyLock::new(|| regex(r"[\w.+-]+@[\w-]+\.[\w.-]+"));
/// A web address or a bare domain in an edit.
static URL: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\b(?:https?://|www\.)\S+|\b[a-z0-9-]+\.(?:com|org|net|edu|gov|io|info|co|us|uk|ca|de)\b",
    )
});
/// A phone number in an edit.
static PHONE: LazyLock<Regex> = LazyLock::new(|| regex(r"\+?\(?\d[\d\s().-]{5,}\d"));
/// Six digits in a row in an edit.
static DIGITS: LazyLock<Regex> = LazyLock::new(|| regex(r"\d{6,}"));

/// Function words left out of the overlap: an edit made only of them shares nothing to check.
const STOPWORDS: &[&str] = &[
    "a", "about", "after", "all", "also", "am", "an", "and", "any", "are", "as", "at", "be",
    "because", "been", "but", "by", "can", "could", "did", "do", "does", "for", "from", "had",
    "has", "have", "he", "her", "him", "his", "how", "i", "if", "in", "into", "is", "it", "its",
    "just", "me", "my", "myself", "no", "not", "of", "on", "or", "our", "out", "she", "so", "some",
    "that", "the", "their", "them", "then", "there", "these", "they", "this", "those", "to", "up",
    "us", "was", "we", "were", "what", "when", "where", "which", "who", "why", "will", "with",
    "would", "you", "your",
];

/// Lowercase letters and digits of each word.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether `name`'s words occur in order, as whole words, in `words`. A one-word name under
/// three letters is ignored, since it would match ordinary words.
fn names_in(words: &[String], name: &str) -> bool {
    let needle = tokens(name);
    match needle.len() {
        0 => false,
        1 if needle[0].chars().count() < 3 => false,
        n => words.windows(n).any(|w| w == needle.as_slice()),
    }
}

/// Spec 04, Student questions, step 3: whether `edit` may be stored, given the student's
/// original words and the registry's teacher, alias and sangha names. Returns its word count.
pub fn check(
    edit: &str,
    original: &str,
    names: &[String],
    cfg: &SegmentSettings,
) -> Result<usize, Rejection> {
    let words = tokens(edit);
    let count = edit.split_whitespace().count();
    if count < 3 || count > cfg.student_question_max_words {
        return Err(Rejection::WordCount(count));
    }
    if EMAIL.is_match(edit) {
        return Err(Rejection::Email);
    }
    if URL.is_match(edit) {
        return Err(Rejection::WebAddress);
    }
    if DIGITS.is_match(edit) {
        return Err(Rejection::DigitRun);
    }
    if PHONE
        .find_iter(edit)
        .any(|m| m.as_str().chars().filter(char::is_ascii_digit).count() >= 7)
    {
        return Err(Rejection::Phone);
    }
    if names.iter().any(|n| names_in(&words, n)) {
        return Err(Rejection::RegistryName);
    }
    let own: BTreeSet<String> = tokens(original).into_iter().collect();
    let content: Vec<&String> = words
        .iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect();
    let checked: Vec<&String> = if content.is_empty() {
        words.iter().collect()
    } else {
        content
    };
    let known = |w: &str| {
        own.contains(w)
            || w.strip_suffix('s').is_some_and(|s| own.contains(s))
            || own.contains(&format!("{w}s"))
    };
    let shared = checked.iter().filter(|w| known(w)).count();
    let share = if checked.is_empty() {
        0.0
    } else {
        shared as f64 / checked.len() as f64
    };
    if share < cfg.student_question_min_overlap {
        return Err(Rejection::Overlap(share));
    }
    Ok(count)
}

/// What editing a talk's questions came to, for the job log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditReport {
    /// Answered student turns.
    pub turns: usize,
    /// Edits that passed and will be stored.
    pub stored: usize,
    /// Edits the checks refused, by reason.
    pub rejected: Vec<String>,
    /// Calls that failed.
    pub failed: usize,
    /// Whether the cap stopped the calls; the remaining gaps stay.
    pub capped: bool,
    /// Whether no editor was available, so no call was made.
    pub no_editor: bool,
}

/// Edits every answered turn and keeps the edits that pass [`check`]. Without an editor, as when
/// real text may not go to the model, nothing is called and every gap stays. A cap refusal
/// stops further calls.
pub async fn edit_all(
    turns: &[AnsweredTurn],
    editor: Option<&dyn QuestionEditor>,
    names: &[String],
    cfg: &SegmentSettings,
) -> (Vec<EditedQuestion>, EditReport) {
    let mut report = EditReport {
        turns: turns.len(),
        ..EditReport::default()
    };
    let Some(editor) = editor else {
        report.no_editor = !turns.is_empty();
        return (Vec::new(), report);
    };
    let mut out = Vec::new();
    for turn in turns {
        let edit = match editor.edit(turn).await {
            Ok(edit) => edit,
            Err(EditError::CapReached(m)) => {
                tracing::warn!(reason = %m, "question edits stopped at the spend cap");
                report.capped = true;
                break;
            }
            Err(EditError::Failed(m)) => {
                tracing::warn!(start_ms = turn.start_ms, error = %m, "question edit failed");
                report.failed += 1;
                continue;
            }
        };
        let text = edit
            .question
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        match check(&text, &turn.student, names, cfg) {
            Ok(count) => {
                out.push(EditedQuestion {
                    start_ms: turn.start_ms,
                    end_ms: turn.end_ms,
                    answer_start_ms: turn.answer_start_ms,
                    word_count: i32::try_from(count).unwrap_or(i32::MAX),
                    content_hash: sha256_hex(text.as_bytes()),
                    text,
                    edit_model: editor.model().to_owned(),
                });
                report.stored += 1;
            }
            Err(r) => {
                tracing::info!(start_ms = turn.start_ms, reason = %r.reason(),
                               "question edit not stored");
                report.rejected.push(r.reason());
            }
        }
    }
    (out, report)
}

/// An editor for tests: returns the edit `f` makes of the student's words.
pub struct FakeEditor<F: Fn(&str) -> String + Send + Sync> {
    /// Makes the edit.
    f: F,
    /// Turns it was asked to edit.
    pub calls: std::sync::atomic::AtomicUsize,
}

impl<F: Fn(&str) -> String + Send + Sync> FakeEditor<F> {
    /// An editor applying `f`.
    pub fn new(f: F) -> Self {
        Self {
            f,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl<F: Fn(&str) -> String + Send + Sync> QuestionEditor for FakeEditor<F> {
    fn model(&self) -> &str {
        "fake-editor"
    }

    async fn edit(&self, turn: &AnsweredTurn) -> Result<QuestionEdit, EditError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(QuestionEdit {
            question: (self.f)(&turn.student),
            removed_identifying_details: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SegmentSettings {
        SegmentSettings::default()
    }

    const ORIGINAL: &str = "um so I was wondering uh when my mind wanders during zazen \
                            should I count my breath again or just let it wander";

    fn names() -> Vec<String> {
        vec![
            "Ada Fixture".into(),
            "Ada".into(),
            "Fixture Mountain Zendo".into(),
            "Ed".into(),
        ]
    }

    #[test]
    fn a_light_edit_passes() {
        let edit = "When my mind wanders during zazen, should I count my breath again or just \
                    let it wander?";
        assert_eq!(check(edit, ORIGINAL, &names(), &cfg()), Ok(17));
    }

    #[test]
    fn identifying_details_and_rewrites_are_refused() {
        let c = cfg();
        let n = names();
        let bad = [
            (
                "Ada, when my mind wanders should I count",
                Rejection::RegistryName,
            ),
            (
                "At Fixture Mountain Zendo my mind wanders during zazen",
                Rejection::RegistryName,
            ),
            (
                "Write me at sam@example.org when my mind wanders",
                Rejection::Email,
            ),
            (
                "My mind wanders, call 415 555 0100 during zazen",
                Rejection::Phone,
            ),
            (
                "My mind wanders, see example.com during zazen",
                Rejection::WebAddress,
            ),
            (
                "My mind wanders at https://x.test/a during zazen",
                Rejection::WebAddress,
            ),
            (
                "My mind wanders, member 1234567 during zazen",
                Rejection::DigitRun,
            ),
            ("Why?", Rejection::WordCount(1)),
        ];
        for (edit, want) in bad {
            assert_eq!(check(edit, ORIGINAL, &n, &c), Err(want), "{edit}");
        }
        // An answer instead of the question shares too few of the student's words.
        let answer = "Simply return attention gently, noticing thoughts without judgment.";
        assert!(matches!(
            check(answer, ORIGINAL, &n, &c),
            Err(Rejection::Overlap(_))
        ));
        let long = "word ".repeat(121);
        assert_eq!(check(&long, &long, &n, &c), Err(Rejection::WordCount(121)));
        // A two-letter alias is not matched inside ordinary words or alone.
        assert!(check("I edited my breath count during zazen", ORIGINAL, &n, &c).is_ok());
    }

    #[tokio::test]
    async fn only_passing_edits_are_kept_and_no_editor_means_no_call() {
        let turn = AnsweredTurn {
            start_ms: 1000,
            end_ms: 5000,
            answer_start_ms: 6000,
            student: ORIGINAL.into(),
            answer: "t1 t2".into(),
        };
        let editor = FakeEditor::new(|s: &str| s.replace("um so ", "").replace("uh ", ""));
        let (kept, report) =
            edit_all(std::slice::from_ref(&turn), Some(&editor), &names(), &cfg()).await;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].answer_start_ms, 6000);
        assert_eq!(kept[0].edit_model, "fake-editor");
        assert_eq!(report.stored, 1);

        let leaky = FakeEditor::new(|s: &str| format!("Ada Fixture asks: {s}"));
        let (kept, report) =
            edit_all(std::slice::from_ref(&turn), Some(&leaky), &names(), &cfg()).await;
        assert!(kept.is_empty());
        assert_eq!(report.rejected, ["registry_name"]);

        let (kept, report) = edit_all(std::slice::from_ref(&turn), None, &names(), &cfg()).await;
        assert!(kept.is_empty() && report.no_editor);
    }

    #[test]
    fn debug_output_never_shows_the_students_words() {
        let turn = AnsweredTurn {
            start_ms: 1,
            end_ms: 2,
            answer_start_ms: 3,
            student: "secret words".into(),
            answer: "t".into(),
        };
        let shown = format!("{turn:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }
}
