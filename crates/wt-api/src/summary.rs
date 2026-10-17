//! The summary block of the answer page (Spec 05, Summary and verifier; Spec 06, Answer page
//! structure, item 4). Sentences are model text; every quotation behind a marker is sliced by
//! code from serving rows through its stored span, so a permalink shows only what still serves.

use std::collections::BTreeSet;

use wt_core::answer::{StoredSummary, SummaryPart};
use wt_core::excerpt::{Excerpt, WindowPassage};
use wt_core::ids::PassageId;
use wt_db::answer::Served;

use crate::view::AnswerView;

/// One summary sentence with its markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentenceView {
    /// The sentence, model text.
    pub text: String,
    /// The numbers of its quoted lines.
    pub markers: Vec<usize>,
}

/// One quoted line behind a marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteView {
    /// Its number, in order of first appearance.
    pub number: usize,
    /// The verbatim quotation.
    pub text: String,
    /// The teacher.
    pub teacher: String,
    /// The talk title.
    pub title: String,
    /// The talk's date, when known.
    pub date: Option<String>,
    /// The number of the excerpt below it comes from.
    pub excerpt: usize,
}

/// The summary as the page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryView {
    /// The one-sentence answer.
    pub headline: SentenceView,
    /// The framing paragraph's sentences.
    pub framing: Vec<SentenceView>,
    /// Where the talks agree.
    pub agree: Vec<SentenceView>,
    /// Where they differ.
    pub differ: Vec<SentenceView>,
    /// Points to keep in mind from the model.
    pub keep: Vec<SentenceView>,
    /// Points to keep in mind the code writes from counts.
    pub notes: Vec<String>,
    /// The closing sentence.
    pub sum_up: Option<SentenceView>,
    /// "The summary draws on n of these excerpts." (Spec 11, PB2.3)
    pub drawn_from: String,
    /// The quoted lines, in marker order.
    pub quotes: Vec<QuoteView>,
}

impl SummaryView {
    /// Whether "Points to keep in mind" has anything to show.
    pub fn has_points(&self) -> bool {
        !self.keep.is_empty() || !self.notes.is_empty()
    }
}

