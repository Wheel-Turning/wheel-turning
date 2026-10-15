//! Feedback on the answer, and "Flag this query" (Spec 06, Answer page structure, rule 6, and
//! Logging, rule 4, as the owner decided on October 12 and 15, 2026). The page shows one control
//! at its foot; the excerpt and summary targets still take a post from a page served before. One
//! submission carries several reasons and an optional note. With the script the status comes
//! back as text for the control's live region; without it the page reloads at the control.

use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use wt_core::ids::{AnswerId, PassageId};
use wt_db::answer::{FeedbackOutcome, FeedbackTarget};

use crate::{AppState, Session, csrf_refused, not_found_page, parse, server_error};

/// The excerpt reasons with the words the page shows (Spec 09, Excerpt footer and feedback).
pub const EXCERPT_REASONS: [(&str, &str); 6] = [
    ("good", "Helpful"),
    ("not_relevant", "Not relevant to my question"),
    ("out_of_context", "Cut off or missing its context"),
    ("not_faithful", "Words look wrong (a transcription error)"),
    ("wrong_details", "Wrong teacher, talk or date"),
    ("unclear", "Hard to follow"),
];

/// The summary reasons with the words the page shows.
pub const SUMMARY_REASONS: [(&str, &str); 5] = [
    ("good", "Helpful"),
    ("unsupported", "Not supported by the excerpts"),
    ("misrepresents", "Misrepresents a teacher"),
    ("missing", "Missing something important"),
    ("unclear", "Unclear"),
];

/// The reasons for the answer as a whole, the page's one control, with the words it shows.
pub const ANSWER_REASONS: [(&str, &str); 7] = [
    ("good", "Helpful"),
    ("not_relevant", "Not relevant to my question"),
    (
        "unsupported",
        "The summary is not supported by the excerpts",
    ),
    (
        "out_of_context",
        "An excerpt is cut off or missing its context",
    ),
    ("not_faithful", "Words look wrong (a transcription error)"),
    ("wrong_details", "Wrong teacher, talk or date"),
    ("unclear", "Hard to follow"),
];

/// The longest note, in characters.
pub const NOTE_MAX: usize = 1000;

/// Shown when nothing was chosen.
pub const EMPTY: &str = "Choose at least one reason or write a note.";
/// Shown when the note is too long.
pub const TOO_LONG: &str = "Please keep the note under 1,000 characters.";

/// What a submission was about, as the page names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum About {
    /// Excerpt `n` on the page.
    Excerpt(usize),
    /// The summary.
    Summary,
    /// The answer as a whole.
    Answer,
}

impl About {
    /// The control's element id, which the no-script reload jumps to.
    pub fn anchor(self) -> String {
        match self {
            Self::Excerpt(n) => format!("fb-p{n}"),
            Self::Summary => "fb-summary".to_owned(),
            Self::Answer => "fb-answer".to_owned(),
        }
    }

    /// The `fb` query value.
    fn key(self) -> String {
        match self {
            Self::Excerpt(n) => format!("p{n}"),
            Self::Summary => "summary".to_owned(),
            Self::Answer => "answer".to_owned(),
        }
    }

    /// Reads the `fb` query value.
    pub fn from_key(raw: &str) -> Option<Self> {
        match raw {
            "summary" => return Some(Self::Summary),
            "answer" => return Some(Self::Answer),
            _ => {}
        }
        raw.strip_prefix('p')
            .and_then(|n| n.parse().ok())
            .filter(|n| (1..=999).contains(n))
            .map(Self::Excerpt)
    }

    /// The thank-you line.
    pub fn thanks(self) -> String {
        match self {
            Self::Excerpt(n) => format!("Thank you. Your feedback on excerpt {n} was recorded."),
            Self::Summary => "Thank you. Your feedback on the summary was recorded.".to_owned(),
            Self::Answer => "Thank you. Your feedback on these answers was recorded.".to_owned(),
        }
    }
}

