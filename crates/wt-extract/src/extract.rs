//! Citation extraction (Spec 04, Extraction and citation resolution). The model reads the
//! corrected segments in windows and names the words that cite or quote a text. Only words that
//! really occur in the window are kept: each citation is matched back to the window by the code,
//! never by the model's offsets, and anything not found is discarded and counted. Kept citations
//! record positions in the stored, uncorrected teacher words, which a glossary change never
//! moves.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use wt_core::config::LlmStage;
use wt_core::transcript::Word;

use crate::llm::{DataClass, LlmClient, LlmError, LlmRequest, complete_json};

/// Words per window.
pub const WINDOW_WORDS: usize = 6_000;
/// Words shared by neighboring windows.
pub const OVERLAP_WORDS: usize = 200;

/// The extractor's instructions (Spec 04): copy the teacher's exact words for each citation or
/// quotation, never paraphrase, with title and locator hints and a kind.
const SYSTEM: &str = "You read a transcript of a Zen dharma talk and find where the teacher \
cites or quotes a text: a sutra, a teaching of Dogen or another ancestor, a koan or case, a \
poem or verse. For each, copy the teacher's exact words from the transcript that make the \
citation or quotation, without changing or adding anything. Give the work's title as a hint if \
the teacher names or clearly means one, and a locator hint such as a paragraph, case or verse \
number if the teacher gives one. Classify each as reference (the teacher points to a text), \
quotation (the teacher recites words of a text) or case (a koan or case story). Also report the \
practice period if the talk names one, such as a sesshin or practice period theme. If there are \
no citations, return an empty list.";

/// What a citation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CitationKind {
    /// Points to a text.
    Reference,
    /// Recites a text's words.
    Quotation,
    /// A koan or case.
    Case,
}

/// One citation as the model reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RawCitation {
    /// The exact words from the window.
    pub text: String,
    /// The work, such as "Genjokoan".
    pub work_hint: Option<String>,
    /// A place in it, such as "paragraph 5".
    pub locator_hint: Option<String>,
    /// What it does.
    pub kind: CitationKind,
}

/// The model's answer for one window.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct WindowFacts {
    /// A practice period the window names, if any.
    pub practice_period: Option<String>,
    /// The citations.
    pub citations: Vec<RawCitation>,
}

/// How a citation was resolved (Spec 04, resolution rules).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    /// `resolved`, `probable` or `unresolved`.
    pub confidence: String,
    /// `hand_list`, `series_label` or `llm`; none when unresolved.
    pub method: Option<String>,
    /// The hand-listed locator; none when unresolved.
    pub work_locator_id: Option<String>,
}

/// A verified citation in the staging object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedCitation {
    /// What the model reported.
    #[serde(flatten)]
    pub raw: RawCitation,
    /// The first stored (uncorrected) teacher word it covers.
    pub w0: usize,
    /// One past the last.
    pub w1: usize,
    /// Set by the resolve step.
    #[serde(default)]
    pub resolution: Option<Resolution>,
}

/// The staging object `citations/<talk_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staging {
    /// Format version.
    pub schema: u32,
    /// The talk.
    pub talk_id: String,
    /// The practice period, if any window named one.
    pub practice_period: Option<String>,
    /// Verified citations in order.
    pub citations: Vec<StagedCitation>,
    /// Citations discarded because their words were not in the window.
    pub discarded: usize,
    /// Hash of the corrected text extracted from, so a rerun on the same text skips the model.
    #[serde(default)]
    pub input_hash: Option<String>,
    /// Why no extraction ran, when the model was unavailable or may not see the talk; the
    /// citations are empty and extraction runs once it may.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// Lowercase letters and digits: the form citations are matched in, so case, spacing and
