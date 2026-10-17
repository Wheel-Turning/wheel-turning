//! The deterministic filters (Spec 05, Summary and verifier), run in the spec's order before
//! any verifier call. Each returns the reason a sentence is dropped, or `None` to keep it.

use std::collections::BTreeSet;

use crate::DropReason;

/// Spec 11, PB2.3: model text carries no em dashes. Each dash used as one (an em dash, a spaced
/// en dash or a spaced double hyphen) becomes punctuation: a colon when one dash leads into a
/// list, a comma otherwise, so a pair around an aside becomes two commas. It runs on the model's
/// sentences before the filters and the verifier; quotations are cut from the excerpts later
/// and never pass through it.
pub fn replace_dashes(text: &str) -> String {
    let marked = text
        .replace(" \u{2013} ", "\u{2014}")
        .replace(" -- ", "\u{2014}");
    if !marked.contains('\u{2014}') {
        return text.to_owned();
    }
    let parts: Vec<&str> = marked
        .split('\u{2014}')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let mut out = String::new();
    for part in &parts {
        if !out.is_empty() {
            if !out.ends_with([',', ':', ';']) {
                let list = parts.len() == 2 && part.contains(',');
                out.push(if list { ':' } else { ',' });
            }
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

/// Filter 4: one sentence of at most `max_words` words.
pub fn one_sentence(description: &str, max_words: usize) -> Option<DropReason> {
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
    if text.split_whitespace().count() > max_words {
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

/// Words in a teacher's alias that are titles, not names, so they never count as naming one.
const TITLE_WORDS: &[&str] = &[
    "roshi",
    "sensei",
    "osho",
    "reverend",
    "rev",
    "sister",
    "brother",
    "father",
    "mother",
    "teacher",
    "dharma",
    "zenji",
    "sama",
    "lama",
    "ajahn",
    "bhante",
    "venerable",
    "zen",
];

/// The name words of each alias: words of at least four letters that are not titles,
/// lowercased, paired with whose they are.
pub fn name_words<T: Copy>(aliases: &[(T, String)]) -> Vec<(T, String)> {
    let mut out = Vec::new();
    for (who, alias) in aliases {
        for w in words(alias) {
            if w.chars().count() >= 4 && !TITLE_WORDS.contains(&w.as_str()) {
                out.push((*who, w));
            }
        }
    }
    out
}

/// Filter 6, names: a capitalized word of the sentence that is a name word of some teacher none
/// of whose excerpts is cited. Returns the word.
pub fn misattributed<T: Copy + PartialEq>(
    sentence: &str,
    names: &[(T, String)],
    cited: &[T],
) -> Option<String> {
    sentence
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().next().is_some_and(char::is_uppercase))
        .find_map(|w| {
            let lower = w.to_lowercase();
            let owners: Vec<T> = names
                .iter()
                .filter(|(_, n)| *n == lower)
                .map(|(t, _)| *t)
                .collect();
            (!owners.is_empty() && !owners.iter().any(|o| cited.contains(o))).then(|| w.to_owned())
        })
}

/// The years the sentence names: four-digit numbers from 1900 to 2099.
pub fn years(sentence: &str) -> Vec<i32> {
    sentence
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|w| w.parse().ok())
        .filter(|y| (1900..=2099).contains(y))
        .collect()
}

/// Filter 7: a word or phrase that states what a teacher believes rather than what a talk says.
pub fn belief_word(sentence: &str) -> Option<&'static str> {
    let w = words(sentence);
    let has = |phrase: &[&str]| {
        w.windows(phrase.len())
            .any(|win| win.iter().zip(phrase).all(|(a, b)| a == b))
    };
    [
        &["believes"][..],
        &["believe"],
        &["thinks"],
        &["holds", "that"],
        &["is", "convinced"],
    ]
    .into_iter()
    .find(|p| has(p))
    .and_then(|p| p.first().copied())
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
    // How a sentence refers to the passages, the talks and their relation.
    "address",
    "addresses",
    "agree",
    "agrees",
    "alike",
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
    "excerpt",
    "excerpts",
    "explain",
    "explains",
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
    "point",
    "points",
    "present",
    "presents",
    "said",
    "say",
    "says",
    "see",
    "sees",
    "share",
    "shared",
    "stress",
    "stresses",
    "talk",
    "talks",
    "teacher",
    "teachers",
    "treat",
    "treats",
    "view",
    "views",
    "discuss",
    "discusses",
    "mention",
    "mentions",
    "multiple",
    "note",
    "notes",
    "offer",
    "offers",
    "several",
    "suggest",
    "suggests",
    "various",
    "whilst",
];

/// A crude stem: lowercase with a common English suffix removed, applied alike to the theme
/// and the passages, so "noticing" and "notice" meet at the same stem.
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

/// Filter 5, a third check: an excerpt label such as "E3" (or "P3") in the sentence. The page
/// numbers excerpts differently from the model's labels, so a label would point to the wrong one.
pub fn has_passage_label(description: &str) -> bool {
    words(description).iter().any(|w| is_label(w))
}

/// A label such as `e3` or `p3`, which is not a content word.
fn is_label(word: &str) -> bool {
    word.strip_prefix('e')
        .or_else(|| word.strip_prefix('p'))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The stems of a text's content words: no stopwords, excerpt labels or bare numbers, which the
/// theme's overlap with its passages is measured on.
fn content_stems(text: &str) -> Vec<String> {
    words(text)
        .into_iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()) && !is_label(w))
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .map(|w| stem(&w))
        .collect()
}

/// Filter 8: the share of the sentence's distinct content words, other than `names`, that
/// appear in the union of its cited excerpts. A sentence with no content words scores 0.
pub fn grounding_ratio(description: &str, passages: &[&str], names: &[String]) -> f32 {
    let kept: Vec<String> = words(description)
        .into_iter()
        .filter(|w| !names.contains(w))
        .collect();
    let theme: BTreeSet<String> = content_stems(&kept.join(" ")).into_iter().collect();
    if theme.is_empty() {
        return 0.0;
    }
    let source: BTreeSet<String> = passages.iter().flat_map(|p| content_stems(p)).collect();
    let found = theme.iter().filter(|w| source.contains(*w)).count();
    found as f32 / theme.len() as f32
}

/// Filter 7, second half: a word that speaks to the reader, which makes a sentence advice in
/// the site's own voice rather than a report of what a talk says.
pub fn addresses_reader(sentence: &str) -> Option<&'static str> {
    const READER: &[&str] = &["you", "your", "yours", "yourself", "yourselves"];
    words(sentence)
        .iter()
        .find_map(|w| READER.iter().find(|r| **r == w.as_str()).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: usize = 40;

    #[test]
    fn em_dashes_become_commas_or_a_colon() {
        let cases = [
            (
                "Two talks say to sit \u{2014} not to strive.",
                "Two talks say to sit, not to strive.",
            ),
            (
                "Practice\u{2014}as one talk puts it\u{2014}is not self-improvement.",
                "Practice, as one talk puts it, is not self-improvement.",
            ),
            (
                "Three talks name what to return to \u{2014} breath, posture, sound.",
                "Three talks name what to return to: breath, posture, sound.",
            ),
            ("Sit -- and breathe.", "Sit, and breathe."),
            ("Sit \u{2013} and breathe.", "Sit, and breathe."),
            (
                "Talks from 2010\u{2013}2012.",
                "Talks from 2010\u{2013}2012.",
            ),
            ("No dash here.", "No dash here."),
        ];
        for (input, want) in cases {
            assert_eq!(replace_dashes(input), want, "{input}");
        }
    }

    #[test]
    fn one_sentence_passes_a_single_short_sentence() {
        assert_eq!(
            one_sentence(
                "In a 2011 talk, Ada Fixture says judging thoughts are weather to watch.",
                MAX
            ),
            None
        );
        assert_eq!(
            one_sentence(
                "Breath counting, e.g.counting to ten, steadies attention.",
                MAX
            ),
            None
        );
    }

    #[test]
    fn one_sentence_drops_two_sentences_and_long_ones() {
        assert_eq!(
            one_sentence("Some talks stress posture. Others stress breath.", MAX),
            Some(DropReason::NotOneSentence)
        );
        assert_eq!(
            one_sentence("Posture matters.\nBreath matters", MAX),
            Some(DropReason::NotOneSentence)
        );
        let long = vec!["word"; MAX + 1].join(" ");
        assert_eq!(one_sentence(&long, MAX), Some(DropReason::TooLong));
        let exact = vec!["word"; MAX].join(" ");
        assert_eq!(one_sentence(&exact, MAX), None);
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
    fn excerpt_labels_are_found_in_the_sentence() {
        assert!(has_passage_label(
            "E1 and E3 both treat doubt as a doorway."
        ));
        assert!(has_passage_label("Unlike p12, others stress breath."));
        assert!(!has_passage_label(
            "Several talks treat doubt as a doorway."
        ));
        assert!(!has_passage_label(
            "Practice begins at step 1 with posture."
        ));
    }

    #[test]
    fn a_name_counts_only_for_a_cited_teacher() {
        let names = name_words(&[
            (1, "Ada Fixture".to_owned()),
            (2, "Bo Lin Roshi".to_owned()),
            (3, "Grace Fixture".to_owned()),
        ]);
        // Short words and titles are not names.
        assert!(
            !names
                .iter()
                .any(|(_, w)| w == "ada" || w == "roshi" || w == "bo")
        );
        let s = "In a talk, Fixture says to sit with grace.";
        assert_eq!(misattributed(s, &names, &[1]), None, "Fixture is Ada's");
        assert_eq!(misattributed(s, &names, &[2]), Some("Fixture".to_owned()));
        assert_eq!(
            misattributed("Grace Fixture describes the bell.", &names, &[1]),
            Some("Grace".to_owned())
        );
        assert_eq!(
            misattributed("Sitting with grace, the talk ends.", &names, &[2]),
            None,
            "a lower-case word is not a name"
        );
    }

    #[test]
    fn years_and_belief_words_are_found() {
        assert_eq!(
            years("In a 2011 talk and a 1998 one, 12000 people sat."),
            vec![2011, 1998]
        );
        assert!(years("A talk on posture.").is_empty());
        assert_eq!(
            belief_word("Ada Fixture believes posture matters."),
            Some("believes")
        );
        assert_eq!(
            belief_word("The talk holds that breath is enough."),
            Some("holds")
        );
        assert_eq!(belief_word("A 2011 talk says posture matters."), None);
        assert_eq!(belief_word("She holds the bowl."), None);
    }

    #[test]
    fn a_sentence_that_speaks_to_the_reader_is_found() {
        assert_eq!(
            addresses_reader("Return to your breath when anger comes."),
            Some("your")
        );
        assert_eq!(addresses_reader("You need not battle anger."), Some("you"));
        assert_eq!(
            addresses_reader("Two talks describe returning to the breath."),
            None
        );
        assert_eq!(addresses_reader("A talk on youth and practice."), None);
    }

    #[test]
    fn grounding_counts_content_words_found_in_the_passages() {
        let passages = ["When judging thoughts arise, notice them and return to the breath."];
        let none: Vec<String> = Vec::new();
        let grounded = "Both passages treat judging thoughts as something to notice before returning to breath.";
        assert!(grounding_ratio(grounded, &passages, &none) >= 0.8);
        let drifted =
            "Both passages recommend therapy, journaling and medication for anxiety disorders.";
        assert!(grounding_ratio(drifted, &passages, &none) < 0.4);
        assert!(grounding_ratio("E1 and P2.", &passages, &none) == 0.0);
        let named = "In a talk, Fixture says judging thoughts are to notice and return to breath.";
        let names = vec!["fixture".to_owned()];
        assert!(
            grounding_ratio(named, &passages, &names) > grounding_ratio(named, &passages, &none)
        );
    }
}
