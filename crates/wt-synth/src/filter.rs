//! The deterministic filters (Spec 05, Synthesis and verifier), run in the spec's order before
//! any verifier call. Each returns the reason a theme is dropped, or `None` to keep it.

use std::collections::BTreeSet;

use crate::DropReason;

/// Most words a theme may have.
pub const MAX_WORDS: usize = 30;

/// Filter 4: one sentence of at most [`MAX_WORDS`] words.
pub fn one_sentence(description: &str) -> Option<DropReason> {
    let text = description.trim();
    if text.is_empty() {
        return Some(DropReason::NotOneSentence);
    }
    if text.contains('\n') {
        return Some(DropReason::NotOneSentence);
    }
    // A sentence end followed by more text means a second sentence.
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if matches!(c, '.' | '!' | '?')
            && chars.get(i + 1).is_some_and(|n| n.is_whitespace())
            && chars[i + 1..].iter().any(|n| n.is_alphanumeric())
        {
            return Some(DropReason::NotOneSentence);
        }
    }
    if text.split_whitespace().count() > MAX_WORDS {
        return Some(DropReason::TooLong);
    }
    None
}

/// Filter 5, first half: any quotation mark. An apostrophe between two letters, as in
/// "teacher's", is not one.
pub fn has_quotation_mark(description: &str) -> bool {
    let chars: Vec<char> = description.chars().collect();
    chars.iter().enumerate().any(|(i, c)| match c {
        '"' | '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{00AB}' | '\u{00BB}'
        | '\u{2039}' | '\u{203A}' | '\u{300C}' | '\u{300D}' | '\u{300E}' | '\u{300F}'
        | '\u{2018}' | '\u{201A}' | '\u{201B}' | '`' => true,
        '\'' | '\u{2019}' => {
            let before = i.checked_sub(1).and_then(|j| chars.get(j));
            let after = chars.get(i + 1);
            !(before.is_some_and(|b| b.is_alphanumeric())
                && after.is_some_and(|a| a.is_alphabetic()))
        }
        _ => false,
    })
}

/// Filter 5, second half: the first registry alias the description contains, matched without
/// regard to case on word boundaries.
pub fn alias_in<'a>(description: &str, aliases: &'a [String]) -> Option<&'a str> {
    let words = words(description);
    aliases.iter().map(String::as_str).find(|alias| {
        let target = words_of(alias);
        !target.is_empty() && words.windows(target.len()).any(|w| w == target.as_slice())
    })
}

fn words_of(text: &str) -> Vec<String> {
    words(text)
}

/// Lowercased runs of letters and digits.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Words that carry no claim about the passages: function words, question words and the
/// vocabulary a theme uses to talk about the passages themselves.
const STOPWORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "also",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "down",
    "during",
    "each",
    "either",
    "few",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "him",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "may",
    "me",
    "might",
    "more",
    "most",
    "much",
    "must",
    "my",
    "neither",
    "no",
    "nor",
    "not",
    "of",
    "off",
    "on",
    "once",
    "one",
    "only",
    "or",
    "other",
    "others",
    "our",
    "out",
    "over",
    "own",
    "rather",
    "same",
    "she",
    "should",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "upon",
    "very",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "whereas",
    "whether",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "within",
    "without",
    "would",
    "you",
    "your",
    "yet",
    // How a theme refers to the passages and to their relation.
    "another",
    "approach",
    "approaches",
    "contrast",
    "describe",
    "describes",
    "differ",
    "difference",
    "differences",
    "different",
    "differently",
    "emphasis",
    "emphasize",
    "emphasizes",
    "emphasise",
    "emphasises",
    "focus",
    "focuses",
    "instead",
    "passage",
    "passages",
    "present",
    "presents",
    "see",
    "sees",
    "share",
    "shared",
    "stress",
    "stresses",
    "treat",
    "treats",
    "view",
    "views",
    "whilst",
];

/// A crude stem: lowercase with a common English suffix removed, applied alike to the theme
/// and the passages, so "practicing" and "practice" meet at "practic".
fn stem(word: &str) -> String {
    for suffix in [
        "ingly", "edly", "ness", "ment", "ing", "ies", "ied", "ed", "es", "ly", "er", "s", "e",
    ] {
        if let Some(root) = word.strip_suffix(suffix)
            && root.chars().count() >= 3
        {
            return root.to_owned();
        }
    }
    word.to_owned()
}

/// A passage label such as `p3`, which is not a content word.
fn is_label(word: &str) -> bool {
    word.strip_prefix('p')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn content_stems(text: &str) -> Vec<String> {
    words(text)
        .into_iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()) && !is_label(w))
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .map(|w| stem(&w))
        .collect()
}

/// Filter 6: the share of the description's distinct content words that appear in the union
/// of its cited passages. A description with no content words scores 0.
pub fn grounding_ratio(description: &str, passages: &[&str]) -> f32 {
    let theme: BTreeSet<String> = content_stems(description).into_iter().collect();
    if theme.is_empty() {
        return 0.0;
    }
    let source: BTreeSet<String> = passages.iter().flat_map(|p| content_stems(p)).collect();
    let found = theme.iter().filter(|w| source.contains(*w)).count();
    found as f32 / theme.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_sentence_passes_a_single_short_sentence() {
        assert_eq!(
            one_sentence("P1 and P2 both treat judging thoughts as weather to be watched."),
            None
        );
        assert_eq!(
            one_sentence("Breath counting, e.g.counting to ten, steadies attention."),
            None
        );
    }

    #[test]
    fn one_sentence_drops_two_sentences_and_long_ones() {
        assert_eq!(
            one_sentence("Some passages stress posture. Others stress breath."),
            Some(DropReason::NotOneSentence)
        );
        assert_eq!(
            one_sentence("Posture matters.\nBreath matters"),
            Some(DropReason::NotOneSentence)
        );
        let long = vec!["word"; MAX_WORDS + 1].join(" ");
        assert_eq!(one_sentence(&long), Some(DropReason::TooLong));
        let exact = vec!["word"; MAX_WORDS].join(" ");
        assert_eq!(one_sentence(&exact), None);
    }

    #[test]
    fn quotation_marks_are_found_but_apostrophes_are_not() {
        assert!(has_quotation_mark("They call it \"just sitting\" in both."));
        assert!(has_quotation_mark(
            "They call it \u{201C}just sitting\u{201D}."
        ));
        assert!(has_quotation_mark("They call it 'just sitting' here."));
        assert!(!has_quotation_mark(
            "The teacher's view and the student\u{2019}s doubt."
        ));
        assert!(!has_quotation_mark("Both treat doubt as a doorway."));
    }

    #[test]
    fn aliases_match_whole_words_without_case() {
        let aliases = vec!["Ada Fixture".to_owned(), "Bo".to_owned()];
        assert_eq!(
            alias_in("As ada fixture says, sit.", &aliases),
            Some("Ada Fixture")
        );
        assert_eq!(alias_in("Bo and others sit.", &aliases), Some("Bo"));
        assert_eq!(alias_in("Both passages sit with boredom.", &aliases), None);
    }

    #[test]
    fn grounding_counts_content_words_found_in_the_passages() {
        let passages = ["When judging thoughts arise, notice them and return to the breath."];
        let grounded = "Both passages treat judging thoughts as something to notice before returning to breath.";
        assert!(grounding_ratio(grounded, &passages) >= 0.8);
        let drifted =
            "Both passages recommend therapy, journaling and medication for anxiety disorders.";
        assert!(grounding_ratio(drifted, &passages) < 0.4);
        assert!(grounding_ratio("P1 and P2.", &passages) == 0.0);
    }
}
