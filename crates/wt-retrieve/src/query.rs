//! Query parsing and named-teacher detection (Spec 05, Query parsing and named-teacher
//! detection).

use std::collections::BTreeSet;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use wt_core::ids::TeacherId;

/// Shortest and longest query, in characters.
pub const MIN_QUERY_CHARS: usize = 3;
/// See [`MIN_QUERY_CHARS`].
pub const MAX_QUERY_CHARS: usize = 500;

/// Words that, just before a short or common alias, mark it as a name (rule 3).
const TITLE_WORDS: [&str; 4] = ["roshi", "sensei", "rev.", "dr."];

/// Why a query was refused. The page shows the message as is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    /// Fewer than three characters after normalizing.
    #[error("Please ask a question of at least {MIN_QUERY_CHARS} characters.")]
    TooShort,
    /// More than 500 characters.
    #[error("Please keep your question under {MAX_QUERY_CHARS} characters.")]
    TooLong,
}

/// Rule 1: trims, collapses whitespace and strips control characters, then checks the length.
pub fn normalize(raw: &str) -> Result<String, QueryError> {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let q = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    match q.chars().count() {
        n if n < MIN_QUERY_CHARS => Err(QueryError::TooShort),
        n if n > MAX_QUERY_CHARS => Err(QueryError::TooLong),
        _ => Ok(q),
    }
}

/// How a query runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Several teachers, chosen by relevance.
    Standard,
    /// One named teacher's passages only.
    NamedTeacher(TeacherId),
}

/// A parsed query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    /// The normalized query, as the page and the log show it.
    pub text: String,
    /// The text searched: the query with a named teacher's alias removed (rule 4).
    pub search_text: String,
    /// Standard or named.
    pub mode: Mode,
    /// Two or more distinct teachers were named, so standard mode runs with a notice (rule 5).
    pub multi_named: bool,
}

/// Finds teacher aliases in a query (rules 2 to 5). Build it at startup and again when the
/// registry changes.
pub struct NamedDetector {
    automaton: AhoCorasick,
    owners: Vec<(TeacherId, String)>,
    common: BTreeSet<String>,
}

impl std::fmt::Debug for NamedDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedDetector")
            .field("aliases", &self.owners.len())
            .finish_non_exhaustive()
    }
}

/// The detector could not be built.
#[derive(Debug, thiserror::Error)]
#[error("alias automaton: {0}")]
pub struct DetectorError(String);

impl NamedDetector {
    /// Builds the detector from `(teacher, alias)` pairs, display names included, and the
    /// lines of `config/common_words.txt`.
    pub fn new(
        aliases: impl IntoIterator<Item = (TeacherId, String)>,
        common_words: &str,
    ) -> Result<Self, DetectorError> {
        let owners: Vec<(TeacherId, String)> = aliases
            .into_iter()
            .map(|(t, a)| (t, a.trim().to_lowercase()))
            .filter(|(_, a)| !a.is_empty())
            .collect();
        let automaton = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::LeftmostLongest)
            .build(owners.iter().map(|(_, a)| a.as_str()))
            .map_err(|e| DetectorError(e.to_string()))?;
        let common = common_words
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_lowercase)
            .collect();
        Ok(Self {
            automaton,
            owners,
            common,
        })
    }

    /// Parses a normalized query.
    pub fn parse(&self, text: &str) -> ParsedQuery {
        let mut found: Vec<(TeacherId, usize, usize)> = Vec::new();
        for m in self.automaton.find_iter(text) {
            if !on_word_boundary(text, m.start(), m.end()) {
                continue;
            }
            let (teacher, alias) = &self.owners[m.pattern().as_usize()];
            let weak = alias.chars().count() < 4 || self.common.contains(alias);
            if weak && !after_title_word(text, m.start()) {
                continue;
            }
            found.push((*teacher, m.start(), m.end()));
        }
        let teachers: BTreeSet<TeacherId> = found.iter().map(|(t, _, _)| *t).collect();
        match teachers.len() {
            1 => {
                let mut search = String::with_capacity(text.len());
                let mut at = 0;
                for (_, start, end) in &found {
                    search.push_str(&text[at..*start]);
                    search.push(' ');
                    at = *end;
                }
                search.push_str(&text[at..]);
                let search_text = search.split_whitespace().collect::<Vec<_>>().join(" ");
                ParsedQuery {
                    text: text.to_owned(),
                    search_text,
                    mode: Mode::NamedTeacher(found[0].0),
                    multi_named: false,
                }
            }
            n => ParsedQuery {
                text: text.to_owned(),
                search_text: text.to_owned(),
                mode: Mode::Standard,
                multi_named: n > 1,
            },
        }
    }
}

fn on_word_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
}

fn after_title_word(text: &str, start: usize) -> bool {
    text[..start]
        .split_whitespace()
        .next_back()
        .is_some_and(|w| TITLE_WORDS.contains(&w.to_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> TeacherId {
        TeacherId::from_uuid(Uuid::from_u128(n))
    }

    fn detector() -> NamedDetector {
        NamedDetector::new(
            [
                (id(1), "Ada Fixture".to_owned()),
                (id(1), "Ada".to_owned()),
                (id(2), "Bo Sample".to_owned()),
                (id(2), "Bo".to_owned()),
                (id(3), "Joy Example".to_owned()),
                (id(3), "Joy".to_owned()),
            ],
            "# comment\njoy\n",
        )
        .unwrap()
    }

    #[test]
    fn normalize_trims_collapses_and_strips_control_characters() {
        assert_eq!(
            normalize("  how\tdo I\u{7}  sit?\n").unwrap(),
            "how do I sit?"
        );
        assert_eq!(normalize(" a "), Err(QueryError::TooShort));
        assert_eq!(normalize(&"x".repeat(501)), Err(QueryError::TooLong));
        assert!(normalize(&"x".repeat(500)).is_ok());
    }

    #[test]
    fn one_named_teacher_sets_named_mode_and_leaves_the_alias_out_of_the_search() {
        let q = detector().parse("What does Ada Fixture say about anger?");
        assert_eq!(q.mode, Mode::NamedTeacher(id(1)));
        assert_eq!(q.search_text, "What does say about anger?");
        assert_eq!(q.text, "What does Ada Fixture say about anger?");
    }

    #[test]
    fn two_named_teachers_run_standard_mode_with_the_notice() {
        let q = detector().parse("Ada Fixture and Bo Sample on grief");
        assert_eq!(q.mode, Mode::Standard);
        assert!(q.multi_named);
        assert_eq!(q.search_text, q.text);
    }

    #[test]
    fn short_and_common_aliases_need_a_title_word() {
        let d = detector();
        assert_eq!(d.parse("finding joy in practice").mode, Mode::Standard);
        assert_eq!(d.parse("what did bo say").mode, Mode::Standard);
        assert_eq!(
            d.parse("what did sensei Bo say").mode,
            Mode::NamedTeacher(id(2))
        );
        assert_eq!(
            d.parse("Roshi Joy on effort").mode,
            Mode::NamedTeacher(id(3))
        );
    }

    #[test]
    fn an_alias_inside_a_word_does_not_match() {
        assert_eq!(detector().parse("Adams on zazen").mode, Mode::Standard);
    }
}