/// A count with its noun, singular for one.
pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The note the code writes from counts over the excerpts the page shows: few talks (Spec 05,
/// Summary and verifier, item 5). The one-sangha note was removed at the owner's request
/// (Spec 11, PB2.3).
pub fn notes(served: &[Served], shown: &[PassageId]) -> Vec<String> {
    let talks: BTreeSet<_> = served
        .iter()
        .filter(|s| shown.contains(&s.id))
        .map(|s| s.talk.id)
        .collect();
    let mut out = Vec::new();
    if !talks.is_empty() && talks.len() <= 2 {
        out.push(format!(
            "Only {} in this collection matched this question closely.",
            plural(talks.len(), "talk", "talks")
        ));
    }
    out
}

/// Builds the summary block for a stored summary. A citation shows only while its excerpt is
/// on the page and its quotation still rebuilds from serving rows; a sentence left without
/// one is left out, and without its headline there is no summary.
pub fn build(
    stored: &StoredSummary,
    view: &AnswerView,
    served: &[Served],
    rows: &[WindowPassage],
) -> Option<SummaryView> {
    let numbers: Vec<(PassageId, usize)> = view
        .groups
        .iter()
        .chain(&view.also_related)
        .flat_map(|g| g.passages.iter().map(|p| (p.id, p.number)))
        .collect();
    let mut quotes: Vec<QuoteView> = Vec::new();
    let mut keys: Vec<(PassageId, Vec<PassageId>, usize, usize)> = Vec::new();
    let mut cited: Vec<PassageId> = Vec::new();
    let mut sentences: Vec<(SummaryPart, SentenceView)> = Vec::new();
    for s in &stored.sentences {
        let mut markers = Vec::new();
        for c in &s.cites {
            let Some(&(_, excerpt)) = numbers.iter().find(|(id, _)| *id == c.passage_id) else {
                continue;
            };
            let Some(card) = served.iter().find(|p| p.id == c.passage_id) else {
                continue;
            };
            let Some(text) = Excerpt::rebuild(&c.quote, rows)
                .map(|e| wt_core::entities::decode(&e.text).into_owned())
            else {
                continue;
            };
            let key = (
                c.passage_id,
                c.quote.covers.clone(),
                c.quote.start,
                c.quote.end,
            );
            let number = match keys.iter().position(|k| *k == key) {
                Some(i) => i + 1,
                None => {
                    keys.push(key);
                    let number = keys.len();
                    quotes.push(QuoteView {
                        number,
                        text,
                        teacher: card.talk.teacher_name.clone(),
                        title: card.talk.title.clone(),
                        date: card.talk.date.map(|d| d.format("%B %-d, %Y").to_string()),
                        excerpt,
                    });
                    number
                }
            };
            if !markers.contains(&number) {
                markers.push(number);
            }
            if !cited.contains(&c.passage_id) {
                cited.push(c.passage_id);
            }
        }
        if !markers.is_empty() {
            sentences.push((
                s.part,
                SentenceView {
                    text: s.text.clone(),
                    markers,
                },
            ));
        }
    }
    let take = |part: SummaryPart| -> Vec<SentenceView> {
        sentences
            .iter()
            .filter(|(p, _)| *p == part)
            .map(|(_, s)| s.clone())
            .collect()
    };
    let headline = take(SummaryPart::Headline).into_iter().next()?;
    let shown: Vec<PassageId> = numbers.iter().map(|(id, _)| *id).collect();
    Some(SummaryView {
        headline,
        framing: take(SummaryPart::Framing),
        agree: take(SummaryPart::Agree),
        differ: take(SummaryPart::Differ),
        keep: take(SummaryPart::KeepInMind),
        notes: notes(served, &shown),
        sum_up: take(SummaryPart::SumUp).into_iter().next(),
        drawn_from: format!("The summary draws on {} of these excerpts.", cited.len()),
        quotes,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use sqlx::types::Uuid;
    use wt_core::ids::{SanghaId, TalkId, TeacherId};
    use wt_db::answer::TalkCard;

    fn served(n: u128, talk: u128, sangha: &str) -> Served {
        Served {
            id: PassageId::from_uuid(Uuid::from_u128(n)),
            text: "Sit.".into(),
            ordinal: 0,
            start_ms: 0,
            end_ms: 1000,
            talk: TalkCard {
                id: TalkId::from_uuid(Uuid::from_u128(100 + talk)),
                title: "On sitting".into(),
                date: None,
                series: None,
                part: None,
                platform: "youtube".into(),
                external_id: "abc".into(),
                source_url: "https://example.invalid/talk".into(),
                teacher_id: TeacherId::from_uuid(Uuid::from_u128(200 + talk)),
                teacher_name: "Ada Fixture".into(),
                sangha_id: SanghaId::from_uuid(Uuid::from_u128(300)),
                sangha_name: sangha.into(),
                sangha_listed: true,
                donation_url: None,
                donation_verified: false,
                rights_basis: None,
                display_notice: None,
                permission_of: None,
                display_level: "full".into(),
                transcript_from_source: false,
                transcript_url: None,
                transcript_origin: Some("asr".into()),
                citation_name: None,
                citation_sort: None,
            },
        }
    }

    #[test]
    fn the_code_notes_few_talks_and_never_one_sangha() {
        let s = vec![
            served(1, 1, "North Fixture Zendo"),
            served(2, 2, "North Fixture Zendo"),
            served(3, 3, "South Fixture Sangha"),
        ];
        let ids: Vec<PassageId> = s.iter().map(|p| p.id).collect();
        assert!(
            notes(&s, &ids).is_empty(),
            "no note on how many excerpts come from one sangha"
        );
        assert_eq!(
            notes(&s, &ids[..1]),
            vec!["Only 1 talk in this collection matched this question closely.".to_owned()],
            "only the excerpts on the page count"
        );
    }
}