/// The status a control shows after a reload without the script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Which control.
    pub about: About,
    /// The line to show.
    pub message: String,
    /// The submission was refused, so the line is an error.
    pub error: bool,
}

impl Status {
    /// From the `fb` and `fbs` query values.
    pub fn from_query(fb: Option<&str>, fbs: Option<&str>) -> Option<Self> {
        let about = About::from_key(fb?)?;
        let (message, error) = match fbs? {
            "ok" => (about.thanks(), false),
            "empty" => (EMPTY.to_owned(), true),
            "long" => (TOO_LONG.to_owned(), true),
            _ => return None,
        };
        Some(Self {
            about,
            message,
            error,
        })
    }

    /// Whether this status belongs to excerpt `n`.
    pub fn is_excerpt(&self, n: &usize) -> bool {
        self.about == About::Excerpt(*n)
    }

    /// Whether this status belongs to the summary.
    pub fn is_summary(&self) -> bool {
        self.about == About::Summary
    }

    /// Whether this status belongs to the answer's control.
    pub fn is_answer(&self) -> bool {
        self.about == About::Answer
    }
}

/// A submission read from the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// The CSRF token.
    pub csrf: String,
    /// "Flag this query" rather than reasons.
    pub flag: bool,
    /// What it is about, for the page.
    pub about: Option<About>,
    /// The passage, for an excerpt.
    pub passage: Option<PassageId>,
    /// The chosen reasons, in form order.
    pub reasons: Vec<String>,
    /// The note, trimmed.
    pub note: Option<String>,
}

/// Reads the form's pairs. Checkboxes repeat `reason`, so the form is read as pairs.
pub fn read(pairs: &[(String, String)]) -> Submission {
    let one = |k: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    let target = one("target");
    let number = one("n").and_then(|n| n.parse::<usize>().ok());
    let about = match target {
        Some("summary") => Some(About::Summary),
        Some("answer") => Some(About::Answer),
        Some("excerpt") => number.filter(|n| (1..=999).contains(n)).map(About::Excerpt),
        _ => None,
    };
    Submission {
        csrf: one("csrf").unwrap_or_default().to_owned(),
        flag: one("kind") == Some("flag_query"),
        about,
        passage: one("passage").and_then(parse::<PassageId>),
        reasons: pairs
            .iter()
            .filter(|(k, _)| k == "reason")
            .map(|(_, v)| v.clone())
            .collect(),
        note: one("note")
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned),
    }
}

