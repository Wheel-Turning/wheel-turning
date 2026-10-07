//! Legal pages, the notice scan and the language check (Spec 08, Source copies and legal pages;
//! Source policy for URL ingestion; Mandated sources). Copyright pages, imprints, license
//! notices and colophons are kept apart from the units, whole, in `text_edition.legal_text`.
//! The notice scan reads them with the first and last blocks of the text for copyright notices,
//! and a notice naming a holder the evidence does not cover blocks the document as
//! `third_party_notice`, unless a mandated source's `notice_action` says to flag or hold it.
//! Only English is loaded in v1.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use super::extract::Section;

/// Section titles that are legal pages.
const LEGAL_TITLES: [&str; 8] = [
    "copyright",
    "colophon",
    "imprint",
    "license",
    "licence",
    "permissions",
    "legal",
    "rights",
];

static NOTICE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(copyright|\u{00A9}|\(c\)\s*\d{4}|all rights reserved|\bisbn\b|creative commons|licensed under|no part of this (book|publication|work) may be)",
    )
    .ok()
});

/// Who a copyright line names: what follows "Copyright" and a year, the sign, or "Copyright
/// by", up to a period, semicolon or line end. "Copyright" alone in prose names nobody.
static HOLDER: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:copyright\s*(?:\u{00A9}|\(c\))?\s*\d{4}(?:\s*[-\u{2013},]\s*\d{2,4})*|(?:\u{00A9}|\(c\))\s*(?:\d{4}(?:\s*[-\u{2013},]\s*\d{2,4})*)?|copyright\s+by)\s*,?\s*(?:by\s+)?([^.;\n\u{00A9}]{2,80})",
    )
    .ok()
});

/// Whether a block is a legal notice rather than text: a notice line that is most of it.
pub fn is_notice(block: &str) -> bool {
    let Some(re) = NOTICE.as_ref() else {
        return false;
    };
    let words = block.split_whitespace().count();
    re.is_match(block) && words <= 120
}

fn legal_title(title: &str) -> bool {
    let t = title.to_lowercase();
    LEGAL_TITLES.iter().any(|w| {
        t.split(|c: char| !c.is_alphanumeric())
            .any(|word| word == *w)
    })
}

/// Takes legal pages out of the sections: whole sections titled as legal pages, and notice
/// blocks among the first and last few blocks of the text. Returns them joined, with where
/// each came from.
pub fn take_legal(sections: &mut Vec<Section>) -> (Vec<String>, Vec<String>) {
    let mut text = Vec::new();
    let mut places = Vec::new();
    sections.retain(|s| {
        let legal = s.title.as_deref().is_some_and(legal_title);
        if legal {
            text.push(s.blocks.join("\n"));
            places.push(format!("section {:?}", s.title.as_deref().unwrap_or("")));
        }
        !legal
    });
    let total: usize = sections.iter().map(|s| s.blocks.len()).sum();
    let edge = 5.min(total);
    let mut seen = 0;
    for s in sections.iter_mut() {
        let mut kept = Vec::new();
        for b in std::mem::take(&mut s.blocks) {
            let at_edge = seen < edge || seen >= total.saturating_sub(edge);
            seen += 1;
            if at_edge && is_notice(&b) {
                places.push(format!("block {seen}"));
                text.push(b);
            } else {
                kept.push(b);
            }
        }
        s.blocks = kept;
    }
    sections.retain(|s| !s.blocks.is_empty() || s.title.is_some());
    (text, places)
}

/// A copyright notice and the holder it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notice {
    /// The line, up to 160 characters.
    pub line: String,
    /// Who it names.
    pub holder: String,
}

