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
/// The output limit of one window's reply.
pub const MAX_REPLY_TOKENS: u32 = 4_096;
/// The smallest window a cut-off reply is split down to. A window this small cannot honestly
/// need more than [`MAX_REPLY_TOKENS`] of exact quotations, so a reply still cut off here is a
/// runaway reply, and that window is given up rather than the talk.
pub const MIN_WINDOW_WORDS: usize = 500;

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
    /// Windows of [`MIN_WINDOW_WORDS`] or fewer whose reply was still cut off at the output
    /// limit, so their citations are missing.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cut_off_windows: usize,
}

/// For `skip_serializing_if`, so staging objects written before `cut_off_windows` existed keep
/// their bytes and their resolve and index keys.
fn is_zero(n: &usize) -> bool {
    *n == 0
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

/// Adds `c` unless a citation of its kind already covers its words. Overlapping windows can find
/// the same words twice, and a quotation cut at a window's edge again whole in the next window;
/// the longer reading is kept.
fn merge(citations: &mut Vec<StagedCitation>, c: StagedCitation) {
    let covers = |a: &StagedCitation, b: &StagedCitation| {
        a.raw.kind == b.raw.kind && a.w0 <= b.w0 && b.w1 <= a.w1
    };
    if citations.iter().any(|s| covers(s, &c)) {
        return;
    }
    citations.retain(|s| !covers(&c, s));
    citations.push(c);
}

/// The two halves of a window whose reply was cut off, overlapping by [`OVERLAP_WORDS`].
fn halves(window: (usize, usize)) -> [(usize, usize); 2] {
    let (start, end) = window;
    let mid = start + (end - start) / 2;
    let half_overlap = OVERLAP_WORDS / 2;
    [
        (start, (mid + half_overlap).min(end)),
        (mid.saturating_sub(half_overlap).max(start), end),
    ]
}

/// Runs extraction over a talk's corrected words. A window whose reply is cut off at the output
/// limit is split in two and each half asked again, down to [`MIN_WINDOW_WORDS`]; a window that
/// small still cut off is counted in `cut_off_windows` and the rest of the talk is kept.
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
        cut_off_windows: 0,
    };
    // Windows still to ask, the next on top, so they are asked in order.
    let mut pending: Vec<(usize, usize)> = windows(words.len());
    pending.reverse();
    while let Some(window) = pending.pop() {
        let text = words[window.0..window.1]
            .iter()
            .map(|(w, _)| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let req = LlmRequest {
            stage: LlmStage::Extract,
            system: SYSTEM.into(),
            user: text,
            max_tokens: MAX_REPLY_TOKENS,
        };
        let facts: WindowFacts = match complete_json(client, &req, class).await {
            Ok(facts) => facts,
            Err(LlmError::Truncated) if window.1 - window.0 > MIN_WINDOW_WORDS => {
                let [first, second] = halves(window);
                tracing::info!(
                    talk = talk_id,
                    words = window.1 - window.0,
                    "reply cut off; asking again in two halves"
                );
                pending.push(second);
                pending.push(first);
                continue;
            }
            Err(LlmError::Truncated) => {
                tracing::warn!(
                    talk = talk_id,
                    from = window.0,
                    to = window.1,
                    "reply cut off even for a small window; its citations are missing"
                );
                staging.cut_off_windows += 1;
                continue;
            }
            Err(e) => return Err(e),
        };
        if staging.practice_period.is_none() {
            staging.practice_period = facts.practice_period.clone();
        }
        let (kept, discarded) = verify(words, window, &facts);
        staging.discarded += discarded;
        for c in kept {
            merge(&mut staging.citations, c);
        }
    }
    staging.citations.sort_by_key(|c| (c.w0, c.w1));
    tracing::info!(
        talk = talk_id,
        kept = staging.citations.len(),
        discarded = staging.discarded,
        cut_off_windows = staging.cut_off_windows,
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

    /// `n` invented words, `word0 word1 ...`.
    fn numbered(n: usize) -> Vec<(Word, (usize, usize))> {
        mapped(
            &(0..n)
                .map(|i| format!("word{i}"))
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    /// A quotation of words `from..to` of [`numbered`].
    fn quote(from: usize, to: usize) -> serde_json::Value {
        let text = (from..to).map(|i| format!("word{i}")).collect::<Vec<_>>();
        json!({"text": text.join(" "), "work_hint": null, "locator_hint": null,
               "kind": "quotation"})
    }

    fn words_sent(fake: &FakeLlm) -> Vec<usize> {
        fake.seen
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .map(|(req, _)| req.user.split_whitespace().count())
            .collect()
    }

    #[tokio::test]
    async fn a_cut_off_reply_is_asked_again_in_halves_and_the_results_merged() {
        let words = numbered(1_200);
        let fake = FakeLlm::scripted(vec![
            Err(LlmError::Truncated),
            // Words 0..700: one quotation, and one cut off at the window's edge.
            Ok(json!({"practice_period": null,
                      "citations": [quote(10, 13), quote(690, 700)]})),
            // Words 500..1200: the edge quotation whole, and one more.
            Ok(json!({"practice_period": "Fall practice period",
                      "citations": [quote(690, 705), quote(900, 902)]})),
        ]);
        let staging = extract(&fake, "t1", &words, DataClass::Synthetic)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(words_sent(&fake), [1_200, 700, 700]);
        let ranges: Vec<_> = staging.citations.iter().map(|c| (c.w0, c.w1)).collect();
        assert_eq!(
            ranges,
            [(10, 13), (690, 705), (900, 902)],
            "the edge quotation is kept once, whole"
        );
        assert_eq!(staging.cut_off_windows, 0);
        assert_eq!(
            staging.practice_period.as_deref(),
            Some("Fall practice period")
        );
    }

    #[tokio::test]
    async fn a_small_window_still_cut_off_is_given_up_and_the_rest_kept() {
        let words = numbered(800);
        let fake = FakeLlm::scripted(vec![
            Err(LlmError::Truncated),
            Ok(json!({"practice_period": null, "citations": [quote(10, 13)]})),
            Err(LlmError::Truncated),
        ]);
        let staging = extract(&fake, "t1", &words, DataClass::Synthetic)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(words_sent(&fake), [800, 500, 500]);
        assert_eq!(staging.citations.len(), 1);
        assert_eq!(staging.cut_off_windows, 1);
        let json = serde_json::to_value(&staging).unwrap_or_default();
        assert_eq!(json["cut_off_windows"], 1);
    }

    #[test]
    fn a_staging_with_no_cut_off_window_serializes_as_before() {
        let staging = Staging {
            schema: 1,
            talk_id: "t1".into(),
            practice_period: None,
            citations: Vec::new(),
            discarded: 0,
            input_hash: None,
            skipped: None,
            cut_off_windows: 0,
        };
        let json = serde_json::to_string(&staging).unwrap_or_default();
        assert!(!json.contains("cut_off_windows"));
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
