//! Glossary correction (Spec 04, Glossary correction). `apply` replaces known variants, which
//! are mis-hearings and alternate spellings, with each term's canonical form. It is a pure
//! function: the stored transcript stays uncorrected, and the index step calls the same
//! function, so corrected text and timing cannot disagree. Near misses are only suggested.

use std::collections::BTreeSet;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use serde::{Deserialize, Serialize};
use wt_core::records::sha256_hex;
use wt_core::transcript::Word;

use crate::RegistryError;

/// What a term is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermKind {
    /// Practice vocabulary, such as shikantaza.
    Term,
    /// A person's name, such as Dogen.
    Name,
    /// A text's title, such as Genjokoan.
    TextTitle,
}

/// One glossary term, as in the `glossary_term` table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GlossaryTerm {
    /// The spelling text should use.
    pub canonical: String,
    /// What it is.
    pub kind: TermKind,
    /// Known mis-hearings and alternate spellings.
    #[serde(default)]
    pub variants: Vec<String>,
}

/// A glossary file such as `scripts/seed_glossary.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlossaryFile {
    /// The terms.
    #[serde(rename = "term", default)]
    pub terms: Vec<GlossaryTerm>,
}

impl GlossaryFile {
    /// Parses a glossary file, refusing blank canonical forms.
    pub fn parse(text: &str, origin: &str) -> Result<Self, RegistryError> {
        let file: GlossaryFile = toml::from_str(text).map_err(|e| RegistryError::Parse {
            path: origin.to_owned(),
            source: Box::new(e),
        })?;
        if let Some(blank) = file.terms.iter().find(|t| t.canonical.trim().is_empty()) {
            return Err(RegistryError::Invalid(format!(
                "{origin}: a term has a blank canonical form (variants {:?})",
                blank.variants
            )));
        }
        Ok(file)
    }
}

/// A possible variant the glossary does not know yet, for a person to review. Never applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The word's index in the list given.
    pub word_index: usize,
    /// The word as heard.
    pub heard: String,
    /// The canonical term it may be.
    pub canonical: String,
    /// Damerau-Levenshtein distance between them.
    pub distance: usize,
    /// Up to five words either side.
    pub context: String,
}

/// The glossary, compiled.
#[derive(Debug, Clone)]
pub struct Glossary {
    version: String,
    /// One entry per pattern: the canonical form's words.
    replacements: Vec<Vec<String>>,
    automaton: Option<AhoCorasick>,
    /// Single-word canonical forms, lowercased, for near-miss detection.
    singles: Vec<(String, String)>,
}