fn fold(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The copyright notices in `texts` and who each names.
pub fn notices<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<Notice> {
    let Some(re) = HOLDER.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<Notice> = Vec::new();
    for text in texts {
        for line in text.lines() {
            for c in re.captures_iter(line) {
                let holder = c[1]
                    .trim()
                    .trim_end_matches(|ch: char| !ch.is_alphanumeric())
                    .trim_start_matches(|ch: char| !ch.is_alphanumeric())
                    .to_owned();
                let lower = holder.to_lowercase();
                if holder.is_empty()
                    || lower.starts_with("all rights")
                    || lower.starts_with("page")
                    || !holder.chars().any(char::is_alphabetic)
                {
                    continue;
                }
                if !out.iter().any(|n| fold(&n.holder) == fold(&holder)) {
                    out.push(Notice {
                        line: line.trim().chars().take(160).collect(),
                        holder,
                    });
                }
            }
        }
    }
    out
}

/// Whether a holder is one the evidence covers: either name contains the other, ignoring case
/// and punctuation.
pub fn covered(holder: &str, covers: &[&str]) -> bool {
    let h = fold(holder);
    !h.is_empty()
        && covers
            .iter()
            .map(|c| fold(c))
            .any(|c| !c.is_empty() && (h.contains(&c) || c.contains(&h)))
}

/// The notices whose holder the evidence does not cover.
pub fn third_party(found: &[Notice], covers: &[&str]) -> Vec<Notice> {
    found
        .iter()
        .filter(|n| !covered(&n.holder, covers))
        .cloned()
        .collect()
}

/// The first and last `n` blocks of the text, which the notice scan reads with the legal pages.
pub fn edges(sections: &[Section], n: usize) -> Vec<&str> {
    let all: Vec<&str> = sections
        .iter()
        .flat_map(|s| s.blocks.iter().map(String::as_str))
        .collect();
    if all.len() <= 2 * n {
        return all;
    }
    all[..n]
        .iter()
        .chain(&all[all.len() - n..])
        .copied()
        .collect()
}

const ENGLISH: [&str; 40] = [
    "the", "and", "of", "to", "were", "in", "is", "that", "it", "was", "for", "on", "as", "with",
    "his", "he", "be", "at", "by", "this", "are", "not", "from", "or", "but", "have", "an", "they",
    "which", "you", "one", "all", "we", "there", "has", "what", "when", "your", "can", "its",
];

/// The share of a text's words that are common English words. English prose runs near 0.4;
/// other languages written in Latin letters stay under 0.1.
pub fn english_share(text: &str) -> f32 {
    let mut total = 0_u32;
    let mut hits = 0_u32;
    for w in text.split_whitespace().take(5000) {
        let w: String = w
            .chars()
            .filter(|c| c.is_alphabetic())
            .flat_map(char::to_lowercase)
            .collect();
        if w.is_empty() {
            continue;
        }
        total += 1;
        if ENGLISH.contains(&w.as_str()) {
            hits += 1;
        }
    }
    if total == 0 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let share = hits as f32 / total as f32;
    share
}

/// Below this share of common English words, a text of enough words is not English.
pub const MIN_ENGLISH_SHARE: f32 = 0.15;
/// Too few words to judge.
pub const MIN_WORDS_TO_JUDGE: usize = 40;

/// Whether the text reads as English, or is too short to tell.
pub fn is_english(text: &str) -> bool {
    text.split_whitespace().count() < MIN_WORDS_TO_JUDGE || english_share(text) >= MIN_ENGLISH_SHARE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(title: Option<&str>, blocks: &[&str]) -> Section {
        Section {
            title: title.map(str::to_owned),
            blocks: blocks.iter().map(|b| (*b).to_owned()).collect(),
        }
    }

    #[test]
    fn takes_legal_pages_and_notice_blocks_out() {
        let mut sections = vec![
            section(
                Some("Copyright Page"),
                &["Copyright 1999 Fixture Press. All rights reserved."],
            ),
            section(
                Some("Chapter One"),
                &[
                    "\u{00A9} 2001 by Fixture Translator. ISBN 0-000-00000-0",
                    "The student sat facing the wall in the quiet hall before dawn.",
                    "The bell rang three times and the hall was still.",
                ],
            ),
        ];
        let (legal, places) = take_legal(&mut sections);
        assert_eq!(legal.len(), 2);
        assert_eq!(places, vec!["section \"Copyright Page\"", "block 1"]);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].blocks.len(), 2);
        assert!(!sections[0].blocks.iter().any(|b| b.contains("ISBN")));
    }

    #[test]
    fn finds_holders_and_which_the_evidence_covers() {
        let found = notices([
            "Copyright \u{00A9} 1994 by Fixture Cleary Estate. All rights reserved.",
            "(c) 2007-2008 Fixture Wonderwheel; used here",
            "Copyright 2026 Fixture Press",
            "No copyright line here at all, just text.",
        ]);
        let holders: Vec<&str> = found.iter().map(|n| n.holder.as_str()).collect();
        assert_eq!(
            holders,
            vec![
                "Fixture Cleary Estate",
                "Fixture Wonderwheel",
                "Fixture Press"
            ]
        );
        let third = third_party(&found, &["Fixture Press, Inc."]);
        assert_eq!(third.len(), 2);
        assert!(covered("FIXTURE PRESS", &["Fixture Press"]));
        assert!(!covered("Someone Else", &["Fixture Press"]));
    }

    #[test]
    fn tells_english_from_other_languages() {
        let english = "When the talk ended the students bowed and walked back to the garden, and \
                       the teacher said that the moon is in the water and the water is in the \
                       moon, and that this is what it is to sit with all of it for one day.";
        let hungarian = "A tanítvány reggel leült a fal elé és figyelte a lélegzetét amíg a \
                         harang háromszor meg nem szólalt a csendes teremben, aztán felállt és \
                         kiment a kertbe ahol a reggeli fény a köveken feküdt és a madarak \
                         énekeltek a fák között a hegy lábánál egészen délig minden nap.";
        assert!(is_english(english), "{}", english_share(english));
        assert!(!is_english(hungarian), "{}", english_share(hungarian));
        assert!(is_english("Too short to judge."));
    }
}
