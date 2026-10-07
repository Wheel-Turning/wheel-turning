//! Cutting units (Spec 08, Import pipeline, steps 4 and 5): by scheme, long units split at
//! sentence boundaries into `.a`, `.b` and so on, short fragments merged into a neighbor, and
//! locators assigned, with a duplicate locator failing the import.

use regex::Regex;
use serde::Serialize;

use super::TextError;
use super::extract::Section;
use super::manifest::{Scheme, UnitsSection};

/// A fragment under this many words merges into its neighbor.
pub const MIN_FRAGMENT_WORDS: usize = 5;
/// `text_unit.text` must be at least this many characters.
pub const MIN_CHARS: usize = 10;
/// And at most this many.
pub const MAX_CHARS: usize = 2500;

/// One unit, ready to store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unit {
    /// Its place in the edition, from 0.
    pub ordinal: usize,
    /// Such as `dhammapada:1` or `dhammapada:1.a`.
    pub locator: String,
    /// Such as `Dhammapada, verse 1`.
    pub label: String,
    /// Its section's title.
    pub heading: Option<String>,
    /// The text.
    pub text: String,
    /// Its word count.
    pub word_count: usize,
}

fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// A unit before locators: its number, chapter, heading and text.
#[derive(Debug, Clone)]
struct Draft {
    n: String,
    chapter: usize,
    heading: Option<String>,
    text: String,
}

/// Splits `text` at sentence ends into pieces of at most `max` words; a sentence longer than
/// `max` is cut at `max` words.
fn split_sentences(text: &str, max: usize) -> Vec<String> {
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_inclusive(char::is_whitespace) {
        cur.push_str(word);
        let w = word.trim_end();
        if w.ends_with(['.', '!', '?'])
            || w.ends_with(".\"")
            || w.ends_with(".'")
            || w.ends_with("?\"")
            || w.ends_with("!\"")
        {
            sentences.push(std::mem::take(&mut cur));
        }
    }
    if !cur.trim().is_empty() {
        sentences.push(cur);
    }
    let mut pieces: Vec<String> = Vec::new();
    let mut piece = String::new();
    for s in sentences {
        let fits = words(&piece) + words(&s) <= max;
        if !piece.trim().is_empty() && !fits {
            pieces.push(piece.trim().to_owned());
            piece.clear();
        }
        if words(&s) > max {
            let ws: Vec<&str> = s.split_whitespace().collect();
            for chunk in ws.chunks(max.max(1)) {
                pieces.push(chunk.join(" "));
            }
        } else {
            piece.push_str(&s);
        }
    }
    if !piece.trim().is_empty() {
        pieces.push(piece.trim().to_owned());
    }
    pieces
}

/// The drafts of one section, by scheme.
fn drafts(
    section: &Section,
    chapter: usize,
    cfg: &UnitsSection,
    number: Option<&Regex>,
    counter: &mut usize,
) -> Result<Vec<Draft>, TextError> {
    let next = |counter: &mut usize| {
        *counter += 1;
        counter.to_string()
    };
    let heading = section.title.clone();
    let mut out = Vec::new();
    match cfg.scheme {
        Scheme::Paragraph => {
            for b in &section.blocks {
                out.push(Draft {
                    n: next(counter),
                    chapter,
                    heading: heading.clone(),
                    text: b.split_whitespace().collect::<Vec<_>>().join(" "),
                });
            }
        }
        Scheme::Verse => {
            for b in &section.blocks {
                out.push(Draft {
                    n: next(counter),
                    chapter,
                    heading: heading.clone(),
                    text: b.clone(),
                });
            }
        }
        Scheme::Heading => {
            if !section.blocks.is_empty() {
                out.push(Draft {
                    n: next(counter),
                    chapter,
                    heading: heading.clone(),
                    text: section.blocks.join("\n\n"),
                });
            }
        }
        Scheme::Numbered => {
            let re = number.ok_or_else(|| {
                TextError::Manifest("the numbered scheme needs number_pattern".into())
            })?;
            let body = section.blocks.join("\n");
            let starts: Vec<(usize, usize, String)> = re
                .captures_iter(&body)
                .filter_map(|c| {
                    let whole = c.get(0)?;
                    let n = c.get(1)?.as_str().to_owned();
                    Some((whole.start(), whole.end(), n))
                })
                .collect();
            for (k, (_, text_start, n)) in starts.iter().enumerate() {
                let end = starts.get(k + 1).map_or(body.len(), |s| s.0);
                let text = body[*text_start..end].trim();
                if !text.is_empty() {
                    out.push(Draft {
                        n: n.clone(),
                        chapter,
                        heading: heading.clone(),
                        text: text.to_owned(),
                    });
                }
            }
        }
    }
    Ok(out)
}

fn label(template: &str, work: &str, n: &str, chapter: usize, heading: Option<&str>) -> String {
    template
        .replace("{work}", work)
        .replace("{n}", n)
        .replace("{chapter}", &chapter.to_string())
        .replace("{heading}", heading.unwrap_or(""))
        .trim()
        .to_owned()
}

