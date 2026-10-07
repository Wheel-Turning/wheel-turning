//! Suspect units (Spec 08, PDFs and OCR, step 4): a unit whose mean word confidence is under
//! `min_unit_confidence`, or fewer of whose words are in the English word list and the
//! glossary than `min_dictionary_share`, is suspect, and too many suspect units stop the
//! import.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use super::super::TextError;

/// Lower case without accents, so `Dōgen` and `dogen` meet.
fn fold(word: &str) -> String {
    word.nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// The words a unit is checked against.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    words: HashSet<String>,
}

impl Dictionary {
    /// Builds one from word-list lines and glossary forms; each form's words are added.
    pub fn new<'a>(
        list: impl IntoIterator<Item = &'a str>,
        glossary: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut words = HashSet::new();
        for w in list {
            let w = fold(w.trim());
            if !w.is_empty() {
                words.insert(w);
            }
        }
        for form in glossary {
            for w in form.split(|c: char| !c.is_alphanumeric() && c != '\'') {
                let w = fold(w);
                if !w.is_empty() {
                    words.insert(w);
                }
            }
        }
        Self { words }
    }

    /// Reads a word list, one word to a line, such as `/usr/share/dict/words`.
    pub fn load(path: &Path, glossary: &[String]) -> Result<Self, TextError> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            TextError::Source(format!(
                "the English word list {} could not be read ({e}); install one (apt install \
                 wamerican) or set [texts] word_list in pipeline.toml",
                path.display()
            ))
        })?;
        Ok(Self::new(text.lines(), glossary.iter().map(String::as_str)))
    }

    fn knows(&self, token: &str) -> bool {
        if self.words.contains(token) {
            return true;
        }
        // Possessives and simple plurals the list may not carry.
        for suffix in ["'s", "'", "s"] {
            if let Some(stem) = token.strip_suffix(suffix)
                && !stem.is_empty()
                && self.words.contains(stem)
            {
                return true;
            }
        }
        false
    }

    /// The share of a text's word tokens the dictionary knows. Numbers and punctuation are not
    /// words; hyphenated words count by their parts. Text with no words scores 1.
    pub fn share(&self, text: &str) -> f32 {
        let mut known = 0_u32;
        let mut total = 0_u32;
        for raw in text.split(|c: char| c.is_whitespace() || c == '-' || c == '\u{2014}') {
            let token = fold(raw.trim_matches(|c: char| !c.is_alphanumeric()));
            let token = token.replace('\u{2019}', "'");
            if !token.chars().any(char::is_alphabetic) {
                continue;
            }
            total += 1;
            if self.knows(&token) {
                known += 1;
            }
        }
        if total == 0 {
            1.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let share = known as f32 / total as f32;
            share
        }
    }
}

/// A suspect unit, for the report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suspect {
    /// Its locator.
    pub locator: String,
    /// Its mean word confidence, for an OCR unit.
    pub confidence: Option<f32>,
    /// The share of its words in the dictionary.
    pub dictionary_share: f32,
}

/// Whether a unit is suspect under the thresholds.
pub fn suspect(
    confidence: Option<f32>,
    dictionary_share: f32,
    min_unit_confidence: f32,
    min_dictionary_share: f32,
) -> bool {
    confidence.is_some_and(|c| c < min_unit_confidence) || dictionary_share < min_dictionary_share
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> Dictionary {
        Dictionary::new(
            [
                "the", "mind", "is", "like", "moon", "on", "water", "student", "sit",
            ],
            ["Dōgen Zenji", "zazen"],
        )
    }

    #[test]
    fn counts_known_words() {
        let d = dict();
        assert!((d.share("The mind is like the moon on water.") - 1.0).abs() < 1e-6);
        assert!(
            (d.share("The m1nd is l1ke the m00n") - 0.5).abs() < 1e-6,
            "{}",
            d.share("The m1nd is l1ke the m00n")
        );
        assert!((d.share("Dogen's zazen, 1253.") - 1.0).abs() < 1e-6);
        assert!((d.share("students sit") - 1.0).abs() < 1e-6);
        assert!((d.share("12 34") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn flags_low_confidence_or_unknown_words() {
        assert!(suspect(Some(0.7), 1.0, 0.8, 0.85));
        assert!(suspect(None, 0.5, 0.8, 0.85));
        assert!(!suspect(None, 0.9, 0.8, 0.85));
        assert!(!suspect(Some(0.95), 0.9, 0.8, 0.85));
    }
}
