//! The persisted form of an answer (`answer.payload`, Spec 05): mode, counts, the summary's
//! sentences with the passages and quotation spans they cite (theme text with passage ids for
//! answers stored before the summary), and ordered passage ids. Never passage text. Takedown
//! reads and rewrites it to strip what cites removed passages.

use serde::{Deserialize, Serialize};

use crate::excerpt::ExcerptSpan;
use crate::ids::PassageId;

/// One theme as stored: its one-line description and the passages that support it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTheme {
    /// The theme sentence.
    pub description: String,
    /// The supporting passages.
    pub passage_ids: Vec<PassageId>,
}

/// Where a summary sentence sits (Spec 05, Summary and verifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryPart {
    /// The one-sentence answer.
    Headline,
    /// What the excerpts address.
    Framing,
    /// Where the talks agree.
    Agree,
    /// Where they differ.
    Differ,
    /// Context a quoted line leaves out.
    KeepInMind,
    /// The closing sentence.
    SumUp,
}

/// One citation of a summary sentence: the excerpt it cites and the quotation the code took
/// from it, kept as a span so a permalink rebuilds the words from serving rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCite {
    /// The selected passage whose excerpt is cited.
    pub passage_id: PassageId,
    /// The cited sentences: the passages they cover and their word offsets.
    pub quote: ExcerptSpan,
}

impl StoredCite {
    /// Whether the citation rests on any of `removed`.
    pub fn touches(&self, removed: &[PassageId]) -> bool {
        removed.contains(&self.passage_id) || self.quote.covers.iter().any(|p| removed.contains(p))
    }
}

/// One verified summary sentence: model text, never a quotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSentence {
    /// Its section.
    pub part: SummaryPart,
    /// The sentence.
    pub text: String,
    /// Its citations, in the order the model gave them.
    pub cites: Vec<StoredCite>,
}

impl StoredSentence {
    /// The distinct cited passages, in citing order.
    pub fn passage_ids(&self) -> Vec<PassageId> {
        let mut ids: Vec<PassageId> = Vec::new();
        for c in &self.cites {
            if !ids.contains(&c.passage_id) {
                ids.push(c.passage_id);
            }
        }
        ids
    }
}

/// The summary as stored: its sentences in page order. It always holds a headline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSummary {
    /// Sentences, headline first.
    pub sentences: Vec<StoredSentence>,
}

impl StoredSummary {
    /// Whether a headline is among the sentences.
    pub fn has_headline(&self) -> bool {
        self.sentences
            .iter()
            .any(|s| s.part == SummaryPart::Headline)
    }
}

/// The model sentences a reviewer rates claim by claim (Spec 07): the summary's sentences, or
/// for an answer stored before the summary, its themes.
pub fn claims(themes: &[StoredTheme], summary: Option<&StoredSummary>) -> Vec<StoredTheme> {
    match summary {
        Some(s) => s
            .sentences
            .iter()
            .map(|s| StoredTheme {
                description: s.text.clone(),
                passage_ids: s.passage_ids(),
            })
            .collect(),
        None => themes.to_vec(),
    }
}

/// Removes every sentence that cites any of `removed`, and the whole summary when its headline
/// goes. Returns how many sentences went.
pub fn strip_summary(summary: &mut Option<StoredSummary>, removed: &[PassageId]) -> usize {
    let Some(s) = summary.as_mut() else {
        return 0;
    };
    let before = s.sentences.len();
    s.sentences
        .retain(|sentence| !sentence.cites.iter().any(|c| c.touches(removed)));
    if !s.has_headline() {
        *summary = None;
        return before;
    }
    before - s.sentences.len()
}

/// The JSON stored in `answer.payload`. Unknown fields are kept so older and newer builds can
/// read each other's answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerPayload {
    /// The themes shown above the passages.
    #[serde(default)]
    pub themes: Vec<StoredTheme>,
    /// The summary, for answers stored since October 12, 2026.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<StoredSummary>,
    /// The passages in the order they were shown.
    #[serde(default)]
    pub passage_ids: Vec<PassageId>,
    /// Everything else (mode, counts) as written.
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

impl AnswerPayload {
    /// Removes every theme and summary sentence that cites any of `removed` (the whole summary
    /// when its headline goes), and the removed passages from the order. Returns how many
    /// themes and sentences were dropped.
    pub fn strip_passages(&mut self, removed: &[PassageId]) -> usize {
        let before = self.themes.len();
        self.themes
            .retain(|theme| !theme.passage_ids.iter().any(|id| removed.contains(id)));
        self.passage_ids.retain(|id| !removed.contains(id));
        before - self.themes.len() + strip_summary(&mut self.summary, removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn stripping_drops_themes_that_cite_removed_passages() {
        let keep = PassageId::from(Uuid::new_v4());
        let gone = PassageId::from(Uuid::new_v4());
        let mut payload: AnswerPayload = serde_json::from_value(serde_json::json!({
            "mode": "standard",
            "themes": [
                {"description": "A", "passage_ids": [keep, gone]},
                {"description": "B", "passage_ids": [keep]}
            ],
            "passage_ids": [keep, gone]
        }))
        .unwrap();
        assert_eq!(payload.strip_passages(&[gone]), 1);
        assert_eq!(payload.themes.len(), 1);
        assert_eq!(payload.themes[0].description, "B");
        assert_eq!(payload.passage_ids, vec![keep]);
        assert_eq!(payload.rest["mode"], "standard");
    }

    fn sentence(part: SummaryPart, cited: PassageId, neighbor: PassageId) -> StoredSentence {
        StoredSentence {
            part,
            text: format!("{part:?}"),
            cites: vec![StoredCite {
                passage_id: cited,
                quote: ExcerptSpan {
                    passage_id: cited,
                    covers: vec![neighbor, cited],
                    start: 3,
                    end: 5,
                    chooser: crate::excerpt::Chooser::Model,
                },
            }],
        }
    }

    #[test]
    fn stripping_drops_summary_sentences_and_the_summary_without_its_headline() {
        let (a, b, n) = (
            PassageId::from(Uuid::new_v4()),
            PassageId::from(Uuid::new_v4()),
            PassageId::from(Uuid::new_v4()),
        );
        let summary = StoredSummary {
            sentences: vec![
                sentence(SummaryPart::Headline, a, a),
                sentence(SummaryPart::Agree, b, n),
            ],
        };
        let mut payload: AnswerPayload = serde_json::from_value(serde_json::json!({
            "mode": "standard", "passage_ids": [a, b], "summary": summary
        }))
        .unwrap();
        // A neighbor covered only by a quotation still takes its sentence with it.
        assert_eq!(payload.strip_passages(&[n]), 1);
        assert_eq!(payload.summary.as_ref().unwrap().sentences.len(), 1);
        assert_eq!(payload.strip_passages(&[a]), 1);
        assert!(payload.summary.is_none(), "no headline, no summary");
        let cited: Vec<Vec<PassageId>> = claims(&[], Some(&summary))
            .into_iter()
            .map(|c| c.passage_ids)
            .collect();
        assert_eq!(cited, vec![vec![a], vec![b]]);
    }
}