/// `POST /a/{answer}/feedback`.
pub async fn post(
    State(state): State<AppState>,
    s: Session,
    Path(answer): Path<String>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let form = read(&pairs);
    if !s.csrf_ok(&state.site.keys, &form.csrf) {
        return csrf_refused();
    }
    let Some(answer) = parse::<AnswerId>(&answer) else {
        return not_found_page();
    };
    if form.flag {
        return match wt_db::answer::flag_query(&state.pool, answer, s.tester).await {
            Ok(true) => Redirect::to(&format!("/a/{answer}?thanks=1")).into_response(),
            Ok(false) => not_found_page(),
            Err(e) => server_error("recording feedback", &e),
        };
    }
    let Some(about) = form.about else {
        return not_found_page();
    };
    let target = match about {
        About::Summary => Some(FeedbackTarget::Summary),
        About::Answer => Some(FeedbackTarget::Answer),
        About::Excerpt(_) => form.passage.map(FeedbackTarget::Excerpt),
    };
    let Some(target) = target else {
        return not_found_page();
    };
    let script = headers.get("hx-request").is_some();
    let reply = |status: StatusCode, code: &str, message: String| -> Response {
        if script {
            (
                status,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                message,
            )
                .into_response()
        } else {
            Redirect::to(&format!(
                "/a/{answer}?fb={}&fbs={code}#{}",
                about.key(),
                about.anchor()
            ))
            .into_response()
        }
    };
    if form
        .note
        .as_ref()
        .is_some_and(|n| n.chars().count() > NOTE_MAX)
    {
        return reply(
            StatusCode::UNPROCESSABLE_ENTITY,
            "long",
            TOO_LONG.to_owned(),
        );
    }
    let note = form.note.as_deref().map(crate::privacy::scrub);
    let reasons: Vec<&str> = form.reasons.iter().map(String::as_str).collect();
    match wt_db::answer::submit_feedback(
        &state.pool,
        answer,
        s.tester,
        target,
        &reasons,
        note.as_deref(),
    )
    .await
    {
        Ok(FeedbackOutcome::Recorded) => reply(StatusCode::OK, "ok", about.thanks()),
        Ok(FeedbackOutcome::Refused) => {
            reply(StatusCode::UNPROCESSABLE_ENTITY, "empty", EMPTY.to_owned())
        }
        Ok(FeedbackOutcome::NotFound) => not_found_page(),
        Err(e) => server_error("recording feedback", &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(raw: &[(&str, &str)]) -> Vec<(String, String)> {
        raw.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn several_reasons_and_a_note_are_read_in_order() {
        let s = read(&pairs(&[
            ("csrf", "t"),
            ("target", "excerpt"),
            ("n", "3"),
            ("passage", "00000000-0000-0000-0000-000000000001"),
            ("reason", "good"),
            ("reason", "unclear"),
            ("note", "  a note  "),
        ]));
        assert_eq!(s.about, Some(About::Excerpt(3)));
        assert!(s.passage.is_some());
        assert_eq!(s.reasons, ["good", "unclear"]);
        assert_eq!(s.note.as_deref(), Some("a note"));
        assert!(!s.flag);
    }

    #[test]
    fn a_blank_note_is_no_note_and_a_bad_number_is_no_target() {
        let s = read(&pairs(&[
            ("target", "excerpt"),
            ("n", "0"),
            ("note", "   "),
        ]));
        assert_eq!(s.about, None);
        assert_eq!(s.note, None);
        assert_eq!(
            read(&pairs(&[("target", "summary")])).about,
            Some(About::Summary)
        );
    }

    #[test]
    fn the_status_comes_back_from_the_query() {
        let ok = Status::from_query(Some("p2"), Some("ok")).unwrap();
        assert!(ok.is_excerpt(&2) && !ok.error);
        assert_eq!(
            ok.message,
            "Thank you. Your feedback on excerpt 2 was recorded."
        );
        let empty = Status::from_query(Some("summary"), Some("empty")).unwrap();
        assert!(empty.is_summary() && empty.error);
        assert_eq!(Status::from_query(Some("p2"), Some("other")), None);
        assert_eq!(Status::from_query(Some("x"), Some("ok")), None);
    }

    #[test]
    fn the_page_labels_cover_exactly_the_reasons_the_database_takes() {
        let excerpt: Vec<&str> = EXCERPT_REASONS.iter().map(|(k, _)| *k).collect();
        assert_eq!(excerpt, wt_db::answer::EXCERPT_REASONS);
        let summary: Vec<&str> = SUMMARY_REASONS.iter().map(|(k, _)| *k).collect();
        assert_eq!(summary, wt_db::answer::SUMMARY_REASONS);
        let answer: Vec<&str> = ANSWER_REASONS.iter().map(|(k, _)| *k).collect();
        assert_eq!(answer, wt_db::answer::ANSWER_REASONS);
    }

    #[test]
    fn the_answer_control_reads_and_thanks() {
        assert_eq!(
            read(&pairs(&[("target", "answer")])).about,
            Some(About::Answer)
        );
        let ok = Status::from_query(Some("answer"), Some("ok")).unwrap();
        assert!(ok.is_answer() && !ok.error);
        assert_eq!(
            ok.message,
            "Thank you. Your feedback on these answers was recorded."
        );
        assert_eq!(About::Answer.anchor(), "fb-answer");
    }
}