/// Lowercase letters and digits of a word, the form patterns are matched in.
fn token(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A phrase as space-separated tokens.
fn tokens(phrase: &str) -> String {
    phrase
        .split_whitespace()
        .map(token)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Glossary {
    /// Compiles the terms. Each canonical form also matches itself in any case, so "dogen"
    /// becomes "Dogen".
    pub fn new(terms: &[GlossaryTerm]) -> Result<Self, RegistryError> {
        let mut sorted: Vec<GlossaryTerm> = terms
            .iter()
            .map(|t| {
                let mut t = t.clone();
                t.variants.sort();
                t.variants.dedup();
                t
            })
            .collect();
        sorted.sort();
        let canonical_json = serde_json::to_string(&sorted).unwrap_or_default();
        let version = sha256_hex(canonical_json.as_bytes());

        let mut patterns: Vec<String> = Vec::new();
        let mut replacements: Vec<Vec<String>> = Vec::new();
        let mut seen = BTreeSet::new();
        for term in &sorted {
            let canonical: Vec<String> = term
                .canonical
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            for variant in std::iter::once(&term.canonical).chain(&term.variants) {
                let pattern = tokens(variant);
                // The first term to claim a pattern keeps it, so the result is deterministic.
                if !pattern.is_empty() && seen.insert(pattern.clone()) {
                    patterns.push(pattern);
                    replacements.push(canonical.clone());
                }
            }
        }
        let automaton = if patterns.is_empty() {
            None
        } else {
            Some(
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::LeftmostLongest)
                    .build(&patterns)
                    .map_err(|e| RegistryError::Matcher(e.to_string()))?,
            )
        };
        let singles = sorted
            .iter()
            .filter(|t| !t.canonical.contains(char::is_whitespace))
            .map(|t| (token(&t.canonical), t.canonical.clone()))
            .filter(|(t, _)| !t.is_empty())
            .collect();
        Ok(Self {
            version,
            replacements,
            automaton,
            singles,
        })
    }

    /// The SHA-256 of the sorted glossary. Part of the `correct` and `index` job input hashes,
    /// so a change requeues exactly the talks it affects.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Each match as (first word, one past the last word, replacement), in order.
    fn matches(&self, words: &[Word]) -> Vec<(usize, usize, usize)> {
        let Some(automaton) = &self.automaton else {
            return Vec::new();
        };
        // The text matched against: word tokens joined by single spaces, with each token's
        // byte range, so a match maps back to whole words only.
        let mut text = String::new();
        let mut starts = Vec::with_capacity(words.len());
        for w in words {
            if !text.is_empty() {
                text.push(' ');
            }
            starts.push(text.len());
            text.push_str(&token(&w.text));
        }
        let word_at = |byte: usize| starts.partition_point(|&s| s <= byte).saturating_sub(1);
        let mut out = Vec::new();
        for m in automaton.find_iter(&text) {
            let (first, last) = (word_at(m.start()), word_at(m.end().saturating_sub(1)));
            let starts_on_word = starts.get(first) == Some(&m.start());
            let ends_on_word = m.end() == text.len() || text.as_bytes().get(m.end()) == Some(&b' ');
            if starts_on_word && ends_on_word {
                out.push((first, last + 1, m.pattern().as_usize()));
            }
        }
        out
    }

    /// Replaces every known variant with its canonical form. A replacement spanning several
    /// words takes the start of the first and the end of the last; if the canonical form has
    /// several words, they share that time evenly. Punctuation after the last replaced word is
    /// kept.
    pub fn apply(&self, words: &[Word]) -> Vec<Word> {
        self.apply_mapped(words)
            .into_iter()
            .map(|(w, _)| w)
            .collect()
    }

    /// Like [`Glossary::apply`], with each corrected word's source: the first and one past the
    /// last index of the input words it came from. Positions in the uncorrected list stay
    /// valid whatever the glossary, so citations are stored against them.
    pub fn apply_mapped(&self, words: &[Word]) -> Vec<(Word, (usize, usize))> {
        let mut out = Vec::with_capacity(words.len());
        let mut next = 0;
        for (first, end, replacement) in self.matches(words) {
            out.extend(
                words[next..first]
                    .iter()
                    .enumerate()
                    .map(|(k, w)| (w.clone(), (next + k, next + k + 1))),
            );
            let span = &words[first..end];
            let (start_ms, end_ms) = (span[0].start_ms, span[span.len() - 1].end_ms);
            let prob = span.iter().map(|w| w.prob).fold(f32::INFINITY, f32::min);
            let trailing: String = span[span.len() - 1]
                .text
                .chars()
                .rev()
                .take_while(|c| !c.is_alphanumeric())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let canonical = &self.replacements[replacement];
            let n = i32::try_from(canonical.len()).unwrap_or(1).max(1);
            let each = (end_ms - start_ms) / n;
            for (k, part) in canonical.iter().enumerate() {
                let k = i32::try_from(k).unwrap_or(0);
                let last = k == n - 1;
                let mut text = part.clone();
                if last {
                    text.push_str(&trailing);
                }
                out.push((
                    Word {
                        text,
                        start_ms: start_ms + each * k,
                        end_ms: if last {
                            end_ms
                        } else {
                            start_ms + each * (k + 1)
                        },
                        prob,
                    },
                    (first, end),
                ));
            }
            next = end;
        }
        out.extend(
            words[next..]
                .iter()
                .enumerate()
                .map(|(k, w)| (w.clone(), (next + k, next + k + 1))),
        );
        out
    }

    /// Words that may be unknown variants: at least six letters, not English, within
    /// Damerau-Levenshtein distance 2 of a single-word canonical term and sounding like it.
    /// Words a known variant already covers are skipped. Nothing here is ever applied.
    pub fn near_misses(
        &self,
        words: &[Word],
        is_english: &dyn Fn(&str) -> bool,
    ) -> Vec<Suggestion> {
        let covered: BTreeSet<usize> = self
            .matches(words)
            .into_iter()
            .flat_map(|(a, b, _)| a..b)
            .collect();
        let mut out = Vec::new();
        for (i, w) in words.iter().enumerate() {
            let t = token(&w.text);
            if covered.contains(&i) || t.chars().count() < 6 || is_english(&t) {
                continue;
            }
            let best = self
                .singles
                .iter()
                .map(|(lower, canonical)| (damerau_levenshtein(&t, lower), canonical))
                .filter(|(d, canonical)| *d <= 2 && soundex(&t) == soundex(&token(canonical)))
                .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
            if let Some((distance, canonical)) = best {
                let from = i.saturating_sub(5);
                let to = (i + 6).min(words.len());
                out.push(Suggestion {
                    word_index: i,
                    heard: w.text.clone(),
                    canonical: canonical.clone(),
                    distance,
                    context: words[from..to]
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                });
            }
        }
        out
    }
}

/// Optimal string alignment distance: insertions, deletions, substitutions and adjacent
/// transpositions.
pub fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// American Soundex, the phonetic key.
pub fn soundex(word: &str) -> String {
    let code = |c: char| match c {
        'b' | 'f' | 'p' | 'v' => Some('1'),
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => Some('2'),
        'd' | 't' => Some('3'),
        'l' => Some('4'),
        'm' | 'n' => Some('5'),
        'r' => Some('6'),
        _ => None,
    };
    let letters: Vec<char> = word
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let Some(&first) = letters.first() else {
        return String::new();
    };
    let mut out = first.to_ascii_uppercase().to_string();
    let mut last = code(first);
    for &c in &letters[1..] {
        let digit = code(c);
        if digit.is_some() && digit != last {
            out.extend(digit);
        }
        // h and w do not separate equal codes; vowels do.
        if !matches!(c, 'h' | 'w') {
            last = digit;
        }
        if out.len() == 4 {
            break;
        }
    }
    while out.len() < 4 {
        out.push('0');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soundex_and_distance() {
        assert_eq!(soundex("Robert"), "R163");
        assert_eq!(soundex("Rupert"), "R163");
        assert_eq!(soundex("Ashcraft"), "A261");
        assert_eq!(damerau_levenshtein("shikantaza", "shikantasa"), 1);
        assert_eq!(damerau_levenshtein("ac", "ca"), 1);
        assert_eq!(damerau_levenshtein("", "abc"), 3);
    }
}