/// Cuts the kept sections into units. `chapter` numbering counts kept sections from 1.
pub fn cut(sections: &[Section], cfg: &UnitsSection, work: &str) -> Result<Vec<Unit>, TextError> {
    let number = match &cfg.number_pattern {
        Some(p) => Some(
            Regex::new(&format!("(?m){p}"))
                .map_err(|e| TextError::Manifest(format!("number_pattern: {e}")))?,
        ),
        None => None,
    };
    let mut all: Vec<Draft> = Vec::new();
    let mut counter = 0;
    for (i, section) in sections.iter().enumerate() {
        if cfg.chapters {
            counter = 0;
        }
        all.extend(drafts(section, i + 1, cfg, number.as_ref(), &mut counter)?);
    }

    // A fragment under the minimum merges into the unit before it, or after it when first.
    let mut merged: Vec<Draft> = Vec::with_capacity(all.len());
    let mut carry: Option<String> = None;
    for mut d in all {
        if let Some(c) = carry.take() {
            d.text = format!("{c}\n{}", d.text);
        }
        if words(&d.text) < MIN_FRAGMENT_WORDS {
            match merged.last_mut() {
                Some(prev) => prev.text = format!("{}\n{}", prev.text, d.text),
                None => carry = Some(d.text),
            }
            continue;
        }
        merged.push(d);
    }
    if let (Some(c), Some(prev)) = (carry, merged.last_mut()) {
        prev.text = format!("{}\n{c}", prev.text);
    }

    let mut units = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for d in merged {
        let base = if cfg.chapters {
            format!("{}:{}:{}", cfg.locator_prefix, d.chapter, d.n)
        } else {
            format!("{}:{}", cfg.locator_prefix, d.n)
        };
        let pieces = if words(&d.text) > cfg.max_words || d.text.chars().count() > MAX_CHARS {
            split_sentences(&d.text, cfg.max_words)
        } else {
            vec![d.text.clone()]
        };
        let lettered = pieces.len() > 1;
        for (k, piece) in pieces.into_iter().enumerate() {
            let suffix = if lettered {
                char::from(b'a' + u8::try_from(k % 26).unwrap_or(0)).to_string()
            } else {
                String::new()
            };
            let locator = if lettered {
                format!("{base}.{suffix}")
            } else {
                base.clone()
            };
            if !seen.insert(locator.clone()) {
                return Err(TextError::DuplicateLocator(locator));
            }
            let n = format!("{}{suffix}", d.n);
            if piece.chars().count() < MIN_CHARS {
                continue;
            }
            units.push(Unit {
                ordinal: units.len(),
                label: label(
                    &cfg.label_template,
                    work,
                    &n,
                    d.chapter,
                    d.heading.as_deref(),
                ),
                heading: d.heading.clone(),
                word_count: words(&piece),
                text: piece,
                locator,
            });
        }
    }
    Ok(units)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn cfg(scheme: Scheme, max_words: usize) -> UnitsSection {
        UnitsSection {
            scheme,
            locator_prefix: "fixture".into(),
            number_pattern: Some(r"^\s*(\d+)\.\s".into()),
            label_template: "{work}, verse {n}".into(),
            max_words,
            chapters: false,
        }
    }

    fn section(blocks: &[&str]) -> Section {
        Section {
            title: Some("Chapter 1".into()),
            blocks: blocks.iter().map(|b| (*b).to_owned()).collect(),
        }
    }

    #[test]
    fn numbered_units_take_their_numbers_and_labels() {
        let s = section(&[
            "1. All that we are is the result of what we have thought.",
            "2. If a person speaks or acts with a calm mind, joy follows.",
        ]);
        let u = cut(&[s], &cfg(Scheme::Numbered, 220), "Fixture Sutra").unwrap();
        assert_eq!(u.len(), 2);
        assert_eq!(
            (u[0].locator.as_str(), u[0].label.as_str()),
            ("fixture:1", "Fixture Sutra, verse 1")
        );
        assert!(u[0].text.starts_with("All that we are"), "{}", u[0].text);
        assert_eq!(u[1].locator, "fixture:2");
    }

    #[test]
    fn verse_paragraph_and_heading_schemes_cut_as_named() {
        let s = section(&[
            "The first stanza runs\nacross two lines here.",
            "A second stanza of five words or more.",
        ]);
        let verse = cut(std::slice::from_ref(&s), &cfg(Scheme::Verse, 220), "W").unwrap();
        assert_eq!(verse.len(), 2);
        assert!(verse[0].text.contains('\n'), "verse keeps its line breaks");
        let para = cut(std::slice::from_ref(&s), &cfg(Scheme::Paragraph, 220), "W").unwrap();
        assert!(!para[0].text.contains('\n'));
        let heading = cut(std::slice::from_ref(&s), &cfg(Scheme::Heading, 220), "W").unwrap();
        assert_eq!(heading.len(), 1);
        assert_eq!(heading[0].heading.as_deref(), Some("Chapter 1"));
    }

    #[test]
    fn long_units_split_into_lettered_parts_and_fragments_merge() {
        let long = "One two three four five six. Seven eight nine ten eleven twelve. Thirteen fourteen fifteen sixteen seventeen eighteen.";
        let s = section(&[
            long,
            "Too short.",
            "Another paragraph with enough words in it.",
        ]);
        let u = cut(&[s], &cfg(Scheme::Paragraph, 8), "W").unwrap();
        let locators: Vec<&str> = u.iter().map(|x| x.locator.as_str()).collect();
        assert_eq!(
            locators,
            ["fixture:1.a", "fixture:1.b", "fixture:1.c", "fixture:3"]
        );
        assert!(
            u[2].text.ends_with("Too short."),
            "the fragment joined the unit before it: {}",
            u[2].text
        );
        assert_eq!(u[0].label, "W, verse 1a");
    }

    #[test]
    fn a_duplicate_locator_fails() {
        let s = section(&[
            "1. A verse long enough to keep here.",
            "1. The same number again, also long enough.",
        ]);
        let err = cut(&[s], &cfg(Scheme::Numbered, 220), "W").unwrap_err();
        assert!(
            matches!(err, TextError::DuplicateLocator(ref l) if l == "fixture:1"),
            "{err}"
        );
    }
}