/// punctuation do not matter.
fn token(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Where `needle` occurs in `hay` as consecutive tokens.
fn find(hay: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// Window bounds over `n` words.
pub fn windows(n: usize) -> Vec<(usize, usize)> {
    let step = WINDOW_WORDS - OVERLAP_WORDS;
    let mut out = Vec::new();
    let mut start = 0;
    while start < n {
        let end = (start + WINDOW_WORDS).min(n);
        out.push((start, end));
        if end == n {
            break;
        }
        start += step;
    }
    out
}

/// Verifies one window's citations against its words. `words` are the corrected words with
/// their source ranges; returns the kept citations and how many were discarded.
pub fn verify(
    words: &[(Word, (usize, usize))],
    window: (usize, usize),
    facts: &WindowFacts,
) -> (Vec<StagedCitation>, usize) {
    let hay: Vec<String> = words[window.0..window.1]
        .iter()
        .map(|(w, _)| token(&w.text))
        .collect();
    let mut kept = Vec::new();
    let mut discarded = 0;
    for c in &facts.citations {
        let needle: Vec<String> = c
            .text
            .split_whitespace()
            .map(token)
            .filter(|t| !t.is_empty())
            .collect();
        match find(&hay, &needle) {
            Some(at) => {
                let (first, last) = (window.0 + at, window.0 + at + needle.len() - 1);
                kept.push(StagedCitation {
                    raw: c.clone(),
                    w0: words[first].1.0,
                    w1: words[last].1.1,
                    resolution: None,
                });
            }
            None => discarded += 1,
        }
    }
    (kept, discarded)
}

/// Runs extraction over a talk's corrected words.
pub async fn extract(
    client: &dyn LlmClient,
    talk_id: &str,
    words: &[(Word, (usize, usize))],
    class: DataClass,
) -> Result<Staging, LlmError> {
    let mut staging = Staging {
        schema: 1,
        talk_id: talk_id.to_owned(),
        practice_period: None,
        citations: Vec::new(),
        discarded: 0,
        input_hash: None,
        skipped: None,
    };
    for window in windows(words.len()) {
        let text = words[window.0..window.1]
            .iter()
            .map(|(w, _)| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let req = LlmRequest {
            stage: LlmStage::Extract,
            system: SYSTEM.into(),
            user: text,
            max_tokens: 4_096,
        };
        let facts: WindowFacts = complete_json(client, &req, class).await?;
        if staging.practice_period.is_none() {
            staging.practice_period = facts.practice_period.clone();
        }
        let (kept, discarded) = verify(words, window, &facts);
        staging.discarded += discarded;
        for c in kept {
            // Overlapping windows can find the same words twice.
            if !staging
                .citations
                .iter()
                .any(|s| (s.w0, s.w1) == (c.w0, c.w1))
            {
                staging.citations.push(c);
            }
        }
    }
    staging.citations.sort_by_key(|c| (c.w0, c.w1));
    tracing::info!(
        talk = talk_id,
        kept = staging.citations.len(),
        discarded = staging.discarded,
        "citations extracted"
    );
    Ok(staging)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FakeLlm;
    use serde_json::json;

    fn mapped(text: &str) -> Vec<(Word, (usize, usize))> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, w)| {
                let at = i32::try_from(i).unwrap_or(0) * 300;
                (Word::new(w, at, at + 250, 0.9), (i, i + 1))
            })
            .collect()
    }

    #[tokio::test]
    async fn a_fabricated_citation_is_discarded_and_a_real_one_kept() {
        let words = mapped("As Dogen says, to study the way is to study the self. That is all.");
        let fake = FakeLlm::new(vec![Ok(json!({
            "practice_period": null,
            "citations": [
                {"text": "to study the Way is to study the self",
                 "work_hint": "Genjokoan", "locator_hint": null, "kind": "quotation"},
                {"text": "to forget the self is to be verified by all things",
                 "work_hint": "Genjokoan", "locator_hint": null, "kind": "quotation"}
            ]
        }))]);
        let staging = extract(&fake, "t1", &words, DataClass::Synthetic)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            staging.discarded, 1,
            "words not in the window are discarded"
        );
        assert_eq!(staging.citations.len(), 1);
        let c = &staging.citations[0];
        assert_eq!((c.w0, c.w1), (3, 12), "word positions found by the code");
    }

    #[test]
    fn windows_overlap_and_cover_everything() {
        assert_eq!(windows(0), Vec::<(usize, usize)>::new());
        assert_eq!(windows(100), [(0, 100)]);
        let w = windows(13_000);
        assert_eq!(w, [(0, 6_000), (5_800, 11_800), (11_600, 13_000)]);
    }

    #[test]
    fn positions_map_back_through_glossary_merges() {
        // "Genjo koan" became one corrected word covering stored words 4 and 5.
        let words = vec![
            (Word::new("read", 0, 10, 0.9), (0, 1)),
            (Word::new("the", 10, 20, 0.9), (1, 2)),
            (Word::new("opening", 20, 30, 0.9), (2, 3)),
            (Word::new("of", 30, 40, 0.9), (3, 4)),
            (Word::new("Genjokoan", 40, 60, 0.9), (4, 6)),
        ];
        let facts = WindowFacts {
            practice_period: None,
            citations: vec![RawCitation {
                text: "opening of Genjokoan".into(),
                work_hint: None,
                locator_hint: None,
                kind: CitationKind::Reference,
            }],
        };
        let (kept, discarded) = verify(&words, (0, words.len()), &facts);
        assert_eq!(discarded, 0);
        assert_eq!((kept[0].w0, kept[0].w1), (2, 6));
    }
}
